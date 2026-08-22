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

/// Request body for the conversation and space-session creation endpoints.
///
/// Every field is optional and the body itself may be omitted: an absent
/// `agent_id` selects the default `"gateway"` agent, and an absent
/// `selected_model_id` leaves the session's model unselected. A named
/// `agent_id` must be `"gateway"` or an agent definition visible to the
/// target space; the gateway rejects anything else with a validation error.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateConversationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub selected_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<Effort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ConversationResponse {
    pub id: Uuid,
    pub space_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ConversationSummary {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_preview: Option<String>,
    pub has_interrupted_turn: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ConversationDetail {
    pub id: Uuid,
    pub space_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<MessageView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ConversationBranchResponse {
    pub child_thread_id: Uuid,
    pub parent_thread_id: Uuid,
    pub branched_from_message_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateConversationBranchRequest {
    pub thread_id: Uuid,
    pub from_message_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
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
pub struct ToolCallView {
    pub call_id: String,
    pub tool: String,
    #[ts(type = "Record<string, unknown>")]
    pub arguments: Value,
    #[ts(type = "string | Record<string, unknown> | null")]
    pub result: Option<Value>,
    pub status: Option<String>,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    pub truncated: bool,
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct MessageView {
    pub thread_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub turn_state: Option<TurnState>,
    pub tool_calls: Vec<ToolCallView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationSummary>,
}
