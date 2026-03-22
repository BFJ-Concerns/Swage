//! Google Gemini LLM provider implementation.
//!
//! Supports the Gemini API with streaming completions and approximate
//! token counting. System messages are prepended to the first user message.

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

/// Default base URL for Google Gemini API.
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Default environment variable for Google API key.
const DEFAULT_API_KEY_ENV: &str = "GOOGLE_API_KEY";

/// Google Gemini LLM provider implementation.
///
/// Supports the Gemini API with streaming completions and approximate
/// token counting. Unlike OpenAI and Anthropic, Google uses "model" role
/// instead of "assistant", and the API key is passed as a query parameter.
pub struct GoogleProvider {
    client: HttpClient,
    base_url: String,
    api_key: String,
    rate_limiter: RateLimiter,
}

impl GoogleProvider {
    /// Creates a new Google provider with the given configuration.
    ///
    /// Resolves the API key from the secret provider using the configured
    /// environment variable name (or `GOOGLE_API_KEY` if not specified).
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

    /// Builds the Gemini API request body from a completion request.
    ///
    /// Extracts system messages and prepends them to the first user message,
    /// as Gemini doesn't have a separate system message field.
    fn build_request_body(request: &CompletionRequest) -> GoogleRequest {
        // Extract system message content (concatenate all system messages)
        let system_content: Vec<String> = request
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::message::Role::System))
            .map(|m| m.content.clone())
            .collect();

        let system_prefix = if !system_content.is_empty() {
            Some(system_content.join("\n\n"))
        } else {
            None
        };

        // Build contents array with only user and assistant messages
        let mut contents: Vec<GoogleContent> = Vec::new();
        let mut first_user_message = true;

        for msg in &request.messages {
            match msg.role {
                crate::message::Role::System => {
                    // Already handled above, skip
                }
                crate::message::Role::User => {
                    let content = if first_user_message {
                        if let Some(prefix) = &system_prefix {
                            // Prepend system message to first user message
                            first_user_message = false;
                            format!("{}\n\n{}", prefix, msg.content)
                        } else {
                            msg.content.clone()
                        }
                    } else {
                        msg.content.clone()
                    };

                    contents.push(GoogleContent {
                        role: "user".to_string(),
                        parts: vec![GooglePart { text: content }],
                    });
                }
                crate::message::Role::Assistant => {
                    contents.push(GoogleContent {
                        role: "model".to_string(),
                        parts: vec![GooglePart {
                            text: msg.content.clone(),
                        }],
                    });
                }
            }
        }

        let generation_config = GoogleGenerationConfig {
            temperature: request.temperature,
            max_output_tokens: request.max_tokens,
        };

        GoogleRequest {
            contents,
            generation_config,
        }
    }

    /// Parses the Gemini API response into a CompletionResponse.
    fn parse_response(response: GoogleResponse) -> Result<CompletionResponse, ProviderError> {
        // Extract content from first candidate's first part
        let content = response
            .candidates
            .into_iter()
            .next()
            .and_then(|c| c.content.parts.into_iter().next())
            .map(|p| p.text)
            .unwrap_or_default();

        // Google doesn't provide token usage in the response, so we approximate
        let prompt_tokens = 0;
        let completion_tokens = (content.chars().count() as f64 / 4.0).ceil() as usize;

        Ok(CompletionResponse {
            content,
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        })
    }
}

#[async_trait]
impl LlmProvider for GoogleProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        // Estimate tokens for rate limiting.
        // Serialisation failure here is acceptable: the request will be serialised again
        // for the actual HTTP call where errors will surface. Use conservative fallback
        // (1000 tokens) to avoid underestimating and hitting provider rate limits.
        let estimated_tokens = self.count_tokens(&serde_json::to_string(request).unwrap_or_default()).await.unwrap_or(1000);

        // Wait for rate limiter
        self.rate_limiter.acquire(estimated_tokens).await;

        // Google Gemini API requires the API key as a URL query parameter.
        // This is the documented authentication method for this API.
        // See: https://ai.google.dev/gemini-api/docs/api-key
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url,
            request.model,
            self.api_key
        );
        let body = Self::build_request_body(request);

        let response = self
            .client
            .client()
            .post(&url)
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

        let google_response: GoogleResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::InvalidRequest(format!("failed to parse response: {}", e)))?;

        Self::parse_response(google_response)
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

        // Google Gemini API requires the API key as a URL query parameter.
        // This is the documented authentication method for this API.
        // See: https://ai.google.dev/gemini-api/docs/api-key
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url,
            request.model,
            self.api_key
        );
        let body = Self::build_request_body(request);

        let response = self
            .client
            .client()
            .post(&url)
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
        // Google doesn't provide a public tokeniser, so we use an approximate count.
        // Heuristic: average ~4 characters per token.
        // This is a rough approximation but sufficient for rate limiting purposes.

        let char_count = text.chars().count();
        let estimated_tokens = (char_count as f64 / 4.0).ceil() as usize;

        Ok(estimated_tokens)
    }
}

