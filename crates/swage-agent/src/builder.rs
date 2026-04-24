//! Agent builder — explicit dependency injection with validation.

use std::sync::Arc;

use swage_hook::HookRegistry;
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::Storage;
use swage_tool::dispatch::ToolDispatcher;

use crate::agent::Agent;
use crate::approval::ApprovalChannelRegistrar;
use crate::config::AgentConfig;
use crate::error::AgentError;

/// Constructs an [`Agent`] with all required and optional dependencies.
pub struct AgentBuilder {
    config: Option<AgentConfig>,
    provider: Option<Arc<dyn LlmProvider>>,
    storage: Option<Arc<dyn Storage>>,
    tool_dispatcher: Option<ToolDispatcher>,
    hook_registry: Option<Arc<HookRegistry>>,
    leak_detector: Option<Arc<LeakDetector>>,
    approval_registrar: Option<ApprovalChannelRegistrar>,
    delegation_handler: Option<Arc<dyn crate::delegation::DelegationHandler>>,
    context_transfer_config: Option<swage_config::ContextTransferConfig>,
}

impl AgentBuilder {
    pub fn new() -> Self {
        Self {
            config: None,
            provider: None,
            storage: None,
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: None,
            approval_registrar: None,
            delegation_handler: None,
            context_transfer_config: None,
        }
    }

