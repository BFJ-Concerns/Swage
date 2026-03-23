use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_config::ResolvedConfig;
use swage_provider::LlmProvider;
use swage_sandbox::Sandbox;
use swage_storage::store::{ProjectStore, Storage};

use crate::{lifecycle::ActivityTracker, tool::ToolDispatcher, websocket::ServerEvent};

/// Shared application state available to all route handlers via axum's `State` extractor.
///
/// All fields are wrapped in `Arc` so that cloning the state is cheap.
/// The `active_turns` and `active_connections` fields use `Mutex` to allow
/// concurrent modification from multiple handler tasks.
#[derive(Clone)]
pub struct AppState {
    /// Persistent storage backend (sessions, threads, turns, messages, tool calls).
    pub storage: Arc<dyn Storage>,
    /// Project storage backend. Separate from `storage` because `ProjectStore` is
    /// not included in the `Storage` supertrait — consumers that need project
    /// operations import it independently.
    pub project_store: Arc<dyn ProjectStore>,
    /// LLM provider for streaming message generation.
    pub provider: Arc<dyn LlmProvider>,
    /// Resolved configuration (server, provider, workspace, etc.).
    pub config: ResolvedConfig,
    /// Maps session_id → CancellationToken for turns currently being processed.
    /// Used by `cancel_turn` and by graceful shutdown to interrupt active work.
    pub active_turns: Arc<Mutex<HashMap<Uuid, CancellationToken>>>,
    /// Set of session_ids that currently have an active WebSocket connection.
    /// Prevents duplicate connections to the same conversation.
    pub active_connections: Arc<Mutex<HashSet<Uuid>>>,
    /// Dispatch pipeline: resolves tool lookups, permission checks, rate limiting,
    /// schema validation, execution, and leak detection for every tool call.
    pub tool_dispatcher: Arc<ToolDispatcher>,
    /// Sandboxed agent process for isolated tool execution.
    ///
    /// `None` when sandbox is disabled (config.sandbox.enabled = false) or when
    /// bubblewrap is not available on the host. Tool calls fall back to the normal
    /// dispatch path in either case.
    pub sandbox: Option<Arc<dyn Sandbox>>,
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
}

impl AppState {
    pub fn new(
        storage: Arc<dyn Storage>,
        project_store: Arc<dyn ProjectStore>,
        provider: Arc<dyn LlmProvider>,
        config: ResolvedConfig,
        tool_dispatcher: ToolDispatcher,
        sandbox: Option<Arc<dyn Sandbox>>,
    ) -> Self {
        let (broadcast, _) = broadcast::channel(16);
        Self {
            storage,
            project_store,
            provider,
            config,
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(HashSet::new())),
            tool_dispatcher: Arc::new(tool_dispatcher),
            sandbox,
            activity_tracker: Arc::new(ActivityTracker::new()),
            broadcast,
            shutdown_signal: CancellationToken::new(),
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

    use async_trait::async_trait;
    use serde_json::Value;
    use swage_core::SandboxError;
    use swage_sandbox::{Sandbox, ToolCallResult};
    use tokio::sync::Mutex;

    use super::*;
    use crate::turn::tests::{MockProvider, MockStorage};

    // Minimal no-op sandbox for testing AppState field construction.
    struct NoopSandbox;

    #[async_trait]
    impl Sandbox for NoopSandbox {
        async fn spawn_agent(&mut self) -> Result<(), SandboxError> {
            Ok(())
        }

        async fn execute_tool(
            &self,
            _name: &str,
            _params: Value,
        ) -> Result<ToolCallResult, SandboxError> {
            Ok(ToolCallResult::success(Value::Null, 0))
        }

        async fn shutdown(&mut self) -> Result<(), SandboxError> {
            Ok(())
        }
    }

    fn make_minimal_state(sandbox: Option<Arc<dyn Sandbox>>) -> AppState {
        let storage = Arc::new(MockStorage::new());
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: Arc::new(MockProvider::new(vec![])) as Arc<dyn swage_provider::LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: CancellationToken::new(),
        }
    }

    #[test]
    fn sandbox_is_none_when_constructed_with_none() {
        let state = make_minimal_state(None);
        assert!(state.sandbox.is_none(), "sandbox should be None when not provided");
    }

    #[test]
    fn sandbox_is_some_when_constructed_with_sandbox() {
        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let state = make_minimal_state(Some(sandbox));
        assert!(state.sandbox.is_some(), "sandbox should be Some when provided");
    }

    #[test]
    fn appstate_new_sets_sandbox_field() {
        let storage = Arc::new(MockStorage::new());
        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            Arc::clone(&storage) as Arc<dyn swage_storage::store::ProjectStore>,
            Arc::new(MockProvider::new(vec![])) as Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            Some(Arc::clone(&sandbox)),
        );
        assert!(state.sandbox.is_some());
    }
}
