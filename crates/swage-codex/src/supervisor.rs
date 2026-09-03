use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use serde_json::{Value, json};
use swage_sandbox::ChildProcessManager;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, broadcast};

use crate::{AppServerClient, client::ClientError};

#[derive(Debug, Clone)]
pub struct AppServerConfig {
    pub command: String,
    pub state_dir: PathBuf,
    pub request_timeout: Duration,
}

impl AppServerConfig {
    pub fn codex(state_dir: PathBuf) -> Self {
        Self {
            command: "codex".to_string(),
            state_dir,
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Spawned {
        command: String,
        pid: u32,
        cli_version: Option<String>,
    },
    Initialised {
        user_agent: Option<String>,
    },
    Stopped {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("failed to start Codex App Server: {0}")]
    Spawn(String),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("Codex App Server restart is temporarily throttled; retry in {retry_after_ms}ms")]
    Backoff { retry_after_ms: u64 },
}

pub struct AppServerHandle {
    pub client: AppServerClient,
    pub user_agent: Option<String>,
    pub cli_version: Option<String>,
}

struct RunningServer {
    process: Option<ChildProcessManager>,
    client: AppServerClient,
    user_agent: Option<String>,
    cli_version: Option<String>,
    started_at: tokio::time::Instant,
}

#[derive(Default)]
struct SupervisorState {
    running: Option<RunningServer>,
    consecutive_failures: u32,
    next_spawn_at: Option<tokio::time::Instant>,
}

/// Lazily owns one app-server process for the gateway.
pub struct AppServerSupervisor {
    config: AppServerConfig,
    state: Mutex<SupervisorState>,
    events: broadcast::Sender<RuntimeEvent>,
}

impl AppServerSupervisor {
    pub fn new(config: AppServerConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(32);
        Arc::new(Self {
            config,
            state: Mutex::new(SupervisorState::default()),
            events,
        })
    }

    /// Construct a supervisor around an already-connected transport.
    ///
    /// This keeps protocol scenario tests on the same `ensure_running()` path
    /// without launching a platform process merely to join two duplex streams.
    #[doc(hidden)]
    /// The directory the Codex process is given as `CODEX_HOME` — where the
    /// CLI keeps its own auth store and session state.
    pub fn state_dir(&self) -> PathBuf {
        self.config.state_dir.clone()
    }

    pub fn with_client(config: AppServerConfig, client: AppServerClient) -> Arc<Self> {
        let supervisor = Self::new(config);
        if let Ok(mut state) = supervisor.state.try_lock() {
            state.running = Some(RunningServer {
                process: None,
                client,
                user_agent: Some("swage-fake-app-server/stream".to_string()),
                cli_version: Some("fake-stream".to_string()),
                started_at: tokio::time::Instant::now(),
            });
        }
        supervisor
    }

    pub fn subscribe_runtime(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.events.subscribe()
    }

