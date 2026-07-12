//! Child process lifecycle management.
//!
//! Spawns, monitors, and terminates sandboxed agent child processes.
//! Handles graceful shutdown with configurable grace period, forceful
//! termination, and die-with-parent on Linux.

use std::time::Duration;

#[cfg(test)]
use std::process::ExitStatus;

use swage_core::SandboxError;

/// Manages the lifecycle of a sandboxed child process.
///
/// Created by calling [`ChildProcessManager::spawn`]. The manager owns the
/// child handle and exposes lifecycle controls: PID queries, running checks,
/// graceful shutdown with a configurable grace period, and force kill.
///
/// On Linux, the child is spawned with `PR_SET_PDEATHSIG` so it receives
/// SIGTERM if the host process terminates unexpectedly — credentials and state
/// cannot leak through an orphaned child.
pub struct ChildProcessManager {
    child: tokio::process::Child,
    pid: u32,
}

impl ChildProcessManager {
    /// Spawns a child process from the given command.
    ///
    /// On Linux, sets `PR_SET_PDEATHSIG` via `prctl` in a `pre_exec` closure
    /// so the child receives `SIGTERM` when the host process dies.
    pub fn spawn(mut command: tokio::process::Command) -> Result<Self, SandboxError> {
        #[cfg(target_os = "linux")]
        {
            // Safety: prctl is async-signal-safe; this runs between fork and exec
            // in the child process. No allocations, no Rust runtime.
            unsafe {
                command.pre_exec(|| {
                    let ret = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
                    if ret != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let child = command.spawn().map_err(|e| SandboxError::SpawnFailed {
            message: format!("failed to spawn child process: {e}"),
        })?;

        let pid = child.id().ok_or_else(|| SandboxError::SpawnFailed {
            message: "child process exited immediately after spawn".into(),
        })?;

        Ok(Self { child, pid })
    }

    /// Returns the child process PID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Take ownership of the child's stdin pipe, if one was configured.
    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.stdin.take()
    }

    /// Take ownership of the child's stdout pipe, if one was configured.
    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.stdout.take()
    }

    /// Take ownership of the child's stderr pipe, if one was configured.
    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.stderr.take()
    }

    /// Returns true if the child process is still running.
    ///
    /// Uses a non-blocking `try_wait` — does not block the caller.
    pub fn is_running(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,     // No exit status yet — still running.
            Ok(Some(_)) => false, // Exited.
            Err(_) => false,      // Error querying state — treat as exited.
        }
    }

    /// Waits asynchronously for the child process to exit.
    #[cfg(test)]
    pub async fn wait_for_exit(&mut self) -> Result<ExitStatus, SandboxError> {
        self.child
            .wait()
            .await
            .map_err(|e| SandboxError::SpawnFailed {
                message: format!("error waiting for child process: {e}"),
            })
    }

    /// Sends SIGTERM and waits for the child to exit within `grace_period`.
    ///
    /// If the child does not exit in time, falls back to SIGKILL and waits
    /// for the reap. Succeeds silently if the child has already exited.
    ///
    /// The timed future only waits on a child handle. On timeout this method
    /// immediately sends SIGKILL and reaps the same child synchronously below.
    pub async fn graceful_shutdown(&mut self, grace_period: Duration) -> Result<(), SandboxError> {
        // Nothing to do if already exited — idempotent.
        if !self.is_running() {
            return Ok(());
        }

        // Send SIGTERM and give the child time to shut down cleanly.
        // Safety: pid is valid for the duration of this call; SIGTERM is benign
        // if the process has already exited (the signal is silently dropped).
        unsafe {
            libc::kill(self.pid as libc::pid_t, libc::SIGTERM);
        }

        match tokio::time::timeout(grace_period, self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(SandboxError::SpawnFailed {
                message: format!("error waiting for child after SIGTERM: {e}"),
            }),
            Err(_timeout) => {
                // Grace period elapsed — SIGKILL and reap.
                tracing::warn!(
                    pid = self.pid,
                    "child did not exit within grace period; sending SIGKILL"
                );
                self.force_kill()?;
                // Wait for the kernel to reap the process so no zombie is left.
                self.child
                    .wait()
                    .await
                    .map_err(|e| SandboxError::SpawnFailed {
                        message: format!("error reaping child after SIGKILL: {e}"),
                    })?;
                Ok(())
            }
        }
    }

    /// Sends SIGKILL to the child process immediately, without waiting.
    ///
    /// The child process will be terminated by the kernel, but this method
    /// returns before it has exited. Call `wait_for_exit` to reap the process
    /// if needed.
    pub fn force_kill(&mut self) -> Result<(), SandboxError> {
        // start_kill sends SIGKILL without blocking on process exit.
        self.child
            .start_kill()
            .map_err(|e| SandboxError::SpawnFailed {
                message: format!("failed to send SIGKILL to child process: {e}"),
            })
    }
}

