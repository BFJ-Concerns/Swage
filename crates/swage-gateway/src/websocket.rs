use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{state::AppState, turn};

// ---------------------------------------------------------------------------
// Client → Server event types
// ---------------------------------------------------------------------------

/// Events sent by the client over the WebSocket connection.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// Send a message to the conversation, triggering a provider turn.
    SendMessage { content: String },
    /// Cancel the currently active turn.
    Cancel {},
}

// ---------------------------------------------------------------------------
// Server → Client event types
// ---------------------------------------------------------------------------

/// Events sent by the server to the client over the WebSocket connection.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A token streamed from the provider.
    Token { content: String },
    /// A tool call has begun execution. Correlates with a later ToolEnd via call_id.
    ToolStart {
        call_id: String,
        tool: String,
        arguments: serde_json::Value,
        /// call_id of the enclosing tool if this is a sub-tool invocation.
        ///
        /// Omitted from the serialised JSON when None so existing clients that
        /// do not handle sub-tools remain unaffected.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    /// A tool call has finished. Contains the result and execution metadata.
    ToolEnd {
        call_id: String,
        tool: String,
        result: serde_json::Value,
        /// "success" or "error".
        status: String,
        duration_ms: u64,
        truncated: bool,
        total_bytes: Option<u64>,
        /// call_id of the enclosing tool if this is a sub-tool invocation.
        ///
        /// Omitted from the serialised JSON when None.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    /// The provider has finished and the turn has been committed.
    TurnComplete { turn_id: Uuid },
    /// An error occurred during or before turn processing.
    Error { code: String, message: String },
    /// A pending turn was successfully cancelled.
    Cancelled {},
}

/// Well-known error codes for the `Error` event.
pub mod error_codes {
    pub const PROVIDER_ERROR: &str = "provider_error";
    pub const TURN_IN_PROGRESS: &str = "turn_in_progress";
    pub const INVALID_CONFIG: &str = "invalid_config";
    pub const INTERNAL_ERROR: &str = "internal_error";
}

// ---------------------------------------------------------------------------
// Route handler
// ---------------------------------------------------------------------------

/// Validate preconditions for a WebSocket upgrade.
///
/// Returns `None` if the upgrade can proceed, or `Some(error_response)` if it
/// should be rejected. Extracted so the precondition logic can be tested
/// independently of the WebSocket upgrade machinery.
pub(crate) async fn check_upgrade_preconditions(
    state: &AppState,
    session_id: Uuid,
) -> Option<Response> {
    // Verify the conversation (session) exists in storage.
    match state.storage.get_session(session_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Some(
                (
                    StatusCode::NOT_FOUND,
                    axum::Json(serde_json::json!({
                        "code": "not_found",
                        "message": format!("conversation {session_id} not found")
                    })),
                )
                    .into_response(),
            );
        }
        Err(e) => {
            tracing::error!(%session_id, error = %e, "storage error checking conversation");
            return Some(
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({
                        "code": "internal_error",
                        "message": "failed to check conversation"
                    })),
                )
                    .into_response(),
            );
        }
    }

    // Reject duplicate connections to the same conversation.
    if state.active_connections.lock().await.contains(&session_id) {
        return Some(
            (
                StatusCode::CONFLICT,
                axum::Json(serde_json::json!({
                    "code": "already_connected",
                    "message": format!("conversation {session_id} already has an active connection")
                })),
            )
                .into_response(),
        );
    }

    None
}

/// Axum route handler for `GET /api/conversations/:id/ws`.
///
/// Validates the conversation exists and has no existing live connection,
/// then upgrades to a WebSocket.
pub async fn ws_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(rejection) = check_upgrade_preconditions(&state, session_id).await {
        return rejection;
    }

    // Mark connection as active now that preconditions passed.
    state.active_connections.lock().await.insert(session_id);

    ws.on_upgrade(move |socket| handle_connection(socket, state, session_id))
}

