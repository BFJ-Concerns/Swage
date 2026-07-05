use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use chrono::{DateTime, Utc};

use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_agent::ApprovalDecision;
use swage_config::{CommunicationConfig, ResolvedConfig};
use swage_hook::HookRegistry;
use swage_provider::{LlmProvider, UnsupportedVoiceProvider, VoiceProvider};
use swage_storage::{
    model::{ApprovalKind, ApprovalRequest},
    store::{BenchStore, Storage},
};

use crate::channel_gateway::{ChannelOrigin, DeliveryTarget, ExternalChannelIntegration};
use crate::{
    capabilities::WorkflowCapabilities, execution_host::ExecutionHostRuntime,
    lifecycle::ActivityTracker, onboarding::RecoveryExportSnapshot, presence::PresenceRuntime,
    secret::SecretManager, telegram::TelegramClient, tool::ToolDispatcher, websocket::ServerEvent,
};

/// Shared application state available to all route handlers via axum's `State` extractor.
///
/// All fields are wrapped in `Arc` so that cloning the state is cheap.
/// The `active_turns` and `active_connections` fields use `Mutex` to allow
/// concurrent modification from multiple handler tasks.
#[derive(Clone)]
pub struct AppState {
    /// Persistent storage backend (sessions, threads, turns, messages, tool calls).
    pub storage: Arc<dyn Storage>,
    /// Bench storage backend. Separate from `storage` because `BenchStore` is
    /// not included in the `Storage` supertrait — consumers that need bench
    /// operations import it independently.
    pub bench_store: Arc<dyn BenchStore>,
    /// LLM provider for streaming message generation.
    pub provider: Arc<dyn LlmProvider>,
    /// Voice provider for speech-to-text and text-to-speech.
    pub voice_provider: Arc<dyn VoiceProvider>,
    /// Resolved configuration (server, provider, workspace, etc.).
    pub config: ResolvedConfig,
    /// Maps session_id → CancellationToken for turns currently being processed.
    /// Used by `cancel_turn` and by graceful shutdown to interrupt active work.
    pub active_turns: Arc<AsyncMutex<HashMap<Uuid, CancellationToken>>>,
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
    /// Runtime presence for recent clients and registered machines.
    pub presence: Arc<AsyncMutex<PresenceRuntime>>,
    /// Registered local and attached execution hosts.
    pub execution_hosts: Arc<AsyncMutex<ExecutionHostRuntime>>,
    /// One-time node pairing codes awaiting redemption by a remote host.
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
    /// Reschedulable inter-agent message expiry task.
    agent_message_expiry: Arc<Mutex<Option<AgentMessageExpiryRuntime>>>,
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
    pub bench_id: Option<Uuid>,
    pub agent_id: Option<String>,
    pub home_target: Option<DeliveryTarget>,
    pub expires_at: DateTime<Utc>,
}

impl AppState {
    pub fn new(
        storage: Arc<dyn Storage>,
        bench_store: Arc<dyn BenchStore>,
        provider: Arc<dyn LlmProvider>,
        config: ResolvedConfig,
        tool_dispatcher: ToolDispatcher,
        hook_registry: Option<Arc<HookRegistry>>,
    ) -> Self {
        let (broadcast, _) = broadcast::channel(16);
        let workflow_capabilities =
            WorkflowCapabilities::for_protocol(config.default_provider_protocol());
        let tool_dispatcher = Arc::new(tool_dispatcher);
        Self {
            storage: storage.clone(),
            bench_store,
            provider: provider.clone(),
            voice_provider: Arc::new(UnsupportedVoiceProvider::new(
                "voice is unavailable for the configured runtime",
            )),
            config: config.clone(),
            active_turns: Arc::new(AsyncMutex::new(HashMap::new())),
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
            execution_hosts: Arc::new(AsyncMutex::new(ExecutionHostRuntime::default())),
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
            agent_message_expiry: Arc::new(Mutex::new(None)),
        }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApprovalGrantKey {
    pub bench_id: Option<Uuid>,
    pub kind: ApprovalKind,
    pub name: String,
    pub access: Option<String>,
}

impl ApprovalGrantKey {
    pub fn from_approval(approval: &ApprovalRequest) -> Option<Self> {
        let name = match approval.kind {
            ApprovalKind::Tool => approval.tool_name.clone(),
            ApprovalKind::Hook => approval.hook_name.clone(),
            ApprovalKind::Location => None,
        }?;

        Some(Self {
            bench_id: None,
            kind: approval.kind.clone(),
            name,
            access: None,
        })
    }
}

impl ApprovalGrantKey {
    pub fn location(
        bench_id: Option<Uuid>,
        path: impl Into<String>,
        access: impl Into<String>,
    ) -> Self {
        Self {
            bench_id,
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
            Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
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
            Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
            Arc::new(MockProvider::new(vec![])) as Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        assert!(state.approval_channels.lock().unwrap().is_empty());
    }
}
