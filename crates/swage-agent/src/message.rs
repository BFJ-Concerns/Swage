//! Agent boundary types — the serialisable messages exchanged between caller and agent.
//!
//! All types here are designed for future IPC compatibility: they serialise cleanly
//! to JSON and carry no references into agent internals.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_storage::TurnState;

/// A message from the caller into the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    /// The user's message text.
    pub message: String,
    /// Target session.
    pub session_id: Uuid,
    /// Target thread within session; defaults to the active thread if absent.
    pub thread_id: Option<Uuid>,
}

#[cfg(test)]
mod inbound_message_tests {
    use super::*;

    #[test]
    fn round_trip_with_thread_id() {
        let original = InboundMessage {
            message: "Hello, agent!".into(),
            session_id: Uuid::new_v4(),
            thread_id: Some(Uuid::new_v4()),
        };
        let json = serde_json::to_string(&original).expect("serialise");
        let decoded: InboundMessage = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(decoded.message, original.message);
        assert_eq!(decoded.session_id, original.session_id);
        assert_eq!(decoded.thread_id, original.thread_id);
    }

    #[test]
    fn round_trip_without_thread_id() {
        let original = InboundMessage {
            message: "No thread specified".into(),
            session_id: Uuid::new_v4(),
            thread_id: None,
        };
        let json = serde_json::to_string(&original).expect("serialise");
        let decoded: InboundMessage = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(decoded.message, original.message);
        assert_eq!(decoded.session_id, original.session_id);
        assert!(decoded.thread_id.is_none());
    }

    #[test]
    fn all_fields_present_in_json() {
        let session_id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let msg = InboundMessage {
            message: "test".into(),
            session_id,
            thread_id: Some(thread_id),
        };
        let json = serde_json::to_string(&msg).expect("serialise");
        // All three fields must appear in the output.
        assert!(json.contains("\"message\""), "missing message field");
        assert!(json.contains("\"session_id\""), "missing session_id field");
        assert!(json.contains("\"thread_id\""), "missing thread_id field");
    }

    #[test]
    fn clone_is_equal() {
        let msg = InboundMessage {
            message: "clone me".into(),
            session_id: Uuid::new_v4(),
            thread_id: None,
        };
        assert_eq!(format!("{:?}", msg), format!("{:?}", msg.clone()));
    }
}

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
    }

    #[test]
    fn all_fields_present_in_json() {
        let json = serde_json::to_string(&sample()).expect("serialise");
        assert!(json.contains("\"response\""), "missing response field");
        assert!(json.contains("\"turn_id\""), "missing turn_id field");
        assert!(json.contains("\"input_tokens\""), "missing input_tokens field");
        assert!(json.contains("\"output_tokens\""), "missing output_tokens field");
        assert!(json.contains("\"tool_calls_count\""), "missing tool_calls_count field");
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
        };
        let json = serde_json::to_string(&msg).expect("serialise");
        let decoded: OutboundMessage = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(decoded.input_tokens, 0);
        assert_eq!(decoded.output_tokens, 0);
        assert_eq!(decoded.tool_calls_count, 0);
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
