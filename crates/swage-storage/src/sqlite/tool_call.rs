//! SQLite implementation of ToolCallStore.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::{
    StorageError, compact_tool_arguments_for_model, compact_tool_result_for_model,
    compact_tool_result_for_model_with_reference, tool_result_reference,
};

use crate::model::{NewToolCall, ToolCallRecord};
use crate::sqlite::SqliteStorage;
use crate::store::ToolCallStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// Map a row from `SELECT id, turn_id, agent_id, call_id, tool, arguments,
/// model_arguments, result, model_result, status, duration_ms, truncated,
/// total_bytes, provider_round, position, created_at, parent_call_id,
/// display_summary`.
fn row_to_tool_call(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolCallRecord> {
    let id: String = row.get(0)?;
    let turn_id: String = row.get(1)?;
    let created: String = row.get(15)?;

    // SQLite stores booleans as integers; map 0/1 back to bool.
    let truncated_int: i64 = row.get(11)?;
    let arguments: String = row.get(5)?;
    let model_arguments = row
        .get::<_, Option<String>>(6)?
        .unwrap_or_else(|| compact_arguments_from_raw(&arguments));
    let result: Option<String> = row.get(7)?;
    let status: Option<String> = row.get(9)?;
    let model_result = row
        .get::<_, Option<String>>(8)?
        .or_else(|| compact_result_from_raw(result.as_deref(), status.as_deref()));

    Ok(ToolCallRecord {
        id: parse_uuid(&id)?,
        turn_id: parse_uuid(&turn_id)?,
        agent_id: row.get(2)?,
        call_id: row.get(3)?,
        tool: row.get(4)?,
        arguments,
        model_arguments,
        result,
        model_result,
        status,
        duration_ms: row.get(10)?,
        truncated: truncated_int != 0,
        total_bytes: row.get(12)?,
        provider_round: row.get(13)?,
        position: row.get(14)?,
        created_at: parse_dt(&created)?,
        parent_call_id: row.get(16)?,
        display_summary: row.get(17)?,
    })
}

fn compact_arguments_from_raw(arguments: &str) -> String {
    let value = serde_json::from_str(arguments)
        .unwrap_or_else(|_| serde_json::Value::String(arguments.to_string()));
    compact_tool_arguments_for_model(&value)
}

fn compact_result_from_raw(result: Option<&str>, status: Option<&str>) -> Option<String> {
    result.map(|result| compact_tool_result_for_model(result, status != Some("success")))
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

// ---------------------------------------------------------------------------
// ToolCallStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ToolCallStore for SqliteStorage {
    async fn create_tool_call(
        &self,
        tool_call: NewToolCall,
    ) -> Result<ToolCallRecord, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO tool_calls
                 (id, turn_id, agent_id, call_id, tool, arguments, model_arguments,
                  result, model_result, status,
                  duration_ms, truncated, total_bytes, provider_round, position, created_at,
                  parent_call_id, display_summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    id.to_string(),
                    tool_call.turn_id.to_string(),
                    &tool_call.agent_id,
                    &tool_call.call_id,
                    &tool_call.tool,
                    &tool_call.arguments,
                    &tool_call.model_arguments,
                    &tool_call.result,
                    &tool_call.model_result,
                    &tool_call.status,
                    tool_call.duration_ms,
                    tool_call.truncated as i32,
                    tool_call.total_bytes,
                    tool_call.provider_round,
                    tool_call.position,
                    &now_str,
                    &tool_call.parent_call_id,
                    &tool_call.display_summary,
                ],
            )?;

            Ok(ToolCallRecord {
                id,
                turn_id: tool_call.turn_id,
                agent_id: tool_call.agent_id,
                call_id: tool_call.call_id,
                tool: tool_call.tool,
                arguments: tool_call.arguments,
                model_arguments: tool_call.model_arguments,
                result: tool_call.result,
                model_result: tool_call.model_result,
                status: tool_call.status,
                duration_ms: tool_call.duration_ms,
                truncated: tool_call.truncated,
                total_bytes: tool_call.total_bytes,
                provider_round: tool_call.provider_round,
                position: tool_call.position,
                created_at: now,
                parent_call_id: tool_call.parent_call_id,
                display_summary: tool_call.display_summary,
            })
        })
        .await
    }

    async fn get_tool_call(&self, id: Uuid) -> Result<Option<ToolCallRecord>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, turn_id, agent_id, call_id, tool, arguments, model_arguments,
                        result, model_result, status, duration_ms, truncated, total_bytes,
                        provider_round, position, created_at, parent_call_id,
                        display_summary
                 FROM tool_calls WHERE id = ?1",
                params![id.to_string()],
                row_to_tool_call,
            )
            .optional()
        })
        .await
    }

    async fn get_tool_call_by_turn_and_call_id(
        &self,
        turn_id: Uuid,
        call_id: &str,
    ) -> Result<Option<ToolCallRecord>, StorageError> {
        let call_id = call_id.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, turn_id, agent_id, call_id, tool, arguments, model_arguments,
                        result, model_result, status, duration_ms, truncated, total_bytes,
                        provider_round, position, created_at, parent_call_id,
                        display_summary
                 FROM tool_calls WHERE turn_id = ?1 AND call_id = ?2",
                params![turn_id.to_string(), call_id],
                row_to_tool_call,
            )
            .optional()
        })
        .await
    }

    async fn update_tool_call_result(
        &self,
        call_id: &str,
        turn_id: Uuid,
        result: &str,
        status: &str,
        duration_ms: i64,
        truncated: bool,
        total_bytes: Option<i64>,
    ) -> Result<(), StorageError> {
        let call_id = call_id.to_owned();
        let turn_id_str = turn_id.to_string();
        let result = result.to_owned();
        let status = status.to_owned();
        let model_result = compact_tool_result_for_model_with_reference(
            &result,
            status != "success",
            tool_result_reference(&turn_id_str, &call_id),
        );

        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE tool_calls
                 SET result=?1, model_result=?2, status=?3, duration_ms=?4, truncated=?5,
                     total_bytes=?6
                 WHERE call_id=?7 AND turn_id=?8",
                params![
                    &result,
                    &model_result,
                    &status,
                    duration_ms,
                    truncated as i32,
                    total_bytes,
                    &call_id,
                    &turn_id_str,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn list_tool_calls_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ToolCallRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, turn_id, agent_id, call_id, tool, arguments, model_arguments,
                        result, model_result, status, duration_ms, truncated, total_bytes,
                        provider_round, position, created_at,
                        parent_call_id, display_summary
                 FROM tool_calls
                 WHERE turn_id = ?1
                 ORDER BY provider_round ASC, position ASC",
            )?;
            let rows = stmt.query_map(params![turn_id.to_string()], row_to_tool_call)?;
            rows.collect()
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::model::{NewSession, NewSpace, NewThread, NewTurn, TurnState};
    use crate::store::{SessionStore, SpaceStore, ThreadStore, ToolCallStore, TurnStore};

    use super::*;

    /// Set up an in-memory DB with a space, session, thread, and turn.
    ///
    /// Inserts the thread via raw SQL because create_thread (task 6ay8) is not
    /// yet implemented. Task 1byf will update this once the trait lands.
    async fn make_turn() -> (SqliteStorage, Uuid) {
        use chrono::Utc;
        use rusqlite::params as sql_params;

        let storage = SqliteStorage::in_memory().unwrap();
        let space = SpaceStore::create(
            &storage,
            NewSpace {
                name: "p".into(),
                slug: "p".into(),
            },
        )
        .await
        .unwrap();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "test-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        // Insert a thread row so the FK on turns.thread_id is satisfied.
        let thread_id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
                     VALUES (?1, ?2, 'test-agent', ?3, ?3)",
                    sql_params![thread_id.to_string(), session.id.to_string(), &now],
                )
            })
            .await
            .unwrap();

        let turn = storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "test-agent".into(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        (storage, turn.id)
    }

    fn new_call(turn_id: Uuid, call_id: &str, position: i32) -> NewToolCall {
        NewToolCall {
            turn_id,
            agent_id: "agent-test".into(),
            call_id: call_id.into(),
            tool: "shell".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
            model_arguments: compact_arguments_from_raw(r#"{"cmd":"ls"}"#),
            result: None,
            model_result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            provider_round: 1,
            position,
            parent_call_id: None,
            display_summary: Some("ls".into()),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_returns_record_with_id() {
        let (storage, turn_id) = make_turn().await;
        let record = storage
            .create_tool_call(new_call(turn_id, "toolu_01", 0))
            .await
            .unwrap();

        assert_eq!(record.turn_id, turn_id);
        assert_eq!(record.call_id, "toolu_01");
        assert_eq!(record.tool, "shell");
        assert_eq!(record.arguments, r#"{"cmd":"ls"}"#);
        assert!(record.result.is_none());
        assert!(!record.truncated);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_turn_ordered_by_provider_round_then_position() {
        let (storage, turn_id) = make_turn().await;

        // Insert out of order to verify round sorting before position sorting.
        let mut round_two_first = new_call(turn_id, "toolu_03", 0);
        round_two_first.provider_round = 2;
        storage.create_tool_call(round_two_first).await.unwrap();
        let mut round_one_second = new_call(turn_id, "toolu_02", 1);
        round_one_second.provider_round = 1;
        storage.create_tool_call(round_one_second).await.unwrap();
        let mut round_one_first = new_call(turn_id, "toolu_01", 0);
        round_one_first.provider_round = 1;
        storage.create_tool_call(round_one_first).await.unwrap();

        let calls = storage.list_tool_calls_by_turn(turn_id).await.unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].call_id, "toolu_01");
        assert_eq!(calls[0].provider_round, 1);
        assert_eq!(calls[0].position, 0);
        assert_eq!(calls[1].call_id, "toolu_02");
        assert_eq!(calls[1].provider_round, 1);
        assert_eq!(calls[1].position, 1);
        assert_eq!(calls[2].call_id, "toolu_03");
        assert_eq!(calls[2].provider_round, 2);
        assert_eq!(calls[2].position, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_result_round_trip() {
        let (storage, turn_id) = make_turn().await;
        let record = storage
            .create_tool_call(new_call(turn_id, "toolu_03", 0))
            .await
            .unwrap();

        storage
            .update_tool_call_result(
                &record.call_id,
                turn_id,
                "file1\nfile2",
                "success",
                42,
                false,
                Some(22),
            )
            .await
            .unwrap();

        let calls = storage.list_tool_calls_by_turn(turn_id).await.unwrap();
        assert_eq!(calls.len(), 1);
        let updated = &calls[0];
        assert_eq!(updated.result.as_deref(), Some("file1\nfile2"));
        assert_eq!(updated.status.as_deref(), Some("success"));
        assert_eq!(updated.duration_ms, Some(42));
        assert!(!updated.truncated);
        assert_eq!(updated.total_bytes, Some(22));
        let envelope: swage_core::CompactToolReplay =
            serde_json::from_str(updated.model_result.as_deref().unwrap())
                .expect("model_result should be a compact replay envelope");
        assert_eq!(
            envelope.full_result_ref.as_deref(),
            Some(tool_result_reference(&turn_id.to_string(), &record.call_id).as_str())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_result_with_truncation() {
        let (storage, turn_id) = make_turn().await;
        let record = storage
            .create_tool_call(new_call(turn_id, "toolu_04", 0))
            .await
            .unwrap();

        storage
            .update_tool_call_result(
                &record.call_id,
                turn_id,
                "...truncated",
                "success",
                10,
                true,
                Some(50_000),
            )
            .await
            .unwrap();

        let calls = storage.list_tool_calls_by_turn(turn_id).await.unwrap();
        let updated = &calls[0];
        assert!(updated.truncated, "truncated flag should be set");
        assert_eq!(updated.total_bytes, Some(50_000));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_turn_returns_empty_for_unknown_turn() {
        let (storage, _) = make_turn().await;
        let unknown = Uuid::new_v4();
        let calls = storage.list_tool_calls_by_turn(unknown).await.unwrap();
        assert!(calls.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parent_call_id_persists_and_retrieves() {
        let (storage, turn_id) = make_turn().await;

        // Native call — no parent.
        let native = storage
            .create_tool_call(NewToolCall {
                parent_call_id: None,
                ..new_call(turn_id, "native_01", 0)
            })
            .await
            .unwrap();
        assert!(native.parent_call_id.is_none());

        // Sub-tool call — has a parent.
        let sub = storage
            .create_tool_call(NewToolCall {
                parent_call_id: Some("native_01".to_string()),
                ..new_call(turn_id, "sub_01", 1)
            })
            .await
            .unwrap();
        assert_eq!(sub.parent_call_id.as_deref(), Some("native_01"));

        // Verify round-trip via list.
        let calls = storage.list_tool_calls_by_turn(turn_id).await.unwrap();
        assert_eq!(calls.len(), 2);
        assert!(
            calls[0].parent_call_id.is_none(),
            "native call should have no parent"
        );
        assert_eq!(
            calls[1].parent_call_id.as_deref(),
            Some("native_01"),
            "sub-call should carry parent_call_id"
        );
    }

    /// get_tool_call returns the stored record with agent_id.
    #[tokio::test(flavor = "multi_thread")]
    async fn get_tool_call_returns_record_with_agent_id() {
        let (storage, turn_id) = make_turn().await;
        let created = storage
            .create_tool_call(new_call(turn_id, "toolu_get", 0))
            .await
            .unwrap();

        let fetched = storage.get_tool_call(created.id).await.unwrap();
        assert!(fetched.is_some(), "should find tool call by id");
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.agent_id, "agent-test");
        assert_eq!(fetched.call_id, "toolu_get");
        assert_eq!(fetched.tool, "shell");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_tool_call_by_turn_and_call_id_scopes_lookup_to_turn() {
        let (storage, first_turn_id) = make_turn().await;
        let first_turn = storage.get_turn(first_turn_id).await.unwrap().unwrap();
        let first_thread = ThreadStore::get(&storage, first_turn.thread_id)
            .await
            .unwrap()
            .unwrap();
        let second_thread = ThreadStore::create(
            &storage,
            NewThread {
                session_id: first_thread.session_id,
                agent_id: "test-agent".into(),
            },
        )
        .await
        .unwrap();
        let second_turn = storage
            .create_turn(NewTurn {
                thread_id: second_thread.id,
                agent_id: "test-agent".into(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let first = storage
            .create_tool_call(new_call(first_turn_id, "shared_call_id", 0))
            .await
            .unwrap();
        let second = storage
            .create_tool_call(new_call(second_turn.id, "shared_call_id", 0))
            .await
            .unwrap();

        let fetched = storage
            .get_tool_call_by_turn_and_call_id(first_turn_id, "shared_call_id")
            .await
            .unwrap()
            .expect("call should be found");

        assert_eq!(fetched.id, first.id);
        assert_ne!(fetched.id, second.id);
        assert!(
            storage
                .get_tool_call_by_turn_and_call_id(first_turn_id, "missing")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// get_tool_call returns None for unknown id.
    #[tokio::test(flavor = "multi_thread")]
    async fn get_tool_call_returns_none_for_unknown() {
        let (storage, _) = make_turn().await;
        let result = storage.get_tool_call(Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
    }

    /// create_tool_call persists agent_id and it round-trips through list_by_turn.
    #[tokio::test(flavor = "multi_thread")]
    async fn agent_id_persists_through_create_and_list() {
        let (storage, turn_id) = make_turn().await;
        let record = storage
            .create_tool_call(new_call(turn_id, "toolu_aid", 0))
            .await
            .unwrap();

        assert_eq!(record.agent_id, "agent-test");

        let calls = storage.list_tool_calls_by_turn(turn_id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].agent_id, "agent-test");
    }

    /// Full round-trip: create → update → list → verify all fields.
    #[tokio::test(flavor = "multi_thread")]
    async fn full_round_trip() {
        let (storage, turn_id) = make_turn().await;

        // Create two tool calls.
        let call_a = storage
            .create_tool_call(NewToolCall {
                turn_id,
                agent_id: "agent-test".into(),
                call_id: "toolu_a".into(),
                tool: "file_read".into(),
                arguments: r#"{"path":"/etc/hosts"}"#.into(),
                model_arguments: compact_arguments_from_raw(r#"{"path":"/etc/hosts"}"#),
                result: None,
                model_result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                provider_round: 1,
                position: 0,
                parent_call_id: None,
                display_summary: Some("/etc/hosts".into()),
            })
            .await
            .unwrap();

        let _call_b = storage
            .create_tool_call(NewToolCall {
                turn_id,
                agent_id: "agent-test".into(),
                call_id: "toolu_b".into(),
                tool: "shell".into(),
                arguments: r#"{"cmd":"pwd"}"#.into(),
                model_arguments: compact_arguments_from_raw(r#"{"cmd":"pwd"}"#),
                result: None,
                model_result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                provider_round: 2,
                position: 1,
                parent_call_id: None,
                display_summary: None,
            })
            .await
            .unwrap();

        // Update only call_a's result.
        storage
            .update_tool_call_result(
                &call_a.call_id,
                turn_id,
                "127.0.0.1 localhost",
                "success",
                5,
                false,
                Some(20),
            )
            .await
            .unwrap();

        let calls = storage.list_tool_calls_by_turn(turn_id).await.unwrap();
        assert_eq!(calls.len(), 2);

        // call_a should have a result now.
        assert_eq!(calls[0].call_id, "toolu_a");
        assert_eq!(calls[0].result.as_deref(), Some("127.0.0.1 localhost"));
        assert_eq!(calls[0].tool, "file_read");

        // call_b should be unchanged.
        assert_eq!(calls[1].call_id, "toolu_b");
        assert!(calls[1].result.is_none());
    }
}
