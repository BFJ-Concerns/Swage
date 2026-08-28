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
