use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use swage_core::{LlmRequest, LlmResponse, ProviderError, StreamChunk};

/// Resolved configuration for connecting to an LLM provider.
///
/// The API key is already resolved — no further environment lookups needed.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

impl ProviderConfig {
    /// Construct a config from explicit values.
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
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
    /// Returns `ProviderError::AuthError` if the variable is not set.
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
            api_key,
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
/// Implementations must be `Send + Sync` so they can be shared across async
/// tasks without additional synchronisation.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a message and collect the full response before returning.
    async fn send_message(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError>;

    /// Send a message and return a stream of chunks for progressive rendering.
    fn stream_message(
        &self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use swage_core::{ChatMessage, Role, TokenUsage};

    // -----------------------------------------------------------------------
    // MockProvider — minimal impl used to verify the trait compiles and works.
    // -----------------------------------------------------------------------

    struct MockProvider;

    #[async_trait]
    impl LlmProvider for MockProvider {
        async fn send_message(
            &self,
            _request: LlmRequest,
        ) -> Result<LlmResponse, ProviderError> {
            Ok(LlmResponse {
                message: ChatMessage::new(Role::Assistant, "mock response"),
                usage: TokenUsage { input_tokens: 10, output_tokens: 5 },
                model: "mock-model".into(),
            })
        }

        fn stream_message(
            &self,
            _request: LlmRequest,
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

    #[tokio::test]
    async fn mock_send_message_returns_canned_response() {
        let provider = MockProvider;
        let req = LlmRequest::builder("mock").build();
        let resp = provider.send_message(req).await.unwrap();

        assert_eq!(resp.message.role, Role::Assistant);
        // content is Vec<ContentPart>; extract the text of the first part.
        let text = resp.message.content.iter()
            .filter_map(|p| if let swage_core::ContentPart::Text { text } = p { Some(text.as_str()) } else { None })
            .next()
            .unwrap_or("");
        assert_eq!(text, "mock response");
        assert_eq!(resp.model, "mock-model");
    }

    #[tokio::test]
    async fn mock_stream_message_emits_three_deltas_then_done() {
        let provider = MockProvider;
        let req = LlmRequest::builder("mock").build();
        let chunks: Vec<StreamChunk> = provider.stream_message(req).collect().await;

        // Verify delta contents.
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

    #[test]
    fn provider_config_new() {
        let cfg = ProviderConfig::new("sk-test", "gpt-4o", "https://api.openai.com/v1");
        assert_eq!(cfg.api_key, "sk-test");
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
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
        assert_eq!(cfg.api_key, "test-key-value");
        assert_eq!(cfg.model, "claude-sonnet");
        assert!(cfg.base_url.contains("anthropic"));

        unsafe { std::env::remove_var("TEVIK_TEST_API_KEY_XYZ") };
    }
}
