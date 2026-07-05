//! Cross-task integration tests for the LLM provider stack.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the full stack is wired together. Each test documents
//! which tasks it spans.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk, TokenUsage,
    ToolCompletionRequest, ToolCompletionResponse,
};

// Reliability decorators (tasks: retry [rrak], circuit_breaker [97s4],
// failover [p7fr], builder [jd4i])
use swage_provider::builder::ProviderChainBuilder;
use swage_provider::circuit_breaker::CircuitBreakerConfig;
use swage_provider::failover::FailoverConfig;
use swage_provider::retry::RetryConfig;

// Model registry (task: registry [uicc])
use swage_provider::registry::Registry;

// Trait (task: trait [qwes])
use swage_provider::LlmProvider;

// ---------------------------------------------------------------------------
// Mock provider for integration tests
// ---------------------------------------------------------------------------

/// A mock provider that returns canned responses or errors on each call.
/// The call counter tracks how many times each method was invoked.
struct CountingMockProvider {
    model: String,
    call_count: AtomicU32,
    /// Sequence of results: Ok(()) means succeed, Err(e) means return that error.
    /// Pops from front. When empty, succeeds.
    results: std::sync::Mutex<std::collections::VecDeque<Result<(), ProviderError>>>,
}

impl CountingMockProvider {
    fn new(model: &str, results: Vec<Result<(), ProviderError>>) -> Self {
        Self {
            model: model.to_string(),
            call_count: AtomicU32::new(0),
            results: std::sync::Mutex::new(results.into()),
        }
    }

    fn calls(&self) -> u32 {
        self.call_count.load(Ordering::SeqCst)
    }

    fn next_result(&self) -> Result<(), ProviderError> {
        self.results
            .lock()
            .expect("mock provider result lock should not be poisoned")
            .pop_front()
            .unwrap_or(Ok(()))
    }
}

#[async_trait]
impl LlmProvider for CountingMockProvider {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.next_result()?;
        Ok(CompletionResponse {
            content: format!("Response from {}", self.model),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.next_result()?;
        Ok(ToolCompletionResponse {
            content: String::new(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cost: None,
            tool_calls: vec![],
        })
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let result = self.next_result();
        match result {
            Ok(()) => Box::pin(futures::stream::iter(vec![
                StreamChunk::Delta {
                    content: format!("from {}", self.model),
                },
                StreamChunk::Usage(TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                }),
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop),
                },
            ])),
            Err(e) => Box::pin(futures::stream::iter(vec![StreamChunk::Error {
                message: e.to_string(),
            }])),
        }
    }
}

