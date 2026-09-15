//! Surfaces — agent-run tabs on a topic, assembled from module instances.
//!
//! The database is authoritative for surface content; every mutation lands an
//! audit event (the surface's provenance trail) and broadcasts a live
//! `SurfaceUpdated` signal so open surfaces refetch rather than poll. Module
//! kinds are compiled-in until module authoring lands; `document` (markdown
//! source, client-rendered) is the first.

use axum::extract::State;
use serde_json::json;
use uuid::Uuid;

use swage_core::{GatewayError, StorageError};
use swage_protocol::client::surface::{
    CreateSurfaceAnnotationRequest, CreateSurfaceRequest, CreateSurfaceTemplateRequest,
    DeleteSurfaceAnnotationResponse, SurfaceAnnotationView, SurfaceListResponse, SurfaceModuleView,
    SurfaceResponse, SurfaceSummary, SurfaceTemplateListResponse, SurfaceTemplateModuleView,
    SurfaceTemplateResponse, SurfaceTemplateSummary, UpdateSurfaceAnnotationRequest,
    WriteSurfaceModuleRequest,
};
use swage_protocol::client::websocket::ServerEvent;
use swage_storage::{
    NewAuditEvent, NewSurface, NewSurfaceAnnotation, NewSurfaceModule, NewSurfaceTemplate,
    NewSurfaceTemplateModuleInput, SurfaceAnnotation, SurfaceModule,
};

use crate::error::{ApiError, Json, Path};
use crate::state::AppState;

/// The module kinds the client can render. A write naming any other kind is
/// rejected before anything lands — an unrenderable module would be a surface
/// the user cannot see.
const MODULE_KINDS: &[&str] = &["document"];

pub async fn create_page(
    state: &AppState,
    topic_id: Uuid,
    request: CreateSurfaceRequest,
) -> Result<SurfaceResponse, GatewayError> {
    let title = request.title.trim();
    if title.is_empty() {
        return Err(GatewayError::ValidationError(
            "a surface needs a non-empty title".into(),
        ));
    }
    state
        .topic_store
        .get(topic_id)
        .await?
        .ok_or(GatewayError::TopicNotFound { id: topic_id })?;

    // A template seeds the surface; everything is validated before anything
    // lands, so a rejected fork leaves no surface behind.
    let template = match request.template_id {
        Some(template_id) => Some(resolve_template(state, topic_id, template_id).await?),
        None => None,
    };

    let surface = match &template {
        Some(template) => {
            state
                .storage
                .create_surface_from_template(
                    NewSurface {
                        topic_id,
                        title: title.to_string(),
                    },
                    template.id,
                )
                .await?
        }
        None => {
            state
                .storage
                .create_page(NewSurface {
                    topic_id,
                    title: title.to_string(),
                })
                .await?
        }
    };

    let template_metadata = template
        .as_ref()
        .map(|template| json!({ "template_id": template.id, "template_name": template.name }));
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "surface".into(),
            entity_id: surface.id.to_string(),
            event_kind: "surface_created".into(),
            summary: match &template {
                Some(template) => {
                    format!("surface created from template '{}': {title}", template.name)
                }
                None => format!("surface created: {title}"),
            },
            metadata: json!({
                "topic_id": surface.topic_id,
                "title": title,
                "forked_from": template_metadata,
            }),
        })
        .await?;
    // Creation is a surface-level change: open surfaces refetch their list.
    let _ = state.broadcast.send(ServerEvent::SurfaceUpdated {
        surface_id: surface.id,
        module_id: None,
    });
    let modules = state.storage.list_surface_modules(surface.id).await?;
    Ok(SurfaceResponse {
        id: surface.id,
        topic_id: surface.topic_id,
        title: surface.title,
        created_at: surface.created_at,
        updated_at: surface.updated_at,
        modules: modules
            .into_iter()
            .map(|module| module_view(module, Vec::new()))
            .collect(),
        forked_from_template_id: surface.forked_from_template_id,
        forked_at: surface.forked_at,
    })
}

