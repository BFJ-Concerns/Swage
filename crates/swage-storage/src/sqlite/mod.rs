pub mod agent_definition;
pub mod agent_message;
pub mod approval;
pub mod attachment;
pub mod audit;
pub mod channel_link;
pub mod channel_queue;
pub mod citation;
pub mod context_snapshot;
pub mod curation_assertion;
pub mod delivery;
pub mod device;
pub mod material;
pub mod message;
pub mod migrations;
pub mod observation;
pub mod orientation_card;
pub mod provider_round;
pub mod schedule;
pub mod secret;
pub mod session;
pub mod session_variable;
pub mod skill_activation;
pub mod surface;
pub mod surface_annotation;
pub mod task_definition;
pub mod thread;
pub mod tool_call;
pub mod topic;
pub mod turn;
pub mod turn_queue;
pub mod workflow;
pub mod workspace_attachment;

use std::path::Path;

use rusqlite::{Connection, ErrorCode};
use swage_core::StorageError;
use tokio::sync::Mutex;

use migrations::run_migrations;

/// SQLite-backed storage with WAL mode and async-safe connection access.
pub struct SqliteStorage {
    conn: Mutex<Connection>,
    /// Every registered secret value, masked out of each conversation record
    /// this store writes. The secret store keeps it current.
    secret_values: swage_core::RegisteredSecretValues,
}

impl SqliteStorage {
    /// Open a file-backed database, creating a fresh schema or refusing an incompatible one.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let conn =
            Connection::open(path).map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
        Self::configure_and_migrate(conn)
    }

    /// Open an in-memory database. Primarily used in tests.
    pub fn in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
        Self::configure_and_migrate(conn)
    }

    /// Configure connection pragmas, check or initialise the schema, and wrap in Self.
    fn configure_and_migrate(conn: Connection) -> Result<Self, StorageError> {
        // WAL mode: concurrent reads with a single writer, better crash safety.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;

        // Enforce foreign key constraints (SQLite disables them by default).
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;

        // Avoid immediate "database is locked" errors on busy writes.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;

        run_migrations(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            secret_values: swage_core::RegisteredSecretValues::default(),
        })
    }

    /// The registered values this store masks out of what it records.
    pub(crate) fn mask(&self, text: &mut String) {
        self.secret_values.mask(text);
    }

    /// [`Self::mask`] over an optional text.
    pub(crate) fn mask_option(&self, text: &mut Option<String>) {
        self.secret_values.mask_option(text);
    }

    /// [`Self::mask`] over the strings of a JSON document, keys included.
    pub(crate) fn mask_json(&self, value: &mut serde_json::Value) {
        swage_core::secret_masking::mask_json_strings(value, &self.secret_values.current());
    }

    /// Acquire the connection lock and run a blocking closure.
    ///
    /// `block_in_place` moves the current task to a blocking thread for the
    /// duration of the closure, preventing the async executor from stalling
    /// while SQLite holds a lock.
    pub async fn with_conn<F, T>(&self, f: F) -> Result<T, StorageError>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    {
        let guard = self.conn.lock().await;
        tokio::task::block_in_place(|| f(&guard)).map_err(map_rusqlite_error)
    }
}

