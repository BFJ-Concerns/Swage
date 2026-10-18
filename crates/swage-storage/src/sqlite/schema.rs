//! The store's schema as one definition: `schema.sql` holds every table,
//! index, trigger and virtual table, built whole into an empty store and
//! fingerprinted against an existing one. There is no upgrade path during
//! the private alpha: a store built from an earlier definition is refused
//! on open and rebuilt (AGENTS.md, Status).

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use swage_core::StorageError;

/// The DDL that creates every object in the store, in dependency order.
/// Editing it is the whole of a schema change: there is nothing to add
/// elsewhere, and the fingerprint check below refuses stores built from the
/// previous text.
const SCHEMA: &str = include_str!("schema.sql");

/// Build the schema into an empty store, or check that a populated store was
/// built from this definition.
pub fn ensure_schema(conn: &Connection) -> Result<(), StorageError> {
    let populated: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%')",
            [],
            |row| row.get(0),
        )
        .map_err(schema_failed)?;
    if !populated {
        return build_schema(conn);
    }
    if schema_fingerprint(conn)? != *expected_fingerprint()? {
        return Err(StorageError::UnknownSchema {
            message: "the store schema does not match this build; rebuild this data root".into(),
        });
    }
    Ok(())
}

/// Construct the whole schema and its seed rows in one transaction, so a
/// first start cut off part-way leaves the file empty for the next start to
/// build, never a partial store for the fingerprint check to refuse.
fn build_schema(conn: &Connection) -> Result<(), StorageError> {
    let transaction = conn.unchecked_transaction().map_err(schema_failed)?;
    transaction.execute_batch(SCHEMA).map_err(schema_failed)?;
    seed_general_scope(&transaction)?;
    transaction.commit().map_err(schema_failed)
}

/// The fingerprint a store built from this definition carries, computed once
/// per process from a fresh in-memory build.
fn expected_fingerprint() -> Result<&'static Vec<u8>, StorageError> {
    static EXPECTED: OnceLock<Result<Vec<u8>, StorageError>> = OnceLock::new();
    EXPECTED
        .get_or_init(|| {
            let fresh = Connection::open_in_memory()
                .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
            build_schema(&fresh)?;
            schema_fingerprint(&fresh)
        })
        .as_ref()
        .map_err(|e| StorageError::SchemaFailed(e.to_string()))
}

/// A digest of every object definition the store holds, as SQLite stores
/// it, so two stores built from the same text fingerprint alike and any
/// difference in a definition — a column, a constraint, an index — refuses.
fn schema_fingerprint(conn: &Connection) -> Result<Vec<u8>, StorageError> {
    let mut statement = conn
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .map_err(schema_failed)?;
    let definitions = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(schema_failed)?;
    let mut hash = Sha256::new();
    for definition in definitions {
        let definition = definition.map_err(schema_failed)?;
        let encoded = serde_json::to_vec(&definition).map_err(schema_failed)?;
        hash.update(encoded);
    }
    Ok(hash.finalize().to_vec())
}

/// Insert the general scope — the scope of every session started without a
/// topic — into a store being built.
fn seed_general_scope(conn: &Connection) -> Result<(), StorageError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO topics (id, name, is_general, slug, created_at, updated_at)
         VALUES (?1, 'general', 1, 'general', ?2, ?2)",
        params![id, now],
    )
    .map_err(schema_failed)?;
    Ok(())
}

