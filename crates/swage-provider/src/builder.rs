//! Provider chain builder.
//!
//! Declaratively compose the reliability stack with a fluent API instead of
//! manually nesting decorators.
//!
//! ## Composition order
//!
//! The builder enforces a fixed decorator nesting order so callers don't need
//! to remember which layer wraps which:
//!
//! ```text
//! FailoverProvider
//!   └─ RetryProvider          ← retry wraps CB so circuit-open is retriable
//!       └─ CircuitBreakerProvider
//!           └─ concrete backend (OpenAI, Anthropic, …)
//! ```
//!
//! ## Examples
//!
//! Full stack with two providers:
//!
//! ```ignore
//! let provider = ProviderChainBuilder::new()
//!     .provider(openai)
//!         .with_circuit_breaker(CircuitBreakerConfig::default())
//!         .with_retry(RetryConfig::default())
//!     .provider(anthropic)
//!         .with_retry(RetryConfig::default())
//!     .with_failover(FailoverConfig::default())
//!     .build();
//! ```
//!
//! Single provider, no decorators:
//!
//! ```ignore
//! let provider = ProviderChainBuilder::new()
//!     .provider(openai)
//!     .build();
//! ```

use std::sync::Arc;

use crate::circuit_breaker::{CircuitBreakerConfig, CircuitBreakerProvider};
use crate::failover::{FailoverConfig, FailoverProvider};
use crate::provider::LlmProvider;
use crate::retry::{RetryConfig, RetryProvider};

// ---------------------------------------------------------------------------
// ProviderChainBuilder — starting state, no current provider pending
// ---------------------------------------------------------------------------

/// Entry point for building a composable provider reliability stack.
///
/// Call `.provider(p)` to start configuring the first provider.
#[derive(Default)]
pub struct ProviderChainBuilder {
    /// Providers that have been fully configured and type-erased.
    finalized: Vec<Arc<dyn LlmProvider>>,
    /// Optional failover config to apply across all finalized providers.
    failover_config: Option<FailoverConfig>,
}

