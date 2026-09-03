//! Pages — agent-run tabs on a space, assembled from module instances.
//!
//! The database is authoritative for page content; every mutation lands an
//! audit event (the page's provenance trail) and broadcasts a live
//! `PageUpdated` signal so open surfaces refetch rather than poll. Module
//! kinds are compiled-in until module authoring lands; `document` (markdown
//! source, client-rendered) is the first.

use axum::extract::State;
use serde_json::json;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_protocol::client::page::{
    CreatePageAnnotationRequest, CreatePageRequest, CreatePageTemplateRequest,
    DeletePageAnnotationResponse, PageAnnotationView, PageListResponse, PageModuleView,
    PageResponse, PageSummary, PageTemplateListResponse, PageTemplateModuleView,
    PageTemplateResponse, PageTemplateSummary, UpdatePageAnnotationRequest, WritePageModuleRequest,
};
use swage_protocol::client::websocket::ServerEvent;
use swage_storage::{
    NewAuditEvent, NewPage, NewPageAnnotation, NewPageModule, NewPageTemplate,
    NewPageTemplateModuleInput, PageAnnotation, PageModule,
};

use crate::error::{ApiError, Json, Path};
use crate::state::AppState;

/// The module kinds the client can render. A write naming any other kind is
/// rejected before anything lands — an unrenderable module would be a page
/// the user cannot see.
const MODULE_KINDS: &[&str] = &["document"];

pub async fn create_page(
    state: &AppState,
    space_id: Uuid,
    request: CreatePageRequest,
) -> Result<PageResponse, GatewayError> {
    let title = request.title.trim();
    if title.is_empty() {
        return Err(GatewayError::ValidationError(
            "a page needs a non-empty title".into(),
        ));
    }
    state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;

    // A template seeds the page; everything is validated before anything
    // lands, so a rejected fork leaves no page behind.
    let template = match request.template_id {
        Some(template_id) => Some(resolve_template(state, space_id, template_id).await?),
        None => None,
    };

    let page = match &template {
        Some(template) => {
            state
                .storage
                .create_page_from_template(
                    NewPage {
                        space_id,
                        title: title.to_string(),
                    },
                    template.id,
                )
                .await?
        }
        None => {
            state
                .storage
                .create_page(NewPage {
                    space_id,
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
            entity_kind: "page".into(),
            entity_id: page.id.to_string(),
            event_kind: "page_created".into(),
            summary: match &template {
                Some(template) => {
                    format!("page created from template '{}': {title}", template.name)
                }
                None => format!("page created: {title}"),
            },
            metadata: json!({
                "space_id": page.space_id,
                "title": title,
                "forked_from": template_metadata,
            }),
        })
        .await?;
    // Creation is a page-level change: open surfaces refetch their list.
    let _ = state.broadcast.send(ServerEvent::PageUpdated {
        page_id: page.id,
        module_id: None,
    });
    let modules = state.storage.list_page_modules(page.id).await?;
    Ok(PageResponse {
        id: page.id,
        space_id: page.space_id,
        title: page.title,
        created_at: page.created_at,
        updated_at: page.updated_at,
        modules: modules
            .into_iter()
            .map(|module| module_view(module, Vec::new()))
            .collect(),
        forked_from_template_id: page.forked_from_template_id,
        forked_at: page.forked_at,
    })
}

/// A template usable from this space: it must exist and belong here.
/// Templates are space-local — there is no sharing across spaces.
async fn resolve_template(
    state: &AppState,
    space_id: Uuid,
    template_id: Uuid,
) -> Result<swage_storage::PageTemplate, GatewayError> {
    state
        .storage
        .get_page_template(template_id)
        .await?
        .filter(|template| template.space_id == space_id)
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no template {template_id} in space {space_id}"))
        })
}

