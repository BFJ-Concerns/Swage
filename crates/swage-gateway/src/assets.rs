//! Embedded frontend asset serving for production builds.
//!
//! In production, the compiled Svelte SPA is embedded in the binary via
//! `rust-embed`. This module provides the axum route handler that serves
//! those assets, with SPA fallback so that direct navigation to any UI
//! path (e.g. a conversation URL on page refresh) loads the application.

use axum::{
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

/// Compiled frontend assets embedded at build time.
///
/// The path is relative to the crate's Cargo.toml, pointing at the Vite
/// build output directory. If the directory doesn't exist at compile time,
/// rust-embed produces an empty asset set (the dev-mode fallback handles this).
#[derive(Embed)]
#[folder = "../../frontend/dist"]
pub struct FrontendAssets;

/// Axum fallback handler: serves embedded frontend assets with SPA fallback.
///
/// Route priority (highest to lowest):
/// 1. `/api/*` paths — registered before this fallback, never reach here
///    unless the endpoint genuinely doesn't exist, in which case: 404.
/// 2. Exact asset match — served with Content-Type and Cache-Control derived
///    from the file extension and path.
/// 3. SPA fallback — any other unrecognised path serves `index.html` so that
///    direct navigation (e.g. refreshing /conversations/:id) loads the app.
pub async fn serve_frontend(uri: Uri) -> Response {
    let path = uri.path();

    // API paths that reach the fallback are genuinely missing routes.
    // Never substitute index.html — that would mask API 404s as 200 HTML.
    if path.starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Strip the leading slash for rust-embed key lookup (keys use bare paths).
    let asset_key = path.trim_start_matches('/');

    // Try to serve the requested file directly.
    if let Some(file) = FrontendAssets::get(asset_key) {
        return serve_static_file(asset_key, file);
    }

    // SPA fallback: serve index.html for any path that doesn't match a file.
    // This includes the root path ("") and any URL the frontend router owns
    // (e.g. /conversations/:id on page refresh).
    if let Some(file) = FrontendAssets::get("index.html") {
        return serve_index(file);
    }

    // Dev mode: the frontend hasn't been built yet. rust-embed produces an
    // empty asset set when dist/ is missing — requests fall through to here.
    dev_fallback()
}

/// Serve a static embedded file with Content-Type and Cache-Control headers.
fn serve_static_file(path: &str, file: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    // Assets under /assets/ are content-addressed (Vite appends a hash to
    // the filename), so they can safely be cached forever. Everything else —
    // primarily index.html itself — must revalidate to pick up new deploys.
    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::from(file.data.into_owned()))
        .unwrap()
}

/// Serve index.html with no-cache headers.
///
/// Used both for direct requests to the SPA root and as the fallback for any
/// path that the frontend router handles (URL-based navigation, page refresh).
fn serve_index(file: rust_embed::EmbeddedFile) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(file.data.into_owned()))
        .unwrap()
}

/// Response returned when no embedded assets exist (frontend not yet built).
///
/// Points the developer at the Vite dev server rather than silently failing.
fn dev_fallback() -> Response {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(
            "Frontend not built. \
             Run 'npm run build' in the frontend/ directory, \
             or start the Vite dev server at http://localhost:5173 during development.",
        ))
        .unwrap()
}
