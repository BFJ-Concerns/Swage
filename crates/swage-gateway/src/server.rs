use axum::{
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

use crate::{assets, handlers, state::AppState, websocket};


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
        // Serve the compiled frontend SPA for all non-API paths.
        // API routes above take priority - the fallback only fires for
        // paths that don't match any registered route.
        .fallback(assets::serve_frontend)
        // Emit a tracing span for every inbound request.
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
