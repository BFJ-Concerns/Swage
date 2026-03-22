use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};

// ---------------------------------------------------------------------------
// ProviderConfig
// ---------------------------------------------------------------------------

/// Resolved configuration for connecting to an LLM provider.
///
/// The API key is `Option<String>` to support keyless local endpoints (e.g.
/// Ollama running on localhost). Remote providers populate it from an
/// environment variable during resolution.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub model: String,
    pub base_url: String,
}

impl ProviderConfig {
    /// Construct a config with an API key (remote providers).
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            api_key: Some(api_key.into()),
            model: model.into(),
            base_url: base_url.into(),
        }
    }

    /// Construct a config without an API key (local endpoints like Ollama).
    pub fn keyless(
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            api_key: None,
            model: model.into(),
            base_url: base_url.into(),
        }
    }
}

impl TryFrom<&swage_config::ProviderConfig> for ProviderConfig {
    type Error = ProviderError;

    /// Resolve the provider config from a `swage_config::ProviderConfig`.
    ///
    /// Reads the API key from the environment variable named in `api_key_env`.
    /// Returns `ProviderError::Auth` if the variable is not set.
    fn try_from(config: &swage_config::ProviderConfig) -> Result<Self, Self::Error> {
        let api_key = std::env::var(&config.api_key_env).map_err(|_| {
            ProviderError::Auth {
                provider: format!(
                    "environment variable '{}' is not set",
                    config.api_key_env
                ),
            }
        })?;

        // Derive the provider base URL from the configured protocol.
        let base_url = match config.protocol {
            swage_config::Protocol::OpenAi => "https://api.openai.com/v1",
            swage_config::Protocol::Anthropic => "https://api.anthropic.com",
            swage_config::Protocol::Google => "https://generativelanguage.googleapis.com",
            swage_config::Protocol::OpenRouter => "https://openrouter.ai/api/v1",
        };

        Ok(Self {
            api_key: Some(api_key),
            model: config.model.clone(),
            base_url: base_url.to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over any LLM backend.
///
/// Four methods cover the full provider surface:
/// - `model_id` — identify which model this provider targets (synchronous)
/// - `complete` — one-shot text completion
/// - `complete_with_tools` — completion with tool definitions and tool call parsing
/// - `stream` — streaming completion returning a chunk stream
///
/// The trait is object-safe: all async methods return boxed futures (via
/// `async_trait`) and `stream` returns a boxed stream. Implementations must
/// be `Send + Sync` so they can be shared across async tasks.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// The model identifier this provider is configured to use.
    fn model_id(&self) -> &str;

    /// Send a completion request and collect the full response.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError>;

    /// Send a completion request with tool definitions and collect a response
    /// that may include tool call requests.
    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError>;

    /// Send a completion request and return a stream of chunks for progressive
    /// rendering.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>>;
}

// ---------------------------------------------------------------------------
// Blanket impl for Arc<dyn LlmProvider>
// ---------------------------------------------------------------------------

use std::sync::Arc;

/// Allows `Arc<dyn LlmProvider>` to be used anywhere `impl LlmProvider` is
/// expected (e.g. as an argument to `ProviderChainBuilder::provider()`).
///
/// This is useful when a provider is shared across threads (e.g. in tests
/// where you want to observe calls via a shared counter) and must also be
/// handed off to a builder that takes ownership.
#[async_trait]
impl LlmProvider for Arc<dyn LlmProvider> {
    fn model_id(&self) -> &str {
        (**self).model_id()
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        (**self).complete(request).await
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        (**self).complete_with_tools(request).await
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        (**self).stream(request)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use swage_core::{
        ChatMessage, CompletionRequest, CompletionResponse, FinishReason, Role,
        ToolCall, ToolCompletionRequest, ToolCompletionResponse,
    };

    // -----------------------------------------------------------------------
    // MockProvider — minimal impl used to verify the trait compiles and works.
    // -----------------------------------------------------------------------

    struct MockProvider;

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: "mock response".into(),
                input_tokens: 10,
                output_tokens: 5,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            Ok(ToolCompletionResponse {
                content: String::new(),
                input_tokens: 10,
                output_tokens: 5,
                finish_reason: FinishReason::ToolUse,
                cost: None,
                tool_calls: vec![ToolCall {
                    call_id: "call_mock".into(),
                    name: "test_tool".into(),
                    arguments: serde_json::json!({}),
                }],
            })
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            // Three delta chunks followed by Done — the standard stream shape.
            Box::pin(futures::stream::iter(vec![
                StreamChunk::Delta { content: "chunk1".into() },
                StreamChunk::Delta { content: "chunk2".into() },
                StreamChunk::Delta { content: "chunk3".into() },
                StreamChunk::Done { finish_reason: None },
            ]))
        }
    }

    #[test]
    fn provider_config_new() {
        let cfg = ProviderConfig::new("sk-test", "gpt-4o", "https://api.openai.com/v1");
        assert_eq!(cfg.api_key, Some("sk-test".into()));
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn provider_config_keyless_for_local_endpoints() {
        // Local providers like Ollama don't need an API key.
        let cfg = ProviderConfig::keyless("llama3", "http://localhost:11434");
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.model, "llama3");
        assert_eq!(cfg.base_url, "http://localhost:11434");
    }

    #[test]
    fn provider_config_try_from_missing_env_var() {
        // Use a name that is extremely unlikely to be set.
        let raw = swage_config::ProviderConfig {
            protocol: swage_config::Protocol::Anthropic,
            model: "claude".into(),
            api_key_env: "TEVIK_TEST_KEY_DEFINITELY_NOT_SET_XYZ".into(),
        };

        // Safety: unsafe needed in edition 2024 for env mutation; only in tests.
        unsafe { std::env::remove_var("TEVIK_TEST_KEY_DEFINITELY_NOT_SET_XYZ") };

        let result = ProviderConfig::try_from(&raw);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProviderError::Auth { .. }));
    }