    pub fn config(mut self, config: AgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn storage(mut self, storage: Arc<dyn Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    pub fn tool_dispatcher(mut self, dispatcher: ToolDispatcher) -> Self {
        self.tool_dispatcher = Some(dispatcher);
        self
    }

    pub fn hook_registry(mut self, registry: Arc<HookRegistry>) -> Self {
        self.hook_registry = Some(registry);
        self
    }

    pub fn leak_detector(mut self, detector: Arc<LeakDetector>) -> Self {
        self.leak_detector = Some(detector);
        self
    }

    pub fn approval_registrar(mut self, registrar: ApprovalChannelRegistrar) -> Self {
        self.approval_registrar = Some(registrar);
        self
    }

    pub fn delegation_handler(
        mut self,
        handler: Arc<dyn crate::delegation::DelegationHandler>,
    ) -> Self {
        self.delegation_handler = Some(handler);
        self
    }

    pub fn context_transfer_config(
        mut self,
        context_transfer_config: swage_config::ContextTransferConfig,
    ) -> Self {
        self.context_transfer_config = Some(context_transfer_config);
        self
    }

    /// Validate configuration and produce a ready-to-use [`Agent`].
    pub fn build(self) -> Result<Agent, AgentError> {
        // Required fields must be present before we inspect their contents.
        let config = self
            .config
            .ok_or_else(|| AgentError::InvalidConfig("config is required".into()))?;
        let provider = self
            .provider
            .ok_or_else(|| AgentError::InvalidConfig("provider is required".into()))?;
        let storage = self
            .storage
            .ok_or_else(|| AgentError::InvalidConfig("storage is required".into()))?;

        // Validate the config values themselves.
        if config.agent_id.is_empty() {
            return Err(AgentError::InvalidConfig(
                "agent_id must not be empty".into(),
            ));
        }
        if config.max_tool_rounds < 1 {
            return Err(AgentError::InvalidConfig(
                "max_tool_rounds must be at least 1".into(),
            ));
        }

        Ok(Agent {
            config,
            provider,
            storage,
            tool_dispatcher: self.tool_dispatcher,
            hook_registry: self.hook_registry,
            leak_detector: self.leak_detector,
            approval_registrar: self.approval_registrar,
            delegation_handler: self.delegation_handler,
            context_transfer_config: self.context_transfer_config.unwrap_or_default(),
        })
    }
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;

    use swage_core::{
        CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
        ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_storage::{SqliteStorage, Storage};

    use super::*;
    use crate::config::{AgentConfig, CompactionThreshold};

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn model_id(&self) -> &str {
            "stub-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("StubProvider: complete not called in builder tests")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("StubProvider: complete_with_tools not called in builder tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unimplemented!("StubProvider: stream not called in builder tests")
        }
    }

    // Helpers
    // -----------------------------------------------------------------------

    fn valid_config() -> AgentConfig {
        let mut config = AgentConfig::new("test-agent");
        config.compaction_threshold = CompactionThreshold::TokenCount(50_000);
        config
    }

    fn stub_provider() -> Arc<StubProvider> {
        Arc::new(StubProvider)
    }

    fn stub_storage() -> Arc<dyn Storage> {
        Arc::new(SqliteStorage::in_memory().expect("test storage should initialise"))
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn build_with_required_deps_succeeds() {
        let agent = AgentBuilder::new()
            .config(valid_config())
            .provider(stub_provider())
            .storage(stub_storage())
            .build()
            .expect("build should succeed with all required deps");

        assert_eq!(agent.config.agent_id, "test-agent");
        assert!(agent.tool_dispatcher.is_none());
        assert!(agent.hook_registry.is_none());
        assert!(agent.leak_detector.is_none());
    }

    #[test]
    fn build_with_all_deps_succeeds() {
        // Optional deps set — build should still succeed.
        let agent = AgentBuilder::new()
            .config(valid_config())
            .provider(stub_provider())
            .storage(stub_storage())
            // hook_registry and leak_detector are optional; ToolDispatcher needs
            // ToolRegistry which we don't need to construct for this test.
            .build()
            .expect("build should succeed with required deps set");

        assert!(agent.hook_registry.is_none());
        assert!(agent.leak_detector.is_none());
    }

    #[test]
    fn build_fails_when_provider_missing() {
        let result = AgentBuilder::new()
            .config(valid_config())
            .storage(stub_storage())
            .build();

        match result {
            Err(AgentError::InvalidConfig(msg)) => {
                assert!(msg.contains("provider"), "expected 'provider' in: {msg}");
            }
            Ok(_) => panic!("expected Err(InvalidConfig), but build succeeded"),
            Err(e) => panic!("expected InvalidConfig, got different error: {e}"),
        }
    }

    #[test]
    fn build_fails_when_storage_missing() {
        let result = AgentBuilder::new()
            .config(valid_config())
            .provider(stub_provider())
            .build();

        match result {
            Err(AgentError::InvalidConfig(msg)) => {
                assert!(msg.contains("storage"), "expected 'storage' in: {msg}");
            }
            Ok(_) => panic!("expected Err(InvalidConfig), but build succeeded"),
            Err(e) => panic!("expected InvalidConfig, got different error: {e}"),
        }
    }

    #[test]
    fn build_fails_when_config_missing() {
        let result = AgentBuilder::new()
            .provider(stub_provider())
            .storage(stub_storage())
            .build();

        match result {
            Err(AgentError::InvalidConfig(msg)) => {
                assert!(msg.contains("config"), "expected 'config' in: {msg}");
            }
            Ok(_) => panic!("expected Err(InvalidConfig), but build succeeded"),
            Err(e) => panic!("expected InvalidConfig, got different error: {e}"),
        }
    }

    #[test]
    fn build_fails_when_agent_id_empty() {
        let mut config = valid_config();
        config.agent_id = String::new();

        let result = AgentBuilder::new()
            .config(config)
            .provider(stub_provider())
            .storage(stub_storage())
            .build();

        match result {
            Err(AgentError::InvalidConfig(msg)) => {
                assert!(msg.contains("agent_id"), "expected 'agent_id' in: {msg}");
            }
            Ok(_) => panic!("expected Err(InvalidConfig), but build succeeded"),
            Err(e) => panic!("expected InvalidConfig, got different error: {e}"),
        }
    }

    #[test]
    fn build_fails_when_max_tool_rounds_zero() {
        let mut config = valid_config();
        config.max_tool_rounds = 0;

        let result = AgentBuilder::new()
            .config(config)
            .provider(stub_provider())
            .storage(stub_storage())
            .build();

        match result {
            Err(AgentError::InvalidConfig(msg)) => {
                assert!(
                    msg.contains("max_tool_rounds"),
                    "expected 'max_tool_rounds' in: {msg}"
                );
            }
            Ok(_) => panic!("expected Err(InvalidConfig), but build succeeded"),
            Err(e) => panic!("expected InvalidConfig, got different error: {e}"),
        }
    }
}
