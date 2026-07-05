use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};

// ---------------------------------------------------------------------------
// ProviderConfig
// ---------------------------------------------------------------------------

/// Resolved configuration for connecting to an LLM provider.
///
/// The API key is `Option<String>` to support keyless local endpoints (e.g.
/// Ollama running on localhost). Remote providers populate it from an
/// configuration before provider construction. Providers should not know where
/// a credential came from; that keeps secret access in the governed runtime
/// surface that can validate and audit it.
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
    pub fn keyless(model: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: None,
            model: model.into(),
            base_url: base_url.into(),
        }
    }
}

impl
    TryFrom<(
        &swage_config::ProviderEndpointConfig,
        &swage_config::ModelEndpointConfig,
    )> for ProviderConfig
{
    type Error = ProviderError;

    /// Legacy conversion path for callers that have not moved credential
    /// resolution into the gateway yet.
    ///
    /// New gateway-owned provider construction resolves `api_key_secret_ref`
    /// through `SecretManager` and calls `ProviderConfig::new` or `keyless`.
    fn try_from(
        (provider, model): (
            &swage_config::ProviderEndpointConfig,
            &swage_config::ModelEndpointConfig,
        ),
    ) -> Result<Self, Self::Error> {
        let api_key_env = provider
            .api_key_env
            .as_deref()
            .ok_or_else(|| ProviderError::Auth {
                provider: "provider has no api_key_env fallback configured".into(),
            })?;
        let api_key = std::env::var(api_key_env).map_err(|_| ProviderError::Auth {
            provider: format!("environment variable '{api_key_env}' is not set"),
        })?;

        // Derive the provider base URL from the configured protocol.
        let base_url = provider
            .base_url
            .clone()
            .unwrap_or_else(|| default_base_url(provider.protocol.clone()).to_owned());

        Ok(Self {
            api_key: Some(api_key),
            model: model.model_id.clone(),
            base_url,
        })
    }
}

fn default_base_url(protocol: swage_config::Protocol) -> &'static str {
    match protocol {
        swage_config::Protocol::OpenAi => "https://api.openai.com/v1",
        swage_config::Protocol::Anthropic => "https://api.anthropic.com",
        swage_config::Protocol::Google => "https://generativelanguage.googleapis.com",
        swage_config::Protocol::OpenRouter => "https://openrouter.ai/api/v1",
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

    /// Return the provider-visible request body for a completion request.
    ///
    /// Concrete providers override this with their exact wire conversion. The
    /// default keeps tests and unsupported providers debuggable without
    /// pretending to know a provider-specific protocol.
    fn provider_request_snapshot(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "provider": "generic",
            "model_id": self.model_id(),
            "stream": stream,
            "body": request,
        })
    }

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
    fn stream(&self, request: CompletionRequest)
    -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>>;
}

// ---------------------------------------------------------------------------
// UnsupportedLlmProvider
// ---------------------------------------------------------------------------

/// LLM provider placeholder for configurations that must fail closed.
///
/// This preserves gateway startup for recoverable configuration workflows while
/// ensuring a broken provider/model link never falls through to a real backend.
pub struct UnsupportedLlmProvider {
    reason: String,
}

impl UnsupportedLlmProvider {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn error(&self) -> ProviderError {
        ProviderError::InvalidConfig {
            message: self.reason.clone(),
        }
    }
}

#[async_trait]
impl LlmProvider for UnsupportedLlmProvider {
    fn model_id(&self) -> &str {
        "unavailable"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Err(self.error())
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        Err(self.error())
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::once(std::future::ready(
            StreamChunk::Error {
                message: self.reason.clone(),
            },
        )))
    }
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
        ChatMessage, CompletionRequest, CompletionResponse, FinishReason, Role, ToolCall,
        ToolCompletionRequest, ToolCompletionResponse,
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
                StreamChunk::Delta {
                    content: "chunk1".into(),
                },
                StreamChunk::Delta {
                    content: "chunk2".into(),
                },
                StreamChunk::Delta {
                    content: "chunk3".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
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
        let provider = swage_config::ProviderEndpointConfig {
            protocol: swage_config::Protocol::Anthropic,
            api_key_secret_ref: None,
            api_key_env: Some("TEVIK_TEST_KEY_DEFINITELY_NOT_SET_XYZ".into()),
            base_url: None,
        };
        let model = swage_config::ModelEndpointConfig {
            provider_id: "anthropic".into(),
            model_id: "claude".into(),
            aliases: Vec::new(),
            context_window: None,
            generation_preset: None,
            capabilities: swage_config::ModelCapabilitiesConfig::default(),
        };

        // Safety: unsafe needed in edition 2024 for env mutation; only in tests.
        unsafe { std::env::remove_var("TEVIK_TEST_KEY_DEFINITELY_NOT_SET_XYZ") };

        let result = ProviderConfig::try_from((&provider, &model));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProviderError::Auth { .. }));
    }

    #[test]
    fn provider_config_try_from_resolves_env_var() {
        let provider = swage_config::ProviderEndpointConfig {
            protocol: swage_config::Protocol::Anthropic,
            api_key_secret_ref: None,
            api_key_env: Some("TEVIK_TEST_API_KEY_XYZ".into()),
            base_url: Some("http://127.0.0.1:11434/v1".into()),
        };
        let model = swage_config::ModelEndpointConfig {
            provider_id: "anthropic".into(),
            model_id: "claude-sonnet".into(),
            aliases: Vec::new(),
            context_window: None,
            generation_preset: None,
            capabilities: swage_config::ModelCapabilitiesConfig::default(),
        };

        // Safety: unsafe needed in edition 2024 for env mutation; only in tests.
        unsafe { std::env::set_var("TEVIK_TEST_API_KEY_XYZ", "test-key-value") };

        let cfg = ProviderConfig::try_from((&provider, &model)).unwrap();
        assert_eq!(cfg.api_key, Some("test-key-value".into()));
        assert_eq!(cfg.model, "claude-sonnet");
        assert_eq!(cfg.base_url, "http://127.0.0.1:11434/v1");

        unsafe { std::env::remove_var("TEVIK_TEST_API_KEY_XYZ") };
    }

    #[tokio::test]
    async fn unsupported_provider_fails_closed() {
        let provider = UnsupportedLlmProvider::new("provider/model configuration is invalid");
        let request = CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };

        let error = provider.complete(request).await.unwrap_err();

        assert_eq!(provider.model_id(), "unavailable");
        assert!(matches!(
            error,
            ProviderError::InvalidConfig { message }
                if message == "provider/model configuration is invalid"
        ));
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
            tool_definitions: vec![],
        };
        let response = provider.complete(request).await.unwrap();
        assert_eq!(response.content, "mock response");
        assert_eq!(response.input_tokens, 10);
        assert_eq!(response.output_tokens, 5);
        assert_eq!(response.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn mock_complete_with_tools_returns_tool_calls() {
        use swage_core::{FinishReason, ToolCompletionRequest};

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
            tool_definitions: vec![],
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
            tool_definitions: vec![],
        };
        let response = provider.complete(request).await.unwrap();
        assert_eq!(response.content, "mock response");
    }
}