fn map_rusqlite_error(error: rusqlite::Error) -> StorageError {
    match error {
        rusqlite::Error::SqliteFailure(inner, message)
            if matches!(inner.code, ErrorCode::ConstraintViolation) =>
        {
            StorageError::ConstraintViolation(message.unwrap_or_else(|| inner.to_string()))
        }
        other => StorageError::QueryFailed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value scanned for in a test: a registered value must rest in
    /// no column of any row, whichever writer recorded it.
    async fn rows_holding(storage: &SqliteStorage, needle: &str) -> Vec<String> {
        let needle = needle.to_owned();
        storage
            .with_conn(move |conn| {
                let mut tables = conn.prepare(
                    "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                )?;
                let tables: Vec<String> = tables
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut found = Vec::new();
                for table in tables {
                    let mut columns = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
                    let columns: Vec<String> = columns
                        .query_map([], |row| row.get(0))?
                        .collect::<Result<_, _>>()?;
                    for column in columns {
                        let count: i64 = conn.query_row(
                            &format!("SELECT COUNT(*) FROM \"{table}\" WHERE instr(CAST(\"{column}\" AS TEXT), ?1) > 0"),
                            [&needle],
                            |row| row.get(0),
                        )?;
                        if count > 0 {
                            found.push(format!("{table}.{column}"));
                        }
                    }
                }
                Ok(found)
            })
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registered_value_rests_in_no_conversation_record() {
        use crate::model::*;
        use crate::store::*;

        let storage = SqliteStorage::in_memory().unwrap();
        let secret = r#"zq7-"registered"-value"#;
        storage
            .registered_secret_values()
            .replace(vec![secret.to_string()]);
        let text = format!("the key is {secret}, keep it");
        let json = serde_json::json!({ "cmd": format!("echo {secret}") });

        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(crate::fixtures::new_session(topic.id))
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &storage,
            NewThread {
                session_id: session.id,
                agent_id: crate::fixtures::FIXTURE_AGENT_ID.into(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: crate::fixtures::FIXTURE_AGENT_ID.into(),
                state: TurnState::Pending,
                user_input: Some(text.clone()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        assert_eq!(
            turn.user_input.as_deref(),
            Some("the key is [REDACTED_SECRET], keep it"),
            "the record handed back is the masked one"
        );
        storage
            .append_message(AppendMessage {
                thread_id: thread.id,
                attachments: Vec::new(),
                turn_id: Some(turn.id),
                role: swage_core::Role::User,
                origin: MessageOrigin::User,
                content: text.clone(),
                agent_id: crate::fixtures::FIXTURE_AGENT_ID.into(),
            })
            .await
            .unwrap();
        storage.update_response(turn.id, &text).await.unwrap();
        storage
            .record_turn_failure_message(turn.id, &text)
            .await
            .unwrap();
        storage
            .update_structured_output(turn.id, &json)
            .await
            .unwrap();
        storage
            .store_snapshot(turn.id, &json.to_string())
            .await
            .unwrap();
        let call = storage
            .create_tool_call(NewToolCall {
                execution: None,
                turn_id: turn.id,
                agent_id: crate::fixtures::FIXTURE_AGENT_ID.into(),
                call_id: "call-1".into(),
                tool: "shell".into(),
                arguments: json.to_string(),
                model_arguments: json.to_string(),
                result: None,
                model_result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                provider_round: 1,
                position: 0,
                parent_call_id: None,
                display_summary: Some(text.clone()),
            })
            .await
            .unwrap();
        storage
            .update_tool_call_result(
                call.id,
                &serde_json::json!({ "output": text }).to_string(),
                &text,
                "success",
                1,
                false,
                None,
                None,
            )
            .await
            .unwrap();
        storage
            .create_approval(NewApprovalRequest {
                turn_id: turn.id,
                agent_id: crate::fixtures::FIXTURE_AGENT_ID.into(),
                kind: ApprovalKind::Tool,
                tool_name: Some("shell".into()),
                tool_arguments: Some(json.clone()),
                hook_name: None,
                prompt: text.clone(),
                context: Some(json.clone()),
                timeout_at: chrono::Utc::now() + chrono::Duration::seconds(30),
                timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
            })
            .await
            .unwrap();
        storage
            .enqueue_turn_input(NewQueuedTurnInput {
                delivery_targets: Vec::new(),
                client_id: None,
                attended: true,
                session_id: session.id,
                thread_id: thread.id,
                origin: MessageOrigin::User,
                content: text.clone(),
                attachments: Vec::new(),
                schedule_run_id: None,
                channel_message_ids: Vec::new(),
            })
            .await
            .unwrap();

        let escaped = serde_json::to_string(secret).unwrap();
        let escaped = &escaped[1..escaped.len() - 1];
        assert_eq!(rows_holding(&storage, secret).await, Vec::<String>::new());
        assert_eq!(rows_holding(&storage, escaped).await, Vec::<String>::new());
        assert!(
            !rows_holding(&storage, swage_core::REDACTED_SECRET)
                .await
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn in_memory_opens_and_migrates() {
        let storage = SqliteStorage::in_memory().expect("in_memory should succeed");

        // Schema should be present — query the topics table.
        storage
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM topics", [], |r| r.get::<_, i64>(0))
            })
            .await
            .expect("topics table should exist after migration");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_file_backed_db() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.db");

        SqliteStorage::open(&path).expect("open should succeed");
        assert!(path.exists(), "database file should be created");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migration_idempotency() {
        // Running migrations twice (via two separate opens) must not fail.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("idempotent.db");

        SqliteStorage::open(&path).expect("first open");
        SqliteStorage::open(&path).expect("second open — migrations already applied");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn general_scope_seeded() {
        let storage = SqliteStorage::in_memory().expect("in_memory should succeed");

        let count: i64 = storage
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM topics WHERE is_general = 1",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .expect("query should succeed");

        assert_eq!(count, 1, "exactly one general scope should be seeded");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wal_mode_active() {
        let storage = SqliteStorage::in_memory().expect("in_memory should succeed");

        let mode: String = storage
            .with_conn(|conn| conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)))
            .await
            .expect("pragma query should succeed");

        // In-memory databases report "memory" even after WAL pragma — WAL requires
        // a file. Verify the pragma executes without error; file-backed tests check
        // the actual "wal" value.
        assert!(
            mode == "wal" || mode == "memory",
            "unexpected journal mode: {mode}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wal_mode_on_file_db() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("wal.db");
        let storage = SqliteStorage::open(&path).expect("open should succeed");

        let mode: String = storage
            .with_conn(|conn| conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)))
            .await
            .expect("pragma query should succeed");

        assert_eq!(mode, "wal", "file-backed DB should use WAL journal mode");
    }
}
