//! Conversation HTTP request and response bodies.

use crate::client::settings::Effort;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Pending,
    Processing,
    Completed,
    Failed,
    Interrupted,
    AwaitingApproval,
    Delegating,
}

/// Request body for the conversation and topic-session creation endpoints.
///
/// Every field is optional and the body itself may be omitted: an absent
/// `agent_id` selects the default `"gateway"` agent, and an absent
/// `selected_model_id` leaves the session's model unselected. A named
/// `agent_id` must be `"gateway"` or an agent definition visible to the
/// target topic; the gateway rejects anything else with a validation error.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct CreateConversationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub selected_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub effort: Option<Effort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ConversationResponse {
    pub id: Uuid,
    /// The topic the session runs under; absent for a session started
    /// without one, which runs in the general scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<Uuid>,
    /// The agent definition the session runs under.
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ConversationSummary {
    pub id: Uuid,
    /// The agent definition the session runs under.
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_preview: Option<String>,
    pub has_interrupted_turn: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ConversationDetail {
    pub id: Uuid,
    /// The topic the session runs under; absent for a session in the
    /// general scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<Uuid>,
    /// The agent definition the session runs under.
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<MessageView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ConversationBranchResponse {
    pub child_thread_id: Uuid,
    pub parent_thread_id: Uuid,
    pub branched_from_message_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct CreateConversationBranchRequest {
    pub thread_id: Uuid,
    pub from_message_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ConversationThreadDetail {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub state: String,
    pub messages: Vec<MessageView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<ConversationBranchResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ToolCallView {
    pub call_id: String,
    pub tool: String,
    #[ts(type = "Record<string, unknown>")]
    pub arguments: Value,
    #[ts(type = "string | Record<string, unknown> | null")]
    pub result: Option<Value>,
    pub status: Option<String>,
    #[ts(optional, type = "number | null")]
    pub duration_ms: Option<u64>,
    pub truncated: bool,
    #[ts(optional, type = "number | null")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    /// One-line headline for the call, derived from the tool's declared
    /// display hints. Omitted when the tool declares none — renderers then
    /// use their generic fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct MessageView {
    pub thread_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    pub role: String,
    pub content: String,
    /// Content parts recorded beside the text when the message was
    /// submitted; absent for text-only messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub attachments: Vec<super::turn::MessageAttachment>,
    pub timestamp: DateTime<Utc>,
    pub turn_state: Option<TurnState>,
    pub tool_calls: Vec<ToolCallView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationSummary>,
}

/// What opened a session. A direct session is one a person opened; the
/// other kinds are automation, and the recent-sessions list groups them
/// beneath their source instead of listing them at the top level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum SessionOrigin {
    Direct,
    ScheduleRun { schedule_id: Uuid },
    WorkflowWorker { run_id: Uuid },
    Delegation { parent_session_id: Uuid },
}

/// One session as the recent-sessions list shows it.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct RecentSession {
    pub id: Uuid,
    /// Absent for a session in the general scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<Uuid>,
    /// The topic's name, or "General" for a session without one.
    pub topic_name: String,
    pub agent_id: String,
    pub origin: SessionOrigin,
    pub created_at: DateTime<Utc>,
    /// When the latest user or assistant message landed — the list's order.
    pub last_activity: DateTime<Utc>,
    pub message_preview: Option<String>,
    pub has_interrupted_turn: bool,
    /// A turn is executing on the session right now.
    pub has_running_turn: bool,
    /// Set while the inactivity sweep has the session hidden from the
    /// recent list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
}

/// What a recent-sessions entry is: the thing its automated sessions hang
/// beneath.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum RecentSessionsSource {
    /// A conversation a person opened.
    Conversation { session: RecentSession },
    /// A schedule whose runs opened sessions; `message` is the schedule's
    /// standing instruction.
    Schedule { id: Uuid, message: String },
    /// A workflow run started outside any conversation (the operator API);
    /// `label` is its script name or task when it has one.
    WorkflowRun {
        id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

/// One entry in the recent-sessions list: a source and the automated
/// sessions beneath it, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct RecentSessionsEntry {
    pub source: RecentSessionsSource,
    /// The latest activity across the source and everything beneath it —
    /// the list's order.
    pub last_activity: DateTime<Utc>,
    pub automated: Vec<RecentSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct RecentSessionsResponse {
    pub entries: Vec<RecentSessionsEntry>,
}
