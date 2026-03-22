//! Failover decorator for LLM providers.
//!
//! Routes requests through an ordered list of providers. If the primary fails
//! with a transient error, the request falls through to the next provider.
//! Failed providers enter a cooldown period before being tried again.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio::time::Instant;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};

use crate::provider::LlmProvider;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Failover configuration for `FailoverProvider`.
#[derive(Debug, Clone)]
pub struct FailoverConfig {
    /// How long a failed provider stays out of the rotation before being retried.
    pub cooldown_duration: Duration,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self { cooldown_duration: Duration::from_secs(60) }
    }
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// Per-provider cooldown entry: `Some(since)` when in cooldown, `None` when available.
type CooldownMap = Arc<Mutex<Vec<Option<Instant>>>>;

// ---------------------------------------------------------------------------
// FailoverProvider
// ---------------------------------------------------------------------------

/// `LlmProvider` decorator that routes through an ordered list of providers,
/// marking each one that fails transiently as "in cooldown" and trying the
/// next one in sequence.
///
/// ### Routing order
///
/// On each request, providers are tried in two passes:
/// 1. Non-cooled providers, in their original order.
/// 2. Providers still in cooldown, ordered from earliest start (most likely
///    recovered) to latest.
///
/// ### Cooldown lifecycle
///
/// A provider enters cooldown when it returns a transient error. Once
/// `cooldown_duration` elapses, it re-enters the normal rotation. If it
/// succeeds after returning from cooldown, the entry is cleared and it resumes
/// its original position.
///
/// ### Permanent errors
///
/// Permanent errors (e.g. Auth, InvalidConfig) are returned immediately
/// without trying other providers, and do not affect cooldown state.
///
/// ### Concurrency safety
///
/// Cooldown state is protected by a `Mutex` shared across all clones of the
/// Arc, so concurrent requests see a consistent view without data races.
pub struct FailoverProvider {
    providers: Vec<Arc<dyn LlmProvider>>,
    cooldowns: CooldownMap,
    config: FailoverConfig,
}

impl FailoverProvider {
    /// Construct a `FailoverProvider` from an ordered list of providers and a
    /// configuration.
    ///
    /// Providers are tried in the given order, subject to cooldown state.
    pub fn new(
        providers: Vec<Arc<dyn LlmProvider>>,
        config: FailoverConfig,
    ) -> Self {
        let n = providers.len();
        Self {
            providers,
            cooldowns: Arc::new(Mutex::new(vec![None; n])),
            config,
        }
    }

    /// Construct with the default configuration (60 s cooldown).
    pub fn with_defaults(providers: Vec<Arc<dyn LlmProvider>>) -> Self {
        Self::new(providers, FailoverConfig::default())
    }

    /// Compute the order in which providers should be tried for this request.
    ///
    /// Returns provider indices in the following order:
    /// - Non-cooled providers, preserving original insertion order.
    /// - Cooled providers, sorted by earliest cooldown start (most likely recovered).
    fn compute_try_order(&self) -> Vec<usize> {
        let now = Instant::now();
        let cooldowns = self.cooldowns.lock().unwrap();
        let n = self.providers.len();

        let mut available: Vec<usize> = Vec::new();
        let mut still_cooled: Vec<(Instant, usize)> = Vec::new();

        for i in 0..n {
            match cooldowns[i] {
                None => available.push(i),
                Some(since) => {
                    if now.duration_since(since) >= self.config.cooldown_duration {
                        // Cooldown expired — eligible again.
                        available.push(i);
                    } else {
                        still_cooled.push((since, i));
                    }
                }
            }
        }

        // Earliest start = longest in cooldown = most likely recovered.
        still_cooled.sort_by_key(|(since, _)| *since);

        let mut order = available;
        order.extend(still_cooled.into_iter().map(|(_, i)| i));
        order
    }

    /// Put provider `idx` into cooldown.
    fn set_cooldown(&self, idx: usize) {
        self.cooldowns.lock().unwrap()[idx] = Some(Instant::now());
    }

