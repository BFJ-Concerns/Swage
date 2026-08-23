//! `swage gateway start` — launch the gateway as a background process.
//!
//! Forks a copy of the current binary with `--server-mode` and waits for the
//! gateway to become ready (health endpoint responds). Reports the listening
//! address on success, or an error if startup fails.

use std::{path::PathBuf, time::Duration};

use swage_client::{error::ClientError, http::GatewayClient, runtime::RuntimeMetadata};
use tokio::time::{Instant, sleep};

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
    run_with_environment(args, None).await
}

pub(super) async fn run_with_environment(
    args: &StartArgs,
    environment: Option<&super::recovery::GatewayLifecycleEnvironment>,
) -> Result<(), ClientError> {
    let installed = match environment {
        Some(environment) => {
            super::install::load_install_config_for_root(environment.swage_home())?
        }
        None => super::install::load_install_config()?,
    };
    let effective_port = args
        .port
        .or_else(|| installed.as_ref().and_then(|cfg| cfg.port));
    let effective_host = args
        .host
        .clone()
        .or_else(|| installed.as_ref().and_then(|cfg| cfg.host.clone()));
    let effective_config = args
        .config
        .clone()
        .or_else(|| installed.as_ref().and_then(|cfg| cfg.config.clone()));

    // Step 1: Check runtime metadata to detect an existing gateway.
    let metadata_path = environment
        .map(|environment| environment.metadata_path())
        .unwrap_or_else(RuntimeMetadata::metadata_path);
    match RuntimeMetadata::load_from(&metadata_path) {
        Ok(meta) if meta.is_stale() => {
            // Metadata file exists but the recorded PID is dead — crashed gateway.
            eprintln!("Cleaning up stale metadata...");
            RuntimeMetadata::remove_at(&metadata_path)
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
        // No metadata file. Before spawning, recover a daemon that is already
        // serving at the configured address but lost its runtime metadata.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(meta) = super::recovery::recover_configured_gateway_metadata_at(
                effective_host.as_deref(),
                effective_port,
                &metadata_path,
            )
            .await
            {
                eprintln!("Gateway is already running (PID {})", meta.pid);
                return Err(ClientError::GatewayNotRunning {
                    reason: format!(
                        "gateway is already running at {} (PID {})",
                        meta.address, meta.pid
                    ),
                });
            }
        }
        Err(e) => return Err(ClientError::MetadataIo { source: e }),
    }

    // Step 2: Foreground mode — run the gateway inline, blocking until shutdown.
    if args.foreground {
        if let Some(environment) = environment {
            return run_foreground_gateway(
                effective_port,
                effective_host.as_deref(),
                effective_config.as_deref(),
                environment,
            );
        }
        crate::startup::run_gateway(effective_port, effective_host, effective_config).await;
        return Ok(());
    }

    // Step 3: Background mode — spawn a detached child process.
    spawn_background_gateway(
        effective_port,
        effective_host.as_deref(),
        effective_config.as_deref(),
        args.log_file.as_deref(),
        environment,
    )?;

    // Step 4: Poll until the gateway writes its metadata and passes a health check.
    let address = poll_until_ready_at(Duration::from_secs(5), &metadata_path)
        .await
        .map_err(|reason| ClientError::GatewayNotRunning { reason })?;

    // Step 5: Report success to the user.
    println!("Gateway started on {address}");
    println!(
        "Detached output: {}",
        gateway_log_path(args.log_file.as_deref(), environment).display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Background process spawning
// ---------------------------------------------------------------------------

/// Spawn the current binary as a detached background gateway process.
///
/// Redirects stdout/stderr to the selected log file, defaulting under the canonical Swage root.
/// Sets the child process into its own process group so it survives the CLI
/// process exiting without receiving SIGHUP from the terminal.
fn spawn_background_gateway(
    port: Option<u16>,
    host: Option<&str>,
    config: Option<&str>,
    log_file: Option<&str>,
    environment: Option<&super::recovery::GatewayLifecycleEnvironment>,
) -> Result<(), ClientError> {
    let binary = std::env::current_exe().map_err(|e| ClientError::MetadataIo { source: e })?;

    let log_path = gateway_log_path(log_file, environment);
    let (log_file, log_stderr) = open_detached_output(&log_path)?;

    let args = build_spawn_args(port, host.map(str::to_owned), config.map(str::to_owned));

    let mut cmd = std::process::Command::new(&binary);
    cmd.args(&args);
    cmd.stdout(log_file);
    cmd.stderr(log_stderr);
    cmd.stdin(std::process::Stdio::null());
    if let Some(environment) = environment {
        environment.apply_to_command(&mut cmd);
    }

    // Detach the child into its own process group so it outlives the CLI.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    cmd.spawn()
        .map_err(|e| ClientError::MetadataIo { source: e })?;

    Ok(())
}

fn run_foreground_gateway(
    port: Option<u16>,
    host: Option<&str>,
    config: Option<&str>,
    environment: &super::recovery::GatewayLifecycleEnvironment,
) -> Result<(), ClientError> {
    let binary = std::env::current_exe().map_err(|source| ClientError::MetadataIo { source })?;
    let mut command = std::process::Command::new(binary);
    command.args(build_spawn_args(
        port,
        host.map(str::to_owned),
        config.map(str::to_owned),
    ));
    environment.apply_to_command(&mut command);
    let status = command
        .status()
        .map_err(|source| ClientError::MetadataIo { source })?;
    if status.success() {
        Ok(())
    } else {
        Err(ClientError::GatewayNotRunning {
            reason: format!("foreground gateway exited with status {status}"),
        })
    }
}

fn open_detached_output(
    log_path: &std::path::Path,
) -> Result<(std::fs::File, std::fs::File), ClientError> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ClientError::MetadataIo { source: e })?;
    }

    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| ClientError::MetadataIo { source: e })?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| ClientError::MetadataIo { source: e })?;
    Ok((stdout, stderr))
}

