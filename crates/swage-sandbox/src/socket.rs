//! Unix domain socket lifecycle management.
//!
//! Creates socket files in a temporary directory, detects stale sockets from
//! previous crashes, and ensures cleanup on shutdown.

use std::path::{Path, PathBuf};

use swage_core::SandboxError;
use tokio::net::UnixListener;

/// Manages the lifecycle of a Unix domain socket file.
///
/// Created via [`SocketManager::create`]. Owns the socket path and removes
/// it automatically when dropped — sockets do not clean themselves up and
/// stale entries block a fresh bind on the same path.
pub struct SocketManager {
    path: PathBuf,
}

impl SocketManager {
    /// Creates a Unix domain socket in the system temp directory.
    ///
    /// The socket file is named `{prefix}-{uuid}.sock` so multiple sandboxes
    /// can coexist without path collisions. Returns the manager (which owns
    /// cleanup) and the bound listener ready to accept connections.
    pub fn create(prefix: &str) -> Result<(Self, UnixListener), SandboxError> {
        let path = std::env::temp_dir()
            .join(format!("{}-{}.sock", prefix, uuid::Uuid::new_v4()));

        let listener = UnixListener::bind(&path).map_err(|e| SandboxError::SpawnFailed {
            message: format!("failed to bind Unix socket at {}: {e}", path.display()),
        })?;

        tracing::debug!(path = %path.display(), "created Unix domain socket");
        Ok((Self { path }, listener))
    }

    /// Returns the path to the managed socket file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns true if the socket file at `path` is stale.
    ///
    /// A socket is considered stale when it exists on disk but no process is
    /// listening on it — a connection attempt returns `ECONNREFUSED`. Sockets
    /// left behind by a crashed process match this pattern.
    ///
    /// Returns false if the path does not exist, the socket is still active,
    /// or the connection result is ambiguous.
    pub fn is_stale(path: &Path) -> bool {
        if !path.exists() {
            return false;
        }
        // Use a synchronous connect attempt — async would require a running
        // runtime which is not guaranteed at this call site.
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => false,  // Someone is listening — active socket.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => true,
            Err(_) => false, // Other error — cannot confidently call it stale.
        }
    }

    /// Removes the socket file at `path`, warning on failure.
    ///
    /// Failure is not fatal — the OS will eventually reclaim temp files, and
    /// a stale socket only blocks a new bind on the exact same path. The
    /// unique UUID naming convention makes same-path collisions unlikely.
    pub fn cleanup(path: &Path) {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to remove socket file during cleanup"
                );
            }
        }
    }

    /// Scans the temp directory and removes stale socket files whose names
    /// start with `{prefix}-`.
    ///
    /// Intended for use at startup to clean up sockets left by a previous
    /// crash. Only removes sockets that pass the [`is_stale`] check —
    /// active sockets from concurrently running sandboxes are left alone.
    pub fn cleanup_stale(prefix: &str) {
        let temp_dir = std::env::temp_dir();
        let name_prefix = format!("{prefix}-");

        let entries = match std::fs::read_dir(&temp_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    dir = %temp_dir.display(),
                    error = %e,
                    "failed to read temp dir during stale socket cleanup"
                );
                return;
            }
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(&name_prefix) && name_str.ends_with(".sock") {
                let path = entry.path();
                if Self::is_stale(&path) {
                    tracing::debug!(path = %path.display(), "removing stale socket");
                    Self::cleanup(&path);
                }
            }
        }
    }
}

impl Drop for SocketManager {
    fn drop(&mut self) {
        Self::cleanup(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PREFIX: &str = "swage-test-socket";

    // -- Create ---------------------------------------------------------------

    #[tokio::test]
    async fn create_produces_socket_file() {
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX)
            .expect("create should succeed");

        assert!(mgr.path().exists(), "socket file should exist after create");

        // path() should point at the file we just created.
        assert!(mgr.path().is_absolute(), "socket path should be absolute");
    }

    #[tokio::test]
    async fn socket_name_includes_prefix() {
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX)
            .expect("create should succeed");

        let filename = mgr.path().file_name().unwrap().to_string_lossy();
        assert!(
            filename.starts_with(TEST_PREFIX),
            "socket filename should start with prefix, got: {filename}"
        );
        assert!(
            filename.ends_with(".sock"),
            "socket filename should end with .sock, got: {filename}"
        );
    }