fn schema_failed(error: impl std::fmt::Display) -> StorageError {
    StorageError::SchemaFailed(error.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use swage_core::StorageError;

    fn build_in_memory() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        super::ensure_schema(&conn).expect("the schema builds on an empty connection");
        conn
    }

    // Convenience: insert a session (requires agent_id).
    fn insert_session(conn: &Connection, topic_id: &str, now: &str) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO sessions (id, topic_id, agent_id, created_at, updated_at)
             VALUES (?1,?2,'agent-test',?3,?3)",
            rusqlite::params![session_id, topic_id, now],
        )
        .expect("insert session");
        session_id
    }

    // Convenience: insert a thread under a session.
    fn insert_thread(conn: &Connection, session_id: &str, now: &str) -> String {
        let thread_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
             VALUES (?1,?2,'agent-test',?3,?3)",
            rusqlite::params![thread_id, session_id, now],
        )
        .expect("insert thread");
        thread_id
    }

    // Convenience: insert a turn under a thread.
    fn insert_turn(conn: &Connection, thread_id: &str, now: &str) -> String {
        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, thread_id, agent_id, state, created_at)
             VALUES (?1,?2,'agent-test','pending',?3)",
            rusqlite::params![turn_id, thread_id, now],
        )
        .expect("insert turn");
        turn_id
    }

    fn default_topic_id(conn: &Connection) -> String {
        conn.query_row("SELECT id FROM topics WHERE is_general = 1", [], |r| {
            r.get(0)
        })
        .expect("general scope")
    }

    // ---------------------------------------------------------------------------
    // Building a store, and refusing one this definition did not build
    // ---------------------------------------------------------------------------

    fn open_store(path: &std::path::Path) -> Result<super::super::SqliteStorage, StorageError> {
        super::super::SqliteStorage::open(path)
    }

    fn table_count(path: &std::path::Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_build_cut_off_before_commit_leaves_nothing_and_the_next_open_builds_whole() {
        let dir = tempfile::tempdir().expect("temporary store");
        let path = dir.path().join("interrupted.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("BEGIN; {}", super::SCHEMA))
                .expect("the definitions run inside the open transaction");
            // Dropped without COMMIT: the process died before the build landed.
        }
        assert_eq!(
            table_count(&path),
            0,
            "an uncommitted build leaves no object behind"
        );

        let storage = open_store(&path).expect("the next open builds the store whole");
        drop(storage);
        let general_scopes: i64 = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM topics WHERE is_general = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(general_scopes, 1, "the seed rows land with the definitions");
    }

    #[test]
    fn reopening_a_built_store_keeps_its_rows() {
        let dir = tempfile::tempdir().expect("temporary store");
        let path = dir.path().join("reopened.db");
        {
            let conn = Connection::open(&path).unwrap();
            super::ensure_schema(&conn).unwrap();
            let topic_id = default_topic_id(&conn);
            let now = chrono::Utc::now().to_rfc3339();
            let session_id = insert_session(&conn, &topic_id, &now);
            let thread_id = insert_thread(&conn, &session_id, &now);
            insert_turn(&conn, &thread_id, &now);
        }
        let conn = Connection::open(&path).unwrap();
        super::ensure_schema(&conn).expect("a store this definition built reopens");
        let turns: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            turns, 1,
            "reopening checks the definitions and touches no row"
        );
    }

    #[test]
    fn changed_schema_definition_is_refused_on_open() {
        let dir = tempfile::tempdir().expect("temporary store");
        let path = dir.path().join("different-definitions.db");
        {
            let conn = Connection::open(&path).unwrap();
            super::ensure_schema(&conn).unwrap();
            conn.execute_batch("ALTER TABLE topics ADD COLUMN obsolete_column TEXT;")
                .unwrap();
        }
        assert!(
            matches!(open_store(&path), Err(StorageError::UnknownSchema { .. })),
            "a store with a different schema definition must be refused on open"
        );
    }

    #[test]
    fn store_missing_a_table_this_build_defines_is_refused_on_open() {
        let dir = tempfile::tempdir().expect("temporary store");
        let path = dir.path().join("missing-table.db");
        {
            let conn = Connection::open(&path).unwrap();
            super::ensure_schema(&conn).unwrap();
            conn.execute_batch("DROP TABLE channel_link_codes;")
                .unwrap();
        }
        let error = open_store(&path)
            .err()
            .expect("the store lacks a table this build reads");
        match error {
            StorageError::UnknownSchema { message } => {
                assert!(message.contains("does not match this build"), "{message}");
                assert!(message.contains("rebuild this data root"), "{message}");
            }
            other => panic!("expected UnknownSchema, got {other:?}"),
        }
    }

    #[test]
    fn store_carrying_a_migration_history_table_is_refused_on_open() {
        // A store the earlier numbered-migration builds made records its
        // history in a table this definition does not create.
        let dir = tempfile::tempdir().expect("temporary store");
        let path = dir.path().join("migrated.db");
        {
            let conn = Connection::open(&path).unwrap();
            super::ensure_schema(&conn).unwrap();
            conn.execute_batch(
                "CREATE TABLE _swage_migrations (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     name TEXT NOT NULL UNIQUE,
                     applied_at TEXT NOT NULL
                 );
                 INSERT INTO _swage_migrations (name, applied_at)
                 VALUES ('001_complete_schema', datetime('now'));",
            )
            .unwrap();
        }
        assert!(
            matches!(open_store(&path), Err(StorageError::UnknownSchema { .. })),
            "a store another build's migrations made must be refused on open"
        );
    }

    #[test]
    fn tool_calls_table_created() {
        let conn = build_in_memory();
        // Querying the table is the cleanest proof it exists.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .expect("tool_calls table should exist");
        assert_eq!(count, 0, "fresh DB should have no rows");
    }

    // ---------------------------------------------------------------------------
    // Index checks
    // ---------------------------------------------------------------------------

    #[test]
    fn tool_calls_turn_id_index_exists() {
        let conn = build_in_memory();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_tool_calls_turn_id'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(idx_count, 1, "idx_tool_calls_turn_id index should exist");
    }

    #[test]
    fn tool_calls_call_id_index_exists() {
        let conn = build_in_memory();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_tool_calls_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(idx_count, 1, "idx_tool_calls_call_id index should exist");
    }

    // ---------------------------------------------------------------------------
    // Column existence: tool_calls
    // ---------------------------------------------------------------------------

    #[test]
    fn tool_calls_parent_call_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tool_calls') WHERE name='parent_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "parent_call_id column should exist");
    }

    #[test]
    fn tool_calls_parent_call_id_nullable() {
        // Existing rows should have NULL parent_call_id when not set.
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert without parent_call_id — should default to NULL.
        let call_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO tool_calls
             (id, turn_id, call_id, tool, arguments, agent_id, truncated, position, created_at)
             VALUES (?1,?2,?3,'shell','{}','agent-test',0,0,?4)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), turn_id, call_id, now],
        )
        .expect("insert without parent_call_id");

        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_call_id FROM tool_calls WHERE call_id = ?1",
                rusqlite::params![call_id],
                |r| r.get(0),
            )
            .expect("select parent_call_id");
        assert!(
            parent.is_none(),
            "parent_call_id should be NULL for native calls"
        );
    }

    // ---------------------------------------------------------------------------
    // Topics: slug column
    // ---------------------------------------------------------------------------

    #[test]
    fn sessions_carry_an_origin_and_an_archive_stamp_and_runs_their_dispatching_session() {
        let conn = build_in_memory();
        for column in ["origin_kind", "origin_source_id", "archived_at"] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = ?1",
                    [column],
                    |r| r.get(0),
                )
                .expect("pragma_table_info query");
            assert_eq!(present, 1, "sessions.{column} should exist");
        }
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('workflow_runs') WHERE name = 'dispatching_session_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(present, 1);
        // A session inserted without an origin is a direct one.
        let topic_id = default_topic_id(&conn);
        conn.execute(
            "INSERT INTO sessions (id, topic_id, agent_id, created_at, updated_at)
             VALUES ('s-1', ?1, 'gateway', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [topic_id],
        )
        .expect("insert session");
        let origin: String = conn
            .query_row(
                "SELECT origin_kind FROM sessions WHERE id = 's-1'",
                [],
                |r| r.get(0),
            )
            .expect("origin_kind");
        assert_eq!(origin, "direct");
    }

    #[test]
    fn slug_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('topics') WHERE name='slug'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "slug column should exist");
    }

    #[test]
    fn general_scope_is_seeded_with_its_slug() {
        let conn = build_in_memory();
        let slug: String = conn
            .query_row("SELECT slug FROM topics WHERE is_general = 1", [], |r| {
                r.get(0)
            })
            .expect("the general scope should be seeded");
        assert_eq!(slug, "general");
    }

    #[test]
    fn slug_uniqueness_constraint_enforced() {
        let conn = build_in_memory();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO topics (id, name, slug, created_at, updated_at)
             VALUES (?1, 'alpha', 'alpha', ?2, ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), now],
        )
        .expect("first insert should succeed");

        let result = conn.execute(
            "INSERT INTO topics (id, name, slug, created_at, updated_at)
             VALUES (?1, 'beta', 'alpha', ?2, ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), now],
        );
        assert!(
            result.is_err(),
            "duplicate slug should violate the unique index"
        );
    }

    // ---------------------------------------------------------------------------
    // New-schema tests: threads table, agent_id columns, updated FK chains.
    // ---------------------------------------------------------------------------

    #[test]
    fn threads_table_created() {
        let conn = build_in_memory();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))
            .expect("threads table should exist");
        assert_eq!(count, 0, "fresh DB should have no thread rows");
    }

    #[test]
    fn sessions_agent_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "sessions.agent_id column should exist");
    }

    #[test]
    fn sessions_agent_id_index_exists() {
        let conn = build_in_memory();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_sessions_agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(idx_count, 1, "idx_sessions_agent_id index should exist");
    }

    #[test]
    fn threads_agent_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('threads') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "threads.agent_id column should exist");
    }

    #[test]
    fn threads_state_column_exists_with_idle_default() {
        let conn = build_in_memory();
        // Verify column is present.
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('threads') WHERE name='state'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "threads.state column should exist");

        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);

        // Insert a thread without specifying state — should default to 'idle'.
        let thread_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
             VALUES (?1,?2,'agent-test',?3,?3)",
            rusqlite::params![thread_id, session_id, now],
        )
        .expect("insert thread without explicit state");

        let state: String = conn
            .query_row(
                "SELECT state FROM threads WHERE id=?1",
                rusqlite::params![thread_id],
                |r| r.get(0),
            )
            .expect("query thread state");
        assert_eq!(state, "idle", "threads.state should default to 'idle'");
    }

    #[test]
    fn turns_thread_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('turns') WHERE name='thread_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "turns.thread_id column should exist");
    }

    #[test]
    fn turns_agent_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('turns') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "turns.agent_id column should exist");
    }

    #[test]
    fn turns_state_defaults_to_pending() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        // Insert a turn without specifying state — should default to 'pending'.
        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, thread_id, agent_id, created_at)
             VALUES (?1,?2,'agent-test',?3)",
            rusqlite::params![turn_id, thread_id, now],
        )
        .expect("insert turn without explicit state");

        let state: String = conn
            .query_row(
                "SELECT state FROM turns WHERE id=?1",
                rusqlite::params![turn_id],
                |r| r.get(0),
            )
            .expect("query turn state");
        assert_eq!(state, "pending", "turns.state should default to 'pending'");
    }

    #[test]
    fn messages_thread_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='thread_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.thread_id column should exist");
    }

    #[test]
    fn messages_agent_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.agent_id column should exist");
    }

    #[test]
    fn messages_tool_call_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='tool_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.tool_call_id column should exist");
    }

    #[test]
    fn messages_tool_name_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='tool_name'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.tool_name column should exist");
    }

    #[test]
    fn messages_role_rejects_unknown_values() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        conn.execute(
            "INSERT INTO messages
             (id, thread_id, role, origin, content, position, agent_id, created_at)
             VALUES (?1, ?2, 'assistant', 'assistant', 'ok', 0, 'agent-test', ?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), thread_id, now],
        )
        .expect("known role should insert");

        let result = conn.execute(
            "INSERT INTO messages
             (id, thread_id, role, origin, content, position, agent_id, created_at)
             VALUES (?1, ?2, 'moderator', 'user', 'bad', 1, 'agent-test', ?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), thread_id, now],
        );
        assert!(result.is_err(), "unknown messages.role should fail closed");
    }

    #[test]
    fn tool_calls_agent_id_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tool_calls') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "tool_calls.agent_id column should exist");
    }

    #[test]
    fn tool_calls_replay_columns_exist() {
        let conn = build_in_memory();
        for column in &["model_arguments", "model_result", "provider_round"] {
            let col_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('tool_calls') WHERE name=?1",
                    rusqlite::params![column],
                    |r| r.get(0),
                )
                .expect("pragma_table_info query");
            assert_eq!(col_count, 1, "tool_calls.{column} column should exist");
        }
    }

    #[test]
    fn threads_indices_exist() {
        let conn = build_in_memory();
        for idx in &["idx_threads_session_id", "idx_threads_agent_id"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    rusqlite::params![idx],
                    |r| r.get(0),
                )
                .expect("schema query");
            assert_eq!(count, 1, "{idx} should exist");
        }
    }

    // ---------------------------------------------------------------------------
    // Cascade delete tests for the full FK chain
    // ---------------------------------------------------------------------------

    #[test]
    fn cascade_session_delete_removes_threads() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let _thread_id = insert_thread(&conn, &session_id, &now);

        conn.execute(
            "DELETE FROM sessions WHERE id=?1",
            rusqlite::params![session_id],
        )
        .expect("delete session");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "threads should cascade-delete when session is deleted"
        );
    }

    #[test]
    fn cascade_thread_delete_removes_turns() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let _turn_id = insert_turn(&conn, &thread_id, &now);

        conn.execute(
            "DELETE FROM threads WHERE id=?1",
            rusqlite::params![thread_id],
        )
        .expect("delete thread");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "turns should cascade-delete when thread is deleted"
        );
    }

    #[test]
    fn cascade_thread_delete_removes_messages() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        conn.execute(
            "INSERT INTO messages (id, thread_id, role, origin, content, position, agent_id, created_at)
             VALUES (?1,?2,'user','user','hello',0,'agent-test',?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), thread_id, now],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM threads WHERE id=?1",
            rusqlite::params![thread_id],
        )
        .expect("delete thread");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "messages should cascade-delete when thread is deleted"
        );
    }

    #[test]
    fn deleting_turn_orphans_but_keeps_messages() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);
        let message_id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT INTO messages (id, thread_id, turn_id, role, origin, content, position, agent_id, created_at)
             VALUES (?1,?2,?3,'assistant','assistant','hello',0,'agent-test',?4)",
            rusqlite::params![message_id, thread_id, turn_id, now],
        )
        .expect("insert message");

        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let retained_turn_id: Option<String> = conn
            .query_row(
                "SELECT turn_id FROM messages WHERE id=?1",
                rusqlite::params![message_id],
                |r| r.get(0),
            )
            .expect("message should survive turn deletion");
        assert!(
            retained_turn_id.is_none(),
            "deleting a turn should retain its messages with turn_id set to NULL"
        );
    }

    // ---------------------------------------------------------------------------
    // context_snapshots table
    // ---------------------------------------------------------------------------

    #[test]
    fn context_snapshots_table_created() {
        let conn = build_in_memory();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM context_snapshots", [], |r| r.get(0))
            .expect("context_snapshots table should exist");
        assert_eq!(count, 0, "fresh DB should have no snapshot rows");
    }

    #[test]
    fn context_snapshots_cascade_delete_with_turn() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert a snapshot for the turn.
        conn.execute(
            "INSERT INTO context_snapshots (turn_id, report_json, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![turn_id, r#"{"total_tokens":100}"#, now],
        )
        .expect("insert snapshot");

        // Deleting the turn should cascade to context_snapshots.
        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM context_snapshots", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "context_snapshots should be deleted via turn cascade"
        );
    }

    // ---------------------------------------------------------------------------
    // cost_microcents column
    // ---------------------------------------------------------------------------

    #[test]
    fn turns_cost_microcents_column_exists() {
        let conn = build_in_memory();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('turns') WHERE name='cost_microcents'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "turns.cost_microcents column should exist");
    }

    #[test]
    fn turns_cost_microcents_is_nullable() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        // Insert a turn without cost — should default to NULL.
        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, thread_id, agent_id, state, created_at)
             VALUES (?1,?2,'agent-test','pending',?3)",
            rusqlite::params![turn_id, thread_id, now],
        )
        .expect("insert turn");

        let cost: Option<i64> = conn
            .query_row(
                "SELECT cost_microcents FROM turns WHERE id=?1",
                rusqlite::params![turn_id],
                |r| r.get(0),
            )
            .expect("select cost_microcents");
        assert!(cost.is_none(), "cost_microcents should be NULL by default");
    }

    // ---------------------------------------------------------------------------
    // approval_requests table
    // ---------------------------------------------------------------------------

    #[test]
    fn approval_requests_table_created() {
        let conn = build_in_memory();
        // The table exists and is queryable.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM approval_requests", [], |r| r.get(0))
            .expect("approval_requests table should exist");
        assert_eq!(count, 0, "fresh DB should have no approval request rows");
    }

    #[test]
    fn approval_requests_cascade_delete_on_turn_delete() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert a minimal approval request referencing the turn.
        conn.execute(
            "INSERT INTO approval_requests
             (id, turn_id, agent_id, kind, prompt, state, timeout_at, timeout_policy, created_at)
             VALUES (?1,?2,'agent-test','tool','Approve shell?','pending',?3,'interrupt',?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), turn_id, now],
        )
        .expect("insert approval request");

        // Deleting the turn should cascade to approval_requests.
        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM approval_requests", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "approval_requests should cascade-delete with the turn"
        );
    }

    #[test]
    fn approval_requests_indices_exist() {
        let conn = build_in_memory();
        for idx in &[
            "idx_approval_requests_turn_id",
            "idx_approval_requests_agent_id",
            "idx_approval_requests_state",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    rusqlite::params![idx],
                    |r| r.get(0),
                )
                .expect("schema query");
            assert_eq!(count, 1, "{idx} should exist");
        }
    }

    #[test]
    fn tool_calls_foreign_key_cascade_on_turn_delete() {
        let conn = build_in_memory();
        let topic_id = default_topic_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &topic_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert a tool call referencing the turn.
        let call_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO tool_calls
             (id, turn_id, call_id, tool, arguments, agent_id, truncated, position, created_at)
             VALUES (?1,?2,?3,'shell','{\"cmd\":\"ls\"}','agent-test',0,0,?4)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), turn_id, call_id, now],
        )
        .expect("insert tool call");

        // Deleting the turn should cascade to tool_calls.
        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(remaining, 0, "tool_calls should be deleted via cascade");
    }

    #[test]
    fn approval_memory_table_created() {
        let conn = build_in_memory();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM approval_memory", [], |r| r.get(0))
            .expect("approval_memory table should exist");
        assert_eq!(count, 0);
    }
}
