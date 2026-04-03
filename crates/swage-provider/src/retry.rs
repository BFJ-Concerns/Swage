//! Retry decorator for LLM providers.
//!
//! Wraps any `LlmProvider` with automatic retry using exponential backoff
//! and jitter. Only transient errors trigger retries — permanent errors pass
//! through immediately.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};

use crate::provider::LlmProvider;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Retry configuration for `RetryProvider`.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (not counting the initial attempt).
    pub max_retries: u32,
    /// Base delay before the first retry; doubles with each subsequent attempt.
    pub initial_backoff: Duration,
    /// Upper bound on the computed backoff delay before jitter is applied.
    pub max_backoff: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
        }
    }
}

// ---------------------------------------------------------------------------
// RetryProvider
// ---------------------------------------------------------------------------

/// `LlmProvider` decorator that retries transient failures with exponential
/// backoff and full jitter.
///
/// Permanent errors (`Auth`, `InvalidConfig`, etc.) are returned immediately
/// without retrying. `RateLimited` errors honour the provider's `retry_after`
/// hint — the decorator always waits at least that long before retrying.
///
/// For streaming: if the inner stream contains a `StreamChunk::Error`, the
/// entire request is retried from the beginning. Chunks from failed attempts
/// are discarded; the caller only sees chunks from the successful run. This
/// means the first bytes are delayed by retry latency when the initial attempt
/// fails, but the caller is never exposed to partial-then-interrupted output.
pub struct RetryProvider {
    inner: Arc<dyn LlmProvider>,
    config: RetryConfig,
}

impl RetryProvider {
    /// Wrap a provider with the given retry configuration.
    ///
    /// Accepts any concrete `LlmProvider` implementation. The provider is
    /// type-erased internally so `RetryProvider` can be stored as
    /// `Arc<dyn LlmProvider>` or placed inside other decorators without
    /// propagating the concrete type through the call chain.
    pub fn new(inner: impl LlmProvider + 'static, config: RetryConfig) -> Self {
        Self {
            inner: Arc::new(inner),
            config,
        }
    }

    /// Wrap a provider with the default retry configuration (3 retries, 1 s
    /// initial backoff, 60 s max backoff).
    pub fn with_defaults(inner: impl LlmProvider + 'static) -> Self {
        Self::new(inner, RetryConfig::default())
    }

    /// Wrap an already-boxed, type-erased provider.
    ///
    /// Used by the chain builder when composing decorators (e.g.
    /// `CircuitBreakerProvider` → `RetryProvider`) without exposing concrete
    /// type parameters through the entire stack.
    pub fn new_erased(inner: Box<dyn LlmProvider>, config: RetryConfig) -> Self {
        Self {
            inner: Arc::from(inner),
            config,
        }
    }

    /// Shared backoff core: `initial × 2^attempt`, capped at `max_backoff`, with
    /// full jitter applied.
    ///
    /// Returns a value in `[0, min(initial × 2^attempt, max_backoff)]`.
    fn jittered_backoff(config: &RetryConfig, attempt: u32) -> Duration {
        // Exponential window: initial * 2^attempt, capped at max_backoff.
        let base = config
            .initial_backoff
            .saturating_mul(2u32.saturating_pow(attempt));
        let capped = base.min(config.max_backoff);

        // Full jitter: pick a uniform random fraction of the window.
        let jitter: f64 = rand::random();
        capped.mul_f64(jitter)
    }

