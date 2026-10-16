//! A plugin's brokered process as the gateway places and records it.
//!
//! The kernel's `ProcessPlugin` starts a process per session and speaks the
//! plugin-process contract with it; the gateway decides where that process
//! runs and what is recorded of it. A process runs inside the same sandbox
//! the session's turn built for its commands — the same namespaces,
//! read-only root and mounts — and plainly where the turn itself runs
//! without one (a declared boundary, the sandbox disabled, or the permitted
//! degraded fallback). Every start, stop and exit is an audit event on the
//! plugin, with the session it served.

use std::path::Path;
use std::sync::Arc;

use swage_plugin::{PlainLauncher, PluginError, ProcessLauncher, ProcessObserver, ProcessSpec};
use swage_protocol::plugin::{PLUGIN_SOCKET_ENV, PluginProcessStopReason};
use swage_sandbox::SandboxConfig;
use swage_storage::model::NewAuditEvent;
use swage_storage::store::Storage;
use uuid::Uuid;

/// Where a session's turn places the processes started on its behalf: the
/// boundary its own commands run under.
#[derive(Debug, Clone)]
pub enum ProcessPlacement {
    /// No boundary: the turn itself has none (a declared boundary, or the
    /// sandbox disabled), and the process runs as the gateway does.
    Plain,
    /// Inside bubblewrap with the sandbox `config` — namespaces, read-only
    /// root, mounts — and under its Landlock rules.
    Sandboxed(SandboxConfig),
    /// The permitted fallback the turn's commands run under: no bubblewrap,
    /// but the sandbox's Landlock rules applied by the governor.
    Fallback(SandboxConfig),
}

impl ProcessPlacement {
    pub fn plain() -> Self {
        Self::Plain
    }

    pub fn sandboxed(config: SandboxConfig) -> Self {
        Self::Sandboxed(config)
    }

    pub fn fallback(config: SandboxConfig) -> Self {
        Self::Fallback(config)
    }
}

/// Places a plugin's process where the turn's placement says.
pub struct GatewayProcessLauncher {
    placement: Arc<ProcessPlacement>,
}

impl GatewayProcessLauncher {
    pub fn new(placement: Arc<ProcessPlacement>) -> Self {
        Self { placement }
    }
}

impl ProcessLauncher for GatewayProcessLauncher {
    fn command(
        &self,
        spec: &ProcessSpec,
        socket: &Path,
    ) -> Result<tokio::process::Command, PluginError> {
        // The turn's rules name the session's paths, not the plugin's: the
        // installed package joins them read-only, so the sandbox can mount
        // it and the rules let the program be read and executed from it.
        let with_install = |config: &SandboxConfig| {
            let mut config = config.clone();
            if let Some(install) = spec.command.parent() {
                config.read_only_mounts.push(install.to_path_buf());
                config.landlock_read_paths.push(install.to_path_buf());
            }
            config
        };
        match &*self.placement {
            ProcessPlacement::Sandboxed(config) => {
                let config = with_install(config);
                let mut command = swage_sandbox::build_bwrap_command_for(
                    &config,
                    socket,
                    PLUGIN_SOCKET_ENV,
                    &spec.command,
                    &spec.args,
                    &[],
                )
                .map_err(|error| PluginError::Process(error.to_string()))?;
                command.stdin(std::process::Stdio::null());
                command.stdout(std::process::Stdio::null());
                Ok(command)
            }
            ProcessPlacement::Fallback(config) => {
                let config = with_install(config);
                let mut command =
                    swage_sandbox::governed_command(&config, &spec.command, &spec.args)
                        .map_err(|error| PluginError::Process(error.to_string()))?;
                command.env(PLUGIN_SOCKET_ENV, socket);
                command.stdin(std::process::Stdio::null());
                command.stdout(std::process::Stdio::null());
                Ok(command)
            }
            ProcessPlacement::Plain => PlainLauncher.command(spec, socket),
        }
    }
}

/// Records each process's start, stop and exit as an audit event on the
/// plugin, with the session it served.
pub struct AuditingObserver {
    storage: Arc<dyn Storage>,
    /// The writes in flight, joined by [`Self::flush`] so a stop at gateway
    /// stop is recorded before the runtime goes.
    writes: std::sync::Mutex<tokio::task::JoinSet<()>>,
}

impl AuditingObserver {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self {
            storage,
            writes: std::sync::Mutex::new(tokio::task::JoinSet::new()),
        }
    }

    /// Wait for every event written so far to land.
    pub async fn flush(&self) {
        let mut writes = std::mem::take(
            &mut *self
                .writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        while writes.join_next().await.is_some() {}
    }

    fn record(
        &self,
        plugin: &str,
        session: &str,
        kind: &str,
        summary: String,
        metadata: serde_json::Value,
    ) {
        let storage = Arc::clone(&self.storage);
        let event = NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: session.parse::<Uuid>().ok(),
            entity_kind: "plugin".to_owned(),
            entity_id: plugin.to_owned(),
            event_kind: kind.to_owned(),
            summary,
            metadata,
        };
        let kind = kind.to_owned();
        let mut writes = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while writes.try_join_next().is_some() {}
        writes.spawn(async move {
            if let Err(error) = storage.create_audit_event(event).await {
                tracing::warn!(%error, kind, "plugin process event could not be recorded");
            }
        });
    }
}

