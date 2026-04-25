use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};

use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_agent::ApprovalDecision;
use swage_config::ResolvedConfig;
use swage_hook::HookRegistry;
use swage_provider::{LlmProvider, UnsupportedVoiceProvider, VoiceProvider};
use swage_storage::store::{BenchStore, Storage};

use crate::channel_gateway::ChannelOrigin;
use crate::{
    capabilities::WorkflowCapabilities, execution_host::ExecutionHostRuntime,
    lifecycle::ActivityTracker, presence::PresenceRuntime, secret::SecretManager,
    tool::ToolDispatcher, websocket::ServerEvent,
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
    /// Used by the idle lifecycle monitor to trigger automatic shutdown when
    /// no activity has been seen for a configured duration.
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
    /// Canonical secret-management and resolution surface.
    pub secret_manager: Arc<SecretManager>,
    /// Capability surface used to gate attachment and media workflows.
    pub workflow_capabilities: WorkflowCapabilities,
}

#[derive(Debug, Clone)]
pub struct PairingCodeRecord {
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
            WorkflowCapabilities::for_protocol(config.provider.protocol.clone());
        Self {
            storage: storage.clone(),
            bench_store,
            provider,
            voice_provider: Arc::new(UnsupportedVoiceProvider::new(
                "voice is unavailable for the configured runtime",
            )),
            config,
            active_turns: Arc::new(AsyncMutex::new(HashMap::new())),
            active_connections: Arc::new(AsyncMutex::new(HashSet::new())),
            tool_dispatcher: Arc::new(tool_dispatcher),
            activity_tracker: Arc::new(ActivityTracker::new()),
            broadcast,
            shutdown_signal: CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
            hook_registry,
            channel_routes: Arc::new(AsyncMutex::new(HashMap::new())),
            presence: Arc::new(AsyncMutex::new(PresenceRuntime::default())),
            execution_hosts: Arc::new(AsyncMutex::new(ExecutionHostRuntime::default())),
            pairing_codes: Arc::new(AsyncMutex::new(HashMap::new())),
            secret_manager: Arc::new(SecretManager::new(storage.clone())),
            workflow_capabilities,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use swage_agent::approval::ApprovalDecisionValue;

    use super::*;
    use crate::turn::tests::{MockProvider, MockStorage};

    fn make_minimal_state() -> AppState {
        let storage = Arc::new(MockStorage::new());
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            bench_store: Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
            provider: Arc::new(MockProvider::new(vec![])) as Arc<dyn swage_provider::LlmProvider>,
            voice_provider: Arc::new(UnsupportedVoiceProvider::new(
                "voice is unavailable for the configured runtime",
            )) as Arc<dyn swage_provider::VoiceProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(AsyncMutex::new(HashMap::new())),
            active_connections: Arc::new(AsyncMutex::new(HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
            hook_registry: None,
            channel_routes: Arc::new(AsyncMutex::new(HashMap::new())),
            presence: Arc::new(AsyncMutex::new(PresenceRuntime::default())),
            execution_hosts: Arc::new(AsyncMutex::new(ExecutionHostRuntime::default())),
            pairing_codes: Arc::new(AsyncMutex::new(HashMap::new())),
            secret_manager: Arc::new(SecretManager::new(
                Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
            )),
            workflow_capabilities: WorkflowCapabilities::default(),
        }
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