/// Create a space's reusable page shape. A template is inert: it seeds
/// pages at their creation and is never projected into pages already
/// forked from it.
pub async fn create_page_template(
    state: &AppState,
    space_id: Uuid,
    request: CreatePageTemplateRequest,
) -> Result<PageTemplateResponse, GatewayError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(GatewayError::ValidationError(
            "a template needs a non-empty name".into(),
        ));
    }
    state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
    // Validate every module kind before the first write: a template that
    // seeds an unrenderable module is a page the user cannot see.
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
        .create_page_template_with_modules(
            NewPageTemplate {
                space_id,
                name: name.to_string(),
            },
            request
                .modules
                .iter()
                .enumerate()
                .map(|(position, module)| NewPageTemplateModuleInput {
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
            entity_kind: "page_template".into(),
            entity_id: template.id.to_string(),
            event_kind: "page_template_created".into(),
            summary: format!("page template created: {name}"),
            metadata: json!({
                "space_id": template.space_id,
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
) -> Result<PageTemplateResponse, GatewayError> {
    let template = state
        .storage
        .get_page_template(template_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no template with id {template_id}"))
        })?;
    let modules = state
        .storage
        .list_page_template_modules(template.id)
        .await?;
    Ok(PageTemplateResponse {
        id: template.id,
        space_id: template.space_id,
        name: template.name,
        created_at: template.created_at,
        updated_at: template.updated_at,
        modules: modules
            .into_iter()
            .map(|module| PageTemplateModuleView {
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
pub async fn write_page_module(
    state: &AppState,
    page_id: Uuid,
    request: WritePageModuleRequest,
    author: &str,
) -> Result<PageModuleView, GatewayError> {
    let page = state
        .storage
        .get_page(page_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError(format!("no page with id {page_id}")))?;

    let (module, prior_source) = match request.module_id {
        Some(module_id) => {
            let existing = state
                .storage
                .get_page_module(module_id)
                .await?
                .filter(|module| module.page_id == page.id)
                .ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "no module {module_id} on page {page_id}"
                    ))
                })?;
            state
                .storage
                .update_page_module_source(existing.id, &request.source)
                .await?;
            // Read the row back rather than patching a copy: the store
            // stamped updated_at, and the view must carry the stored truth.
            let updated = state
                .storage
                .get_page_module(existing.id)
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
            let position = state.storage.list_page_modules(page.id).await?.len() as i32;
            let created = state
                .storage
                .create_page_module(NewPageModule {
                    page_id: page.id,
                    kind: kind.to_string(),
                    position,
                    source: request.source.clone(),
                })
                .await?;
            (created, None)
        }
    };

    // Provenance: who changed what, with the displaced content retained —
    // the page row holds only current truth, so the trail is where a
    // superseded source survives. Recording failure propagates AFTER the
    // mutation landed — a loud applied-but-unrecorded error, the platform's
    // decided posture (see record_config_change) — never a success report
    // on an unrecorded change, and never a second buffered record.
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "page".into(),
            entity_id: page.id.to_string(),
            event_kind: "page_module_written".into(),
            summary: format!("module written on page: {}", page.title),
            metadata: json!({
                "module_id": module.id,
                "kind": module.kind,
                "author": author,
                "prior_source": prior_source,
                "new_source": request.source,
            }),
        })
        .await?;

    // The live signal: open surfaces refetch the page. Content rides the
    // fetch, not the event.
    let _ = state.broadcast.send(ServerEvent::PageUpdated {
        page_id: page.id,
        module_id: Some(module.id),
    });

    let annotations = state
        .storage
        .list_page_annotations(page.id)
        .await?
        .into_iter()
        .filter(|annotation| annotation.module_id == module.id)
        .map(annotation_view)
        .collect();
    Ok(module_view(module, annotations))
}

/// Leave a note on one of a page's modules. `author` names who left it in the
/// provenance record (an agent id, or "user" for the web surface).
///
/// A note carries no authority: it is one party's words recorded against a
/// stable id, and nothing keys any permission off it.
pub async fn create_page_annotation(
    state: &AppState,
    page_id: Uuid,
    request: CreatePageAnnotationRequest,
    author: &str,
) -> Result<PageAnnotationView, GatewayError> {
    let page = state
        .storage
        .get_page(page_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError(format!("no page with id {page_id}")))?;
    let body = request.body.trim();
    if body.is_empty() {
        return Err(GatewayError::ValidationError(
            "an annotation needs a non-empty body".into(),
        ));
    }
    // The anchor must be real and must be on this page: a note addressed to
    // another page's module would name a location neither party can see.
    let module_id = request.module_id;
    state
        .storage
        .get_page_module(module_id)
        .await?
        .filter(|module| module.page_id == page.id)
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no module {module_id} on page {page_id}"))
        })?;
    let tags: Vec<String> = request
        .tags
        .iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();

    let annotation = state
        .storage
        .create_page_annotation(NewPageAnnotation {
            page_id: page.id,
            module_id,
            body: body.to_string(),
            tags,
            author: author.to_string(),
        })
        .await?;

    record_annotation_change(
        state,
        &page.title,
        page.id,
        &annotation,
        "page_annotation_created",
        None,
        author,
    )
    .await?;

    let _ = state.broadcast.send(ServerEvent::PageUpdated {
        page_id: page.id,
        module_id: Some(annotation.module_id),
    });

    Ok(annotation_view(annotation))
}