impl ProcessObserver for AuditingObserver {
    fn started(&self, plugin: &str, session: &str, pid: u32) {
        self.record(
            plugin,
            session,
            "plugin_process_started",
            format!("Plugin {plugin} started a process for session {session}"),
            serde_json::json!({ "session": session, "pid": pid }),
        );
    }

    fn stopped(&self, plugin: &str, session: &str, reason: PluginProcessStopReason) {
        let reason = serde_json::to_value(reason).unwrap_or(serde_json::Value::Null);
        self.record(
            plugin,
            session,
            "plugin_process_stopped",
            format!("Plugin {plugin}'s process for session {session} was stopped: {reason}"),
            serde_json::json!({ "session": session, "reason": reason }),
        );
    }

    fn exited(&self, plugin: &str, session: &str, detail: &str) {
        self.record(
            plugin,
            session,
            "plugin_process_exited",
            format!("Plugin {plugin}'s process for session {session} ended on its own: {detail}"),
            serde_json::json!({ "session": session, "detail": detail }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use swage_plugin::ProcessLauncher;

    fn spec() -> ProcessSpec {
        ProcessSpec {
            plugin: "driver".into(),
            command: PathBuf::from("/data/plugins/driver/1/driver"),
            args: vec!["--serve".into()],
            idle: Duration::from_secs(60),
            grants: Vec::new(),
            tools: vec!["open".into()],
        }
    }

    fn config() -> SandboxConfig {
        SandboxConfig::builder()
            .working_directory("/tmp/turn-workspace")
            .build()
            .unwrap()
    }

    /// The placement decides the boundary: a sandboxed placement is the
    /// bubblewrap command through the governor, a fallback placement the
    /// governor alone, a plain placement the program itself.
    #[test]
    fn the_launcher_runs_the_program_where_the_placement_says() {
        let socket = Path::new("/run/swage/plugin.sock");
        let program_of =
            |command: &tokio::process::Command| command.as_std().get_program().to_os_string();
        let args_of = |command: &tokio::process::Command| {
            command
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };

        let plain = GatewayProcessLauncher::new(Arc::new(ProcessPlacement::plain()))
            .command(&spec(), socket)
            .unwrap();
        assert_eq!(program_of(&plain), "/data/plugins/driver/1/driver");
        assert_eq!(args_of(&plain), vec!["--serve"]);

        let fallback = GatewayProcessLauncher::new(Arc::new(ProcessPlacement::fallback(config())))
            .command(&spec(), socket)
            .unwrap();
        let governor = swage_sandbox::host_binary().unwrap();
        assert_eq!(program_of(&fallback), governor.as_os_str());
        assert_eq!(
            args_of(&fallback),
            vec![
                "--sandboxed-exec",
                "/data/plugins/driver/1/driver",
                "--serve"
            ]
        );
        let env: Vec<(String, Option<String>)> = fallback
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let rules = env
            .iter()
            .find(|(name, _)| name == swage_sandbox::SANDBOX_CONFIG_ENV)
            .and_then(|(_, value)| value.clone())
            .expect("the rules travel in the environment");
        assert!(
            rules.contains("/data/plugins/driver/1"),
            "the rules let the program be read and run from its install directory: {rules}"
        );
        assert!(env.iter().any(|(name, value)| name == PLUGIN_SOCKET_ENV
            && value.as_deref() == Some("/run/swage/plugin.sock")));

        match GatewayProcessLauncher::new(Arc::new(ProcessPlacement::sandboxed(config())))
            .command(&spec(), socket)
        {
            Ok(sandboxed) => {
                assert!(
                    program_of(&sandboxed).to_string_lossy().ends_with("bwrap"),
                    "a sandboxed placement is the bubblewrap command"
                );
                let args = args_of(&sandboxed);
                assert!(args.iter().any(|arg| arg == "--clearenv"));
                assert!(
                    args.windows(2)
                        .any(|pair| pair[0] == "--ro-bind" && pair[1] == "/data/plugins/driver/1"),
                    "the install directory is bound in: {args:?}"
                );
                assert!(args.iter().any(|arg| arg == "--sandboxed-exec"));
                assert_eq!(args.last().map(String::as_str), Some("--serve"));
            }
            Err(error) => assert!(
                error.to_string().contains("bubblewrap") || error.to_string().contains("bwrap"),
                "without bubblewrap the placement is refused by name: {error}"
            ),
        }
    }
}
