use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use swage_config::discovery::resolve_root;
use swage_core::{GatewayError, StorageError};
use swage_storage::{Attachment, AttachmentScopeKind, Material, MaterialSourceKind, NewMaterial};

use crate::{state::AppState, tool::workspace::validate_path};

#[derive(Debug, Clone, Deserialize)]
pub struct CreateMaterialRequest {
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub title: String,
    pub relative_path: String,
    pub mime_type: Option<String>,
    pub content_text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromoteMaterialRequest {
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaterialResponse {
    pub id: Uuid,
    pub scope_kind: String,
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub source_kind: String,
    pub source_attachment_id: Option<Uuid>,
    pub status: String,
    pub title: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub mime_type: String,
    pub content_text: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaterialListResponse {
    pub materials: Vec<MaterialResponse>,
}

pub async fn create_material(
    state: &AppState,
    request: CreateMaterialRequest,
) -> Result<MaterialResponse, GatewayError> {
    if request.title.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "material title must not be empty".into(),
        ));
    }
    if request.content_text.is_empty() {
        return Err(GatewayError::ValidationError(
            "material content_text must not be empty".into(),
        ));
    }
    let scope = resolve_scope(
        state,
        &request.scope_kind,
        request.session_id,
        request.space_id,
    )
    .await?;
    let resolved_path = validate_material_relative_path(&scope.root, &request.relative_path)?;
    let mime_type = request
        .mime_type
        .unwrap_or_else(|| "text/markdown".to_string());
    write_material_file(&resolved_path, &request.content_text).await?;
    let material = state
        .storage
        .create_material(NewMaterial {
            id: Uuid::new_v4(),
            scope_kind: scope.scope_kind.clone(),
            session_id: scope.session_id,
            space_id: scope.space_id,
            source_kind: MaterialSourceKind::Manual,
            source_attachment_id: None,
            title: request.title,
            relative_path: request.relative_path,
            mime_type,
            content_text: request.content_text,
        })
        .await?;
    map_material_response(state, material).await
}

pub async fn get_material(
    state: &AppState,
    material_id: Uuid,
) -> Result<MaterialResponse, GatewayError> {
    let material = state
        .storage
        .get_material(material_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("material {material_id} not found"))
        })?;
    map_material_response(state, material).await
}

pub async fn list_session_materials(
    state: &AppState,
    session_id: Uuid,
) -> Result<MaterialListResponse, GatewayError> {
    state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let materials = state.storage.list_materials_by_session(session_id).await?;
    let mut items = Vec::with_capacity(materials.len());
    for material in materials {
        items.push(map_material_response(state, material).await?);
    }
    Ok(MaterialListResponse { materials: items })
}

pub async fn list_space_materials(
    state: &AppState,
    space_id: Uuid,
) -> Result<MaterialListResponse, GatewayError> {
    state
        .space_store
        .get(space_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError(format!("space {space_id} not found")))?;
    let materials = state.storage.list_materials_by_space(space_id).await?;
    let mut items = Vec::with_capacity(materials.len());
    for material in materials {
        items.push(map_material_response(state, material).await?);
    }
    Ok(MaterialListResponse { materials: items })
}

pub async fn archive_material(
    state: &AppState,
    material_id: Uuid,
) -> Result<MaterialResponse, GatewayError> {
    let material = state
        .storage
        .get_material(material_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("material {material_id} not found"))
        })?;
    let active_path = material_absolute_path(state, &material).await?;
    let archive_path = archived_material_path(state, &material).await?;
    move_material_file(&active_path, &archive_path).await?;
    let archived = state.storage.archive_material(material_id).await?;
    map_material_response(state, archived).await
}

pub async fn restore_material(
    state: &AppState,
    material_id: Uuid,
) -> Result<MaterialResponse, GatewayError> {
    let material = state
        .storage
        .get_material(material_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("material {material_id} not found"))
        })?;
    let archive_path = archived_material_path(state, &material).await?;
    let active_path = material_active_path(state, &material).await?;
    move_material_file(&archive_path, &active_path).await?;
    let restored = state.storage.restore_material(material_id).await?;
    map_material_response(state, restored).await
}

