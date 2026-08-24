//! Asynchronous turn and inter-agent message HTTP contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SubmitTurnRequest {
    pub conversation_id: Uuid,
    pub thread_id: Option<Uuid>,
    pub message: String,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub compaction_focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct SubmitTurnResponse {
    pub turn_id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
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
pub struct MessageResponse {
    pub id: Uuid,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::SubmitTurnRequest;

    #[test]
    fn submit_turn_defaults_focus_when_omitted() {
        let request: SubmitTurnRequest = serde_json::from_value(serde_json::json!({
            "conversation_id": uuid::Uuid::new_v4(),
            "thread_id": null,
            "message": "hello",
            "output_schema": null
        }))
        .expect("request without focus should remain valid");

        assert!(request.compaction_focus.is_none());
        assert!(request.model.is_none());
    }

    #[test]
    fn submit_turn_serialises_an_optional_model_only_when_selected() {
        let request = SubmitTurnRequest {
            conversation_id: uuid::Uuid::new_v4(),
            thread_id: None,
            message: "hello".to_string(),
            output_schema: None,
            compaction_focus: None,
            model: Some("fast".to_string()),
        };

        let serialised = serde_json::to_value(request).expect("serialise request");
        assert_eq!(serialised["model"], "fast");
        assert!(serialised.get("compaction_focus").is_none());
    }

    #[test]
    fn submit_turn_rejects_unknown_fields() {
        let error = serde_json::from_value::<SubmitTurnRequest>(serde_json::json!({
            "conversation_id": uuid::Uuid::new_v4(),
            "thread_id": null,
            "message": "hello",
            "output_schema": null,
            "focus_hint": "out of band"
        }))
        .expect_err("unknown request fields must be rejected");

        assert!(error.to_string().contains("unknown field `focus_hint`"));
    }
}
