use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Turn lifecycle state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

impl TurnState {
    /// Whether this state can transition to `target`.
    ///
    /// InProgress can transition to any terminal state.
    /// Terminal states (Completed, Failed, Interrupted) cannot transition further.
    pub fn can_transition_to(&self, target: &TurnState) -> bool {
        matches!(
            (self, target),
            (TurnState::InProgress, TurnState::Completed)
                | (TurnState::InProgress, TurnState::Failed)
                | (TurnState::InProgress, TurnState::Interrupted)
        )
    }
}

impl std::fmt::Display for TurnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TurnState::InProgress => "in_progress",
            TurnState::Completed => "completed",
            TurnState::Failed => "failed",
            TurnState::Interrupted => "interrupted",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TurnState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "in_progress" => Ok(TurnState::InProgress),
            "completed" => Ok(TurnState::Completed),
            "failed" => Ok(TurnState::Failed),
            "interrupted" => Ok(TurnState::Interrupted),
            other => Err(format!("unknown turn state: {other}")),
        }
    }
}

/// A project groups conversations and provides configuration scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    /// Filesystem-safe identifier used for the project's directory under
    /// `~/.swage/projects/{slug}/`.
    pub slug: String,
    pub workspace_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A session (conversation) within a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A single agent turn within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    pub session_id: Uuid,
    pub state: TurnState,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// A message within a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub turn_id: Uuid,
    pub role: String,
    pub content: String,
    pub position: i32,
    pub created_at: DateTime<Utc>,
}

/// A tool call within a turn — persisted for conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: Uuid,
    pub turn_id: Uuid,
    /// Provider-assigned identifier for this individual call (e.g. Anthropic `tool_use` id).
    pub call_id: String,
    /// Name of the tool that was called.
    pub tool: String,
    /// Tool arguments serialised as a JSON string.
    pub arguments: String,
    /// Raw output returned by the tool, if it has completed.
    pub result: Option<String>,
    /// Execution outcome ("success", "error", etc.).
    pub status: Option<String>,
    /// Wall-clock execution time in milliseconds.
    pub duration_ms: Option<i64>,
    /// True when the result was cut off due to length limits.
    pub truncated: bool,
    /// Total byte count of the untruncated result, for reporting.
    pub total_bytes: Option<i64>,
    /// Ordering index within the parent turn.
    pub position: i32,
    pub created_at: DateTime<Utc>,
    /// call_id of the enclosing tool call, if this is a sub-tool invocation.
    ///
    /// None for native (top-level) tool calls. Set when a tool such as
    /// python_execute dispatches further tool calls on behalf of the model.
    pub parent_call_id: Option<String>,
}

/// Input for inserting a new tool call record.
///
/// `id` and `created_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewToolCall {
    pub turn_id: Uuid,
    pub call_id: String,
    pub tool: String,
    pub arguments: String,
    pub result: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<i64>,
    pub truncated: bool,
    pub total_bytes: Option<i64>,
    pub position: i32,
    /// call_id of the enclosing tool call, if this is a sub-tool invocation.
    ///
    /// None for native (top-level) tool calls.
    pub parent_call_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Creation types — inputs for insert operations (no auto-generated fields)
// ---------------------------------------------------------------------------

/// Input for creating a new project.
#[derive(Debug, Clone)]
pub struct NewProject {
    pub name: String,
    /// Filesystem-safe identifier for the project directory.
    pub slug: String,
    pub workspace_path: Option<String>,
}

/// Input for creating a new session.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub project_id: Uuid,
}

/// Input for creating a new turn.
#[derive(Debug, Clone)]
pub struct NewTurn {
    pub session_id: Uuid,
    pub state: TurnState,
}