pub async fn promote_material(
    state: &AppState,
    material_id: Uuid,
    request: PromoteMaterialRequest,
) -> Result<MaterialResponse, GatewayError> {
    let material = state
        .storage
        .get_material(material_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("material {material_id} not found"))
        })?;
    let target_scope = resolve_scope(
        state,
        &request.scope_kind,
        request.session_id,
        request.space_id,
    )
    .await?;
    let current_path = material_absolute_path(state, &material).await?;
    let target_path = validate_material_relative_path(&target_scope.root, &request.relative_path)?;
    move_material_file(&current_path, &target_path).await?;
    let promoted = state
        .storage
        .promote_material(
            material_id,
            target_scope.scope_kind,
            target_scope.session_id,
            target_scope.space_id,
            &request.relative_path,
        )
        .await?;
    map_material_response(state, promoted).await
}

pub async fn ensure_attachment_material(
    state: &AppState,
    attachment: &Attachment,
    content_text: &str,
) -> Result<Material, GatewayError> {
    let relative_path = attachment_material_relative_path(attachment);
    let scope = resolve_scope(
        state,
        &attachment.scope_kind.to_string(),
        attachment.session_id,
        attachment.space_id,
    )
    .await?;
    let absolute_path = validate_material_relative_path(&scope.root, &relative_path)?;
    write_material_file(&absolute_path, content_text).await?;
    if let Some(existing) = state
        .storage
        .get_material_by_source_attachment(attachment.id)
        .await?
    {
        let promoted = state
            .storage
            .promote_material(
                existing.id,
                attachment.scope_kind.clone(),
                attachment.session_id,
                attachment.space_id,
                &relative_path,
            )
            .await?;
        return state
            .storage
            .update_material_content(promoted.id, content_text, "text/markdown")
            .await
            .map_err(GatewayError::from);
    }
    state
        .storage
        .create_material(NewMaterial {
            id: Uuid::new_v4(),
            scope_kind: attachment.scope_kind.clone(),
            session_id: attachment.session_id,
            space_id: attachment.space_id,
            source_kind: MaterialSourceKind::AttachmentIngestion,
            source_attachment_id: Some(attachment.id),
            title: attachment.filename.clone(),
            relative_path,
            mime_type: "text/markdown".into(),
            content_text: content_text.to_string(),
        })
        .await
        .map_err(GatewayError::from)
}

#[derive(Debug, Clone)]
struct MaterialScope {
    scope_kind: AttachmentScopeKind,
    session_id: Option<Uuid>,
    space_id: Option<Uuid>,
    root: PathBuf,
}

async fn resolve_scope(
    state: &AppState,
    scope_kind: &str,
    session_id: Option<Uuid>,
    space_id: Option<Uuid>,
) -> Result<MaterialScope, GatewayError> {
    let scope_kind = scope_kind
        .parse::<AttachmentScopeKind>()
        .map_err(GatewayError::ValidationError)?;
    match scope_kind {
        AttachmentScopeKind::Session => {
            let session_id = session_id.ok_or_else(|| {
                GatewayError::ValidationError("session materials require session_id".into())
            })?;
            if space_id.is_some() {
                return Err(GatewayError::ValidationError(
                    "session materials must not set space_id".into(),
                ));
            }
            let session = state
                .storage
                .get_session(session_id)
                .await?
                .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
            let space = state
                .space_store
                .get(session.space_id)
                .await?
                .ok_or_else(|| {
                    GatewayError::ValidationError(format!("space {} not found", session.space_id))
                })?;
            let space_root = space_data_root(&space.slug)?;
            Ok(MaterialScope {
                scope_kind,
                session_id: Some(session_id),
                space_id: Some(session.space_id),
                root: space_root
                    .join(".swage")
                    .join("materials")
                    .join("sessions")
                    .join(session_id.to_string()),
            })
        }
        AttachmentScopeKind::Space => {
            let space_id = space_id.ok_or_else(|| {
                GatewayError::ValidationError("space materials require space_id".into())
            })?;
            if session_id.is_some() {
                return Err(GatewayError::ValidationError(
                    "space materials must not set session_id".into(),
                ));
            }
            let space = state.space_store.get(space_id).await?.ok_or_else(|| {
                GatewayError::ValidationError(format!("space {space_id} not found"))
            })?;
            let space_root = space_data_root(&space.slug)?;
            Ok(MaterialScope {
                scope_kind,
                session_id: None,
                space_id: Some(space_id),
                root: space_root
                    .join(".swage")
                    .join("materials")
                    .join("spaces")
                    .join(space_id.to_string()),
            })
        }
    }
}

