//! `swage gateway stop` — request a clean shutdown of the running gateway.
//!
//! Default path uses the HTTP shutdown endpoint for graceful shutdown with
//! client notification. The `--force` flag falls back to direct process
//! termination (SIGTERM) for unresponsive gateways.

use std::time::Duration;
use swage_client::{error::ClientError, http::GatewayClient, runtime::RuntimeMetadata};

use super::StopArgs;

/// Maximum time to wait for the gateway to exit after a graceful shutdown request.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum time to wait for the gateway to exit after SIGTERM.
const FORCE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Polling interval when checking whether the gateway process has exited.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Stops the running gateway.
///
/// Returns `Ok(())` when the gateway exits cleanly. Returns
/// `Err(GatewayNotRunning)` when there is no gateway to stop.
/// Other `ClientError` variants surface HTTP or I/O failures.
pub async fn run(args: &StopArgs) -> Result<(), ClientError> {
    // Load metadata — a missing or unreadable file means the gateway isn't running.
    let meta = match RuntimeMetadata::load() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Gateway is not running");
            return Err(ClientError::GatewayNotRunning {
                reason: "no metadata file found".to_string(),
            });
        }
        Err(e) => return Err(ClientError::MetadataIo { source: e }),
    };

    // A stale PID means the gateway crashed without cleaning up.
    if meta.is_stale() {
        eprintln!("Gateway is not running");
        // Clean up the orphaned metadata file.
        let _ = RuntimeMetadata::remove();
        return Err(ClientError::GatewayNotRunning {
            reason: format!("PID {} is no longer running", meta.pid),
        });
    }

    if args.force {
        force_stop(&meta).await
    } else {
        graceful_stop(&meta).await
    }
}

/// Requests a graceful shutdown via the HTTP endpoint, then waits for the process to exit.
async fn graceful_stop(meta: &RuntimeMetadata) -> Result<(), ClientError> {
    let client = GatewayClient::new(&meta.address);

    if let Err(e) = client.shutdown().await {
        eprintln!("Gateway is not responding. Use --force to terminate the process.");
        return Err(e);
    }

    if wait_for_exit(meta.pid, GRACEFUL_SHUTDOWN_TIMEOUT).await {
        // Gateway confirmed dead — remove any leftover metadata file.
        let _ = RuntimeMetadata::remove();
        println!("Gateway stopped");
        Ok(())
    } else {
        eprintln!(
            "Gateway did not exit within {} seconds. Use --force to terminate.",
            GRACEFUL_SHUTDOWN_TIMEOUT.as_secs()
        );
        Err(ClientError::GatewayNotRunning {
            reason: format!("PID {} did not exit after shutdown request", meta.pid),
        })
    }
}

/// Sends SIGTERM to the gateway process and waits for it to exit.
async fn force_stop(meta: &RuntimeMetadata) -> Result<(), ClientError> {
    // SAFETY: libc::kill is an FFI call with no Rust memory safety implications.
    // We send SIGTERM (signal 15) to request process termination.
    let kill_result = unsafe { libc::kill(meta.pid as i32, libc::SIGTERM) };

    if kill_result != 0 {
        let err = std::io::Error::last_os_error();
        eprintln!(
            "Failed to send SIGTERM to gateway (PID {}): {err}",
            meta.pid
        );
        return Err(ClientError::MetadataIo { source: err });
    }

    if wait_for_exit(meta.pid, FORCE_SHUTDOWN_TIMEOUT).await {
        let _ = RuntimeMetadata::remove();
        println!("Gateway stopped (forced)");
        Ok(())
    } else {
        eprintln!(
            "Gateway (PID {}) did not exit within {} seconds after SIGTERM",
            meta.pid,
            FORCE_SHUTDOWN_TIMEOUT.as_secs()
        );
        Err(ClientError::GatewayNotRunning {
            reason: format!("PID {} did not exit after SIGTERM", meta.pid),
        })
    }
}

