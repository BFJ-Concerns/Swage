use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderValue, Request},
    middleware::{self, Next},
    response::Response,
    routing::{get, post, put},
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

/// Build the axum router for the full gateway API and frontend shell.
///
/// # Routes
///
/// The router handles the following endpoints (in priority order):
/// - `POST /api/projects` / `GET /api/projects` - project management
/// - `POST /api/teams` / topology-run routes - team and topology execution
/// - conversation, thread, turn, approval, and agent-message APIs
/// - `GET /api/conversations/:id/ws` - WebSocket upgrade for live turn events
/// - `GET /api/skills`, `POST /api/shutdown`, `GET /api/health`
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
        .route(
            "/api/projects/:project_id/teams",
            get(handlers::list_teams).post(handlers::create_team),
        )
        .route(
            "/api/teams/:id",
            get(handlers::get_team)
                .put(handlers::update_team)
                .delete(handlers::delete_team),
        )
        .route(
            "/api/teams/:id/runs",
            get(handlers::list_topology_runs).post(handlers::create_topology_run),
        )
        .route("/api/topology-runs/:id", get(handlers::get_topology_run))
        .route(
            "/api/conversations/:id/branches",
            get(handlers::list_conversation_branches).post(handlers::create_conversation_branch),
        )
        .route("/api/threads/:id", get(handlers::get_thread_detail))
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
        // Skills listing — resolved hierarchy for a project/agent combination.
        .route("/api/skills", get(handlers::list_skills))
        // External communication channel gateway.
        .route(
            "/api/channels/dev-dm/inbound",
            post(handlers::channel_dev_dm_inbound),
        )
        .route(
            "/api/channels/dev-thread/inbound",
            post(handlers::channel_dev_thread_inbound),
        )
        .route("/api/deliveries/log", get(handlers::delivery_log))
        // Turn context snapshots — assembly reports for past turns.
        .route("/api/turns/:id/context", get(handlers::get_turn_context))
        // Programmatic interface — async turn submission and polling.
        .route("/api/turns", post(handlers::submit_turn))
        .route("/api/turns/:id", get(handlers::get_turn))
        // Turn listing per conversation.
        .route("/api/conversations/:id/turns", get(handlers::list_turns))
        // Agent messaging — send and query inter-agent messages.
        .route("/api/messages", post(handlers::send_agent_message))
        .route("/api/messages/:id", get(handlers::get_message_status))
        // Durable scheduled jobs and recorded runs.
        .route(
            "/api/scheduled-jobs",
            get(handlers::list_scheduled_jobs).post(handlers::create_scheduled_job),
        )
        .route(
            "/api/scheduled-jobs/:id",
            put(handlers::update_scheduled_job).delete(handlers::delete_scheduled_job),
        )
        .route(
            "/api/scheduled-jobs/:id/pause",
            post(handlers::pause_scheduled_job),
        )
        .route(
            "/api/scheduled-jobs/:id/resume",
            post(handlers::resume_scheduled_job),
        )
        .route(
            "/api/scheduled-jobs/:id/trigger",
            post(handlers::trigger_scheduled_job),
        )
        .route(
            "/api/scheduled-jobs/:id/runs",
            get(handlers::list_scheduled_runs),
        )
        // Durable heartbeats and recorded runs.
        .route(
            "/api/heartbeats",
            get(handlers::list_heartbeats).post(handlers::create_heartbeat),
        )
        .route(
            "/api/heartbeats/:id",
            put(handlers::update_heartbeat).delete(handlers::delete_heartbeat),
        )
        .route(
            "/api/heartbeats/:id/enable",
            post(handlers::enable_heartbeat),
        )
        .route(
            "/api/heartbeats/:id/disable",
            post(handlers::disable_heartbeat),
        )
        .route(
            "/api/heartbeats/:id/runs",
            get(handlers::list_heartbeat_runs),
        )
        // Runtime presence and liveness.
        .route("/api/presence", get(handlers::presence_snapshot))
        .route(
            "/api/presence/machines/heartbeat",
            post(handlers::machine_heartbeat),
        )
        .route(
            "/api/presence/machines/:id",
            axum::routing::delete(handlers::disconnect_machine),
        )
        // Execution hosts and attached nodes.
        .route("/api/execution-hosts", get(handlers::list_execution_hosts))
        .route(
            "/api/execution-hosts/register",
            post(handlers::register_execution_host),
        )
        .route("/api/voice/capabilities", get(handlers::voice_capabilities))
        .route("/api/voice/transcriptions", post(handlers::transcribe_audio))
        .route("/api/voice/speech", post(handlers::synthesise_speech))
        // Approval decisions via REST (same path for TUI, web UI, external scripts).
        .route("/api/approvals", get(handlers::list_approvals))
        .route("/api/approvals/:id", post(handlers::decide_approval))
        // Graceful shutdown endpoint — localhost only, broadcasts Shutdown event.
        .route("/api/shutdown", post(handlers::shutdown))
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