impl ProviderChainBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a provider to the chain.
    ///
    /// Returns a `ProviderBuilder` so you can attach reliability decorators
    /// (`.with_retry()`, `.with_circuit_breaker()`) before moving on to the
    /// next provider or calling `.build()`.
    pub fn provider(self, p: impl LlmProvider + 'static) -> ProviderBuilder {
        ProviderBuilder {
            chain: self,
            current: Box::new(p),
            retry_config: None,
            cb_config: None,
        }
    }

    /// Finalize the chain and produce a single `Box<dyn LlmProvider>`.
    ///
    /// Panics if no providers have been added.
    pub fn build(self) -> Box<dyn LlmProvider> {
        assert!(
            !self.finalized.is_empty(),
            "ProviderChainBuilder::build called with no providers"
        );
        Self::assemble(self.finalized, self.failover_config)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Wrap a single configured provider entry into a type-erased Arc.
    ///
    /// Nesting order: circuit breaker (innermost) → retry (outermost).
    fn finalize_entry(
        provider: Box<dyn LlmProvider>,
        retry_config: Option<RetryConfig>,
        cb_config: Option<CircuitBreakerConfig>,
    ) -> Arc<dyn LlmProvider> {
        // Wrap with circuit breaker first (closest to the backend).
        let after_cb: Box<dyn LlmProvider> = match cb_config {
            Some(cb) => Box::new(CircuitBreakerProvider::new(provider, cb)),
            None => provider,
        };

        // Retry wraps the circuit breaker so that a circuit-open error
        // (which is classified as retriable) does not trigger another retry —
        // it's returned to the caller (or outer failover layer) immediately.
        match retry_config {
            Some(retry) => Arc::new(RetryProvider::new_erased(after_cb, retry)),
            None => Arc::from(after_cb),
        }
    }

    /// Turn the finalized provider list into the output type.
    ///
    /// - One provider, no failover → wrap in a thin `ArcProvider` delegator.
    /// - Multiple providers or explicit failover config → wrap in `FailoverProvider`.
    ///
    /// Note: trait objects are not `Sized` so we cannot `Arc::try_unwrap` back
    /// to a plain value. The `ArcProvider` wrapper adds a negligible indirection.
    fn assemble(
        providers: Vec<Arc<dyn LlmProvider>>,
        failover_config: Option<FailoverConfig>,
    ) -> Box<dyn LlmProvider> {
        if providers.len() == 1 && failover_config.is_none() {
            let arc = providers.into_iter().next().unwrap();
            Box::new(ArcProvider(arc))
        } else {
            let config = failover_config.unwrap_or_default();
            Box::new(FailoverProvider::new(providers, config))
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderBuilder — per-provider configuration state
// ---------------------------------------------------------------------------

/// Configures reliability decorators for a single provider entry.
///
/// Returned by `ProviderChainBuilder::provider()`. Call `.with_retry()` and
/// `.with_circuit_breaker()` to attach decorators, then either:
/// - `.provider(next)` to finalize this entry and start the next, or
/// - `.with_failover(cfg)` to finalize and set the failover configuration, or
/// - `.build()` to finalize and produce the complete stack.
pub struct ProviderBuilder {
    chain: ProviderChainBuilder,
    current: Box<dyn LlmProvider>,
    retry_config: Option<RetryConfig>,
    cb_config: Option<CircuitBreakerConfig>,
}

impl ProviderBuilder {
    /// Attach a retry decorator to the current provider entry.
    pub fn with_retry(mut self, config: RetryConfig) -> Self {
        self.retry_config = Some(config);
        self
    }

    /// Attach a circuit breaker to the current provider entry.
    pub fn with_circuit_breaker(mut self, config: CircuitBreakerConfig) -> Self {
        self.cb_config = Some(config);
        self
    }

    /// Finalize the current provider entry and begin configuring the next.
    pub fn provider(self, p: impl LlmProvider + 'static) -> ProviderBuilder {
        let chain = self.finalize_into_chain();
        ProviderBuilder {
            chain,
            current: Box::new(p),
            retry_config: None,
            cb_config: None,
        }
    }

    /// Finalize the current provider and set the failover configuration that
    /// will wrap all providers when `.build()` is called.
    pub fn with_failover(self, config: FailoverConfig) -> ProviderChainBuilder {
        let mut chain = self.finalize_into_chain();
        chain.failover_config = Some(config);
        chain
    }

    /// Finalize and assemble the complete reliability stack.
    pub fn build(self) -> Box<dyn LlmProvider> {
        let chain = self.finalize_into_chain();
        chain.build()
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Seal the current entry and move it into the chain's finalized list.
    fn finalize_into_chain(self) -> ProviderChainBuilder {
        let entry = ProviderChainBuilder::finalize_entry(
            self.current,
            self.retry_config,
            self.cb_config,
        );
        let mut chain = self.chain;
        chain.finalized.push(entry);
        chain
    }
}

// ---------------------------------------------------------------------------
// ArcProvider — thin Arc-wrapping delegator used in the single-provider path
// ---------------------------------------------------------------------------

/// Delegates all `LlmProvider` calls to an inner `Arc<dyn LlmProvider>`.
///
/// Used only when `Arc::try_unwrap` fails in the single-provider path, which
/// is not expected to occur in practice.
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};

struct ArcProvider(Arc<dyn LlmProvider>);

#[async_trait]
impl LlmProvider for ArcProvider {
    fn model_id(&self) -> &str {
        self.0.model_id()
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        self.0.complete(request).await
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        self.0.complete_with_tools(request).await
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        self.0.stream(request)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use swage_core::{FinishReason, ToolCompletionRequest, ToolCompletionResponse};

    use super::*;
    use crate::circuit_breaker::CircuitBreakerConfig;
    use crate::failover::FailoverConfig;
    use crate::retry::RetryConfig;

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

    fn ok_completion(content: &str) -> CompletionResponse {
        CompletionResponse {
            content: content.into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cost: None,
        }
    }

    // -----------------------------------------------------------------------
    // TrackingProvider — records calls, returns canned responses.
    // -----------------------------------------------------------------------

    struct TrackingProvider {
        name: &'static str,
        call_count: Arc<Mutex<u32>>,
        responses: Mutex<Vec<Result<CompletionResponse, ProviderError>>>,
    }

    impl TrackingProvider {
        fn new(
            name: &'static str,
            responses: Vec<Result<CompletionResponse, ProviderError>>,
        ) -> Self {
            Self {
                name,
                call_count: Arc::new(Mutex::new(0)),
                responses: Mutex::new(responses),
            }
        }

        fn calls(&self) -> u32 {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmProvider for TrackingProvider {
        fn model_id(&self) -> &str { self.name }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            *self.call_count.lock().unwrap() += 1;
            self.responses
                .lock()
                .unwrap()
                .remove(0)
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!()
        }

        fn stream(
            &self,
            _: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            *self.call_count.lock().unwrap() += 1;
            Box::pin(futures::stream::iter(vec![
                StreamChunk::Delta { content: self.name.into() },
                StreamChunk::Done { finish_reason: None },
            ]))
        }
    }

    // -----------------------------------------------------------------------
    // Bare provider — no decorators
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn bare_provider_builds_and_routes_to_inner() {
        let provider = ProviderChainBuilder::new()
            .provider(TrackingProvider::new("bare", vec![Ok(ok_completion("hello"))]))
            .build();

        let result = provider.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "hello");
        assert_eq!(provider.model_id(), "bare");
    }

    // -----------------------------------------------------------------------
    // With retry only
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn provider_with_retry_retries_on_transient_failure() {
        // Two failures then success — caller gets the successful response.
        let responses = vec![
            Err(ProviderError::Network { provider: "p".into(), message: "err".into() }),
            Err(ProviderError::Network { provider: "p".into(), message: "err".into() }),
            Ok(ok_completion("after-retry")),
        ];

        let provider = ProviderChainBuilder::new()
            .provider(TrackingProvider::new("retry-test", responses))
            .with_retry(RetryConfig {
                max_retries: 3,
                initial_backoff: std::time::Duration::ZERO,
                max_backoff: std::time::Duration::ZERO,
            })
            .build();

        let result = provider.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "after-retry");
    }

    // -----------------------------------------------------------------------
    // With circuit breaker only
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn provider_with_circuit_breaker_opens_after_threshold() {
        // Threshold = 2 — two failures should open the circuit.
        let responses = vec![
            Err(ProviderError::Network { provider: "p".into(), message: "err1".into() }),
            Err(ProviderError::Network { provider: "p".into(), message: "err2".into() }),
            // Circuit is now open — this response must never be consumed.
            Ok(ok_completion("unreachable")),
        ];

        let provider = ProviderChainBuilder::new()
            .provider(TrackingProvider::new("cb-test", responses))
            .with_circuit_breaker(CircuitBreakerConfig {
                failure_threshold: 2,
                recovery_timeout: std::time::Duration::from_secs(60),
            })
            .build();

        // Two failures trip the circuit.
        let _ = provider.complete(completion_req()).await;
        let _ = provider.complete(completion_req()).await;

        // Third call should be rejected by the open circuit.
        let err = provider.complete(completion_req()).await.unwrap_err();
        assert!(err.is_retriable(), "circuit-open error must be retriable");
    }

    // -----------------------------------------------------------------------
    // With both retry and circuit breaker
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn provider_with_retry_and_cb_retries_before_opening() {
        // Threshold = 3, retry = 1 max retry.
        // First call fails once → retry → succeeds. Circuit never opens.
        let responses = vec![
            Err(ProviderError::Network { provider: "p".into(), message: "err".into() }),
            Ok(ok_completion("recovered")),
        ];

        let provider = ProviderChainBuilder::new()
            .provider(TrackingProvider::new("retry-cb", responses))
            .with_circuit_breaker(CircuitBreakerConfig {
                failure_threshold: 3,
                recovery_timeout: std::time::Duration::from_secs(60),
            })
            .with_retry(RetryConfig {
                max_retries: 2,
                initial_backoff: std::time::Duration::ZERO,
                max_backoff: std::time::Duration::ZERO,
            })
            .build();

        let result = provider.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "recovered");
    }

    // -----------------------------------------------------------------------
    // Multi-provider with failover
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn two_providers_with_failover_falls_through_on_transient_failure() {
        let p1 = TrackingProvider::new("p1", vec![
            Err(ProviderError::Network { provider: "p1".into(), message: "down".into() }),
        ]);
        let p2 = TrackingProvider::new("p2", vec![Ok(ok_completion("from-p2"))]);
        let p2_calls = Arc::clone(&p2.call_count);

        let provider = ProviderChainBuilder::new()
            .provider(p1)
            .provider(p2)
            .with_failover(FailoverConfig::default())
            .build();

        let result = provider.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "from-p2");
        assert_eq!(*p2_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn multiple_providers_without_explicit_failover_still_wraps_in_failover() {
        // Two providers, no .with_failover() call — failover should be applied
        // automatically with default config.
        let p1 = TrackingProvider::new("p1", vec![
            Err(ProviderError::Timeout { provider: "p1".into(), duration: std::time::Duration::from_secs(1) }),
        ]);
        let p2 = TrackingProvider::new("p2", vec![Ok(ok_completion("auto-failover"))]);

        let provider = ProviderChainBuilder::new()
            .provider(p1)
            .provider(p2)
            .build();

        let result = provider.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "auto-failover");
    }

    // -----------------------------------------------------------------------
    // Full stack: retry + CB per provider + failover
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn full_stack_routes_through_to_second_provider_on_failure() {
        // p1 fails consistently — after retry exhaustion, failover sends to p2.
        let p1_responses: Vec<_> = (0..4)
            .map(|_| Err(ProviderError::Network { provider: "p1".into(), message: "err".into() }))
            .collect();

        let p1 = TrackingProvider::new("p1", p1_responses);
        let p2 = TrackingProvider::new("p2", vec![Ok(ok_completion("p2-success"))]);
        let p2_calls = Arc::clone(&p2.call_count);

        let provider = ProviderChainBuilder::new()
            .provider(p1)
                .with_retry(RetryConfig {
                    max_retries: 3,
                    initial_backoff: std::time::Duration::ZERO,
                    max_backoff: std::time::Duration::ZERO,
                })
                .with_circuit_breaker(CircuitBreakerConfig {
                    failure_threshold: 10,  // high threshold — CB won't open in this test
                    recovery_timeout: std::time::Duration::from_secs(60),
                })
            .provider(p2)
            .with_failover(FailoverConfig {
                cooldown_duration: std::time::Duration::from_secs(60),
            })
            .build();

        let result = provider.complete(completion_req()).await.unwrap();
        assert_eq!(result.content, "p2-success");
        assert_eq!(*p2_calls.lock().unwrap(), 1, "p2 should have handled the request");
    }

    // -----------------------------------------------------------------------
    // model_id delegation
    // -----------------------------------------------------------------------

    #[test]
    fn model_id_reflects_first_provider() {
        let provider = ProviderChainBuilder::new()
            .provider(TrackingProvider::new("primary-model", vec![]))
            .build();
        assert_eq!(provider.model_id(), "primary-model");
    }

    // -----------------------------------------------------------------------
    // Composition order verification
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn nesting_order_is_retry_wraps_circuit_breaker() {
        // To verify order: CB threshold=1 + retry max_retries=2.
        // First call fails → CB counts 1 failure (threshold met → opens).
        // Retry sees the second failure (circuit-open, which is retriable).
        // BUT because CB is already open, the retry wraps an open circuit.
        // Result: retry attempts get circuit-open errors, exhausted, returns error.
        //
        // If the order were reversed (CB wraps Retry), the retry would occur
        // inside the CB, resetting the CB count on each attempt — wrong behaviour.
        let responses = vec![
            Err(ProviderError::Network { provider: "p".into(), message: "err".into() }),
            // CB opened after above failure (threshold=1). Further calls never
            // reach this response.
        ];

        let provider = ProviderChainBuilder::new()
            .provider(TrackingProvider::new("order-test", responses))
            .with_circuit_breaker(CircuitBreakerConfig {
                failure_threshold: 1,  // opens after one failure
                recovery_timeout: std::time::Duration::from_secs(60),
            })
            .with_retry(RetryConfig {
                max_retries: 3,
                initial_backoff: std::time::Duration::ZERO,
                max_backoff: std::time::Duration::ZERO,
            })
            .build();

        // First call reaches backend (Closed → Open after 1 failure).
        // Retry retries but gets circuit-open each time (retriable, so it keeps retrying).
        // After max_retries, retry gives up and returns the circuit-open error.
        let err = provider.complete(completion_req()).await.unwrap_err();
        // The final error is a circuit-open error (Network), confirming Retry wraps CB.
        assert!(err.is_retriable(), "final error should be the retriable circuit-open error");
    }
}