    // -- Drop cleanup ---------------------------------------------------------

    #[tokio::test]
    async fn drop_removes_socket_file() {
        let path = {
            let (mgr, _listener) = SocketManager::create(TEST_PREFIX)
                .expect("create should succeed");
            let p = mgr.path().to_path_buf();
            assert!(p.exists(), "socket file should exist before drop");
            p
            // mgr is dropped here
        };

        assert!(!path.exists(), "socket file should be removed after drop");
    }

    // -- Manual cleanup -------------------------------------------------------

    #[tokio::test]
    async fn cleanup_removes_socket_file() {
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX)
            .expect("create should succeed");

        let path = mgr.path().to_path_buf();

        // Manually clean up without relying on Drop.
        // We have to forget mgr to avoid a double-remove.
        std::mem::forget(mgr);

        SocketManager::cleanup(&path);

        assert!(!path.exists(), "socket file should be gone after cleanup");
    }

    #[test]
    fn cleanup_on_nonexistent_path_is_safe() {
        let path = std::env::temp_dir().join("swage-nonexistent-12345.sock");
        // Must not panic or error.
        SocketManager::cleanup(&path);
    }

    // -- Stale detection ------------------------------------------------------

    #[test]
    fn is_stale_false_for_nonexistent_path() {
        let path = std::env::temp_dir().join("swage-never-created-99999.sock");
        assert!(
            !SocketManager::is_stale(&path),
            "non-existent path should not be considered stale"
        );
    }

    #[tokio::test]
    async fn is_stale_false_for_active_socket() {
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX)
            .expect("create should succeed");

        // Socket has an active listener — should not be stale.
        assert!(
            !SocketManager::is_stale(mgr.path()),
            "socket with active listener should not be stale"
        );
    }

    #[test]
    fn is_stale_true_for_abandoned_socket() {
        use std::os::unix::net::UnixListener as StdUnixListener;

        let path = std::env::temp_dir()
            .join(format!("{}-stale-test-{}.sock", TEST_PREFIX, uuid::Uuid::new_v4()));

        // Bind and immediately drop — leaves the file but no listener.
        let listener = StdUnixListener::bind(&path).expect("bind should succeed");
        drop(listener);

        assert!(
            SocketManager::is_stale(&path),
            "socket file with no listener should be detected as stale"
        );

        // Clean up the test file.
        std::fs::remove_file(&path).ok();
    }

    // -- Stale cleanup --------------------------------------------------------

    #[test]
    fn cleanup_stale_removes_stale_sockets() {
        use std::os::unix::net::UnixListener as StdUnixListener;

        // Keep the prefix short — socket paths are limited to SUN_LEN (108 chars).
        let short_id = &uuid::Uuid::new_v4().to_string()[..8];
        let prefix = format!("swage-sc-{short_id}");

        // Create two stale sockets with the test prefix.
        let path1 = std::env::temp_dir().join(format!("{prefix}-a.sock"));
        let path2 = std::env::temp_dir().join(format!("{prefix}-b.sock"));
        let listener1 = StdUnixListener::bind(&path1).unwrap();
        let listener2 = StdUnixListener::bind(&path2).unwrap();
        drop(listener1);
        drop(listener2);

        assert!(path1.exists());
        assert!(path2.exists());

        SocketManager::cleanup_stale(&prefix);

        assert!(!path1.exists(), "stale socket 1 should be removed");
        assert!(!path2.exists(), "stale socket 2 should be removed");
    }

    #[tokio::test]
    async fn cleanup_stale_does_not_remove_active_sockets() {
        // Keep the prefix short — socket paths are limited to SUN_LEN (108 chars).
        let short_id = &uuid::Uuid::new_v4().to_string()[..8];
        let prefix = format!("swage-ap-{short_id}");

        // Create an active socket (still listening).
        let (mgr, _listener) = SocketManager::create(&prefix)
            .expect("create should succeed");
        let path = mgr.path().to_path_buf();

        // cleanup_stale should leave the active socket alone.
        std::mem::forget(mgr); // Prevent Drop from removing it during the test.
        SocketManager::cleanup_stale(&prefix);

        let still_exists = path.exists();

        // Clean up regardless of assertion outcome.
        std::fs::remove_file(&path).ok();

        assert!(still_exists, "active socket should not be removed by cleanup_stale");
    }
}
