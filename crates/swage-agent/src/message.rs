//! Agent boundary types — the serialisable messages exchanged between caller and agent.
//!
//! All types here are designed for future IPC compatibility: they serialise cleanly
//! to JSON and carry no references into agent internals.

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::context::TurnExecutionContext;
use swage_core::ChatMessage;
use swage_storage::TurnState;

/// A successful response from the agent to the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// The assistant's response text.
    pub response: String,
    /// ID of the completed turn.
    pub turn_id: Uuid,
    /// Total input tokens consumed.
    pub input_tokens: u32,
    /// Total output tokens consumed.
    pub output_tokens: u32,
    /// Number of tool calls dispatched during the turn.
    pub tool_calls_count: u32,
    /// Parsed, validated JSON when an output schema is configured and
    /// validation passes. Omitted from serialised JSON when None so
    /// existing consumers are unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
}

#[cfg(test)]
mod outbound_message_tests {
    use super::*;

    fn sample() -> OutboundMessage {
        OutboundMessage {
            response: "The answer is 42.".into(),
            turn_id: Uuid::new_v4(),
            input_tokens: 128,
            output_tokens: 64,
            tool_calls_count: 3,
            structured_output: None,
        }
    }

    #[test]
    fn round_trip() {
        let original = sample();
        let json = serde_json::to_string(&original).expect("serialise");
        let decoded: OutboundMessage = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(decoded.response, original.response);
        assert_eq!(decoded.turn_id, original.turn_id);
        assert_eq!(decoded.input_tokens, original.input_tokens);
        assert_eq!(decoded.output_tokens, original.output_tokens);
        assert_eq!(decoded.tool_calls_count, original.tool_calls_count);
        assert_eq!(decoded.structured_output, None);
    }

    #[test]
    fn all_fields_present_in_json() {
        // structured_output is skip_serializing_if None, so use a sample with Some.
        let msg = OutboundMessage {
            structured_output: Some(serde_json::json!({"result": "ok"})),
            ..sample()
        };
        let json = serde_json::to_string(&msg).expect("serialise");
        assert!(json.contains("\"response\""), "missing response field");
        assert!(json.contains("\"turn_id\""), "missing turn_id field");
        assert!(
            json.contains("\"input_tokens\""),
            "missing input_tokens field"
        );
        assert!(
            json.contains("\"output_tokens\""),
            "missing output_tokens field"
        );
        assert!(
            json.contains("\"tool_calls_count\""),
            "missing tool_calls_count field"
        );
        assert!(
            json.contains("\"structured_output\""),
            "missing structured_output field"
        );
    }

    #[test]
    fn zero_token_counts_serialise() {
        // Counts of zero must be preserved — not omitted as absent.
        let msg = OutboundMessage {
            response: "simple".into(),
            turn_id: Uuid::new_v4(),
            input_tokens: 0,
            output_tokens: 0,
            tool_calls_count: 0,
            structured_output: None,
        };
        let json = serde_json::to_string(&msg).expect("serialise");
        let decoded: OutboundMessage = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(decoded.input_tokens, 0);
        assert_eq!(decoded.output_tokens, 0);
        assert_eq!(decoded.tool_calls_count, 0);
    }

    #[test]
    fn structured_output_serialises_when_present() {
        let payload = serde_json::json!({"x": 1});
        let msg = OutboundMessage {
            structured_output: Some(payload.clone()),
            ..sample()
        };
        let json = serde_json::to_string(&msg).expect("serialise");
        assert!(
            json.contains("\"structured_output\""),
            "field should be present"
        );

        let decoded: OutboundMessage = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(decoded.structured_output, Some(payload));
    }

    #[test]
    fn structured_output_absent_from_json_when_none() {
        let msg = sample(); // structured_output is None
        let json = serde_json::to_string(&msg).expect("serialise");
        assert!(
            !json.contains("\"structured_output\""),
            "field should be absent when None, got: {json}"
        );
    }
}

/// The input to `Agent::process_turn_streaming()`.
///
/// The gateway pre-assembles context from the context engine and passes it here,
/// so the agent does not rebuild context from thread history in the streaming path.
/// The user message is already canonicalised. The agent creates the turn record
/// by default, but callers may also supply a pre-created pending turn to reuse.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    /// Canonical execution metadata for this turn.
    pub execution: TurnExecutionContext,
    /// Canonical user input for this turn.
    pub user_message: String,
    /// Pre-assembled message context from the context engine.
    pub assembled_context: Vec<ChatMessage>,
    /// Optional caller guidance for agent-side summarisation compaction.
    pub compaction_focus: Option<String>,
    /// Optional parent turn for delegation chain tracking. `None` for
    /// user-initiated turns; `Some(sender_turn_id)` for delegated turns.
    pub parent_turn_id: Option<Uuid>,
    /// Optional pre-created Pending turn to reuse instead of creating a fresh
    /// turn record inside the agent runtime.
    pub existing_turn_id: Option<Uuid>,
    /// Per-turn provider reasoning effort override.
    pub effort: Option<swage_core::Effort>,
    /// Cooperative cancellation token — the gateway signals this when the client
    /// disconnects or a shutdown is requested.
    pub cancellation_token: CancellationToken,
}