/// Parses SSE chunks from a text buffer into stream chunks.
///
/// Google Gemini uses Server-Sent Events for streaming with JSON payloads
/// containing candidate deltas.
fn parse_sse_chunks(text: &str) -> Result<Vec<StreamChunk>, ProviderError> {
    let mut chunks = Vec::new();

    for line in text.lines() {
        if let Some(data) = parse_sse_line(line) {
            // Parse JSON chunk
            match serde_json::from_str::<GoogleResponse>(data) {
                Ok(response) => {
                    // Extract text from first candidate's first part
                    if let Some(candidate) = response.candidates.into_iter().next() {
                        if let Some(part) = candidate.content.parts.into_iter().next() {
                            if !part.text.is_empty() {
                                chunks.push(StreamChunk { delta: part.text });
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

/// Parses Google API error response for a more informative message.
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
// Google Gemini API types
// --------------------------------------------------------------------------

/// Google Gemini API request body.
#[derive(Debug, Serialize)]
struct GoogleRequest {
    contents: Vec<GoogleContent>,
    #[serde(rename = "generationConfig")]
    generation_config: GoogleGenerationConfig,
}

/// Google content block (analogous to a message).
#[derive(Debug, Serialize, Deserialize)]
struct GoogleContent {
    role: String,
    parts: Vec<GooglePart>,
}

/// Google content part (contains the text).
#[derive(Debug, Serialize, Deserialize)]
struct GooglePart {
    text: String,
}

/// Google generation configuration.
#[derive(Debug, Serialize)]
struct GoogleGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: Option<usize>,
}

/// Google Gemini API response.
#[derive(Debug, Deserialize)]
struct GoogleResponse {
    candidates: Vec<GoogleCandidate>,
}

/// Google response candidate.
#[derive(Debug, Deserialize)]
struct GoogleCandidate {
    content: GoogleContent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Role};

    #[test]
    fn build_request_body_basic() {
        let request = CompletionRequest {
            model: "gemini-pro".to_string(),
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

        let body = GoogleProvider::build_request_body(&request);

        // Should have 1 content entry (system prepended to user message)
        assert_eq!(body.contents.len(), 1);
        assert_eq!(body.contents[0].role, "user");
        assert!(body.contents[0].parts[0].text.contains("You are a helpful assistant."));
        assert!(body.contents[0].parts[0].text.contains("Hello!"));
        assert_eq!(body.generation_config.temperature, Some(0.7));
        assert_eq!(body.generation_config.max_output_tokens, Some(100));
    }

    #[test]
    fn build_request_body_no_system_message() {
        let request = CompletionRequest {
            model: "gemini-pro".to_string(),
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

        let body = GoogleProvider::build_request_body(&request);

        assert_eq!(body.contents.len(), 2);
        assert_eq!(body.contents[0].role, "user");
        assert_eq!(body.contents[0].parts[0].text, "Hello!");
        assert_eq!(body.contents[1].role, "model");
        assert_eq!(body.contents[1].parts[0].text, "Hi there!");
        assert!(body.generation_config.temperature.is_none());
        assert!(body.generation_config.max_output_tokens.is_none());
    }

    #[test]
    fn build_request_body_role_mapping() {
        let request = CompletionRequest {
            model: "gemini-pro".to_string(),
            messages: vec![
                Message {
                    role: Role::User,
                    content: "Question".to_string(),
                },
                Message {
                    role: Role::Assistant,
                    content: "Answer".to_string(),
                },
                Message {
                    role: Role::User,
                    content: "Follow-up".to_string(),
                },
            ],
            temperature: None,
            max_tokens: None,
        };

        let body = GoogleProvider::build_request_body(&request);

        assert_eq!(body.contents.len(), 3);
        assert_eq!(body.contents[0].role, "user");
        assert_eq!(body.contents[1].role, "model"); // Assistant → model
        assert_eq!(body.contents[2].role, "user");
    }

    #[test]
    fn build_request_body_multiple_system_messages() {
        let request = CompletionRequest {
            model: "gemini-pro".to_string(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: "First instruction.".to_string(),
                },
                Message {
                    role: Role::System,
                    content: "Second instruction.".to_string(),
                },
                Message {
                    role: Role::User,
                    content: "Hello!".to_string(),
                },
            ],
            temperature: None,
            max_tokens: None,
        };

        let body = GoogleProvider::build_request_body(&request);

        assert_eq!(body.contents.len(), 1);
        assert_eq!(body.contents[0].role, "user");
        // Both system messages should be prepended to the user message
        assert!(body.contents[0].parts[0].text.contains("First instruction."));
        assert!(body.contents[0].parts[0].text.contains("Second instruction."));
        assert!(body.contents[0].parts[0].text.contains("Hello!"));
    }

    #[test]
    fn parse_response_extracts_content() {
        let response = GoogleResponse {
            candidates: vec![GoogleCandidate {
                content: GoogleContent {
                    role: "model".to_string(),
                    parts: vec![GooglePart {
                        text: "Hello, how can I help?".to_string(),
                    }],
                },
            }],
        };

        let result = GoogleProvider::parse_response(response).unwrap();

        assert_eq!(result.content, "Hello, how can I help?");
        // Token counts are approximated
        assert!(result.usage.completion_tokens > 0);
    }

    #[test]
    fn parse_response_handles_empty_candidates() {
        let response = GoogleResponse {
            candidates: vec![],
        };

        let result = GoogleProvider::parse_response(response).unwrap();

        assert_eq!(result.content, "");
    }

    #[test]
    fn parse_sse_chunks_basic() {
        let text = r#"data: {"candidates":[{"content":{"parts":[{"text":"Hello"}],"role":"model"}}]}
data: {"candidates":[{"content":{"parts":[{"text":" world"}],"role":"model"}}]}
"#;

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].delta, "Hello");
        assert_eq!(chunks[1].delta, " world");
    }

    #[test]
    fn parse_sse_chunks_skips_empty_text() {
        let text = r#"data: {"candidates":[{"content":{"parts":[{"text":""}],"role":"model"}}]}
data: {"candidates":[{"content":{"parts":[{"text":"Valid"}],"role":"model"}}]}
"#;

        let chunks = parse_sse_chunks(text).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].delta, "Valid");
    }

    #[test]
    fn parse_api_error_extracts_message() {
        let body = r#"{"error":{"message":"Invalid API key","code":401}}"#;

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
        std::env::set_var("GOOGLE_API_KEY", "test-key");

        let config = ProviderConfig {
            id: "test".to_string(),
            provider_type: ProviderType::Google,
            api_key_env: None,
            base_url: None,
            requests_per_minute: None,
            tokens_per_minute: None,
        };

        let secrets = EnvSecretProvider::new();
        let provider = GoogleProvider::new(&config, &secrets).await.unwrap();

        // Test approximate counting
        let tokens = provider.count_tokens("Hello, world!").await.unwrap();

        // "Hello, world!" is 13 characters, so ~4 tokens (rounded up)
        assert_eq!(tokens, 4);

        // Longer text
        let tokens = provider.count_tokens("This is a longer sentence with many words").await.unwrap();
        // 42 characters / 4 = 10.5, rounded up to 11
        assert_eq!(tokens, 11);

        std::env::remove_var("GOOGLE_API_KEY");
    }

    #[test]
    fn request_body_serialises_correctly() {
        let request = CompletionRequest {
            model: "gemini-pro".to_string(),
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

        let body = GoogleProvider::build_request_body(&request);
        let json = serde_json::to_string(&body).unwrap();

        // Verify it contains expected fields
        assert!(json.contains("\"contents\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("Be helpful."));
        assert!(json.contains("Hi"));
        assert!(json.contains("\"generationConfig\""));
        assert!(json.contains("\"temperature\":0.5"));
        assert!(json.contains("\"maxOutputTokens\":50"));
    }
}
