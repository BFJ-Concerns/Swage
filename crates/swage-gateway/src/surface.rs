//! Surfaces — a session's work surfaces, assembled from module instances —
//! and the shared library they are preserved in.
//!
//! A surface belongs to the session that created it and stays with that
//! session's record. Either party can preserve one in the library with a
//! name, a description of what it does and when to use it, and tags; any
//! session can add a library entry as a new surface of its own. A topic's
//! default entries open in each of its new sessions. Content travels with an
//! entry only within the scope it was saved from: added elsewhere, the shape
//! comes over and the modules start empty for the destination's data.
//!
//! The database is authoritative for surface content; every mutation lands an
//! audit event (the surface's provenance trail) and broadcasts a live
//! `SurfaceUpdated` signal so open surfaces refetch rather than poll. Module
//! kinds are compiled-in until module authoring lands; `document` (markdown
//! source, client-rendered) is the first.

use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use swage_core::{GatewayError, StorageError};
use swage_protocol::client::surface::{
    CreateSurfaceAnnotationRequest, CreateSurfaceRequest, DeleteSurfaceAnnotationResponse,
    LibraryEntryModuleView, LibraryEntryResponse, LibraryEntrySummary, LibraryListResponse,
    SaveSurfaceToLibraryRequest, SurfaceAnnotationView, SurfaceListResponse, SurfaceModuleView,
    SurfaceResponse, SurfaceSummary, TopicDefaultSurfaceRequest, UpdateSurfaceAnnotationRequest,
    WriteSurfaceModuleRequest,
};
use swage_protocol::client::websocket::ServerEvent;
use swage_storage::{
    LibraryEntry, LibrarySeeding, NewAuditEvent, NewLibraryEntry, NewLibraryEntryModule,
    NewSurface, NewSurfaceAnnotation, NewSurfaceModule, Session, SurfaceAnnotation, SurfaceModule,
};

use crate::error::{ApiError, Json, Path};
use crate::state::AppState;

/// The module kinds the client can render. A write naming any other kind is
/// rejected before anything lands — an unrenderable module would be a surface
/// the user cannot see.
const MODULE_KINDS: &[&str] = &["document"];

async fn session_or_not_found(state: &AppState, session_id: Uuid) -> Result<Session, GatewayError> {
    state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })
}

async fn entry_or_invalid(state: &AppState, entry_id: Uuid) -> Result<LibraryEntry, GatewayError> {
    state
        .storage
        .get_library_entry(entry_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no library entry with id {entry_id}"))
        })
}

/// Whether a library entry's content travels into a session: only within the
/// scope the entry was saved from. Every other scope receives the shape and
/// supplies its own data — another topic's content never arrives by accident.
pub fn seeding_for(entry: &LibraryEntry, session: &Session) -> LibrarySeeding {
    if entry.origin_topic_id == session.topic_id {
        LibrarySeeding::WithContent
    } else {
        LibrarySeeding::ShapeOnly
    }
}

/// Create a surface for a session: blank, or seeded from a library entry.
/// Everything is validated before anything lands, so a rejected seed leaves
/// no surface behind.
pub async fn create_surface(
    state: &AppState,
    session_id: Uuid,
    request: CreateSurfaceRequest,
) -> Result<SurfaceResponse, GatewayError> {
    let title = request.title.trim();
    if title.is_empty() {
        return Err(GatewayError::ValidationError(
            "a surface needs a non-empty title".into(),
        ));
    }
    let session = session_or_not_found(state, session_id).await?;
    let entry = match request.library_entry_id {
        Some(entry_id) => Some(entry_or_invalid(state, entry_id).await?),
        None => None,
    };

    let surface = match &entry {
        Some(entry) => {
            state
                .storage
                .create_surface_from_library(
                    NewSurface {
                        session_id,
                        title: title.to_string(),
                    },
                    entry.id,
                    seeding_for(entry, &session),
                )
                .await?
        }
        None => {
            state
                .storage
                .create_page(NewSurface {
                    session_id,
                    title: title.to_string(),
                })
                .await?
        }
    };

    let seeded_from = entry.as_ref().map(|entry| {
        json!({
            "library_entry_id": entry.id,
            "name": entry.name,
            "seeding": match seeding_for(entry, &session) {
                LibrarySeeding::WithContent => "with_content",
                LibrarySeeding::ShapeOnly => "shape_only",
            },
        })
    });
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: Some(session_id),
            entity_kind: "surface".into(),
            entity_id: surface.id.to_string(),
            event_kind: "surface_created".into(),
            summary: match &entry {
                Some(entry) => format!(
                    "surface added from the library entry '{}': {title}",
                    entry.name
                ),
                None => format!("surface created: {title}"),
            },
            metadata: json!({
                "session_id": session_id,
                "topic_id": session.topic_id,
                "title": title,
                "seeded_from": seeded_from,
            }),
        })
        .await?;
    // Creation is a surface-level change: open surfaces refetch their list.
    let _ = state.broadcast.send(ServerEvent::SurfaceUpdated {
        surface_id: surface.id,
        module_id: None,
    });
    surface_response(state, surface.id).await
}

