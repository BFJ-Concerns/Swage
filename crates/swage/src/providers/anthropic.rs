//! Anthropic LLM provider implementation.
//!
//! Supports the Anthropic Messages API with streaming and approximate
//! token counting. Unlike OpenAI, Anthropic does not provide a public
//! tokeniser, so token counts are estimated using a simple heuristic.

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::config::ProviderConfig;
use crate::error::ProviderError;
use crate::http::{map_reqwest_error, map_status_code, parse_sse_line, HttpClient};
use crate::provider::{
    CompletionRequest, CompletionResponse, LlmProvider, StreamChunk, TokenUsage,
};
use crate::rate_limit::{RateLimitConfig, RateLimiter};
use crate::secret::SecretProvider;

/// Default base URL for Anthropic API.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Default environment variable for Anthropic API key.
const DEFAULT_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Anthropic API version to use.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic LLM provider implementation.
///
/// Supports the Anthropic Messages API with streaming and approximate
/// token counting. System messages are extracted from the messages array
/// and placed in the top-level "system" field as required by the API.
pub struct AnthropicProvider {
    client: HttpClient,
    base_url: String,
    api_key: String,
    rate_limiter: RateLimiter,
}

impl AnthropicProvider {
    /// Creates a new Anthropic provider with the given configuration.
    ///
    /// Resolves the API key from the secret provider using the configured
    /// environment variable name (or `ANTHROPIC_API_KEY` if not specified).
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::Auth` if the API key cannot be retrieved.
    /// Returns `ProviderError::InvalidConfig` if the HTTP client fails to initialise.
    pub async fn new(
        config: &ProviderConfig,
        secrets: &impl SecretProvider,
    ) -> Result<Self, ProviderError> {
        // Determine which environment variable to use for the API key
        let api_key_env = config
            .api_key_env
            .as_deref()
            .unwrap_or(DEFAULT_API_KEY_ENV);

        // Resolve API key from secrets provider
        let api_key = secrets
            .get_secret(api_key_env)
            .await
            .map_err(|e| ProviderError::Auth(format!("failed to retrieve API key: {}", e)))?;

        // Use custom base URL or default
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        // Create rate limiter from config
        let rate_limiter = RateLimiter::new(RateLimitConfig::new(
            config.requests_per_minute.map(|n| n as usize),
            config.tokens_per_minute.map(|n| n as usize),
        ));

        // Create HTTP client
        let client = HttpClient::new()?;

        Ok(Self {
            client,
            base_url,
            api_key,
            rate_limiter,
        })
    }

    /// Builds the Anthropic API request body from a completion request.
    ///
    /// Extracts system messages into the top-level "system" field,
    /// as required by the Anthropic API.
    fn build_request_body(request: &CompletionRequest, stream: bool) -> AnthropicRequest {
        // Extract system message (take first system message, if any)
        let system = request
            .messages
            .iter()
            .find(|m| matches!(m.role, crate::message::Role::System))
            .map(|m| m.content.clone());

        // Build messages array with only user and assistant messages
        let messages = request
            .messages
            .iter()
            .filter(|m| !matches!(m.role, crate::message::Role::System))
            .map(|m| AnthropicMessage {
                role: match m.role {
                    crate::message::Role::User => "user".to_string(),
                    crate::message::Role::Assistant => "assistant".to_string(),
                    crate::message::Role::System => unreachable!("filtered out above"),
                },
                content: m.content.clone(),
            })
            .collect();

        AnthropicRequest {
            model: request.model.clone(),
            messages,
            system,
            temperature: request.temperature,
            max_tokens: request.max_tokens.unwrap_or(4096), // Anthropic requires max_tokens
            stream: if stream { Some(true) } else { None },
        }
    }

    /// Parses the Anthropic API response into a CompletionResponse.
    fn parse_response(response: AnthropicResponse) -> Result<CompletionResponse, ProviderError> {
        // Extract content from first content block
        let content = response
            .content
            .into_iter()
            .next()
            .map(|c| c.text)
            .unwrap_or_default();

        Ok(CompletionResponse {
            content,
            usage: TokenUsage {
                prompt_tokens: response.usage.input_tokens,
                completion_tokens: response.usage.output_tokens,
                total_tokens: response.usage.input_tokens + response.usage.output_tokens,
            },
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        // Estimate tokens for rate limiting (rough approximation).
        // Serialisation failure here is acceptable: the request will be serialised again
        // for the actual HTTP call where errors will surface. Use conservative fallback
        // (1000 tokens) to avoid underestimating and hitting provider rate limits.
        let estimated_tokens = self.count_tokens(&serde_json::to_string(request).unwrap_or_default()).await.unwrap_or(1000);

        // Wait for rate limiter
        self.rate_limiter.acquire(estimated_tokens).await;

        let url = format!("{}/messages", self.base_url);
        let body = Self::build_request_body(request, false);

        let response = self
            .client
            .client()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            // Try to parse error response for better error messages
            let error_text = response.text().await.unwrap_or_else(|_| "(unable to read error body)".to_string());
            let error_message = parse_api_error(&error_text).unwrap_or(error_text);
            return Err(map_status_code(status, error_message));
        }

        let anthropic_response: AnthropicResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::InvalidRequest(format!("failed to parse response: {}", e)))?;

        Self::parse_response(anthropic_response)
    }

    async fn stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>, ProviderError> {
        // Estimate tokens for rate limiting.
        // Serialisation failure here is acceptable: the request will be serialised again
        // for the actual HTTP call where errors will surface. Use conservative fallback
        // (1000 tokens) to avoid underestimating and hitting provider rate limits.
        let estimated_tokens = self.count_tokens(&serde_json::to_string(request).unwrap_or_default()).await.unwrap_or(1000);

        // Wait for rate limiter
        self.rate_limiter.acquire(estimated_tokens).await;

        let url = format!("{}/messages", self.base_url);
        let body = Self::build_request_body(request, true);

        let response = self
            .client
            .client()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "(unable to read error body)".to_string());
            let error_message = parse_api_error(&error_text).unwrap_or(error_text);
            return Err(map_status_code(status, error_message));
        }

        // Convert response body to stream of SSE events
        let byte_stream = response.bytes_stream();

        let stream = byte_stream
            .map(|result| {
                result
                    .map_err(map_reqwest_error)
                    .and_then(|bytes| {
                        // Parse bytes as UTF-8 lines
                        let text = String::from_utf8_lossy(&bytes);
                        parse_sse_chunks(&text)
                    })
            })
            // Flatten nested results and filter empty chunks
            .flat_map(|result| {
                futures::stream::iter(match result {
                    Ok(chunks) => chunks.into_iter().map(Ok).collect::<Vec<_>>(),
                    Err(e) => vec![Err(e)],
                })
            });

        Ok(Box::pin(stream))
    }

    async fn count_tokens(&self, text: &str) -> Result<usize, ProviderError> {
        // Anthropic does not provide a public tokeniser, so we use an approximate count.
        // Heuristic: split on whitespace and punctuation, average ~4 characters per token.
        // This is a rough approximation but sufficient for rate limiting purposes.

        let char_count = text.chars().count();
        let estimated_tokens = (char_count as f64 / 4.0).ceil() as usize;

        Ok(estimated_tokens)
    }
}

/// Parses SSE chunks from a text buffer into stream chunks.
///
/// Anthropic uses different event types than OpenAI:
/// - "content_block_delta" events contain delta.text
/// - "message_stop" event signals completion
fn parse_sse_chunks(text: &str) -> Result<Vec<StreamChunk>, ProviderError> {
    let mut chunks = Vec::new();

    for line in text.lines() {
        if let Some(data) = parse_sse_line(line) {
            // Check for message_stop event (sent as event type, not in data)
            // In practice, we can stop when we see this in the JSON

            // Parse JSON chunk
            match serde_json::from_str::<AnthropicStreamEvent>(data) {
                Ok(chunk) => match chunk {
                    AnthropicStreamEvent::ContentBlockDelta { delta, .. } => {
                        if let Some(text) = delta.text {
                            if !text.is_empty() {
                                chunks.push(StreamChunk { delta: text });
                            }
                        }
                    }
                    AnthropicStreamEvent::MessageStop => {
                        // End of stream
                        break;
                    }
                    _ => {
                        // Ignore other event types (message_start, content_block_start, etc.)
                    }
                },
                Err(e) => {
                    tracing::debug!("Failed to parse SSE JSON chunk: {}", e);
                }
            }
        }
    }

    Ok(chunks)
}

/// Parses Anthropic API error response for a more informative message.
fn parse_api_error(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrorWrapper {
        error: ErrorBody,
    }

    #[derive(Deserialize)]
    struct ErrorBody {
        message: String,
    }

    serde_json::from_str::<ErrorWrapper>(body)
        .ok()
        .map(|e| e.error.message)
}

// --------------------------------------------------------------------------
// Anthropic API types
// --------------------------------------------------------------------------

/// Anthropic Messages API request body.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

/// Anthropic message format.
#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

/// Anthropic Messages API response.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    usage: AnthropicUsage,
}

/// Anthropic content block.
#[derive(Debug, Deserialize)]
struct AnthropicContent {
    text: String,
}

/// Anthropic token usage.
#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: usize,
    output_tokens: usize,
}

/// Anthropic streaming event types.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart,
    #[serde(rename = "content_block_start")]
    ContentBlockStart,
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        delta: AnthropicDelta,
        #[allow(dead_code)]
        index: usize,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop,
    #[serde(rename = "message_delta")]
    MessageDelta,
    #[serde(rename = "message_stop")]
    MessageStop,
}

/// Anthropic streaming delta.
#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    _delta_type: String,
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Role};

    #[test]
    fn build_request_body_basic() {
        let request = CompletionRequest {
            model: "claude-3-opus-20240229".to_string(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: "You are a helpful assistant.".to_string(),
                },
                Message {
                    role: Role::User,
                    content: "Hello!".to_string(),
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(100),
        };

        let body = AnthropicProvider::build_request_body(&request, false);

        assert_eq!(body.model, "claude-3-opus-20240229");
        assert_eq!(body.messages.len(), 1); // Only user message
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(body.messages[0].content, "Hello!");
        assert_eq!(body.system, Some("You are a helpful assistant.".to_string()));
        assert_eq!(body.temperature, Some(0.7));
        assert_eq!(body.max_tokens, 100);
        assert!(body.stream.is_none());
    }

    #[test]
    fn build_request_body_no_system_message() {
        let request = CompletionRequest {
            model: "claude-3-sonnet-20240229".to_string(),
            messages: vec![
                Message {
                    role: Role::User,
                    content: "Hello!".to_string(),
                },
                Message {
                    role: Role::Assistant,
                    content: "Hi there!".to_string(),
                },
            ],
            temperature: None,
            max_tokens: None,
        };

        let body = AnthropicProvider::build_request_body(&request, false);

        assert_eq!(body.messages.len(), 2);
        assert!(body.system.is_none());
        assert_eq!(body.max_tokens, 4096); // Default when not specified
    }

    #[test]
    fn build_request_body_streaming() {
        let request = CompletionRequest {
            model: "claude-3-opus-20240229".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: "Test".to_string(),
            }],
            temperature: None,
            max_tokens: Some(500),
        };

        let body = AnthropicProvider::build_request_body(&request, true);

        assert_eq!(body.stream, Some(true));
        assert_eq!(body.max_tokens, 500);
    }

    #[test]
    fn parse_response_extracts_content() {
        let response = AnthropicResponse {
            content: vec![AnthropicContent {
                text: "Hello, how can I help?".to_string(),
            }],
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };

        let result = AnthropicProvider::parse_response(response).unwrap();

        assert_eq!(result.content, "Hello, how can I help?");
        assert_eq!(result.usage.prompt_tokens, 10);
        assert_eq!(result.usage.completion_tokens, 5);
        assert_eq!(result.usage.total_tokens, 15);
    }

    #[test]
    fn parse_response_handles_empty_content() {
        let response = AnthropicResponse {
            content: vec![],
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 0,
            },
        };

        let result = AnthropicProvider::parse_response(response).unwrap();

        assert_eq!(result.content, "");
    }

    #[test]
    fn parse_sse_chunks_content_block_delta() {
        let text = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}
data: {"type":"message_stop"}
"#;

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].delta, "Hello");
        assert_eq!(chunks[1].delta, " world");
    }

    #[test]
    fn parse_sse_chunks_handles_message_stop() {
        let text = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Test"}}
data: {"type":"message_stop"}
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"After stop"}}
"#;

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].delta, "Test");
    }

    #[test]
    fn parse_sse_chunks_skips_empty_text() {
        let text = r#"data: {"type":"message_start"}
data: {"type":"content_block_start"}
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Valid"}}
"#;

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].delta, "Valid");
    }

    #[test]
    fn parse_api_error_extracts_message() {
        let body = r#"{"error":{"message":"Invalid API key","type":"authentication_error"}}"#;

        let message = parse_api_error(body);

        assert_eq!(message, Some("Invalid API key".to_string()));
    }

    #[test]
    fn parse_api_error_returns_none_for_invalid_json() {
        let body = "not valid json";

        let message = parse_api_error(body);

        assert!(message.is_none());
    }

    #[tokio::test]
    async fn count_tokens_approximation() {
        use crate::config::{ProviderConfig, ProviderType};
        use crate::secret::EnvSecretProvider;

        // Set up test environment
        std::env::set_var("ANTHROPIC_API_KEY", "test-key");

        let config = ProviderConfig {
            id: "test".to_string(),
            provider_type: ProviderType::Anthropic,
            api_key_env: None,
            base_url: None,
            requests_per_minute: None,
            tokens_per_minute: None,
        };

        let secrets = EnvSecretProvider::new();
        let provider = AnthropicProvider::new(&config, &secrets).await.unwrap();

        // Test approximate counting
        let tokens = provider.count_tokens("Hello, world!").await.unwrap();

        // "Hello, world!" is 13 characters, so ~4 tokens (rounded up)
        assert_eq!(tokens, 4);

        // Longer text
        let tokens = provider.count_tokens("This is a longer sentence with many words").await.unwrap();
        // 42 characters / 4 = 10.5, rounded up to 11
        assert_eq!(tokens, 11);

        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn request_body_serialises_correctly() {
        let request = CompletionRequest {
            model: "claude-3-opus-20240229".to_string(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: "Be helpful.".to_string(),
                },
                Message {
                    role: Role::User,
                    content: "Hi".to_string(),
                },
            ],
            temperature: Some(0.5),
            max_tokens: Some(50),
        };

        let body = AnthropicProvider::build_request_body(&request, false);
        let json = serde_json::to_string(&body).unwrap();

        // Verify it contains expected fields
        assert!(json.contains("\"model\":\"claude-3-opus-20240229\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"Hi\""));
        assert!(json.contains("\"system\":\"Be helpful.\""));
        assert!(json.contains("\"temperature\":0.5"));
        assert!(json.contains("\"max_tokens\":50"));
        // stream should not be present when false/None
        assert!(!json.contains("\"stream\""));
    }

    #[test]
    fn request_body_includes_stream_when_true() {
        let request = CompletionRequest {
            model: "claude-3-opus-20240229".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: "Hi".to_string(),
            }],
            temperature: None,
            max_tokens: None,
        };

        let body = AnthropicProvider::build_request_body(&request, true);
        let json = serde_json::to_string(&body).unwrap();

        assert!(json.contains("\"stream\":true"));
    }
}