/// Polls PID liveness until the process exits or the timeout elapses.
///
/// Returns `true` if the process has exited within `timeout`, `false` if the
/// process is still alive when the deadline passes.
async fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;

    while tokio::time::Instant::now() < deadline {
        if is_pid_dead(pid) {
            return true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Final check after the deadline has passed.
    is_pid_dead(pid)
}

/// Returns `true` if `pid` no longer corresponds to a running process.
///
/// On Linux, reads `/proc/{pid}/status` to distinguish live processes from
/// zombies. Zombie processes have terminated and are awaiting parent `wait()`;
/// they no longer serve requests and should be treated as dead. If the proc
/// entry is absent the process is fully gone.
fn is_pid_dead(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let status_path = format!("/proc/{pid}/status");
        match std::fs::read_to_string(&status_path) {
            // No /proc entry — process is gone.
            Err(_) => true,
            Ok(contents) => {
                // State: Z means zombie — terminated but not yet reaped.
                // A zombie no longer serves requests; treat it as dead.
                contents
                    .lines()
                    .find(|line| line.starts_with("State:"))
                    .map(|line| line.contains('Z'))
                    .unwrap_or(false)
            }
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: `kill(pid, 0)` does not send a signal; it only queries
        // process existence and permission. ESRCH means the PID is gone.
        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == 0 {
            return false;
        }

        let err = std::io::Error::last_os_error();
        matches!(err.raw_os_error(), Some(libc::ESRCH))
    }

    #[cfg(all(not(unix), not(target_os = "linux")))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::sync::Mutex;

    // Serialise env-touching tests to prevent XDG_RUNTIME_DIR races. The guard
    // intentionally spans awaits because runtime metadata reads process env.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn stop_with_no_metadata_returns_gateway_not_running() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmpdir = tempfile::TempDir::new().unwrap();
        // SAFETY: single-threaded within the lock.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path()) };

        let args = StopArgs { force: false };
        let result = run(&args).await;

        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        match result {
            Err(ClientError::GatewayNotRunning { .. }) => {}
            other => panic!("expected GatewayNotRunning, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stop_force_with_no_metadata_returns_gateway_not_running() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmpdir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path()) };

        let args = StopArgs { force: true };
        let result = run(&args).await;

        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        match result {
            Err(ClientError::GatewayNotRunning { .. }) => {}
            other => panic!("expected GatewayNotRunning, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stop_with_stale_pid_cleans_up_and_returns_not_running() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmpdir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path()) };

        // Write metadata with a PID that will never exist.
        let meta = RuntimeMetadata {
            pid: 4_194_304, // Linux max_pid — never assigned
            address: "http://127.0.0.1:3000".to_string(),
            started_at: chrono::Utc::now(),
        };
        meta.save().unwrap();

        let args = StopArgs { force: false };
        let result = run(&args).await;

        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        #[cfg(target_os = "linux")]
        {
            match result {
                Err(ClientError::GatewayNotRunning { .. }) => {}
                other => panic!("expected GatewayNotRunning for stale PID, got: {other:?}"),
            }
        }
        // On non-Linux is_stale() always returns false, so the graceful path is taken.
        #[cfg(not(target_os = "linux"))]
        let _ = result;
    }

    #[tokio::test]
    async fn force_stop_terminates_spawned_process_and_cleans_metadata() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmpdir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path()) };

        // Spawn a long-running process to simulate the gateway.
        let mut child = std::process::Command::new("sleep")
            .arg("100")
            .spawn()
            .expect("failed to spawn sleep process");
        let pid = child.id();

        let meta = RuntimeMetadata::new(pid, "http://127.0.0.1:3000".to_string());
        meta.save().unwrap();

        let args = StopArgs { force: true };
        let result = run(&args).await;
        let _ = child.wait();

        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        assert!(result.is_ok(), "force stop should succeed: {result:?}");

        // Metadata file should be gone after confirmed process exit.
        let meta_path = tmpdir.path().join("swage").join("metadata.json");
        assert!(
            !meta_path.exists(),
            "metadata file should be removed after force stop"
        );
    }
}
