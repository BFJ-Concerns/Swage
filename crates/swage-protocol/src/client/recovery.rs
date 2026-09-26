//! Recovery export listing contract.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct RecoveryExportSummary {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    #[ts(type = "number")]
    pub size_bytes: usize,
    #[ts(type = "number")]
    pub topics: usize,
    #[ts(type = "number")]
    pub workspaces: usize,
    #[ts(type = "number")]
    pub sessions: usize,
    #[ts(type = "number")]
    pub secrets: usize,
    #[ts(type = "number")]
    pub channel_links: usize,
    #[ts(type = "number")]
    pub devices: usize,
    #[ts(type = "number")]
    pub schedules: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct RecoveryExportListResponse {
    pub exports: Vec<RecoveryExportSummary>,
}