/// Seed a new session's default surfaces: its topic's designated library
/// entries, each added as the session's own copy through the same
/// content rule as any other add. The general scope designates none, so a
/// topicless session opens with nothing. Called wherever a person's
/// session is opened — the conversation service's two creation paths, the
/// channel paths — never for a worker or a delegated child, whose surfaces
/// are the dispatcher's concern.
pub async fn open_default_surfaces(
    storage: &dyn swage_storage::Storage,
    session_id: Uuid,
) -> Result<(), GatewayError> {
    let session = storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    for entry in storage.list_topic_default_entries(session.topic_id).await? {
        let seeding = seeding_for(&entry, &session);
        storage
            .create_surface_from_library(
                NewSurface {
                    session_id,
                    title: entry.name,
                },
                entry.id,
                seeding,
            )
            .await?;
    }
    Ok(())
}

/// Preserve a surface in the shared library. The entry copies the surface's
/// modules as they stand; the surface itself is untouched and stays with its
/// session. Saving opens the entry nowhere.
pub async fn save_surface_to_library(
    state: &AppState,
    surface_id: Uuid,
    request: SaveSurfaceToLibraryRequest,
    author: &str,
) -> Result<LibraryEntryResponse, GatewayError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(GatewayError::ValidationError(
            "a library entry needs a non-empty name".into(),
        ));
    }
    let description = request.description.trim();
    if description.is_empty() {
        return Err(GatewayError::ValidationError(
            "a library entry needs a description of what the surface does and when to use it"
                .into(),
        ));
    }
    let surface =
        state.storage.get_page(surface_id).await?.ok_or_else(|| {
            GatewayError::ValidationError(format!("no surface with id {surface_id}"))
        })?;
    let session = session_or_not_found(state, surface.session_id).await?;
    let tags = normalised_tags(&request.tags);
    let modules = state
        .storage
        .list_surface_modules(surface.id)
        .await?
        .into_iter()
        .map(|module| NewLibraryEntryModule {
            kind: module.kind,
            position: module.position,
            source: module.source,
        })
        .collect::<Vec<_>>();

    let entry = state
        .storage
        .create_library_entry(
            NewLibraryEntry {
                name: name.to_string(),
                description: description.to_string(),
                tags,
                origin_topic_id: session.topic_id,
                saved_from_surface_id: Some(surface.id),
            },
            modules,
        )
        .await?;

    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: Some(session.id),
            entity_kind: "surface_library_entry".into(),
            entity_id: entry.id.to_string(),
            event_kind: "surface_saved_to_library".into(),
            summary: format!(
                "surface saved to the library as '{name}': {}",
                surface.title
            ),
            metadata: json!({
                "surface_id": surface.id,
                "origin_topic_id": session.topic_id,
                "author": author,
                "name": name,
                "description": description,
                "tags": entry.tags,
            }),
        })
        .await?;
    let _ = state
        .broadcast
        .send(ServerEvent::LibraryUpdated { entry_id: entry.id });

    library_entry_response(state, entry.id).await
}

