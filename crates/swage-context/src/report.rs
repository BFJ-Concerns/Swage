//! Assembly report types — structured output describing what the assembly produced.
//!
//! The report is serialised into the `ContextAssembled` WebSocket event and
//! consumed by the gateway's context inspector panel.

use serde::{Deserialize, Serialize};
use swage_core::ChatMessage;
pub use swage_protocol::client::context::{
    AssemblyError, AssemblyReport, CompactionRecord, ElementReport, NamedAssemblyReport,
};

// ─────────────────────────────────────────────────────────────────────────────
// Output wrapper
// ─────────────────────────────────────────────────────────────────────────────

/// The complete output of a context assembly run: a non-empty message list
/// paired with a structured report describing how the context was built.
#[derive(Debug, Clone, Serialize)]
pub struct AssemblyOutput {
    /// The assembled context messages. Always non-empty — construction is
    /// guarded by [`AssemblyOutput::new`].
    pub messages: Vec<ChatMessage>,

    /// A full account of every element processed, any compaction applied,
    /// and any errors or warnings encountered.
    pub report: AssemblyReport,
}

#[derive(Debug, Deserialize)]
struct AssemblyOutputDef {
    messages: Vec<ChatMessage>,
    report: AssemblyReport,
}

impl AssemblyOutput {
    /// Construct an `AssemblyOutput`, returning `Err` when `messages` is empty.
    ///
    /// The invariant is that context assembly must always produce at least one
    /// message — callers that receive an empty list have a logic error.
    pub fn new(messages: Vec<ChatMessage>, report: AssemblyReport) -> Result<Self, String> {
        if messages.is_empty() {
            return Err("AssemblyOutput requires at least one message".to_string());
        }
        Ok(Self { messages, report })
    }

    /// Convenience constructor that panics on an empty message list.
    ///
    /// Use only in contexts where emptiness is a programming error rather than
    /// a runtime condition (e.g. the assembly engine itself, after validation).
    pub fn new_unchecked(messages: Vec<ChatMessage>, report: AssemblyReport) -> Self {
        assert!(
            !messages.is_empty(),
            "AssemblyOutput requires at least one message"
        );
        Self { messages, report }
    }
}

impl TryFrom<AssemblyOutputDef> for AssemblyOutput {
    type Error = String;

    fn try_from(value: AssemblyOutputDef) -> Result<Self, Self::Error> {
        Self::new(value.messages, value.report)
    }
}