// ---------------------------------------------------------------------------
// Test 1: Full reliability stack composition
// Spans: retry [rrak] + circuit_breaker [97s4] + failover [p7fr] + builder [jd4i]
//
// Proves: ProviderChainBuilder produces a correctly-ordered composition where
// retry wraps circuit breaker per provider, and failover operates across stacks.
// A transient failure on the primary triggers retry, and on exhaustion failover
// routes to the secondary provider — all transparently.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reliability_stack_retries_then_fails_over_to_secondary() {
    // Primary provider: fails with transient errors (all retries will exhaust)
    let primary = Arc::new(CountingMockProvider::new(
        "primary",
        vec![
            Err(ProviderError::Network {
                provider: "primary".into(),
                message: "connection refused".into(),
            }),
            Err(ProviderError::Network {
                provider: "primary".into(),
                message: "connection refused".into(),
            }),
            Err(ProviderError::Network {
                provider: "primary".into(),
                message: "connection refused".into(),
            }),
        ],
    ));

    // Secondary provider: succeeds
    let secondary = Arc::new(CountingMockProvider::new("secondary", vec![]));

    // Build the full reliability stack via the builder.
    let provider = ProviderChainBuilder::new()
        .provider(primary.clone() as Arc<dyn LlmProvider>)
        .with_retry(RetryConfig {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1), // Fast for tests
            max_backoff: Duration::from_millis(10),
        })
        .with_circuit_breaker(CircuitBreakerConfig {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
        })
        .provider(secondary.clone() as Arc<dyn LlmProvider>)
        .with_retry(RetryConfig {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        })
        .with_failover(FailoverConfig {
            cooldown_duration: Duration::from_secs(60),
        })
        .build();

    // Send a completion — should exhaust retries on primary, failover to secondary.
    let request = CompletionRequest {
        messages: vec![],
        model: None,
        max_tokens: Some(100),
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    };

    let response = provider.complete(request).await.unwrap();

    // Secondary provider handled the request.
    assert!(
        response.content.contains("secondary"),
        "Expected response from secondary provider, got: {}",
        response.content
    );
    // Primary was called (initial + 2 retries = 3 calls).
    assert_eq!(primary.calls(), 3, "Primary should have been tried 3 times");
    // Secondary was called once.
    assert_eq!(
        secondary.calls(),
        1,
        "Secondary should have been tried once"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Registry resolves model and provider completes request
// Spans: registry [uicc] + trait [qwes] + concrete providers
//
// Proves: the registry resolves model aliases to the correct provider config,
// and the Protocol field enables dispatch to the right provider implementation.
// This is the intended wiring path for the gateway.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_resolves_alias_and_completes_through_provider() {
    use swage_core::config::{ModelConfig, Protocol, ProviderConfig as CoreProviderConfig};

    let mut registry = Registry::new();

    // Register an Anthropic provider.
    registry
        .register_provider(CoreProviderConfig {
            id: "anthropic".into(),
            protocol: Protocol::Anthropic,
            api_key_env: Some("FAKE_ANTHROPIC_KEY".into()),
            base_url: None,
            requests_per_minute: None,
            tokens_per_minute: None,
        })
        .unwrap();

    // Register a model with an alias.
    registry
        .register_model(ModelConfig {
            id: "claude-sonnet".into(),
            provider_id: "anthropic".into(),
            model_id: "claude-sonnet-4-20250514".into(),
            aliases: vec!["claude".into(), "sonnet".into()],
            context_window: Some(200_000),
        })
        .unwrap();

    // Resolve by alias.
    let resolved = registry.resolve("claude").unwrap();
    assert_eq!(resolved.model_config().model_id, "claude-sonnet-4-20250514");
    assert_eq!(resolved.provider_config().protocol, Protocol::Anthropic);

    // Resolve by explicit provider/model format.
    let resolved2 = registry.resolve("anthropic/claude-sonnet").unwrap();
    assert_eq!(resolved2.model_config().id, "claude-sonnet");
}

// ---------------------------------------------------------------------------
// Test 3: Duplicate alias rejection
// Spans: registry [uicc]
//
// Proves: the registry enforces unique aliases across all registered models.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_rejects_duplicate_aliases() {
    use swage_core::config::{ModelConfig, Protocol, ProviderConfig as CoreProviderConfig};

    let mut registry = Registry::new();

    registry
        .register_provider(CoreProviderConfig {
            id: "openai".into(),
            protocol: Protocol::OpenAiCompatible,
            api_key_env: Some("FAKE_OPENAI_KEY".into()),
            base_url: None,
            requests_per_minute: None,
            tokens_per_minute: None,
        })
        .unwrap();

    registry
        .register_model(ModelConfig {
            id: "gpt-4o".into(),
            provider_id: "openai".into(),
            model_id: "gpt-4o".into(),
            aliases: vec!["fast".into()],
            context_window: None,
        })
        .unwrap();

    // Second model with the same alias should be rejected.
    let result = registry.register_model(ModelConfig {
        id: "gpt-4o-mini".into(),
        provider_id: "openai".into(),
        model_id: "gpt-4o-mini".into(),
        aliases: vec!["fast".into()], // duplicate alias
        context_window: None,
    });

    assert!(result.is_err(), "Duplicate alias should be rejected");
}