fn normalised_tags(tags: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tags.iter()
        .map(|tag| tag.trim().to_lowercase())
        .filter(|tag| !tag.is_empty() && seen.insert(tag.clone()))
        .collect()
}

/// The library's catalogue: names, descriptions and tags, never module
/// content. `tags` narrows to entries carrying any of them; an empty list is
/// the whole library.
pub async fn library_catalogue(
    state: &AppState,
    tags: &[String],
) -> Result<LibraryListResponse, GatewayError> {
    let wanted = normalised_tags(tags);
    let scopes = state.topic_store.list_scopes().await?;
    let entries = state
        .storage
        .list_library_entries()
        .await?
        .into_iter()
        .filter(|entry| wanted.is_empty() || entry.tags.iter().any(|tag| wanted.contains(tag)))
        .map(|entry| entry_summary(entry, &scopes))
        .collect();
    Ok(LibraryListResponse { entries })
}

fn entry_summary(entry: LibraryEntry, scopes: &[swage_storage::Topic]) -> LibraryEntrySummary {
    let origin = scopes
        .iter()
        .find(|scope| scope.id == entry.origin_topic_id);
    LibraryEntrySummary {
        id: entry.id,
        name: entry.name,
        description: entry.description,
        tags: entry.tags,
        origin_topic_id: origin
            .and_then(|scope| scope.client_topic_id())
            .or_else(|| origin.is_none().then_some(entry.origin_topic_id)),
        origin_topic_deleted: origin.is_none(),
        updated_at: entry.updated_at,
    }
}

pub async fn library_entry_response(
    state: &AppState,
    entry_id: Uuid,
) -> Result<LibraryEntryResponse, GatewayError> {
    let entry = entry_or_invalid(state, entry_id).await?;
    let origin = state.topic_store.get(entry.origin_topic_id).await?;
    let modules = state.storage.list_library_entry_modules(entry.id).await?;
    Ok(LibraryEntryResponse {
        id: entry.id,
        name: entry.name,
        description: entry.description,
        tags: entry.tags,
        origin_topic_id: origin
            .as_ref()
            .and_then(|topic| topic.client_topic_id())
            .or_else(|| origin.is_none().then_some(entry.origin_topic_id)),
        origin_topic_deleted: origin.is_none(),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        modules: modules
            .into_iter()
            .map(|module| LibraryEntryModuleView {
                id: module.id,
                kind: module.kind,
                position: module.position,
                source: module.source,
            })
            .collect(),
    })
}

pub async fn delete_library_entry(
    state: &AppState,
    entry_id: Uuid,
    author: &str,
) -> Result<(), GatewayError> {
    let entry = entry_or_invalid(state, entry_id).await?;
    if !state.storage.delete_library_entry(entry.id).await? {
        return Err(GatewayError::Storage(StorageError::NotFound {
            entity: "surface library entry".into(),
            id: entry_id.to_string(),
        }));
    }
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "surface_library_entry".into(),
            entity_id: entry.id.to_string(),
            event_kind: "surface_library_entry_deleted".into(),
            summary: format!("library entry removed: {}", entry.name),
            metadata: json!({
                "author": author,
                "name": entry.name,
                "description": entry.description,
                "tags": entry.tags,
            }),
        })
        .await?;
    let _ = state
        .broadcast
        .send(ServerEvent::LibraryUpdated { entry_id: entry.id });
    Ok(())
}

/// A topic's default surfaces, in designation order.
pub async fn topic_defaults(
    state: &AppState,
    topic_id: Uuid,
) -> Result<LibraryListResponse, GatewayError> {
    let topic = state
        .topic_store
        .get(topic_id)
        .await?
        .ok_or(GatewayError::TopicNotFound { id: topic_id })?;
    if topic.is_general {
        return Err(GatewayError::TopicNotFound { id: topic_id });
    }
    let scopes = state.topic_store.list_scopes().await?;
    let entries = state
        .storage
        .list_topic_default_entries(topic.id)
        .await?
        .into_iter()
        .map(|entry| entry_summary(entry, &scopes))
        .collect();
    Ok(LibraryListResponse { entries })
}

