//! Asynchronous turn and inter-agent message HTTP contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Something submitted beside a message's text and carried to the model as
/// its own content part. The tag names what the attachment is; a client
/// sends only kinds the gateway's configured provider can consume, and the
/// gateway refuses the turn otherwise rather than dropping the part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum MessageAttachment {
    /// An image the model reads, addressed by URL — `https://` or a
    /// `data:` URL carrying the bytes inline.
    ImageUrl { url: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct SubmitTurnRequest {
    pub conversation_id: Uuid,
    pub thread_id: Option<Uuid>,
    pub message: String,
    /// Content parts submitted beside the text, in order. Absent means
    /// text only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub attachments: Vec<MessageAttachment>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub compaction_focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct SubmitTurnResponse {
    /// The turn the submission opened — or, when the message was queued
    /// behind a running turn, that running turn; absent when the message was
    /// queued and the turn it waits on could not be named (it ended in the
    /// same instant, and the queue is running the message as the next turn).
    pub turn_id: Option<Uuid>,
    /// `pending` for a turn that opened; `queued` for a message held for the
    /// next turn.
    pub status: String,
    /// The held message, present only when `status` is `queued`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub queued: Option<super::conversation::QueuedMessageView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct TurnPollResponse {
    pub turn_id: Uuid,
    pub state: String,
    pub assistant_response: Option<String>,
    #[ts(type = "unknown")]
    pub structured_output: Option<serde_json::Value>,
    #[ts(optional, type = "number | null")]
    pub input_tokens: Option<i64>,
    #[ts(optional, type = "number | null")]
    pub output_tokens: Option<i64>,
    /// Tokens spent by this turn's delegated children (transitively) — a
    /// billing figure separate from the turn's own tokens, which alone
    /// measure its context window. Null when no delegation has completed.
    #[ts(optional, type = "number | null")]
    pub delegated_input_tokens: Option<i64>,
    #[ts(optional, type = "number | null")]
    pub delegated_output_tokens: Option<i64>,
    pub parent_turn_id: Option<Uuid>,
    /// The execution boundary the turn ran inside, once its dispatcher
    /// settled. Null until then.
    pub execution_isolation: Option<TurnIsolationView>,
}

/// The execution boundary a turn ran inside: sandboxed on the host, degraded
/// for a stated reason, or the boundary the install declares for every turn
/// (a container image, for one) — which is neither of the other two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum TurnIsolationView {
    Isolated,
    Degraded { reason: String },
    Declared { boundary: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct SendMessageRequest {
    pub sender_agent_id: String,
    pub recipient_agent_id: String,
    pub kind: String,
    pub correlation_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
    pub sender_turn_id: Option<Uuid>,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
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
            attachments: Vec::new(),
            output_schema: None,
            compaction_focus: None,
            model: Some("fast".to_string()),
        };

        let serialised = serde_json::to_value(request).expect("serialise request");
        assert_eq!(serialised["model"], "fast");
        assert!(serialised.get("compaction_focus").is_none());
        assert!(serialised.get("attachments").is_none());
    }

    #[test]
    fn submit_turn_carries_tagged_attachments() {
        let request: SubmitTurnRequest = serde_json::from_value(serde_json::json!({
            "conversation_id": uuid::Uuid::new_v4(),
            "thread_id": null,
            "message": "what is in this picture?",
            "output_schema": null,
            "attachments": [{"kind": "image_url", "url": "https://example.test/cat.png"}]
        }))
        .expect("a tagged image attachment parses");

        assert_eq!(
            request.attachments,
            vec![super::MessageAttachment::ImageUrl {
                url: "https://example.test/cat.png".into()
            }]
        );
        let serialised = serde_json::to_value(&request).expect("serialise request");
        assert_eq!(
            serialised["attachments"],
            serde_json::json!([{"kind": "image_url", "url": "https://example.test/cat.png"}])
        );
    }

    #[test]
    fn submit_turn_rejects_an_attachment_kind_outside_the_contract() {
        let error = serde_json::from_value::<SubmitTurnRequest>(serde_json::json!({
            "conversation_id": uuid::Uuid::new_v4(),
            "thread_id": null,
            "message": "hello",
            "output_schema": null,
            "attachments": [{"kind": "audio_url", "url": "https://example.test/a.ogg"}]
        }))
        .expect_err("an unknown attachment kind must be rejected, never dropped");

        assert!(error.to_string().contains("audio_url"), "{error}");
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
