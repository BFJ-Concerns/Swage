//! Live signal channel for the work surfaces, independent of any conversation.
//!
//! The conversation socket carries a turn: it needs a conversation id, and the
//! gateway admits only one connection per conversation. Page signals are not
//! turn events — a page changes whenever anyone writes to it, and the user
//! watching a page may have no conversation selected at all. This channel is
//! how an open work surface hears about that: a listen-only socket that any
//! client may open, carrying the server-wide signals a work surface refetches
//! on.
//!
//! The channel is a signal, not a payload route. Clients answer a signal by
//! reading through the authoritative HTTP path; nothing here is rendered.

use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{State, WebSocketUpgrade},
    response::Response,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast;

use crate::state::AppState;

use super::ServerEvent;

/// Whether a broadcast event is a work-surface signal.
///
/// `PageUpdated` tells an open surface that a page it may be showing has
/// changed; `Shutdown` tells it the gateway is going away. Everything else on
/// the broadcast channel belongs to a conversation or to observability.
pub(crate) fn is_work_surface_signal(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::PageUpdated { .. } | ServerEvent::Shutdown { .. }
    )
}

/// Axum route handler for `GET /api/work-surface/ws`.
///
/// Takes no identity and rejects nothing: an open surface is entitled to hear
/// that its page changed regardless of what else the client has open.
pub async fn work_surface_ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| handle_connection(socket, state))
}

async fn handle_connection(socket: WebSocket, state: AppState) {
    let (mut ws_sink, mut ws_stream) = socket.split();
    let mut bcast_rx = state.broadcast.subscribe();

    loop {
        tokio::select! {
            // The channel is listen-only. Draining the read half is still
            // required: it is how a client disconnect reaches us.
            frame = ws_stream.next() => {
                match frame {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }

            result = bcast_rx.recv() => {
                match result {
                    Ok(event) => {
                        if !is_work_surface_signal(&event) {
                            continue;
                        }
                        let is_shutdown = matches!(event, ServerEvent::Shutdown { .. });
                        match serde_json::to_string(&event) {
                            Ok(json) => {
                                if ws_sink.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                                if is_shutdown {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "failed to serialise work-surface signal");
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Signals are idempotent — the client refetches on the
                        // next one, so dropped signals cost at most a delay.
                        tracing::warn!(n, "work-surface receiver lagged, {n} signal(s) dropped");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    let _ = ws_sink.send(Message::Close(None)).await;
}