/// Replace an annotation's body and tags. The displaced text survives in the
/// provenance trail, which is the whole of the history — there is no version
/// row and no undo.
pub async fn update_page_annotation(
    state: &AppState,
    annotation_id: Uuid,
    request: UpdatePageAnnotationRequest,
    author: &str,
) -> Result<PageAnnotationView, GatewayError> {
    let existing = state
        .storage
        .get_page_annotation(annotation_id)
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
        .update_page_annotation(existing.id, body, &tags)
        .await?;
    // Read the row back rather than patching a copy: the store stamped
    // updated_at, and the view must carry the stored truth.
    let updated = state
        .storage
        .get_page_annotation(existing.id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!(
                "annotation {annotation_id} disappeared during the write"
            ))
        })?;

    let page = state.storage.get_page(updated.page_id).await?;
    let title = page.map(|page| page.title).unwrap_or_default();
    record_annotation_change(
        state,
        &title,
        updated.page_id,
        &updated,
        "page_annotation_updated",
        Some(&existing),
        author,
    )
    .await?;

    let _ = state.broadcast.send(ServerEvent::PageUpdated {
        page_id: updated.page_id,
        module_id: Some(updated.module_id),
    });

    Ok(annotation_view(updated))
}

/// Remove an annotation. The trail retains what it said, so the deletion is
/// recoverable by re-writing the note, never by an undo mechanism.
pub async fn delete_page_annotation(
    state: &AppState,
    annotation_id: Uuid,
    author: &str,
) -> Result<(), GatewayError> {
    let existing = state
        .storage
        .get_page_annotation(annotation_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no annotation with id {annotation_id}"))
        })?;
    state.storage.delete_page_annotation(existing.id).await?;

    let page = state.storage.get_page(existing.page_id).await?;
    let title = page.map(|page| page.title).unwrap_or_default();
    record_annotation_change(
        state,
        &title,
        existing.page_id,
        &existing,
        "page_annotation_deleted",
        Some(&existing),
        author,
    )
    .await?;

    let _ = state.broadcast.send(ServerEvent::PageUpdated {
        page_id: existing.page_id,
        module_id: Some(existing.module_id),
    });
    Ok(())
}

/// Provenance for an annotation mutation: who changed what, with the
/// displaced body and tags retained — the row holds only current truth, so
/// the trail is where a superseded note survives. Recording failure
/// propagates AFTER the mutation landed, matching write_page_module: a loud
/// applied-but-unrecorded error, never a success report on an unrecorded
/// change.
async fn record_annotation_change(
    state: &AppState,
    page_title: &str,
    page_id: Uuid,
    annotation: &PageAnnotation,
    event_kind: &str,
    prior: Option<&PageAnnotation>,
    author: &str,
) -> Result<(), GatewayError> {
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "page".into(),
            entity_id: page_id.to_string(),
            event_kind: event_kind.into(),
            summary: format!("annotation change on page: {page_title}"),
            metadata: json!({
                "annotation_id": annotation.id,
                "module_id": annotation.module_id,
                "author": author,
                "annotation_author": annotation.author,
                "prior_body": prior.map(|prior| prior.body.clone()),
                "prior_tags": prior.map(|prior| prior.tags.clone()),
                "new_body": annotation.body,
                "new_tags": annotation.tags,
            }),
        })
        .await?;
    Ok(())
}

pub async fn page_response(state: &AppState, page_id: Uuid) -> Result<PageResponse, GatewayError> {
    let page = state
        .storage
        .get_page(page_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError(format!("no page with id {page_id}")))?;
    let modules = state.storage.list_page_modules(page.id).await?;
    let annotations = state.storage.list_page_annotations(page.id).await?;
    // Each note travels with the module it hangs on, so the reader finds it
    // where the location is rather than having to match ids by hand.
    Ok(PageResponse {
        id: page.id,
        space_id: page.space_id,
        title: page.title,
        created_at: page.created_at,
        updated_at: page.updated_at,
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
        forked_from_template_id: page.forked_from_template_id,
        forked_at: page.forked_at,
    })
}

fn module_view(module: PageModule, annotations: Vec<PageAnnotationView>) -> PageModuleView {
    PageModuleView {
        id: module.id,
        kind: module.kind,
        position: module.position,
        source: module.source,
        updated_at: module.updated_at,
        annotations,
    }
}

