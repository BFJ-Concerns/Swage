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
    CompletionRequest, CompletionResponse, EffortCapability, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};

use crate::provider::{
    LlmProvider, contains_user_visible_content, may_forward_before_attempt_commits,
    spawn_stream_forwarder,
};

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
/// For streaming, errors are retried only until an inner chunk has been
/// forwarded. Once output starts, chunks are delivered incrementally and any
/// later error terminates the stream rather than replaying the request.
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

/// Classify only the envelopes written by `ProviderError`'s Display impl.
/// Streaming adapters expose rendered errors here, so only the leading status
/// token is authoritative. Provider body text must never supply a status or
/// override the envelope.
fn is_permanent_stream_error(message: &str) -> bool {
    if message.starts_with("authentication failed for provider ") {
        return true;
    }
    let Some(rest) = message.strip_prefix("HTTP error ") else {
        return false;
    };
    let Some((status, _body)) = rest.split_once(": ") else {
        return false;
    };
    if status.len() != 3 || !status.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(status) = status.parse::<u16>() else {
        return false;
    };
    (400..500).contains(&status) && !matches!(status, 408 | 409 | 429)
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for RetryProvider {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn provider_request_snapshot(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> serde_json::Value {
        self.inner.provider_request_snapshot(request, stream)
    }

    fn effort_capability(&self, model: &str) -> Option<EffortCapability> {
        self.inner.effort_capability(model)
    }

    async fn count_tokens(
        &self,
        request: &CompletionRequest,
    ) -> Result<Option<u64>, ProviderError> {
        // An advisory read: on failure the caller falls back to its own
        // estimate, so retry ceremony would only delay that fallback.
        self.inner.count_tokens(request).await
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

    /// Start a streaming completion, retrying errors that precede all output.
    ///
    /// Content and live progress chunks are forwarded as they arrive. Usage
    /// and completion are held until the attempt commits, so accounting from a
    /// discarded attempt cannot escape. After the first content chunk is
    /// forwarded, a later error is terminal and the request is not replayed.
    /// Dropping the returned stream cancels an active attempt or retry wait.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let inner = Arc::clone(&self.inner);
        let config = self.config.clone();
        let provider = inner.model_id().to_string();
        spawn_stream_forwarder(move |tx| async move {
            let mut attempt = 0u32;
            let mut active_retry: Option<u32> = None;

            loop {
                if tx.is_closed() {
                    return;
                }
                let mut stream = inner.stream(request.clone());
                let mut output_started = false;
                let mut pending_chunks = Vec::new();
                let mut retryable_error = None;

                while let Some(chunk) = tokio::select! {
                    () = tx.closed() => return,
                    chunk = stream.next() => chunk,
                } {
                    match chunk {
                        StreamChunk::Error { message } if !output_started => {
                            retryable_error = Some((message, false));
                            break;
                        }
                        StreamChunk::ProviderAnomaly { message } if !output_started => {
                            retryable_error = Some((message, true));
                            break;
                        }
                        StreamChunk::ProviderAnomaly { message } => {
                            if tx.send(StreamChunk::ProviderAnomaly { message }).is_err() {
                                return;
                            }
                            return;
                        }
                        StreamChunk::Error { message } => {
                            if tx.send(StreamChunk::Error { message }).is_err() {
                                return;
                            }
                            return;
                        }
                        chunk => {
                            if output_started {
                                if tx.send(chunk).is_err() {
                                    return;
                                }
                            } else if contains_user_visible_content(&chunk) {
                                output_started = true;
                                if let Some(retry_attempt) = active_retry.take()
                                    && tx
                                        .send(StreamChunk::ProviderRetrySettled {
                                            attempt: retry_attempt,
                                            max_retries: config.max_retries,
                                            provider: provider.clone(),
                                            success: true,
                                        })
                                        .is_err()
                                {
                                    return;
                                }
                                for pending_chunk in pending_chunks.drain(..) {
                                    if tx.send(pending_chunk).is_err() {
                                        return;
                                    }
                                }
                                if tx.send(chunk).is_err() {
                                    return;
                                }
                            } else if may_forward_before_attempt_commits(&chunk) {
                                if tx.send(chunk).is_err() {
                                    return;
                                }
                            } else {
                                pending_chunks.push(chunk);
                            }
                        }
                    }
                }

                let Some((message, is_anomaly)) = retryable_error else {
                    if let Some(retry_attempt) = active_retry.take()
                        && tx
                            .send(StreamChunk::ProviderRetrySettled {
                                attempt: retry_attempt,
                                max_retries: config.max_retries,
                                provider: provider.clone(),
                                success: true,
                            })
                            .is_err()
                    {
                        return;
                    }
                    for pending_chunk in pending_chunks {
                        if tx.send(pending_chunk).is_err() {
                            return;
                        }
                    }
                    return;
                };

                if attempt >= config.max_retries
                    || (!is_anomaly && is_permanent_stream_error(&message))
                {
                    if let Some(retry_attempt) = active_retry.take()
                        && tx
                            .send(StreamChunk::ProviderRetrySettled {
                                attempt: retry_attempt,
                                max_retries: config.max_retries,
                                provider: provider.clone(),
                                success: false,
                            })
                            .is_err()
                    {
                        return;
                    }
                    let terminal = if is_anomaly {
                        StreamChunk::ProviderAnomaly { message }
                    } else {
                        StreamChunk::Error { message }
                    };
                    if tx.send(terminal).is_err() {
                        return;
                    }
                    return;
                }

                attempt += 1;
                active_retry = Some(attempt);
                let delay = RetryProvider::jittered_backoff(&config, attempt - 1);
                if tx
                    .send(StreamChunk::ProviderRetryStarted {
                        attempt,
                        max_retries: config.max_retries,
                        provider: provider.clone(),
                        wait_ms: delay.as_millis().min(u128::from(u64::MAX)) as u64,
                    })
                    .is_err()
                {
                    return;
                }
                tokio::select! {
                    () = tx.closed() => return,
                    () = tokio::time::sleep(delay) => {}
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::task::{Context, Poll};

    use async_trait::async_trait;
    use futures::{SinkExt, StreamExt};
    use swage_core::{FinishReason, ProviderResponseWitness, TokenUsage};

    use super::*;

    struct ControlledStream {
        chunks: futures::channel::mpsc::UnboundedReceiver<StreamChunk>,
        dropped: Arc<AtomicBool>,
    }

    impl Stream for ControlledStream {
        type Item = StreamChunk;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Pin::new(&mut self.chunks).poll_next(cx)
        }
    }

    impl Drop for ControlledStream {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    struct ControlledProvider {
        chunks: Mutex<Option<futures::channel::mpsc::UnboundedReceiver<StreamChunk>>>,
        dropped: Arc<AtomicBool>,
    }

    impl ControlledProvider {
        fn new() -> (
            Self,
            futures::channel::mpsc::UnboundedSender<StreamChunk>,
            Arc<AtomicBool>,
        ) {
            let (tx, rx) = futures::channel::mpsc::unbounded();
            let dropped = Arc::new(AtomicBool::new(false));
            (
                Self {
                    chunks: Mutex::new(Some(rx)),
                    dropped: Arc::clone(&dropped),
                },
                tx,
                dropped,
            )
        }
    }

    #[async_trait]
    impl LlmProvider for ControlledProvider {
        fn model_id(&self) -> &str {
            "controlled"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unreachable!("controlled provider is streaming-only")
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("controlled provider is streaming-only")
        }

        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(ControlledStream {
                chunks: self
                    .chunks
                    .lock()
                    .unwrap()
                    .take()
                    .expect("controlled stream requested more than once"),
                dropped: Arc::clone(&self.dropped),
            })
        }
    }

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

    struct ErrorEndpoint {
        base_url: String,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for ErrorEndpoint {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl ErrorEndpoint {
        async fn start(statuses: Vec<u16>, body: &'static str) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let received_calls = Arc::clone(&calls);
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0; 4096];
                    loop {
                        let read = socket.read(&mut buffer).await.unwrap();
                        assert_ne!(read, 0, "request must finish before the client disconnects");
                        request.extend_from_slice(&buffer[..read]);
                        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                        {
                            let headers = std::str::from_utf8(&request[..end]).unwrap();
                            let content_length = headers
                                .lines()
                                .filter_map(|line| line.split_once(':'))
                                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                            if request.len() >= end + 4 + content_length {
                                break;
                            }
                        }
                    }
                    let call = received_calls.fetch_add(1, Ordering::SeqCst);
                    let status = statuses[call.min(statuses.len() - 1)];
                    let response = format!(
                        "HTTP/1.1 {status} Rejected\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                    socket.shutdown().await.unwrap();
                }
            });
            Self {
                base_url,
                calls,
                task,
            }
        }
    }

    fn http_provider(adapter: &str, base_url: &str) -> Box<dyn LlmProvider> {
        let config = crate::ProviderConfig::new("test-key", "unserved-model", base_url);
        match adapter {
            "openai" => Box::new(crate::openai::OpenAiProvider::from_config(config)),
            "responses" => Box::new(crate::responses::ResponsesProvider::from_config(config)),
            "anthropic" => Box::new(crate::anthropic::AnthropicProvider::from_config(config)),
            "google" => Box::new(crate::google::GoogleProvider::from_config(config)),
            _ => panic!("unknown test adapter: {adapter}"),
        }
    }

    async fn http_error_chunks(adapter: &str, endpoint: &ErrorEndpoint) -> Vec<StreamChunk> {
        let retry =
            RetryProvider::new_erased(http_provider(adapter, &endpoint.base_url), zero_backoff());
        tokio::time::timeout(
            Duration::from_secs(5),
            retry.stream(completion_req()).collect(),
        )
        .await
        .expect("the local HTTP rejection must terminate the stream")
    }

    async fn assert_permanent_http_rejections(adapter: &str) {
        let body = r#"{"error":{"code":"model_not_found","type":"not_found_error","message":"Unknown model; HTTP error 503: do not retry"}}"#;
        for status in (400..500).filter(|status| !matches!(status, 401 | 408 | 409 | 429)) {
            let endpoint = ErrorEndpoint::start(vec![status], body).await;
            let chunks = http_error_chunks(adapter, &endpoint).await;
            assert_eq!(
                endpoint.calls.load(Ordering::SeqCst),
                1,
                "{adapter}: HTTP {status} must make exactly one request: {chunks:?}"
            );
            assert_eq!(
                chunks,
                vec![StreamChunk::Error {
                    message: format!("HTTP error {status}: {body}"),
                }],
                "{adapter}: permanent rejection must preserve the error without retry events"
            );
        }
    }

    #[tokio::test]
    async fn stream_permanent_http_rejections_openai() {
        assert_permanent_http_rejections("openai").await;
    }

    #[tokio::test]
    async fn stream_permanent_http_rejections_responses() {
        assert_permanent_http_rejections("responses").await;
    }

    #[tokio::test]
    async fn stream_permanent_http_rejections_anthropic() {
        assert_permanent_http_rejections("anthropic").await;
    }

    #[tokio::test]
    async fn stream_permanent_http_rejections_google() {
        assert_permanent_http_rejections("google").await;
    }

    #[tokio::test]
    async fn stream_authentication_rejection_is_permanent() {
        let body = r#"{"error":{"message":"HTTP error 503: try again"}}"#;
        for adapter in ["openai", "responses", "anthropic", "google"] {
            let endpoint = ErrorEndpoint::start(vec![401], body).await;
            let chunks = http_error_chunks(adapter, &endpoint).await;
            assert_eq!(
                endpoint.calls.load(Ordering::SeqCst),
                1,
                "{adapter}: authentication failure must stop after one request: {chunks:?}"
            );
            assert_eq!(
                chunks,
                vec![StreamChunk::Error {
                    message: format!("authentication failed for provider {body}"),
                }]
            );
        }
    }

    #[tokio::test]
    async fn stream_unrecognised_errors_keep_retrying() {
        for message in [
            "connection reset by peer",
            "body mentions HTTP error 400: unknown model",
            "HTTP error 4000: malformed status",
            "HTTP error 400 : malformed separator",
            "HTTP error xyz: malformed status",
        ] {
            let provider = SequencedStreamProvider {
                call_count: Arc::new(Mutex::new(0)),
                attempts: Mutex::new(vec![
                    vec![StreamChunk::Error {
                        message: message.into()
                    }];
                    4
                ]),
            };
            let calls = Arc::clone(&provider.call_count);
            let retry = RetryProvider::new(provider, zero_backoff());
            let chunks: Vec<_> = retry.stream(completion_req()).collect().await;
            assert_eq!(
                *calls.lock().unwrap(),
                4,
                "unrecognised error must retain retries: {message}: {chunks:?}"
            );
            assert_eq!(
                chunks
                    .iter()
                    .filter(|chunk| matches!(chunk, StreamChunk::ProviderRetryStarted { .. }))
                    .count(),
                3
            );
            assert!(
                matches!(chunks.last(), Some(StreamChunk::Error { message: error }) if error == message)
            );
        }
    }

    #[tokio::test]
    async fn stream_transient_http_rejections_keep_retrying() {
        // A permanent-looking code and embedded status must not override the HTTP status.
        let body =
            r#"{"error":{"code":"model_not_found","message":"HTTP error 400: unknown model"}}"#;
        for adapter in ["openai", "responses", "anthropic", "google"] {
            for status in [408, 409, 429, 500, 501, 502, 503, 504, 529, 599] {
                let endpoint = ErrorEndpoint::start(vec![status], body).await;
                let chunks = http_error_chunks(adapter, &endpoint).await;
                assert_eq!(
                    endpoint.calls.load(Ordering::SeqCst),
                    4,
                    "{adapter}: HTTP {status} must retain all three retries: {chunks:?}"
                );
                assert_eq!(
                    chunks
                        .iter()
                        .filter(|chunk| matches!(chunk, StreamChunk::ProviderRetryStarted { .. }))
                        .count(),
                    3
                );
                assert!(matches!(chunks.last(), Some(StreamChunk::Error { .. })));
            }
        }
    }

    #[tokio::test]
    async fn stream_permanent_http_error_settles_prior_retry() {
        let body = r#"{"error":{"code":"model_not_found"}}"#;
        for adapter in ["openai", "responses", "anthropic", "google"] {
            let endpoint = ErrorEndpoint::start(vec![503, 400], body).await;
            let chunks = http_error_chunks(adapter, &endpoint).await;
            assert_eq!(
                endpoint.calls.load(Ordering::SeqCst),
                2,
                "{adapter}: the permanent second rejection must stop further requests: {chunks:?}"
            );
            assert!(
                matches!(chunks.as_slice(), [
                StreamChunk::ProviderRetryStarted { attempt: 1, .. },
                StreamChunk::ProviderRetrySettled { attempt: 1, success: false, .. },
                StreamChunk::Error { message },
            ] if message == &format!("HTTP error 400: {body}")),
                "{adapter}: settle the active retry before forwarding its terminal error: {chunks:?}"
            );
        }
    }

    fn completion_req() -> CompletionRequest {
        CompletionRequest {
            effort: None,
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
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

    struct SequencedStreamProvider {
        call_count: Arc<Mutex<u32>>,
        attempts: Mutex<Vec<Vec<StreamChunk>>>,
    }

    #[async_trait]
    impl LlmProvider for SequencedStreamProvider {
        fn model_id(&self) -> &str {
            "sequenced-stream"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unreachable!("sequenced stream provider is streaming-only")
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("sequenced stream provider is streaming-only")
        }

        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            *self.call_count.lock().unwrap() += 1;
            Box::pin(futures::stream::iter(
                self.attempts.lock().unwrap().remove(0),
            ))
        }
    }

    impl AlwaysFail {
        fn new(error: ProviderError) -> Self {
            Self {
                call_count: Arc::new(Mutex::new(0)),
                error,
            }
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
    async fn stream_forwards_first_chunk_before_inner_stream_finishes() {
        let (inner, mut inner_tx, _) = ControlledProvider::new();
        let retry = RetryProvider::new(inner, zero_backoff());
        let mut stream = retry.stream(completion_req());

        inner_tx
            .send(StreamChunk::Delta {
                content: "first".into(),
            })
            .await
            .unwrap();

        let first = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .expect("first chunk must arrive while the inner stream remains open");
        assert!(matches!(
            first,
            Some(StreamChunk::Delta { content }) if content == "first"
        ));
    }

    #[tokio::test]
    async fn stream_retries_twice_and_returns_successful_chunks() {
        // First two stream attempts return an error chunk; third succeeds.
        // Caller must see retry progress plus the successful attempt's chunks.
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
        assert_eq!(
            chunks
                .iter()
                .filter(|c| matches!(c, StreamChunk::ProviderRetryStarted { .. }))
                .count(),
            2,
        );
        assert_eq!(
            chunks
                .iter()
                .filter(|c| {
                    matches!(c, StreamChunk::ProviderRetrySettled { success: true, .. })
                })
                .count(),
            1,
        );
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, StreamChunk::Delta { content } if content == "hello"))
        );
        assert!(matches!(chunks.last(), Some(StreamChunk::Done { .. })));
    }

    #[tokio::test]
    async fn stream_retries_after_pre_content_accounting_chunk() {
        let call_count = Arc::new(Mutex::new(0));
        let retry = RetryProvider::new(
            SequencedStreamProvider {
                call_count: Arc::clone(&call_count),
                attempts: Mutex::new(vec![
                    vec![
                        StreamChunk::ProviderResponseWitness(ProviderResponseWitness {
                            http_status: 200,
                            request_id: Some("discarded-attempt".into()),
                            ..ProviderResponseWitness::default()
                        }),
                        StreamChunk::Usage(TokenUsage {
                            input_tokens: 100,
                            output_tokens: 50,
                        }),
                        StreamChunk::ProviderAnomaly {
                            message: "first attempt failed".into(),
                        },
                    ],
                    vec![
                        StreamChunk::ProviderResponseWitness(ProviderResponseWitness {
                            http_status: 200,
                            request_id: Some("successful-attempt".into()),
                            ..ProviderResponseWitness::default()
                        }),
                        StreamChunk::Delta {
                            content: "success".into(),
                        },
                        StreamChunk::Usage(TokenUsage {
                            input_tokens: 10,
                            output_tokens: 5,
                        }),
                        StreamChunk::Done {
                            finish_reason: None,
                        },
                    ],
                ]),
            },
            RetryConfig {
                max_retries: 1,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::ZERO,
            },
        );

        let chunks: Vec<_> = retry.stream(completion_req()).collect().await;

        assert_eq!(*call_count.lock().unwrap(), 2);
        assert!(
            chunks.iter().any(
                |chunk| matches!(chunk, StreamChunk::Delta { content } if content == "success")
            )
        );
        let usage: Vec<_> = chunks
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::Usage(usage) => Some(usage),
                _ => None,
            })
            .collect();
        assert_eq!(usage.len(), 1, "discarded attempts must not report usage");
        assert_eq!(usage[0].input_tokens, 10);
        assert_eq!(usage[0].output_tokens, 5);
        let request_ids: Vec<_> = chunks
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::ProviderResponseWitness(witness) => witness.request_id.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(request_ids, ["discarded-attempt", "successful-attempt"]);
    }

    #[tokio::test]
    async fn stream_retries_after_empty_delta_then_anomaly() {
        let call_count = Arc::new(Mutex::new(0));
        let retry = RetryProvider::new(
            SequencedStreamProvider {
                call_count: Arc::clone(&call_count),
                attempts: Mutex::new(vec![
                    vec![
                        StreamChunk::Delta {
                            content: String::new(),
                        },
                        StreamChunk::ProviderAnomaly {
                            message: "empty response".into(),
                        },
                    ],
                    vec![
                        StreamChunk::Delta {
                            content: "recovered".into(),
                        },
                        StreamChunk::Done {
                            finish_reason: None,
                        },
                    ],
                ]),
            },
            RetryConfig {
                max_retries: 1,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::ZERO,
            },
        );

        let chunks: Vec<_> = retry.stream(completion_req()).collect().await;

        assert_eq!(*call_count.lock().unwrap(), 2);
        assert!(chunks.iter().any(
            |chunk| matches!(chunk, StreamChunk::Delta { content } if content == "recovered")
        ));
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
        assert!(
            chunks
                .iter()
                .any(|c| { matches!(c, StreamChunk::ProviderRetrySettled { success: false, .. }) }),
            "expected retry failure to settle before final error"
        );
    }

    #[tokio::test]
    async fn stream_forwards_post_output_error_without_retrying() {
        let (inner, mut inner_tx, _) = ControlledProvider::new();
        let retry = RetryProvider::new(inner, zero_backoff());
        let mut stream = retry.stream(completion_req());

        inner_tx
            .send(StreamChunk::Delta {
                content: "partial".into(),
            })
            .await
            .unwrap();
        inner_tx
            .send(StreamChunk::Error {
                message: "interrupted".into(),
            })
            .await
            .unwrap();

        assert!(matches!(
            stream.next().await,
            Some(StreamChunk::Delta { content }) if content == "partial"
        ));
        assert!(matches!(
            stream.next().await,
            Some(StreamChunk::Error { message }) if message == "interrupted"
        ));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn stream_does_not_retry_after_non_delta_content() {
        let content_chunks = [
            StreamChunk::Reasoning {
                content: "working".into(),
                details: None,
            },
            StreamChunk::ToolUse {
                call_id: "call-1".into(),
                name: "lookup".into(),
                arguments: serde_json::json!({"query": "status"}),
            },
        ];

        for content_chunk in content_chunks {
            let call_count = Arc::new(Mutex::new(0));
            let retry = RetryProvider::new(
                SequencedStreamProvider {
                    call_count: Arc::clone(&call_count),
                    attempts: Mutex::new(vec![
                        vec![
                            content_chunk.clone(),
                            StreamChunk::Error {
                                message: "interrupted".into(),
                            },
                        ],
                        vec![StreamChunk::Delta {
                            content: "duplicate".into(),
                        }],
                    ]),
                },
                zero_backoff(),
            );

            let chunks: Vec<_> = retry.stream(completion_req()).collect().await;

            assert_eq!(
                *call_count.lock().unwrap(),
                1,
                "content must commit the first attempt: {content_chunk:?}"
            );
            assert!(chunks.contains(&content_chunk));
            assert!(matches!(
                chunks.last(),
                Some(StreamChunk::Error { message }) if message == "interrupted"
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_receiver_during_backoff_prevents_retry() {
        let inner = FailN::new(2, ProviderError::StreamError("transient".into()));
        let call_count = Arc::clone(&inner.call_count);
        let retry = RetryProvider::new(
            inner,
            RetryConfig {
                max_retries: 1,
                initial_backoff: Duration::from_secs(60),
                max_backoff: Duration::from_secs(60),
            },
        );
        let mut stream = retry.stream(completion_req());

        assert!(matches!(
            stream.next().await,
            Some(StreamChunk::ProviderRetryStarted { attempt: 1, .. })
        ));
        assert_eq!(*call_count.lock().unwrap(), 1);
        drop(stream);

        tokio::time::advance(Duration::from_secs(61)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            *call_count.lock().unwrap(),
            1,
            "abandoning the stream during backoff must cancel the retry"
        );
    }

    #[tokio::test]
    async fn dropping_receiver_drops_active_inner_stream() {
        let (inner, mut inner_tx, dropped) = ControlledProvider::new();
        let retry = RetryProvider::new(inner, zero_backoff());
        let mut stream = retry.stream(completion_req());

        inner_tx
            .send(StreamChunk::Delta {
                content: "first".into(),
            })
            .await
            .unwrap();
        assert!(stream.next().await.is_some());
        drop(stream);

        tokio::time::timeout(Duration::from_millis(100), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("receiver closure must drop the active inner stream");
    }

    struct CountingProbe;

    #[async_trait]
    impl LlmProvider for CountingProbe {
        fn model_id(&self) -> &str {
            "counting-probe"
        }

        fn provider_request_snapshot(
            &self,
            _: &CompletionRequest,
            stream: bool,
        ) -> serde_json::Value {
            serde_json::json!({"provider": "probe", "stream": stream})
        }

        fn effort_capability(&self, _: &str) -> Option<EffortCapability> {
            Some(EffortCapability::Levels {
                levels: vec![swage_core::Effort::Low],
                can_disable: false,
            })
        }

        async fn count_tokens(&self, _: &CompletionRequest) -> Result<Option<u64>, ProviderError> {
            Ok(Some(42))
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unreachable!("count-only probe")
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("count-only probe")
        }

        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unreachable!("count-only probe")
        }
    }

    fn count_req() -> CompletionRequest {
        CompletionRequest {
            effort: None,
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        }
    }

    // A defaulted trait method is invisible to a wrapper, so delegation is
    // asserted explicitly: an unforwarded count would report None here.
    #[tokio::test]
    async fn count_tokens_delegates_to_the_wrapped_provider() {
        let provider = RetryProvider::with_defaults(CountingProbe);
        assert_eq!(provider.count_tokens(&count_req()).await.unwrap(), Some(42));
    }

    #[test]
    fn effort_capability_delegates_to_the_wrapped_provider() {
        let provider = RetryProvider::with_defaults(CountingProbe);
        assert!(provider.effort_capability("any").is_some());
    }

    #[test]
    fn provider_request_snapshot_delegates_to_the_wrapped_provider() {
        let provider = RetryProvider::with_defaults(CountingProbe);
        assert_eq!(
            provider.provider_request_snapshot(&count_req(), true),
            serde_json::json!({"provider": "probe", "stream": true})
        );
    }
}
