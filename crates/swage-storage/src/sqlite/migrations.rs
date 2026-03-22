use rusqlite::{Connection, params};
use swage_core::StorageError;

/// Track which migrations have been applied.
const CREATE_MIGRATIONS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS _swage_migrations (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        name       TEXT    NOT NULL UNIQUE,
        applied_at TEXT    NOT NULL
    )
";

/// The ordered set of migrations to apply. Each entry is (name, sql).
///
/// Order is significant — migrations are applied in array order and never re-run.
fn migrations() -> Vec<(&'static str, &'static str)> {
    vec![
        ("001_initial_schema", INITIAL_SCHEMA),
        ("002_tool_calls", TOOL_CALLS),
        ("003_tool_calls_parent_call_id", TOOL_CALLS_PARENT_CALL_ID),
        ("004_projects_slug", PROJECTS_SLUG),
        ("005_sessions_cascade_on_project_delete", SESSIONS_CASCADE),
    ]
}

const INITIAL_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS projects (
        id             TEXT NOT NULL PRIMARY KEY,
        name           TEXT NOT NULL UNIQUE,
        workspace_path TEXT,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS sessions (
        id         TEXT NOT NULL PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id),
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS turns (
        id           TEXT NOT NULL PRIMARY KEY,
        session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        state        TEXT NOT NULL DEFAULT 'in_progress',
        created_at   TEXT NOT NULL,
        completed_at TEXT
    );

    CREATE TABLE IF NOT EXISTS messages (
        id         TEXT    NOT NULL PRIMARY KEY,
        turn_id    TEXT    NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
        role       TEXT    NOT NULL,
        content    TEXT    NOT NULL,
        position   INTEGER NOT NULL,
        created_at TEXT    NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_project_id ON sessions(project_id);
    CREATE INDEX IF NOT EXISTS idx_turns_session_id    ON turns(session_id);
    CREATE INDEX IF NOT EXISTS idx_turns_state         ON turns(state);
    CREATE INDEX IF NOT EXISTS idx_messages_turn_id    ON messages(turn_id);
";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn open_migrated() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        super::run_migrations(&conn).expect("migrations should succeed");
        conn
    }

    #[test]
    fn tool_calls_table_created() {
        let conn = open_migrated();
        // Querying the table is the cleanest proof it exists.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .expect("tool_calls table should exist after migration");
        assert_eq!(count, 0, "fresh DB should have no rows");
    }

    #[test]
    fn tool_calls_migration_idempotent() {
        let conn = open_migrated();
        // Running migrations a second time must not error (already-applied guard).
        super::run_migrations(&conn).expect("second migration run should be idempotent");
        // Table should still be queryable.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .expect("table should still exist");
    }

    #[test]
    fn existing_data_unaffected_after_migration() {
        // Insert data into pre-existing tables, then re-run migrations.
        let conn = open_migrated();

        // Retrieve the seeded default project id.
        let project_id: String = conn
            .query_row("SELECT id FROM projects WHERE name='default'", [], |r| {
                r.get(0)
            })
            .expect("default project should be seeded");

        // Insert a session and a turn so we have data to protect.
        let session_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO sessions (id, project_id, created_at, updated_at) VALUES (?1,?2,?3,?3)",
            rusqlite::params![session_id, project_id, now],
        )
        .expect("insert session");

        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, session_id, state, created_at) VALUES (?1,?2,'in_progress',?3)",
            rusqlite::params![turn_id, session_id, now],
        )
        .expect("insert turn");

        // Re-run migrations (idempotent).
        super::run_migrations(&conn).expect("re-run should succeed");

        // Original data must still be present.
        let session_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .expect("sessions query");
        assert_eq!(session_count, 1, "session should survive migration re-run");

        let turn_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
            .expect("turns query");
        assert_eq!(turn_count, 1, "turn should survive migration re-run");
    }

    #[test]
    fn tool_calls_turn_id_index_exists() {
        let conn = open_migrated();
        // sqlite_master lists all schema objects — verify the index was created.
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
        let conn = open_migrated();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_tool_calls_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(idx_count, 1, "idx_tool_calls_call_id index should exist");
    }

    #[test]
    fn tool_calls_parent_call_id_column_exists() {
        let conn = open_migrated();
        // Verify the column was added by migration 003.
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tool_calls') WHERE name='parent_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "parent_call_id column should exist after migration 003");
    }

    #[test]
    fn tool_calls_parent_call_id_nullable() {
        // Existing rows should survive migration with NULL in the new column.
        let conn = open_migrated();

        let project_id: String = conn
            .query_row("SELECT id FROM projects WHERE name='default'", [], |r| r.get(0))
            .expect("default project");
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO sessions (id, project_id, created_at, updated_at) VALUES (?1,?2,?3,?3)",
            rusqlite::params![session_id, project_id, now],
        ).unwrap();
        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, session_id, state, created_at) VALUES (?1,?2,'in_progress',?3)",
            rusqlite::params![turn_id, session_id, now],
        ).unwrap();

        // Insert without parent_call_id — should default to NULL.
        let call_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO tool_calls
             (id, turn_id, call_id, tool, arguments, truncated, position, created_at)
             VALUES (?1,?2,?3,'shell','{}',0,0,?4)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), turn_id, call_id, now],
        ).expect("insert without parent_call_id");

        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_call_id FROM tool_calls WHERE call_id = ?1",
                rusqlite::params![call_id],
                |r| r.get(0),
            )
            .expect("select parent_call_id");
        assert!(parent.is_none(), "parent_call_id should be NULL for native calls");
    }

    // ---------------------------------------------------------------------------
    // Migration 004: projects slug column
    // ---------------------------------------------------------------------------

    #[test]
    fn slug_column_exists_after_migration() {
        let conn = open_migrated();
        // pragma_table_info lists one row per column.
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('projects') WHERE name='slug'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "slug column should exist after migration 004");
    }

    #[test]
    fn default_project_has_slug_default() {
        let conn = open_migrated();
        let slug: String = conn
            .query_row(
                "SELECT slug FROM projects WHERE name = 'default'",
                [],
                |r| r.get(0),
            )
            .expect("default project should exist with slug");
        assert_eq!(slug, "default", "default project slug should be 'default'");
    }

    #[test]
    fn slug_uniqueness_constraint_enforced() {
        let conn = open_migrated();
        // Retrieve the seeded default project id for a valid project_id reference.
        let now = chrono::Utc::now().to_rfc3339();

        // Insert a project with slug "alpha" — should succeed.
        conn.execute(
            "INSERT INTO projects (id, name, slug, workspace_path, created_at, updated_at)
             VALUES (?1, 'alpha', 'alpha', NULL, ?2, ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), now],
        )
        .expect("first insert should succeed");

        // Inserting another row with the same slug must fail.
        let result = conn.execute(
            "INSERT INTO projects (id, name, slug, workspace_path, created_at, updated_at)
             VALUES (?1, 'beta', 'alpha', NULL, ?2, ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), now],
        );
        assert!(result.is_err(), "duplicate slug should violate the unique index");
    }

    #[test]
    fn slug_migration_idempotent() {
        let conn = open_migrated();
        // Running migrations a second time must not error.
        super::run_migrations(&conn).expect("second migration run should be idempotent");
        // slug column must still be queryable.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('projects') WHERE name='slug'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query after idempotent re-run");
        assert_eq!(count, 1, "slug column should still exist after idempotent re-run");
    }

    #[test]
    fn tool_calls_foreign_key_cascade_on_turn_delete() {
        let conn = open_migrated();

        // Look up the seeded default project.
        let project_id: String = conn
            .query_row("SELECT id FROM projects WHERE name='default'", [], |r| {
                r.get(0)
            })
            .expect("default project");

        let now = chrono::Utc::now().to_rfc3339();
        let session_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO sessions (id, project_id, created_at, updated_at) VALUES (?1,?2,?3,?3)",
            rusqlite::params![session_id, project_id, now],
        )
        .unwrap();

        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, session_id, state, created_at) VALUES (?1,?2,'in_progress',?3)",
            rusqlite::params![turn_id, session_id, now],
        )
        .unwrap();

        // Insert a tool call referencing the turn.
        let call_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO tool_calls
             (id, turn_id, call_id, tool, arguments, truncated, position, created_at)
             VALUES (?1,?2,?3,'shell','{\"cmd\":\"ls\"}',0,0,?4)",
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
}

