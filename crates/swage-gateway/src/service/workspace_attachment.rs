use std::{path::Path, sync::Arc};

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    model::{NewWorkspaceAttachment, WorkspaceAttachment},
    store::{Storage, TopicStore},
};

use crate::config_change::{ConfigChange, record_config_change};
use crate::state::AppState;

pub use swage_protocol::client::topic::{
    CreateWorkspaceAttachmentRequest, RelinkWorkspaceAttachmentRequest,
    UpdateWorkspaceTrustRequest, WorkspaceAttachmentListResponse, WorkspaceAttachmentResponse,
};

fn default_workspace_permissions() -> Vec<String> {
    vec!["read".to_string()]
}

/// Derives a URL-safe slug from a topic name.
///
/// - Lowercases the name
/// - Replaces topics with hyphens
/// - Strips any character that is neither ASCII alphanumeric nor a hyphen
pub struct WorkspaceAttachmentService {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) topic_store: Arc<dyn TopicStore>,
    audit_state: AppState,
}

impl WorkspaceAttachmentService {
    /// Creates the handler-facing mutation surface. Configuration mutations
    /// made through it are recorded with their complete attachment snapshots.
    pub fn from_state(state: AppState) -> Self {
        Self {
            storage: Arc::clone(&state.storage),
            topic_store: Arc::clone(&state.topic_store),
            audit_state: state,
        }
    }

    pub async fn list_workspaces(
        &self,
        topic_id: Uuid,
    ) -> Result<WorkspaceAttachmentListResponse, GatewayError> {
        self.ensure_topic_exists(topic_id).await?;
        let workspaces = self
            .storage
            .list_workspace_attachments_by_topic(topic_id)
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
        topic_id: Uuid,
        req: CreateWorkspaceAttachmentRequest,
    ) -> Result<WorkspaceAttachmentResponse, GatewayError> {
        self.ensure_topic_exists(topic_id).await?;
        let canonical_path = canonical_workspace_path(&req.path)?;

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
                topic_id,
                label,
                path: canonical_path,
                permissions,
                is_trusted: req.is_trusted,
            })
            .await?;
        if req.set_as_default {
            self.storage
                .set_topic_default_workspace(topic_id, Some(workspace.id))
                .await?;
        }
        if let Some(session_id) = req.active_session_id {
            self.ensure_session_belongs_to_topic(topic_id, session_id)
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
        self.record_change(
            "attach_workspace",
            workspace.id,
            format!("attached workspace '{}'", workspace.label),
            None,
            Some(workspace_snapshot(&workspace)?),
        )
        .await?;
        Ok(workspace_to_response(workspace))
    }

    pub async fn set_trust(
        &self,
        topic_id: Uuid,
        workspace_id: Uuid,
        req: UpdateWorkspaceTrustRequest,
    ) -> Result<WorkspaceAttachmentResponse, GatewayError> {
        let prior = self.workspace_snapshot(workspace_id).await?;
        self.ensure_workspace_belongs_to_topic(topic_id, workspace_id)
            .await?;
        let workspace = self
            .storage
            .set_workspace_trust(workspace_id, req.is_trusted)
            .await?;
        self.record_change(
            "set_workspace_trust",
            workspace_id,
            format!(
                "{} trust for workspace '{}'",
                if req.is_trusted { "enabled" } else { "revoked" },
                workspace.label
            ),
            Some(prior),
            Some(workspace_snapshot(&workspace)?),
        )
        .await?;
        Ok(workspace_to_response(workspace))
    }

    pub async fn relink_workspace(
        &self,
        topic_id: Uuid,
        workspace_id: Uuid,
        req: RelinkWorkspaceAttachmentRequest,
    ) -> Result<WorkspaceAttachmentResponse, GatewayError> {
        let canonical_path = canonical_workspace_path(&req.path)?;

        let prior = self.workspace_snapshot(workspace_id).await?;
        self.ensure_workspace_belongs_to_topic(topic_id, workspace_id)
            .await?;
        let workspace = self
            .storage
            .relink_workspace_attachment(workspace_id, canonical_path)
            .await?;
        self.record_change(
            "relink_workspace",
            workspace_id,
            format!("relinked workspace '{}'", workspace.label),
            Some(prior),
            Some(workspace_snapshot(&workspace)?),
        )
        .await?;
        Ok(workspace_to_response(workspace))
    }

    pub async fn detach_workspace(
        &self,
        topic_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<(), GatewayError> {
        let prior = self.workspace_snapshot(workspace_id).await?;
        self.ensure_workspace_belongs_to_topic(topic_id, workspace_id)
            .await?;
        self.storage
            .delete_workspace_attachment(workspace_id)
            .await?;
        self.record_change(
            "detach_workspace",
            workspace_id,
            "detached workspace".to_string(),
            Some(prior),
            None,
        )
        .await?;
        Ok(())
    }

    async fn workspace_snapshot(
        &self,
        workspace_id: Uuid,
    ) -> Result<serde_json::Value, GatewayError> {
        let workspace = self
            .storage
            .get_workspace_attachment(workspace_id)
            .await?
            .ok_or_else(|| {
                GatewayError::ValidationError(format!(
                    "workspace attachment {workspace_id} not found"
                ))
            })?;
        workspace_snapshot(&workspace)
    }

    async fn record_change(
        &self,
        action: &str,
        workspace_id: Uuid,
        summary: String,
        prior_state: Option<serde_json::Value>,
        new_state: Option<serde_json::Value>,
    ) -> Result<(), GatewayError> {
        record_config_change(
            &self.audit_state,
            ConfigChange {
                entity_kind: "workspace_attachment",
                entity_id: &workspace_id.to_string(),
                action,
                source: "operator_http",
                session_id: None,
                summary: &summary,
                prior_state,
                new_state,
            },
        )
        .await
    }

    async fn ensure_topic_exists(&self, topic_id: Uuid) -> Result<(), GatewayError> {
        self.topic_store
            .get(topic_id)
            .await?
            .map(|_| ())
            .ok_or(GatewayError::TopicNotFound { id: topic_id })
    }

    async fn ensure_workspace_belongs_to_topic(
        &self,
        topic_id: Uuid,
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
        if workspace.topic_id != topic_id {
            return Err(GatewayError::ValidationError(format!(
                "workspace attachment {workspace_id} does not belong to topic {topic_id}"
            )));
        }
        Ok(())
    }

    async fn ensure_session_belongs_to_topic(
        &self,
        topic_id: Uuid,
        session_id: Uuid,
    ) -> Result<(), GatewayError> {
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
        if session.topic_id != topic_id {
            return Err(GatewayError::ValidationError(format!(
                "session {session_id} does not belong to topic {topic_id}"
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

fn workspace_snapshot(workspace: &WorkspaceAttachment) -> Result<serde_json::Value, GatewayError> {
    serde_json::to_value(workspace).map_err(|error| GatewayError::ConfigError(error.to_string()))
}

fn canonical_workspace_path(requested: &str) -> Result<String, GatewayError> {
    let path = requested.trim();
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
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| {
            GatewayError::ValidationError(format!(
                "workspace path '{path}' could not be resolved: {error}"
            ))
        })
}

fn workspace_to_response(workspace: WorkspaceAttachment) -> WorkspaceAttachmentResponse {
    WorkspaceAttachmentResponse {
        id: workspace.id,
        topic_id: workspace.topic_id,
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
