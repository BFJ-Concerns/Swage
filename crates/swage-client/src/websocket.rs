//! WebSocket client for the shared Swage streaming protocol.

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::error::ClientError;

pub use swage_protocol::client::{
    approval::ApprovalDecisionScope,
    context::{AssemblyReport, ElementReport},
    websocket::{ClientEvent, SchemaValidationError, ServerEvent},
};
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
        let url =
            build_ws_url(address, conversation_id).map_err(|msg| ClientError::WebSocketFailed {
                address: address.to_string(),
                message: msg,
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
    /// Non-text frames are silently skipped. Malformed JSON frames are logged
    /// and skipped. Unknown event types are surfaced as `ServerEvent::Unknown`
    /// so callers can decide how to handle them.
    pub async fn next_event(&mut self) -> Result<Option<ServerEvent>, ClientError> {
        loop {
            match self.inner.next().await {
                Some(Err(e)) => {
                    return Err(ClientError::WebSocketFailed {
                        address: self.address.clone(),
                        message: e.to_string(),
                    });
                }

                // Clean close or stream exhausted.
                None | Some(Ok(Message::Close(_))) => return Ok(None),

                Some(Ok(Message::Text(text))) => {
                    let value = match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(value) => value,
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                message = %text,
                                "Skipping malformed WebSocket message from gateway"
                            );
                            continue;
                        }
                    };

                    match serde_json::from_value::<ServerEvent>(value.clone()) {
                        Ok(event) => return Ok(Some(event)),
                        Err(error) => {
                            if let Some(event_type) = value
                                .as_object()
                                .and_then(|object| object.get("type"))
                                .and_then(|value| value.as_str())
                            {
                                tracing::warn!(
                                    error = %error,
                                    event_type,
                                    "Received unrecognised WebSocket event from gateway"
                                );
                                return Ok(Some(ServerEvent::Unknown {
                                    event_type: event_type.to_string(),
                                    payload: value,
                                }));
                            }

                            tracing::warn!(
                                error = %error,
                                message = %text,
                                "Skipping malformed WebSocket message from gateway"
                            );
                        }
                    }
                }

                // Ping, Pong, and Binary frames are not part of the gateway
                // protocol — skip them silently.
                Some(Ok(_)) => {}
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

    /// Submit a human verdict on a pending approval request.
    ///
    /// `decision` must be `"approved"` or `"rejected"`. The gateway validates
    /// the value and returns an `ApprovalDecisionError` event for anything else.
    pub async fn approval_decision(
        &mut self,
        approval_id: Uuid,
        decision: String,
        reason: Option<String>,
    ) -> Result<(), ClientError> {
        self.send(ClientEvent::ApprovalDecision {
            approval_id,
            decision,
            scope: ApprovalDecisionScope::Once,
            reason,
        })
        .await
    }

    /// Submit an approval decision with an explicit scope.
    pub async fn approval_decision_with_scope(
        &mut self,
        approval_id: Uuid,
        decision: String,
        scope: ApprovalDecisionScope,
        reason: Option<String>,
    ) -> Result<(), ClientError> {
        self.send(ClientEvent::ApprovalDecision {
            approval_id,
            decision,
            scope,
            reason,
        })
        .await
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
        let json =
            r#"{"type":"tool_start","call_id":"abc","tool":"shell","arguments":{"cmd":"ls"}}"#;
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
        let ServerEvent::ToolEnd {
            call_id,
            status,
            duration_ms,
            truncated,
            ..
        } = event
        else {
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
        let ServerEvent::TurnComplete {
            turn_id,
            input_tokens,
            output_tokens,
            cost_microcents,
            structured_output,
        } = event
        else {
            panic!("expected TurnComplete variant");
        };
        assert_eq!(turn_id, id);
        // Optional fields default to None when absent.
        assert!(input_tokens.is_none());
        assert!(output_tokens.is_none());
        assert!(cost_microcents.is_none());
        assert!(structured_output.is_none());
    }

    #[test]
    fn turn_complete_with_tokens_deserialises() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"turn_complete","turn_id":"{id}","input_tokens":100,"output_tokens":50,"cost_microcents":200}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::TurnComplete {
            input_tokens,
            output_tokens,
            cost_microcents,
            ..
        } = event
        else {
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
        let ServerEvent::SkillActivated {
            name, token_count, ..
        } = event
        else {
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
        let json = serde_json::json!({
            "type": "context_assembled",
            "turn_id": id,
            "report": AssemblyReport::default(),
        });
        let event: ServerEvent = serde_json::from_value(json).unwrap();
        let ServerEvent::ContextAssembled { turn_id, report } = event else {
            panic!("expected ContextAssembled");
        };
        assert_eq!(turn_id, id);
        assert_eq!(report.assembly_name, "default");
        assert!(report.elements.is_empty());
    }

    #[test]
    fn delegation_started_event_deserialises() {
        let parent_turn_id = Uuid::new_v4();
        let child_turn_id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"delegation_started","parent_turn_id":"{parent_turn_id}","child_turn_id":"{child_turn_id}","target_agent_id":"reviewer"}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::DelegationStarted {
            parent_turn_id: parent,
            child_turn_id: child,
            target_agent_id,
        } = event
        else {
            panic!("expected DelegationStarted");
        };
        assert_eq!(parent, parent_turn_id);
        assert_eq!(child, Some(child_turn_id));
        assert_eq!(target_agent_id, "reviewer");
    }

    #[test]
    fn delegation_started_event_deserialises_with_unknown_child_turn() {
        let parent_turn_id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"delegation_started","parent_turn_id":"{parent_turn_id}","child_turn_id":null,"target_agent_id":"reviewer"}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::DelegationStarted {
            parent_turn_id: parent,
            child_turn_id: child,
            target_agent_id,
        } = event
        else {
            panic!("expected DelegationStarted");
        };
        assert_eq!(parent, parent_turn_id);
        assert_eq!(child, None);
        assert_eq!(target_agent_id, "reviewer");
    }

    #[test]
    fn delegation_completed_event_deserialises() {
        let parent_turn_id = Uuid::new_v4();
        let child_turn_id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"delegation_completed","parent_turn_id":"{parent_turn_id}","child_turn_id":"{child_turn_id}","target_agent_id":"reviewer","success":false}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::DelegationCompleted {
            parent_turn_id: parent,
            child_turn_id: child,
            target_agent_id,
            success,
        } = event
        else {
            panic!("expected DelegationCompleted");
        };
        assert_eq!(parent, parent_turn_id);
        assert_eq!(child, Some(child_turn_id));
        assert_eq!(target_agent_id, "reviewer");
        assert!(!success);
    }

    #[test]
    fn delegation_completed_event_deserialises_with_unknown_child_turn() {
        let parent_turn_id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"delegation_completed","parent_turn_id":"{parent_turn_id}","child_turn_id":null,"target_agent_id":"reviewer","success":false}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::DelegationCompleted {
            parent_turn_id: parent,
            child_turn_id: child,
            target_agent_id,
            success,
        } = event
        else {
            panic!("expected DelegationCompleted");
        };
        assert_eq!(parent, parent_turn_id);
        assert_eq!(child, None);
        assert_eq!(target_agent_id, "reviewer");
        assert!(!success);
    }

    #[test]
    fn approval_required_event_deserialises_full_gateway_shape() {
        let approval_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"approval_required","approval_id":"{approval_id}","turn_id":"{turn_id}","agent_id":"gateway","kind":"tool","tool_name":"shell","tool_arguments":{{"command":"ls"}},"prompt":"Allow shell?","context":{{"cwd":"/tmp"}},"timeout_at":"2026-01-01T00:00:00Z","timeout_policy":"reject"}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::ApprovalRequired {
            approval_id: parsed_approval_id,
            turn_id: parsed_turn_id,
            agent_id,
            kind,
            tool_name,
            tool_arguments,
            prompt,
            context,
            timeout_at,
            timeout_policy,
        } = event
        else {
            panic!("expected ApprovalRequired");
        };
        assert_eq!(parsed_approval_id, approval_id);
        assert_eq!(parsed_turn_id, turn_id);
        assert_eq!(agent_id, "gateway");
        assert_eq!(kind, "tool");
        assert_eq!(tool_name.as_deref(), Some("shell"));
        assert_eq!(tool_arguments, Some(serde_json::json!({"command": "ls"})));
        assert_eq!(prompt, "Allow shell?");
        assert_eq!(context, Some(serde_json::json!({"cwd": "/tmp"})));
        assert_eq!(timeout_at, "2026-01-01T00:00:00Z");
        assert_eq!(timeout_policy, "reject");
    }

    #[test]
    fn approval_confirmed_event_deserialises_reason() {
        let approval_id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"approval_confirmed","approval_id":"{approval_id}","decision":"approved","reason":"looks safe"}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::ApprovalConfirmed {
            approval_id: parsed_approval_id,
            decision,
            reason,
        } = event
        else {
            panic!("expected ApprovalConfirmed");
        };
        assert_eq!(parsed_approval_id, approval_id);
        assert_eq!(decision, "approved");
        assert_eq!(reason.as_deref(), Some("looks safe"));
    }

    #[test]
    fn approval_expired_event_deserialises_timeout_policy() {
        let approval_id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"approval_expired","approval_id":"{approval_id}","timeout_policy":"interrupt"}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::ApprovalExpired {
            approval_id: parsed_approval_id,
            timeout_policy,
        } = event
        else {
            panic!("expected ApprovalExpired");
        };
        assert_eq!(parsed_approval_id, approval_id);
        assert_eq!(timeout_policy, "interrupt");
    }

    #[test]
    fn approval_decision_error_event_deserialises_current_state() {
        let json = r#"{"type":"approval_decision_error","approval_id":"bad-id","error":"already_decided","message":"already resolved","current_state":"approved"}"#;
        assert!(serde_json::from_str::<ServerEvent>(json).is_err());
    }

    // -- ClientEvent serialisation --

    #[test]
    fn send_message_serialises() {
        let event = ClientEvent::SendMessage {
            content: "hello".to_string(),
            attachments: Vec::new(),
            thread_id: None,
            selected_model_id: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "send_message");
        assert_eq!(v["content"], "hello");
        assert!(v.get("thread_id").is_none());
        assert!(
            v.get("attachments").is_none(),
            "text-only sends carry no attachments key"
        );
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
        let event = ClientEvent::Unsubscribe {
            channels: vec!["context".to_string()],
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "unsubscribe");
    }

    #[test]
    fn activate_skill_serialises() {
        let event = ClientEvent::ActivateSkill {
            name: "my-skill".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "activate_skill");
        assert_eq!(v["name"], "my-skill");
    }

    #[test]
    fn deactivate_skill_serialises() {
        let event = ClientEvent::DeactivateSkill {
            name: "my-skill".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "deactivate_skill");
        assert_eq!(v["name"], "my-skill");
    }

    #[test]
    fn approval_decision_serialises() {
        let approval_id = Uuid::new_v4();
        let event = ClientEvent::ApprovalDecision {
            approval_id,
            decision: "approved".to_string(),
            scope: ApprovalDecisionScope::Session,
            reason: Some("looks good".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "approval_decision");
        assert_eq!(v["approval_id"], approval_id.to_string());
        assert_eq!(v["decision"], "approved");
        assert_eq!(v["scope"], "session");
        assert_eq!(v["reason"], "looks good");
    }

    #[test]
    fn approval_decision_omits_none_reason() {
        let event = ClientEvent::ApprovalDecision {
            approval_id: Uuid::new_v4(),
            decision: "rejected".to_string(),
            scope: ApprovalDecisionScope::Once,
            reason: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "approval_decision");
        assert_eq!(v["decision"], "rejected");
        assert!(
            v.get("reason").is_none(),
            "None reason should be omitted from JSON"
        );
        assert!(
            v.get("scope").is_none(),
            "None scope should be omitted from JSON"
        );
    }

    #[test]
    fn turn_complete_with_structured_output_deserialises() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"type":"turn_complete","turn_id":"{id}","structured_output":{{"answer":42}}}}"#
        );
        let event: ServerEvent = serde_json::from_str(&json).unwrap();
        let ServerEvent::TurnComplete {
            structured_output, ..
        } = event
        else {
            panic!("expected TurnComplete variant");
        };
        assert_eq!(structured_output, Some(serde_json::json!({"answer": 42})));
    }

    // -- URL building --

    #[test]
    fn http_address_converts_to_ws_url() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let url = build_ws_url("http://127.0.0.1:3000", id).unwrap();
        assert_eq!(
            url,
            "ws://127.0.0.1:3000/api/conversations/550e8400-e29b-41d4-a716-446655440000/ws"
        );
    }

    #[test]
    fn https_address_converts_to_wss_url() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let url = build_ws_url("https://example.com:443", id).unwrap();
        assert_eq!(
            url,
            "wss://example.com:443/api/conversations/550e8400-e29b-41d4-a716-446655440000/ws"
        );
    }

    #[test]
    fn ws_address_is_passed_through() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let url = build_ws_url("ws://localhost:3000", id).unwrap();
        assert_eq!(
            url,
            "ws://localhost:3000/api/conversations/550e8400-e29b-41d4-a716-446655440000/ws"
        );
    }

    #[test]
    fn wss_address_is_passed_through() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let url = build_ws_url("wss://example.com:443", id).unwrap();
        assert_eq!(
            url,
            "wss://example.com:443/api/conversations/550e8400-e29b-41d4-a716-446655440000/ws"
        );
    }

    #[test]
    fn unknown_scheme_returns_error() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let err = build_ws_url("ftp://example.com", id).unwrap_err();
        assert!(
            err.contains("ftp://example.com"),
            "error should include the offending address"
        );
    }
}
