//! Effective skill catalogue exposed to Swage clients.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct SkillItem {
    /// Stable synthetic identifier for a shipped skill in this space.
    pub id: Option<Uuid>,
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    /// Resolved hierarchy level: `agent`, `space`, or `global`.
    pub level: String,
    pub tools: Vec<String>,
    pub always_active: bool,
    /// True when the definition is compiled into Swage rather than authored.
    pub shipped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct SkillListResponse {
    pub skills: Vec<SkillItem>,
}
