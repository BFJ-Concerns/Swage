//! Conversation message types with multimodal content support.
//!
//! Defines `ChatMessage`, `ContentPart`, `Role`, `ToolCall`, and
//! `sanitize_tool_messages` for representing conversation history across LLM
//! providers. Supports text and image content, along with Anthropic-specific
//! structured content blocks used in tool-use rounds.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

/// Role of a participant in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Tool result messages — produced by the execution layer after a tool invocation.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown chat role: {value}")]
pub struct RoleParseError {
    value: String,
}

impl RoleParseError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        write!(f, "{s}")
    }
}

impl FromStr for Role {
    type Err = RoleParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(Role::System),
            "user" => Ok(Role::User),
            "assistant" => Ok(Role::Assistant),
            "tool" => Ok(Role::Tool),
            other => Err(RoleParseError::new(other)),
        }
    }
}

/// A single piece of content within a message — text or image reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text content.
    Text { text: String },

    /// Reference to an image by URL.
    ///
    /// An empty URL is representable but semantically invalid — providers
    /// will reject it. Callers should validate before sending.
    ImageUrl { url: String },
}

/// A single message in a conversation, potentially containing multimodal content.
///
/// The `content` field holds structured `ContentPart`s for text and image input.
/// The `content_blocks` field holds Anthropic-specific JSON blocks used when
/// representing tool_use/tool_result pairs in the conversation history — these
/// are built by `crate::tool::build_tool_round_messages`.
// TODO Make ChatMessage cross-field invariants type-level
// Public fields plus convenience constructors let callers build a
// ChatMessage where (role, content, content_blocks, tool_call_id,
// tool_calls) are mutually inconsistent — e.g. tool_call_id set on a
// non-Tool role, or both content and content_blocks populated. Convert
// to a sealed enum with one variant per role, or hide the fields and
// expose constructors that enforce the cross-field rules.
// ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Provider-independent message identifier.
    pub id: Uuid,
    /// Who sent the message.
    pub role: Role,
    /// Content parts — may be empty for messages that only carry tool calls
    /// or structured `content_blocks`.
    pub content: Vec<ContentPart>,
    /// When the message was created (UTC).
    pub created_at: DateTime<Utc>,
    /// Optional display name for the participant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Identifies which tool call this message responds to (Role::Tool messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool calls requested by the assistant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<crate::tool::ToolCall>>,
    /// Anthropic-specific structured content blocks (tool_use, tool_result).
    ///
    /// When present, providers use these instead of the `content` vec.
    /// Built by `crate::tool::build_tool_round_messages`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content_blocks: Option<Vec<serde_json::Value>>,
}

impl ChatMessage {
    /// Create a new text message with a generated UUID and current UTC timestamp.
    ///
    /// The `content` string is wrapped in a single `ContentPart::Text`.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role,
            content: vec![ContentPart::Text {
                text: content.into(),
            }],
            created_at: Utc::now(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            content_blocks: None,
        }
    }

    /// Create a message with structured Anthropic content blocks instead of
    /// plain text parts.
    ///
    /// The `content` field is left empty since the blocks carry the actual
    /// payload. Providers check `content_blocks` first.
    pub fn with_blocks(role: Role, blocks: Vec<serde_json::Value>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role,
            content: Vec::new(),
            created_at: Utc::now(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            content_blocks: Some(blocks),
        }
    }

    /// Create a new message from explicit content parts, with a generated UUID
    /// and current UTC timestamp.
    ///
    /// Use this when constructing multimodal messages (e.g. text + image).
    pub fn new_parts(role: Role, content: Vec<ContentPart>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role,
            content,
            created_at: Utc::now(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            content_blocks: None,
        }
    }
}

impl PartialEq for ChatMessage {
    fn eq(&self, other: &Self) -> bool {
        // Compare all fields including id and created_at for exact equality.
        self.id == other.id
            && self.role == other.role
            && self.content == other.content
            && self.created_at == other.created_at
            && self.name == other.name
            && self.tool_call_id == other.tool_call_id
            && self.tool_calls == other.tool_calls
            && self.content_blocks == other.content_blocks
    }
}

