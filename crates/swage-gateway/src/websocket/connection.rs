use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use tokio::sync::{Mutex, broadcast, mpsc};
use uuid::Uuid;

use swage_protocol::client::error::ErrorCode;

use crate::{error::ApiError, state::AppState, subscription::SubscriptionState};

use super::{ServerEvent, dispatch_client_message};

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
            return Some(ApiError::not_found("conversation", session_id).into_response());
        }
        Err(e) => {
            tracing::error!(%session_id, error = %e, "storage error checking conversation");
            return Some(ApiError::internal("failed to check conversation".into()).into_response());
        }
    }

    // Reject duplicate connections to the same conversation.
    if state.active_connections.lock().await.contains(&session_id) {
        return Some(
            ApiError::new(
                ErrorCode::AlreadyConnected,
                format!("conversation {session_id} already has an active connection"),
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
    crate::presence::record_client_connected(&state, session_id).await;

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
    let subscription = Arc::new(Mutex::new(SubscriptionState::new(
        state.active_config().observability.force_on,
    )));

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
                            let is_shutdown = matches!(event, ServerEvent::Shutdown { .. });
                            match serde_json::to_string(&event) {
                                Ok(json) => {
                                    if ws_sink.send(Message::Text(json.into())).await.is_err() {
                                        // Client disconnected — stop forwarding.
                                        break;
                                    }
                                    if is_shutdown {
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

                // Server-wide broadcast events are filtered here rather than
                // through per-connection subscriptions.
                result = bcast_rx.recv() => {
                    match result {
                        Ok(event) => {
                            if !broadcast_event_matches_session(session_id, &event) {
                                continue;
                            }
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

    if state.shutdown_signal.is_cancelled() {
        let _ = tx.send(ServerEvent::Shutdown {
            reason: "admin shutdown request".to_string(),
        });
    }

    // Re-emit any pending approvals for this session's agent. A client may
    // connect (or reconnect) after an approval was raised, so we catch them
    // up immediately rather than leaving them blind until the next turn.
    // Fail-open: storage errors are logged and the connection continues.
    // Re-emit pending approvals scoped to this session only. Previous
    // implementation used list_pending_by_agent which leaked approvals from
    // other sessions sharing the same agent_id.
    match state.storage.list_pending_by_session(session_id).await {
        Ok(pending) => {
            for approval in pending {
                let event = ServerEvent::ApprovalRequired {
                    approval_id: approval.id,
                    turn_id: approval.turn_id,
                    agent_id: approval.agent_id.clone(),
                    kind: approval.kind.to_string(),
                    tool_name: approval.tool_name.clone(),
                    tool_arguments: approval.tool_arguments.clone(),
                    prompt: approval.prompt.clone(),
                    context: approval.context.clone(),
                    timeout_at: approval.timeout_at.to_rfc3339(),
                    timeout_policy: approval.timeout_policy.to_string(),
                };
                // Ignore send errors: if the forward task has already
                // exited there is no client to receive the event.
                let _ = tx.send(event);
            }
        }
        Err(e) => {
            tracing::warn!(
                %session_id,
                error = %e,
                "failed to list pending approvals for re-emission on connect"
            );
        }
    }

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
                        crate::presence::record_client_seen(&state, session_id).await;
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
    crate::presence::record_client_disconnected(&state, session_id).await;
}

pub(super) fn broadcast_event_matches_session(_session_id: Uuid, event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::Shutdown { .. }
            | ServerEvent::PageUpdated { .. }
            | ServerEvent::WorkflowRunStarted { .. }
            | ServerEvent::WorkflowEntryStarted { .. }
            | ServerEvent::WorkflowEntryCompleted { .. }
            | ServerEvent::WorkflowRunCompleted { .. }
    )
}
