use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::warn;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    Attachment, AttachmentKind, AttachmentScopeKind, AttachmentStatus, BackgroundJob,
    BackgroundJobKind, NewAttachment, NewBackgroundJob,
};

use crate::state::AppState;

const ATTACHMENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAttachmentRequest {
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub filename: String,
    pub mime_type: String,
    pub content_base64: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttachmentResponse {
    pub id: Uuid,
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub status: String,
    pub filename: String,
    pub mime_type: String,
    pub byte_count: i64,
    pub ingested_markdown: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttachmentListResponse {
    pub attachments: Vec<AttachmentResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundJobResponse {
    pub id: Uuid,
    pub kind: String,
    pub state: String,
    pub attachment_id: Option<Uuid>,
    pub result: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub attempt_count: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueuedAttachmentResponse {
    pub attachment: AttachmentResponse,
    pub job: BackgroundJobResponse,
}

pub async fn create_attachment(
    state: &AppState,
    request: CreateAttachmentRequest,
) -> Result<QueuedAttachmentResponse, GatewayError> {
    if request.filename.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "attachment filename must not be empty".into(),
        ));
    }
    if request.mime_type.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "attachment mime_type must not be empty".into(),
        ));
    }
    let scope_kind = request
        .scope_kind
        .parse::<AttachmentScopeKind>()
        .map_err(GatewayError::ValidationError)?;
    match scope_kind {
        AttachmentScopeKind::Session => {
            let session_id = request.session_id.ok_or_else(|| {
                GatewayError::ValidationError("session attachments require session_id".into())
            })?;
            if request.project_id.is_some() {
                return Err(GatewayError::ValidationError(
                    "session attachments must not set project_id".into(),
                ));
            }
            state
                .storage
                .get_session(session_id)
                .await?
                .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
        }
        AttachmentScopeKind::Project => {
            let project_id = request.project_id.ok_or_else(|| {
                GatewayError::ValidationError("project attachments require project_id".into())
            })?;
            if request.session_id.is_some() {
                return Err(GatewayError::ValidationError(
                    "project attachments must not set session_id".into(),
                ));
            }
            state.project_store.get(project_id).await?.ok_or_else(|| {
                GatewayError::ValidationError(format!("project {project_id} not found"))
            })?;
        }
    }
    let bytes = BASE64
        .decode(request.content_base64.trim())
        .map_err(|error| {
            GatewayError::ValidationError(format!("invalid content_base64: {error}"))
        })?;
    if bytes.is_empty() {
        return Err(GatewayError::ValidationError(
            "attachment content must not be empty".into(),
        ));
    }
    let kind = classify_attachment_kind(&request.mime_type, &state.workflow_capabilities)?;
    let attachment_id = Uuid::new_v4();
    let job_id = Uuid::new_v4();
    let (attachment, job) = state
        .storage
        .queue_attachment_ingestion(
            NewAttachment {
                id: attachment_id,
                scope_kind,
                session_id: request.session_id,
                project_id: request.project_id,
                kind,
                filename: request.filename,
                mime_type: request.mime_type,
                bytes,
            },
            NewBackgroundJob {
                id: job_id,
                kind: BackgroundJobKind::AttachmentIngestion,
                attachment_id: Some(attachment_id),
                payload: serde_json::json!({ "attachment_id": attachment_id }),
            },
        )
        .await?;
    Ok(QueuedAttachmentResponse {
        attachment: map_attachment(state, attachment).await?,
        job: map_job(job),
    })
}

pub async fn list_session_attachments(
    state: &AppState,
    session_id: Uuid,
) -> Result<AttachmentListResponse, GatewayError> {
    state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let attachments = state
        .storage
        .list_attachments_by_session(session_id)
        .await?;
    Ok(AttachmentListResponse {
        attachments: map_attachments(state, attachments).await?,
    })
}

pub async fn list_project_attachments(
    state: &AppState,
    project_id: Uuid,
) -> Result<AttachmentListResponse, GatewayError> {
    state
        .project_store
        .get(project_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError(format!("project {project_id} not found")))?;
    let attachments = state
        .storage
        .list_attachments_by_project(project_id)
        .await?;
    Ok(AttachmentListResponse {
        attachments: map_attachments(state, attachments).await?,
    })
}

pub async fn get_background_job(
    state: &AppState,
    job_id: Uuid,
) -> Result<BackgroundJobResponse, GatewayError> {
    let job = state
        .storage
        .get_background_job(job_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("background job {job_id} not found"))
        })?;
    Ok(map_job(job))
}

pub async fn retry_background_job(
    state: &AppState,
    job_id: Uuid,
) -> Result<BackgroundJobResponse, GatewayError> {
    let retried = state.storage.retry_background_job(job_id).await?;
    if let Some(attachment_id) = retried.attachment_id {
        let _ = state
            .storage
            .update_attachment_ingestion(attachment_id, AttachmentStatus::Pending)
            .await?;
    }
    Ok(map_job(retried))
}

