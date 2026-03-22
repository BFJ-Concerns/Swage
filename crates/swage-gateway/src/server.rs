use axum::{
    body::Body,
    extract::State,
    http::{HeaderValue, Request},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;
use tracing::Instrument;
use uuid::Uuid;

use crate::{assets, handlers, state::AppState, websocket};

// ---------------------------------------------------------------------------
// Request ID correlation middleware
// ---------------------------------------------------------------------------

/// Axum middleware that assigns a UUID to every inbound HTTP request.
///
/// The UUID is:
/// 1. Recorded in a `request_id` field on the tracing span — all log entries
///    emitted during the request (handlers, services, providers) inherit it,
///    making a single request's log trail greppable by ID.
/// 2. Returned as an `X-Request-Id` response header so clients can correlate
///    their requests with server-side log entries.
async fn request_id_middleware(request: Request<Body>, next: Next) -> Response {
    let request_id = Uuid::new_v4();
    let id_str = request_id.to_string();

    // Run the rest of the request pipeline inside a span that carries the
    // request_id. Every tracing event within next.run() inherits this field.
    let mut response = async move { next.run(request).await }
        .instrument(tracing::info_span!("http_request", request_id = %request_id))
        .await;

    // Echo the ID back so clients can tie their request to server logs.
    if let Ok(header_value) = HeaderValue::from_str(&id_str) {
        response.headers_mut().insert("x-request-id", header_value);
    }

    response
}


// ---------------------------------------------------------------------------
// Activity recording middleware
// ---------------------------------------------------------------------------

/// Axum middleware that records gateway activity on every inbound HTTP request.
///
/// Resets the idle timer so that an active HTTP client keeps the gateway alive.
/// Called before the request reaches any handler, ensuring even rejected or
/// errored requests count as activity (the gateway is clearly not idle).
///
/// Health probes are deliberately excluded: a process manager polling
/// `/api/health` on a short interval would otherwise continuously reset
/// `last_activity`, preventing the idle monitor from ever triggering.
async fn activity_recording_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.uri().path() != "/api/health" {
        state.activity_tracker.record_activity();
    }
    next.run(request).await
}

/// Build the axum router with all conversation routes and tracing middleware.
///
/// # Routes
///
/// The router handles the following endpoints (in priority order):
/// - `POST /api/conversations` - create a new conversation
/// - `GET /api/conversations` - list all conversations
/// - `GET /api/conversations/:id` - get a conversation with message history
/// - `DELETE /api/conversations/:id` - delete a conversation
/// - `GET /api/conversations/:id/ws` - WebSocket upgrade for a conversation
/// - `*` - embedded frontend SPA (with fallback)
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Project CRUD
        .route(
            "/api/projects",
            post(handlers::create_project).get(handlers::list_projects),
        )
        .route(
            "/api/projects/:id",
            get(handlers::get_project)
                .put(handlers::update_project)
                .delete(handlers::delete_project),
        )
        // Conversation CRUD
        .route("/api/conversations", post(handlers::create_conversation))
        .route("/api/conversations", get(handlers::list_conversations))
        .route(
            "/api/conversations/:id",
            get(handlers::get_conversation).delete(handlers::delete_conversation),
        )
        .route("/api/conversations/:id/ws", get(websocket::ws_handler))
        // Project-scoped session routes. These supplement /api/conversations
        // and allow the frontend to fetch/create sessions per project.
        .route(
            "/api/projects/:project_id/sessions",
            get(handlers::list_project_sessions).post(handlers::create_project_session),
        )
        // Health check — lightweight, no DB or provider calls.
        .route("/api/health", get(handlers::health))
        // Serve the compiled frontend SPA for all non-API paths.
        // API routes above take priority - the fallback only fires for
        // paths that don't match any registered route.
        .fallback(assets::serve_frontend)
        // Emit a tracing span for every inbound request.
        .layer(TraceLayer::new_for_http())
        // Assign a UUID request_id to each request; the ID is inherited by all
        // log entries within that request and returned as X-Request-Id.
        // Added last so it is the outermost layer (processes requests first).
        .layer(middleware::from_fn(request_id_middleware))
        // Record activity on every inbound request so the idle monitor knows
        // the gateway is in use. Applied before request_id so even early-exit
        // paths reset the timer.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            activity_recording_middleware,
        ))
        .with_state(state)
}