/// Input for creating a new message.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub turn_id: Uuid,
    pub role: String,
    pub content: String,
    pub position: i32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- ToolCallRecord serialisation ---

    #[test]
    fn tool_call_record_serialises_full() {
        let id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let now = Utc::now();
        let record = ToolCallRecord {
            id,
            turn_id,
            call_id: "toolu_01".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
            result: Some("file1\nfile2".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(42),
            truncated: false,
            total_bytes: Some(20),
            position: 0,
            created_at: now,
            parent_call_id: None,
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.turn_id, turn_id);
        assert_eq!(back.call_id, "toolu_01");
        assert_eq!(back.tool, "shell");
        assert_eq!(back.arguments, r#"{"command":"ls"}"#);
        assert_eq!(back.result, Some("file1\nfile2".to_string()));
        assert_eq!(back.status, Some("success".to_string()));
        assert_eq!(back.duration_ms, Some(42));
        assert!(!back.truncated);
        assert_eq!(back.total_bytes, Some(20));
        assert_eq!(back.position, 0);
    }

    #[test]
    fn tool_call_record_serialises_with_none_fields() {
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            call_id: "toolu_02".to_string(),
            tool: "file_read".to_string(),
            arguments: r#"{"path":"/tmp/x"}"#.to_string(),
            result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            position: 1,
            created_at: Utc::now(),
            parent_call_id: None,
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert!(back.result.is_none());
        assert!(back.status.is_none());
        assert!(back.duration_ms.is_none());
        assert!(back.total_bytes.is_none());
        assert_eq!(back.position, 1);
    }

    #[test]
    fn new_tool_call_is_debug_clone() {
        let original = NewToolCall {
            turn_id: Uuid::new_v4(),
            call_id: "toolu_03".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"pwd"}"#.to_string(),
            result: Some("ok".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(10),
            truncated: false,
            total_bytes: Some(2),
            position: 0,
            parent_call_id: None,
        };
        let cloned = original.clone();
        // Debug: just ensure it formats without panic.
        let _ = format!("{cloned:?}");
        assert_eq!(original.tool, cloned.tool);
    }

    #[test]
    fn tool_call_record_carries_parent_call_id() {
        let parent_id = "toolu_parent".to_string();
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            call_id: "toolu_sub".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            position: 0,
            created_at: Utc::now(),
            parent_call_id: Some(parent_id.clone()),
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.parent_call_id, Some(parent_id));
    }

    #[test]
    fn new_tool_call_carries_parent_call_id() {
        let sub = NewToolCall {
            turn_id: Uuid::new_v4(),
            call_id: "sub_01".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{}"#.to_string(),
            result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            position: 0,
            parent_call_id: Some("parent_01".to_string()),
        };
        assert_eq!(sub.parent_call_id.as_deref(), Some("parent_01"));

        // Top-level calls use None.
        let top = NewToolCall {
            parent_call_id: None,
            ..sub.clone()
        };
        assert!(top.parent_call_id.is_none());
    }

    // --- Project slug field ---

    #[test]
    fn project_slug_field_present_and_serialises() {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let project = Project {
            id,
            name: "my-project".to_string(),
            slug: "my-project".to_string(),
            workspace_path: None,
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string(&project).expect("serialisation should succeed");
        assert!(json.contains("\"slug\":\"my-project\""), "slug should appear in JSON");
    }

    #[test]
    fn new_project_slug_field_present() {
        let np = NewProject {
            name: "test".to_string(),
            slug: "test".to_string(),
            workspace_path: None,
        };
        assert_eq!(np.slug, "test");
    }

    #[test]
    fn turn_state_valid_transitions() {
        assert!(TurnState::InProgress.can_transition_to(&TurnState::Completed));
        assert!(TurnState::InProgress.can_transition_to(&TurnState::Failed));
        assert!(TurnState::InProgress.can_transition_to(&TurnState::Interrupted));
    }

    #[test]
    fn turn_state_invalid_transitions() {
        // Terminal states cannot transition.
        assert!(!TurnState::Completed.can_transition_to(&TurnState::Failed));
        assert!(!TurnState::Failed.can_transition_to(&TurnState::Completed));
        assert!(!TurnState::Interrupted.can_transition_to(&TurnState::Completed));
        // InProgress cannot transition to itself.
        assert!(!TurnState::InProgress.can_transition_to(&TurnState::InProgress));
    }

    #[test]
    fn turn_state_display_from_str_round_trip() {
        let states = [
            TurnState::InProgress,
            TurnState::Completed,
            TurnState::Failed,
            TurnState::Interrupted,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: TurnState = s.parse().expect("round-trip should succeed");
            assert_eq!(state, &parsed);
        }
    }

    #[test]
    fn turn_state_from_str_unknown() {
        let result = "bogus".parse::<TurnState>();
        assert!(result.is_err());
    }
}
