//! Asynchronous turn and inter-agent message HTTP contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct SubmitTurnRequest {
    pub conversation_id: Uuid,
    pub thread_id: Option<Uuid>,
    pub message: String,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct SubmitTurnResponse {
    pub turn_id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct TurnPollResponse {
    pub turn_id: Uuid,
    pub state: String,
    pub assistant_response: Option<String>,
    #[ts(type = "unknown")]
    pub structured_output: Option<serde_json::Value>,
    #[ts(type = "number | null")]
    pub input_tokens: Option<i64>,
    #[ts(type = "number | null")]
    pub output_tokens: Option<i64>,
    pub parent_turn_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct SendMessageRequest {
    pub sender_agent_id: String,
    pub recipient_agent_id: String,
    pub kind: String,
    pub correlation_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub sender_turn_id: Option<Uuid>,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct MessageResponse {
    pub id: Uuid,
    pub status: String,
}
