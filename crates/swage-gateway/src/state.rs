use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use chrono::{DateTime, Utc};

use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_agent::ApprovalDecision;
use swage_codex::{AppServerConfig, AppServerSupervisor, RuntimeEvent};
use swage_config::{CommunicationConfig, ResolvedConfig};
use swage_hook::HookRegistry;
use swage_provider::{LlmProvider, UnsupportedVoiceProvider, VoiceProvider};
use swage_storage::{
    model::{ApprovalKind, ApprovalRequest, NewAuditEvent},
    store::{SpaceStore, Storage},
};

use crate::channel_gateway::{ChannelOrigin, DeliveryTarget, ExternalChannelIntegration};
use crate::{
    capabilities::WorkflowCapabilities, device::DeviceRuntime, lifecycle::ActivityTracker,
    onboarding::RecoveryExportSnapshot, presence::PresenceRuntime, secret::SecretManager,
    telegram::TelegramClient, tool::ToolDispatcher, websocket::ServerEvent,
};

/// Shared application state available to all route handlers via axum's `State` extractor.
///
/// All fields are wrapped in `Arc` so that cloning the state is cheap.
/// The `active_turns` and `active_connections` fields use `Mutex` to allow
/// concurrent modification from multiple handler tasks.
#[derive(Clone)]
pub struct AppState {
    /// Configuration root selected when this gateway instance was constructed.
    pub config_root: PathBuf,
    /// Persistent storage backend (sessions, threads, turns, messages, tool calls).
    pub storage: Arc<dyn Storage>,
    /// Space storage backend. Separate from `storage` because `SpaceStore` is
    /// not included in the `Storage` supertrait — consumers that need space
    /// operations import it independently.
    pub space_store: Arc<dyn SpaceStore>,
    /// LLM provider for streaming message generation.
    pub provider: Arc<dyn LlmProvider>,
    /// Voice provider for speech-to-text and text-to-speech.
    pub voice_provider: Arc<dyn VoiceProvider>,
    /// Resolved configuration (server, provider, workspace, etc.).
    pub config: ResolvedConfig,
    /// Maps session_id → CancellationToken for turns currently being processed.
    /// Used by `cancel_turn` and by graceful shutdown to interrupt active work.
    pub active_turns: Arc<AsyncMutex<HashMap<Uuid, CancellationToken>>>,
    /// Detached workflow runs addressable by run id for cancellation.
    pub active_workflow_runs: Arc<AsyncMutex<HashMap<Uuid, CancellationToken>>>,
    /// In-memory liveness evidence for workflow workers which have not yet
    /// written their terminal journal result.
    pub active_workflow_workers: Arc<AsyncMutex<HashMap<Uuid, swage_workflow::WorkerLiveness>>>,
    /// Set of session_ids that currently have an active WebSocket connection.
    /// Prevents duplicate connections to the same conversation.
    pub active_connections: Arc<AsyncMutex<HashSet<Uuid>>>,
    /// Dispatch pipeline: resolves tool lookups, permission checks, rate limiting,
    /// schema validation, execution, and leak detection for every tool call.
    pub tool_dispatcher: Arc<ToolDispatcher>,
    /// Records the timestamp of the last significant gateway activity.
    ///
    /// Used by operator and presence surfaces to report how recently the
    /// gateway handled non-health work.
    pub activity_tracker: Arc<ActivityTracker>,

    /// Broadcast channel for server-wide event delivery.
    ///
    /// Used by the shutdown endpoint to push a `Shutdown` event to all
    /// connected WebSocket clients before the gateway terminates. Each
    /// WebSocket connection subscribes a receiver and merges it with its
    /// per-turn `mpsc` channel in the forwarding task.
    pub broadcast: broadcast::Sender<ServerEvent>,

    /// Cancellation token that triggers graceful shutdown on demand.
    ///
    /// The HTTP shutdown endpoint signals this token; the server's shutdown
    /// future awaits it alongside SIGTERM so either mechanism causes an
    /// orderly drain.
    pub shutdown_signal: CancellationToken,