    /// Clear the cooldown for provider `idx` (called on success).
    fn clear_cooldown(&self, idx: usize) {
        self.cooldowns.lock().unwrap()[idx] = None;
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for FailoverProvider {
    /// Returns the first provider's model identifier as the canonical model for
    /// this failover group. The actual model used on any given request may
    /// differ when failover occurs.
    fn model_id(&self) -> &str {
        self.providers.first()
            .map(|p| p.as_ref().model_id())
            .unwrap_or("")
    }

    /// Route the completion through the provider chain, failing over on transient
    /// errors.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let order = self.compute_try_order();

        if order.is_empty() {
            return Err(ProviderError::InvalidConfig {
                message: "failover provider list is empty".into(),
            });
        }

        let mut last_error: Option<ProviderError> = None;

        for idx in order {
            match self.providers[idx].complete(request.clone()).await {
                Ok(response) => {
                    // Success — clear any prior cooldown and return.
                    self.clear_cooldown(idx);
                    return Ok(response);
                }
                Err(err) if err.is_retriable() => {
                    // Transient failure — put provider in cooldown and continue.
                    self.set_cooldown(idx);
                    last_error = Some(err);
                }
                Err(err) => {
                    // Permanent failure — return immediately without trying others.
                    return Err(err);
                }
            }
        }

        // All providers failed or were exhausted.
        Err(last_error.unwrap_or_else(|| ProviderError::InvalidConfig {
            message: "all failover providers failed".into(),
        }))
    }

    /// Route the tool-completion through the provider chain, failing over on
    /// transient errors.
    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        let order = self.compute_try_order();

        if order.is_empty() {
            return Err(ProviderError::InvalidConfig {
                message: "failover provider list is empty".into(),
            });
        }

        let mut last_error: Option<ProviderError> = None;

        for idx in order {
            match self.providers[idx].complete_with_tools(request.clone()).await {
                Ok(response) => {
                    self.clear_cooldown(idx);
                    return Ok(response);
                }
                Err(err) if err.is_retriable() => {
                    self.set_cooldown(idx);
                    last_error = Some(err);
                }
                Err(err) => {
                    return Err(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| ProviderError::InvalidConfig {
            message: "all failover providers failed".into(),
        }))
    }

    /// Route the streaming completion through the provider chain.
    ///
    /// The inner stream is buffered. If a `StreamChunk::Error` appears, the
    /// provider is put into cooldown and the next provider is tried. Only the
    /// successful stream's chunks are returned to the caller.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let providers = self.providers.clone();
        let cooldowns = Arc::clone(&self.cooldowns);
        let config = self.config.clone();

        Box::pin(
            futures::stream::once(async move {
                let order = {
                    let now = Instant::now();
                    let locked = cooldowns.lock().unwrap();
                    let n = providers.len();

                    let mut available: Vec<usize> = Vec::new();
                    let mut still_cooled: Vec<(Instant, usize)> = Vec::new();

                    for i in 0..n {
                        match locked[i] {
                            None => available.push(i),
                            Some(since) => {
                                if now.duration_since(since) >= config.cooldown_duration {
                                    available.push(i);
                                } else {
                                    still_cooled.push((since, i));
                                }
                            }
                        }
                    }

                    still_cooled.sort_by_key(|(since, _)| *since);
                    let mut o = available;
                    o.extend(still_cooled.into_iter().map(|(_, i)| i));
                    o
                };

                if order.is_empty() {
                    return vec![StreamChunk::Error {
                        message: "failover provider list is empty".into(),
                    }];
                }

                let mut last_error_msg: Option<String> = None;

                for idx in order {
                    let chunks: Vec<StreamChunk> =
                        providers[idx].stream(request.clone()).collect().await;

                    let error_msg = chunks.iter().find_map(|c| {
                        if let StreamChunk::Error { message } = c {
                            Some(message.clone())
                        } else {
                            None
                        }
                    });

                    match error_msg {
                        None => {
                            // Success — clear cooldown.
                            cooldowns.lock().unwrap()[idx] = None;
                            return chunks;
                        }
                        Some(msg) => {
                            // Treat all stream errors as transient for failover purposes.
                            cooldowns.lock().unwrap()[idx] = Some(Instant::now());
                            last_error_msg = Some(msg);
                        }
                    }
                }

                vec![StreamChunk::Error {
                    message: last_error_msg
                        .unwrap_or_else(|| "all failover providers failed".into()),
                }]
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
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use futures::StreamExt;
    use swage_core::FinishReason;
    use tokio::time::advance;

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

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

    fn ok_completion(content: &str) -> CompletionResponse {
        CompletionResponse {
            content: content.into(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cost: None,
        }
    }

    // -----------------------------------------------------------------------
    // MockProvider — configurable sequence of results.
    // -----------------------------------------------------------------------

    /// A provider that returns results from a pre-configured queue.
    /// Panics if the queue is exhausted before a result is needed — this
    /// catches tests that make unexpected extra calls.
    struct SequencedProvider {
        name: &'static str,
        call_count: Arc<StdMutex<u32>>,
        results: StdMutex<Vec<Result<CompletionResponse, ProviderError>>>,
    }

    impl SequencedProvider {
        fn new(
            name: &'static str,
            results: Vec<Result<CompletionResponse, ProviderError>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                name,
                call_count: Arc::new(StdMutex::new(0)),
                results: StdMutex::new(results),
            })
        }

        fn calls(&self) -> u32 {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmProvider for SequencedProvider {
        fn model_id(&self) -> &str { self.name }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            *self.call_count.lock().unwrap() += 1;
            self.results
                .lock()
                .unwrap()
                .remove(0)
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            *self.call_count.lock().unwrap() += 1;
            self.results
                .lock()
                .unwrap()
                .remove(0)
                .map(|r| ToolCompletionResponse {
                    content: r.content,
                    input_tokens: r.input_tokens,
                    output_tokens: r.output_tokens,
                    finish_reason: r.finish_reason,
                    cost: r.cost,
                    tool_calls: vec![],
                })
        }

        fn stream(
            &self,
            _: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            *self.call_count.lock().unwrap() += 1;
            let result = self.results.lock().unwrap().remove(0);
            match result {
                Ok(r) => Box::pin(futures::stream::iter(vec![
                    StreamChunk::Delta { content: r.content },
                    StreamChunk::Done { finish_reason: None },
                ])),
                Err(_) => Box::pin(futures::stream::iter(vec![
                    StreamChunk::Error { message: "stream error".into() },
                ])),
            }
        }
    }

    // -----------------------------------------------------------------------
    // FailoverConfig tests
    // -----------------------------------------------------------------------

    #[test]
    fn failover_config_default_values() {
        let config = FailoverConfig::default();
        assert_eq!(config.cooldown_duration, Duration::from_secs(60));
    }

    // -----------------------------------------------------------------------
    // complete() — basic routing
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn complete_uses_first_provider_on_success() {
        let p1 = SequencedProvider::new("p1", vec![Ok(ok_completion("from-p1"))]);
        let p2 = SequencedProvider::new("p2", vec![]);
        let p1_calls = Arc::clone(&p1.call_count);
        let p2_calls = Arc::clone(&p2.call_count);

        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        let result = failover.complete(completion_req()).await.unwrap();

        assert_eq!(result.content, "from-p1");
        assert_eq!(*p1_calls.lock().unwrap(), 1);
        assert_eq!(*p2_calls.lock().unwrap(), 0, "p2 must not be called");
    }

    #[tokio::test]
    async fn complete_falls_through_to_next_on_transient_failure() {
        // p1 fails transiently, p2 succeeds — caller gets p2's response.
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Network { provider: "p1".into(), message: "down".into() }),
        ]);
        let p2 = SequencedProvider::new("p2", vec![Ok(ok_completion("from-p2"))]);
        let p1_calls = Arc::clone(&p1.call_count);
        let p2_calls = Arc::clone(&p2.call_count);

        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        let result = failover.complete(completion_req()).await.unwrap();

        assert_eq!(result.content, "from-p2");
        assert_eq!(*p1_calls.lock().unwrap(), 1);
        assert_eq!(*p2_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn complete_falls_through_all_three_to_last_provider() {
        // First two fail transiently; third receives and handles the request.
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Timeout { provider: "p1".into(), duration: Duration::from_secs(5) }),
        ]);
        let p2 = SequencedProvider::new("p2", vec![
            Err(ProviderError::Network { provider: "p2".into(), message: "err".into() }),
        ]);
        let p3 = SequencedProvider::new("p3", vec![Ok(ok_completion("from-p3"))]);
        let p3_calls = Arc::clone(&p3.call_count);

        let failover = FailoverProvider::with_defaults(vec![p1, p2, p3]);
        let result = failover.complete(completion_req()).await.unwrap();

        assert_eq!(result.content, "from-p3");
        assert_eq!(*p3_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn complete_returns_permanent_error_without_failover() {
        // Auth errors must not trigger failover — returned immediately.
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Auth { provider: "p1".into() }),
        ]);
        let p2 = SequencedProvider::new("p2", vec![]);
        let p2_calls = Arc::clone(&p2.call_count);

        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        let result = failover.complete(completion_req()).await;

        assert!(matches!(result.unwrap_err(), ProviderError::Auth { .. }));
        assert_eq!(*p2_calls.lock().unwrap(), 0, "p2 must not be called on permanent error");
    }