// ---------------------------------------------------------------------------
// Health polling
// ---------------------------------------------------------------------------

/// Poll until the gateway is ready, with exponential backoff.
///
/// Reads the runtime metadata file to discover the address, then calls the
/// health endpoint. Returns the address on success or an error string on
/// timeout.
#[cfg(test)]
pub(crate) async fn poll_until_ready(timeout: Duration) -> Result<String, String> {
    poll_until_ready_at(timeout, &RuntimeMetadata::metadata_path()).await
}

async fn poll_until_ready_at(
    timeout: Duration,
    metadata_path: &std::path::Path,
) -> Result<String, String> {
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
        if let Ok(meta) = RuntimeMetadata::load_from(metadata_path) {
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

/// Returns the caller's log path, or the default under the canonical Swage root.
pub(crate) fn gateway_log_path(
    log_file: Option<&str>,
    environment: Option<&super::recovery::GatewayLifecycleEnvironment>,
) -> PathBuf {
    log_file
        .map(PathBuf::from)
        .unwrap_or_else(|| match environment {
            Some(environment) => environment.swage_home().join("gateway.log"),
            None => swage_config::discovery::resolve_root()
                .unwrap_or_else(|_| swage_config::discovery::effective_root_path())
                .join("gateway.log"),
        })
}

/// Builds the argument list for spawning the binary in server mode.
///
/// Always starts with `--server-mode gateway start`, followed by any provided
/// overrides, so the spawned process reuses the same `StartArgs` parser as the
/// foreground CLI path.
///
/// This is a pure function — extracted so tests can verify the args without
/// actually spawning a process.
pub(crate) fn build_spawn_args(
    port: Option<u16>,
    host: Option<String>,
    config: Option<String>,
) -> Vec<String> {
    let mut args = vec![
        "--server-mode".to_string(),
        "gateway".to_string(),
        "start".to_string(),
    ];
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
    use std::io::Write;

    use crate::test_support::{SwageHomeGuard, XdgRuntimeDirGuard};

    use super::*;

    #[test]
    fn log_path_uses_swage_root() {
        let swage_home = SwageHomeGuard::new();
        let path = gateway_log_path(None, None);
        assert_eq!(path, swage_home.path().join("gateway.log"));
    }

    #[test]
    fn explicit_log_path_is_preserved() {
        let path = gateway_log_path(Some("relative/logs/gateway.txt"), None);
        assert_eq!(path, PathBuf::from("relative/logs/gateway.txt"));
    }

    #[test]
    fn detached_stdout_and_stderr_share_the_selected_log() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("nested/gateway.log");
        let (mut stdout, mut stderr) = open_detached_output(&path).unwrap();

        writeln!(stdout, "standard output").unwrap();
        writeln!(stderr, "standard error").unwrap();
        drop(stdout);
        drop(stderr);

        let output = std::fs::read_to_string(path).unwrap();
        assert!(output.contains("standard output"));
        assert!(output.contains("standard error"));
    }

    // When no flags are given, only --server-mode should appear.
    #[test]
    fn spawn_args_with_no_overrides_contains_only_server_mode() {
        let args = build_spawn_args(None, None, None);
        assert_eq!(args, vec!["--server-mode", "gateway", "start"]);
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
            "gateway",
            "start",
            "--port",
            "9090",
            "--host",
            "127.0.0.1",
            "--config",
            "/tmp/cfg.toml",
        ];
        assert_eq!(args, expected);
    }

    // Polling with a very short timeout and no running gateway should return an error.
    #[tokio::test]
    async fn poll_until_ready_times_out_with_no_gateway() {
        use std::time::Duration;
        let _runtime_dir = XdgRuntimeDirGuard::new();
        // Use a 50ms timeout — fast enough for a unit test.
        let result = poll_until_ready(Duration::from_millis(50)).await;
        assert!(result.is_err(), "expected timeout error, got: {result:?}");
    }
}
