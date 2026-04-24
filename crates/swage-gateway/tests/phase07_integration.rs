//! Cross-task integration tests for Phase 07 (Sandboxing, Observability & Production Readiness).
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the gateway's sandbox routing, structured logging,
//! and lifecycle management are wired together.
//!
//! Tests in this file are expected to FAIL until the phase 07 motes are
//! implemented. Each test documents why it currently fails.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use swage_config::ResolvedConfig;
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Mock provider — minimal for non-streaming tests
// ---------------------------------------------------------------------------

/// A provider that never streams — used for tests that exercise HTTP and
/// lifecycle behaviour without needing an LLM response.
struct StubProvider;

#[async_trait]
impl LlmProvider for StubProvider {
    fn model_id(&self) -> &str {
        "stub"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(CompletionResponse {
            content: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::empty())
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Start a test server with a custom ResolvedConfig.
///
/// Returns the router (for tower::oneshot HTTP) and the socket address
/// (for TCP connection checks).
async fn start_test_server_with_config(config: ResolvedConfig) -> (Router, SocketAddr) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let idle_timeout = config.lifecycle.idle_timeout;
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(StubProvider),
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        None,
    );
    // Clone state for the idle monitor; the router holds its own clone via with_state.
    let state_for_monitor = state.clone();
    let router = build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_router = router.clone();
    tokio::spawn(async move {
        axum::serve(listener, serve_router)
            .with_graceful_shutdown(swage_gateway::shutdown::idle_monitor(
                state_for_monitor,
                idle_timeout,
            ))
            .await
            .unwrap();
    });

    (router, addr)
}

/// Like `start_test_server_with_config` but also returns the `AppState`.
///
/// The returned state shares the same `Arc` internals (active_turns, activity_tracker,
/// etc.) as the running server, so tests can inspect and mutate live state — for
/// example, to inject fake active turns for deferral testing.
async fn start_test_server_with_state(config: ResolvedConfig) -> (Router, SocketAddr, AppState) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let idle_timeout = config.lifecycle.idle_timeout;
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(StubProvider),
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        None,
    );
    // Two extra clones: one for the idle monitor task, one for the test to hold.
    let state_for_monitor = state.clone();
    let state_for_test = state.clone();
    let router = build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_router = router.clone();
    tokio::spawn(async move {
        axum::serve(listener, serve_router)
            .with_graceful_shutdown(swage_gateway::shutdown::idle_monitor(
                state_for_monitor,
                idle_timeout,
            ))
            .await
            .unwrap();
    });

    (router, addr, state_for_test)
}

