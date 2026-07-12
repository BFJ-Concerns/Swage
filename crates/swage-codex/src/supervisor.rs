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
    Stopped,
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("failed to start Codex App Server: {0}")]
    Spawn(String),
    #[error(transparent)]
    Client(#[from] ClientError),
}

pub struct AppServerHandle {
    pub client: AppServerClient,
    pub user_agent: Option<String>,
    pub cli_version: Option<String>,
}

struct RunningServer {
    process: ChildProcessManager,
    client: AppServerClient,
    user_agent: Option<String>,
    cli_version: Option<String>,
}

/// Lazily owns one app-server process for the gateway.
pub struct AppServerSupervisor {
    config: AppServerConfig,
    running: Mutex<Option<RunningServer>>,
    events: broadcast::Sender<RuntimeEvent>,
}

impl AppServerSupervisor {
    pub fn new(config: AppServerConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(32);
        Arc::new(Self {
            config,
            running: Mutex::new(None),
            events,
        })
    }

    pub fn subscribe_runtime(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.events.subscribe()
    }

    pub async fn ensure_running(&self) -> Result<AppServerHandle, SupervisorError> {
        let mut running = self.running.lock().await;
        if let Some(server) = running.as_mut()
            && server.process.is_running()
        {
            return Ok(AppServerHandle {
                client: server.client.clone(),
                user_agent: server.user_agent.clone(),
                cli_version: server.cli_version.clone(),
            });
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
        let mut process = ChildProcessManager::spawn(command)
            .map_err(|error| SupervisorError::Spawn(error.to_string()))?;
        let pid = process.pid();
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
                return Err(error.into());
            }
        };
        if let Err(error) = client.notify("initialized", json!({})).await {
            let _ = process.graceful_shutdown(Duration::from_secs(1)).await;
            return Err(error.into());
        }
        let user_agent = result
            .get("userAgent")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let _ = self.events.send(RuntimeEvent::Spawned {
            command: self.config.command.clone(),
            pid,
            cli_version: cli_version.clone(),
        });
        let _ = self.events.send(RuntimeEvent::Initialised {
            user_agent: user_agent.clone(),
        });
        *running = Some(RunningServer {
            process,
            client: client.clone(),
            user_agent: user_agent.clone(),
            cli_version: cli_version.clone(),
        });
        Ok(AppServerHandle {
            client,
            user_agent,
            cli_version,
        })
    }

    pub async fn client(&self) -> Result<AppServerClient, SupervisorError> {
        let mut running = self.running.lock().await;
        if let Some(server) = running.as_mut()
            && server.process.is_running()
        {
            return Ok(server.client.clone());
        }
        drop(running);
        Ok(self.ensure_running().await?.client)
    }

    pub async fn shutdown(&self) {
        let mut running = self.running.lock().await;
        if let Some(mut server) = running.take() {
            if let Err(error) = server
                .process
                .graceful_shutdown(Duration::from_secs(2))
                .await
            {
                tracing::warn!(error = %error, "failed to stop Codex App Server cleanly");
            }
            let _ = self.events.send(RuntimeEvent::Stopped);
        }
    }
}
