//! WebSocket client for the Swage gateway streaming event protocol.
//!
//! Connects to a conversation's WebSocket endpoint and provides typed access
//! to the bidirectional event stream (ServerEvent/ClientEvent). Handles the
//! observability channel subscription protocol.
//!
//! # Usage
//!
//! ```rust,ignore
//! let mut ws = GatewayWebSocket::connect("http://127.0.0.1:3000", conv_id).await?;
//! ws.subscribe(vec!["context".into()]).await?;
//! while let Some(event) = ws.next_event().await? {
//!     match event {
//!         ServerEvent::Token { content } => print!("{content}"),
//!         ServerEvent::TurnComplete { .. } => break,
//!         ServerEvent::Shutdown { .. } => break,
//!         _ => {}
//!     }
//! }
//! ```

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use crate::error::ClientError;

// ---------------------------------------------------------------------------
// Server → Client event types
// ---------------------------------------------------------------------------

/// Events streamed from the gateway to the client.
///
/// Mirrors the gateway's own `ServerEvent` wire format — uses the same
/// `#[serde(tag = "type", rename_all = "snake_case")]` convention so JSON
/// produced by the gateway deserialises into this enum without any mapping.
/// Defined independently here so the client library has no compile-time
/// dependency on the gateway crate.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A token streamed from the LLM provider.
    Token { content: String },

    /// A tool call has started. Use `call_id` to correlate with `ToolEnd`.
    ToolStart {
        call_id: String,
        tool: String,
        arguments: serde_json::Value,
        /// Set when this is a nested sub-tool invocation.
        #[serde(default)]
        parent_call_id: Option<String>,
    },

    /// A tool call has finished.
    ToolEnd {
        call_id: String,
        tool: String,
        result: serde_json::Value,
        /// `"success"` or `"error"`.
        status: String,
        duration_ms: u64,
        truncated: bool,
        #[serde(default)]
        total_bytes: Option<u64>,
        /// Set when this is a nested sub-tool invocation.
        #[serde(default)]
        parent_call_id: Option<String>,
    },

    /// Context assembly completed. Only delivered on `"context"` channel
    /// subscriptions (or always when the report contains errors).
    ContextAssembled {
        turn_id: Uuid,
        report: serde_json::Value,
    },

    /// The turn has completed and been committed to storage.
    TurnComplete {
        turn_id: Uuid,
        #[serde(default)]
        input_tokens: Option<u32>,
        #[serde(default)]
        output_tokens: Option<u32>,
        #[serde(default)]
        cost_microcents: Option<i64>,
    },

    /// An error occurred. `code` is a well-known string (e.g. `"provider_error"`).
    Error { code: String, message: String },

    /// The active turn was successfully cancelled.
    Cancelled {},

    /// The gateway is shutting down. The WebSocket will close imminently.
    Shutdown { reason: String },

    /// A skill was activated for this session.
    SkillActivated {
        name: String,
        display_name: String,
        description: String,
        token_count: usize,
    },

    /// The named skill was already active — no state change.
    SkillAlreadyActive { name: String },

    /// A skill was deactivated for this session.
    SkillDeactivated { name: String },
}

// ---------------------------------------------------------------------------
// Client → Server event types
// ---------------------------------------------------------------------------

/// Events sent from the client to the gateway.
///
/// Mirrors the gateway's `ClientEvent` wire format. The `#[serde(tag = "type",
/// rename_all = "snake_case")]` convention ensures these serialise to the shape
/// the gateway expects without any manual field names.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// Send a message to the conversation, triggering a new LLM turn.
    SendMessage { content: String },
    /// Cancel the currently active turn.
    Cancel {},
    /// Subscribe to one or more observability channels (e.g. `"context"`, `"tools"`).
    Subscribe { channels: Vec<String> },
    /// Unsubscribe from observability channels.
    Unsubscribe { channels: Vec<String> },
    /// Request activation of a named skill.
    ActivateSkill { name: String },
    /// Request deactivation of a named skill.
    DeactivateSkill { name: String },
}