pub async fn add_topic_default(
    state: &AppState,
    topic_id: Uuid,
    request: TopicDefaultSurfaceRequest,
) -> Result<LibraryListResponse, GatewayError> {
    // The topic check first: a missing topic reports 404 before an unknown
    // entry reports a validation error.
    topic_defaults(state, topic_id).await?;
    let entry = entry_or_invalid(state, request.library_entry_id).await?;
    state
        .storage
        .add_topic_default_entry(topic_id, entry.id)
        .await?;
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "topic".into(),
            entity_id: topic_id.to_string(),
            event_kind: "default_surface_designated".into(),
            summary: format!(
                "library entry '{}' designated a default surface",
                entry.name
            ),
            metadata: json!({ "library_entry_id": entry.id, "name": entry.name }),
        })
        .await?;
    topic_defaults(state, topic_id).await
}

pub async fn remove_topic_default(
    state: &AppState,
    topic_id: Uuid,
    entry_id: Uuid,
) -> Result<(), GatewayError> {
    topic_defaults(state, topic_id).await?;
    if !state
        .storage
        .remove_topic_default_entry(topic_id, entry_id)
        .await?
    {
        return Err(GatewayError::ValidationError(format!(
            "library entry {entry_id} is not a default surface of topic {topic_id}"
        )));
    }
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "topic".into(),
            entity_id: topic_id.to_string(),
            event_kind: "default_surface_withdrawn".into(),
            summary: "a default surface was withdrawn".into(),
            metadata: json!({ "library_entry_id": entry_id }),
        })
        .await?;
    Ok(())
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
        session_id: surface.session_id,
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
        library_entry_id: surface.library_entry_id,
        seeded_from_name: surface.seeded_from_name,
        seeded_at: surface.seeded_at,
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

