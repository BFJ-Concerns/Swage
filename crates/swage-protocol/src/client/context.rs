//! Context-assembly reports exposed to client inspectors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct AssemblyReport {
    pub assembly_name: String,
    pub elements: Vec<ElementReport>,
    pub total_tokens: usize,
    pub budget: Option<usize>,
    pub budget_remaining: Option<isize>,
    pub compaction_applied: Vec<CompactionRecord>,
    pub warnings: Vec<String>,
    pub errors: Vec<AssemblyError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Record<string, unknown>")]
    pub effective_tool_permissions: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Record<string, unknown>")]
    pub provider_visible: Option<serde_json::Value>,
    #[ts(type = "{ secs: number; nanos: number }")]
    pub duration: std::time::Duration,
}

impl AssemblyReport {
    pub fn new(assembly_name: impl Into<String>) -> Self {
        Self {
            assembly_name: assembly_name.into(),
            elements: Vec::new(),
            total_tokens: 0,
            budget: None,
            budget_remaining: None,
            compaction_applied: Vec::new(),
            warnings: Vec::new(),
            errors: Vec::new(),
            effective_tool_permissions: None,
            provider_visible: None,
            duration: std::time::Duration::ZERO,
        }
    }
}

impl Default for AssemblyReport {
    fn default() -> Self {
        Self::new("default")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct ElementReport {
    pub name: String,
    pub source: String,
    pub token_count: usize,
    pub preview: Option<String>,
    pub priority: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_assembly: Option<NamedAssemblyReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct NamedAssemblyReport {
    pub assembly_name: String,
    pub resolved_level: String,
    pub child_elements: Vec<ElementReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct CompactionRecord {
    pub element: String,
    pub requested_strategy: String,
    pub actual_strategy: String,
    pub messages_removed: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub source_message_range: Option<(usize, usize)>,
    pub budget: Option<usize>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct AssemblyError {
    pub assembly_name: String,
    pub error_kind: String,
    pub message: String,
    pub fallback_used: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct TurnContextResponse {
    pub turn_id: Uuid,
    pub report: AssemblyReport,
}
