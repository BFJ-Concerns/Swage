//! Spaces and their attached workspaces.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateSpaceRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdateSpaceRequest {
    #[ts(optional)]
    pub name: Option<String>,
    #[ts(optional)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct SpaceResponse {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct SpaceListResponse {
    pub spaces: Vec<SpaceResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateWorkspaceAttachmentRequest {
    pub path: String,
    #[ts(optional)]
    pub label: Option<String>,
    #[serde(default)]
    pub set_as_default: bool,
    #[serde(default)]
    #[ts(optional)]
    pub active_session_id: Option<Uuid>,
    #[serde(default = "default_workspace_permissions")]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdateWorkspaceTrustRequest {
    pub is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkspaceAttachmentResponse {
    pub id: Uuid,
    pub space_id: Uuid,
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
