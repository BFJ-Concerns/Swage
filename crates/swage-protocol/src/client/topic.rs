//! Topics and their attached workspaces.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateTopicRequest {
    pub name: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdateTopicRequest {
    #[ts(optional)]
    pub name: Option<String>,
    #[ts(optional)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
/// A topic the user can choose. The general scope — the scope of a session
/// started without a topic — is never listed here: it is not a topic, and a
/// session in it carries no `topic_id` on the wire.
pub struct TopicResponse {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct TopicListResponse {
    pub topics: Vec<TopicResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateWorkspaceAttachmentRequest {
    pub path: String,
    #[ts(optional)]
    pub label: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub set_as_default: bool,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub active_session_id: Option<Uuid>,
    #[serde(default = "default_workspace_permissions")]
    #[ts(optional, as = "Option<_>")]
    pub permissions: Vec<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdateWorkspaceTrustRequest {
    pub is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkspaceAttachmentResponse {
    pub id: Uuid,
    pub topic_id: Uuid,
    pub label: String,
    pub path: String,
    pub canonical_path: String,
    pub is_missing: bool,
    pub is_default: bool,
    pub permissions: Vec<String>,
    pub is_trusted: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkspaceAttachmentListResponse {
    pub workspaces: Vec<WorkspaceAttachmentResponse>,
}

fn default_workspace_permissions() -> Vec<String> {
    vec!["read".to_string()]
}
