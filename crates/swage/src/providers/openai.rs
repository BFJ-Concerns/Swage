//! OpenAI LLM provider implementation.
//!
//! Supports the OpenAI Chat Completions API with streaming,
//! token counting via tiktoken-rs, and custom base URLs for
//! Azure, proxies, and local endpoints.

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

/// Default base URL for OpenAI API.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Default environment variable for OpenAI API key.
const DEFAULT_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// OpenAI LLM provider implementation.
///
/// Supports the OpenAI Chat Completions API with streaming,
/// token counting via tiktoken-rs, and custom base URLs for
/// Azure, proxies, and local endpoints.
pub struct OpenAiProvider {
    client: HttpClient,
    base_url: String,
    api_key: String,
    rate_limiter: RateLimiter,
}

impl OpenAiProvider {
    /// Creates a new OpenAI provider with the given configuration.
    ///
    /// Resolves the API key from the secret provider using the configured
    /// environment variable name (or `OPENAI_API_KEY` if not specified).
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

    /// Builds the OpenAI API request body from a completion request.
    fn build_request_body(request: &CompletionRequest, stream: bool) -> OpenAiRequest {
        let messages = request
            .messages
            .iter()
            .map(|m| OpenAiMessage {
                role: match m.role {
                    crate::message::Role::System => "system".to_string(),
                    crate::message::Role::User => "user".to_string(),
                    crate::message::Role::Assistant => "assistant".to_string(),
                },
                content: m.content.clone(),
            })
            .collect();

        OpenAiRequest {
            model: request.model.clone(),
            messages,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            stream: if stream { Some(true) } else { None },
        }
    }

    /// Parses the OpenAI API response into a CompletionResponse.
    fn parse_response(response: OpenAiResponse) -> Result<CompletionResponse, ProviderError> {
        // Extract content from first choice
        let content = response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        Ok(CompletionResponse {
            content,
            usage: TokenUsage {
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: response.usage.completion_tokens,
                total_tokens: response.usage.total_tokens,
            },
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        // Estimate tokens for rate limiting (rough approximation).
        // Serialisation failure here is acceptable: the request will be serialised again
        // for the actual HTTP call where errors will surface. Use conservative fallback
        // (1000 tokens) to avoid underestimating and hitting provider rate limits.
        let estimated_tokens = self.count_tokens(&serde_json::to_string(request).unwrap_or_default()).await.unwrap_or(1000);

        // Wait for rate limiter
        self.rate_limiter.acquire(estimated_tokens).await;

        let url = format!("{}/chat/completions", self.base_url);
        let body = Self::build_request_body(request, false);

        let response = self
            .client
            .client()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
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

        let openai_response: OpenAiResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::InvalidRequest(format!("failed to parse response: {}", e)))?;

        Self::parse_response(openai_response)
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

        let url = format!("{}/chat/completions", self.base_url);
        let body = Self::build_request_body(request, true);

        let response = self
            .client
            .client()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
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
        // Use cl100k_base encoding (GPT-4 / GPT-3.5-turbo tokenizer)
        let bpe = tiktoken_rs::cl100k_base()
            .map_err(|e| ProviderError::InvalidRequest(format!("failed to load tokenizer: {}", e)))?;

        let tokens = bpe.encode_with_special_tokens(text);
        Ok(tokens.len())
    }
}

/// Parses SSE chunks from a text buffer into stream chunks.
fn parse_sse_chunks(text: &str) -> Result<Vec<StreamChunk>, ProviderError> {
    let mut chunks = Vec::new();

    for line in text.lines() {
        if let Some(data) = parse_sse_line(line) {
            // Check for [DONE] marker
            if data == "[DONE]" {
                break;
            }

            // Parse JSON chunk
            match serde_json::from_str::<OpenAiStreamChunk>(data) {
                Ok(chunk) => {
                    // Extract delta content from first choice
                    if let Some(choice) = chunk.choices.into_iter().next() {
                        if let Some(content) = choice.delta.content {
                            if !content.is_empty() {
                                chunks.push(StreamChunk { delta: content });
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("Failed to parse SSE JSON chunk: {}", e);
                }
            }
        }
    }

    Ok(chunks)
}

/// Parses OpenAI API error response for a more informative message.
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
// OpenAI API types
// --------------------------------------------------------------------------

/// OpenAI Chat Completions API request body.
#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

/// OpenAI message format.
#[derive(Debug, Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

/// OpenAI Chat Completions API response.
#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage: OpenAiUsage,
}

/// OpenAI response choice.
#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

/// OpenAI token usage.
#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

/// OpenAI streaming response chunk.
#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    choices: Vec<OpenAiStreamChoice>,
}

/// OpenAI streaming choice.
#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiDelta,
}

/// OpenAI streaming delta.
#[derive(Debug, Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Role};

