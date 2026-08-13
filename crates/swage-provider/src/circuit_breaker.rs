//! Circuit breaker decorator for LLM providers.
//!
//! Prevents repeated calls to a failing backend. When transient failures
//! exceed a configured threshold, the circuit opens and subsequent requests
//! fail immediately — giving the backend time to recover without being
//! hammered by retries.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Configuration for a `CircuitBreakerProvider`.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Consecutive transient failures before the circuit opens.
    pub failure_threshold: u32,
    /// How long the circuit stays Open before attempting a probe request.
    pub recovery_timeout: Duration,
}

impl CircuitBreakerConfig {
    /// Construct a validated config.
    ///
    /// Returns `Err` when `failure_threshold` is 0 — which would open the circuit
    /// after every single failure — or when `recovery_timeout` is zero — which
    /// disables the recovery window entirely.
    pub fn new(failure_threshold: u32, recovery_timeout: Duration) -> Result<Self, ProviderError> {
        let config = Self {
            failure_threshold,
            recovery_timeout,
        };
        config.validate()?;
        Ok(config)
    }

    /// Check for degenerate field values, returning a descriptive error if any are found.
    ///
    /// Useful when configs are deserialized from external sources rather than
    /// constructed via [`CircuitBreakerConfig::new`].
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.failure_threshold == 0 {
            return Err(ProviderError::InvalidConfig {
                message: "failure_threshold must be >= 1; 0 opens the circuit on every failure"
                    .into(),
            });
        }
        if self.recovery_timeout == Duration::ZERO {
            return Err(ProviderError::InvalidConfig {
                message: "recovery_timeout must be > zero; Duration::ZERO disables recovery".into(),
            });
        }
        Ok(())
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// Internal state of the circuit breaker.
///
/// Transitions:
/// - Closed → Open:    consecutive transient failures reach `failure_threshold`
/// - Open → HalfOpen:  `recovery_timeout` elapses; next `should_allow_request` call
/// - HalfOpen → Closed: probe request succeeds
/// - HalfOpen → Open:   probe request fails (transient error)
enum CircuitState {
    /// Normal operation — requests pass through to the backend.
    Closed { consecutive_failures: u32 },
    /// Failing fast — requests are rejected without contacting the backend.
    Open { opened_at: Instant },
    /// One probe request is in-flight. Additional requests are rejected.
    HalfOpen,
}

struct CircuitBreakerInner {
    state: CircuitState,
    config: CircuitBreakerConfig,
}

impl CircuitBreakerInner {
    fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: CircuitState::Closed {
                consecutive_failures: 0,
            },
            config,
        }
    }

    /// Determine whether a new request should be allowed through, advancing
    /// state if the recovery timeout has elapsed.
    ///
    /// Returns `true` when the circuit is Closed or when Open→HalfOpen
    /// transition fires (the first probe is allowed). Returns `false` when
    /// the circuit is Open (still within timeout) or already HalfOpen.
    fn should_allow_request(&mut self) -> bool {
        // Check Open → HalfOpen transition without holding a conflicting borrow.
        let should_transition = matches!(
            &self.state,
            CircuitState::Open { opened_at } if opened_at.elapsed() >= self.config.recovery_timeout
        );
        if should_transition {
            self.state = CircuitState::HalfOpen;
            return true;
        }
        matches!(self.state, CircuitState::Closed { .. })
    }

    /// Record a successful request outcome.
    ///
    /// Transitions any state to Closed with a reset failure counter.
    fn record_success(&mut self) {
        self.state = CircuitState::Closed {
            consecutive_failures: 0,
        };
    }

    /// Record a failed request outcome.
    ///
    /// Only transient errors affect circuit state — permanent errors pass
    /// through without changing the breaker.
    fn record_failure(&mut self, error: &ProviderError) {
        if !error.is_retriable() {
            return;
        }
        match &self.state {
            CircuitState::Closed {
                consecutive_failures,
            } => {
                let new_count = consecutive_failures + 1;
                if new_count >= self.config.failure_threshold {
                    self.state = CircuitState::Open {
                        opened_at: Instant::now(),
                    };
                } else {
                    self.state = CircuitState::Closed {
                        consecutive_failures: new_count,
                    };
                }
            }
            CircuitState::HalfOpen => {
                // Probe failed — re-open and restart the recovery countdown.
                self.state = CircuitState::Open {
                    opened_at: Instant::now(),
                };
            }
            // Open: shouldn't happen since Open requests never reach the backend.
            CircuitState::Open { .. } => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// LlmProvider decorator that prevents calls to a failing backend.
///
/// Wraps any `LlmProvider` with a three-state circuit breaker:
/// - **Closed** — requests pass through normally.
/// - **Open** — requests fail immediately with a retriable error so outer
///   retry or failover layers can route the request elsewhere.
/// - **HalfOpen** — one probe request is allowed through after the recovery
///   timeout. Success closes the circuit; failure re-opens it.
///
/// State is held behind `Arc<Mutex<...>>` so the provider can be shared
/// across async tasks.
pub struct CircuitBreakerProvider {
    inner: Box<dyn LlmProvider>,
    state: Arc<Mutex<CircuitBreakerInner>>,
}

impl CircuitBreakerProvider {
    /// Wrap a provider with the given circuit-breaker configuration.
    ///
    /// # Panics
    ///
    /// Panics if `config` has degenerate values — see [`CircuitBreakerConfig::validate`].
    /// These are programmer errors: a zero threshold or zero timeout indicate a
    /// misconfigured setup that should be caught at startup, not during request handling.
    pub fn new(inner: Box<dyn LlmProvider>, config: CircuitBreakerConfig) -> Self {
        config
            .validate()
            .expect("invalid CircuitBreakerConfig — check failure_threshold and recovery_timeout");
        Self {
            inner,
            state: Arc::new(Mutex::new(CircuitBreakerInner::new(config))),
        }
    }
}

/// Error returned when the circuit is Open.
///
/// Classified as retriable (`Network`) so outer retry and failover decorators
/// can route the request to a different backend rather than surfacing the
/// error directly to the caller.
fn circuit_open_error() -> ProviderError {
    ProviderError::Network {
        provider: "circuit-breaker".into(),
        message: "circuit is open — backend is not accepting requests".into(),
    }
}

#[async_trait]
impl LlmProvider for CircuitBreakerProvider {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        if !self
            .state
            .lock()
            .expect("circuit-breaker state lock should not be poisoned")
            .should_allow_request()
        {
            return Err(circuit_open_error());
        }
        match self.inner.complete(request).await {
            Ok(resp) => {
                self.state
                    .lock()
                    .expect("circuit-breaker state lock should not be poisoned")
                    .record_success();
                Ok(resp)
            }
            Err(e) => {
                self.state
                    .lock()
                    .expect("circuit-breaker state lock should not be poisoned")
                    .record_failure(&e);
                Err(e)
            }
        }
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        if !self
            .state
            .lock()
            .expect("circuit-breaker state lock should not be poisoned")
            .should_allow_request()
        {
            return Err(circuit_open_error());
        }
        match self.inner.complete_with_tools(request).await {
            Ok(resp) => {
                self.state
                    .lock()
                    .expect("circuit-breaker state lock should not be poisoned")
                    .record_success();
                Ok(resp)
            }
            Err(e) => {
                self.state
                    .lock()
                    .expect("circuit-breaker state lock should not be poisoned")
                    .record_failure(&e);
                Err(e)
            }
        }
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        // Circuit state is checked at request time — already-in-flight streams
        // continue if the circuit opens mid-stream.
        if !self
            .state
            .lock()
            .expect("circuit-breaker state lock should not be poisoned")
            .should_allow_request()
        {
            return Box::pin(futures::stream::once(async {
                StreamChunk::Error {
                    message: "circuit is open — backend is not accepting requests".into(),
                }
            }));
        }
        let stream = self.inner.stream(request);
        let state = Arc::clone(&self.state);

        Box::pin(futures::stream::unfold(
            (stream, state, false),
            |(mut stream, state, is_recorded)| async move {
                match stream.next().await {
                    Some(chunk) => {
                        let mut next_is_recorded = is_recorded;
                        if !is_recorded {
                            match &chunk {
                                StreamChunk::Error { message } => {
                                    state
                                        .lock()
                                        .expect("circuit-breaker state lock should not be poisoned")
                                        .record_failure(&ProviderError::StreamError(
                                            message.clone(),
                                        ));
                                    next_is_recorded = true;
                                }
                                StreamChunk::ProviderAnomaly { message } => {
                                    state
                                        .lock()
                                        .expect("circuit-breaker state lock should not be poisoned")
                                        .record_failure(&ProviderError::ProviderAnomaly(
                                            message.clone(),
                                        ));
                                    next_is_recorded = true;
                                }
                                StreamChunk::Done { .. } => {
                                    state
                                        .lock()
                                        .expect("circuit-breaker state lock should not be poisoned")
                                        .record_success();
                                    next_is_recorded = true;
                                }
                                StreamChunk::Delta { .. }
                                | StreamChunk::Reasoning { .. }
                                | StreamChunk::Usage(_)
                                | StreamChunk::ProviderResponseWitness(_)
                                | StreamChunk::ProviderRetryStarted { .. }
                                | StreamChunk::ProviderRetrySettled { .. }
                                | StreamChunk::ToolUse { .. } => {}
                            }
                        }
                        Some((chunk, (stream, state, next_is_recorded)))
                    }
                    None => {
                        if !is_recorded {
                            state
                                .lock()
                                .expect("circuit-breaker state lock should not be poisoned")
                                .record_success();
                        }
                        None
                    }
                }
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::Stream;
    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse,
    };

    use super::*;
    use crate::provider::LlmProvider;

    // -----------------------------------------------------------------------
    // Mock provider
    // -----------------------------------------------------------------------

    /// Configurable mock that consumes a queue of responses on each complete() call.
    struct MockProvider {
        model: String,
        responses: Arc<Mutex<VecDeque<Result<CompletionResponse, ProviderError>>>>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockProvider {
        fn new(responses: Vec<Result<CompletionResponse, ProviderError>>) -> Self {
            Self {
                model: "mock-model".into(),
                responses: Arc::new(Mutex::new(responses.into())),
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn model_id(&self) -> &str {
            &self.model
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(ok_response()))
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not needed for circuit breaker tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    struct MockStreamProvider {
        model: String,
        streams: Arc<Mutex<VecDeque<Vec<StreamChunk>>>>,
        stream_call_count: Arc<AtomicUsize>,
    }

    impl MockStreamProvider {
        fn new(streams: Vec<Vec<StreamChunk>>) -> Self {
            Self {
                model: "mock-stream-model".into(),
                streams: Arc::new(Mutex::new(streams.into())),
                stream_call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockStreamProvider {
        fn model_id(&self) -> &str {
            &self.model
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not needed for circuit breaker stream tests")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not needed for circuit breaker stream tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            self.stream_call_count.fetch_add(1, Ordering::SeqCst);
            let chunks = self.streams.lock().unwrap().pop_front().unwrap_or_default();
            Box::pin(futures::stream::iter(chunks))
        }
    }

    // -----------------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------------

    fn ok_response() -> CompletionResponse {
        CompletionResponse {
            content: "ok".into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cost: None,
        }
    }

    fn transient_error() -> ProviderError {
        // Network is retriable — the kind of error that trips the circuit.
        ProviderError::Network {
            provider: "test".into(),
            message: "connection refused".into(),
        }
    }

    fn permanent_error() -> ProviderError {
        // Auth is not retriable — must never trip the circuit.
        ProviderError::Auth {
            provider: "test".into(),
        }
    }

    fn test_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        }
    }

    fn tight_config(threshold: u32) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold: threshold,
            recovery_timeout: Duration::from_millis(1),
        }
    }

    // -----------------------------------------------------------------------
    // Tests: Closed state
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn closed_state_passes_requests_through() {
        let mock = MockProvider::new(vec![Ok(ok_response())]);
        let call_count = mock.call_count.clone();
        let breaker = CircuitBreakerProvider::new(Box::new(mock), CircuitBreakerConfig::default());

        assert!(breaker.complete(test_request()).await.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn success_resets_failure_counter() {
        // Two failures below threshold, then a success — the counter resets,
        // so it takes another three failures to trip the circuit (not one).
        let threshold = 3u32;
        let mock = MockProvider::new(vec![
            Err(transient_error()), // failure 1
            Err(transient_error()), // failure 2 (below threshold, still Closed)
            Ok(ok_response()),      // success — counter resets
            Err(transient_error()), // failure 1 (fresh counter)
            Err(transient_error()), // failure 2
            Err(transient_error()), // failure 3 — trips circuit
            Ok(ok_response()),      // never reached
        ]);
        let call_count = mock.call_count.clone();
        let breaker = CircuitBreakerProvider::new(Box::new(mock), tight_config(threshold));
        let req = test_request();

        // Two failures and a reset.
        let _ = breaker.complete(req.clone()).await;
        let _ = breaker.complete(req.clone()).await;
        assert!(breaker.complete(req.clone()).await.is_ok());

        // Another three failures to trip the freshly-reset circuit.
        for _ in 0..3 {
            let _ = breaker.complete(req.clone()).await;
        }

        // Circuit is now Open — next call does not reach backend.
        let err = breaker.complete(req.clone()).await.unwrap_err();
        assert!(err.is_retriable(), "circuit-open error must be retriable");
        // Six backend calls: 2 + 1 + 3. The 7th (circuit-open) never reaches backend.
        assert_eq!(call_count.load(Ordering::SeqCst), 6);
    }

    // -----------------------------------------------------------------------
    // Tests: Closed → Open transition
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn consecutive_transient_failures_open_circuit() {
        let threshold = 3u32;
        let mock = MockProvider::new(vec![
            Err(transient_error()),
            Err(transient_error()),
            Err(transient_error()),
            Ok(ok_response()), // would be the 4th call — must NOT be reached
        ]);
        let call_count = mock.call_count.clone();
        let breaker = CircuitBreakerProvider::new(
            Box::new(mock),
            CircuitBreakerConfig {
                failure_threshold: threshold,
                recovery_timeout: Duration::from_secs(60),
            },
        );
        let req = test_request();

        // Trip the circuit.
        for _ in 0..threshold {
            let _ = breaker.complete(req.clone()).await;
        }

        // Circuit is now Open — next call should not reach the backend.
        let result = breaker.complete(req.clone()).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().is_retriable(),
            "circuit-open error must be retriable"
        );
        // Exactly three backend calls — not four.
        assert_eq!(call_count.load(Ordering::SeqCst), threshold as usize);
    }

    #[tokio::test]
    async fn open_circuit_does_not_call_backend() {
        // After tripping, many calls should all fast-fail without touching backend.
        let threshold = 3u32;
        let mock = MockProvider::new(
            (0..threshold)
                .map(|_| Err(transient_error()))
                .chain(std::iter::repeat_with(|| Ok(ok_response())).take(20))
                .collect(),
        );
        let call_count = mock.call_count.clone();
        let breaker = CircuitBreakerProvider::new(
            Box::new(mock),
            CircuitBreakerConfig {
                failure_threshold: threshold,
                recovery_timeout: Duration::from_secs(60),
            },
        );
        let req = test_request();

        // Trip.
        for _ in 0..threshold {
            let _ = breaker.complete(req.clone()).await;
        }

        // Ten calls while Open.
        for _ in 0..10 {
            let err = breaker.complete(req.clone()).await.unwrap_err();
            assert!(err.is_retriable());
        }

        // Backend called exactly threshold times.
        assert_eq!(call_count.load(Ordering::SeqCst), threshold as usize);
    }

    // -----------------------------------------------------------------------
    // Tests: Open → HalfOpen → Closed
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn recovery_timeout_allows_probe_and_success_closes_circuit() {
        let mock = MockProvider::new(vec![
            Err(transient_error()), // trip 1
            Err(transient_error()), // trip 2
            Err(transient_error()), // trip 3 → Open
            Ok(ok_response()),      // probe → HalfOpen → Closed
            Ok(ok_response()),      // subsequent request in Closed state
        ]);
        let call_count = mock.call_count.clone();
        let breaker = CircuitBreakerProvider::new(Box::new(mock), tight_config(3));
        let req = test_request();

        // Trip circuit.
        for _ in 0..3 {
            let _ = breaker.complete(req.clone()).await;
        }
        assert_eq!(call_count.load(Ordering::SeqCst), 3);

        // Wait for recovery timeout (1 ms).
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Probe — allowed through HalfOpen, succeeds → Closed.
        assert!(breaker.complete(req.clone()).await.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 4);

        // Subsequent call goes through normally (Closed).
        assert!(breaker.complete(req.clone()).await.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn probe_failure_reopens_circuit() {
        let mock = MockProvider::new(vec![
            Err(transient_error()), // trip 1
            Err(transient_error()), // trip 2
            Err(transient_error()), // trip 3 → Open
            Err(transient_error()), // probe fails → Open again
        ]);
        let call_count = mock.call_count.clone();
        let breaker = CircuitBreakerProvider::new(Box::new(mock), tight_config(3));
        let req = test_request();

        // Trip.
        for _ in 0..3 {
            let _ = breaker.complete(req.clone()).await;
        }

        // Wait for recovery timeout.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Probe fails — circuit re-opens.
        let _ = breaker.complete(req.clone()).await;
        assert_eq!(call_count.load(Ordering::SeqCst), 4);

        // Next call immediately — circuit is Open again, backend not called.
        let err = breaker.complete(req.clone()).await.unwrap_err();
        assert!(err.is_retriable());
        assert_eq!(call_count.load(Ordering::SeqCst), 4);
    }

    // -----------------------------------------------------------------------
    // Tests: Permanent errors
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn permanent_errors_do_not_trip_circuit() {
        let threshold = 3u32;
        // More permanent errors than the threshold — circuit must stay Closed.
        let mock = MockProvider::new((0..10).map(|_| Err(permanent_error())).collect());
        let call_count = mock.call_count.clone();
        let breaker = CircuitBreakerProvider::new(
            Box::new(mock),
            CircuitBreakerConfig {
                failure_threshold: threshold,
                recovery_timeout: Duration::from_secs(30),
            },
        );
        let req = test_request();

        // Six requests — all permanent errors, all reach the backend.
        for i in 0..6usize {
            let result = breaker.complete(req.clone()).await;
            assert!(result.is_err(), "error should propagate");
            assert!(
                !result.unwrap_err().is_retriable(),
                "auth error is permanent"
            );
            assert_eq!(
                call_count.load(Ordering::SeqCst),
                i + 1,
                "backend called every time"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Tests: streaming outcomes
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stream_errors_open_circuit_after_threshold() {
        let threshold = 2u32;
        let mock = MockStreamProvider::new(vec![
            vec![StreamChunk::Error {
                message: "stream lost".into(),
            }],
            vec![StreamChunk::Error {
                message: "stream lost again".into(),
            }],
            vec![StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            }],
        ]);
        let stream_call_count = mock.stream_call_count.clone();
        let breaker = CircuitBreakerProvider::new(
            Box::new(mock),
            CircuitBreakerConfig {
                failure_threshold: threshold,
                recovery_timeout: Duration::from_secs(60),
            },
        );
        let req = test_request();

        for _ in 0..threshold {
            let chunks: Vec<StreamChunk> = breaker.stream(req.clone()).collect().await;
            assert!(matches!(chunks.as_slice(), [StreamChunk::Error { .. }]));
        }

        let chunks: Vec<StreamChunk> = breaker.stream(req.clone()).collect().await;
        assert!(
            matches!(chunks.as_slice(), [StreamChunk::Error { message }] if message.contains("circuit is open"))
        );
        assert_eq!(stream_call_count.load(Ordering::SeqCst), threshold as usize);
    }

    #[tokio::test]
    async fn stream_success_after_recovery_timeout_closes_circuit() {
        let mock = MockStreamProvider::new(vec![
            vec![StreamChunk::Error {
                message: "stream lost".into(),
            }],
            vec![StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            }],
            vec![StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            }],
        ]);
        let stream_call_count = mock.stream_call_count.clone();
        let breaker = CircuitBreakerProvider::new(Box::new(mock), tight_config(1));
        let req = test_request();

        let chunks: Vec<StreamChunk> = breaker.stream(req.clone()).collect().await;
        assert!(matches!(chunks.as_slice(), [StreamChunk::Error { .. }]));

        tokio::time::sleep(Duration::from_millis(5)).await;

        let chunks: Vec<StreamChunk> = breaker.stream(req.clone()).collect().await;
        assert!(matches!(chunks.as_slice(), [StreamChunk::Done { .. }]));

        let chunks: Vec<StreamChunk> = breaker.stream(req.clone()).collect().await;
        assert!(matches!(chunks.as_slice(), [StreamChunk::Done { .. }]));
        assert_eq!(stream_call_count.load(Ordering::SeqCst), 3);
    }

    // -----------------------------------------------------------------------
    // Tests: model_id delegation
    // -----------------------------------------------------------------------

    #[test]
    fn model_id_delegates_to_inner() {
        let mock = MockProvider::new(vec![]);
        let breaker = CircuitBreakerProvider::new(Box::new(mock), CircuitBreakerConfig::default());
        assert_eq!(breaker.model_id(), "mock-model");
    }

    // -----------------------------------------------------------------------
    // Tests: config validation
    // -----------------------------------------------------------------------

    #[test]
    fn config_new_rejects_zero_failure_threshold() {
        let err = CircuitBreakerConfig::new(0, Duration::from_secs(30)).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidConfig { .. }));
    }

    #[test]
    fn config_new_rejects_zero_recovery_timeout() {
        let err = CircuitBreakerConfig::new(5, Duration::ZERO).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidConfig { .. }));
    }

    #[test]
    fn config_new_accepts_minimal_valid_values() {
        // threshold=1 and 1 ns timeout are the smallest valid values.
        assert!(CircuitBreakerConfig::new(1, Duration::from_nanos(1)).is_ok());
    }

    #[test]
    fn config_validate_rejects_zero_threshold() {
        let config = CircuitBreakerConfig {
            failure_threshold: 0,
            recovery_timeout: Duration::from_secs(1),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_validate_rejects_zero_timeout() {
        let config = CircuitBreakerConfig {
            failure_threshold: 5,
            recovery_timeout: Duration::ZERO,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    #[should_panic(expected = "invalid CircuitBreakerConfig")]
    fn provider_new_panics_on_zero_threshold() {
        let mock = MockProvider::new(vec![]);
        CircuitBreakerProvider::new(
            Box::new(mock),
            CircuitBreakerConfig {
                failure_threshold: 0,
                recovery_timeout: Duration::from_secs(30),
            },
        );
    }

    #[test]
    #[should_panic(expected = "invalid CircuitBreakerConfig")]
    fn provider_new_panics_on_zero_recovery_timeout() {
        let mock = MockProvider::new(vec![]);
        CircuitBreakerProvider::new(
            Box::new(mock),
            CircuitBreakerConfig {
                failure_threshold: 5,
                recovery_timeout: Duration::ZERO,
            },
        );
    }
}
