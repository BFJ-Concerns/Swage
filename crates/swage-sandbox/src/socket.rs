//! Unix domain socket lifecycle management.
//!
//! Creates socket files in the user's runtime directory, detects stale sockets
//! from previous crashes, and ensures cleanup on shutdown.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use swage_core::SandboxError;
use tokio::net::UnixListener;

const FALLBACK_SOCKET_PARENT: &str = "/tmp";

/// The short root used when `XDG_RUNTIME_DIR` is unset or too long: a
/// per-user `swage-<uid>` directory under `/tmp`. Bubblewrap overlays `/tmp`
/// with an empty tmpfs, so the sandbox sees a socket here only through the
/// command builder's bind of the socket file itself.
fn fallback_socket_root() -> PathBuf {
    // Safety: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    Path::new(FALLBACK_SOCKET_PARENT).join(format!("swage-{uid}"))
}

fn socket_path_fits(root: &Path, prefix: &str) -> bool {
    let socket_address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    root.join(format!("{prefix}-{}.sock", "0".repeat(36)))
        .as_os_str()
        .as_bytes()
        .len()
        < socket_address.sun_path.len()
}

fn socket_root(prefix: &str) -> Result<PathBuf, SandboxError> {
    let root = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(xdg_runtime_dir) => PathBuf::from(xdg_runtime_dir).join("swage"),
        None => fallback_socket_root(),
    };
    let root = if socket_path_fits(&root, prefix) {
        root
    } else {
        let fallback = fallback_socket_root();
        if !socket_path_fits(&fallback, prefix) {
            return Err(SandboxError::SpawnFailed {
                message: format!(
                    "Unix socket paths are too long under both {} and {}; set XDG_RUNTIME_DIR to a shorter path or use a shorter socket prefix",
                    root.display(),
                    fallback.display()
                ),
            });
        }
        fallback
    };
    create_private_dir(&root)?;
    Ok(root)
}

/// Creates `root` if needed and ensures it is a directory owned by this user
/// with mode 0700. The fallback lives in world-writable `/tmp`, so a
/// pre-existing directory owned by someone else is refused, not reused.
fn create_private_dir(root: &Path) -> Result<(), SandboxError> {
    std::fs::create_dir_all(root).map_err(|error| SandboxError::SpawnFailed {
        message: format!(
            "failed to create Unix socket directory at {}: {error}",
            root.display()
        ),
    })?;
    let metadata = std::fs::symlink_metadata(root).map_err(|error| SandboxError::SpawnFailed {
        message: format!(
            "failed to inspect Unix socket directory at {}: {error}",
            root.display()
        ),
    })?;
    // Safety: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid {
        return Err(SandboxError::SpawnFailed {
            message: format!(
                "Unix socket directory at {} is not a directory owned by this user",
                root.display()
            ),
        });
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        SandboxError::SpawnFailed {
            message: format!(
                "failed to protect Unix socket directory at {}: {error}",
                root.display()
            ),
        }
    })
}

/// Manages the lifecycle of a Unix domain socket file.
///
/// Created via [`SocketManager::create`]. Owns the socket path and removes
/// it automatically when dropped — sockets do not clean themselves up and
/// stale entries block a fresh bind on the same path.
pub struct SocketManager {
    path: PathBuf,
}

impl SocketManager {
    /// Creates a Unix domain socket under the user's short runtime root.
    ///
    /// The socket file is named `{prefix}-{uuid}.sock` so multiple sandboxes
    /// can coexist without path collisions. Returns the manager (which owns
    /// cleanup) and the bound listener ready to accept connections.
    pub fn create(prefix: &str) -> Result<(Self, UnixListener), SandboxError> {
        let path = socket_root(prefix)?.join(format!("{}-{}.sock", prefix, uuid::Uuid::new_v4()));

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
    #[cfg(test)]
    pub fn is_stale(path: &Path) -> bool {
        if !path.exists() {
            return false;
        }
        // Use a synchronous connect attempt — async would require a running
        // runtime which is not guaranteed at this call site.
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => false, // Someone is listening — active socket.
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
        if path.exists()
            && let Err(e) = std::fs::remove_file(path)
        {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to remove socket file during cleanup"
            );
        }
    }

