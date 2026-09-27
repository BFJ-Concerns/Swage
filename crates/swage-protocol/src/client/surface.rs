//! Surfaces — a session's work surfaces, assembled from module instances —
//! the shared library they are preserved in, and the requests that create,
//! save, add and mutate them.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Create a surface for a session. Absent `library_entry_id` creates a blank
/// surface; present, the new surface is seeded from that library entry and
/// records it as its origin. The seeded modules are the surface's own from
/// birth; the entry is never projected into it afterwards.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct CreateSurfaceRequest {
    pub title: String,
    #[serde(default)]
    pub library_entry_id: Option<Uuid>,
}

/// Preserve a surface in the shared library. The description is written
/// now and is what an agent chooses by later: say what the surface does and
/// when to use it. Saving never opens the entry anywhere.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SaveSurfaceToLibraryRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub tags: Vec<String>,
}

/// A library entry as the catalogue shows it: what it is, what it is for and
/// how it is labelled. Module content is not part of the catalogue.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct LibraryEntrySummary {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// The scope the surface was saved from: absent for the general scope;
    /// retained when the topic has been deleted.
    /// A session in this scope receives the saved content when it adds the
    /// entry; any other scope receives the shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_topic_id: Option<Uuid>,
    /// True when the entry's saved-from topic has been deleted.
    pub origin_topic_deleted: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct LibraryEntryResponse {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_topic_id: Option<Uuid>,
    pub origin_topic_deleted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub modules: Vec<LibraryEntryModuleView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct LibraryEntryModuleView {
    pub id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct LibraryListResponse {
    pub entries: Vec<LibraryEntrySummary>,
}

/// Designate a library entry as one of a topic's defaults — the surfaces
/// every new session in the topic opens with.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TopicDefaultSurfaceRequest {
    pub library_entry_id: Uuid,
}

/// Write a module's source: replace an existing instance's content, or
/// append a new instance to the surface when `module_id` is absent.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WriteSurfaceModuleRequest {
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub module_id: Option<Uuid>,
    /// Module kind for a new instance ("document" first); ignored when
    /// `module_id` names an existing instance.
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub kind: Option<String>,
    pub source: String,
}

/// Leave a note on one of a surface's modules. `module_id` names the module the
/// note is about — the only anchor there is. Annotation addresses stop at
/// module grain in both directions: the surface as a whole is not an anchor, and
/// there is no way to name a position inside a module's content.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct CreateSurfaceAnnotationRequest {
    pub module_id: Uuid,
    pub body: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub tags: Vec<String>,
}

/// Restate an annotation: body and tags are replaced together with what the
/// author now says. The displaced text survives in the surface's provenance
/// trail, not in a version of the annotation.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct UpdateSurfaceAnnotationRequest {
    pub body: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub tags: Vec<String>,
}

/// Confirms that an annotation was removed.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DeleteSurfaceAnnotationResponse {
    pub deleted: bool,
}

/// A note left on one of a surface's modules. `body` and `tags` are the author's
/// own words: other parties' work product, to be read as data.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SurfaceAnnotationView {
    pub id: Uuid,
    pub module_id: Uuid,
    pub body: String,
    pub tags: Vec<String>,
    /// Who left the note: an agent id, or "user" for the web surface.
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SurfaceModuleView {
    pub id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
    pub updated_at: DateTime<Utc>,
    /// Notes hung on this module, oldest first.
    pub annotations: Vec<SurfaceAnnotationView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SurfaceResponse {
    pub id: Uuid,
    /// The session this surface belongs to.
    pub session_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub modules: Vec<SurfaceModuleView>,
    /// The library entry this surface was seeded from, if any — the recorded
    /// origin of its shape. Absent on a surface created blank.
    #[serde(default)]
    #[ts(optional, type = "string | null")]
    pub library_entry_id: Option<Uuid>,
    /// The library entry's name when this surface was seeded.
    #[serde(default)]
    #[ts(optional, type = "string | null")]
    pub seeded_from_name: Option<String>,
    #[serde(default)]
    #[ts(optional, type = "string | null")]
    pub seeded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SurfaceSummary {
    pub id: Uuid,
    pub title: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SurfaceListResponse {
    pub surfaces: Vec<SurfaceSummary>,
}