/// A template usable from this topic: it must exist and belong here.
/// Templates are topic-local — there is no sharing across topics.
async fn resolve_template(
    state: &AppState,
    topic_id: Uuid,
    template_id: Uuid,
) -> Result<swage_storage::SurfaceTemplate, GatewayError> {
    state
        .storage
        .get_surface_template(template_id)
        .await?
        .filter(|template| template.topic_id == topic_id)
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no template {template_id} in topic {topic_id}"))
        })
}

/// Create a topic's reusable surface shape. A template is inert: it seeds
/// surfaces at their creation and is never projected into surfaces already
/// forked from it.
pub async fn create_surface_template(
    state: &AppState,
    topic_id: Uuid,
    request: CreateSurfaceTemplateRequest,
) -> Result<SurfaceTemplateResponse, GatewayError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(GatewayError::ValidationError(
            "a template needs a non-empty name".into(),
        ));
    }
    state
        .topic_store
        .get(topic_id)
        .await?
        .ok_or(GatewayError::TopicNotFound { id: topic_id })?;
    // Validate every module kind before the first write: a template that
    // seeds an unrenderable module is a surface the user cannot see.
    for module in &request.modules {
        let kind = module.kind.as_deref().unwrap_or("document");
        if !MODULE_KINDS.contains(&kind) {
            return Err(GatewayError::ValidationError(format!(
                "unknown module kind '{kind}' — known kinds: {}",
                MODULE_KINDS.join(", ")
            )));
        }
    }

    let template = state
        .storage
        .create_surface_template_with_modules(
            NewSurfaceTemplate {
                topic_id,
                name: name.to_string(),
            },
            request
                .modules
                .iter()
                .enumerate()
                .map(|(position, module)| NewSurfaceTemplateModuleInput {
                    kind: module.kind.clone().unwrap_or_else(|| "document".into()),
                    position: position as i32,
                    source: module.source.clone(),
                })
                .collect(),
        )
        .await?;

    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "surface_template".into(),
            entity_id: template.id.to_string(),
            event_kind: "surface_template_created".into(),
            summary: format!("surface template created: {name}"),
            metadata: json!({
                "topic_id": template.topic_id,
                "name": name,
                "module_count": request.modules.len(),
                "modules": request.modules.iter().enumerate().map(|(position, module)| json!({
                    "kind": module.kind.as_deref().unwrap_or("document"),
                    "position": position,
                    "source": module.source,
                })).collect::<Vec<_>>(),
            }),
        })
        .await?;

    template_response(state, template.id).await
}

pub async fn template_response(
    state: &AppState,
    template_id: Uuid,
) -> Result<SurfaceTemplateResponse, GatewayError> {
    let template = state
        .storage
        .get_surface_template(template_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no template with id {template_id}"))
        })?;
    let modules = state
        .storage
        .list_surface_template_modules(template.id)
        .await?;
    Ok(SurfaceTemplateResponse {
        id: template.id,
        topic_id: template.topic_id,
        name: template.name,
        created_at: template.created_at,
        updated_at: template.updated_at,
        modules: modules
            .into_iter()
            .map(|module| SurfaceTemplateModuleView {
                id: module.id,
                kind: module.kind,
                position: module.position,
                source: module.source,
            })
            .collect(),
    })
}