    /// Routes approval decisions from WebSocket handlers to waiting agent tasks.
    ///
    /// When an agent suspends on a tool awaiting human approval it inserts a
    /// `oneshot::Sender<ApprovalDecision>` keyed by `approval_id`. The WebSocket
    /// and REST handlers remove the sender and fire it when the client submits a
    /// decision. A plain std mutex is sufficient here — the critical section is a
    /// single HashMap operation with no async work under the lock.
    pub approval_channels: Arc<Mutex<HashMap<Uuid, oneshot::Sender<ApprovalDecision>>>>,
    /// Runtime-only approval grants created by "allow for this session".
    ///
    /// These are intentionally process-local: they affect newly built turn
    /// dispatchers until the gateway exits, without rewriting user config.
    pub session_approval_grants: Arc<Mutex<HashSet<ApprovalGrantKey>>>,
    /// Optional lifecycle hooks honoured by gateway-owned execution paths.
    pub hook_registry: Option<Arc<HookRegistry>>,
    /// Session routing map for external channels.
    pub channel_routes: Arc<AsyncMutex<HashMap<ChannelOrigin, Uuid>>>,
    /// Runtime presence for recent clients and registered devices.
    pub presence: Arc<AsyncMutex<PresenceRuntime>>,
    /// Registered local and attached devices.
    pub devices: Arc<AsyncMutex<DeviceRuntime>>,
    /// One-time device pairing codes awaiting redemption by a remote device.
    pub pairing_codes: Arc<AsyncMutex<HashMap<String, PairingCodeRecord>>>,
    /// One-time external-channel link codes awaiting redemption by a chat sender.
    pub channel_link_codes: Arc<AsyncMutex<HashMap<String, ChannelLinkCodeRecord>>>,
    /// Canonical secret-management and resolution surface.
    pub secret_manager: Arc<SecretManager>,
    /// Configured Telegram bot client, if the channel is enabled.
    pub telegram: Option<Arc<TelegramClient>>,
    /// Recently produced recovery exports available for operator download.
    ///
    /// Recovery bundles include secret material when managed secrets are in use,
    /// so this catalogue is intentionally process-local until the product has
    /// an explicit encrypted archive retention model.
    pub recovery_exports: Arc<AsyncMutex<Vec<RecoveryExportSnapshot>>>,
    /// Capability surface used to gate attachment and media workflows.
    pub workflow_capabilities: WorkflowCapabilities,
    /// True after persisted global settings differ from this running process.
    pub restart_required: Arc<AtomicBool>,
    /// Whether this process can restart itself via the desktop/runtime wrapper.
    pub managed_gateway: bool,
    /// Hot-swapped runtime config used by new turns and settings reads.
    pub runtime_config: Arc<RwLock<ResolvedConfig>>,
    /// Hot-swapped provider catalogue used by new turns.
    pub runtime_provider: Arc<RwLock<Arc<dyn LlmProvider>>>,
    /// Hot-swapped base tool dispatcher used to build new turn dispatchers.
    pub runtime_tool_dispatcher: Arc<RwLock<Arc<ToolDispatcher>>>,
    /// One long-lived Codex App Server, started lazily by sign-in, discovery,
    /// or the first Codex-backed turn.
    pub codex_supervisor: Arc<AppServerSupervisor>,
    /// Runtime-owned git worktrees used by placed workflow and delegated workers.
    pub worktree_host: Arc<crate::worktree::WorktreeHost>,
    /// Reschedulable inter-agent message expiry task.
    agent_message_expiry: Arc<Mutex<Option<AgentMessageExpiryRuntime>>>,
    #[cfg(test)]
    dispatcher_build_failures_remaining: Arc<AtomicUsize>,
    #[cfg(test)]
    test_sandbox_enabled: bool,
}

struct AgentMessageExpiryRuntime {
    communication: CommunicationConfig,
    shutdown: CancellationToken,
    _handle: JoinHandle<()>,
}

#[derive(Debug, Clone)]
pub struct PairingCodeRecord {
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ChannelLinkCodeRecord {
    pub integration: ExternalChannelIntegration,
    pub space_id: Option<Uuid>,
    pub agent_id: Option<String>,
    pub home_target: Option<DeliveryTarget>,
    pub expires_at: DateTime<Utc>,
}

impl AppState {
    /// Owner directories that must survive startup recovery because the
    /// workflow wake loop can replay them from durable placement journals.
    pub(crate) async fn replayable_worktree_owner_ids(&self) -> Result<HashSet<Uuid>, String> {
        let mut owners = self
            .storage
            .list_interrupted_workflow_runs()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|run| run.id)
            .collect::<HashSet<_>>();

