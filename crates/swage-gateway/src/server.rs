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

use crate::{assets, handlers, knowledge_mirror, state::AppState, websocket, workflow};

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
/// Health probes are deliberately excluded so process-manager polling does not
/// make the operator view look busier than it is.
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
/// - `POST /api/spaces` / `GET /api/spaces` - space management
/// - conversation, thread, turn, approval, and agent-message APIs
/// - `GET /api/conversations/{id}/ws` - WebSocket upgrade for live turn events
/// - `GET /api/skills`, `POST /api/shutdown`, `GET /api/health`
/// - `*` - embedded frontend SPA (with fallback)
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Space CRUD
        .route(
            "/api/spaces",
            post(handlers::create_space).get(handlers::list_spaces),
        )
        .route(
            "/api/spaces/{id}",
            get(handlers::get_space)
                .put(handlers::update_space)
                .delete(handlers::delete_space),
        )
        .route(
            "/api/spaces/{id}/workspaces",
            get(handlers::list_space_workspaces).post(handlers::attach_space_workspace),
        )
        .route(
            "/api/spaces/{space_id}/workspaces/{workspace_id}",
            axum::routing::delete(handlers::detach_space_workspace),
        )
        .route(
            "/api/spaces/{space_id}/workspaces/{workspace_id}/trust",
            put(handlers::update_space_workspace_trust),
        )
        .route(
            "/api/spaces/{id}/knowledge-mirror",
            post(knowledge_mirror::regenerate),
        )
        .route(
            "/api/spaces/{id}/workflow-scripts",
            get(workflow::list_scripts).post(workflow::create_script),
        )
        .route(
            "/api/workflow-scripts/{id}",
            get(workflow::get_script).put(workflow::update_script),
        )
        .route(
            "/api/workflow-scripts/{id}/runs",
            post(workflow::start_saved_run),
        )
        .route(
            "/api/spaces/{id}/workflow-runs",
            get(workflow::list_runs).post(workflow::start_inline_run),
        )
        .route("/api/workflow-runs/{id}", get(workflow::get_run))
        .route("/api/workflow-runs/{id}/cancel", post(workflow::cancel_run))
        .route("/api/workflow-runs/{id}/resume", post(workflow::resume_run))
        .route(
            "/api/conversations/{id}/branches",
            get(handlers::list_conversation_branches).post(handlers::create_conversation_branch),
        )
        .route("/api/threads/{id}", get(handlers::get_thread_detail))
        // Conversation CRUD
        .route("/api/conversations", post(handlers::create_conversation))
        .route("/api/conversations", get(handlers::list_conversations))
        .route(
            "/api/conversations/{id}",
            get(handlers::get_conversation).delete(handlers::delete_conversation),
        )
        .route("/api/conversations/{id}/ws", get(websocket::ws_handler))
        .route(
            "/api/sessions/{id}/attachments",
            get(handlers::list_session_attachments),
        )
        .route(
            "/api/sessions/{id}/materials",
            get(handlers::list_session_materials),
        )
        // Space-scoped session routes. These supplement /api/conversations
        // and allow the frontend to fetch/create sessions per space.
        .route(
            "/api/spaces/{space_id}/sessions",
            get(handlers::list_space_sessions).post(handlers::create_space_session),
        )
        .route(
            "/api/spaces/{id}/attachments",
            get(handlers::list_space_attachments),
        )
        .route(
            "/api/spaces/{id}/materials",
            get(handlers::list_space_materials),
        )
        // Skills listing — resolved hierarchy for a space/agent combination.
        .route("/api/skills", get(handlers::list_skills))
        .route("/api/capabilities", get(handlers::workflow_capabilities))
        // External communication channel gateway.
        .route(
            "/api/channels/dev-dm/inbound",
            post(handlers::channel_dev_dm_inbound),
        )
        .route(
            "/api/channels/dev-thread/inbound",
            post(handlers::channel_dev_thread_inbound),
        )
        .route("/api/channels", get(handlers::list_channel_statuses))
        .route(
            "/api/onboarding/channels",
            get(handlers::list_channel_links).post(handlers::create_channel_link),
        )
        .route(
            "/api/onboarding/channels/link-codes",
            post(handlers::create_channel_link_code),
        )
        .route(
            "/api/onboarding/devices/pairing-codes",
            post(handlers::create_device_pairing_code),
        )
        .route(
            "/api/onboarding/devices/redeem",
            post(handlers::redeem_device_pairing_code),
        )
        .route(
            "/api/recovery/export",
            get(handlers::export_gateway_recovery),
        )
        .route(
            "/api/recovery/exports",
            get(handlers::list_gateway_recovery_exports),
        )
        .route(
            "/api/recovery/exports/{id}",
            get(handlers::get_gateway_recovery_export),
        )
        .route(
            "/api/recovery/import",
            post(handlers::import_gateway_recovery),
        )
        .route("/api/deliveries/log", get(handlers::delivery_log))
        .route("/api/operator/overview", get(handlers::operator_overview))
        .route(
            "/api/operator/agent-definitions",
            get(handlers::list_agent_definitions).post(handlers::create_agent_definition),
        )
        .route(
            "/api/operator/agent-definitions/{agent_id}",
            put(handlers::update_agent_definition).delete(handlers::delete_agent_definition),
        )
        .route("/api/operator/search", get(handlers::operator_search))
        .route("/api/operator/usage", get(handlers::operator_usage))
        .route(
            "/api/secrets",
            get(handlers::list_secrets).post(handlers::create_secret),
        )
        .route("/api/secrets/generate", post(handlers::generate_secret))
        .route("/api/secrets/{id}", get(handlers::get_secret))
        .route("/api/secrets/{id}/rotate", post(handlers::rotate_secret))
        .route("/api/secrets/{id}/revoke", post(handlers::revoke_secret))
        .route("/api/secrets/{id}/restore", post(handlers::restore_secret))
        .route(
            "/api/secrets/{id}/validate",
            post(handlers::validate_secret),
        )
        .route(
            "/api/operator/memories",
            get(handlers::list_operator_memories),
        )
        .route(
            "/api/operator/memories/{id}",
            get(handlers::get_operator_memory),
        )
        .route("/api/operator/runs", get(handlers::list_operator_runs))
        .route(
            "/api/operator/runs/{kind}/{id}",
            get(handlers::get_operator_run),
        )
        // Turn context snapshots — assembly reports for past turns.
        .route("/api/turns/{id}/context", get(handlers::get_turn_context))
        // Programmatic interface — async turn submission and polling.
        .route("/api/turns", post(handlers::submit_turn))
        .route("/api/turns/{id}", get(handlers::get_turn))
        // Turn listing per conversation.
        .route("/api/conversations/{id}/turns", get(handlers::list_turns))
        // Agent messaging — send and query inter-agent messages.
        .route("/api/messages", post(handlers::send_agent_message))
        .route("/api/messages/{id}", get(handlers::get_message_status))
        .route("/api/attachments", post(handlers::create_attachment))
        .route("/api/materials", post(handlers::create_material))
        .route("/api/materials/{id}", get(handlers::get_material))
        .route(
            "/api/materials/{id}/archive",
            post(handlers::archive_material),
        )
        .route(
            "/api/materials/{id}/restore",
            post(handlers::restore_material),
        )
        .route(
            "/api/materials/{id}/promote",
            post(handlers::promote_material),
        )
        .route(
            "/api/background-jobs/{id}",
            get(handlers::get_background_job),
        )
        .route(
            "/api/background-jobs/{id}/retry",
            post(handlers::retry_background_job),
        )
        // Durable schedules and their recorded runs.
        .route(
            "/api/schedules",
            get(handlers::list_schedules).post(handlers::create_schedule),
        )
        .route(
            "/api/schedules/{id}",
            put(handlers::update_schedule).delete(handlers::delete_schedule),
        )
        .route("/api/schedules/{id}/pause", post(handlers::pause_schedule))
        .route(
            "/api/schedules/{id}/resume",
            post(handlers::resume_schedule),
        )
        .route(
            "/api/schedules/{id}/trigger",
            post(handlers::trigger_schedule),
        )
        .route(
            "/api/schedules/{id}/runs",
            get(handlers::list_schedule_runs),
        )
        // Runtime presence and liveness.
        .route("/api/presence", get(handlers::presence_snapshot))
        .route(
            "/api/presence/machines/heartbeat",
            post(handlers::machine_heartbeat),
        )
        .route(
            "/api/presence/machines/{id}",
            axum::routing::delete(handlers::disconnect_machine),
        )
        // Devices and attached devices.
        .route("/api/devices", get(handlers::list_devices))
        .route("/api/devices/register", post(handlers::register_device))
        .route("/api/voice/capabilities", get(handlers::voice_capabilities))
        .route("/api/settings", get(handlers::get_settings))
        .route("/api/settings", put(handlers::save_settings))
        .route(
            "/api/settings/providers/{id}/models",
            get(handlers::discover_provider_models).post(handlers::discover_draft_provider_models),
        )
        .route(
            "/api/settings/providers/{id}/sign-in",
            get(handlers::provider_sign_in_state)
                .post(handlers::start_provider_sign_in)
                .delete(handlers::provider_sign_out),
        )
        .route(
            "/api/settings/restart-required",
            get(handlers::restart_status),
        )
        .route("/api/settings/restart", post(handlers::restart_gateway))
        .route(
            "/api/voice/transcriptions",
            post(handlers::transcribe_audio),
        )
        .route("/api/voice/speech", post(handlers::synthesise_speech))
        // Approval decisions via REST (same path for TUI, web UI, external scripts).
        .route("/api/approvals", get(handlers::list_approvals))
        .route("/api/approvals/{id}", post(handlers::decide_approval))
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
        // Record activity on every inbound request so presence and operator
        // views can show recent non-health work. Applied before request_id so
        // even early-exit paths update the timestamp.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            activity_recording_middleware,
        ))
        .with_state(state)
}