impl<'de> Deserialize<'de> for AssemblyOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = AssemblyOutputDef::deserialize(deserializer)?;
        AssemblyOutput::try_from(raw).map_err(serde::de::Error::custom)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::{ChatMessage, Role};

    fn make_message() -> ChatMessage {
        ChatMessage::new(Role::User, "hello")
    }

    // ── AssemblyOutput construction ──────────────────────────────────────────

    #[test]
    fn assembly_output_new_rejects_empty_messages() {
        let report = AssemblyReport::default();
        let result = AssemblyOutput::new(vec![], report);
        assert!(result.is_err(), "empty messages should be rejected");
        assert!(result.unwrap_err().contains("at least one message"));
    }

    #[test]
    fn assembly_output_new_accepts_non_empty_messages() {
        let report = AssemblyReport::default();
        let output = AssemblyOutput::new(vec![make_message()], report).unwrap();
        assert_eq!(output.messages.len(), 1);
    }

    #[test]
    fn assembly_output_deserialisation_rejects_empty_messages() {
        let error = serde_json::from_str::<AssemblyOutput>(
            r#"{"messages":[],"report":{"assembly_name":"default","elements":[],"total_tokens":0,"budget":null,"budget_remaining":null,"compaction_applied":[],"warnings":[],"errors":[],"duration":{"secs":0,"nanos":0}}}"#,
        )
        .expect_err("empty messages should be rejected");
        assert!(error.to_string().contains("at least one message"));
    }

    #[test]
    #[should_panic(expected = "at least one message")]
    fn assembly_output_new_unchecked_panics_on_empty() {
        AssemblyOutput::new_unchecked(vec![], AssemblyReport::default());
    }

    // ── AssemblyReport defaults and construction ─────────────────────────────

    #[test]
    fn assembly_report_default_has_expected_shape() {
        let report = AssemblyReport::default();
        assert_eq!(report.assembly_name, "default");
        assert!(report.elements.is_empty());
        assert_eq!(report.total_tokens, 0);
        assert!(report.budget.is_none());
        assert!(report.budget_remaining.is_none());
        assert!(report.compaction_applied.is_empty());
        assert!(report.warnings.is_empty());
        assert!(report.errors.is_empty());
        assert_eq!(report.duration, std::time::Duration::ZERO);
    }

    #[test]
    fn assembly_report_new_uses_provided_name() {
        let report = AssemblyReport::new("custom");
        assert_eq!(report.assembly_name, "custom");
    }

    // ── Serialisation round-trip ─────────────────────────────────────────────

    #[test]
    fn assembly_output_serialises_and_deserialises() {
        let report = AssemblyReport {
            assembly_name: "default".to_string(),
            elements: vec![ElementReport {
                name: "history".to_string(),
                source: "thread-123".to_string(),
                token_count: 42,
                preview: Some("Hello…".to_string()),
                priority: 0,
                named_assembly: None,
            }],
            total_tokens: 42,
            budget: Some(1000),
            budget_remaining: Some(958),
            compaction_applied: vec![],
            warnings: vec![],
            errors: vec![],
            effective_tool_permissions: None,
            provider_visible: None,
            duration: std::time::Duration::from_millis(12),
        };

        let output = AssemblyOutput::new(vec![make_message()], report).unwrap();
        let json = serde_json::to_string(&output).expect("serialisation should succeed");
        let decoded: AssemblyOutput =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(decoded.messages.len(), 1);
        assert_eq!(decoded.report.assembly_name, "default");
        assert_eq!(decoded.report.total_tokens, 42);
        assert_eq!(decoded.report.budget, Some(1000));
        assert_eq!(decoded.report.budget_remaining, Some(958));
        assert_eq!(decoded.report.elements.len(), 1);
        assert_eq!(decoded.report.elements[0].name, "history");
        assert_eq!(decoded.report.elements[0].token_count, 42);
    }

    #[test]
    fn compaction_record_serialises_and_deserialises() {
        let record = CompactionRecord {
            element: "history".to_string(),
            requested_strategy: "truncation".to_string(),
            actual_strategy: "truncation".to_string(),
            messages_removed: 5,
            tokens_before: 1200,
            tokens_after: 800,
            source_message_range: None,
            budget: None,
            failure_reason: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        let decoded: CompactionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.element, "history");
        assert_eq!(decoded.requested_strategy, "truncation");
        assert_eq!(decoded.actual_strategy, "truncation");
        assert_eq!(decoded.messages_removed, 5);
        assert_eq!(decoded.tokens_before, 1200);
        assert_eq!(decoded.tokens_after, 800);
    }

    #[test]
    fn assembly_error_serialises_and_deserialises() {
        let err = AssemblyError {
            assembly_name: "custom".to_string(),
            error_kind: "timeout".to_string(),
            message: "Assembly timed out after 5s".to_string(),
            fallback_used: "default".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let decoded: AssemblyError = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.assembly_name, "custom");
        assert_eq!(decoded.error_kind, "timeout");
        assert_eq!(decoded.fallback_used, "default");
    }

    // ── CompactionRecord extended fields ─────────────────────────────────────

    #[test]
    fn compaction_record_requested_and_actual_strategy_round_trip() {
        // Simulates a case where structured_extraction was requested but LLM
        // was unavailable, so the actual strategy fell back to truncation.
        let record = CompactionRecord {
            element: "history".to_string(),
            requested_strategy: "structured_extraction".to_string(),
            actual_strategy: "truncation".to_string(),
            messages_removed: 3,
            tokens_before: 1000,
            tokens_after: 600,
            source_message_range: Some((2, 8)),
            budget: Some(700),
            failure_reason: Some("LLM timeout".to_string()),
        };
        let json = serde_json::to_string(&record).unwrap();
        let decoded: CompactionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.requested_strategy, "structured_extraction");
        assert_eq!(decoded.actual_strategy, "truncation");
        assert_eq!(decoded.source_message_range, Some((2, 8)));
        assert_eq!(decoded.budget, Some(700));
        assert_eq!(decoded.failure_reason.as_deref(), Some("LLM timeout"));
    }

    #[test]
    fn compaction_record_none_fields_round_trip() {
        // No fallback: requested and actual strategies are the same; optional
        // fields not populated.
        let record = CompactionRecord {
            element: "history".to_string(),
            requested_strategy: "truncation".to_string(),
            actual_strategy: "truncation".to_string(),
            messages_removed: 0,
            tokens_before: 500,
            tokens_after: 500,
            source_message_range: None,
            budget: None,
            failure_reason: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        let decoded: CompactionRecord = serde_json::from_str(&json).unwrap();
        assert!(decoded.source_message_range.is_none());
        assert!(decoded.budget.is_none());
        assert!(decoded.failure_reason.is_none());
    }

    // ── NamedAssemblyReport in ElementReport ─────────────────────────────────

    #[test]
    fn element_report_with_named_assembly_round_trips() {
        let report = ElementReport {
            name: "custom-assembly".to_string(),
            source: "named assembly".to_string(),
            token_count: 150,
            preview: None,
            priority: 50,
            named_assembly: Some(NamedAssemblyReport {
                assembly_name: "summarise".to_string(),
                resolved_level: "space".to_string(),
                child_elements: vec![
                    ElementReport {
                        name: "history".to_string(),
                        source: "thread-abc".to_string(),
                        token_count: 100,
                        preview: None,
                        priority: 0,
                        named_assembly: None,
                    },
                    ElementReport {
                        name: "prompts".to_string(),
                        source: "prompt rules".to_string(),
                        token_count: 50,
                        preview: None,
                        priority: 100,
                        named_assembly: None,
                    },
                ],
            }),
        };
        let json = serde_json::to_string(&report).unwrap();
        let decoded: ElementReport = serde_json::from_str(&json).unwrap();

        let named = decoded
            .named_assembly
            .expect("named_assembly should be Some");
        assert_eq!(named.assembly_name, "summarise");
        assert_eq!(named.resolved_level, "space");
        assert_eq!(named.child_elements.len(), 2);
        assert_eq!(named.child_elements[0].name, "history");
        assert_eq!(named.child_elements[1].name, "prompts");
    }

    #[test]
    fn element_report_without_named_assembly_round_trips() {
        let report = ElementReport {
            name: "history".to_string(),
            source: "thread-123".to_string(),
            token_count: 42,
            preview: Some("Hello…".to_string()),
            priority: 0,
            named_assembly: None,
        };
        let json = serde_json::to_string(&report).unwrap();
        let decoded: ElementReport = serde_json::from_str(&json).unwrap();
        assert!(decoded.named_assembly.is_none());
        assert_eq!(decoded.name, "history");
    }

    #[test]
    fn named_assembly_report_child_elements_are_independent() {
        // Deeply nested ElementReports (children of a named assembly) can
        // themselves carry no named_assembly — None serialises cleanly.
        let nested = NamedAssemblyReport {
            assembly_name: "agent-context".to_string(),
            resolved_level: "agent".to_string(),
            child_elements: vec![ElementReport {
                name: "prompts".to_string(),
                source: "agent prompts".to_string(),
                token_count: 120,
                preview: Some("Be concise.".to_string()),
                priority: 100,
                named_assembly: None,
            }],
        };
        let json = serde_json::to_string(&nested).unwrap();
        let decoded: NamedAssemblyReport = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.assembly_name, "agent-context");
        assert_eq!(decoded.resolved_level, "agent");
        assert_eq!(decoded.child_elements[0].name, "prompts");
    }

    // ── Budget remaining can be negative ────────────────────────────────────

    #[test]
    fn budget_remaining_can_be_negative_when_over_budget() {
        let report = AssemblyReport {
            budget: Some(100),
            budget_remaining: Some(-50),
            ..AssemblyReport::default()
        };
        assert_eq!(report.budget_remaining, Some(-50));
        // Serialise and back to verify isize round-trips cleanly
        let json = serde_json::to_string(&report).unwrap();
        let decoded: AssemblyReport = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.budget_remaining, Some(-50));
    }
}