/// Best-effort cleanup for a child that failed before the sandbox stored its
/// process handle. The caller keeps and returns the original startup error.
pub async fn cleanup_failed_startup_process(
    mut process: ChildProcessManager,
    grace_period: Duration,
    context: &'static str,
) {
    if let Err(error) = process.graceful_shutdown(grace_period).await {
        tracing::warn!(error = %error, "{context}");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sleep_command(secs: u64) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg(secs.to_string());
        cmd
    }

    fn true_command() -> tokio::process::Command {
        tokio::process::Command::new("true")
    }

    // -- Spawn ----------------------------------------------------------------

    #[tokio::test]
    async fn spawn_produces_different_pid_from_host() {
        let mut mgr = ChildProcessManager::spawn(sleep_command(60)).expect("spawn should succeed");

        // The child must be a different process.
        assert_ne!(
            mgr.pid(),
            std::process::id(),
            "child PID should differ from host PID"
        );

        // Clean up.
        mgr.force_kill().ok();
        mgr.wait_for_exit().await.ok();
    }

    #[tokio::test]
    async fn pid_is_nonzero() {
        let mut mgr = ChildProcessManager::spawn(sleep_command(60)).expect("spawn should succeed");

        assert_ne!(mgr.pid(), 0, "PID should be nonzero");

        mgr.force_kill().ok();
        mgr.wait_for_exit().await.ok();
    }

    // -- Running state --------------------------------------------------------

    #[tokio::test]
    async fn is_running_true_for_live_child() {
        let mut mgr = ChildProcessManager::spawn(sleep_command(60)).expect("spawn should succeed");

        assert!(
            mgr.is_running(),
            "child should be running immediately after spawn"
        );

        mgr.force_kill().ok();
        mgr.wait_for_exit().await.ok();
    }

    #[tokio::test]
    async fn is_running_false_after_exit() {
        let mut mgr = ChildProcessManager::spawn(true_command()).expect("spawn should succeed");

        // `true` exits immediately — wait for it, then check.
        mgr.wait_for_exit()
            .await
            .expect("wait_for_exit should succeed");

        assert!(
            !mgr.is_running(),
            "is_running should be false after child exits"
        );
    }

    // -- Wait -----------------------------------------------------------------

    #[tokio::test]
    async fn wait_for_exit_returns_on_child_completion() {
        let mut mgr = ChildProcessManager::spawn(true_command()).expect("spawn should succeed");

        let status = mgr
            .wait_for_exit()
            .await
            .expect("wait_for_exit should succeed");

        // `true` exits with code 0.
        assert!(status.success(), "expected successful exit from `true`");
    }

    // -- Graceful shutdown ----------------------------------------------------

    #[tokio::test]
    async fn graceful_shutdown_terminates_child() {
        let mut mgr = ChildProcessManager::spawn(sleep_command(60)).expect("spawn should succeed");

        assert!(mgr.is_running(), "child should be running before shutdown");

        mgr.graceful_shutdown(Duration::from_secs(5))
            .await
            .expect("graceful_shutdown should succeed");

        assert!(
            !mgr.is_running(),
            "child should no longer be running after shutdown"
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_is_idempotent() {
        let mut mgr = ChildProcessManager::spawn(sleep_command(60)).expect("spawn should succeed");

        mgr.graceful_shutdown(Duration::from_secs(5))
            .await
            .expect("first graceful_shutdown should succeed");

        // A second call on an already-terminated child must also succeed.
        mgr.graceful_shutdown(Duration::from_secs(5))
            .await
            .expect("second graceful_shutdown should succeed silently");
    }

    // -- Force kill -----------------------------------------------------------

    #[tokio::test]
    async fn force_kill_terminates_child() {
        let mut mgr = ChildProcessManager::spawn(sleep_command(60)).expect("spawn should succeed");

        mgr.force_kill().expect("force_kill should succeed");

        // Reap the process and verify it exited.
        mgr.wait_for_exit()
            .await
            .expect("wait_for_exit should succeed after SIGKILL");

        assert!(
            !mgr.is_running(),
            "child should not be running after force kill"
        );
    }

    // -- Die-with-parent (Linux only) ----------------------------------------

    /// Verifies that spawning a child process on Linux succeeds, which means
    /// the PR_SET_PDEATHSIG pre_exec hook executed without error.
    ///
    /// Full die-with-parent verification (killing the host and confirming the
    /// child terminates) requires an out-of-process integration test, as we
    /// cannot kill our own test process mid-run.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_pdeathsig_spawn_succeeds() {
        let mut mgr = ChildProcessManager::spawn(sleep_command(60))
            .expect("spawn with PR_SET_PDEATHSIG pre_exec should succeed on Linux");

        assert!(mgr.is_running());

        mgr.graceful_shutdown(Duration::from_secs(5)).await.ok();
    }
}