/// Send an HTTP request via tower::oneshot and return (status, JSON body).
async fn http(router: &Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Send an HTTP request and return the full response (for header inspection).
async fn http_response(router: &Router, method: &str, uri: &str) -> axum::http::Response<Body> {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

// ===========================================================================
// Health endpoint
// ===========================================================================

/// Health endpoint returns 200 with a status body.
///
/// Delivery Closure: Startup time verification surface
/// Spans: [seyr] activity tracker + health handler, [dxct] route wiring
///
/// Proves: the health check endpoint is reachable and returns a well-formed
/// response. External process managers (systemd, wrapper scripts) use this to
/// confirm the gateway is ready to serve.
///
/// Currently fails: the /api/health route does not exist (returns 404).
#[tokio::test(flavor = "multi_thread")]
async fn health_endpoint_returns_ok() {
    let (router, _addr) = start_test_server_with_config(ResolvedConfig::default()).await;

    let (status, body) = http(&router, "GET", "/api/health").await;

    assert_eq!(status, StatusCode::OK, "health endpoint should return 200");
    assert_eq!(
        body["status"], "ok",
        "health body should contain {{\"status\": \"ok\"}}, got: {body}"
    );
}

// ===========================================================================
// Request ID correlation middleware
// ===========================================================================

/// Every HTTP response carries an X-Request-Id header containing a UUID.
///
/// Spans: [dxct] request ID correlation middleware
///
/// Proves: the middleware generates a unique UUID per request and returns it
/// in the X-Request-Id response header for client-side correlation.
///
/// Currently fails: the request_id middleware does not exist.
#[tokio::test(flavor = "multi_thread")]
async fn request_id_header_present_on_health_response() {
    let (router, _addr) = start_test_server_with_config(ResolvedConfig::default()).await;

    let resp = http_response(&router, "GET", "/api/health").await;

    let request_id = resp
        .headers()
        .get("x-request-id")
        .expect("X-Request-Id header should be present on every response");

    // Must be a valid UUID (36-char hyphenated form).
    let id_str = request_id
        .to_str()
        .expect("X-Request-Id should be valid UTF-8");
    assert!(
        Uuid::parse_str(id_str).is_ok(),
        "X-Request-Id should be a valid UUID, got: {id_str}",
    );
}

/// Two concurrent requests receive distinct request IDs.
///
/// Spans: [dxct] request ID correlation middleware
///
/// Proves: the middleware generates a fresh UUID for each request, not a
/// shared global value. If both requests share the same ID, log entries
/// would be indistinguishable under load.
///
/// Currently fails: the request_id middleware does not exist.
#[tokio::test(flavor = "multi_thread")]
async fn request_ids_are_unique_across_requests() {
    let (router, _addr) = start_test_server_with_config(ResolvedConfig::default()).await;

    let resp_a = http_response(&router, "GET", "/api/health").await;
    let resp_b = http_response(&router, "GET", "/api/health").await;

    let id_a = resp_a
        .headers()
        .get("x-request-id")
        .expect("first response should have X-Request-Id")
        .to_str()
        .unwrap()
        .to_owned();

    let id_b = resp_b
        .headers()
        .get("x-request-id")
        .expect("second response should have X-Request-Id")
        .to_str()
        .unwrap()
        .to_owned();

    assert_ne!(
        id_a, id_b,
        "each request should receive a distinct request_id"
    );
}

// ===========================================================================
// Idle lifecycle
// ===========================================================================

/// The gateway shuts down after the configured idle timeout with no activity.
///
/// Delivery Closure: On-demand lifecycle — idle detection and shutdown
/// Spans: [seyr] activity tracker, [oe5k] activity recording, [ugev] idle monitor
///
/// Proves: the idle monitor checks elapsed time since last activity, and when
/// the idle_timeout is exceeded with no active turns, the server shuts down.
/// The TCP listener is closed so new connections are refused.
///
/// Currently fails: no idle monitor exists — the server stays alive indefinitely,
/// so the TCP connection after the idle timeout succeeds instead of being refused.
#[tokio::test(flavor = "multi_thread")]
async fn server_shuts_down_after_idle_timeout() {
    let config = ResolvedConfig {
        lifecycle: swage_config::LifecycleConfig { idle_timeout: 1 },
        ..ResolvedConfig::default()
    };

    let (router, addr) = start_test_server_with_config(config).await;

    // Verify the server is alive: existing endpoint returns 200.
    let (status, _) = http(&router, "GET", "/api/conversations").await;
    assert_eq!(status, StatusCode::OK, "server should be alive initially");

    // Wait longer than the idle timeout (1s) plus a generous buffer for
    // the monitor's check interval and shutdown sequence.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Try to open a TCP connection — should fail because the server shut down.
    let connect_result =
        tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect(addr)).await;

    match connect_result {
        Ok(Ok(_stream)) => {
            panic!(
                "server should have shut down after idle timeout, \
                 but TCP connection succeeded"
            );
        }
        Ok(Err(_)) => {
            // Connection refused — server shut down. This is the expected outcome.
        }
        Err(_) => {
            panic!("TCP connect timed out — server may be in a half-closed state");
        }
    }
}

/// An active turn defers idle shutdown until completion.
///
/// Delivery Closure: On-demand lifecycle — graceful shutdown deferral
/// Spans: [seyr] activity tracker, [oe5k] activity recording, [ugev] idle monitor,
///        turn processing (activity during tool execution)
///
/// Proves: when the idle timeout fires while a turn is in progress, the
/// gateway defers shutdown until the turn completes (or the grace period
/// expires). The server remains accessible during the deferral.
///
/// Currently fails: no idle monitor exists, so there is no deferral to test.
/// The test verifies that the gateway logs "idle timeout reached but N turn(s)
/// active, deferring" by checking that the server is still alive after the idle
/// timeout when a turn is active. Without the idle monitor, the server is alive
/// for the wrong reason (no shutdown attempted), so this test's structure is
/// validated by the idle_shutdown test above succeeding first.
#[tokio::test(flavor = "multi_thread")]
async fn active_turn_defers_idle_shutdown() {
    // Use a 2-second idle timeout so the grace period (also 2s) gives enough
    // room to assert liveness in the middle of the deferral window.
    //
    // Timeline:
    //   T=0s  — server starts; activity_tracker baseline = now
    //   T=2s  — idle threshold exceeded; fake turn in active_turns → deferral begins
    //   T=3s  — still within grace period (1s elapsed < 2s grace); server is alive ✓
    //   T=3s  — fake turn removed; active_turns is empty
    //   T=4s  — monitor tick: idle, no active turns → clean shutdown
    //   T=7s  — TCP connection refused; server has shut down ✓
    let config = ResolvedConfig {
        lifecycle: swage_config::LifecycleConfig { idle_timeout: 2 },
        ..ResolvedConfig::default()
    };

    let (_router, addr, state) = start_test_server_with_state(config).await;

    // Insert a fake active turn to trigger the deferral path.
    // The monitor checks `state.active_turns.lock().len()` — any non-empty entry
    // represents an in-flight turn and will defer shutdown.
    let fake_turn_id = Uuid::new_v4();
    let cancel_token = CancellationToken::new();
    state
        .active_turns
        .lock()
        .await
        .insert(fake_turn_id, cancel_token);

    // Wait 3 seconds: past the idle threshold (2s) but still within the grace
    // period (2s from when idle was first detected at ~T=2s → expires at ~T=4s).
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The server must still accept TCP connections — the active turn is deferring shutdown.
    let still_alive = tokio::net::TcpStream::connect(addr).await;
    assert!(
        still_alive.is_ok(),
        "server should remain alive while a turn is active (shutdown deferred)"
    );

    // Complete the turn — remove it from active_turns to signal it is done.
    state.active_turns.lock().await.remove(&fake_turn_id);

    // Allow the monitor enough time to detect no active turns and complete shutdown.
    // One monitor tick (1s) + port-close propagation + generous buffer = 4s.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // TCP connection must now be refused — the server has shut down.
    let connect_result =
        tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect(addr)).await;

    match connect_result {
        Ok(Ok(_stream)) => {
            panic!(
                "server should have shut down after the active turn completed, \
                 but TCP connection succeeded"
            );
        }
        Ok(Err(_)) => {
            // Connection refused — server shut down as expected.
        }
        Err(_) => {
            panic!("TCP connect timed out — server may be in a half-closed state");
        }
    }
}
