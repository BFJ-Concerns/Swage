//! `swage gateway start` — launch the gateway as a background process.
//!
//! Forks a copy of the current binary with `--server-mode` and waits for the
//! gateway to become ready (health endpoint responds). Reports the listening
//! address on success, or an error if startup fails.

use std::{path::PathBuf, time::Duration};

use swage_client::{error::ClientError, http::GatewayClient, runtime::RuntimeMetadata};
use tokio::time::{sleep, Instant};

use crate::cli::StartArgs;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the `swage gateway start` command.
///
/// Detects an already-running gateway (or cleans up stale metadata), then
/// either runs the gateway in the foreground or spawns it as a detached
/// background daemon and polls until it is ready.
pub async fn run(args: &StartArgs) -> Result<(), ClientError> {
    // Step 1: Check runtime metadata to detect an existing gateway.
    match RuntimeMetadata::load() {
        Ok(meta) if meta.is_stale() => {
            // Metadata file exists but the recorded PID is dead — crashed gateway.
            eprintln!("Cleaning up stale metadata...");
            RuntimeMetadata::remove()
                .map_err(|e| ClientError::MetadataIo { source: e })?;
        }
        Ok(meta) => {
            // PID is alive — the gateway is already running.
            eprintln!("Gateway is already running (PID {})", meta.pid);
            return Err(ClientError::GatewayNotRunning {
                reason: format!(
                    "gateway is already running at {} (PID {})",
                    meta.address, meta.pid
                ),
            });
        }
        // No metadata file — gateway is not running, proceed normally.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ClientError::MetadataIo { source: e }),
    }

    // Step 2: Foreground mode — run the gateway inline, blocking until shutdown.
    if args.foreground {
        crate::startup::run_gateway(args.port, args.host.clone(), args.config.clone()).await;
        return Ok(());
    }

    // Step 3: Background mode — spawn a detached child process.
    spawn_background_gateway(args.port, args.host.as_deref(), args.config.as_deref())?;

    // Step 4: Poll until the gateway writes its metadata and passes a health check.
    let address = poll_until_ready(Duration::from_secs(5))
        .await
        .map_err(|reason| ClientError::GatewayNotRunning { reason })?;

    // Step 5: Report success to the user.
    println!("Gateway started on {address}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Background process spawning
// ---------------------------------------------------------------------------

/// Spawn the current binary as a detached background gateway process.
///
/// Redirects stdout/stderr to `~/.swage/gateway.log`. Sets the child process
/// into its own process group so it survives the CLI process exiting without
/// receiving SIGHUP from the terminal.
fn spawn_background_gateway(
    port: Option<u16>,
    host: Option<&str>,
    config: Option<&str>,
) -> Result<(), ClientError> {
    let binary = std::env::current_exe()
        .map_err(|e| ClientError::MetadataIo { source: e })?;

    let log_path = gateway_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ClientError::MetadataIo { source: e })?;
    }

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| ClientError::MetadataIo { source: e })?;

    let log_stderr = log_file
        .try_clone()
        .map_err(|e| ClientError::MetadataIo { source: e })?;

    let args = build_spawn_args(port, host.map(str::to_owned), config.map(str::to_owned));

    let mut cmd = std::process::Command::new(&binary);
    cmd.args(&args);
    cmd.stdout(log_file);
    cmd.stderr(log_stderr);
    cmd.stdin(std::process::Stdio::null());

    // Detach the child into its own process group so it outlives the CLI.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    cmd.spawn().map_err(|e| ClientError::MetadataIo { source: e })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Health polling
// ---------------------------------------------------------------------------

/// Poll until the gateway is ready, with exponential backoff.
///
/// Reads the runtime metadata file to discover the address, then calls the
/// health endpoint. Returns the address on success or an error string on
/// timeout.
pub(crate) async fn poll_until_ready(timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(100);

    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "gateway did not become ready within {}s",
                timeout.as_secs()
            ));
        }

        // Check for the metadata file — the gateway writes it after binding.
        if let Ok(meta) = RuntimeMetadata::load() {
            let client = GatewayClient::new(&meta.address);
            if client.health().await.is_ok() {
                return Ok(meta.address);
            }
        }

        sleep(delay).await;
        // Double the delay up to 800 ms to avoid hammering the process.
        delay = (delay * 2).min(Duration::from_millis(800));
    }
}

