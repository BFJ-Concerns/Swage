use std::sync::Arc;

use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex};
use uuid::Uuid;

use crate::{state::AppState, subscription::SubscriptionState, turn};

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
    /// Subscribe to one or more observability channels by name.
    ///
    /// Valid channel names: `"context"`, `"tools"`. Unknown names are silently
    /// ignored so forward-compatible clients do not break on older servers.
    Subscribe { channels: Vec<String> },
    /// Unsubscribe from one or more observability channels by name.
    Unsubscribe { channels: Vec<String> },
    /// Client requests activating a named skill for the current session.
    ActivateSkill { name: String },
    /// Client requests deactivating a named skill for the current session.
    DeactivateSkill { name: String },
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
    /// Context assembly completed for a turn.
    ///
    /// Contains the full assembly report for diagnostics and token budget
    /// inspection. Only delivered to connections subscribed to the `"context"`
    /// channel unless the report contains errors, in which case it is always
    /// sent so the client can surface assembly failures to the user.
    ///
    /// The report is carried as `serde_json::Value` to decouple the wire
    /// format from swage-context's internal types — the shape matches
    /// `swage_context::report::AssemblyReport` when serialised.
    ContextAssembled {
        turn_id: Uuid,
        report: serde_json::Value,
    },
    /// The provider has finished and the turn has been committed.
    ///
    /// The base event always includes `turn_id`. Usage fields are populated
    /// only for connections subscribed to the `"context"` channel — unsubscribed
    /// clients receive `turn_id` alone (the `skip_serializing_if` annotation
    /// keeps the JSON shape backward-compatible).
    TurnComplete {
        turn_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_microcents: Option<i64>,
    },
    /// An error occurred during or before turn processing.
    Error { code: String, message: String },
    /// A pending turn was successfully cancelled.
    Cancelled {},
    /// The server is shutting down. Clients should clean up and disconnect.
    ///
    /// Always delivered regardless of subscription state — a client that
    /// misses this event will simply see the connection close without a
    /// graceful handshake.
    Shutdown { reason: String },
    /// A skill was successfully activated for the current session.
    ///
    /// Includes metadata so the client can display the skill without a
    /// separate lookup.
    SkillActivated {
        name: String,
        display_name: String,
        description: String,
        /// Token budget consumed by this skill's instructions in the context window.
        token_count: usize,
    },
    /// A skill was already active — no state change occurred.
    SkillAlreadyActive { name: String },
    /// A skill was successfully deactivated for the current session.
    SkillDeactivated { name: String },
}

/// Well-known error codes for the `Error` event.
pub mod error_codes {
    pub const PROVIDER_ERROR:    &str = "provider_error";
    pub const TURN_IN_PROGRESS:  &str = "turn_in_progress";
    pub const INVALID_CONFIG:    &str = "invalid_config";
    pub const INTERNAL_ERROR:    &str = "internal_error";
    pub const SKILL_NOT_FOUND:   &str = "skill_not_found";
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

    // Per-connection observability subscription state. Initialised with
    // force_on from config so all events are delivered when the gateway is
    // running in development/debug mode without requiring explicit subscriptions.
    let subscription = Arc::new(Mutex::new(
        SubscriptionState::new(state.config.observability.force_on),
    ));

    // Subscribe to the server-wide broadcast channel before spawning the
    // forwarding task so no broadcast events (e.g. Shutdown) are missed.
    let mut bcast_rx = state.broadcast.subscribe();