/// Write a module's source — replace an existing instance's content, or
/// append a new instance when the request names none. `author` names who
/// made the change in the provenance record (an agent id, or "user" for the
/// web surface).
pub async fn write_surface_module(
    state: &AppState,
    surface_id: Uuid,
    request: WriteSurfaceModuleRequest,
    author: &str,
) -> Result<SurfaceModuleView, GatewayError> {
    let surface =
        state.storage.get_page(surface_id).await?.ok_or_else(|| {
            GatewayError::ValidationError(format!("no surface with id {surface_id}"))
        })?;

    let (module, prior_source) = match request.module_id {
        Some(module_id) => {
            let existing = state
                .storage
                .get_surface_module(module_id)
                .await?
                .filter(|module| module.surface_id == surface.id)
                .ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "no module {module_id} on surface {surface_id}"
                    ))
                })?;
            state
                .storage
                .update_surface_module_source(existing.id, &request.source)
                .await?;
            // Read the row back rather than patching a copy: the store
            // stamped updated_at, and the view must carry the stored truth.
            let updated = state
                .storage
                .get_surface_module(existing.id)
                .await?
                .ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "module {module_id} disappeared during the write"
                    ))
                })?;
            (updated, Some(existing.source))
        }
        None => {
            let kind = request.kind.as_deref().unwrap_or("document");
            if !MODULE_KINDS.contains(&kind) {
                return Err(GatewayError::ValidationError(format!(
                    "unknown module kind '{kind}' — known kinds: {}",
                    MODULE_KINDS.join(", ")
                )));
            }
            let position = state.storage.list_surface_modules(surface.id).await?.len() as i32;
            let created = state
                .storage
                .create_surface_module(NewSurfaceModule {
                    surface_id: surface.id,
                    kind: kind.to_string(),
                    position,
                    source: request.source.clone(),
                })
                .await?;
            (created, None)
        }
    };

    // Provenance: who changed what, with the displaced content retained —
    // the surface row holds only current truth, so the trail is where a
    // superseded source survives. Recording failure propagates AFTER the
    // mutation landed — a loud applied-but-unrecorded error, the platform's
    // decided posture (see record_config_change) — never a success report
    // on an unrecorded change, and never a second buffered record.
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "surface".into(),
            entity_id: surface.id.to_string(),
            event_kind: "surface_module_written".into(),
            summary: format!("module written on surface: {}", surface.title),
            metadata: json!({
                "module_id": module.id,
                "kind": module.kind,
                "author": author,
                "prior_source": prior_source,
                "new_source": request.source,
            }),
        })
        .await?;

    // The live signal: open surfaces refetch the surface. Content rides the
    // fetch, not the event.
    let _ = state.broadcast.send(ServerEvent::SurfaceUpdated {
        surface_id: surface.id,
        module_id: Some(module.id),
    });

    let annotations = state
        .storage
        .list_surface_annotations(surface.id)
        .await?
        .into_iter()
        .filter(|annotation| annotation.module_id == module.id)
        .map(annotation_view)
        .collect();
    Ok(module_view(module, annotations))
}

/// Leave a note on one of a surface's modules. `author` names who left it in the
/// provenance record (an agent id, or "user" for the web surface).
///
/// A note carries no authority: it is one party's words recorded against a
/// stable id, and nothing keys any permission off it.
pub async fn create_surface_annotation(
    state: &AppState,
    surface_id: Uuid,
    request: CreateSurfaceAnnotationRequest,
    author: &str,
) -> Result<SurfaceAnnotationView, GatewayError> {
    let surface =
        state.storage.get_page(surface_id).await?.ok_or_else(|| {
            GatewayError::ValidationError(format!("no surface with id {surface_id}"))
        })?;
    let body = request.body.trim();
    if body.is_empty() {
        return Err(GatewayError::ValidationError(
            "an annotation needs a non-empty body".into(),
        ));
    }
    // The anchor must be real and must be on this surface: a note addressed to
    // another surface's module would name a location neither party can see.
    let module_id = request.module_id;
    state
        .storage
        .get_surface_module(module_id)
        .await?
        .filter(|module| module.surface_id == surface.id)
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no module {module_id} on surface {surface_id}"))
        })?;
    let tags: Vec<String> = request
        .tags
        .iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();

    let annotation = state
        .storage
        .create_surface_annotation(NewSurfaceAnnotation {
            surface_id: surface.id,
            module_id,
            body: body.to_string(),
            tags,
            author: author.to_string(),
        })
        .await?;

    record_annotation_change(
        state,
        &surface.title,
        surface.id,
        &annotation,
        "surface_annotation_created",
        None,
        author,
    )
    .await?;

    let _ = state.broadcast.send(ServerEvent::SurfaceUpdated {
        surface_id: surface.id,
        module_id: Some(annotation.module_id),
    });

    Ok(annotation_view(annotation))
}