        let space_ids = self
            .space_store
            .list_all()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|space| space.id)
            .collect::<HashSet<_>>();
        owners.extend(
            self.storage
                .list_parked_workflow_runs()
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|run| space_ids.contains(&run.space_id))
                .map(|run| run.id),
        );
        Ok(owners)
    }

    pub fn new(
        storage: Arc<dyn Storage>,
        space_store: Arc<dyn SpaceStore>,
        provider: Arc<dyn LlmProvider>,
        config: ResolvedConfig,
        tool_dispatcher: ToolDispatcher,
        hook_registry: Option<Arc<HookRegistry>>,
    ) -> Self {
        let config_root = swage_config::discovery::resolve_root()
            .unwrap_or_else(|_| swage_config::discovery::effective_root_path());
        Self::new_with_config_root(
            storage,
            space_store,
            provider,
            config,
            tool_dispatcher,
            hook_registry,
            config_root,
        )
    }

    /// Construct application state with a root already selected by the caller.
    ///
    /// Startup and root-sensitive tests use this entry point so state
    /// construction cannot be redirected by a concurrent process-environment
    /// change between root discovery and retention.
    pub fn new_with_config_root(
        storage: Arc<dyn Storage>,
        space_store: Arc<dyn SpaceStore>,
        provider: Arc<dyn LlmProvider>,
        config: ResolvedConfig,
        tool_dispatcher: ToolDispatcher,
        hook_registry: Option<Arc<HookRegistry>>,
        config_root: PathBuf,
    ) -> Self {
        let (broadcast, _) = broadcast::channel(16);
        let workflow_capabilities =
            WorkflowCapabilities::for_protocol(config.default_provider_protocol());
        let tool_dispatcher = Arc::new(tool_dispatcher);
        let codex_endpoint = config
            .providers
            .entries
            .values()
            .find(|provider| provider.protocol == swage_config::Protocol::Codex);
        let codex_state_dir = codex_endpoint
            .and_then(|provider| provider.state_dir.clone())
            .or_else(|| Some(config_root.join("codex")))
            .unwrap_or_else(|| std::path::PathBuf::from(".swage/codex"));
        let mut codex_config = AppServerConfig::codex(codex_state_dir);
        if let Some(command) = codex_endpoint.and_then(|provider| provider.command.clone()) {
            codex_config.command = command;
        }
        let codex_supervisor = AppServerSupervisor::new(codex_config);
        let worktree_root = config_root.join("worker-worktrees");
        let mut codex_runtime_events = codex_supervisor.subscribe_runtime();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let audit_storage = storage.clone();
            runtime.spawn(async move {
                while let Ok(event) = codex_runtime_events.recv().await {
                    if let Err(error) = record_codex_runtime_event(&audit_storage, event).await {
                        tracing::warn!(%error, "failed to record Codex runtime lifecycle");
                    }
                }
            });
        }
        Self {
            config_root,
            storage: storage.clone(),
            space_store,
            provider: provider.clone(),
            voice_provider: Arc::new(UnsupportedVoiceProvider::new(
                "voice is unavailable for the configured runtime",
            )),
            config: config.clone(),
            active_turns: Arc::new(AsyncMutex::new(HashMap::new())),
            active_workflow_runs: Arc::new(AsyncMutex::new(HashMap::new())),
            active_workflow_workers: Arc::new(AsyncMutex::new(HashMap::new())),
            active_connections: Arc::new(AsyncMutex::new(HashSet::new())),
            tool_dispatcher: tool_dispatcher.clone(),
            activity_tracker: Arc::new(ActivityTracker::new()),
            broadcast,
            shutdown_signal: CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
            session_approval_grants: Arc::new(Mutex::new(HashSet::new())),
            hook_registry,
            channel_routes: Arc::new(AsyncMutex::new(HashMap::new())),
            presence: Arc::new(AsyncMutex::new(PresenceRuntime::default())),
            devices: Arc::new(AsyncMutex::new(DeviceRuntime::default())),
            pairing_codes: Arc::new(AsyncMutex::new(HashMap::new())),
            channel_link_codes: Arc::new(AsyncMutex::new(HashMap::new())),
            secret_manager: Arc::new(SecretManager::new(storage.clone())),
            telegram: None,
            recovery_exports: Arc::new(AsyncMutex::new(Vec::new())),
            workflow_capabilities,
            restart_required: Arc::new(AtomicBool::new(false)),
            managed_gateway: std::env::var("SWAGE_MANAGED_GATEWAY")
                .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            runtime_config: Arc::new(RwLock::new(config)),
            runtime_provider: Arc::new(RwLock::new(provider)),
            runtime_tool_dispatcher: Arc::new(RwLock::new(tool_dispatcher)),
            codex_supervisor,
            worktree_host: Arc::new(crate::worktree::WorktreeHost::new(
                crate::worktree::WorktreeConfig {
                    root: worktree_root,
                },
            )),
            agent_message_expiry: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            dispatcher_build_failures_remaining: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            test_sandbox_enabled: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_worktree_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.worktree_host = Arc::new(crate::worktree::WorktreeHost::new(
            crate::worktree::WorktreeConfig { root: root.into() },
        ));
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_sandbox(mut self) -> Self {
        self.test_sandbox_enabled = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn test_sandbox_enabled(&self) -> bool {
        self.test_sandbox_enabled
    }

    #[cfg(test)]
    pub(crate) fn fail_next_dispatcher_builds(&self, count: usize) {
        self.dispatcher_build_failures_remaining
            .store(count, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn take_dispatcher_build_failure(&self) -> bool {
        self.dispatcher_build_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    pub fn clear_restart_required(&self) {
        self.restart_required.store(false, Ordering::SeqCst);
    }

    pub fn restart_required(&self) -> bool {
        self.restart_required.load(Ordering::SeqCst)
    }

    pub fn active_config(&self) -> ResolvedConfig {
        self.runtime_config
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn active_provider(&self) -> Arc<dyn LlmProvider> {
        self.runtime_provider
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn active_tool_dispatcher(&self) -> Arc<ToolDispatcher> {
        self.runtime_tool_dispatcher
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn replace_runtime(
        &self,
        config: ResolvedConfig,
        provider: Arc<dyn LlmProvider>,
        tool_dispatcher: ToolDispatcher,
    ) {
        let previous_communication = self.active_config().communication;
        let next_communication = config.communication.clone();
        *self
            .runtime_config
            .write()
            .unwrap_or_else(|error| error.into_inner()) = config;
        *self
            .runtime_provider
            .write()
            .unwrap_or_else(|error| error.into_inner()) = provider;
        *self
            .runtime_tool_dispatcher
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Arc::new(tool_dispatcher);
        self.reschedule_agent_message_expiry_if_needed(previous_communication, next_communication);
        self.clear_restart_required();
    }

    pub fn start_agent_message_expiry(&self) {
        self.replace_agent_message_expiry(self.active_config().communication);
    }

    pub fn stop_agent_message_expiry(&self) {
        if let Some(runtime) = self
            .agent_message_expiry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            runtime.shutdown.cancel();
        }
    }

    fn reschedule_agent_message_expiry_if_needed(
        &self,
        previous: CommunicationConfig,
        next: CommunicationConfig,
    ) {
        if previous == next {
            return;
        }
        let should_reschedule = self
            .agent_message_expiry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|runtime| runtime.communication != next);
        if should_reschedule {
            self.replace_agent_message_expiry(next);
        }
    }

    fn replace_agent_message_expiry(&self, communication: CommunicationConfig) {
        let ttl = std::time::Duration::from_secs(communication.message_ttl_secs);
        let interval = std::time::Duration::from_secs(communication.expiry_interval_secs);
        let shutdown = CancellationToken::new();
        let handle = swage_agent::expiry::spawn_expiry_task_until_cancelled(
            self.storage.clone(),
            ttl,
            interval,
            shutdown.clone(),
        );
        let previous = self
            .agent_message_expiry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .replace(AgentMessageExpiryRuntime {
                communication,
                shutdown,
                _handle: handle,
            });
        if let Some(previous) = previous {
            previous.shutdown.cancel();
        }
    }

    #[cfg(test)]
    pub(crate) fn active_agent_message_expiry_communication(&self) -> Option<CommunicationConfig> {
        self.agent_message_expiry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|runtime| runtime.communication.clone())
    }
}

async fn record_codex_runtime_event(
    storage: &Arc<dyn Storage>,
    event: RuntimeEvent,
) -> Result<(), swage_core::StorageError> {
    let (summary, metadata) = match event {
        RuntimeEvent::Spawned {
            command,
            pid,
            cli_version,
        } => (
            "Codex App Server spawned",
            serde_json::json!({
                "lifecycle":"spawned",
                "command":command,
                "pid":pid,
                "cli_version":cli_version,
            }),
        ),
        RuntimeEvent::Initialised { user_agent } => (
            "Codex App Server initialised",
            serde_json::json!({
                "lifecycle":"initialised",
                "user_agent":user_agent,
            }),
        ),
        RuntimeEvent::Stopped { reason } => (
            "Codex App Server stopped",
            serde_json::json!({
                "lifecycle":"stopped",
                "reason":reason,
            }),
        ),
    };
    storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "gateway".to_string(),
            entity_id: "codex-app-server".to_string(),
            event_kind: "codex_runtime".to_string(),
            summary: summary.to_string(),
            metadata,
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod codex_runtime_audit_tests {
    use super::*;
    use swage_storage::SqliteStorage;

    #[tokio::test(flavor = "multi_thread")]
    async fn lifecycle_events_are_recorded_at_gateway_grain() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        for event in [
            RuntimeEvent::Spawned {
                command: "codex".to_string(),
                pid: 42,
                cli_version: Some("codex 1".to_string()),
            },
            RuntimeEvent::Initialised {
                user_agent: Some("swage-test".to_string()),
            },
            RuntimeEvent::Stopped {
                reason: "process exited unexpectedly".to_string(),
            },
        ] {
            record_codex_runtime_event(&storage, event).await.unwrap();
        }

        let events = storage.list_audit_events().await.unwrap();
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|event| {
            event.event_kind == "codex_runtime"
                && event.entity_kind == "gateway"
                && event.entity_id == "codex-app-server"
                && event.session_id.is_none()
        }));
        let mut lifecycles = events
            .iter()
            .filter_map(|event| event.metadata["lifecycle"].as_str())
            .collect::<Vec<_>>();
        lifecycles.sort_unstable();
        assert_eq!(lifecycles, ["initialised", "spawned", "stopped"]);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApprovalGrantKey {
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub kind: ApprovalKind,
    pub name: String,
    pub access: Option<String>,
}

impl ApprovalGrantKey {
    pub fn from_approval(approval: &ApprovalRequest, session_id: Option<Uuid>) -> Option<Self> {
        let name = match approval.kind {
            ApprovalKind::Tool => approval.tool_name.clone(),
            ApprovalKind::Hook => approval.hook_name.clone(),
            ApprovalKind::Location => None,
        }?;

        Some(Self {
            session_id,
            space_id: None,
            kind: approval.kind.clone(),
            name,
            access: None,
        })
    }
}

impl ApprovalGrantKey {
    pub fn location(
        session_id: Option<Uuid>,
        space_id: Option<Uuid>,
        path: impl Into<String>,
        access: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            space_id,
            kind: ApprovalKind::Location,
            name: path.into(),
            access: Some(access.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_agent::approval::ApprovalDecisionValue;

    use super::*;
    use crate::turn::tests::{MockProvider, MockStorage};

    fn make_minimal_state() -> AppState {
        let storage = Arc::new(MockStorage::new());
        AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
            Arc::new(MockProvider::new(vec![])) as Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
    }

    #[test]
    fn approval_channels_starts_empty() {
        // Newly constructed state must have an empty approval_channels map —
        // no leftover entries from a previous session.
        let state = make_minimal_state();
        let guard = state
            .approval_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(
            guard.is_empty(),
            "approval_channels should be empty on construction"
        );
    }

    #[tokio::test]
    async fn approval_decision_roundtrip() {
        // An agent inserts a oneshot sender keyed by approval_id;
        // the WebSocket handler removes it and fires the decision.
        let state = make_minimal_state();
        let approval_id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel::<ApprovalDecision>();
        state
            .approval_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(approval_id, tx);
        let sender = state
            .approval_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&approval_id)
            .expect("sender should be present");
        sender
            .send(ApprovalDecision {
                decision: ApprovalDecisionValue::Approved,
                scope: swage_agent::approval::ApprovalDecisionScope::Once,
                reason: None,
            })
            .unwrap();
        let decision = rx.await.expect("receiver should get decision");
        assert_eq!(decision.decision, ApprovalDecisionValue::Approved);
        assert_eq!(decision.reason, None);
    }

    #[test]
    fn appstate_new_initialises_approval_channels() {
        let storage = Arc::new(MockStorage::new());
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
            Arc::new(MockProvider::new(vec![])) as Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        assert!(state.approval_channels.lock().unwrap().is_empty());
    }
}