/// Ensures tool-result messages (Role::Tool) always follow a corresponding
/// tool-use message. Orphaned tool results — those whose `tool_call_id` does
/// not match any `ToolCall.call_id` in preceding assistant messages — are
/// removed.
///
/// This prevents provider errors caused by dangling tool results in the
/// conversation history (e.g. after message truncation or editing).
pub fn sanitize_tool_messages(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    // Collect all tool-call IDs issued by assistant messages seen so far.
    let mut issued_call_ids = std::collections::HashSet::new();

    messages
        .into_iter()
        .filter(|message| {
            // Accumulate tool-call IDs from assistant messages before filtering.
            // We handle both branches explicitly because the borrow checker
            // prevents mutating the set inside a closure that also borrows the
            // message for the Role::Tool check.
            match message.role {
                Role::Assistant => {
                    if let Some(ref calls) = message.tool_calls {
                        for call in calls {
                            issued_call_ids.insert(call.call_id.clone());
                        }
                    }
                    true
                }
                Role::Tool => {
                    // Keep only if the tool_call_id matches a previously issued call.
                    message
                        .tool_call_id
                        .as_ref()
                        .is_some_and(|id| issued_call_ids.contains(id))
                }
                _ => true,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Construction ----------------------------------------------------------

    #[test]
    fn new_creates_text_message_with_id_and_timestamp() {
        let msg = ChatMessage::new(Role::User, "hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(
            msg.content,
            vec![ContentPart::Text {
                text: "hello".into()
            }]
        );
        // id should be a non-nil UUID
        assert_ne!(msg.id, Uuid::nil());
        // created_at should be recent (within last 5 seconds)
        let age = Utc::now() - msg.created_at;
        assert!(age.num_seconds() < 5);
        assert!(msg.content_blocks.is_none());
    }

    #[test]
    fn new_has_no_optional_fields() {
        let msg = ChatMessage::new(Role::Assistant, "hi");
        assert!(msg.name.is_none());
        assert!(msg.tool_call_id.is_none());
        assert!(msg.tool_calls.is_none());
        assert!(msg.content_blocks.is_none());
    }

    #[test]
    fn new_parts_creates_multimodal_message() {
        let parts = vec![
            ContentPart::Text {
                text: "Look at this:".into(),
            },
            ContentPart::ImageUrl {
                url: "https://example.com/photo.jpg".into(),
            },
        ];
        let msg = ChatMessage::new_parts(Role::User, parts.clone());
        assert_eq!(msg.content, parts);
        assert_ne!(msg.id, Uuid::nil());
        let age = Utc::now() - msg.created_at;
        assert!(age.num_seconds() < 5);
    }

    #[test]
    fn with_blocks_sets_blocks_and_empty_content() {
        let blocks = vec![serde_json::json!({ "type": "text", "text": "hi" })];
        let msg = ChatMessage::with_blocks(Role::Assistant, blocks.clone());
        assert_eq!(msg.role, Role::Assistant);
        assert!(msg.content.is_empty());
        assert_eq!(msg.content_blocks, Some(blocks));
    }

    // -- Serialisation round-trip ---------------------------------------------

    #[test]
    fn role_serialises_lowercase() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn role_display_lowercase() {
        assert_eq!(Role::User.to_string(), "user");
        assert_eq!(Role::Assistant.to_string(), "assistant");
        assert_eq!(Role::System.to_string(), "system");
        assert_eq!(Role::Tool.to_string(), "tool");
    }

    #[test]
    fn role_from_str_rejects_unknown_role() {
        let error = "moderator"
            .parse::<Role>()
            .expect_err("unknown role should fail");
        assert_eq!(error.to_string(), "unknown chat role: moderator");
    }

    #[test]
    fn chat_message_roundtrips_through_json() {
        let original = ChatMessage::new_parts(
            Role::User,
            vec![
                ContentPart::Text {
                    text: "What is this?".into(),
                },
                ContentPart::ImageUrl {
                    url: "https://example.com/img.png".into(),
                },
            ],
        );
        // Attach optional fields manually to test their round-trip.
        let mut original = original;
        original.name = Some("alice".into());

        let json = serde_json::to_string(&original).unwrap();
        let restored: ChatMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.role, Role::User);
        assert_eq!(restored.content.len(), 2);
        assert_eq!(restored.name, Some("alice".into()));
        assert!(restored.tool_call_id.is_none());
        assert!(restored.tool_calls.is_none());
    }

    #[test]
    fn optional_fields_omitted_from_json_when_none() {
        let msg = ChatMessage::new(Role::User, "hi");

        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("\"name\""));
        assert!(!json.contains("tool_call_id"));
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("content_blocks"));
    }

    #[test]
    fn chat_message_with_tool_calls_roundtrips() {
        let mut msg = ChatMessage::new(Role::Assistant, "");
        msg.content = Vec::new();
        msg.tool_calls = Some(vec![crate::tool::ToolCall {
            call_id: "call_abc".into(),
            name: "search".into(),
            arguments: serde_json::json!({"query": "rust"}),
        }]);

        let json = serde_json::to_string(&msg).unwrap();
        let restored: ChatMessage = serde_json::from_str(&json).unwrap();

        let calls = restored.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "call_abc");
    }

    #[test]
    fn chat_message_serde_round_trip() {
        let msg = ChatMessage::new(Role::Assistant, "world");
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn chat_message_with_blocks_serde_round_trip() {
        let blocks = vec![serde_json::json!({
            "type": "tool_use", "id": "call_1", "name": "echo", "input": {}
        })];
        let msg = ChatMessage::with_blocks(Role::Assistant, blocks);
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("content_blocks"));
        let decoded: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn chat_message_without_content_blocks_deserialises_from_legacy_json() {
        // JSON produced before content_blocks existed must still deserialise.
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","role":"user","content":[{"type":"text","text":"hi"}],"created_at":"2024-01-01T00:00:00Z"}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, vec![ContentPart::Text { text: "hi".into() }]);
        assert!(msg.content_blocks.is_none());
    }

    // -- sanitize_tool_messages -----------------------------------------------

    #[test]
    fn sanitize_removes_orphaned_tool_result() {
        let messages = vec![
            ChatMessage::new(Role::User, "hello"),
            // Orphan: no preceding assistant message issued call_999
            {
                let mut m = ChatMessage::new(Role::Tool, "result");
                m.tool_call_id = Some("call_999".into());
                m
            },
        ];

        let sanitized = sanitize_tool_messages(messages);
        assert_eq!(sanitized.len(), 1);
        assert_eq!(sanitized[0].role, Role::User);
    }

    #[test]
    fn sanitize_keeps_valid_tool_result() {
        let assistant_msg = {
            let mut m = ChatMessage::new(Role::Assistant, "");
            m.content = Vec::new();
            m.tool_calls = Some(vec![crate::tool::ToolCall {
                call_id: "call_1".into(),
                name: "search".into(),
                arguments: serde_json::json!({}),
            }]);
            m
        };
        let tool_msg = {
            let mut m = ChatMessage::new(Role::Tool, "found it");
            m.tool_call_id = Some("call_1".into());
            m
        };

        let sanitized = sanitize_tool_messages(vec![assistant_msg, tool_msg]);
        assert_eq!(sanitized.len(), 2);
        assert_eq!(sanitized[1].role, Role::Tool);
    }

    #[test]
    fn sanitize_removes_tool_result_without_tool_call_id() {
        // A Tool message with no tool_call_id is always orphaned.
        let messages = vec![ChatMessage::new(Role::Tool, "stray")];
        let sanitized = sanitize_tool_messages(messages);
        assert!(sanitized.is_empty());
    }

    #[test]
    fn sanitize_preserves_non_tool_messages() {
        let messages = vec![
            ChatMessage::new(Role::System, "system prompt"),
            ChatMessage::new(Role::User, "hi"),
            ChatMessage::new(Role::Assistant, "hello"),
        ];
        let sanitized = sanitize_tool_messages(messages);
        assert_eq!(sanitized.len(), 3);
    }

    #[test]
    fn sanitize_handles_mixed_valid_and_orphaned_tool_results() {
        let assistant_msg = {
            let mut m = ChatMessage::new(Role::Assistant, "");
            m.content = Vec::new();
            m.tool_calls = Some(vec![crate::tool::ToolCall {
                call_id: "call_a".into(),
                name: "tool_a".into(),
                arguments: serde_json::json!({}),
            }]);
            m
        };
        // Valid — matches call_a
        let valid_tool = {
            let mut m = ChatMessage::new(Role::Tool, "result a");
            m.tool_call_id = Some("call_a".into());
            m
        };
        // Orphan — call_b was never issued
        let orphaned_tool = {
            let mut m = ChatMessage::new(Role::Tool, "result b");
            m.tool_call_id = Some("call_b".into());
            m
        };

        let sanitized = sanitize_tool_messages(vec![assistant_msg, valid_tool, orphaned_tool]);
        assert_eq!(sanitized.len(), 2);
        assert_eq!(sanitized[0].role, Role::Assistant);
        assert_eq!(sanitized[1].role, Role::Tool);
        assert_eq!(sanitized[1].tool_call_id, Some("call_a".into()));
    }
}
