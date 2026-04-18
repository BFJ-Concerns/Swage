pub mod agent_message;
pub mod approval;
pub mod attachment;
pub mod audit;
pub mod channel_link;
pub mod context_snapshot;
pub mod delivery;
pub mod execution_host;
pub mod heartbeat;
pub mod lens;
pub mod material;
pub mod message;
pub mod migrations;
pub mod project;
pub mod schedule;
pub mod secret;
pub mod session;
pub mod shared_reference;
pub mod skill_activation;
pub mod team;
pub mod thread;
pub mod tool_call;
pub mod topology;
pub mod turn;

use std::path::Path;

use rusqlite::{Connection, ErrorCode};
use swage_core::StorageError;
use tokio::sync::Mutex;

use migrations::run_migrations;

/// SQLite-backed storage with WAL mode and async-safe connection access.
pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    /// Open a file-backed database at `path`, applying migrations on first run.
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

    /// Configure connection pragmas, run migrations, and wrap in Self.
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
        })
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
        tokio::task::block_in_place(|| f(&*guard)).map_err(map_rusqlite_error)
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

    #[tokio::test(flavor = "multi_thread")]
    async fn in_memory_opens_and_migrates() {
        let storage = SqliteStorage::in_memory().expect("in_memory should succeed");

        // Schema should be present — query the projects table.
        storage
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_, i64>(0))
            })
            .await
            .expect("projects table should exist after migration");
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
    async fn default_project_seeded() {
        let storage = SqliteStorage::in_memory().expect("in_memory should succeed");

        let count: i64 = storage
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM projects WHERE name = 'default'",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .expect("query should succeed");

        assert_eq!(count, 1, "exactly one default project should be seeded");
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
