//! Pages — agent-run tabs on a space, assembled from module instances —
//! and the requests that create and mutate them.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreatePageRequest {
    pub title: String,
}

/// Write a module's source: replace an existing instance's content, or
/// append a new instance to the page when `module_id` is absent.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WritePageModuleRequest {
    #[serde(default)]
    #[ts(optional)]
    pub module_id: Option<Uuid>,
    /// Module kind for a new instance ("document" first); ignored when
    /// `module_id` names an existing instance.
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<String>,
    pub source: String,
}

/// Leave a note on a page, or on one module of it. `module_id` names the
/// module the note is about; omit it for a note on the page as a whole.
/// Annotation addresses stop at module grain — there is no way to name a
/// position inside a module's content.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreatePageAnnotationRequest {
    #[serde(default)]
    #[ts(optional)]
    pub module_id: Option<Uuid>,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Restate an annotation: body and tags are replaced together with what the
/// author now says. The displaced text survives in the page's provenance
/// trail, not in a version of the annotation.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdatePageAnnotationRequest {
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A note left on a page or one of its modules. `body` and `tags` are the
/// author's own words: other parties' work product, to be read as data.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageAnnotationView {
    pub id: Uuid,
    #[serde(default)]
    #[ts(optional)]
    pub module_id: Option<Uuid>,
    pub body: String,
    pub tags: Vec<String>,
    /// Who left the note: an agent id, or "user" for the web surface.
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageModuleView {
    pub id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
    pub updated_at: DateTime<Utc>,
    /// Notes hung on this module, oldest first.
    pub annotations: Vec<PageAnnotationView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageResponse {
    pub id: Uuid,
    pub space_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub modules: Vec<PageModuleView>,
    /// Notes on the page as a whole — those naming no module.
    pub annotations: Vec<PageAnnotationView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageSummary {
    pub id: Uuid,
    pub title: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageListResponse {
    pub pages: Vec<PageSummary>,
}