/// Replace an annotation's body and tags. The displaced text survives in the
/// provenance trail, which is the whole of the history — there is no version
/// row and no undo.
pub async fn update_surface_annotation(
    state: &AppState,
    annotation_id: Uuid,
    request: UpdateSurfaceAnnotationRequest,
    author: &str,
) -> Result<SurfaceAnnotationView, GatewayError> {
    let existing = state
        .storage
        .get_surface_annotation(annotation_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no annotation with id {annotation_id}"))
        })?;
    let body = request.body.trim();
    if body.is_empty() {
        return Err(GatewayError::ValidationError(
            "an annotation needs a non-empty body".into(),
        ));
    }
    let tags: Vec<String> = request
        .tags
        .iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();

    state
        .storage
        .update_surface_annotation(existing.id, body, &tags)
        .await?;
    // Read the row back rather than patching a copy: the store stamped
    // updated_at, and the view must carry the stored truth.
    let updated = state
        .storage
        .get_surface_annotation(existing.id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!(
                "annotation {annotation_id} disappeared during the write"
            ))
        })?;

    let surface = state.storage.get_page(updated.surface_id).await?;
    let title = surface.map(|surface| surface.title).unwrap_or_default();
    record_annotation_change(
        state,
        &title,
        updated.surface_id,
        &updated,
        "surface_annotation_updated",
        Some(&existing),
        author,
    )
    .await?;

    let _ = state.broadcast.send(ServerEvent::SurfaceUpdated {
        surface_id: updated.surface_id,
        module_id: Some(updated.module_id),
    });

    Ok(annotation_view(updated))
}

/// Remove an annotation. The trail retains what it said, so the deletion is
/// recoverable by re-writing the note, never by an undo mechanism.
pub async fn delete_surface_annotation(
    state: &AppState,
    annotation_id: Uuid,
    author: &str,
) -> Result<(), GatewayError> {
    let existing = state
        .storage
        .get_surface_annotation(annotation_id)
        .await?
        .ok_or_else(|| {
            GatewayError::Storage(StorageError::NotFound {
                entity: "surface annotation".into(),
                id: annotation_id.to_string(),
            })
        })?;
    delete_surface_annotation_after_read(state, annotation_id, existing, author).await
}

/// Finish an annotation deletion after its caller has read the note. The
/// storage operation remains the authority for a race with another deleter:
/// its zero-row result is the typed not-found response, not a second audit.
async fn delete_surface_annotation_after_read(
    state: &AppState,
    annotation_id: Uuid,
    existing: SurfaceAnnotation,
    author: &str,
) -> Result<(), GatewayError> {
    let surface = state.storage.get_page(existing.surface_id).await?;
    let title = surface.map(|surface| surface.title).unwrap_or_default();
    let audit_event = annotation_change_event(
        &title,
        existing.surface_id,
        &existing,
        "surface_annotation_deleted",
        Some(&existing),
        None,
        author,
    );
    if !state
        .storage
        .delete_surface_annotation_with_audit(existing.clone(), audit_event)
        .await?
    {
        return Err(GatewayError::Storage(StorageError::NotFound {
            entity: "surface annotation".into(),
            id: annotation_id.to_string(),
        }));
    }

    let _ = state.broadcast.send(ServerEvent::SurfaceUpdated {
        surface_id: existing.surface_id,
        module_id: Some(existing.module_id),
    });
    Ok(())
}

/// Provenance for an annotation mutation: who changed what, with the
/// displaced body and tags retained — the row holds only current truth, so
/// the trail is where a superseded note survives. Recording failure
/// propagates AFTER the mutation landed, matching write_surface_module: a loud
/// applied-but-unrecorded error, never a success report on an unrecorded
/// change.
async fn record_annotation_change(
    state: &AppState,
    surface_title: &str,
    surface_id: Uuid,
    annotation: &SurfaceAnnotation,
    event_kind: &str,
    prior: Option<&SurfaceAnnotation>,
    author: &str,
) -> Result<(), GatewayError> {
    state
        .storage
        .create_audit_event(annotation_change_event(
            surface_title,
            surface_id,
            annotation,
            event_kind,
            prior,
            Some(annotation),
            author,
        ))
        .await?;
    Ok(())
}