/// Adds nullable parent_call_id column to tool_calls.
///
/// Uses ALTER TABLE so existing rows remain valid (NULL = native top-level call).
const TOOL_CALLS_PARENT_CALL_ID: &str = "
    ALTER TABLE tool_calls ADD COLUMN parent_call_id TEXT;
";

/// Adds a slug column to projects, backfills existing rows, and creates a
/// unique index.
///
/// SQLite's ALTER TABLE cannot add NOT NULL without a DEFAULT, so we provide
/// an empty-string sentinel as the default and immediately overwrite all rows.
/// The UNIQUE index enforces uniqueness going forward.
///
/// Slug derivation for non-default projects: lowercase the name and replace
/// spaces with hyphens. Full non-alphanumeric stripping is applied by the
/// application layer when creating new projects; migration covers existing rows
/// with the simpler SQLite-available transform.
const PROJECTS_SLUG: &str = "
    ALTER TABLE projects ADD COLUMN slug TEXT NOT NULL DEFAULT '';
    UPDATE projects SET slug = 'default' WHERE name = 'default';
    UPDATE projects SET slug = lower(replace(name, ' ', '-')) WHERE name != 'default' AND slug = '';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_slug ON projects(slug);
";

/// Recreate the sessions table with ON DELETE CASCADE on the project_id FK.
///
/// SQLite doesn't support ALTER CONSTRAINT, so we recreate the table. The
/// original schema omitted CASCADE, which caused FK violations when deleting
/// a project that still had sessions.
const SESSIONS_CASCADE: &str = "
    CREATE TABLE sessions_new (
        id         TEXT NOT NULL PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    INSERT INTO sessions_new SELECT id, project_id, created_at, updated_at FROM sessions;
    DROP TABLE sessions;
    ALTER TABLE sessions_new RENAME TO sessions;
    CREATE INDEX IF NOT EXISTS idx_sessions_project_id ON sessions(project_id);
";

const TOOL_CALLS: &str = "
    CREATE TABLE IF NOT EXISTS tool_calls (
        id          TEXT    NOT NULL PRIMARY KEY,
        turn_id     TEXT    NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
        call_id     TEXT    NOT NULL,
        tool        TEXT    NOT NULL,
        arguments   TEXT    NOT NULL,
        result      TEXT,
        status      TEXT,
        duration_ms INTEGER,
        truncated   INTEGER NOT NULL DEFAULT 0,
        total_bytes INTEGER,
        position    INTEGER NOT NULL,
        created_at  TEXT    NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_tool_calls_turn_id ON tool_calls(turn_id);
    CREATE INDEX IF NOT EXISTS idx_tool_calls_call_id ON tool_calls(call_id);
";

/// Apply all pending migrations to `conn`.
///
/// Migrations are tracked by name in `_swage_migrations`. Already-applied
/// migrations are skipped, so this is safe to call on every startup.
pub fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    // Ensure the tracking table exists first.
    conn.execute_batch(CREATE_MIGRATIONS_TABLE)
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

    for (name, sql) in migrations() {
        // Check whether this migration has already been applied.
        let already_applied: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM _swage_migrations WHERE name = ?1",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

        if already_applied {
            continue;
        }

        // Apply migration inside a transaction for atomicity.
        conn.execute_batch(&format!("BEGIN; {sql} COMMIT;"))
            .map_err(|e| {
                StorageError::MigrationFailed(format!("migration '{name}' failed: {e}"))
            })?;

        conn.execute(
            "INSERT INTO _swage_migrations (name, applied_at) VALUES (?1, datetime('now'))",
            params![name],
        )
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;
    }

    // Seed the default project if it does not already exist.
    seed_default_project(conn)
}

/// Insert the default project on first run. Idempotent via INSERT OR IGNORE.
fn seed_default_project(conn: &Connection) -> Result<(), StorageError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO projects (id, name, slug, workspace_path, created_at, updated_at)
         VALUES (?1, 'default', 'default', NULL, ?2, ?2)",
        params![id, now],
    )
    .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

    Ok(())
}