    pub async fn ensure_running(&self) -> Result<AppServerHandle, SupervisorError> {
        let mut state = self.state.lock().await;
        if let Some((handle, stable)) = state.running.as_mut().and_then(|server| {
            server
                .process
                .as_mut()
                .is_none_or(ChildProcessManager::is_running)
                .then(|| {
                    (
                        AppServerHandle {
                            client: server.client.clone(),
                            user_agent: server.user_agent.clone(),
                            cli_version: server.cli_version.clone(),
                        },
                        server.started_at.elapsed() >= Duration::from_secs(30),
                    )
                })
        }) {
            // A process which survives ordinary use clears crash-loop history.
            if stable {
                state.consecutive_failures = 0;
                state.next_spawn_at = None;
            }
            return Ok(handle);
        }

        if state.running.as_mut().is_some_and(|server| {
            server
                .process
                .as_mut()
                .is_some_and(|process| !process.is_running())
        }) {
            state.running.take();
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            let delay = restart_delay(state.consecutive_failures);
            state.next_spawn_at = Some(tokio::time::Instant::now() + delay);
            let _ = self.events.send(RuntimeEvent::Stopped {
                reason: "process exited unexpectedly".to_string(),
            });
        }
        if let Some(next_spawn_at) = state.next_spawn_at
            && next_spawn_at > tokio::time::Instant::now()
        {
            let retry_after_ms = u64::try_from(
                next_spawn_at
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis(),
            )
            .unwrap_or(u64::MAX);
            return Err(SupervisorError::Backoff { retry_after_ms });
        }

        std::fs::create_dir_all(&self.config.state_dir)
            .map_err(|error| SupervisorError::Spawn(error.to_string()))?;
        let cli_version = tokio::process::Command::new(&self.config.command)
            .arg("--version")
            .output()
            .await
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|version| version.trim().to_string())
            .filter(|version| !version.is_empty());
        let mut command = tokio::process::Command::new(&self.config.command);
        command
            .arg("app-server")
            .env("CODEX_HOME", &self.config.state_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = match ChildProcessManager::spawn(command) {
            Ok(process) => process,
            Err(error) => {
                register_start_failure(&mut state);
                return Err(SupervisorError::Spawn(error.to_string()));
            }
        };
        let pid = process.pid();
        let _ = self.events.send(RuntimeEvent::Spawned {
            command: self.config.command.clone(),
            pid,
            cli_version: cli_version.clone(),
        });
        let stdin = process
            .take_stdin()
            .ok_or_else(|| SupervisorError::Spawn("app-server stdin unavailable".to_string()))?;
        let stdout = process
            .take_stdout()
            .ok_or_else(|| SupervisorError::Spawn("app-server stdout unavailable".to_string()))?;
        if let Some(stderr) = process.take_stderr() {
            // A long-lived child must have every pipe drained; otherwise a
            // verbose CLI can fill the kernel buffer and stall unrelated RPCs.
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(message = %line, "Codex App Server stderr");
                }
            });
        }
        let client = AppServerClient::new(stdin, stdout, self.config.request_timeout);
        let result = match client
            .request(
                "initialize",
                json!({
                    "clientInfo": {"name":"swage", "version":env!("CARGO_PKG_VERSION")},
                    "capabilities": {"experimentalApi":true}
                }),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let _ = process.graceful_shutdown(Duration::from_secs(1)).await;
                register_start_failure(&mut state);
                let _ = self.events.send(RuntimeEvent::Stopped {
                    reason: format!("initialisation failed: {error}"),
                });
                return Err(error.into());
            }
        };
        if let Err(error) = client.notify("initialized", json!({})).await {
            let _ = process.graceful_shutdown(Duration::from_secs(1)).await;
            register_start_failure(&mut state);
            let _ = self.events.send(RuntimeEvent::Stopped {
                reason: format!("initialised notification failed: {error}"),
            });
            return Err(error.into());
        }
        let user_agent = result
            .get("userAgent")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let _ = self.events.send(RuntimeEvent::Initialised {
            user_agent: user_agent.clone(),
        });
        state.next_spawn_at = None;
        state.running = Some(RunningServer {
            process: Some(process),
            client: client.clone(),
            user_agent: user_agent.clone(),
            cli_version: cli_version.clone(),
            started_at: tokio::time::Instant::now(),
        });
        Ok(AppServerHandle {
            client,
            user_agent,
            cli_version,
        })
    }

    pub async fn client(&self) -> Result<AppServerClient, SupervisorError> {
        let mut state = self.state.lock().await;
        if let Some(server) = state.running.as_mut()
            && server
                .process
                .as_mut()
                .is_none_or(ChildProcessManager::is_running)
        {
            return Ok(server.client.clone());
        }
        drop(state);
        Ok(self.ensure_running().await?.client)
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        if let Some(mut server) = state.running.take() {
            if let Some(process) = server.process.as_mut()
                && let Err(error) = process.graceful_shutdown(Duration::from_secs(2)).await
            {
                tracing::warn!(error = %error, "failed to stop Codex App Server cleanly");
            }
            let _ = self.events.send(RuntimeEvent::Stopped {
                reason: "gateway shutdown".to_string(),
            });
        }
    }
}

fn register_start_failure(state: &mut SupervisorState) {
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    state.next_spawn_at =
        Some(tokio::time::Instant::now() + restart_delay(state.consecutive_failures));
}

fn restart_delay(consecutive_failures: u32) -> Duration {
    const BASE_MS: u64 = 250;
    const MAX_MS: u64 = 5_000;
    let exponent = consecutive_failures.saturating_sub(1).min(5);
    Duration::from_millis((BASE_MS.saturating_mul(1_u64 << exponent)).min(MAX_MS))
}

#[cfg(test)]
mod tests {
    use super::restart_delay;
    use std::time::Duration;

    #[test]
    fn restart_delay_uses_a_small_exponential_cap() {
        assert_eq!(restart_delay(1), Duration::from_millis(250));
        assert_eq!(restart_delay(2), Duration::from_millis(500));
        assert_eq!(restart_delay(8), Duration::from_secs(5));
        assert_eq!(restart_delay(u32::MAX), Duration::from_secs(5));
    }
}
