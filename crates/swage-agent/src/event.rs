//! Streaming event types emitted by the agent during turn processing.
//!
//! The gateway (or any consumer) receives these events through a channel
//! and forwards them to connected clients. This decouples the agent's
//! execution loop from transport concerns.

use uuid::Uuid;

/// Events emitted by the agent over a channel during streaming turn execution.
///
/// All variants are `Send + 'static`, making them safe to transport through
/// tokio unbounded channels from the agent to any number of consumers.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// The turn record has been created and processing is beginning.
    TurnStarted { turn_id: Uuid },

    /// A streaming token arrived from the LLM.
    Token { content: String },

    /// A tool call has started; consumers can show a "running…" indicator.
    ToolStart {
        call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },

    /// A tool call has finished.
    ToolEnd {
        call_id: String,
        tool_name: String,
        result: serde_json::Value,
        /// "success" | "error" | "cancelled"
        status: String,
        /// Wall-clock time of the tool execution, if available.
        duration_ms: Option<u64>,
    },

    /// The turn has completed successfully; token counts are final.
    TurnCompleted {
        turn_id: Uuid,
        input_tokens: u32,
        output_tokens: u32,
        tool_calls_count: u32,
    },

    /// A non-recoverable error occurred during turn processing.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turn_started_constructible() {
        let turn_id = Uuid::new_v4();
        let event = AgentEvent::TurnStarted { turn_id };
        if let AgentEvent::TurnStarted { turn_id: id } = event {
            assert_eq!(id, turn_id);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_token_constructible() {
        let event = AgentEvent::Token { content: "hello".to_string() };
        if let AgentEvent::Token { content } = event {
            assert_eq!(content, "hello");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_tool_start_constructible() {
        let event = AgentEvent::ToolStart {
            call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            arguments: serde_json::json!({"cmd": "ls"}),
        };
        if let AgentEvent::ToolStart { call_id, tool_name, arguments } = event {
            assert_eq!(call_id, "call-1");
            assert_eq!(tool_name, "bash");
            assert_eq!(arguments["cmd"], "ls");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_tool_end_constructible() {
        let event = AgentEvent::ToolEnd {
            call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            result: serde_json::json!({"output": "file.txt"}),
            status: "success".to_string(),
            duration_ms: Some(42),
        };
        if let AgentEvent::ToolEnd { call_id, tool_name, result, status, duration_ms } = event {
            assert_eq!(call_id, "call-1");
            assert_eq!(tool_name, "bash");
            assert_eq!(result["output"], "file.txt");
            assert_eq!(status, "success");
            assert_eq!(duration_ms, Some(42));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_tool_end_no_duration() {
        let event = AgentEvent::ToolEnd {
            call_id: "call-2".to_string(),
            tool_name: "read_file".to_string(),
            result: serde_json::Value::Null,
            status: "error".to_string(),
            duration_ms: None,
        };
        if let AgentEvent::ToolEnd { duration_ms, .. } = event {
            assert!(duration_ms.is_none());
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_turn_completed_constructible() {
        let turn_id = Uuid::new_v4();
        let event = AgentEvent::TurnCompleted {
            turn_id,
            input_tokens: 100,
            output_tokens: 200,
            tool_calls_count: 3,
        };
        if let AgentEvent::TurnCompleted { turn_id: id, input_tokens, output_tokens, tool_calls_count } = event {
            assert_eq!(id, turn_id);
            assert_eq!(input_tokens, 100);
            assert_eq!(output_tokens, 200);
            assert_eq!(tool_calls_count, 3);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_error_constructible() {
        let event = AgentEvent::Error { message: "something went wrong".to_string() };
        if let AgentEvent::Error { message } = event {
            assert_eq!(message, "something went wrong");
        } else {
            panic!("unexpected variant");
        }
    }

    #[tokio::test]
    async fn test_channel_send() {
        // Verify all variants can traverse a tokio unbounded channel (Send + 'static).
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let turn_id = Uuid::new_v4();

        tx.send(AgentEvent::TurnStarted { turn_id }).unwrap();
        tx.send(AgentEvent::Token { content: "tok".to_string() }).unwrap();
        tx.send(AgentEvent::ToolStart {
            call_id: "c".to_string(),
            tool_name: "t".to_string(),
            arguments: serde_json::Value::Null,
        }).unwrap();
        tx.send(AgentEvent::ToolEnd {
            call_id: "c".to_string(),
            tool_name: "t".to_string(),
            result: serde_json::Value::Null,
            status: "success".to_string(),
            duration_ms: None,
        }).unwrap();
        tx.send(AgentEvent::TurnCompleted {
            turn_id,
            input_tokens: 1,
            output_tokens: 2,
            tool_calls_count: 0,
        }).unwrap();
        tx.send(AgentEvent::Error { message: "err".to_string() }).unwrap();

        // Drain the channel — all 6 events must arrive.
        let mut received = 0;
        drop(tx);
        while rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(received, 6);
    }

    #[test]
    fn test_debug_and_clone() {
        let event = AgentEvent::Token { content: "x".to_string() };
        let cloned = event.clone();
        let debug_str = format!("{:?}", cloned);
        assert!(debug_str.contains("Token"));
    }
}
