use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_config::ResolvedConfig;
use swage_provider::LlmProvider;
use swage_storage::store::Storage;

use crate::tool::ToolRegistry;

/// Shared application state available to all route handlers via axum's `State` extractor.
///
/// All fields are wrapped in `Arc` so that cloning the state is cheap.
/// The `active_turns` and `active_connections` fields use `Mutex` to allow
/// concurrent modification from multiple handler tasks.
#[derive(Clone)]
pub struct AppState {
    /// Persistent storage backend.
    pub storage: Arc<dyn Storage>,
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
    /// Registered built-in tools available for the model to invoke.
    pub tool_registry: Arc<ToolRegistry>,
}

impl AppState {
    pub fn new(
        storage: Arc<dyn Storage>,
        provider: Arc<dyn LlmProvider>,
        config: ResolvedConfig,
        tool_registry: ToolRegistry,
    ) -> Self {
        Self {
            storage,
            provider,
            config,
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(HashSet::new())),
            tool_registry: Arc::new(tool_registry),
        }
    }
}
