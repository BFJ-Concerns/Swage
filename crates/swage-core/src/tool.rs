//! Tool types shared across the Swage platform.
//!
//! Defines the contract between the tool system, provider layer, storage, and
//! gateway. Types here are provider-agnostic — Anthropic-specific wire format
//! lives in swage-provider.

use serde::{Deserialize, Serialize};

use crate::message::{ChatMessage, Role};

/// Describes a tool the agent can invoke.
///
/// The `input_schema` is a JSON Schema object (type: "object") describing
/// the parameters the tool accepts. Providers use this to tell the model
/// what arguments to supply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

/// A specific invocation of a tool by the model.
///
/// `call_id` is assigned by the provider and used to correlate this call
/// with its `ToolResult` across async execution boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned identifier; links this call to its result.
    pub call_id: String,
    pub name: String,
    /// Arguments the model supplied, matching the tool's `input_schema`.
    pub arguments: serde_json::Value,
}

/// The outcome of executing a tool call.
///
/// `is_error` distinguishes a tool that ran but produced an error message
/// (e.g. command not found, permission denied) from one that succeeded.
/// Both cases carry a `content` string the model can read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Matches the `call_id` on the originating `ToolCall`.
    pub call_id: String,
    pub content: String,
    /// True when the tool execution failed; false on success.
    pub is_error: bool,
}