    #[test]
    fn provider_config_try_from_resolves_env_var() {
        let raw = swage_config::ProviderConfig {
            protocol: swage_config::Protocol::Anthropic,
            model: "claude-sonnet".into(),
            api_key_env: "TEVIK_TEST_API_KEY_XYZ".into(),
        };

        // Safety: unsafe needed in edition 2024 for env mutation; only in tests.
        unsafe { std::env::set_var("TEVIK_TEST_API_KEY_XYZ", "test-key-value") };

        let cfg = ProviderConfig::try_from(&raw).unwrap();
        assert_eq!(cfg.api_key, Some("test-key-value".into()));
        assert_eq!(cfg.model, "claude-sonnet");
        assert!(cfg.base_url.contains("anthropic"));

        unsafe { std::env::remove_var("TEVIK_TEST_API_KEY_XYZ") };
    }

    // -----------------------------------------------------------------------
    // New four-method trait tests
    // -----------------------------------------------------------------------

    #[test]
    fn mock_model_id_returns_configured_model() {
        let provider = MockProvider;
        assert_eq!(provider.model_id(), "mock-model");
    }

    #[tokio::test]
    async fn mock_complete_returns_completion_response() {
        use swage_core::{CompletionRequest, FinishReason};

        let provider = MockProvider;
        let request = CompletionRequest {
            messages: vec![ChatMessage::new(Role::User, "hello")],
            model: None,
            max_tokens: Some(100),
            temperature: None,
            stop_sequences: None,
            metadata: None,
        };
        let response = provider.complete(request).await.unwrap();
        assert_eq!(response.content, "mock response");
        assert_eq!(response.input_tokens, 10);
        assert_eq!(response.output_tokens, 5);
        assert_eq!(response.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn mock_complete_with_tools_returns_tool_calls() {
        use swage_core::{ToolCompletionRequest, FinishReason};

        let provider = MockProvider;
        let request = ToolCompletionRequest {
            messages: vec![ChatMessage::new(Role::User, "use a tool")],
            model: None,
            max_tokens: Some(100),
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![serde_json::json!({
                "name": "test_tool",
                "description": "A test tool"
            })],
        };
        let response = provider.complete_with_tools(request).await.unwrap();
        assert_eq!(response.finish_reason, FinishReason::ToolUse);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "test_tool");
    }

    #[tokio::test]
    async fn mock_stream_emits_deltas_then_done() {
        use swage_core::CompletionRequest;

        let provider = MockProvider;
        let request = CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
        };
        let chunks: Vec<StreamChunk> = provider.stream(request).collect().await;

        let deltas: Vec<&str> = chunks
            .iter()
            .filter_map(|c| {
                if let StreamChunk::Delta { content } = c {
                    Some(content.as_str())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(deltas, ["chunk1", "chunk2", "chunk3"]);
        assert!(
            matches!(chunks.last(), Some(StreamChunk::Done { .. })),
            "last chunk should be Done"
        );
    }

    /// The trait must be object-safe — verify it can be used as `dyn LlmProvider`.
    #[tokio::test]
    async fn trait_is_object_safe() {
        use swage_core::CompletionRequest;

        let provider: Box<dyn LlmProvider> = Box::new(MockProvider);
        assert_eq!(provider.model_id(), "mock-model");

        let request = CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
        };
        let response = provider.complete(request).await.unwrap();
        assert_eq!(response.content, "mock response");
    }
}