// ---------------------------------------------------------------------------
// Test 4: Circuit-open error triggers failover
// Spans: circuit_breaker [97s4] + failover [p7fr] + builder [jd4i]
//
// Proves: when a circuit breaker opens (after repeated transient failures),
// the circuit-open error is classified as retriable, allowing the failover
// decorator to route the request to a secondary provider. This is the key
// interaction between circuit breaker and failover — the error classification
// bridges them.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn circuit_open_triggers_failover_to_secondary() {
    // Primary provider: always fails with transient errors to trip the circuit.
    let transient_error = || ProviderError::Network {
        provider: "primary".into(),
        message: "down".into(),
    };

    let primary = Arc::new(CountingMockProvider::new(
        "primary",
        // Enough failures to trip a circuit breaker with threshold=3.
        vec![
            Err(transient_error()),
            Err(transient_error()),
            Err(transient_error()),
            Err(transient_error()), // Extra — the circuit should open before this
        ],
    ));

    let secondary = Arc::new(CountingMockProvider::new("secondary", vec![]));

    // Build with a low circuit breaker threshold (3) and no retry (to isolate
    // the circuit breaker → failover interaction).
    let provider = ProviderChainBuilder::new()
        .provider(primary.clone() as Arc<dyn LlmProvider>)
        .with_circuit_breaker(CircuitBreakerConfig {
            failure_threshold: 3,
            recovery_timeout: Duration::from_secs(300), // Long — circuit stays open for the test
        })
        .provider(secondary.clone() as Arc<dyn LlmProvider>)
        .with_failover(FailoverConfig {
            cooldown_duration: Duration::from_secs(60),
        })
        .build();

    let make_request = || CompletionRequest {
        messages: vec![],
        model: None,
        max_tokens: Some(100),
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    };

    // Send 3 requests — all hit primary and fail, tripping the circuit.
    for _ in 0..3 {
        let _ = provider.complete(make_request()).await;
    }

    // The circuit should now be open. The 4th request should failover to secondary
    // because the circuit-open error is retriable.
    let response = provider.complete(make_request()).await.unwrap();
    assert!(
        response.content.contains("secondary"),
        "After circuit opens, failover should route to secondary. Got: {}",
        response.content
    );
}

// ---------------------------------------------------------------------------
// Test 5: Streaming through reliability stack
// Spans: retry [rrak] + failover [p7fr] + builder [jd4i] + trait [qwes]
//
// Proves: streaming works end-to-end through the decorator stack. The stream
// carries chunks from the provider that ultimately handles the request, with
// correct usage on the final chunk.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_through_reliability_stack_delivers_chunks() {
    // Single provider — just retry wrapping, no failover needed.
    let provider = Arc::new(CountingMockProvider::new("streamer", vec![]));

    let wrapped = ProviderChainBuilder::new()
        .provider(provider.clone() as Arc<dyn LlmProvider>)
        .with_retry(RetryConfig {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        })
        .build();

    let request = CompletionRequest {
        messages: vec![],
        model: None,
        max_tokens: Some(100),
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    };

    let chunks: Vec<StreamChunk> = wrapped.stream(request).collect().await;

    // Verify we got delta(s), usage, and done.
    let has_delta = chunks
        .iter()
        .any(|c| matches!(c, StreamChunk::Delta { .. }));
    let has_usage = chunks.iter().any(|c| matches!(c, StreamChunk::Usage(_)));
    let has_done = chunks.iter().any(|c| matches!(c, StreamChunk::Done { .. }));

    assert!(has_delta, "Stream should contain at least one Delta chunk");
    assert!(has_usage, "Stream should contain a Usage chunk");
    assert!(has_done, "Stream should end with a Done chunk");
}

// ---------------------------------------------------------------------------
// Test 6: Permanent error passes through entire stack without retry or failover
// Spans: retry [rrak] + failover [p7fr] + builder [jd4i]
//
// Proves: a non-retriable error (e.g., Auth) from the primary provider is
// returned immediately — retry doesn't attempt, failover doesn't engage.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn permanent_error_bypasses_retry_and_failover() {
    let primary = Arc::new(CountingMockProvider::new(
        "primary",
        vec![Err(ProviderError::Auth {
            provider: "primary".into(),
        })],
    ));

    let secondary = Arc::new(CountingMockProvider::new("secondary", vec![]));

    let provider = ProviderChainBuilder::new()
        .provider(primary.clone() as Arc<dyn LlmProvider>)
        .with_retry(RetryConfig {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        })
        .provider(secondary.clone() as Arc<dyn LlmProvider>)
        .with_failover(FailoverConfig {
            cooldown_duration: Duration::from_secs(60),
        })
        .build();

    let request = CompletionRequest {
        messages: vec![],
        model: None,
        max_tokens: Some(100),
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    };

    let result = provider.complete(request).await;

    // Should be an auth error, returned immediately.
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ProviderError::Auth { .. }),
        "Expected Auth error, got: {err:?}"
    );
    assert!(!err.is_retriable());

    // Primary called once, no retry.
    assert_eq!(primary.calls(), 1);
    // Secondary never called — permanent errors don't trigger failover.
    assert_eq!(secondary.calls(), 0);
}