#[cfg(test)]
mod turn_request_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn test_turn_request_constructible() {
        let req = TurnRequest {
            execution: TurnExecutionContext {
                session_id: uuid::Uuid::new_v4(),
                thread_id: uuid::Uuid::new_v4(),
                turn_id: uuid::Uuid::new_v4(),
                space_id: Some(uuid::Uuid::new_v4()),
            },
            user_message: "hello".to_string(),
            assembled_context: vec![],
            compaction_focus: None,
            parent_turn_id: None,
            existing_turn_id: None,
            effort: None,
            cancellation_token: CancellationToken::new(),
        };
        assert_eq!(req.user_message, "hello");
        assert!(req.assembled_context.is_empty());
    }

    #[test]
    fn test_execution_context_is_required() {
        let req = TurnRequest {
            effort: None,
            execution: TurnExecutionContext {
                session_id: uuid::Uuid::new_v4(),
                thread_id: uuid::Uuid::new_v4(),
                turn_id: uuid::Uuid::new_v4(),
                space_id: None,
            },
            user_message: "test".to_string(),
            assembled_context: vec![],
            compaction_focus: None,
            parent_turn_id: None,
            existing_turn_id: None,
            cancellation_token: CancellationToken::new(),
        };
        assert!(!req.execution.thread_id.is_nil());
    }

    #[test]
    fn test_cloneable() {
        let req = TurnRequest {
            effort: None,
            execution: TurnExecutionContext {
                session_id: uuid::Uuid::new_v4(),
                thread_id: uuid::Uuid::new_v4(),
                turn_id: uuid::Uuid::new_v4(),
                space_id: Some(uuid::Uuid::new_v4()),
            },
            user_message: "clone me".to_string(),
            assembled_context: vec![],
            compaction_focus: None,
            parent_turn_id: None,
            existing_turn_id: None,
            cancellation_token: CancellationToken::new(),
        };
        let cloned = req.clone();
        assert_eq!(cloned.user_message, req.user_message);
        assert_eq!(cloned.execution.session_id, req.execution.session_id);
        assert_eq!(cloned.execution.thread_id, req.execution.thread_id);
    }

    #[test]
    fn test_cancellation_token_linked_cancel() {
        // CancellationToken::clone() produces a linked token — both cancel together.
        let token = CancellationToken::new();
        let req = TurnRequest {
            effort: None,
            execution: TurnExecutionContext {
                session_id: uuid::Uuid::new_v4(),
                thread_id: uuid::Uuid::new_v4(),
                turn_id: uuid::Uuid::new_v4(),
                space_id: None,
            },
            user_message: "cancel test".to_string(),
            assembled_context: vec![],
            compaction_focus: None,
            parent_turn_id: None,
            existing_turn_id: None,
            cancellation_token: token.clone(),
        };
        token.cancel();
        assert!(req.cancellation_token.is_cancelled());
    }

    #[test]
    fn test_debug_format() {
        let req = TurnRequest {
            effort: None,
            execution: TurnExecutionContext {
                session_id: uuid::Uuid::new_v4(),
                thread_id: uuid::Uuid::new_v4(),
                turn_id: uuid::Uuid::new_v4(),
                space_id: None,
            },
            user_message: "debug".to_string(),
            assembled_context: vec![],
            compaction_focus: None,
            parent_turn_id: None,
            existing_turn_id: None,
            cancellation_token: CancellationToken::new(),
        };
        let s = format!("{req:?}");
        assert!(s.contains("TurnRequest"));
    }
}

/// An error from the agent to the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Category of the error.
    pub error_type: String,
    /// Human-readable error description.
    pub description: String,
    /// Whether the caller should retry.
    pub is_retriable: bool,
    /// ID of the turn that failed (None if failure occurred before turn creation).
    pub turn_id: Option<Uuid>,
    /// State the turn was in when the error occurred.
    pub turn_state: Option<TurnState>,
}

#[cfg(test)]
mod error_response_tests {
    use super::*;

    #[test]
    fn round_trip_with_turn_info() {
        let original = ErrorResponse {
            error_type: "ProviderFailed".into(),
            description: "LLM call timed out".into(),
            is_retriable: true,
            turn_id: Some(Uuid::new_v4()),
            turn_state: Some(TurnState::Processing),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.error_type, original.error_type);
        assert_eq!(restored.description, original.description);
        assert_eq!(restored.is_retriable, original.is_retriable);
        assert_eq!(restored.turn_id, original.turn_id);
        assert_eq!(restored.turn_state, original.turn_state);
    }

    #[test]
    fn round_trip_without_turn_info() {
        // Failure before turn creation — both optional fields absent.
        let original = ErrorResponse {
            error_type: "InvalidConfig".into(),
            description: "agent_id is empty".into(),
            is_retriable: false,
            turn_id: None,
            turn_state: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.error_type, original.error_type);
        assert!(!restored.is_retriable);
        assert!(restored.turn_id.is_none());
        assert!(restored.turn_state.is_none());
    }

    #[test]
    fn all_fields_present_in_json() {
        let msg = ErrorResponse {
            error_type: "ToolFailed".into(),
            description: "tool panicked".into(),
            is_retriable: false,
            turn_id: None,
            turn_state: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"error_type\""));
        assert!(json.contains("\"description\""));
        assert!(json.contains("\"is_retriable\""));
        assert!(json.contains("\"turn_id\""));
        assert!(json.contains("\"turn_state\""));
    }
}
