//! Pages — agent-run tabs on a space, assembled from module instances.
//!
//! The database is authoritative for page content; every mutation lands an
//! audit event (the page's provenance trail) and broadcasts a live
//! `PageUpdated` signal so open surfaces refetch rather than poll. Module
//! kinds are compiled-in until module authoring lands; `document` (markdown
//! source, client-rendered) is the first.

use axum::Json;
use axum::extract::{Path, State};
use serde_json::json;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_protocol::client::page::{
    CreatePageRequest, CreatePageTemplateRequest, PageListResponse, PageModuleView, PageResponse,
    PageSummary, PageTemplateListResponse, PageTemplateModuleView, PageTemplateResponse,
    PageTemplateSummary, WritePageModuleRequest,
};
use swage_protocol::client::websocket::ServerEvent;
use swage_storage::{
    NewAuditEvent, NewPage, NewPageModule, NewPageTemplate, NewPageTemplateModule, PageModule,
};

use crate::error::ApiError;
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
        modules: modules.into_iter().map(module_view).collect(),
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
        .create_page_template(NewPageTemplate {
            space_id,
            name: name.to_string(),
        })
        .await?;
    for (position, module) in request.modules.iter().enumerate() {
        state
            .storage
            .create_page_template_module(NewPageTemplateModule {
                template_id: template.id,
                kind: module.kind.clone().unwrap_or_else(|| "document".into()),
                position: position as i32,
                source: module.source.clone(),
            })
            .await?;
    }

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

    Ok(module_view(module))
}

pub async fn page_response(state: &AppState, page_id: Uuid) -> Result<PageResponse, GatewayError> {
    let page = state
        .storage
        .get_page(page_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError(format!("no page with id {page_id}")))?;
    let modules = state.storage.list_page_modules(page.id).await?;
    Ok(PageResponse {
        id: page.id,
        space_id: page.space_id,
        title: page.title,
        created_at: page.created_at,
        updated_at: page.updated_at,
        modules: modules.into_iter().map(module_view).collect(),
        forked_from_template_id: page.forked_from_template_id,
        forked_at: page.forked_at,
    })
}

fn module_view(module: PageModule) -> PageModuleView {
    PageModuleView {
        id: module.id,
        kind: module.kind,
        position: module.position,
        source: module.source,
        updated_at: module.updated_at,
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