fn annotation_change_event(
    surface_title: &str,
    surface_id: Uuid,
    annotation: &SurfaceAnnotation,
    event_kind: &str,
    prior: Option<&SurfaceAnnotation>,
    current: Option<&SurfaceAnnotation>,
    author: &str,
) -> NewAuditEvent {
    NewAuditEvent {
        id: Uuid::new_v4().to_string(),
        session_id: None,
        entity_kind: "surface".into(),
        entity_id: surface_id.to_string(),
        event_kind: event_kind.into(),
        summary: format!("annotation change on surface: {surface_title}"),
        metadata: json!({
            "annotation_id": annotation.id,
            "module_id": annotation.module_id,
            "author": author,
            "annotation_author": annotation.author,
            "prior_body": prior.map(|prior| prior.body.clone()),
            "prior_tags": prior.map(|prior| prior.tags.clone()),
            "new_body": current.map(|current| current.body.clone()),
            "new_tags": current.map(|current| current.tags.clone()),
        }),
    }
}

pub async fn surface_response(
    state: &AppState,
    surface_id: Uuid,
) -> Result<SurfaceResponse, GatewayError> {
    let surface =
        state.storage.get_page(surface_id).await?.ok_or_else(|| {
            GatewayError::ValidationError(format!("no surface with id {surface_id}"))
        })?;
    let modules = state.storage.list_surface_modules(surface.id).await?;
    let annotations = state.storage.list_surface_annotations(surface.id).await?;
    // Each note travels with the module it hangs on, so the reader finds it
    // where the location is rather than having to match ids by hand.
    Ok(SurfaceResponse {
        id: surface.id,
        topic_id: surface.topic_id,
        title: surface.title,
        created_at: surface.created_at,
        updated_at: surface.updated_at,
        modules: modules
            .into_iter()
            .map(|module| {
                let hung_here = annotations
                    .iter()
                    .filter(|annotation| annotation.module_id == module.id)
                    .cloned()
                    .map(annotation_view)
                    .collect();
                module_view(module, hung_here)
            })
            .collect(),
        forked_from_template_id: surface.forked_from_template_id,
        forked_at: surface.forked_at,
    })
}

fn module_view(
    module: SurfaceModule,
    annotations: Vec<SurfaceAnnotationView>,
) -> SurfaceModuleView {
    SurfaceModuleView {
        id: module.id,
        kind: module.kind,
        position: module.position,
        source: module.source,
        updated_at: module.updated_at,
        annotations,
    }
}