    /// Compute the sleep duration for the given attempt number.
    ///
    /// Uses full-jitter exponential backoff:
    ///   `delay = uniform(0, min(initial * 2^attempt, max_backoff))`
    ///
    /// The result is floored at `retry_after` extracted from a `RateLimited`
    /// error, ensuring the provider's hint is always respected.
    fn compute_backoff(&self, attempt: u32, error: &ProviderError) -> Duration {
        let retry_after = if let ProviderError::RateLimited { retry_after, .. } = error {
            *retry_after
        } else {
            None
        };

        // Honour the provider's retry-after hint — always wait at least that long.
        Self::jittered_backoff(&self.config, attempt).max(retry_after.unwrap_or(Duration::ZERO))
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for RetryProvider {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    /// Send a completion request, retrying on transient failures.
    ///
    /// Token usage from failed attempts is not accumulated — only the
    /// successful response's usage is returned to the caller.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let mut attempt = 0u32;
        loop {
            match self.inner.complete(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(err) if err.is_retriable() && attempt < self.config.max_retries => {
                    let delay = self.compute_backoff(attempt, &err);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                // Non-retriable error or retries exhausted — pass through.
                Err(err) => return Err(err),
            }
        }
    }

    /// Send a tool-completion request, retrying on transient failures.
    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        let mut attempt = 0u32;
        loop {
            match self.inner.complete_with_tools(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(err) if err.is_retriable() && attempt < self.config.max_retries => {
                    let delay = self.compute_backoff(attempt, &err);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Start a streaming completion, retrying the entire request on stream errors.
    ///
    /// The inner stream is buffered internally. If a `StreamChunk::Error`
    /// appears before `Done`, the attempt is discarded and the request is
    /// retried from scratch. Once an attempt completes without errors, all
    /// its chunks are yielded to the caller in order.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let inner = Arc::clone(&self.inner);
        let config = self.config.clone();

        Box::pin(
            futures::stream::once(async move {
                let mut attempt = 0u32;
                loop {
                    let chunks: Vec<StreamChunk> = inner.stream(request.clone()).collect().await;

                    // Scan for an error chunk in this attempt.
                    let error_msg = chunks.iter().find_map(|c| {
                        if let StreamChunk::Error { message } = c {
                            Some(message.clone())
                        } else {
                            None
                        }
                    });

                    match error_msg {
                        // No error — stream completed successfully.
                        None => return chunks,
                        // Error, retries exhausted — propagate a single error chunk.
                        Some(msg) if attempt >= config.max_retries => {
                            return vec![StreamChunk::Error { message: msg }];
                        }
                        // Transient stream error — back off and retry from scratch.
                        Some(_) => {
                            let delay = RetryProvider::jittered_backoff(&config, attempt);
                            tokio::time::sleep(delay).await;
                            attempt += 1;
                        }
                    }
                }
            })
            .flat_map(futures::stream::iter),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures::StreamExt;
    use swage_core::FinishReason;

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// A retry config with zero backoff durations so tests don't sleep.
    fn zero_backoff() -> RetryConfig {
        RetryConfig {
            max_retries: 3,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        }
    }

    fn completion_req() -> CompletionRequest {
        CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
        }
    }

    fn tool_req() -> ToolCompletionRequest {
        ToolCompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        }
    }

    fn ok_completion() -> CompletionResponse {
        CompletionResponse {
            content: "success".into(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cost: None,
        }
    }

    // -----------------------------------------------------------------------
    // FailN — mock provider that fails exactly N times then succeeds.
    // -----------------------------------------------------------------------

    struct FailN {
        /// Total calls made across all methods.
        call_count: Arc<Mutex<u32>>,
        fail_count: u32,
        error: ProviderError,
    }

    impl FailN {
        fn new(fail_count: u32, error: ProviderError) -> Self {
            Self {
                call_count: Arc::new(Mutex::new(0)),
                fail_count,
                error,
            }
        }

        fn total_calls(&self) -> u32 {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmProvider for FailN {
        fn model_id(&self) -> &str {
            "mock"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            let mut n = self.call_count.lock().unwrap();
            *n += 1;
            if *n <= self.fail_count {
                Err(self.error.clone())
            } else {
                Ok(ok_completion())
            }
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            let mut n = self.call_count.lock().unwrap();
            *n += 1;
            if *n <= self.fail_count {
                Err(self.error.clone())
            } else {
                Ok(ToolCompletionResponse {
                    content: "success".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                    finish_reason: FinishReason::Stop,
                    cost: None,
                    tool_calls: vec![],
                })
            }
        }

        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            let mut n = self.call_count.lock().unwrap();
            *n += 1;
            if *n <= self.fail_count {
                Box::pin(futures::stream::iter(vec![StreamChunk::Error {
                    message: "transient stream error".into(),
                }]))
            } else {
                Box::pin(futures::stream::iter(vec![
                    StreamChunk::Delta {
                        content: "hello".into(),
                    },
                    StreamChunk::Done {
                        finish_reason: None,
                    },
                ]))
            }
        }
    }

    // -----------------------------------------------------------------------
    // AlwaysFail — mock that never succeeds, used to test exhaustion paths.
    // -----------------------------------------------------------------------

    struct AlwaysFail {
        call_count: Arc<Mutex<u32>>,
        error: ProviderError,
    }

    impl AlwaysFail {
        fn new(error: ProviderError) -> Self {
            Self {
                call_count: Arc::new(Mutex::new(0)),
                error,
            }
        }

        fn total_calls(&self) -> u32 {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmProvider for AlwaysFail {
        fn model_id(&self) -> &str {
            "mock"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            *self.call_count.lock().unwrap() += 1;
            Err(self.error.clone())
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            *self.call_count.lock().unwrap() += 1;
            Err(self.error.clone())
        }

        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            *self.call_count.lock().unwrap() += 1;
            Box::pin(futures::stream::iter(vec![StreamChunk::Error {
                message: "persistent error".into(),
            }]))
        }
    }

    // -----------------------------------------------------------------------
    // RetryConfig tests
    // -----------------------------------------------------------------------

    #[test]
    fn retry_config_default_values() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_backoff, Duration::from_secs(1));
        assert_eq!(config.max_backoff, Duration::from_secs(60));
    }

    // -----------------------------------------------------------------------
    // complete() — success and retry paths
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn complete_succeeds_on_first_attempt() {
        let retry = RetryProvider::with_defaults(FailN::new(
            0,
            ProviderError::Network {
                provider: "mock".into(),
                message: "err".into(),
            },
        ));
        let result = retry.complete(completion_req()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "success");
    }

    #[tokio::test]
    async fn complete_retries_twice_and_succeeds() {
        // Fails on calls 1 and 2, succeeds on call 3 — caller gets valid response.
        let provider = FailN::new(
            2,
            ProviderError::Network {
                provider: "mock".into(),
                message: "timeout".into(),
            },
        );
        let retry = RetryProvider::new(provider, zero_backoff());
        let result = retry.complete(completion_req()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "success");
    }

    #[tokio::test]
    async fn complete_permanent_error_returned_without_retry() {
        // Auth errors are permanent — the decorator must not retry.
        let inner = AlwaysFail::new(ProviderError::Auth {
            provider: "mock".into(),
        });
        // Borrow the call_count Arc before moving `inner` into RetryProvider.
        let call_count = Arc::clone(&inner.call_count);
        let retry = RetryProvider::new(inner, zero_backoff());

        let result = retry.complete(completion_req()).await;

        assert!(matches!(result.unwrap_err(), ProviderError::Auth { .. }));
        // Exactly one call — no retries for permanent errors.
        assert_eq!(*call_count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn complete_exhausts_retries_and_returns_final_error() {
        // 4 total calls (initial + 3 retries) all fail — final error is returned.
        let retry = RetryProvider::new(
            AlwaysFail::new(ProviderError::Network {
                provider: "mock".into(),
                message: "persistent".into(),
            }),
            zero_backoff(),
        );
        let result = retry.complete(completion_req()).await;
        assert!(matches!(result.unwrap_err(), ProviderError::Network { .. }));
    }

    // -----------------------------------------------------------------------
    // retry_after — RateLimited hint must be honoured
    // -----------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn complete_waits_at_least_retry_after_on_rate_limit() {
        // Zero initial_backoff so jitter contributes nothing; only retry_after remains.
        let retry_after_hint = Duration::from_secs(5);

        let retry = RetryProvider::new(
            FailN::new(
                1,
                ProviderError::RateLimited {
                    provider: "mock".into(),
                    retry_after: Some(retry_after_hint),
                },
            ),
            RetryConfig {
                max_retries: 3,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::ZERO,
            },
        );

        let start = tokio::time::Instant::now();
        let result = retry.complete(completion_req()).await;
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "expected success after retry");
        // With zero base backoff, the only delay is the retry_after floor.
        assert!(
            elapsed >= retry_after_hint,
            "expected delay >= retry_after ({retry_after_hint:?}), got {elapsed:?}"
        );
    }

    // -----------------------------------------------------------------------
    // complete_with_tools() tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn complete_with_tools_retries_and_succeeds() {
        let retry = RetryProvider::new(
            FailN::new(
                2,
                ProviderError::Timeout {
                    provider: "mock".into(),
                    duration: Duration::from_secs(30),
                },
            ),
            zero_backoff(),
        );
        let result = retry.complete_with_tools(tool_req()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "success");
    }

    #[tokio::test]
    async fn complete_with_tools_permanent_error_no_retry() {
        let inner = AlwaysFail::new(ProviderError::InvalidConfig {
            message: "bad config".into(),
        });
        let call_count = Arc::clone(&inner.call_count);
        let retry = RetryProvider::new(inner, zero_backoff());

        let result = retry.complete_with_tools(tool_req()).await;

        assert!(matches!(
            result.unwrap_err(),
            ProviderError::InvalidConfig { .. }
        ));
        assert_eq!(*call_count.lock().unwrap(), 1);
    }

    // -----------------------------------------------------------------------
    // stream() tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stream_succeeds_on_first_attempt() {
        let retry = RetryProvider::with_defaults(FailN::new(
            0,
            ProviderError::StreamError("unused".into()),
        ));
        let chunks: Vec<StreamChunk> = retry.stream(completion_req()).collect().await;

        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, StreamChunk::Delta { .. }))
        );
        assert!(matches!(chunks.last(), Some(StreamChunk::Done { .. })));
    }

    #[tokio::test]
    async fn stream_retries_twice_and_returns_successful_chunks() {
        // First two stream attempts return an error chunk; third succeeds.
        // Caller must see only the successful attempt's chunks.
        let retry = RetryProvider::new(
            FailN::new(2, ProviderError::StreamError("transient".into())),
            zero_backoff(),
        );
        let chunks: Vec<StreamChunk> = retry.stream(completion_req()).collect().await;

        assert!(
            chunks
                .iter()
                .all(|c| !matches!(c, StreamChunk::Error { .. })),
            "no error chunks expected: {chunks:?}"
        );
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, StreamChunk::Delta { content } if content == "hello"))
        );
        assert!(matches!(chunks.last(), Some(StreamChunk::Done { .. })));
    }

    #[tokio::test]
    async fn stream_returns_error_chunk_after_retries_exhausted() {
        // All attempts fail — after max_retries, an error chunk is emitted.
        let retry = RetryProvider::new(
            AlwaysFail::new(ProviderError::StreamError("persistent".into())),
            zero_backoff(),
        );
        let chunks: Vec<StreamChunk> = retry.stream(completion_req()).collect().await;

        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, StreamChunk::Error { .. })),
            "expected an error chunk when retries are exhausted"
        );
    }
}