// ---------------------------------------------------------------------------
// WebSocket client
// ---------------------------------------------------------------------------

/// Connected WebSocket session to a Swage gateway conversation endpoint.
///
/// Wraps a tokio-tungstenite stream and provides typed send/receive over the
/// gateway's binary JSON protocol. Non-text frames (ping/pong/binary) are
/// silently skipped; unparseable text messages are logged and skipped.
pub struct GatewayWebSocket {
    inner: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    /// The address used to construct this connection — carried for error context.
    address: String,
}

impl GatewayWebSocket {
    /// Connect to the WebSocket endpoint for the given conversation.
    ///
    /// `address` is the gateway's HTTP base URL (e.g. `"http://127.0.0.1:3000"`).
    /// It is converted to a `ws://` URL automatically.
    pub async fn connect(address: &str, conversation_id: Uuid) -> Result<Self, ClientError> {
        let url = build_ws_url(address, conversation_id).map_err(|msg| {
            ClientError::WebSocketFailed {
                address: address.to_string(),
                message: msg,
            }
        })?;

        let (stream, _response) =
            connect_async(&url)
                .await
                .map_err(|e| ClientError::WebSocketFailed {
                    address: address.to_string(),
                    message: e.to_string(),
                })?;

        Ok(Self {
            inner: stream,
            address: address.to_string(),
        })
    }

    /// Read the next typed server event from the stream.
    ///
    /// Returns `Ok(None)` on a clean close (the server sent a Close frame).
    /// Non-text frames are silently skipped. Text frames that cannot be
    /// deserialised are logged as warnings and skipped — forward compatibility
    /// with new event types the client does not yet know about.
    pub async fn next_event(&mut self) -> Result<Option<ServerEvent>, ClientError> {
        loop {
            match self.inner.next().await {
                // Clean close or stream exhausted.
                None => return Ok(None),

                Some(Err(e)) => {
                    return Err(ClientError::WebSocketFailed {
                        address: self.address.clone(),
                        message: e.to_string(),
                    });
                }

                Some(Ok(Message::Close(_))) => return Ok(None),

                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ServerEvent>(&text) {
                        Ok(event) => return Ok(Some(event)),
                        Err(e) => {
                            // Unknown event type from a newer server — log and skip
                            // rather than crashing so old clients stay compatible.
                            tracing::warn!(
                                error = %e,
                                message = %text,
                                "Skipping unrecognised WebSocket message from gateway"
                            );
                            continue;
                        }
                    }
                }

                // Ping, Pong, and Binary frames are not part of the gateway
                // protocol — skip them silently.
                Some(Ok(_)) => continue,
            }
        }
    }

    /// Send a typed client event to the gateway.
    pub async fn send(&mut self, event: ClientEvent) -> Result<(), ClientError> {
        let text = serde_json::to_string(&event).map_err(|e| {
            // Serialising our own types should never fail — if it does,
            // surface it as a WebSocket protocol error so the caller can see it.
            ClientError::WebSocketFailed {
                address: self.address.clone(),
                message: format!("failed to serialise client event: {e}"),
            }
        })?;

        self.inner
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| ClientError::WebSocketFailed {
                address: self.address.clone(),
                message: e.to_string(),
            })
    }

    /// Subscribe to one or more observability channels.
    ///
    /// Valid channel names: `"context"`, `"tools"`. Unknown names are silently
    /// ignored by the server for forward compatibility.
    pub async fn subscribe(&mut self, channels: Vec<String>) -> Result<(), ClientError> {
        self.send(ClientEvent::Subscribe { channels }).await
    }

    /// Unsubscribe from one or more observability channels.
    pub async fn unsubscribe(&mut self, channels: Vec<String>) -> Result<(), ClientError> {
        self.send(ClientEvent::Unsubscribe { channels }).await
    }
}