fn annotation_view(annotation: SurfaceAnnotation) -> SurfaceAnnotationView {
    SurfaceAnnotationView {
        id: annotation.id,
        module_id: annotation.module_id,
        body: annotation.body,
        tags: annotation.tags,
        author: annotation.author,
        created_at: annotation.created_at,
        updated_at: annotation.updated_at,
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

pub async fn list_pages_handler(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
) -> Result<Json<SurfaceListResponse>, ApiError> {
    let surfaces = state
        .storage
        .list_pages_by_topic(topic_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Json(SurfaceListResponse {
        surfaces: surfaces
            .into_iter()
            .map(|surface| SurfaceSummary {
                id: surface.id,
                title: surface.title,
                updated_at: surface.updated_at,
            })
            .collect(),
    }))
}

pub async fn create_surface_handler(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
    Json(request): Json<CreateSurfaceRequest>,
) -> Result<Json<SurfaceResponse>, ApiError> {
    Ok(Json(create_page(&state, topic_id, request).await?))
}

pub async fn get_surface_handler(
    State(state): State<AppState>,
    Path(surface_id): Path<Uuid>,
) -> Result<Json<SurfaceResponse>, ApiError> {
    Ok(Json(surface_response(&state, surface_id).await?))
}

pub async fn write_surface_module_handler(
    State(state): State<AppState>,
    Path(surface_id): Path<Uuid>,
    Json(request): Json<WriteSurfaceModuleRequest>,
) -> Result<Json<SurfaceModuleView>, ApiError> {
    Ok(Json(
        write_surface_module(&state, surface_id, request, "user").await?,
    ))
}

pub async fn create_surface_annotation_handler(
    State(state): State<AppState>,
    Path(surface_id): Path<Uuid>,
    Json(request): Json<CreateSurfaceAnnotationRequest>,
) -> Result<Json<SurfaceAnnotationView>, ApiError> {
    Ok(Json(
        create_surface_annotation(&state, surface_id, request, "user").await?,
    ))
}

pub async fn update_surface_annotation_handler(
    State(state): State<AppState>,
    Path(annotation_id): Path<Uuid>,
    Json(request): Json<UpdateSurfaceAnnotationRequest>,
) -> Result<Json<SurfaceAnnotationView>, ApiError> {
    Ok(Json(
        update_surface_annotation(&state, annotation_id, request, "user").await?,
    ))
}

pub async fn delete_surface_annotation_handler(
    State(state): State<AppState>,
    Path(annotation_id): Path<Uuid>,
) -> Result<Json<DeleteSurfaceAnnotationResponse>, ApiError> {
    delete_surface_annotation(&state, annotation_id, "user").await?;
    Ok(Json(DeleteSurfaceAnnotationResponse { deleted: true }))
}

pub async fn list_surface_templates_handler(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
) -> Result<Json<SurfaceTemplateListResponse>, ApiError> {
    let templates = state
        .storage
        .list_surface_templates_by_topic(topic_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Json(SurfaceTemplateListResponse {
        templates: templates
            .into_iter()
            .map(|template| SurfaceTemplateSummary {
                id: template.id,
                name: template.name,
                updated_at: template.updated_at,
            })
            .collect(),
    }))
}

pub async fn create_surface_template_handler(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
    Json(request): Json<CreateSurfaceTemplateRequest>,
) -> Result<Json<SurfaceTemplateResponse>, ApiError> {
    Ok(Json(
        create_surface_template(&state, topic_id, request).await?,
    ))
}

pub async fn get_surface_template_handler(
    State(state): State<AppState>,
    Path(template_id): Path<Uuid>,
) -> Result<Json<SurfaceTemplateResponse>, ApiError> {
    Ok(Json(template_response(&state, template_id).await?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_storage::{
        AuditStore, NewTopic, SqliteStorage, Storage, SurfaceAnnotationStore, SurfaceStore,
        TopicStore,
    };

    use super::*;

    fn state(storage: Arc<SqliteStorage>) -> AppState {
        AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
    }

    /// A surface with one module, and a second surface with its own module — the
    /// second is what a cross-surface anchor would wrongly reach.
    async fn seeded() -> (AppState, Arc<SqliteStorage>, Uuid, Uuid, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "p".into(),
                slug: "p".into(),
            },
        )
        .await
        .unwrap();
        let state = state(Arc::clone(&storage));
        let surface = create_page(
            &state,
            topic.id,
            CreateSurfaceRequest {
                title: "Planning".into(),
                template_id: None,
            },
        )
        .await
        .unwrap();
        let module = write_surface_module(
            &state,
            surface.id,
            WriteSurfaceModuleRequest {
                module_id: None,
                kind: None,
                source: "# Draft".into(),
            },
            "user",
        )
        .await
        .unwrap();
        let other = create_page(
            &state,
            topic.id,
            CreateSurfaceRequest {
                title: "Elsewhere".into(),
                template_id: None,
            },
        )
        .await
        .unwrap();
        let other_module = write_surface_module(
            &state,
            other.id,
            WriteSurfaceModuleRequest {
                module_id: None,
                kind: None,
                source: "# Other".into(),
            },
            "user",
        )
        .await
        .unwrap();
        (state, storage, surface.id, module.id, other_module.id)
    }

    fn annotation(module_id: Uuid, body: &str) -> CreateSurfaceAnnotationRequest {
        CreateSurfaceAnnotationRequest {
            module_id,
            body: body.into(),
            tags: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_annotation_reaches_the_reader_at_the_module_it_names() {
        let (state, _storage, surface_id, module_id, other_module_id) = seeded().await;

        create_surface_annotation(
            &state,
            surface_id,
            annotation(module_id, "the second claim needs a source"),
            "user",
        )
        .await
        .unwrap();
        create_surface_annotation(
            &state,
            surface_id,
            annotation(other_module_id, "needs a second pass"),
            "user",
        )
        .await
        .expect_err("a module on another surface is not an anchor");

        let response = surface_response(&state, surface_id).await.unwrap();
        let module = response
            .modules
            .iter()
            .find(|module| module.id == module_id)
            .expect("the module is on the surface");
        assert_eq!(
            module
                .annotations
                .iter()
                .map(|a| a.body.as_str())
                .collect::<Vec<_>>(),
            vec!["the second claim needs a source"],
            "the module carries the note addressed to it, and only that one"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_body_is_refused_before_anything_lands() {
        let (state, storage, surface_id, module_id, _) = seeded().await;

        let error =
            create_surface_annotation(&state, surface_id, annotation(module_id, "   "), "user")
                .await
                .expect_err("an empty note names no reference");
        assert!(error.to_string().contains("non-empty"), "{error}");
        assert!(
            storage
                .list_surface_annotations(surface_id)
                .await
                .unwrap()
                .is_empty(),
            "validation runs before the store is touched"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_module_on_another_surface_is_not_an_anchor() {
        let (state, storage, surface_id, _module_id, other_module_id) = seeded().await;

        let error = create_surface_annotation(
            &state,
            surface_id,
            annotation(other_module_id, "hangs nowhere either party can see"),
            "user",
        )
        .await
        .expect_err("the anchor must be on the named surface");
        assert!(
            error.to_string().contains(&other_module_id.to_string()),
            "{error}"
        );
        assert!(
            storage
                .list_surface_annotations(surface_id)
                .await
                .unwrap()
                .is_empty(),
            "the rejected note never landed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restating_a_note_leaves_the_displaced_words_in_the_trail() {
        let (state, storage, surface_id, module_id, _) = seeded().await;
        let created = create_surface_annotation(
            &state,
            surface_id,
            CreateSurfaceAnnotationRequest {
                module_id,
                body: "first reading".into(),
                tags: vec!["draft".into()],
            },
            "user",
        )
        .await
        .unwrap();

        update_surface_annotation(
            &state,
            created.id,
            UpdateSurfaceAnnotationRequest {
                body: "second reading".into(),
                tags: vec!["settled".into()],
            },
            "assistant",
        )
        .await
        .unwrap();

        let stored = storage
            .get_surface_annotation(created.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.body, "second reading",
            "the row holds current truth only"
        );

        let events = storage.list_audit_events().await.unwrap();
        let recorded = events
            .iter()
            .find(|event| event.event_kind == "surface_annotation_updated")
            .expect("the restatement is in the surface's trail");
        assert_eq!(
            recorded.metadata["prior_body"], "first reading",
            "the displaced words survive where the row does not"
        );
        assert_eq!(recorded.metadata["prior_tags"][0], "draft");
        assert_eq!(recorded.metadata["new_body"], "second reading");
        assert_eq!(
            recorded.metadata["author"], "assistant",
            "the trail names who made this change, not only who first wrote it"
        );
        assert_eq!(recorded.metadata["annotation_author"], "user");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_note_retains_what_it_said() {
        let (state, storage, surface_id, module_id, _) = seeded().await;
        let created = create_surface_annotation(
            &state,
            surface_id,
            annotation(module_id, "withdrawn on reflection"),
            "user",
        )
        .await
        .unwrap();
        let before_delete = storage
            .get_page(surface_id)
            .await
            .unwrap()
            .unwrap()
            .updated_at;

        delete_surface_annotation(&state, created.id, "user")
            .await
            .unwrap();

        assert!(
            storage
                .get_surface_annotation(created.id)
                .await
                .unwrap()
                .is_none(),
            "the note is gone from the surface"
        );
        let events = storage.list_audit_events().await.unwrap();
        let recorded = events
            .iter()
            .find(|event| event.event_kind == "surface_annotation_deleted")
            .expect("the removal is in the surface's trail");
        assert_eq!(
            recorded.metadata["prior_body"], "withdrawn on reflection",
            "recovery is re-writing the note, informed by the trail"
        );
        assert_eq!(recorded.metadata["prior_tags"], json!([]));
        assert!(recorded.metadata["new_body"].is_null());
        assert!(recorded.metadata["new_tags"].is_null());
        assert!(
            storage
                .get_page(surface_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at
                > before_delete,
            "deleting a note advances the surface's freshness marker"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_annotation_deletes_leave_one_audit_event() {
        let (state, storage, surface_id, module_id, _) = seeded().await;
        let created = create_surface_annotation(
            &state,
            surface_id,
            annotation(module_id, "only one removal is recorded"),
            "user",
        )
        .await
        .unwrap();

        let (first, second) = tokio::join!(
            delete_surface_annotation(&state, created.id, "user"),
            delete_surface_annotation(&state, created.id, "user"),
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let failure = first
            .err()
            .or(second.err())
            .expect("one delete loses the race");
        assert!(matches!(
            failure,
            GatewayError::Storage(StorageError::NotFound { .. })
        ));
        assert_eq!(
            storage
                .list_audit_events()
                .await
                .unwrap()
                .iter()
                .filter(|event| event.event_kind == "surface_annotation_deleted")
                .count(),
            1,
            "the losing delete must not append a second audit event"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_that_loses_after_reading_the_note_returns_not_found_without_an_audit() {
        let (state, storage, surface_id, module_id, _) = seeded().await;
        let created = create_surface_annotation(
            &state,
            surface_id,
            annotation(module_id, "both requests read this note first"),
            "user",
        )
        .await
        .unwrap();
        let existing = storage
            .get_surface_annotation(created.id)
            .await
            .unwrap()
            .expect("both concurrent requests read the note before either deletes it");

        let (first, second) = tokio::join!(
            delete_surface_annotation_after_read(&state, created.id, existing.clone(), "user"),
            delete_surface_annotation_after_read(&state, created.id, existing, "user"),
        );

        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let failure = first
            .err()
            .or(second.err())
            .expect("the second delete reaches the storage zero-row guard");
        assert!(matches!(
            failure,
            GatewayError::Storage(StorageError::NotFound { .. })
        ));
        assert_eq!(
            storage
                .list_audit_events()
                .await
                .unwrap()
                .iter()
                .filter(|event| event.event_kind == "surface_annotation_deleted")
                .count(),
            1,
            "the zero-row loser must not append an audit event"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_deletion_audit_rolls_back_the_annotation_removal() {
        let (state, storage, surface_id, module_id, _) = seeded().await;
        let annotation = create_surface_annotation(
            &state,
            surface_id,
            annotation(module_id, "the row must survive a failed audit"),
            "user",
        )
        .await
        .unwrap();
        let duplicate_id = "deletion-audit-id";
        storage
            .create_audit_event(NewAuditEvent {
                id: duplicate_id.into(),
                session_id: None,
                entity_kind: "test".into(),
                entity_id: annotation.id.to_string(),
                event_kind: "seed".into(),
                summary: "occupy the deletion audit id".into(),
                metadata: json!({}),
            })
            .await
            .unwrap();
        let existing = storage
            .get_surface_annotation(annotation.id)
            .await
            .unwrap()
            .unwrap();
        let audit = annotation_change_event(
            "Planning",
            surface_id,
            &existing,
            "surface_annotation_deleted",
            None,
            None,
            "user",
        );
        let audit = NewAuditEvent {
            id: duplicate_id.into(),
            ..audit
        };

        assert!(
            storage
                .delete_surface_annotation_with_audit(existing, audit,)
                .await
                .is_err()
        );
        assert!(
            storage
                .get_surface_annotation(annotation.id)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_live_signal_names_the_annotated_module_and_carries_no_words() {
        let (state, _storage, surface_id, module_id, _) = seeded().await;
        let mut events = state.broadcast.subscribe();

        create_surface_annotation(
            &state,
            surface_id,
            annotation(module_id, "secret words that must not ride the wire"),
            "user",
        )
        .await
        .unwrap();

        let event = events.recv().await.expect("the annotation broadcasts");
        match &event {
            ServerEvent::SurfaceUpdated {
                surface_id: signalled_page,
                module_id: signalled_module,
            } => {
                assert_eq!(*signalled_page, surface_id);
                assert_eq!(
                    *signalled_module,
                    Some(module_id),
                    "the signal names where to look"
                );
            }
            other => panic!("expected a surface_updated signal, got {other:?}"),
        }
        let serialised = serde_json::to_string(&event).unwrap();
        assert!(
            !serialised.contains("secret words"),
            "the event is a signal: content rides the refetch, never the event"
        );
    }
}