fn space_data_root(space_slug: &str) -> Result<PathBuf, GatewayError> {
    Ok(resolve_root()
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))?
        .join("spaces")
        .join(space_slug))
}

fn validate_material_relative_path(
    root: &Path,
    relative_path: &str,
) -> Result<PathBuf, GatewayError> {
    if relative_path.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "material relative_path must not be empty".into(),
        ));
    }
    validate_path(root, relative_path).map_err(GatewayError::ValidationError)
}

async fn write_material_file(path: &Path, content_text: &str) -> Result<(), GatewayError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.map_err(map_io_error)?;
    }
    fs::write(path, content_text).await.map_err(map_io_error)
}

async fn move_material_file(from: &Path, to: &Path) -> Result<(), GatewayError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).await.map_err(map_io_error)?;
    }
    fs::rename(from, to).await.map_err(map_io_error)
}

fn map_io_error(error: std::io::Error) -> GatewayError {
    GatewayError::Storage(StorageError::Io(error))
}

fn attachment_material_relative_path(attachment: &Attachment) -> String {
    let stem = Path::new(&attachment.filename)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("attachment");
    format!("attachments/{}-{}.md", attachment.id, stem)
}

async fn material_active_path(
    state: &AppState,
    material: &Material,
) -> Result<PathBuf, GatewayError> {
    let root = scope_root_for_material(state, material).await?;
    validate_material_relative_path(&root, &material.relative_path)
}

async fn archived_material_path(
    state: &AppState,
    material: &Material,
) -> Result<PathBuf, GatewayError> {
    let root = scope_root_for_material(state, material).await?;
    validate_material_relative_path(
        &root.join(".archive").join(material.id.to_string()),
        &material.relative_path,
    )
}

async fn scope_root_for_material(
    state: &AppState,
    material: &Material,
) -> Result<PathBuf, GatewayError> {
    match material.scope_kind {
        AttachmentScopeKind::Session => {
            let session_id = material.session_id.ok_or_else(|| {
                GatewayError::ValidationError(format!(
                    "material {} is missing session scope",
                    material.id
                ))
            })?;
            let space_id = match material.space_id {
                Some(space_id) => space_id,
                None => {
                    state
                        .storage
                        .get_session(session_id)
                        .await?
                        .ok_or(GatewayError::ConversationNotFound { id: session_id })?
                        .space_id
                }
            };
            let space = state.space_store.get(space_id).await?.ok_or_else(|| {
                GatewayError::ValidationError(format!("space {space_id} not found"))
            })?;
            Ok(space_data_root(&space.slug)?
                .join(".swage")
                .join("materials")
                .join("sessions")
                .join(session_id.to_string()))
        }
        AttachmentScopeKind::Space => {
            let space_id = material.space_id.ok_or_else(|| {
                GatewayError::ValidationError(format!(
                    "material {} is missing space scope",
                    material.id
                ))
            })?;
            let space = state.space_store.get(space_id).await?.ok_or_else(|| {
                GatewayError::ValidationError(format!("space {space_id} not found"))
            })?;
            Ok(space_data_root(&space.slug)?
                .join(".swage")
                .join("materials")
                .join("spaces")
                .join(space_id.to_string()))
        }
    }
}

async fn material_absolute_path(
    state: &AppState,
    material: &Material,
) -> Result<PathBuf, GatewayError> {
    if material.status == swage_storage::MaterialStatus::Archived {
        archived_material_path(state, material).await
    } else {
        material_active_path(state, material).await
    }
}

async fn map_material_response(
    state: &AppState,
    material: Material,
) -> Result<MaterialResponse, GatewayError> {
    let absolute_path = material_absolute_path(state, &material).await?;
    Ok(MaterialResponse {
        id: material.id,
        scope_kind: material.scope_kind.to_string(),
        session_id: material.session_id,
        space_id: material.space_id,
        source_kind: material.source_kind.to_string(),
        source_attachment_id: material.source_attachment_id,
        status: material.status.to_string(),
        title: material.title,
        relative_path: material.relative_path,
        absolute_path: absolute_path.to_string_lossy().into_owned(),
        mime_type: material.mime_type,
        content_text: material.content_text,
        created_at: material.created_at,
        updated_at: material.updated_at,
        archived_at: material.archived_at,
    })
}
