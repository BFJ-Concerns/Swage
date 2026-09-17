//! Live signal channel for the work surfaces, independent of any conversation.
//!
//! The conversation socket carries a turn: it needs a conversation id, and the
//! gateway admits only one connection per conversation. Surface signals are not
//! turn events — a surface changes whenever anyone writes to it, and the user
//! watching a surface may have no conversation selected at all. This channel is
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
/// `SurfaceUpdated` tells an open surface that a surface it may be showing has
/// changed; `LibraryUpdated` tells a catalogue view that the shared library
/// changed; `Shutdown` tells it the gateway is going away. Everything else on
/// the broadcast channel belongs to a conversation or to observability.
pub(crate) fn is_work_surface_signal(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::SurfaceUpdated { .. }
            | ServerEvent::LibraryUpdated { .. }
            | ServerEvent::Lagged { .. }
            | ServerEvent::Shutdown { .. }
    )
}

/// Axum route handler for `GET /api/work-surface/ws`.
///
/// Takes no identity and rejects nothing: an open surface is entitled to hear
/// that its surface changed regardless of what else the client has open.
pub async fn work_surface_ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    // The upgrade future runs only after the handshake. Subscribe now so a
    // surface update that immediately follows the request is buffered for the
    // connection task instead of disappearing in that window.
    let receiver = state.broadcast.subscribe();
    ws.on_upgrade(move |socket| handle_connection(socket, receiver))
}

async fn handle_connection(socket: WebSocket, mut receiver: broadcast::Receiver<ServerEvent>) {
    let (mut ws_sink, mut ws_stream) = socket.split();

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

            result = receiver.recv() => {
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
                        // Dropped signals may include the only one for a
                        // surface the client is showing: tell it to refetch
                        // rather than wait for a later signal that may never
                        // come. The client answers `Lagged` as it answers a
                        // reconnect — refetching what it has open.
                        tracing::warn!(n, "work-surface receiver lagged, {n} signal(s) dropped");
                        match serde_json::to_string(&ServerEvent::Lagged { dropped: n }) {
                            Ok(json) => {
                                if ws_sink.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "failed to serialise lag signal");
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    let _ = ws_sink.send(Message::Close(None)).await;
}