pub async fn list_surfaces_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SurfaceListResponse>, ApiError> {
    session_or_not_found(&state, session_id).await?;
    let surfaces = state
        .storage
        .list_surfaces_by_session(session_id)
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
    Path(session_id): Path<Uuid>,
    Json(request): Json<CreateSurfaceRequest>,
) -> Result<Json<SurfaceResponse>, ApiError> {
    Ok(Json(create_surface(&state, session_id, request).await?))
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

pub async fn save_surface_to_library_handler(
    State(state): State<AppState>,
    Path(surface_id): Path<Uuid>,
    Json(request): Json<SaveSurfaceToLibraryRequest>,
) -> Result<Json<LibraryEntryResponse>, ApiError> {
    Ok(Json(
        save_surface_to_library(&state, surface_id, request, "user").await?,
    ))
}

/// Query parameters for the library catalogue.
#[derive(Debug, Default, Deserialize)]
pub struct LibraryQuery {
    /// Narrow to entries carrying this tag.
    pub tag: Option<String>,
}

pub async fn list_library_handler(
    State(state): State<AppState>,
    Query(query): Query<LibraryQuery>,
) -> Result<Json<LibraryListResponse>, ApiError> {
    let tags: Vec<String> = query.tag.into_iter().collect();
    Ok(Json(library_catalogue(&state, &tags).await?))
}

pub async fn get_library_entry_handler(
    State(state): State<AppState>,
    Path(entry_id): Path<Uuid>,
) -> Result<Json<LibraryEntryResponse>, ApiError> {
    Ok(Json(library_entry_response(&state, entry_id).await?))
}

pub async fn delete_library_entry_handler(
    State(state): State<AppState>,
    Path(entry_id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    delete_library_entry(&state, entry_id, "user").await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list_topic_default_surfaces_handler(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
) -> Result<Json<LibraryListResponse>, ApiError> {
    Ok(Json(topic_defaults(&state, topic_id).await?))
}

pub async fn add_topic_default_surface_handler(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
    Json(request): Json<TopicDefaultSurfaceRequest>,
) -> Result<Json<LibraryListResponse>, ApiError> {
    Ok(Json(add_topic_default(&state, topic_id, request).await?))
}

pub async fn remove_topic_default_surface_handler(
    State(state): State<AppState>,
    Path((topic_id, entry_id)): Path<(Uuid, Uuid)>,
) -> Result<axum::http::StatusCode, ApiError> {
    remove_topic_default(&state, topic_id, entry_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_storage::{
        AuditStore, NewSession, NewTopic, SessionOrigin, SessionStore, SqliteStorage, Storage,
        SurfaceAnnotationStore, SurfaceStore, TopicStore,
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

    async fn session_in(storage: &SqliteStorage, topic_id: Uuid) -> Uuid {
        storage
            .create_session(NewSession {
                topic_id,
                agent_id: "gateway".into(),
                origin: SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap()
            .id
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
        let session = session_in(&storage, topic.id).await;
        let state = state(Arc::clone(&storage));
        let surface = create_surface(
            &state,
            session,
            CreateSurfaceRequest {
                title: "Planning".into(),
                library_entry_id: None,
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
        let other = create_surface(
            &state,
            session,
            CreateSurfaceRequest {
                title: "Elsewhere".into(),
                library_entry_id: None,
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

    async fn topic_named(storage: &SqliteStorage, name: &str) -> Uuid {
        TopicStore::create(
            storage,
            NewTopic {
                name: name.into(),
                slug: name.to_lowercase().into(),
            },
        )
        .await
        .unwrap()
        .id
    }

    fn save_request(name: &str, tags: &[&str]) -> SaveSurfaceToLibraryRequest {
        SaveSurfaceToLibraryRequest {
            name: name.into(),
            description: "A daily standup: use at the start of a working day.".into(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        }
    }

    async fn module_sources(state: &AppState, surface_id: Uuid) -> Vec<String> {
        surface_response(state, surface_id)
            .await
            .unwrap()
            .modules
            .into_iter()
            .map(|module| module.source)
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_saved_surface_keeps_its_content_within_its_topic_and_only_its_shape_beyond() {
        let (state, storage, surface_id, _, _) = seeded().await;
        let origin = storage.get_page(surface_id).await.unwrap().unwrap();
        let origin_session = storage
            .get_session(origin.session_id)
            .await
            .unwrap()
            .unwrap();
        let entry = save_surface_to_library(
            &state,
            surface_id,
            save_request("Standup", &["Standup", " ops ", "standup"]),
            "user",
        )
        .await
        .unwrap();
        assert_eq!(
            entry.tags,
            vec!["standup", "ops"],
            "tags are trimmed, lowered, deduplicated"
        );
        assert_eq!(entry.origin_topic_id, Some(origin_session.topic_id));
        assert_eq!(entry.modules.len(), 1);
        assert_eq!(entry.modules[0].source, "# Draft");
        // Saving opened nothing: the origin session still has its two surfaces.
        assert_eq!(
            storage
                .list_surfaces_by_session(origin_session.id)
                .await
                .unwrap()
                .len(),
            2
        );

        let sibling = session_in(&storage, origin_session.topic_id).await;
        let elsewhere = session_in(&storage, topic_named(&storage, "Elsewhere").await).await;
        let general = storage.general_scope().await.unwrap();
        let topicless = session_in(&storage, general.id).await;
        let add = |session: Uuid| {
            create_surface(
                &state,
                session,
                CreateSurfaceRequest {
                    title: "Monday".into(),
                    library_entry_id: Some(entry.id),
                },
            )
        };
        let in_topic = add(sibling).await.unwrap();
        let in_other = add(elsewhere).await.unwrap();
        let in_general = add(topicless).await.unwrap();

        assert_eq!(in_topic.library_entry_id, Some(entry.id));
        assert!(in_topic.seeded_at.is_some());
        assert_eq!(module_sources(&state, in_topic.id).await, vec!["# Draft"]);
        assert_eq!(module_sources(&state, in_other.id).await, vec![""]);
        assert_eq!(module_sources(&state, in_general.id).await, vec![""]);
        let created = storage
            .list_audit_events_by_session(elsewhere)
            .await
            .unwrap()
            .into_iter()
            .find(|event| event.event_kind == "surface_created")
            .expect("the add is recorded");
        assert_eq!(created.metadata["seeded_from"]["seeding"], "shape_only");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_catalogue_carries_descriptions_and_tags_and_never_content() {
        let (state, storage, surface_id, _, other_module_id) = seeded().await;
        let other_surface_id = storage
            .get_surface_module(other_module_id)
            .await
            .unwrap()
            .unwrap()
            .surface_id;
        save_surface_to_library(
            &state,
            surface_id,
            save_request("Standup", &["daily"]),
            "user",
        )
        .await
        .unwrap();
        save_surface_to_library(
            &state,
            other_surface_id,
            SaveSurfaceToLibraryRequest {
                name: "Retro".into(),
                description: "A retrospective board — use at the end of a sprint.".into(),
                tags: vec!["sprint".into()],
            },
            "assistant",
        )
        .await
        .unwrap();

        let whole = library_catalogue(&state, &[]).await.unwrap();
        assert_eq!(whole.entries.len(), 2);
        let serialised = serde_json::to_string(&whole).unwrap();
        assert!(serialised.contains("use at the start of a working day"));
        assert!(!serialised.contains("# Draft") && !serialised.contains("# Other"));

        let narrowed = library_catalogue(&state, &["Sprint".into()]).await.unwrap();
        assert_eq!(narrowed.entries.len(), 1);
        assert_eq!(narrowed.entries[0].name, "Retro");
        assert!(
            library_catalogue(&state, &["nothing".into()])
                .await
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_topic_default_opens_in_its_new_sessions_and_nowhere_else() {
        let (state, storage, surface_id, _, _) = seeded().await;
        let origin = storage.get_page(surface_id).await.unwrap().unwrap();
        let topic_id = storage
            .get_session(origin.session_id)
            .await
            .unwrap()
            .unwrap()
            .topic_id;
        let entry =
            save_surface_to_library(&state, surface_id, save_request("Standup", &[]), "user")
                .await
                .unwrap();
        let defaults = add_topic_default(
            &state,
            topic_id,
            TopicDefaultSurfaceRequest {
                library_entry_id: entry.id,
            },
        )
        .await
        .unwrap();
        assert_eq!(defaults.entries.len(), 1);

        let service = crate::service::ConversationService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
        );
        let in_topic = service
            .create_session_in_topic_for_agent(topic_id, "gateway", SessionOrigin::Direct, None)
            .await
            .unwrap();
        let other_topic = topic_named(&storage, "Other").await;
        let elsewhere = service
            .create_session_in_topic_for_agent(other_topic, "gateway", SessionOrigin::Direct, None)
            .await
            .unwrap();

        let opened = storage.list_surfaces_by_session(in_topic.id).await.unwrap();
        assert_eq!(
            opened.len(),
            1,
            "the new session opens with the topic's default"
        );
        assert_eq!(opened[0].title, "Standup");
        assert_eq!(opened[0].library_entry_id, Some(entry.id));
        assert_eq!(module_sources(&state, opened[0].id).await, vec!["# Draft"]);
        assert!(
            storage
                .list_surfaces_by_session(elsewhere.id)
                .await
                .unwrap()
                .is_empty(),
            "another topic's session received nothing"
        );

        remove_topic_default(&state, topic_id, entry.id)
            .await
            .unwrap();
        assert!(
            topic_defaults(&state, topic_id)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
        let later = service
            .create_session_in_topic_for_agent(topic_id, "gateway", SessionOrigin::Direct, None)
            .await
            .unwrap();
        assert!(
            storage
                .list_surfaces_by_session(later.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_general_scope_designates_no_defaults() {
        let (state, storage, _, _, _) = seeded().await;
        let general = storage.general_scope().await.unwrap();
        let error = topic_defaults(&state, general.id)
            .await
            .expect_err("the general scope is not a topic");
        assert!(matches!(error, GatewayError::TopicNotFound { .. }));
    }
}
