//! Context-assembly reports exposed to client inspectors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct AssemblyReport {
    pub assembly_name: String,
    pub elements: Vec<ElementReport>,
    pub total_tokens: usize,
    pub budget: Option<usize>,
    pub budget_remaining: Option<isize>,
    pub compaction_applied: Vec<CompactionRecord>,
    pub warnings: Vec<String>,
    pub errors: Vec<AssemblyError>,
    /// Baseline elements the assembly script renounced by name, with the
    /// reason it gave. An element absent from the assembled context and
    /// absent from this list is a floor breach, not a choice.
    #[serde(default)]
    pub declared_omissions: Vec<DeclaredOmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Record<string, unknown>")]
    pub effective_tool_permissions: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Record<string, unknown>")]
    pub tool_scope: Option<serde_json::Value>,
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
            declared_omissions: Vec::new(),
            effective_tool_permissions: None,
            tool_scope: None,
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

/// One baseline element the assembly deliberately left out of the context.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct DeclaredOmission {
    /// The omitted element's name, as it appears in the elements projection.
    pub element: String,
    /// The reason the assembly gave for leaving it out.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
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
pub struct NamedAssemblyReport {
    pub assembly_name: String,
    pub resolved_level: String,
    pub child_elements: Vec<ElementReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub focus: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct AssemblyError {
    pub assembly_name: String,
    pub error_kind: String,
    pub message: String,
    pub fallback_used: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct TurnContextResponse {
    pub turn_id: Uuid,
    pub report: AssemblyReport,
}

#[cfg(test)]
mod tests {
    use super::{AssemblyReport, DeclaredOmission};

    #[test]
    fn a_declared_omission_crosses_the_wire_as_its_element_and_reason() {
        let mut report = AssemblyReport::new("focus");
        report.declared_omissions.push(DeclaredOmission {
            element: "working_norms".to_string(),
            reason: "the operator instructions carry their own".to_string(),
        });

        let serialised = serde_json::to_value(&report).expect("serialise report");
        assert_eq!(
            serialised["declared_omissions"][0]["element"],
            "working_norms"
        );
        assert_eq!(
            serialised["declared_omissions"][0]["reason"],
            "the operator instructions carry their own"
        );

        let parsed: AssemblyReport =
            serde_json::from_value(serialised).expect("a serialised report round-trips");
        assert_eq!(parsed.declared_omissions[0].element, "working_norms");
    }

    #[test]
    fn a_report_without_declared_omissions_parses_with_none_recorded() {
        let parsed: AssemblyReport = serde_json::from_value(serde_json::json!({
            "assembly_name": "default",
            "elements": [],
            "total_tokens": 0,
            "compaction_applied": [],
            "warnings": [],
            "errors": [],
            "effective_tool_permissions": [],
            "provider_visible": null,
            "duration": {"secs": 0, "nanos": 0}
        }))
        .expect("a report omitting the field parses");

        assert!(parsed.declared_omissions.is_empty());
    }
}
