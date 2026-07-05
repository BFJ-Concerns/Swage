//! Tool types shared across the Swage platform.
//!
//! Defines the contract between the tool system, provider layer, storage, and
//! gateway. Types here are provider-agnostic — Anthropic-specific wire format
//! lives in swage-provider.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::message::{ChatMessage, Role, ToolReplayPart};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolResultMetadata {
    pub truncated: bool,
    pub total_bytes: Option<i64>,
}

const MODEL_REPLAY_PREVIEW_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactToolReplay {
    pub kind: String,
    pub bytes: usize,
    pub sha256: String,
    pub truncated: bool,
    pub preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_result_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl CompactToolReplay {
    pub fn to_stable_json(&self) -> String {
        serde_json::to_string(self).expect("compact replay envelope serialises")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultReference {
    pub turn_id: Uuid,
    pub call_id: String,
}

pub fn compact_tool_arguments_for_model(arguments: &serde_json::Value) -> String {
    let raw = stable_json(arguments);
    compact_tool_replay("tool_arguments", &raw, None, None).to_stable_json()
}

pub fn compact_tool_result_for_model(content: &str, is_error: bool) -> String {
    compact_tool_replay("tool_result", content, Some(is_error), None).to_stable_json()
}

pub fn compact_tool_result_for_model_with_reference(
    content: &str,
    is_error: bool,
    full_result_ref: impl Into<String>,
) -> String {
    compact_tool_replay(
        "tool_result",
        content,
        Some(is_error),
        Some(full_result_ref.into()),
    )
    .to_stable_json()
}

pub fn tool_result_reference(turn_id: &str, call_id: &str) -> String {
    format!("tool_call:{turn_id}:{call_id}:result")
}

pub fn parse_tool_result_reference(reference: &str) -> Option<ToolResultReference> {
    let body = reference
        .strip_prefix("tool_call:")?
        .strip_suffix(":result")?;
    let (turn_id, call_id) = body.split_once(':')?;
    let call_id = call_id.trim();
    if call_id.is_empty() {
        return None;
    }
    Some(ToolResultReference {
        turn_id: turn_id.parse().ok()?,
        call_id: call_id.to_string(),
    })
}

pub fn extract_tool_result_metadata(value: &serde_json::Value) -> ToolResultMetadata {
    let Some(object) = value.as_object() else {
        return ToolResultMetadata {
            truncated: false,
            total_bytes: None,
        };
    };

    let stdout_truncated = json_bool(object.get("stdout_truncated")).unwrap_or(false);
    let stderr_truncated = json_bool(object.get("stderr_truncated")).unwrap_or(false);
    let truncated =
        json_bool(object.get("truncated")).unwrap_or(false) || stdout_truncated || stderr_truncated;

    let total_bytes = [
        json_i64(object.get("total_bytes")),
        sum_i64(
            json_i64(object.get("stdout_total_bytes")),
            json_i64(object.get("stderr_total_bytes")),
        ),
    ]
    .into_iter()
    .flatten()
    .max();

    ToolResultMetadata {
        truncated,
        total_bytes,
    }
}

fn json_bool(value: Option<&serde_json::Value>) -> Option<bool> {
    value.and_then(serde_json::Value::as_bool)
}

fn json_i64(value: Option<&serde_json::Value>) -> Option<i64> {
    let value = value?;
    if let Some(number) = value.as_i64() {
        return (number >= 0).then_some(number);
    }
    value.as_u64().and_then(|number| i64::try_from(number).ok())
}

fn sum_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => left.checked_add(right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn compact_tool_replay(
    kind: &'static str,
    raw: &str,
    is_error: Option<bool>,
    full_result_ref: Option<String>,
) -> CompactToolReplay {
    let preview = preview_chars(raw, MODEL_REPLAY_PREVIEW_CHARS);
    CompactToolReplay {
        kind: kind.to_string(),
        bytes: raw.len(),
        sha256: sha256_hex(raw.as_bytes()),
        truncated: preview.len() < raw.len(),
        preview,
        full_result_ref,
        is_error,
    }
}

fn stable_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn preview_chars(raw: &str, max_chars: usize) -> String {
    raw.chars().take(max_chars).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Build the message pair needed to continue a conversation after tool execution.
///
/// Returns two `ChatMessage`s with provider-neutral replay:
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
    build_tool_round_messages_with_reasoning(tool_calls, tool_results, assistant_text, &[])
}

pub fn build_tool_round_messages_with_reasoning(
    tool_calls: &[ToolCall],
    tool_results: &[ToolResult],
    assistant_text: &str,
    reasoning: &[ToolReplayReasoning],
) -> Vec<ChatMessage> {
    let mut assistant_replay: Vec<ToolReplayPart> = Vec::new();

    assistant_replay.extend(reasoning.iter().map(|part| ToolReplayPart::Reasoning {
        content: part.content.clone(),
        details: part.details.clone(),
    }));

    if !assistant_text.is_empty() {
        assistant_replay.push(ToolReplayPart::Text {
            text: assistant_text.to_string(),
        });
    }

    for call in tool_calls {
        assistant_replay.push(ToolReplayPart::ToolCall {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
    }

    let user_replay: Vec<ToolReplayPart> = tool_results
        .iter()
        .map(|result| ToolReplayPart::ToolResult {
            call_id: result.call_id.clone(),
            content: result.content.clone(),
            is_error: result.is_error,
        })
        .collect();

    vec![
        ChatMessage::with_tool_replay(Role::Assistant, assistant_replay),
        ChatMessage::with_tool_replay(Role::User, user_replay),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReplayReasoning {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
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
    fn compact_tool_arguments_for_model_is_deterministic() {
        let arguments = serde_json::json!({"command": "pwd", "limit": 3});

        let first = compact_tool_arguments_for_model(&arguments);
        let second = compact_tool_arguments_for_model(&arguments);

        assert_eq!(first, second);
        let envelope: CompactToolReplay = serde_json::from_str(&first).unwrap();
        assert_eq!(envelope.kind, "tool_arguments");
        assert_eq!(envelope.preview, r#"{"command":"pwd","limit":3}"#);
        assert!(!envelope.truncated);
        assert_eq!(envelope.bytes, envelope.preview.len());
        assert_eq!(envelope.sha256.len(), 64);
        assert_eq!(envelope.full_result_ref, None);
        assert_eq!(envelope.is_error, None);
    }

    #[test]
    fn compact_tool_result_for_model_truncates_and_preserves_error_flag() {
        let content = "x".repeat(MODEL_REPLAY_PREVIEW_CHARS + 20);

        let compact = compact_tool_result_for_model(&content, true);
        let envelope: CompactToolReplay = serde_json::from_str(&compact).unwrap();

        assert_eq!(envelope.kind, "tool_result");
        assert_eq!(envelope.bytes, content.len());
        assert_eq!(envelope.preview.len(), MODEL_REPLAY_PREVIEW_CHARS);
        assert!(envelope.truncated);
        assert_eq!(envelope.full_result_ref, None);
        assert_eq!(envelope.is_error, Some(true));
    }

    #[test]
    fn compact_tool_result_for_model_can_include_full_result_reference() {
        let reference = tool_result_reference("turn_1", "call_1");
        let compact =
            compact_tool_result_for_model_with_reference("full output", false, &reference);

        let envelope: CompactToolReplay = serde_json::from_str(&compact).unwrap();
        assert_eq!(envelope.kind, "tool_result");
        assert_eq!(envelope.preview, "full output");
        assert_eq!(
            envelope.full_result_ref.as_deref(),
            Some(reference.as_str())
        );
        assert_eq!(envelope.is_error, Some(false));
    }

    #[test]
    fn parse_tool_result_reference_recovers_turn_and_call_id() {
        let turn_id = Uuid::parse_str("00000000-0000-0000-0000-000000000123").unwrap();
        let reference = tool_result_reference(&turn_id.to_string(), "call_abc");

        let parsed = parse_tool_result_reference(&reference).unwrap();

        assert_eq!(parsed.turn_id, turn_id);
        assert_eq!(parsed.call_id, "call_abc");
    }

    #[test]
    fn parse_tool_result_reference_rejects_malformed_references() {
        assert!(parse_tool_result_reference("tool_call:not-a-uuid:call_abc:result").is_none());
        assert!(
            parse_tool_result_reference("tool_call:00000000-0000-0000-0000-000000000123::result")
                .is_none()
        );
        assert!(
            parse_tool_result_reference(
                "message:00000000-0000-0000-0000-000000000123:call_abc:result"
            )
            .is_none()
        );
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
        assert_eq!(messages[0].role(), Role::Assistant);
        assert_eq!(messages[1].role(), Role::User);
    }

    #[test]
    fn build_tool_round_messages_assistant_has_canonical_tool_replay() {
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

        let messages = build_tool_round_messages(&calls, &results, "Checking files.");
        let replay = messages[0]
            .tool_replay()
            .expect("assistant should have canonical replay");

        assert_eq!(
            replay,
            [
                ToolReplayPart::Text {
                    text: "Checking files.".into()
                },
                ToolReplayPart::ToolCall {
                    call_id: "call_abc".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({ "command": "ls" }),
                }
            ]
        );
    }

    #[test]
    fn build_tool_round_messages_with_reasoning_keeps_reasoning_before_tool_call() {
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
        let reasoning = vec![ToolReplayReasoning {
            content: "Need to inspect the directory first.".into(),
            details: Some(serde_json::json!({ "provider": "test" })),
        }];

        let messages = build_tool_round_messages_with_reasoning(
            &calls,
            &results,
            "Checking files.",
            &reasoning,
        );
        let replay = messages[0]
            .tool_replay()
            .expect("assistant should have canonical replay");

        assert_eq!(
            replay,
            [
                ToolReplayPart::Reasoning {
                    content: "Need to inspect the directory first.".into(),
                    details: Some(serde_json::json!({ "provider": "test" })),
                },
                ToolReplayPart::Text {
                    text: "Checking files.".into()
                },
                ToolReplayPart::ToolCall {
                    call_id: "call_abc".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({ "command": "ls" }),
                }
            ]
        );
    }

    #[test]
    fn build_tool_round_messages_user_has_canonical_result_replay() {
        let calls = vec![ToolCall {
            call_id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({}),
        }];
        let results = vec![ToolResult {
            call_id: "call_1".into(),
            content: "failed".into(),
            is_error: true,
        }];

        let messages = build_tool_round_messages(&calls, &results, "");
        let replay = messages[1]
            .tool_replay()
            .expect("user should have canonical replay");

        assert_eq!(
            replay,
            [ToolReplayPart::ToolResult {
                call_id: "call_1".into(),
                content: "failed".into(),
                is_error: true,
            }]
        );
    }

    #[test]
    fn build_tool_round_messages_replay_preserves_error_flag() {
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
        let replay = messages[1].tool_replay().unwrap();
        assert_eq!(
            replay,
            [ToolReplayPart::ToolResult {
                call_id: "call_err".into(),
                content: "command not found".into(),
                is_error: true,
            }]
        );
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
            ToolResult {
                call_id: "c1".into(),
                content: "r1".into(),
                is_error: false,
            },
            ToolResult {
                call_id: "c2".into(),
                content: "r2".into(),
                is_error: false,
            },
        ];

        let messages = build_tool_round_messages(&calls, &results, "");
        let assistant_replay = messages[0].tool_replay().unwrap();
        assert_eq!(assistant_replay.len(), 2);
        let user_replay = messages[1].tool_replay().unwrap();
        assert_eq!(user_replay.len(), 2);
    }

    #[test]
    fn extract_tool_result_metadata_reads_standard_truncation_fields() {
        let value = serde_json::json!({
            "output": "preview",
            "truncated": true,
            "total_bytes": 5000
        });

        let metadata = extract_tool_result_metadata(&value);

        assert!(metadata.truncated);
        assert_eq!(metadata.total_bytes, Some(5000));
    }

    #[test]
    fn extract_tool_result_metadata_prefers_larger_shell_stream_totals() {
        let value = serde_json::json!({
            "output": "preview",
            "truncated": true,
            "total_bytes": 64,
            "stdout_truncated": true,
            "stdout_total_bytes": 7000,
            "stderr_truncated": true,
            "stderr_total_bytes": 3000
        });

        let metadata = extract_tool_result_metadata(&value);

        assert!(metadata.truncated);
        assert_eq!(metadata.total_bytes, Some(10_000));
    }
}
