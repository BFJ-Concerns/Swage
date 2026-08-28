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
    CreatePageRequest, PageListResponse, PageModuleView, PageResponse, PageSummary,
    WritePageModuleRequest,
};
use swage_protocol::client::websocket::ServerEvent;
use swage_storage::{NewAuditEvent, NewPage, NewPageModule, PageModule};

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
    let page = state
        .storage
        .create_page(NewPage {
            space_id,
            title: title.to_string(),
        })
        .await?;
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "page".into(),
            entity_id: page.id.to_string(),
            event_kind: "page_created".into(),
            summary: format!("page created: {title}"),
            metadata: json!({ "space_id": page.space_id, "title": title }),
        })
        .await?;
    // Creation is a page-level change: open surfaces refetch their list.
    let _ = state.broadcast.send(ServerEvent::PageUpdated {
        page_id: page.id,
        module_id: None,
    });
    Ok(PageResponse {
        id: page.id,
        space_id: page.space_id,
        title: page.title,
        created_at: page.created_at,
        updated_at: page.updated_at,
        modules: Vec::new(),
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
