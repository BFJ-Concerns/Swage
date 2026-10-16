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
/// sandbox its commands run in, or none where the turn runs without one.
#[derive(Debug, Clone)]
pub struct ProcessPlacement {
    pub sandbox: Option<SandboxConfig>,
}

impl ProcessPlacement {
    /// No boundary: the process runs as the gateway does.
    pub fn plain() -> Self {
        Self { sandbox: None }
    }

    /// Inside the sandbox `config` describes.
    pub fn sandboxed(config: SandboxConfig) -> Self {
        Self {
            sandbox: Some(config),
        }
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
        match &self.placement.sandbox {
            Some(config) => {
                let mut command = swage_sandbox::build_bwrap_command_for(
                    config,
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
            None => PlainLauncher.command(spec, socket),
        }
    }
}

/// Records each process's start, stop and exit as an audit event on the
/// plugin, with the session it served.
pub struct AuditingObserver {
    storage: Arc<dyn Storage>,
}

impl AuditingObserver {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
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
        tokio::spawn(async move {
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
