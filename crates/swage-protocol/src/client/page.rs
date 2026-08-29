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
    /// Seed the new page from a template of the same space. Absent creates a
    /// blank page. The seeded modules are the page's own from birth; the
    /// template is recorded as the fork's origin, never projected into it.
    #[serde(default)]
    #[ts(optional)]
    pub template_id: Option<Uuid>,
}

/// Create a reusable page shape for a space, with its module rows in the
/// order they will seed.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreatePageTemplateRequest {
    pub name: String,
    #[serde(default)]
    pub modules: Vec<PageTemplateModuleInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageTemplateModuleInput {
    /// Module kind for the seeded instance ("document" first).
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageTemplateResponse {
    pub id: Uuid,
    pub space_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub modules: Vec<PageTemplateModuleView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageTemplateModuleView {
    pub id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageTemplateSummary {
    pub id: Uuid,
    pub name: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageTemplateListResponse {
    pub templates: Vec<PageTemplateSummary>,
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

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageModuleView {
    pub id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PageResponse {
    pub id: Uuid,
    pub space_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub modules: Vec<PageModuleView>,
    /// The template this page was forked from, if any — the recorded origin
    /// of its seeded shape. Absent on a page created blank.
    #[serde(default)]
    #[ts(optional)]
    pub forked_from_template_id: Option<Uuid>,
    #[serde(default)]
    #[ts(optional)]
    pub forked_at: Option<DateTime<Utc>>,
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