    /// Scans the socket root and removes stale socket files whose names
    /// start with `{prefix}-`.
    ///
    /// Intended for use at startup to clean up sockets left by a previous
    /// crash. Only removes sockets that pass the [`is_stale`] check —
    /// active sockets from concurrently running sandboxes are left alone.
    #[cfg(test)]
    pub fn cleanup_stale(prefix: &str) {
        let socket_root = match socket_root(prefix) {
            Ok(root) => root,
            Err(error) => {
                tracing::warn!(%error, "failed to resolve stale socket directory");
                return;
            }
        };
        let name_prefix = format!("{prefix}-");

        let entries = match std::fs::read_dir(&socket_root) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    dir = %socket_root.display(),
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
    use std::time::{Duration, Instant};

    const TEST_PREFIX: &str = "swage-test-socket";

    fn test_socket_root() -> PathBuf {
        socket_root(TEST_PREFIX).expect("socket root should resolve")
    }

    fn wait_until_stale(path: &Path) {
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if SocketManager::is_stale(path) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            SocketManager::is_stale(path),
            "socket file with no listener should be detected as stale"
        );
    }

    // -- Create ---------------------------------------------------------------

    #[tokio::test]
    async fn create_produces_socket_file() {
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX).expect("create should succeed");

        assert!(mgr.path().exists(), "socket file should exist after create");