/// Build the message pair needed to continue a conversation after tool execution.
///
/// Returns two `ChatMessage`s with structured `content_blocks`:
/// 1. An **assistant** message reconstructing the model's tool_use output
///    (optionally preceded by text the model produced before calling tools).
/// 2. A **user** message carrying one `tool_result` block per executed tool.
///
/// These are appended to the conversation history before the next streaming
/// request so the model can see its own invocations and their outcomes.
pub fn build_tool_round_messages(
    tool_calls: &[ToolCall],
    tool_results: &[ToolResult],
    assistant_text: &str,
) -> Vec<ChatMessage> {
    // --- assistant message: text (if any) + tool_use blocks ---
    let mut assistant_blocks: Vec<serde_json::Value> = Vec::new();

    if !assistant_text.is_empty() {
        assistant_blocks.push(serde_json::json!({
            "type": "text",
            "text": assistant_text,
        }));
    }

    for call in tool_calls {
        assistant_blocks.push(serde_json::json!({
            "type": "tool_use",
            "id": call.call_id,
            "name": call.name,
            "input": call.arguments,
        }));
    }

    // --- user message: tool_result blocks ---
    let user_blocks: Vec<serde_json::Value> = tool_results
        .iter()
        .map(|result| {
            let mut block = serde_json::json!({
                "type": "tool_result",
                "tool_use_id": result.call_id,
                "content": result.content,
            });
            // Only include is_error when true — omitting it keeps the wire
            // format clean for the common success case.
            if result.is_error {
                block["is_error"] = serde_json::Value::Bool(true);
            }
            block
        })
        .collect();

    vec![
        ChatMessage::with_blocks(Role::Assistant, assistant_blocks),
        ChatMessage::with_blocks(Role::User, user_blocks),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definition_serde_round_trip() {
        let def = ToolDefinition {
            name: "bash".into(),
            description: "Run a shell command".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "command": { "type": "string" } }
            }),
        };
        let json = serde_json::to_string(&def).unwrap();
        let decoded: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, decoded);
    }

    #[test]
    fn tool_call_serde_round_trip() {
        let call = ToolCall {
            call_id: "call_abc123".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "ls -la" }),
        };
        let json = serde_json::to_string(&call).unwrap();
        let decoded: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(call, decoded);
    }

    #[test]
    fn tool_result_serde_round_trip() {
        // Successful tool output
        let result_ok = ToolResult {
            call_id: "call_abc123".into(),
            content: "total 42\ndrwxr-xr-x  2 user group  64 Jan  1 00:00 .".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&result_ok).unwrap();
        let decoded: ToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result_ok, decoded);

        // Error output — is_error distinguishes it from successful output
        let result_err = ToolResult {
            call_id: "call_xyz789".into(),
            content: "bash: notacommand: command not found".into(),
            is_error: true,
        };
        let json = serde_json::to_string(&result_err).unwrap();
        let decoded: ToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result_err, decoded);
    }

    #[test]
    fn call_id_correlates_call_and_result() {
        // The same call_id must appear on both the ToolCall and its ToolResult
        // so the execution loop can match them across async boundaries.
        let call = ToolCall {
            call_id: "call_correlation_test".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "/tmp/test.txt" }),
        };
        let result = ToolResult {
            call_id: call.call_id.clone(),
            content: "file contents".into(),
            is_error: false,
        };
        assert_eq!(call.call_id, result.call_id);
    }

    // -----------------------------------------------------------------------
    // build_tool_round_messages tests
    // -----------------------------------------------------------------------

    #[test]
    fn build_tool_round_messages_produces_assistant_and_user_pair() {
        let calls = vec![ToolCall {
            call_id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hello" }),
        }];
        let results = vec![ToolResult {
            call_id: "call_1".into(),
            content: "hello".into(),
            is_error: false,
        }];

        let messages = build_tool_round_messages(&calls, &results, "");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[1].role, Role::User);
    }

    #[test]
    fn build_tool_round_messages_assistant_has_tool_use_blocks() {
        let calls = vec![ToolCall {
            call_id: "call_abc".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "ls" }),
        }];
        let results = vec![ToolResult {
            call_id: "call_abc".into(),
            content: "file.txt".into(),
            is_error: false,
        }];

        let messages = build_tool_round_messages(&calls, &results, "");
        let blocks = messages[0].content_blocks.as_ref().expect("should have blocks");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["id"], "call_abc");
        assert_eq!(blocks[0]["name"], "bash");
    }

    #[test]
    fn build_tool_round_messages_prepends_text_when_present() {
        let calls = vec![ToolCall {
            call_id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({}),
        }];
        let results = vec![ToolResult {
            call_id: "call_1".into(),
            content: "ok".into(),
            is_error: false,
        }];

        let messages = build_tool_round_messages(&calls, &results, "Let me try that.");
        let blocks = messages[0].content_blocks.as_ref().unwrap();
        // First block should be text, second should be tool_use.
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Let me try that.");
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn build_tool_round_messages_user_has_tool_result_blocks() {
        let calls = vec![ToolCall {
            call_id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({}),
        }];
        let results = vec![ToolResult {
            call_id: "call_1".into(),
            content: "echoed".into(),
            is_error: false,
        }];

        let messages = build_tool_round_messages(&calls, &results, "");
        let blocks = messages[1].content_blocks.as_ref().expect("should have blocks");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "call_1");
        assert_eq!(blocks[0]["content"], "echoed");
        // is_error omitted when false
        assert!(blocks[0].get("is_error").is_none());
    }

    #[test]
    fn build_tool_round_messages_includes_is_error_when_true() {
        let calls = vec![ToolCall {
            call_id: "call_err".into(),
            name: "bash".into(),
            arguments: serde_json::json!({}),
        }];
        let results = vec![ToolResult {
            call_id: "call_err".into(),
            content: "command not found".into(),
            is_error: true,
        }];

        let messages = build_tool_round_messages(&calls, &results, "");
        let blocks = messages[1].content_blocks.as_ref().unwrap();
        assert_eq!(blocks[0]["is_error"], true);
    }

    #[test]
    fn build_tool_round_messages_multiple_tools() {
        let calls = vec![
            ToolCall {
                call_id: "c1".into(),
                name: "a".into(),
                arguments: serde_json::json!({}),
            },
            ToolCall {
                call_id: "c2".into(),
                name: "b".into(),
                arguments: serde_json::json!({}),
            },
        ];
        let results = vec![
            ToolResult { call_id: "c1".into(), content: "r1".into(), is_error: false },
            ToolResult { call_id: "c2".into(), content: "r2".into(), is_error: false },
        ];

        let messages = build_tool_round_messages(&calls, &results, "");
        let assistant_blocks = messages[0].content_blocks.as_ref().unwrap();
        assert_eq!(assistant_blocks.len(), 2);
        let user_blocks = messages[1].content_blocks.as_ref().unwrap();
        assert_eq!(user_blocks.len(), 2);
    }
}