fn annotation_view(annotation: PageAnnotation) -> PageAnnotationView {
    PageAnnotationView {
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
    Path(space_id): Path<Uuid>,
) -> Result<Json<PageListResponse>, ApiError> {
    let pages = state
        .storage
        .list_pages_by_space(space_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Json(PageListResponse {
        pages: pages
            .into_iter()
            .map(|page| PageSummary {
                id: page.id,
                title: page.title,
                updated_at: page.updated_at,
            })
            .collect(),
    }))
}

pub async fn create_page_handler(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
    Json(request): Json<CreatePageRequest>,
) -> Result<Json<PageResponse>, ApiError> {
    Ok(Json(create_page(&state, space_id, request).await?))
}

pub async fn get_page_handler(
    State(state): State<AppState>,
    Path(page_id): Path<Uuid>,
) -> Result<Json<PageResponse>, ApiError> {
    Ok(Json(page_response(&state, page_id).await?))
}

pub async fn write_page_module_handler(
    State(state): State<AppState>,
    Path(page_id): Path<Uuid>,
    Json(request): Json<WritePageModuleRequest>,
) -> Result<Json<PageModuleView>, ApiError> {
    Ok(Json(
        write_page_module(&state, page_id, request, "user").await?,
    ))
}

pub async fn create_page_annotation_handler(
    State(state): State<AppState>,
    Path(page_id): Path<Uuid>,
    Json(request): Json<CreatePageAnnotationRequest>,
) -> Result<Json<PageAnnotationView>, ApiError> {
    Ok(Json(
        create_page_annotation(&state, page_id, request, "user").await?,
    ))
}

pub async fn update_page_annotation_handler(
    State(state): State<AppState>,
    Path(annotation_id): Path<Uuid>,
    Json(request): Json<UpdatePageAnnotationRequest>,
) -> Result<Json<PageAnnotationView>, ApiError> {
    Ok(Json(
        update_page_annotation(&state, annotation_id, request, "user").await?,
    ))
}

pub async fn delete_page_annotation_handler(
    State(state): State<AppState>,
    Path(annotation_id): Path<Uuid>,
) -> Result<Json<DeletePageAnnotationResponse>, ApiError> {
    delete_page_annotation(&state, annotation_id, "user").await?;
    Ok(Json(DeletePageAnnotationResponse { deleted: true }))
}

pub async fn list_page_templates_handler(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
) -> Result<Json<PageTemplateListResponse>, ApiError> {
    let templates = state
        .storage
        .list_page_templates_by_space(space_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Json(PageTemplateListResponse {
        templates: templates
            .into_iter()
            .map(|template| PageTemplateSummary {
                id: template.id,
                name: template.name,
                updated_at: template.updated_at,
            })
            .collect(),
    }))
}

pub async fn create_page_template_handler(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
    Json(request): Json<CreatePageTemplateRequest>,
) -> Result<Json<PageTemplateResponse>, ApiError> {
    Ok(Json(create_page_template(&state, space_id, request).await?))
}

pub async fn get_page_template_handler(
    State(state): State<AppState>,
    Path(template_id): Path<Uuid>,
) -> Result<Json<PageTemplateResponse>, ApiError> {
    Ok(Json(template_response(&state, template_id).await?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_storage::{
        AuditStore, NewSpace, PageAnnotationStore, SpaceStore, SqliteStorage, Storage,
    };

    use super::*;

    fn state(storage: Arc<SqliteStorage>) -> AppState {
        AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
    }

    /// A page with one module, and a second page with its own module — the
    /// second is what a cross-page anchor would wrongly reach.
    async fn seeded() -> (AppState, Arc<SqliteStorage>, Uuid, Uuid, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let space = SpaceStore::create(
            storage.as_ref(),
            NewSpace {
                name: "p".into(),
                slug: "p".into(),
            },
        )
        .await
        .unwrap();
        let state = state(Arc::clone(&storage));
        let page = create_page(
            &state,
            space.id,
            CreatePageRequest {
                title: "Planning".into(),
                template_id: None,
            },
        )
        .await
        .unwrap();
        let module = write_page_module(
            &state,
            page.id,
            WritePageModuleRequest {
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
            space.id,
            CreatePageRequest {
                title: "Elsewhere".into(),
                template_id: None,
            },
        )
        .await
        .unwrap();
        let other_module = write_page_module(
            &state,
            other.id,
            WritePageModuleRequest {
                module_id: None,
                kind: None,
                source: "# Other".into(),
            },
            "user",
        )
        .await
        .unwrap();
        (state, storage, page.id, module.id, other_module.id)
    }

    fn annotation(module_id: Uuid, body: &str) -> CreatePageAnnotationRequest {
        CreatePageAnnotationRequest {
            module_id,
            body: body.into(),
            tags: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_annotation_reaches_the_reader_at_the_module_it_names() {
        let (state, _storage, page_id, module_id, other_module_id) = seeded().await;

        create_page_annotation(
            &state,
            page_id,
            annotation(module_id, "the second claim needs a source"),
            "user",
        )
        .await
        .unwrap();
        create_page_annotation(
            &state,
            page_id,
            annotation(other_module_id, "needs a second pass"),
            "user",
        )
        .await
        .expect_err("a module on another page is not an anchor");

        let response = page_response(&state, page_id).await.unwrap();
        let module = response
            .modules
            .iter()
            .find(|module| module.id == module_id)
            .expect("the module is on the page");
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
        let (state, storage, page_id, module_id, _) = seeded().await;

        let error = create_page_annotation(&state, page_id, annotation(module_id, "   "), "user")
            .await
            .expect_err("an empty note names no reference");
        assert!(error.to_string().contains("non-empty"), "{error}");
        assert!(
            storage
                .list_page_annotations(page_id)
                .await
                .unwrap()
                .is_empty(),
            "validation runs before the store is touched"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_module_on_another_page_is_not_an_anchor() {
        let (state, storage, page_id, _module_id, other_module_id) = seeded().await;

        let error = create_page_annotation(
            &state,
            page_id,
            annotation(other_module_id, "hangs nowhere either party can see"),
            "user",
        )
        .await
        .expect_err("the anchor must be on the named page");
        assert!(
            error.to_string().contains(&other_module_id.to_string()),
            "{error}"
        );
        assert!(
            storage
                .list_page_annotations(page_id)
                .await
                .unwrap()
                .is_empty(),
            "the rejected note never landed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restating_a_note_leaves_the_displaced_words_in_the_trail() {
        let (state, storage, page_id, module_id, _) = seeded().await;
        let created = create_page_annotation(
            &state,
            page_id,
            CreatePageAnnotationRequest {
                module_id,
                body: "first reading".into(),
                tags: vec!["draft".into()],
            },
            "user",
        )
        .await
        .unwrap();

        update_page_annotation(
            &state,
            created.id,
            UpdatePageAnnotationRequest {
                body: "second reading".into(),
                tags: vec!["settled".into()],
            },
            "assistant",
        )
        .await
        .unwrap();

        let stored = storage
            .get_page_annotation(created.id)
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
            .find(|event| event.event_kind == "page_annotation_updated")
            .expect("the restatement is in the page's trail");
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
        let (state, storage, page_id, module_id, _) = seeded().await;
        let created = create_page_annotation(
            &state,
            page_id,
            annotation(module_id, "withdrawn on reflection"),
            "user",
        )
        .await
        .unwrap();

        delete_page_annotation(&state, created.id, "user")
            .await
            .unwrap();

        assert!(
            storage
                .get_page_annotation(created.id)
                .await
                .unwrap()
                .is_none(),
            "the note is gone from the page"
        );
        let events = storage.list_audit_events().await.unwrap();
        let recorded = events
            .iter()
            .find(|event| event.event_kind == "page_annotation_deleted")
            .expect("the removal is in the page's trail");
        assert_eq!(
            recorded.metadata["prior_body"], "withdrawn on reflection",
            "recovery is re-writing the note, informed by the trail"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_live_signal_names_the_annotated_module_and_carries_no_words() {
        let (state, _storage, page_id, module_id, _) = seeded().await;
        let mut events = state.broadcast.subscribe();

        create_page_annotation(
            &state,
            page_id,
            annotation(module_id, "secret words that must not ride the wire"),
            "user",
        )
        .await
        .unwrap();

        let event = events.recv().await.expect("the annotation broadcasts");
        match &event {
            ServerEvent::PageUpdated {
                page_id: signalled_page,
                module_id: signalled_module,
            } => {
                assert_eq!(*signalled_page, page_id);
                assert_eq!(
                    *signalled_module,
                    Some(module_id),
                    "the signal names where to look"
                );
            }
            other => panic!("expected a page_updated signal, got {other:?}"),
        }
        let serialised = serde_json::to_string(&event).unwrap();
        assert!(
            !serialised.contains("secret words"),
            "the event is a signal: content rides the refetch, never the event"
        );
    }
}