/// Handle an established WebSocket connection for a conversation.
///
/// Splits the socket into a sender and receiver. Incoming client events are
/// dispatched to turn processing; outgoing server events are forwarded from a
/// channel to the WebSocket.
///
/// If the client disconnects while a turn is active, the turn continues in the
/// background and writes its result to storage.
async fn handle_connection(socket: WebSocket, state: AppState, session_id: Uuid) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Channel that turn processing uses to send events back to this connection.
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    // Spawn a forwarding task: reads from the channel and writes to the WebSocket.
    // When the channel is closed (turn dropped the sender), this task ends naturally.
    let mut forward_task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match serde_json::to_string(&event) {
                Ok(json) => {
                    if ws_sink.send(Message::Text(json.into())).await.is_err() {
                        // Client disconnected — stop forwarding.
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to serialise server event");
                }
            }
        }
        // Send a close frame when the channel is exhausted.
        let _ = ws_sink.send(Message::Close(None)).await;
    });

    // Main receive loop: read client messages and dispatch.
    loop {
        tokio::select! {
            // If the forwarding task exits early (client disconnected), stop reading.
            _ = &mut forward_task => break,

            msg = ws_stream.next() => {
                match msg {
                    None => break, // Client closed the connection.
                    Some(Err(e)) => {
                        tracing::warn!(%session_id, error = %e, "WebSocket receive error");
                        break;
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Text(text))) => {
                        dispatch_client_message(
                            text.as_str(),
                            &state,
                            session_id,
                            tx.clone(),
                        )
                        .await;
                    }
                    Some(Ok(_)) => {
                        // Ignore binary, ping, and pong frames.
                    }
                }
            }
        }
    }

    // Connection is done — remove from the active set.
    {
        let mut connections = state.active_connections.lock().await;
        connections.remove(&session_id);
    }
}

/// Parse and dispatch a raw text frame from the client.
async fn dispatch_client_message(
    text: &str,
    state: &AppState,
    session_id: Uuid,
    tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let event = match serde_json::from_str::<ClientEvent>(text) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(%session_id, error = %e, raw = text, "unparseable client event");
            let _ = tx.send(ServerEvent::Error {
                code: error_codes::INTERNAL_ERROR.to_string(),
                message: format!("invalid event: {e}"),
            });
            return;
        }
    };

    match event {
        ClientEvent::SendMessage { content } => {
            // Spawn the turn so the receive loop remains available for Cancel events.
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(e) = turn::process_turn(&state, session_id, content, tx.clone()).await {
                    let (code, message) = classify_gateway_error(&e);
                    let _ = tx.send(ServerEvent::Error { code, message });
                }
            });
        }
        ClientEvent::Cancel {} => {
            if let Err(e) = turn::cancel_turn(state, session_id).await {
                tracing::warn!(%session_id, error = %e, "cancel_turn failed");
                let _ = tx.send(ServerEvent::Error {
                    code: error_codes::INTERNAL_ERROR.to_string(),
                    message: e.to_string(),
                });
            }
        }
    }
}