pub async fn tick_once(state: &AppState) -> Result<usize, GatewayError> {
    let jobs = state.storage.list_runnable_background_jobs().await?;
    let mut processed = 0;
    for job in jobs {
        let job = state.storage.mark_background_job_running(job.id).await?;
        process_attachment_job(state, &job).await?;
        processed += 1;
    }
    Ok(processed)
}

pub fn spawn_ingestion_loop(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = tick_once(&state).await {
                warn!(%error, "attachment ingestion tick failed");
            }
            tokio::time::sleep(ATTACHMENT_POLL_INTERVAL).await;
        }
    })
}

fn classify_attachment_kind(
    mime_type: &str,
    capabilities: &crate::capabilities::WorkflowCapabilities,
) -> Result<AttachmentKind, GatewayError> {
    match mime_type {
        "text/plain" | "text/markdown" => {
            if !capabilities.text {
                return Err(GatewayError::ValidationError(
                    "text ingestion is unavailable for the configured provider".into(),
                ));
            }
            Ok(AttachmentKind::Document)
        }
        "image/png" | "image/jpeg" => {
            if !capabilities.vision {
                return Err(GatewayError::ValidationError(
                    "vision ingestion is unavailable for the configured provider".into(),
                ));
            }
            Ok(AttachmentKind::Image)
        }
        other => Err(GatewayError::ValidationError(format!(
            "unsupported attachment mime type: {other}"
        ))),
    }
}

async fn process_attachment_job(state: &AppState, job: &BackgroundJob) -> Result<(), GatewayError> {
    let Some(attachment_id) = job.attachment_id else {
        state
            .storage
            .fail_background_job(job.id, "attachment ingestion job is missing attachment_id")
            .await?;
        return Ok(());
    };
    let attachment = state
        .storage
        .get_attachment(attachment_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("attachment {attachment_id} not found"))
        })?;
    match ingest_attachment_markdown(&attachment) {
        Ok(markdown) => {
            let _ = state
                .storage
                .update_attachment_ingestion(attachment.id, AttachmentStatus::Ready)
                .await?;
            if let Some(markdown) = markdown.as_deref() {
                let _ = crate::material::ensure_attachment_material(state, &attachment, markdown)
                    .await?;
            }
            let result = serde_json::json!({
                "attachment_id": attachment.id,
                "status": "ready",
                "completed_at": Utc::now(),
            });
            let _ = state
                .storage
                .complete_background_job(job.id, Some(&result))
                .await?;
        }
        Err(error) => {
            let _ = state
                .storage
                .update_attachment_ingestion(attachment.id, AttachmentStatus::Failed)
                .await?;
            let _ = state.storage.fail_background_job(job.id, &error).await?;
        }
    }
    Ok(())
}

fn ingest_attachment_markdown(attachment: &Attachment) -> Result<Option<String>, String> {
    match attachment.kind {
        AttachmentKind::Document => match attachment.mime_type.as_str() {
            "text/plain" | "text/markdown" => String::from_utf8(attachment.bytes.clone())
                .map(Some)
                .map_err(|error| format!("document attachment is not valid UTF-8: {error}")),
            other => Err(format!(
                "document attachment mime type is not ingestible: {other}"
            )),
        },
        AttachmentKind::Image => Ok(Some(format!(
            "![{}](attachment://{})",
            attachment.filename, attachment.id
        ))),
    }
}

async fn map_attachments(
    state: &AppState,
    attachments: Vec<Attachment>,
) -> Result<Vec<AttachmentResponse>, GatewayError> {
    let mut responses = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        responses.push(map_attachment(state, attachment).await?);
    }
    Ok(responses)
}

async fn map_attachment(
    state: &AppState,
    attachment: Attachment,
) -> Result<AttachmentResponse, GatewayError> {
    let ingested_markdown = if matches!(attachment.status, AttachmentStatus::Ready) {
        state
            .storage
            .get_material_by_source_attachment(attachment.id)
            .await?
            .map(|material| material.content_text)
    } else {
        None
    };
    Ok(AttachmentResponse {
        id: attachment.id,
        scope_kind: attachment.scope_kind.to_string(),
        session_id: attachment.session_id,
        project_id: attachment.project_id,
        kind: attachment.kind.to_string(),
        status: attachment.status.to_string(),
        filename: attachment.filename,
        mime_type: attachment.mime_type,
        byte_count: attachment.byte_count,
        ingested_markdown,
        created_at: attachment.created_at,
        updated_at: attachment.updated_at,
    })
}

fn map_job(job: BackgroundJob) -> BackgroundJobResponse {
    BackgroundJobResponse {
        id: job.id,
        kind: job.kind.to_string(),
        state: job.state.to_string(),
        attachment_id: job.attachment_id,
        result: job.result,
        error_message: job.error_message,
        attempt_count: job.attempt_count,
        created_at: job.created_at,
        updated_at: job.updated_at,
        started_at: job.started_at,
        completed_at: job.completed_at,
    }
}
