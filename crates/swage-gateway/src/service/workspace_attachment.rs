use std::{path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    model::{NewWorkspaceAttachment, WorkspaceAttachment},
    store::{BenchStore, Storage},
};

// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceAttachmentRequest {
    pub path: String,
    pub label: Option<String>,
    #[serde(default)]
    pub set_as_default: bool,
    #[serde(default)]
    pub active_session_id: Option<Uuid>,
    #[serde(default = "default_workspace_permissions")]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub is_trusted: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceTrustRequest {
    pub is_trusted: bool,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceAttachmentResponse {
    pub id: Uuid,
    pub bench_id: Uuid,
    pub label: String,
    pub path: String,
    pub canonical_path: String,
    pub is_missing: bool,
    pub is_default: bool,
    pub permissions: Vec<String>,
    pub is_trusted: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceAttachmentListResponse {
    pub workspaces: Vec<WorkspaceAttachmentResponse>,
}

fn default_workspace_permissions() -> Vec<String> {
    vec!["read".to_string()]
}

/// Derives a URL-safe slug from a bench name.
///
/// - Lowercases the name
/// - Replaces spaces with hyphens
/// - Strips any character that is neither ASCII alphanumeric nor a hyphen
pub struct WorkspaceAttachmentService {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) bench_store: Arc<dyn BenchStore>,
}

impl WorkspaceAttachmentService {
    pub fn new(storage: Arc<dyn Storage>, bench_store: Arc<dyn BenchStore>) -> Self {
        Self {
            storage,
            bench_store,
        }
    }

    pub async fn list_workspaces(
        &self,
        bench_id: Uuid,
    ) -> Result<WorkspaceAttachmentListResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        let workspaces = self
            .storage
            .list_workspace_attachments_by_bench(bench_id)
            .await?;
        let mut refreshed = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            refreshed.push(self.refresh_workspace_missing_state(workspace).await?);
        }
        Ok(WorkspaceAttachmentListResponse {
            workspaces: refreshed.into_iter().map(workspace_to_response).collect(),
        })
    }

    pub async fn attach_workspace(
        &self,
        bench_id: Uuid,
        req: CreateWorkspaceAttachmentRequest,
    ) -> Result<WorkspaceAttachmentResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        let path = req.path.trim();
        if path.is_empty() {
            return Err(GatewayError::ValidationError(
                "workspace path must not be empty".into(),
            ));
        }
        if !Path::new(path).exists() {
            return Err(GatewayError::ValidationError(format!(
                "workspace path '{path}' does not exist on disk"
            )));
        }
        let canonical_path = std::fs::canonicalize(path).map_err(|error| {
            GatewayError::ValidationError(format!(
                "workspace path '{path}' could not be resolved: {error}"
            ))
        })?;
        let canonical_path = canonical_path.to_string_lossy().into_owned();

        let label = req
            .label
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                Path::new(&canonical_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("workspace")
                    .to_string()
            });
        let permissions = if req.permissions.is_empty() {
            default_workspace_permissions()
        } else {
            req.permissions
        };

        let workspace = self
            .storage
            .create_workspace_attachment(NewWorkspaceAttachment {
                bench_id,
                label,
                path: canonical_path,
                permissions,
                is_trusted: req.is_trusted,
            })
            .await?;
        if req.set_as_default {
            self.storage
                .set_bench_default_workspace(bench_id, Some(workspace.id))
                .await?;
        }
        if let Some(session_id) = req.active_session_id {
            self.ensure_session_belongs_to_bench(bench_id, session_id)
                .await?;
            self.storage
                .set_session_active_workspace(session_id, Some(workspace.id))
                .await?;
        }
        let workspace = self
            .storage
            .get_workspace_attachment(workspace.id)
            .await?
            .ok_or_else(|| GatewayError::ValidationError("created workspace disappeared".into()))?;
        Ok(workspace_to_response(workspace))
    }

    pub async fn set_trust(
        &self,
        bench_id: Uuid,
        workspace_id: Uuid,
        req: UpdateWorkspaceTrustRequest,
    ) -> Result<WorkspaceAttachmentResponse, GatewayError> {
        self.ensure_workspace_belongs_to_bench(bench_id, workspace_id)
            .await?;
        let workspace = self
            .storage
            .set_workspace_trust(workspace_id, req.is_trusted)
            .await?;
        Ok(workspace_to_response(workspace))
    }

    pub async fn detach_workspace(
        &self,
        bench_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<(), GatewayError> {
        self.ensure_workspace_belongs_to_bench(bench_id, workspace_id)
            .await?;
        self.storage
            .delete_workspace_attachment(workspace_id)
            .await?;
        Ok(())
    }

    async fn ensure_bench_exists(&self, bench_id: Uuid) -> Result<(), GatewayError> {
        self.bench_store
            .get(bench_id)
            .await?
            .map(|_| ())
            .ok_or(GatewayError::BenchNotFound { id: bench_id })
    }

    async fn ensure_workspace_belongs_to_bench(
        &self,
        bench_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<(), GatewayError> {
        let workspace = self
            .storage
            .get_workspace_attachment(workspace_id)
            .await?
            .ok_or_else(|| {
                GatewayError::ValidationError(format!(
                    "workspace attachment {workspace_id} not found"
                ))
            })?;
        if workspace.bench_id != bench_id {
            return Err(GatewayError::ValidationError(format!(
                "workspace attachment {workspace_id} does not belong to bench {bench_id}"
            )));
        }
        Ok(())
    }

    async fn ensure_session_belongs_to_bench(
        &self,
        bench_id: Uuid,
        session_id: Uuid,
    ) -> Result<(), GatewayError> {
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
        if session.bench_id != bench_id {
            return Err(GatewayError::ValidationError(format!(
                "session {session_id} does not belong to bench {bench_id}"
            )));
        }
        Ok(())
    }

    async fn refresh_workspace_missing_state(
        &self,
        workspace: WorkspaceAttachment,
    ) -> Result<WorkspaceAttachment, GatewayError> {
        let check_path = if workspace.canonical_path.is_empty() {
            &workspace.path
        } else {
            &workspace.canonical_path
        };
        let is_missing = !Path::new(check_path).exists();
        if workspace.is_missing == is_missing {
            return Ok(workspace);
        }
        Ok(self
            .storage
            .set_workspace_missing_state(workspace.id, is_missing)
            .await?)
    }
}

fn workspace_to_response(workspace: WorkspaceAttachment) -> WorkspaceAttachmentResponse {
    WorkspaceAttachmentResponse {
        id: workspace.id,
        bench_id: workspace.bench_id,
        label: workspace.label,
        path: workspace.path,
        canonical_path: workspace.canonical_path,
        is_missing: workspace.is_missing,
        is_default: workspace.is_default,
        permissions: workspace.permissions,
        is_trusted: workspace.is_trusted,
        created_at: workspace.created_at,
    }
}