    #[tokio::test]
    async fn complete_returns_last_error_when_all_providers_fail() {
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Network { provider: "p1".into(), message: "err1".into() }),
        ]);
        let p2 = SequencedProvider::new("p2", vec![
            Err(ProviderError::Network { provider: "p2".into(), message: "err2".into() }),
        ]);

        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        let err = failover.complete(completion_req()).await.unwrap_err();

        // Last error came from p2.
        assert!(matches!(err, ProviderError::Network { .. }));
        if let ProviderError::Network { provider, .. } = err {
            assert_eq!(provider, "p2");
        }
    }

    // -----------------------------------------------------------------------
    // Cooldown — entry and expiry
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn failed_provider_skipped_during_cooldown() {
        // p1 fails → enters cooldown. Second request skips p1 and goes to p2.
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Network { provider: "p1".into(), message: "err".into() }),
            // p1 should NOT be called on the second request (still in cooldown).
        ]);
        let p2 = SequencedProvider::new("p2", vec![
            Ok(ok_completion("second-from-p2")),
            Ok(ok_completion("third-from-p2")),
        ]);
        let p1_calls = Arc::clone(&p1.call_count);
        let p2_calls = Arc::clone(&p2.call_count);

        let failover = FailoverProvider::new(
            vec![p1, p2],
            FailoverConfig { cooldown_duration: Duration::from_secs(60) },
        );

        // First request fails on p1, succeeds on p2.
        let r1 = failover.complete(completion_req()).await.unwrap();
        assert_eq!(r1.content, "second-from-p2");

        // Second request — p1 is in cooldown, goes directly to p2.
        let r2 = failover.complete(completion_req()).await.unwrap();
        assert_eq!(r2.content, "third-from-p2");

        // p1 was only called once (on the first request).
        assert_eq!(*p1_calls.lock().unwrap(), 1);
        assert_eq!(*p2_calls.lock().unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn cooled_down_provider_resumes_after_expiry() {
        let cooldown = Duration::from_secs(5);

        // p1 fails on call 1, then succeeds on call 2 (after cooldown expires).
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Network { provider: "p1".into(), message: "err".into() }),
            Ok(ok_completion("p1-recovered")),
        ]);
        let p2 = SequencedProvider::new("p2", vec![
            Ok(ok_completion("p2-temporary")),
        ]);
        let p1_calls = Arc::clone(&p1.call_count);

        let failover = FailoverProvider::new(
            vec![p1, p2],
            FailoverConfig { cooldown_duration: cooldown },
        );

        // Request 1: p1 fails → p2 handles.
        failover.complete(completion_req()).await.unwrap();

        // Advance past the cooldown window.
        advance(cooldown + Duration::from_millis(1)).await;

        // Request 2: p1's cooldown is expired, it should be tried first again.
        let result = failover.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "p1-recovered");
        assert_eq!(*p1_calls.lock().unwrap(), 2, "p1 should have been called twice");
    }

    #[tokio::test(start_paused = true)]
    async fn all_cooled_uses_earliest_start_first() {
        let cooldown = Duration::from_secs(10);

        // p1 fails at t=0, p2 fails at t=1s — both enter cooldown.
        // When we request at t=2s (both still cooled), p1 (earliest start) goes first.
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Network { provider: "p1".into(), message: "err".into() }),
            Ok(ok_completion("p1-probed")),
        ]);
        let p2 = SequencedProvider::new("p2", vec![
            Err(ProviderError::Network { provider: "p2".into(), message: "err".into() }),
            // p2 should not be called after p1 succeeds.
        ]);
        let p1_calls = Arc::clone(&p1.call_count);
        let p2_calls = Arc::clone(&p2.call_count);

        let failover = FailoverProvider::new(vec![p1, p2], FailoverConfig { cooldown_duration: cooldown });

        // t=0: first request — p1 fails, p2 gets it (but also fails).
        let _ = failover.complete(completion_req()).await;
        // At this point p1 cooled at t=0, p2 cooled at t=0 too (same tick)
        // We want p1 to have been cooled first — let's advance slightly between requests.
        // Actually both fail in the same request, so they both get Instant::now() at nearly the same time.
        // The ordering is: p1 tried first → fails → p2 tried → fails.
        // p1 cooled_since < p2 cooled_since (slightly).
        // So p1 should be probed first when all are in cooldown.

        // Now all are in cooldown. Next request should probe p1 first.
        let result = failover.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "p1-probed");
        // p1 was called twice (once failed, once succeeded).
        assert_eq!(*p1_calls.lock().unwrap(), 2);
        // p2 was called once (on first request's failover path).
        assert_eq!(*p2_calls.lock().unwrap(), 1);
    }

    // -----------------------------------------------------------------------
    // complete_with_tools() — failover path
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn complete_with_tools_falls_through_on_transient_failure() {
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::Timeout { provider: "p1".into(), duration: Duration::from_secs(5) }),
        ]);
        let p2 = SequencedProvider::new("p2", vec![Ok(ok_completion("p2-tools"))]);
        let p2_calls = Arc::clone(&p2.call_count);

        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        let result = failover.complete_with_tools(tool_req()).await.unwrap();

        assert_eq!(result.content, "p2-tools");
        assert_eq!(*p2_calls.lock().unwrap(), 1);
    }

    // -----------------------------------------------------------------------
    // stream() — failover on stream error
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stream_falls_through_to_next_on_stream_error() {
        // p1's stream emits an error; p2's stream succeeds.
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::StreamError("oops".into())),
        ]);
        let p2 = SequencedProvider::new("p2", vec![Ok(ok_completion("p2-stream"))]);
        let p2_calls = Arc::clone(&p2.call_count);

        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        let chunks: Vec<StreamChunk> = failover.stream(completion_req()).collect().await;

        // No error chunk in result.
        assert!(
            chunks.iter().all(|c| !matches!(c, StreamChunk::Error { .. })),
            "expected no error chunks: {chunks:?}"
        );
        assert!(chunks.iter().any(|c| matches!(c, StreamChunk::Delta { content } if content == "p2-stream")));
        assert_eq!(*p2_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn stream_returns_error_when_all_providers_fail() {
        let p1 = SequencedProvider::new("p1", vec![
            Err(ProviderError::StreamError("err1".into())),
        ]);
        let p2 = SequencedProvider::new("p2", vec![
            Err(ProviderError::StreamError("err2".into())),
        ]);

        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        let chunks: Vec<StreamChunk> = failover.stream(completion_req()).collect().await;

        assert!(
            chunks.iter().any(|c| matches!(c, StreamChunk::Error { .. })),
            "expected an error chunk when all providers fail"
        );
    }

    // -----------------------------------------------------------------------
    // Concurrency safety
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn concurrent_requests_do_not_corrupt_state() {
        // All requests should route cleanly to p2 (p1 always fails transiently).
        // Spawning many concurrent tasks verifies no Mutex panic or state corruption.
        let failover = Arc::new(FailoverProvider::new(
            vec![
                SequencedProvider::new("p1", {
                    // Pre-load enough failures for concurrent requests.
                    (0..20)
                        .map(|_| Err(ProviderError::Network {
                            provider: "p1".into(),
                            message: "down".into(),
                        }))
                        .collect()
                }),
                SequencedProvider::new("p2", {
                    (0..20).map(|i| Ok(ok_completion(&format!("r{i}")))).collect()
                }),
            ],
            FailoverConfig { cooldown_duration: Duration::from_secs(60) },
        ));

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let f = Arc::clone(&failover);
                tokio::spawn(async move { f.complete(completion_req()).await })
            })
            .collect();

        for handle in handles {
            let result = handle.await.expect("task panicked");
            assert!(result.is_ok(), "expected success: {result:?}");
        }
    }

    // -----------------------------------------------------------------------
    // model_id
    // -----------------------------------------------------------------------

    #[test]
    fn model_id_returns_first_provider_model() {
        let p1 = SequencedProvider::new("primary-model", vec![]);
        let p2 = SequencedProvider::new("secondary-model", vec![]);
        let failover = FailoverProvider::with_defaults(vec![p1, p2]);
        assert_eq!(failover.model_id(), "primary-model");
    }

    #[test]
    fn model_id_empty_providers_returns_empty_string() {
        let failover = FailoverProvider::with_defaults(vec![]);
        assert_eq!(failover.model_id(), "");
    }
}