// ---------------------------------------------------------------------------
// Path helpers (extracted for testability)
// ---------------------------------------------------------------------------

/// Returns the path to the gateway log file: `~/.swage/gateway.log`.
pub(crate) fn gateway_log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".swage").join("gateway.log")
}

/// Builds the argument list for spawning the binary in server mode.
///
/// Always starts with `--server-mode`, followed by any provided overrides.
/// This is a pure function — extracted so tests can verify the args without
/// actually spawning a process.
pub(crate) fn build_spawn_args(
    port: Option<u16>,
    host: Option<String>,
    config: Option<String>,
) -> Vec<String> {
    // TODO Background daemon spawn args are unrecognised by the top-level parser
    // priority: high | tags: correctness, review:branch-era-02-delivery
    // `build_spawn_args` places `--port`, `--host`, and `--config` at the top level
    // (e.g. `swage --server-mode --port 8080`), but those flags belong to the
    // `gateway start` subcommand in `StartArgs`. Clap rejects them as unknown
    // arguments, so the child process exits with a parse error before reaching the
    // `--server-mode` branch. Any `swage gateway start --port X` invocation silently
    // fails to launch the daemon with the requested overrides.
    // Fix: either add the override flags to `Cli` directly and read them in `main.rs`,
    // or pass the config via the environment rather than re-parsing argv.
    // ---
    let mut args = vec!["--server-mode".to_string()];
    if let Some(p) = port {
        args.push("--port".to_string());
        args.push(p.to_string());
    }
    if let Some(h) = host {
        args.push("--host".to_string());
        args.push(h);
    }
    if let Some(c) = config {
        args.push("--config".to_string());
        args.push(c);
    }
    args
}

// ---------------------------------------------------------------------------
// Tests — written first; they fail until the implementation is present
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // The log file must go to ~/.swage/gateway.log so users know where to look.
    #[test]
    fn log_path_ends_with_swage_gateway_log() {
        let path = gateway_log_path();
        let s = path.to_string_lossy();
        assert!(
            s.ends_with(".swage/gateway.log"),
            "expected path ending with .swage/gateway.log, got: {s}"
        );
    }

    // When no flags are given, only --server-mode should appear.
    #[test]
    fn spawn_args_with_no_overrides_contains_only_server_mode() {
        let args = build_spawn_args(None, None, None);
        assert_eq!(args, vec!["--server-mode"]);
    }

    // Port flag is added when provided.
    #[test]
    fn spawn_args_includes_port_when_set() {
        let args = build_spawn_args(Some(8080), None, None);
        assert!(args.contains(&"--server-mode".to_string()));
        assert!(args.contains(&"--port".to_string()));
        assert!(args.contains(&"8080".to_string()));
    }

    // Host flag is added when provided.
    #[test]
    fn spawn_args_includes_host_when_set() {
        let args = build_spawn_args(None, Some("0.0.0.0".to_string()), None);
        assert!(args.contains(&"--host".to_string()));
        assert!(args.contains(&"0.0.0.0".to_string()));
    }

    // Config flag is added when provided.
    #[test]
    fn spawn_args_includes_config_when_set() {
        let args = build_spawn_args(None, None, Some("/etc/swage.toml".to_string()));
        assert!(args.contains(&"--config".to_string()));
        assert!(args.contains(&"/etc/swage.toml".to_string()));
    }

    // All flags together.
    #[test]
    fn spawn_args_with_all_overrides() {
        let args = build_spawn_args(
            Some(9090),
            Some("127.0.0.1".to_string()),
            Some("/tmp/cfg.toml".to_string()),
        );
        let expected = vec![
            "--server-mode",
            "--port", "9090",
            "--host", "127.0.0.1",
            "--config", "/tmp/cfg.toml",
        ];
        assert_eq!(args, expected);
    }

    // Polling with a very short timeout and no running gateway should return an error.
    #[tokio::test]
    async fn poll_until_ready_times_out_with_no_gateway() {
        use std::time::Duration;
        // Use a 50ms timeout — fast enough for a unit test.
        let result = poll_until_ready(Duration::from_millis(50)).await;
        assert!(result.is_err(), "expected timeout error, got: {result:?}");
    }
}
