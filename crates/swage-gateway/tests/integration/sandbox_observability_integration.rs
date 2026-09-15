//! Cross-task integration tests for sandboxing, observability, and lifecycle management.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the gateway's sandbox routing, structured logging,
//! and lifecycle management are wired together.

#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::support::start_live_server;

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
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(StubProvider),
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let router = build_router(state);

    let (addr, _server) = start_live_server(router.clone()).await;

    (router, addr)
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