    // Channel that turn processing uses to send events back to this connection.
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    // Spawn a forwarding task: reads from both the per-turn mpsc channel and
    // the server-wide broadcast channel, writing each event to the WebSocket.
    let mut forward_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // Per-turn events (Token, ToolStart, ToolEnd, TurnComplete, etc.)
                event = rx.recv() => {
                    match event {
                        None => break, // All senders dropped — turn and connection done.
                        Some(event) => {
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
                    }
                }

                // Server-wide broadcast events (Shutdown). Always delivered —
                // no subscription check needed here since the rules in
                // SubscriptionState::should_deliver already require delivery
                // for Shutdown regardless of subscription state.
                result = bcast_rx.recv() => {
                    match result {
                        Ok(event) => {
                            let is_shutdown = matches!(event, ServerEvent::Shutdown { .. });
                            match serde_json::to_string(&event) {
                                Ok(json) => {
                                    // Attempt the send; ignore the result for broadcast events.
                                    let _ = ws_sink.send(Message::Text(json.into())).await;
                                    if is_shutdown {
                                        // Server is going down — close after delivering the event.
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "failed to serialise broadcast event");
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            // The broadcast buffer overflowed; skip the missed events.
                            tracing::warn!(n, "broadcast receiver lagged, {n} event(s) dropped");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // Broadcast sender dropped (server shutdown) — close cleanly.
                            break;
                        }
                    }
                }
            }
        }

        // Send a close frame when the forwarding loop exits for any reason.
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
                        // Only actual messages reset the idle timer —
                        // an open but silent connection does not count.
                        state.activity_tracker.record_activity();
                        dispatch_client_message(
                            text.as_str(),
                            &state,
                            session_id,
                            tx.clone(),
                            Arc::clone(&subscription),
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

/// Discover skills available to a session (fail-open).
///
/// Resolves the Swage root, looks up the session's project slug and agent id,
/// builds the config hierarchy, and delegates to [`crate::skill::SkillManager`].
/// On any failure (missing root, unknown session, storage error) it logs a
/// warning and returns an empty skill set so the caller can degrade gracefully.
async fn discover_available_for_session(
    state: &AppState,
    session_id: Uuid,
) -> Vec<swage_config::skill::SkillDefinition> {
    use swage_storage::store::ProjectStore;

    // Fetch session to get project_id and agent_id.
    let session = match state.storage.get_session(session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            tracing::warn!(%session_id, "session not found during skill discovery");
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(%session_id, error = %e, "storage error during skill discovery");
            return Vec::new();
        }
    };

    // Fetch project to get the slug used in path construction.
    let project = match state.project_store.get(session.project_id).await {
        Ok(Some(p)) => p,
        _ => {
            tracing::warn!(%session_id, project_id = %session.project_id, "project not found during skill discovery");
            return Vec::new();
        }
    };

    // Resolve the Swage root directory.
    let root = match swage_config::discovery::resolve_root() {
        Ok(r) => r,
        Err(e) => {
            // Missing root is expected in development environments — not a fatal error.
            tracing::debug!(%session_id, "skill discovery skipped: {e}");
            return Vec::new();
        }
    };

    // Build hierarchy paths that mirror load_resolved_config_from.
    let project_dir = root.join("projects").join(&project.slug);
    let agent_dir   = project_dir.join("agents").join(&session.agent_id);

    let (skills, _warnings) = crate::skill::SkillManager::discover_available(
        &root,
        &project_dir,
        Some(&agent_dir),
    );
    skills
}

/// Parse and dispatch a raw text frame from the client.
///
/// Subscribe/Unsubscribe update per-connection [`SubscriptionState`].
/// SendMessage intercepts slash commands before spawning a turn.
/// ActivateSkill and DeactivateSkill delegate to [`crate::skill::SkillManager`].
async fn dispatch_client_message(
    text: &str,
    state: &AppState,
    session_id: Uuid,
    tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
) {
    use crate::skill::{SkillActivationResult, SkillError, SkillManager, SlashCommandResult};
    use swage_context::tokeniser::TiktokenTokeniser;

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
        ClientEvent::Subscribe { channels } => {
            // Update subscription state — unknown channel names are silently ignored.
            subscription.lock().await.subscribe(&channels);
        }
        ClientEvent::Unsubscribe { channels } => {
            subscription.lock().await.unsubscribe(&channels);
        }

        ClientEvent::SendMessage { content } => {
            // Check for a slash command before deciding whether to spawn a turn.
            match crate::skill::parse_slash_command(&content) {
                SlashCommandResult::SkillActivation { skill_name, remaining_text } => {
                    let available = discover_available_for_session(state, session_id).await;
                    let is_known = available.iter().any(|s| s.name == skill_name);

                    if is_known {
                        // Activate the skill and forward any trailing text as a turn.
                        let tok = TiktokenTokeniser::new(&state.config.provider.model);
                        match SkillManager::activate(session_id, &skill_name, &available, state.storage.as_ref(), &tok).await {
                            Ok(SkillActivationResult::Activated { name, display_name, description, token_count }) => {
                                let _ = tx.send(ServerEvent::SkillActivated { name, display_name, description, token_count });
                            }
                            Ok(SkillActivationResult::AlreadyActive) => {
                                let _ = tx.send(ServerEvent::SkillAlreadyActive { name: skill_name.clone() });
                            }
                            Err(SkillError::NotFound(n)) => {
                                tracing::warn!(%session_id, skill = %skill_name, "slash command skill not found");
                                let _ = tx.send(ServerEvent::Error {
                                    code: error_codes::SKILL_NOT_FOUND.to_string(),
                                    message: format!("skill '{n}' not found"),
                                });
                            }
                            Err(e) => {
                                tracing::warn!(%session_id, skill = %skill_name, error = ?e, "slash command skill activation failed");
                                let _ = tx.send(ServerEvent::Error {
                                    code: error_codes::INTERNAL_ERROR.to_string(),
                                    message: "skill activation failed".to_string(),
                                });
                            }
                        }

                        // If the slash command had trailing text, forward it as a turn message.
                        if let Some(text) = remaining_text {
                            let state = state.clone();
                            let sub   = Arc::clone(&subscription);
                            tokio::spawn(async move {
                                if let Err(e) = turn::process_turn(&state, session_id, text, tx.clone(), sub).await {
                                    let (code, message) = classify_gateway_error(&e);
                                    let _ = tx.send(ServerEvent::Error { code, message });
                                }
                            });
                        }
                        // Bare `/skill-name` (no trailing text) completes with activation only.
                    } else {
                        // Unknown skill — pass the original message through unchanged.
                        let state = state.clone();
                        let sub   = Arc::clone(&subscription);
                        tokio::spawn(async move {
                            if let Err(e) = turn::process_turn(&state, session_id, content, tx.clone(), sub).await {
                                let (code, message) = classify_gateway_error(&e);
                                let _ = tx.send(ServerEvent::Error { code, message });
                            }
                        });
                    }
                }
                SlashCommandResult::NotACommand => {
                    // Regular message — spawn the turn as normal.
                    let state = state.clone();
                    let sub   = Arc::clone(&subscription);
                    tokio::spawn(async move {
                        if let Err(e) = turn::process_turn(&state, session_id, content, tx.clone(), sub).await {
                            let (code, message) = classify_gateway_error(&e);
                            let _ = tx.send(ServerEvent::Error { code, message });
                        }
                    });
                }
            }
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

        ClientEvent::ActivateSkill { name } => {
            let available = discover_available_for_session(state, session_id).await;
            let tok = TiktokenTokeniser::new(&state.config.provider.model);

            match SkillManager::activate(session_id, &name, &available, state.storage.as_ref(), &tok).await {
                Ok(SkillActivationResult::Activated { name, display_name, description, token_count }) => {
                    let _ = tx.send(ServerEvent::SkillActivated { name, display_name, description, token_count });
                }
                Ok(SkillActivationResult::AlreadyActive) => {
                    let _ = tx.send(ServerEvent::SkillAlreadyActive { name });
                }
                Err(SkillError::NotFound(n)) => {
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::SKILL_NOT_FOUND.to_string(),
                        message: format!("skill '{n}' not found"),
                    });
                }
                Err(e) => {
                    tracing::error!(%session_id, skill = %name, error = ?e, "skill activation error");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "skill activation failed".to_string(),
                    });
                }
            }
        }

        ClientEvent::DeactivateSkill { name } => {
            let available = discover_available_for_session(state, session_id).await;

            match SkillManager::deactivate(session_id, &name, &available, state.storage.as_ref()).await {
                Ok(()) => {
                    let _ = tx.send(ServerEvent::SkillDeactivated { name });
                }
                Err(SkillError::AgentLevelCannotDeactivate(n)) => {
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: format!("Cannot deactivate agent-level skill '{n}'"),
                    });
                }
                Err(e) => {
                    tracing::error!(%session_id, skill = %name, error = ?e, "skill deactivation error");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "skill deactivation failed".to_string(),
                    });
                }
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
        let event = ServerEvent::TurnComplete {
            turn_id: id,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "turn_complete");
        assert_eq!(json["turn_id"], id.to_string());
        // Optional fields should be absent when None.
        assert!(json.get("input_tokens").is_none());
        assert!(json.get("output_tokens").is_none());
        assert!(json.get("cost_microcents").is_none());
    }

    #[test]
    fn server_event_turn_complete_with_usage_serialises() {
        let id = Uuid::new_v4();
        let event = ServerEvent::TurnComplete {
            turn_id: id,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_microcents: Some(1500),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "turn_complete");
        assert_eq!(json["turn_id"], id.to_string());
        assert_eq!(json["input_tokens"], 100);
        assert_eq!(json["output_tokens"], 50);
        assert_eq!(json["cost_microcents"], 1500);
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
    // Observability event serialisation / deserialisation (ogpt)
    // -----------------------------------------------------------------------

    #[test]
    fn client_event_subscribe_deserialises() {
        let json = r#"{"type": "subscribe", "channels": ["context", "tools"]}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        match event {
            ClientEvent::Subscribe { channels } => {
                assert_eq!(channels, vec!["context", "tools"]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn client_event_subscribe_empty_channels_deserialises() {
        let json = r#"{"type": "subscribe", "channels": []}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::Subscribe { channels } if channels.is_empty()));
    }

    #[test]
    fn client_event_unsubscribe_deserialises() {
        let json = r#"{"type": "unsubscribe", "channels": ["tools"]}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        match event {
            ClientEvent::Unsubscribe { channels } => {
                assert_eq!(channels, vec!["tools"]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn server_event_context_assembled_serialises() {
        let turn_id = Uuid::new_v4();
        let report = serde_json::json!({
            "assembly_name": "default",
            "total_tokens": 512,
            "errors": [],
        });
        let event = ServerEvent::ContextAssembled {
            turn_id,
            report: report.clone(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "context_assembled");
        assert_eq!(json["turn_id"], turn_id.to_string());
        assert_eq!(json["report"]["assembly_name"], "default");
        assert_eq!(json["report"]["total_tokens"], 512);
    }

    #[test]
    fn server_event_context_assembled_round_trips() {
        let turn_id = Uuid::new_v4();
        let report = serde_json::json!({
            "assembly_name": "custom",
            "errors": [{"assembly_name": "custom", "error_kind": "timeout", "message": "timed out", "fallback_used": "default"}],
        });
        let event = ServerEvent::ContextAssembled { turn_id, report };
        let serialised = serde_json::to_string(&event).unwrap();
        // Verify type tag is present in the raw JSON string.
        assert!(
            serialised.contains(r#""type":"context_assembled""#),
            "expected context_assembled type tag, got: {serialised}",
        );
    }

    #[test]
    fn server_event_shutdown_serialises() {
        let event = ServerEvent::Shutdown {
            reason: "idle timeout".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "shutdown");
        assert_eq!(json["reason"], "idle timeout");
    }

    #[test]
    fn server_event_shutdown_round_trips() {
        let event = ServerEvent::Shutdown {
            reason: "admin request".to_string(),
        };
        let serialised = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialised).unwrap();
        assert_eq!(parsed["type"], "shutdown");
        assert_eq!(parsed["reason"], "admin request");
    }

    // -----------------------------------------------------------------------
    // Skill event serialisation / deserialisation [e65s]
    // -----------------------------------------------------------------------

    #[test]
    fn client_event_activate_skill_deserialises() {
        let json = r#"{"type": "activate_skill", "name": "code-review"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::ActivateSkill { name } if name == "code-review"));
    }

    #[test]
    fn client_event_deactivate_skill_deserialises() {
        let json = r#"{"type": "deactivate_skill", "name": "code-review"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::DeactivateSkill { name } if name == "code-review"));
    }

    #[test]
    fn server_event_skill_activated_serialises() {
        let event = ServerEvent::SkillActivated {
            name: "code-review".to_string(),
            display_name: "Code Review".to_string(),
            description: "Reviews code for quality issues".to_string(),
            token_count: 512,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "skill_activated");
        assert_eq!(json["name"], "code-review");
        assert_eq!(json["display_name"], "Code Review");
        assert_eq!(json["description"], "Reviews code for quality issues");
        assert_eq!(json["token_count"], 512);
    }

    #[test]
    fn server_event_skill_already_active_serialises() {
        let event = ServerEvent::SkillAlreadyActive {
            name: "code-review".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "skill_already_active");
        assert_eq!(json["name"], "code-review");
    }

    #[test]
    fn server_event_skill_deactivated_serialises() {
        let event = ServerEvent::SkillDeactivated {
            name: "code-review".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "skill_deactivated");
        assert_eq!(json["name"], "code-review");
    }

    // -----------------------------------------------------------------------
    // Skill dispatch tests [5cpd]
    // -----------------------------------------------------------------------

    /// ActivateSkill for an unknown skill name sends a skill_not_found error event.
    ///
    /// The mock storage has no skill root on disk, so discover_available returns
    /// an empty set, causing SkillManager::activate to return NotFound.
    #[tokio::test]
    async fn activate_skill_event_for_unknown_skill_sends_not_found_error() {
        let (state, session_id) = make_state_with_session().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(false)));

        let json = r#"{"type":"activate_skill","name":"ghost-skill"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an event");
        match event {
            ServerEvent::Error { code, .. } => {
                assert_eq!(code, "skill_not_found", "expected skill_not_found error code");
            }
            other => panic!("expected Error event, got: {other:?}"),
        }
    }

    /// DeactivateSkill for a skill not in available (and not in storage) sends SkillDeactivated.
    ///
    /// Since the skill is not in the available set there's no agent-level check,
    /// and storage.deactivate_skill is a no-op that returns Ok(false) → success.
    #[tokio::test]
    async fn deactivate_skill_event_sends_skill_deactivated() {
        let (state, session_id) = make_state_with_session().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(false)));

        let json = r#"{"type":"deactivate_skill","name":"code-review"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an event");
        assert!(
            matches!(event, ServerEvent::SkillDeactivated { name } if name == "code-review"),
            "expected SkillDeactivated event",
        );
    }

    /// A plain (non-slash) SendMessage still dispatches normally — no skill events.
    #[tokio::test]
    async fn plain_send_message_does_not_emit_skill_event() {
        let (state, session_id) = make_state_with_session().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(false)));

        let json = r#"{"type":"send_message","content":"hello world"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        // The turn spawned by MockProvider will eventually send an event, but
        // we're only verifying no *skill* event appears first.
        // Give the spawned turn a moment to produce events, then drain.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        while let Ok(event) = rx.try_recv() {
            assert!(
                !matches!(event, ServerEvent::SkillActivated { .. } | ServerEvent::SkillAlreadyActive { .. }),
                "unexpected skill event for plain message: {event:?}",
            );
        }
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
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        AppState {
            storage: storage_arc,
            project_store,
            provider,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
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
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        let state = AppState {
            storage: storage_arc,
            project_store,
            provider,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
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