    #[test]
    fn build_request_body_basic() {
        let request = CompletionRequest {
            model: "gpt-4".to_string(),
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

        let body = OpenAiProvider::build_request_body(&request, false);

        assert_eq!(body.model, "gpt-4");
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[0].content, "You are a helpful assistant.");
        assert_eq!(body.messages[1].role, "user");
        assert_eq!(body.messages[1].content, "Hello!");
        assert_eq!(body.temperature, Some(0.7));
        assert_eq!(body.max_tokens, Some(100));
        assert!(body.stream.is_none());
    }

    #[test]
    fn build_request_body_streaming() {
        let request = CompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: "Test".to_string(),
            }],
            temperature: None,
            max_tokens: None,
        };

        let body = OpenAiProvider::build_request_body(&request, true);

        assert_eq!(body.stream, Some(true));
        assert!(body.temperature.is_none());
        assert!(body.max_tokens.is_none());
    }

    #[test]
    fn parse_response_extracts_content() {
        let response = OpenAiResponse {
            choices: vec![OpenAiChoice {
                message: OpenAiMessage {
                    role: "assistant".to_string(),
                    content: "Hello, how can I help?".to_string(),
                },
            }],
            usage: OpenAiUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };

        let result = OpenAiProvider::parse_response(response).unwrap();

        assert_eq!(result.content, "Hello, how can I help?");
        assert_eq!(result.usage.prompt_tokens, 10);
        assert_eq!(result.usage.completion_tokens, 5);
        assert_eq!(result.usage.total_tokens, 15);
    }

    #[test]
    fn parse_response_handles_empty_choices() {
        let response = OpenAiResponse {
            choices: vec![],
            usage: OpenAiUsage {
                prompt_tokens: 10,
                completion_tokens: 0,
                total_tokens: 10,
            },
        };

        let result = OpenAiProvider::parse_response(response).unwrap();

        assert_eq!(result.content, "");
    }

    #[test]
    fn parse_sse_chunks_basic() {
        let text = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\
                   data: [DONE]\n";

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].delta, "Hello");
        assert_eq!(chunks[1].delta, " world");
    }

    #[test]
    fn parse_sse_chunks_handles_done() {
        let text = "data: {\"choices\":[{\"delta\":{\"content\":\"Test\"}}]}\n\
                   data: [DONE]\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"After done\"}}]}\n";

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].delta, "Test");
    }

    #[test]
    fn parse_sse_chunks_skips_empty_content() {
        let text = "data: {\"choices\":[{\"delta\":{}}]}\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"Valid\"}}]}\n";

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].delta, "Valid");
    }

    #[test]
    fn parse_sse_chunks_ignores_non_data_lines() {
        let text = "event: message\n\
                   id: 123\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"Test\"}}]}\n\
                   : comment\n";

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].delta, "Test");
    }

    #[test]
    fn parse_api_error_extracts_message() {
        let body = r#"{"error":{"message":"Invalid API key","type":"invalid_request_error"}}"#;

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
    async fn count_tokens_basic() {
        // Create a minimal provider for testing token counting
        // Since we can't easily construct one without secrets, we test the underlying tiktoken logic
        let bpe = tiktoken_rs::cl100k_base().unwrap();
        let tokens = bpe.encode_with_special_tokens("Hello, world!");

        // cl100k_base tokenizes "Hello, world!" into approximately 4 tokens
        assert!(tokens.len() >= 3 && tokens.len() <= 5);
    }

    #[test]
    fn request_body_serialises_correctly() {
        let request = CompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: "Hi".to_string(),
            }],
            temperature: Some(0.5),
            max_tokens: Some(50),
        };

        let body = OpenAiProvider::build_request_body(&request, false);
        let json = serde_json::to_string(&body).unwrap();

        // Verify it contains expected fields
        assert!(json.contains("\"model\":\"gpt-4\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"Hi\""));
        assert!(json.contains("\"temperature\":0.5"));
        assert!(json.contains("\"max_tokens\":50"));
        // stream should not be present when false/None
        assert!(!json.contains("\"stream\""));
    }

    #[test]
    fn request_body_includes_stream_when_true() {
        let request = CompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: "Hi".to_string(),
            }],
            temperature: None,
            max_tokens: None,
        };

        let body = OpenAiProvider::build_request_body(&request, true);
        let json = serde_json::to_string(&body).unwrap();

        assert!(json.contains("\"stream\":true"));
    }
}
