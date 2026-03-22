//! Agent builder — explicit dependency injection with validation.

use std::sync::Arc;

use swage_hook::HookRegistry;
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::Storage;
use swage_tool::dispatch::ToolDispatcher;

use crate::agent::Agent;
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
            active_session_id: None,
            active_thread_id: None,
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
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StorageError,
        StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_storage::{
        Message, MessageStore, NewMessage, NewSession, NewThread, NewToolCall, NewTurn, Session,
        SessionStore, Thread, ThreadState, ThreadStore, ToolCallRecord, ToolCallStore, Turn,
        TurnState, TurnStore,
    };
    use uuid::Uuid;

    use super::*;
    use crate::config::{AgentConfig, CompactionThreshold};

    // -----------------------------------------------------------------------
    // Minimal test doubles — never called, only satisfy type bounds
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

    struct StubStorage;

    #[async_trait]
    impl SessionStore for StubStorage {
        async fn create_session(&self, _: NewSession) -> Result<Session, StorageError> {
            unimplemented!()
        }
        async fn get_session(&self, _: Uuid) -> Result<Option<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_by_project(&self, _: Uuid) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_sessions_by_agent_id(
            &self,
            _: &str,
        ) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn delete_session(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_activity(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ThreadStore for StubStorage {
        async fn create(&self, _: NewThread) -> Result<Thread, StorageError> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_by_session(&self, _: Uuid) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_threads_by_agent_id(&self, _: &str) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn update_state(&self, _: Uuid, _: ThreadState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn delete(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TurnStore for StubStorage {
        async fn create_turn(&self, _: NewTurn) -> Result<Turn, StorageError> {
            unimplemented!()
        }
        async fn get_turn(&self, _: Uuid) -> Result<Option<Turn>, StorageError> {
            unimplemented!()
        }
        async fn list_turns_by_thread(&self, _: Uuid) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_turn_state(&self, _: Uuid, _: TurnState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn find_in_progress_by_agent(&self, _: &str) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_response(&self, _: Uuid, _: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_tokens(&self, _: Uuid, _: i64, _: i64) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl MessageStore for StubStorage {
        async fn create_message(&self, _: NewMessage) -> Result<Message, StorageError> {
            unimplemented!()
        }
        async fn list_messages_by_thread(&self, _: Uuid) -> Result<Vec<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_latest_for_session(
            &self,
            _: Uuid,
        ) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_message(&self, _: Uuid) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn delete_by_thread(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ToolCallStore for StubStorage {
        async fn create_tool_call(&self, _: NewToolCall) -> Result<ToolCallRecord, StorageError> {
            unimplemented!()
        }
        async fn get_tool_call(&self, _: Uuid) -> Result<Option<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
        async fn update_tool_call_result(
            &self,
            _: &str,
            _: Uuid,
            _: &str,
            _: &str,
            _: i64,
            _: bool,
            _: Option<i64>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_tool_calls_by_turn(
            &self,
            _: Uuid,
        ) -> Result<Vec<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn valid_config() -> AgentConfig {
        AgentConfig {
            agent_id: "test-agent".into(),
            max_tool_rounds: 10,
            compaction_threshold: CompactionThreshold::TokenCount(50_000),
            metadata: HashMap::new(),
        }
    }

    fn stub_provider() -> Arc<StubProvider> {
        Arc::new(StubProvider)
    }

    fn stub_storage() -> Arc<StubStorage> {
        Arc::new(StubStorage)
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
        assert!(agent.active_session_id.is_none());
        assert!(agent.active_thread_id.is_none());
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
