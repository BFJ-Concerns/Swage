//! Material and attachment HTTP contracts.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct CreateAttachmentRequest {
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
    pub filename: String,
    pub mime_type: String,
    pub content_base64: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AttachmentResponse {
    pub id: Uuid,
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
    pub kind: String,
    pub status: String,
    pub filename: String,
    pub mime_type: String,
    #[ts(type = "number")]
    pub byte_count: i64,
    pub ingested_markdown: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AttachmentListResponse {
    pub attachments: Vec<AttachmentResponse>,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct BackgroundJobResponse {
    pub id: Uuid,
    pub kind: String,
    pub state: String,
    pub attachment_id: Option<Uuid>,
    #[ts(type = "Record<string, unknown> | null")]
    pub result: Option<serde_json::Value>,
    pub error_message: Option<String>,
    #[ts(type = "number")]
    pub attempt_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct QueuedAttachmentResponse {
    pub attachment: AttachmentResponse,
    pub job: BackgroundJobResponse,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct CreateMaterialRequest {
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
    pub title: String,
    pub relative_path: String,
    pub mime_type: Option<String>,
    pub content_text: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PromoteMaterialRequest {
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
    pub relative_path: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct MaterialResponse {
    pub id: Uuid,
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
    pub source_kind: String,
    pub source_attachment_id: Option<Uuid>,
    pub status: String,
    pub title: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub mime_type: String,
    pub content_text: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct MaterialListResponse {
    pub materials: Vec<MaterialResponse>,
}
