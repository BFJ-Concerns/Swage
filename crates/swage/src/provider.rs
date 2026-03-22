//! LLM provider abstraction.
//!
//! Defines the `LlmProvider` trait for interacting with language model services.
//! Implementations handle model-specific details like authentication, request
//! formatting, and response parsing.
//!
//! # Provider Responsibilities
//!
//! - **Authentication**: Manage API keys and credentials
//! - **Request translation**: Convert generic completion requests to provider-specific formats
//! - **Response parsing**: Extract structured data from provider responses
//! - **Error mapping**: Convert provider errors to `ProviderError` variants
//! - **Token counting**: Provide accurate or estimated token counts for billing/limits
//!
//! # Examples
//!
//! ```rust,ignore
//! use swage::provider::{LlmProvider, CompletionRequest, Message, Role};
//!
//! async fn generate_text<P: LlmProvider>(provider: &P) -> Result<String, ProviderError> {
//!     let request = CompletionRequest {
//!         model: "gpt-4".to_string(),
//!         messages: vec![
//!             Message {
//!                 role: Role::User,
//!                 content: "Explain async/await in Rust".to_string(),
//!             }
//!         ],
//!         temperature: Some(0.7),
//!         max_tokens: Some(500),
//!     };
//!
//!     let response = provider.complete(&request).await?;
//!     Ok(response.content)
//! }
//! ```

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::error::ProviderError;

// Re-export message types for backwards compatibility
pub use crate::message::{Message, Role};

/// Async trait for LLM provider implementations.
///
/// Provides both blocking (complete) and streaming interfaces for text generation,
/// along with token counting for rate limiting and cost estimation.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent access across async tasks.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Sends a completion request and waits for the full response.
    ///
    /// This method blocks until the entire response is generated. For long-running
    /// requests, consider using `stream()` to receive incremental updates.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` if:
    /// - Authentication fails (`ProviderError::Auth`)
    /// - Rate limits are exceeded (`ProviderError::RateLimit`)
    /// - Request format is invalid (`ProviderError::InvalidRequest`)
    /// - Model is unavailable (`ProviderError::ModelError`)
    /// - Network errors occur (`ProviderError::Network`)
    /// - Request times out (`ProviderError::Timeout`)
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, ProviderError>;

    /// Sends a completion request and returns a stream of incremental chunks.
    ///
    /// Useful for providing real-time feedback to users or implementing custom
    /// processing pipelines. The stream emits text deltas as they're generated.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` immediately if the request cannot be initiated.
    /// Individual stream items may also contain errors if the connection is interrupted
    /// or the provider returns an error mid-stream.
    ///
    /// # Cancellation
    ///
    /// Dropping the stream cancels the request. Providers should clean up resources
    /// when the stream is dropped.
    async fn stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>, ProviderError>;

    /// Estimates the token count for the given text.
    ///
    /// Used for:
    /// - Validating input doesn't exceed model limits
    /// - Estimating API costs before sending requests
    /// - Tracking token usage for rate limiting
    ///
    /// # Accuracy
    ///
    /// Token counting varies by model and provider. Implementations should:
    /// - Use the provider's official tokenizer when available
    /// - Fall back to conservative estimates (slightly over-counting) if exact counting isn't feasible
    /// - Document the tokenization method used
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidRequest` if the text contains invalid encoding
    /// or cannot be tokenized.
    async fn count_tokens(&self, text: &str) -> Result<usize, ProviderError>;
}

/// Request parameters for text completion.
///
/// Encapsulates all inputs needed to generate a completion, including the conversation
/// history, model selection, and sampling parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// The model identifier (e.g., "gpt-4", "claude-3-opus-20240229").
    pub model: String,

    /// The conversation history to use as context.
    pub messages: Vec<Message>,

    /// Sampling temperature (0.0 to 2.0).
    ///
    /// Lower values make output more deterministic and focused.
    /// Higher values increase randomness and creativity.
    /// Defaults to the provider's default (typically 1.0) if `None`.
    pub temperature: Option<f32>,

    /// Maximum tokens to generate in the response.
    ///
    /// If `None`, uses the provider's default or model's maximum.
    /// Note: This is a limit, not a target. The model may generate fewer tokens.
    pub max_tokens: Option<usize>,
}

/// Token usage statistics for a completion.
///
/// Tracks input and output tokens for billing and rate limiting purposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    /// Tokens in the input (prompt + conversation history).
    pub prompt_tokens: usize,

    /// Tokens in the generated completion.
    pub completion_tokens: usize,

    /// Total tokens (prompt + completion).
    pub total_tokens: usize,
}

/// Complete response from a text completion request.
///
/// Contains the generated text and associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// The generated text content.
    pub content: String,

    /// Token usage statistics for this completion.
    pub usage: TokenUsage,
}

/// A single chunk in a streaming completion.
///
/// Represents an incremental update containing a fragment of the generated text.
/// Consumers concatenate deltas from all chunks to build the complete response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    /// The incremental text delta for this chunk.
    ///
    /// Append this to previous deltas to build the full response.
    /// May be empty for chunks that only contain metadata.
    pub delta: String,
}