        // path() should point at the file we just created.
        assert!(mgr.path().is_absolute(), "socket path should be absolute");
    }

    #[test]
    fn create_is_independent_of_long_tmpdir() {
        let root = tempfile::tempdir_in(FALLBACK_SOCKET_PARENT)
            .expect("short temporary test root should be created");
        let long_tmp = root.path().join("t".repeat(120));
        std::fs::create_dir(&long_tmp).expect("long TMPDIR should be created");
        let xdg_runtime_dir = root.path().join("runtime");

        let output = std::process::Command::new(
            std::env::current_exe().expect("current test executable should resolve"),
        )
        .args([
            "--exact",
            "socket::tests::create_is_independent_of_long_tmpdir_child",
            "--ignored",
            "--nocapture",
        ])
        .env("TMPDIR", &long_tmp)
        .env("XDG_RUNTIME_DIR", &xdg_runtime_dir)
        .output()
        .expect("isolated socket regression test should run");

        assert!(
            output.status.success(),
            "isolated socket regression test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
            "the isolated child must report its passing test on stdout; stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "run in an isolated child process by create_is_independent_of_long_tmpdir"]
    async fn create_is_independent_of_long_tmpdir_child() {
        let tmpdir = PathBuf::from(std::env::var_os("TMPDIR").expect("TMPDIR should be set"));
        let xdg_runtime_dir = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR should be set"),
        );
        let expected_root = xdg_runtime_dir.join("swage");
        let (manager, _listener) =
            SocketManager::create(TEST_PREFIX).expect("socket should bind outside long TMPDIR");
        let socket_address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let sun_path_capacity = socket_address.sun_path.len();

        assert!(tmpdir.as_os_str().as_bytes().len() >= sun_path_capacity);
        assert_eq!(manager.path().parent(), Some(expected_root.as_path()));
        assert!(manager.path().as_os_str().as_bytes().len() < sun_path_capacity);
        assert_eq!(
            std::fs::metadata(&expected_root)
                .expect("runtime socket directory should exist")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn overlong_xdg_runtime_dir_uses_short_fallback() {
        let root = tempfile::tempdir_in(FALLBACK_SOCKET_PARENT).expect("temporary root");
        let long_xdg = root.path().join("x".repeat(120));
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "socket::tests::overlong_xdg_runtime_dir_uses_short_fallback_child",
                "--ignored",
                "--nocapture",
            ])
            .env("XDG_RUNTIME_DIR", &long_xdg)
            .output()
            .expect("isolated socket test");

        assert!(
            output.status.success(),
            "isolated socket regression test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "run in an isolated child process by overlong_xdg_runtime_dir_uses_short_fallback"]
    async fn overlong_xdg_runtime_dir_uses_short_fallback_child() {
        let configured = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("XDG set"));
        let (manager, _listener) = SocketManager::create(TEST_PREFIX).expect("fallback socket");
        assert_eq!(
            manager.path().parent(),
            Some(fallback_socket_root().as_path())
        );
        assert!(!configured.join("swage").exists());
    }

    #[test]
    fn unset_xdg_runtime_dir_uses_short_fallback() {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "socket::tests::unset_xdg_runtime_dir_uses_short_fallback_child",
                "--ignored",
                "--nocapture",
            ])
            .env_remove("XDG_RUNTIME_DIR")
            .output()
            .expect("isolated socket test");

        assert!(
            output.status.success(),
            "isolated socket regression test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "run in an isolated child process by unset_xdg_runtime_dir_uses_short_fallback"]
    async fn unset_xdg_runtime_dir_uses_short_fallback_child() {
        assert!(std::env::var_os("XDG_RUNTIME_DIR").is_none());
        let (manager, _listener) = SocketManager::create(TEST_PREFIX).expect("fallback socket");
        assert_eq!(
            manager.path().parent(),
            Some(fallback_socket_root().as_path())
        );
    }

    #[test]
    fn impossible_socket_path_reports_how_to_shorten_it() {
        let error = socket_root(&"p".repeat(200)).expect_err("prefix cannot fit");
        assert!(
            error.to_string().contains("shorter socket prefix"),
            "path-length error should be actionable: {error}"
        );
    }

    #[tokio::test]
    async fn socket_name_includes_prefix() {
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX).expect("create should succeed");

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
            let (mgr, _listener) =
                SocketManager::create(TEST_PREFIX).expect("create should succeed");
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
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX).expect("create should succeed");

        let path = mgr.path().to_path_buf();

        // Manually clean up without relying on Drop.
        // We have to forget mgr to avoid a double-remove.
        std::mem::forget(mgr);

        SocketManager::cleanup(&path);

        assert!(!path.exists(), "socket file should be gone after cleanup");
    }

    #[test]
    fn cleanup_on_nonexistent_path_is_safe() {
        let path = test_socket_root().join("swage-nonexistent-12345.sock");
        // Must not panic or error.
        SocketManager::cleanup(&path);
    }

    // -- Stale detection ------------------------------------------------------

    #[test]
    fn is_stale_false_for_nonexistent_path() {
        let path = test_socket_root().join("swage-never-created-99999.sock");
        assert!(
            !SocketManager::is_stale(&path),
            "non-existent path should not be considered stale"
        );
    }

    #[tokio::test]
    async fn is_stale_false_for_active_socket() {
        let (mgr, _listener) = SocketManager::create(TEST_PREFIX).expect("create should succeed");

        // Socket has an active listener — should not be stale.
        assert!(
            !SocketManager::is_stale(mgr.path()),
            "socket with active listener should not be stale"
        );
    }

    #[test]
    fn is_stale_true_for_abandoned_socket() {
        use std::os::unix::net::UnixListener as StdUnixListener;

        let path = test_socket_root().join(format!(
            "{}-stale-test-{}.sock",
            TEST_PREFIX,
            uuid::Uuid::new_v4()
        ));

        // Bind and immediately drop — leaves the file but no listener.
        let listener = StdUnixListener::bind(&path).expect("bind should succeed");
        drop(listener);
        wait_until_stale(&path);

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
        let path1 = test_socket_root().join(format!("{prefix}-a.sock"));
        let path2 = test_socket_root().join(format!("{prefix}-b.sock"));
        let listener1 = StdUnixListener::bind(&path1).unwrap();
        let listener2 = StdUnixListener::bind(&path2).unwrap();
        drop(listener1);
        drop(listener2);

        assert!(path1.exists());
        assert!(path2.exists());
        wait_until_stale(&path1);
        wait_until_stale(&path2);

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
        let (mgr, _listener) = SocketManager::create(&prefix).expect("create should succeed");
        let path = mgr.path().to_path_buf();

        // cleanup_stale should leave the active socket alone.
        std::mem::forget(mgr); // Prevent Drop from removing it during the test.
        SocketManager::cleanup_stale(&prefix);

        let still_exists = path.exists();

        // Clean up regardless of assertion outcome.
        std::fs::remove_file(&path).ok();

        assert!(
            still_exists,
            "active socket should not be removed by cleanup_stale"
        );
    }
}