// ---------------------------------------------------------------------------
// URL construction
// ---------------------------------------------------------------------------

/// Build the WebSocket URL for a conversation endpoint from an HTTP base address.
///
/// Converts `http://` → `ws://` and `https://` → `wss://` then appends the
/// conversation path. A `ws://` or `wss://` prefix is passed through unchanged.
///
/// Returns an error string if the scheme cannot be inferred from the address.
pub(crate) fn build_ws_url(address: &str, conversation_id: Uuid) -> Result<String, String> {
    let base = if let Some(rest) = address.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = address.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if address.starts_with("ws://") || address.starts_with("wss://") {
        address.to_string()
    } else {
        return Err(format!(
            "cannot determine WebSocket scheme from address '{address}': \
             expected http://, https://, ws://, or wss://"
        ));
    };

    // Trim any trailing slash so the path is always well-formed.
    let base = base.trim_end_matches('/');
    Ok(format!("{base}/api/conversations/{conversation_id}/ws"))
}

// ---------------------------------------------------------------------------
// Tests — written first; they fail to compile until types are defined
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // -- ServerEvent deserialisation --

    #[test]
    fn token_event_deserialises() {
        let json = r#"{"type":"token","content":"hello world"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::Token { content } = event else {
            panic!("expected Token variant");
        };
        assert_eq!(content, "hello world");
    }

    #[test]
    fn tool_start_event_deserialises() {
        let json = r#"{"type":"tool_start","call_id":"abc","tool":"shell","arguments":{"cmd":"ls"}}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::ToolStart { call_id, tool, .. } = event else {
            panic!("expected ToolStart variant");
        };
        assert_eq!(call_id, "abc");
        assert_eq!(tool, "shell");
    }

    #[test]
    fn tool_start_with_parent_call_id_deserialises() {
        let json = r#"{"type":"tool_start","call_id":"child","tool":"x","arguments":{},"parent_call_id":"parent"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::ToolStart { parent_call_id, .. } = event else {
            panic!("expected ToolStart");
        };
        assert_eq!(parent_call_id.as_deref(), Some("parent"));
    }

    #[test]
    fn tool_end_event_deserialises() {
        let json = r#"{"type":"tool_end","call_id":"abc","tool":"shell","result":"ok","status":"success","duration_ms":42,"truncated":false}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::ToolEnd { call_id, status, duration_ms, truncated, .. } = event else {
            panic!("expected ToolEnd variant");
        };
        assert_eq!(call_id, "abc");
        assert_eq!(status, "success");
        assert_eq!(duration_ms, 42);
        assert!(!truncated);
    }

    #[test]
    fn turn_complete_event_deserialises() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{"type":"turn_complete","turn_id":"{id}"}}"#);
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::TurnComplete { turn_id, input_tokens, output_tokens, cost_microcents } = event else {
            panic!("expected TurnComplete variant");
        };
        assert_eq!(turn_id, id);
        // Optional fields default to None when absent.
        assert!(input_tokens.is_none());
        assert!(output_tokens.is_none());
        assert!(cost_microcents.is_none());
    }

    #[test]
    fn turn_complete_with_tokens_deserialises() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"turn_complete","turn_id":"{id}","input_tokens":100,"output_tokens":50,"cost_microcents":200}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::TurnComplete { input_tokens, output_tokens, cost_microcents, .. } = event else {
            panic!("expected TurnComplete variant");
        };
        assert_eq!(input_tokens, Some(100));
        assert_eq!(output_tokens, Some(50));
        assert_eq!(cost_microcents, Some(200));
    }

    #[test]
    fn error_event_deserialises() {
        let json = r#"{"type":"error","code":"provider_error","message":"api key missing"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::Error { code, message } = event else {
            panic!("expected Error variant");
        };
        assert_eq!(code, "provider_error");
        assert_eq!(message, "api key missing");
    }

    #[test]
    fn cancelled_event_deserialises() {
        let json = r#"{"type":"cancelled"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ServerEvent::Cancelled {}));
    }

    #[test]
    fn shutdown_event_deserialises() {
        let json = r#"{"type":"shutdown","reason":"idle timeout"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::Shutdown { reason } = event else {
            panic!("expected Shutdown variant");
        };
        assert_eq!(reason, "idle timeout");
    }

    #[test]
    fn skill_activated_event_deserialises() {
        let json = r#"{"type":"skill_activated","name":"test-skill","display_name":"Test","description":"A test","token_count":42}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::SkillActivated { name, token_count, .. } = event else {
            panic!("expected SkillActivated");
        };
        assert_eq!(name, "test-skill");
        assert_eq!(token_count, 42);
    }

    #[test]
    fn skill_already_active_event_deserialises() {
        let json = r#"{"type":"skill_already_active","name":"my-skill"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::SkillAlreadyActive { name } = event else {
            panic!("expected SkillAlreadyActive");
        };
        assert_eq!(name, "my-skill");
    }

    #[test]
    fn skill_deactivated_event_deserialises() {
        let json = r#"{"type":"skill_deactivated","name":"old-skill"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        let ServerEvent::SkillDeactivated { name } = event else {
            panic!("expected SkillDeactivated");
        };
        assert_eq!(name, "old-skill");
    }

    #[test]
    fn context_assembled_event_deserialises() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{"type":"context_assembled","turn_id":"{id}","report":{{"elements":0}}}}"#);
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::ContextAssembled { turn_id, report } = event else {
            panic!("expected ContextAssembled");
        };
        assert_eq!(turn_id, id);
        assert!(report.is_object());
    }

    // -- ClientEvent serialisation --

    #[test]
    fn send_message_serialises() {
        let event = ClientEvent::SendMessage { content: "hello".to_string() };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "send_message");
        assert_eq!(v["content"], "hello");
    }

    #[test]
    fn cancel_serialises() {
        let event = ClientEvent::Cancel {};
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "cancel");
    }

    #[test]
    fn subscribe_serialises() {
        let event = ClientEvent::Subscribe {
            channels: vec!["context".to_string(), "tools".to_string()],
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "subscribe");
        assert_eq!(v["channels"][0], "context");
        assert_eq!(v["channels"][1], "tools");
    }

    #[test]
    fn unsubscribe_serialises() {
        let event = ClientEvent::Unsubscribe { channels: vec!["context".to_string()] };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "unsubscribe");
    }

    #[test]
    fn activate_skill_serialises() {
        let event = ClientEvent::ActivateSkill { name: "my-skill".to_string() };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "activate_skill");
        assert_eq!(v["name"], "my-skill");
    }

    #[test]
    fn deactivate_skill_serialises() {
        let event = ClientEvent::DeactivateSkill { name: "my-skill".to_string() };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "deactivate_skill");
        assert_eq!(v["name"], "my-skill");
    }

    // -- URL building --

    #[test]
    fn http_address_converts_to_ws_url() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let url = build_ws_url("http://127.0.0.1:3000", id).unwrap();
        assert_eq!(url, "ws://127.0.0.1:3000/api/conversations/550e8400-e29b-41d4-a716-446655440000/ws");
    }

    #[test]
    fn https_address_converts_to_wss_url() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let url = build_ws_url("https://example.com:443", id).unwrap();
        assert_eq!(url, "wss://example.com:443/api/conversations/550e8400-e29b-41d4-a716-446655440000/ws");
    }

    #[test]
    fn ws_address_is_passed_through() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let url = build_ws_url("ws://localhost:3000", id).unwrap();
        assert_eq!(url, "ws://localhost:3000/api/conversations/550e8400-e29b-41d4-a716-446655440000/ws");
    }
}
