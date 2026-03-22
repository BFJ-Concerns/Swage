//! `SwageBuilder` — fluent entry point for constructing an `Agent`.
//!
//! The builder validates required dependencies, applies sensible defaults for
//! optional components, and delegates final construction to [`AgentBuilder`].
//!
//! # Example
//!
//! ```rust,ignore
//! use swage::SwageBuilder;
//!
//! let agent = SwageBuilder::new()
//!     .provider(my_provider)
//!     .agent_id("my-agent")
//!     .storage(my_storage)
//!     .build()?;
//! ```

use std::sync::Arc;

use crate::agent::{Agent, AgentBuilder, AgentConfig};
use crate::core::{GatewayError, SwageError};
use crate::hook::HookRegistry;
use crate::provider::LlmProvider;
use crate::safety::LeakDetector;
use crate::storage::Storage;
use crate::tool::dispatch::ToolDispatcher;

// ---------------------------------------------------------------------------
// SwageBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for constructing a ready-to-use [`Agent`].
///
/// ## Required fields
///
/// - [`provider`](SwageBuilder::provider) — the LLM backend
/// - [`agent_id`](SwageBuilder::agent_id) — unique identifier for this agent instance
/// - [`storage`](SwageBuilder::storage) — persistence backend
///
/// ## Optional fields
///
/// All optional fields have sensible defaults (empty tool registry, default-deny
/// permissions, no hooks, standard leak detection, default agent config).
pub struct SwageBuilder {
    provider: Option<Arc<dyn LlmProvider>>,
    agent_id: Option<String>,
    storage: Option<Arc<dyn Storage>>,
    tool_dispatcher: Option<ToolDispatcher>,
    hook_registry: Option<Arc<HookRegistry>>,
    leak_detector: Option<Arc<LeakDetector>>,
    agent_config: Option<AgentConfig>,
}

impl SwageBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self {
            provider: None,
            agent_id: None,
            storage: None,
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: None,
            agent_config: None,
        }
    }

    /// Set the LLM provider. **Required.**
    pub fn provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Set the agent identifier. **Required.**
    ///
    /// This string scopes all storage records created by this agent.
    pub fn agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    /// Set the storage backend. **Required.**
    pub fn storage(mut self, storage: Arc<dyn Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Override the tool dispatcher.
    ///
    /// When omitted, no tools are registered and all tool calls return a
    /// permission-denied result.
    pub fn tool_dispatcher(mut self, dispatcher: ToolDispatcher) -> Self {
        self.tool_dispatcher = Some(dispatcher);
        self
    }

    /// Override the hook registry.
    ///
    /// When omitted, no lifecycle hooks are registered.
    pub fn hook_registry(mut self, registry: Arc<HookRegistry>) -> Self {
        self.hook_registry = Some(registry);
        self
    }

    /// Override the leak detector.
    ///
    /// When omitted, [`LeakDetector::default()`] is used (standard credential
    /// pattern library enabled).
    pub fn leak_detector(mut self, detector: impl Into<Arc<LeakDetector>>) -> Self {
        self.leak_detector = Some(detector.into());
        self
    }

    /// Override the full agent configuration.
    ///
    /// When omitted, [`AgentConfig::new`] is used with sensible
    /// defaults (10 tool rounds, 100-message compaction threshold).
    ///
    /// Note: when both `agent_id()` and `agent_config()` are set, the
    /// `agent_id` value is ignored in favour of the config's `agent_id` field.
    pub fn agent_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = Some(config);
        self
    }

    /// Validate and construct the [`Agent`].
    ///
    /// # Errors
    ///
    /// Returns [`SwageError`] wrapping a [`GatewayError`] if any required field is missing.
    pub fn build(self) -> Result<Agent, SwageError> {
        let provider = self.provider.ok_or_else(|| SwageError::Gateway(GatewayError::ConfigError(
            "provider is required — call .provider(p) before .build()".into(),
        )))?;

        let agent_id = self.agent_id.ok_or_else(|| {
            SwageError::Gateway(GatewayError::ConfigError(
                "agent_id is required — call .agent_id(id) before .build()".into(),
            ))
        })?;

        let storage = self.storage.ok_or_else(|| {
            SwageError::Gateway(GatewayError::ConfigError(
                "storage is required — call .storage(s) before .build()".into(),
            ))
        })?;

        // Use explicit config if provided, otherwise build from agent_id with defaults.
        let config = self
            .agent_config
            .unwrap_or_else(|| AgentConfig::new(agent_id));

        let mut builder = AgentBuilder::new()
            .config(config)
            .provider(provider)
            .storage(storage);

        if let Some(dispatcher) = self.tool_dispatcher {
            builder = builder.tool_dispatcher(dispatcher);
        }

        if let Some(registry) = self.hook_registry {
            builder = builder.hook_registry(registry);
        }

        if let Some(detector) = self.leak_detector {
            builder = builder.leak_detector(detector);
        }

        builder
            .build()
            .map_err(|e| SwageError::Gateway(GatewayError::ConfigError(e.to_string())))
    }
}

impl Default for SwageBuilder {
    fn default() -> Self {
        Self::new()
    }
}
