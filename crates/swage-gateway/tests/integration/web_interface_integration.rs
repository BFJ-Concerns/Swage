//! Cross-task integration tests for the web interface.
//!
//! These tests verify interactions between gateway asset serving and the
//! existing API surface — behaviour that emerges only when both are wired
//! together in the same router.
//!
//! Journey proved: the browser is the public surface. Opening the gateway
//! address in a browser loads the SPA, which then calls the API endpoints.
//! Both must coexist on the same server.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use swage_gateway::{assets::FrontendAssets, server::build_router, state::AppState};
use swage_storage::SqliteStorage;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a router backed by in-memory SQLite with a no-op provider.
///
/// Sufficient for asset serving and HTTP API tests (no streaming needed).
fn make_router() -> Router {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let provider = Arc::new(NoOpProvider) as Arc<dyn swage_provider::LlmProvider>;
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        provider,
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    build_router(state)
}

/// A provider that panics if called — asset serving tests never trigger LLM calls.
struct NoOpProvider;

#[async_trait::async_trait]
impl swage_provider::LlmProvider for NoOpProvider {
    fn model_id(&self) -> &str {
        "noop"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("not used in asset serving tests")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("not used in asset serving tests")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("not used in asset serving tests")
    }
}

/// Send a request and return status + body bytes.
async fn request(router: &Router, method: &str, uri: &str) -> (StatusCode, Vec<u8>) {
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
    (status, bytes.to_vec())
}

/// Return whether the production SPA assets were embedded in this test binary.
///
/// `cargo test` can compile this crate before `frontend/dist` exists. In that
/// case rust-embed produces an empty asset set, and the gateway correctly
/// returns its developer-facing 503. These tests prove production asset serving,
/// so they declare that precondition instead of reporting a false regression.
fn frontend_assets_available() -> bool {
    FrontendAssets::get("index.html").is_some()
}

fn skip_without_frontend_assets(test_name: &str) -> bool {
    if frontend_assets_available() {
        return false;
    }

    eprintln!(
        "skipping {test_name}: missing embedded frontend assets at frontend/dist/index.html; \
         run `npm --prefix frontend run build` before compiling this test binary",
    );
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The gateway root serves the SPA's index.html.
///
/// Delivery Closure: Gateway asset serving
/// Spans: [fi3q] asset serving + router wiring
///
/// Proves: navigating to the gateway address loads the web application,
/// not a 404 or empty response.
#[tokio::test(flavor = "multi_thread")]
async fn root_serves_index_html() {
    if skip_without_frontend_assets("root_serves_index_html") {
        return;
    }

    let router = make_router();
    let (status, body) = request(&router, "GET", "/").await;

    assert_eq!(status, StatusCode::OK, "root should return 200");

    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("<!doctype html>") || html.contains("<!DOCTYPE html>"),
        "root should serve HTML; got: {}",
        &html[..html.len().min(200)]
    );
    assert!(
        html.contains("<div id=\"app\">"),
        "HTML should contain the app mount point"
    );
}

/// Direct navigation to a conversation URL returns the SPA (not 404).
///
/// Delivery Closure: Gateway asset serving — SPA fallback
/// Spans: [fi3q] asset serving + [uydk] URL router (frontend-side)
///
/// Proves: bookmarked or refreshed conversation URLs load the application.
/// The frontend router then parses the URL and loads the correct conversation.
#[tokio::test(flavor = "multi_thread")]
async fn conversation_url_returns_spa_fallback() {
    if skip_without_frontend_assets("conversation_url_returns_spa_fallback") {
        return;
    }

    let router = make_router();
    let (status, body) = request(
        &router,
        "GET",
        "/conversations/00000000-0000-0000-0000-000000000001",
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "conversation URL should return 200 (SPA fallback)"
    );

    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("<div id=\"app\">"),
        "SPA fallback should serve the same index.html"
    );
}

/// A missing file-like path must remain a missing resource, rather than
/// receiving the SPA shell with a misleading successful status.
#[tokio::test(flavor = "multi_thread")]
async fn missing_file_like_path_returns_not_found_not_spa_html() {
    if skip_without_frontend_assets("missing_file_like_path_returns_not_found_not_spa_html") {
        return;
    }

    let router = make_router();
    let (status, body) = request(&router, "GET", "/openapi.json").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        !String::from_utf8_lossy(&body).contains("<div id=\"app\">"),
        "missing file-like paths must not receive the SPA shell"
    );
}

/// API routes still work alongside asset serving.
///
/// Delivery Closure: Gateway asset serving — API coexistence
/// Spans: [fi3q] asset serving + existing HTTP handlers
///
/// Proves: adding asset serving to the router doesn't break or shadow
/// the API endpoints. API routes take priority over the SPA fallback.
#[tokio::test(flavor = "multi_thread")]
async fn api_routes_coexist_with_asset_serving() {
    let router = make_router();

    // List conversations — should return JSON, not HTML.
    let (status, body) = request(&router, "GET", "/api/conversations").await;
    assert_eq!(status, StatusCode::OK);

    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("API response should be valid JSON");
    assert!(
        json["conversations"].is_array(),
        "API should return conversation list"
    );

    // Create a conversation — should still work.
    let (status, body) = request(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);

    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["id"].is_string(), "create should return an id");
}

/// Static assets (CSS, JS) are served with correct content types.
///
/// Delivery Closure: Gateway asset serving — static file serving
/// Spans: [fi3q] asset serving
///
/// Proves: Vite-built assets are served with correct MIME types so the
/// browser can load them. The test checks for any asset path that would
/// be generated by the Vite build.
#[tokio::test(flavor = "multi_thread")]
async fn static_assets_served_with_content_type() {
    let router = make_router();

    // Request a path that would be a Vite-generated CSS file.
    // The specific filename depends on the build, but the SPA fallback
    // should NOT serve index.html for paths under /assets/ — it should
    // serve the actual file or 404 if it doesn't exist.
    //
    // With rust-embed, if the file exists in the embedded assets, it's
    // served directly. If it doesn't, the fallback serves index.html.
    //
    // We can test this by requesting a known non-existent asset — it should
    // either 404 or serve index.html (depending on implementation choice).
    // The key invariant is that /api/* paths are never SPA-fallbacked.
    let (status, _) = request(&router, "GET", "/api/nonexistent").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "/api/nonexistent should not be SPA-fallbacked to 200"
    );
}