/// Map a `GatewayError` to a (code, message) pair suitable for a client Error event.
fn classify_gateway_error(err: &swage_core::GatewayError) -> (String, String) {
    use swage_core::GatewayError;
    match err {
        GatewayError::TurnInProgress { .. } => (
            error_codes::TURN_IN_PROGRESS.to_string(),
            err.to_string(),
        ),
        GatewayError::Provider(_) => (error_codes::PROVIDER_ERROR.to_string(), err.to_string()),
        GatewayError::ConfigError(_) => {
            (error_codes::INVALID_CONFIG.to_string(), err.to_string())
        }
        _ => (error_codes::INTERNAL_ERROR.to_string(), err.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use tokio::sync::Mutex;

    use swage_provider::LlmProvider;
    use swage_storage::model::Session;

    use super::*;
    use crate::turn::tests::{MockProvider, MockStorage};

    // -----------------------------------------------------------------------
    // Event serialisation / deserialisation round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn client_event_send_message_deserialises() {
        let json = r#"{"type": "send_message", "content": "hello"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::SendMessage { content } if content == "hello"));
    }

    #[test]
    fn client_event_cancel_deserialises() {
        let json = r#"{"type": "cancel"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::Cancel {}));
    }

    #[test]
    fn server_event_token_serialises() {
        let event = ServerEvent::Token {
            content: "hi".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "token");
        assert_eq!(json["content"], "hi");
    }

    #[test]
    fn server_event_turn_complete_serialises() {
        let id = Uuid::new_v4();
        let event = ServerEvent::TurnComplete { turn_id: id };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "turn_complete");
        assert_eq!(json["turn_id"], id.to_string());
    }

    #[test]
    fn server_event_error_serialises() {
        let event = ServerEvent::Error {
            code: "provider_error".to_string(),
            message: "overloaded".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "provider_error");
        assert_eq!(json["message"], "overloaded");
    }

    #[test]
    fn server_event_cancelled_serialises() {
        let event = ServerEvent::Cancelled {};
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "cancelled");
    }

    #[test]
    fn server_event_tool_start_serialises() {
        let event = ServerEvent::ToolStart {
            call_id: "toolu_01abc".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({"command": "ls -la"}),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_start");
        assert_eq!(json["call_id"], "toolu_01abc");
        assert_eq!(json["tool"], "shell");
        assert_eq!(json["arguments"]["command"], "ls -la");
    }

    #[test]
    fn server_event_tool_start_omits_parent_call_id_when_none() {
        let event = ServerEvent::ToolStart {
            call_id: "toolu_native".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        // parent_call_id must be absent entirely, not serialised as null,
        // so existing clients that don't handle nesting remain unaffected.
        assert!(!json.as_object().unwrap().contains_key("parent_call_id"),
            "parent_call_id should be absent when None, got: {json}");
    }

    #[test]
    fn server_event_tool_start_includes_parent_call_id_when_some() {
        let event = ServerEvent::ToolStart {
            call_id: "sub_01".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: Some("py_call_01".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["parent_call_id"], "py_call_01");
    }

    #[test]
    fn server_event_tool_end_serialises() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_01abc".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("file1\nfile2\nfile3"),
            status: "success".to_string(),
            duration_ms: 142,
            truncated: false,
            total_bytes: Some(25),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_end");
        assert_eq!(json["call_id"], "toolu_01abc");
        assert_eq!(json["tool"], "shell");
        assert_eq!(json["result"], "file1\nfile2\nfile3");
        assert_eq!(json["status"], "success");
        assert_eq!(json["duration_ms"], 142);
        assert_eq!(json["truncated"], false);
        assert_eq!(json["total_bytes"], 25);
    }

    #[test]
    fn server_event_tool_end_omits_parent_call_id_when_none() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_native".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("ok"),
            status: "success".to_string(),
            duration_ms: 10,
            truncated: false,
            total_bytes: None,
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(!json.as_object().unwrap().contains_key("parent_call_id"),
            "parent_call_id should be absent when None");
    }

    #[test]
    fn server_event_tool_end_includes_parent_call_id_when_some() {
        let event = ServerEvent::ToolEnd {
            call_id: "sub_01".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("output"),
            status: "success".to_string(),
            duration_ms: 5,
            truncated: false,
            total_bytes: None,
            parent_call_id: Some("py_call_01".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["parent_call_id"], "py_call_01");
    }

    #[test]
    fn server_event_tool_end_serialises_without_total_bytes() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_02".to_string(),
            tool: "file_read".to_string(),
            result: serde_json::json!({"content": "hello"}),
            status: "success".to_string(),
            duration_ms: 5,
            truncated: false,
            total_bytes: None,
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_end");
        assert!(json["total_bytes"].is_null());
        // Structured result (object) serialises correctly.
        assert_eq!(json["result"]["content"], "hello");
    }

    #[test]
    fn server_event_tool_end_truncated() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_03".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("partial output..."),
            status: "success".to_string(),
            duration_ms: 30_000,
            truncated: true,
            total_bytes: Some(1_048_576),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json["truncated"].as_bool().unwrap());
        assert_eq!(json["total_bytes"], 1_048_576);
    }

    // -----------------------------------------------------------------------
    // Precondition tests (tested directly to avoid WebSocket upgrade ceremony)
    //
    // axum's WebSocketUpgrade extractor validates the actual TCP connection
    // in integration scenarios; for unit testing the precondition logic we
    // call check_upgrade_preconditions directly instead.
    // -----------------------------------------------------------------------

    /// Build an AppState with no sessions.
    fn make_empty_state() -> AppState {
        let storage = Arc::new(MockStorage::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![]));
        let storage_arc: Arc<dyn swage_storage::store::Storage> = Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>;
        let project_store: Arc<dyn swage_storage::store::ProjectStore> = storage as Arc<dyn swage_storage::store::ProjectStore>;
        AppState {
            storage: storage_arc,
            project_store,
            provider,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
        }
    }

    /// Build an AppState with a pre-inserted session.
    async fn make_state_with_session() -> (AppState, Uuid) {
        use swage_storage::store::ProjectStore;

        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                agent_id: "gateway".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![]));
        let storage_arc: Arc<dyn swage_storage::store::Storage> = Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>;
        let project_store: Arc<dyn swage_storage::store::ProjectStore> = storage as Arc<dyn swage_storage::store::ProjectStore>;
        let state = AppState {
            storage: storage_arc,
            project_store,
            provider,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
        };

        (state, session_id)
    }

    #[tokio::test]
    async fn preconditions_reject_non_existent_conversation() {
        let state = make_empty_state();
        let missing_id = Uuid::new_v4();

        let response = check_upgrade_preconditions(&state, missing_id)
            .await
            .expect("should return Some rejection");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn preconditions_reject_duplicate_connection() {
        let (state, session_id) = make_state_with_session().await;

        // Pre-mark this session as already connected.
        state.active_connections.lock().await.insert(session_id);

        let response = check_upgrade_preconditions(&state, session_id)
            .await
            .expect("should return Some rejection");

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn preconditions_pass_for_valid_unconnected_session() {
        let (state, session_id) = make_state_with_session().await;

        let result = check_upgrade_preconditions(&state, session_id).await;
        assert!(result.is_none(), "valid session should pass preconditions");
    }
}
