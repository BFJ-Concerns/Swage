//! Context assembly engine — default assembly implementation.
//!
//! The assembly engine accepts typed elements, sorts them by priority, applies
//! any configured token budget, runs truncation compaction on history when
//! needed, and returns a structured `AssemblyOutput` with a full report.
//!
//! The default assembly implementation lives here too.

use std::cmp::Reverse;
use std::time::Instant;

use swage_config::model::ContextConfig;
use swage_core::ChatMessage;

use crate::briefing::substitute_variables;
use crate::compaction::TruncationCompactor;
use crate::element::ContextElement;
use crate::report::{AssemblyOutput, AssemblyReport, CompactionRecord, ElementReport};
use crate::tokeniser::Tokeniser;

/// Produce an assembled context from the provided elements.
///
/// This is the built-in "default" assembly, designed to preserve the legacy
/// prompt shape: given the same prompt rules and conversation history, it
/// produces a message sequence that generates an identical LLM request to the
/// current gateway (system message from instructions, then history in
/// chronological order).
///
/// ## Element ordering
///
/// Elements are sorted by `priority()` descending before their content is
/// placed into the output. Current conventions:
///
/// | Priority | Element |
/// |----------|---------|
/// | 100      | `"prompts"` — system instructions |
/// | 50       | `"memory"` — **skipped by the default assembly** |
/// | 0        | `"history"` — conversation messages |
///
/// ## Budget enforcement
///
/// When `config.budget` is `Some(n)` and the assembled messages exceed `n`
/// tokens, [`TruncationCompactor`] is applied to the history element only.
/// Non-history content (system messages from prompts) is always included in
/// full — its token cost is subtracted from the budget before truncation.
///
/// ## Empty inputs
///
/// If no elements produce messages, the resulting `AssemblyOutput` will be
/// invalid. Callers must ensure at least one element supplies content.
pub fn default_assembly(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
) -> AssemblyOutput {
    default_assembly_inner(elements, config, tokeniser, None)
}

/// Produce the built-in assembly while rendering session-value slots once.
pub fn default_assembly_with_variables(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: &std::collections::BTreeMap<String, String>,
) -> AssemblyOutput {
    default_assembly_inner(elements, config, tokeniser, Some(variables))
}

fn default_assembly_inner(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: Option<&std::collections::BTreeMap<String, String>>,
) -> AssemblyOutput {
    let start = Instant::now();

    // Sort elements by priority (highest first) for deterministic ordering.
    // Sorting on a collected reference slice avoids mutating the input.
    let mut sorted: Vec<&dyn ContextElement> = elements.iter().map(|e| e.as_ref()).collect();
    sorted.sort_by_key(|element| Reverse(element.priority()));

    // The default assembly keeps memory tool-accessible rather than prepending
    // it to the prompt. Custom assemblies can opt in by removing this filter.
    let active: Vec<&dyn ContextElement> = sorted
        .into_iter()
        .filter(|e| e.name() != "memory")
        .collect();

    // Accumulate content and per-element reports in priority order.
    let mut assembled: Vec<ChatMessage> = Vec::new();
    let mut element_reports: Vec<ElementReport> = Vec::new();

    // Track the slice position of history messages so we can target compaction.
    let mut history_range: Option<std::ops::Range<usize>> = None;

    for element in &active {
        let renders_session_values = variables.is_some() && element.renders_session_values();
        let content = match (variables, renders_session_values) {
            (Some(variables), true) => element
                .content()
                .into_iter()
                .map(|message| {
                    ChatMessage::new(
                        message.role(),
                        substitute_variables(&message.text_content(), variables),
                    )
                })
                .collect::<Vec<_>>(),
            _ => element.content(),
        };
        let token_count = if renders_session_values {
            tokeniser.count_messages(&content)
        } else {
            element.token_count(tokeniser)
        };
        let source = element.source();

        if element.name() == "history" {
            let range_start = assembled.len();
            history_range = Some(range_start..range_start + content.len());
        }

        // Skills have a compact identity preview; every other rendered
        // element exposes a bounded excerpt of the actual context it added.
        // The report is an inspector surface, so a permanently empty preview
        // conceals rather than summarises the assembled context.
        let preview = if element.name() == "skills" {
            extract_skill_names_preview(&source).or_else(|| preview_messages(&content))
        } else {
            preview_messages(&content)
        };

        element_reports.push(ElementReport {
            name: element.name().to_string(),
            source,
            token_count,
            preview,
            priority: element.priority(),
            named_assembly: None,
        });

        assembled.extend(content);
    }

    // ── Budget enforcement ────────────────────────────────────────────────────

    let mut compaction_applied: Vec<CompactionRecord> = Vec::new();

    if let Some(budget) = config.budget {
        let total_tokens = tokeniser.count_messages(&assembled);

        if total_tokens > budget
            && let Some(range) = history_range
        {
            // Compute the token cost of non-history messages (prompts, etc.).
            // History gets whatever budget remains after the other elements.
            let non_history_tokens: usize = {
                let owned: Vec<ChatMessage> = assembled
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !range.contains(i))
                    .map(|(_, m)| m)
                    .cloned()
                    .collect();
                tokeniser.count_messages(&owned)
            };

            let history_budget = budget.saturating_sub(non_history_tokens);

            // Extract history messages, compact, then splice back in.
            let history_msgs: Vec<ChatMessage> = assembled[range.clone()].to_vec();
            let compactor = TruncationCompactor::new();
            let (compacted, record) = compactor.compact(history_msgs, history_budget, tokeniser);

            // Update the element report token count to reflect post-compaction cost.
            if let Some(report) = element_reports.iter_mut().find(|r| r.name == "history") {
                report.token_count = record.tokens_after;
            }

            // Splice the compacted messages back into the assembled list.
            let mut rebuilt: Vec<ChatMessage> = Vec::with_capacity(assembled.len());
            rebuilt.extend_from_slice(&assembled[..range.start]);
            rebuilt.extend(compacted);
            rebuilt.extend_from_slice(&assembled[range.end..]);
            assembled = rebuilt;

            compaction_applied.push(record);
        }
    }

    // ── Final report ──────────────────────────────────────────────────────────

    let total_tokens = tokeniser.count_messages(&assembled);
    let budget_remaining = config.budget.map(|b| b as isize - total_tokens as isize);
    let duration = start.elapsed();

    let report = AssemblyReport {
        assembly_name: "default".to_string(),
        elements: element_reports,
        total_tokens,
        budget: config.budget,
        budget_remaining,
        compaction_applied,
        warnings: Vec::new(),
        errors: Vec::new(),
        effective_tool_permissions: None,
        provider_visible: None,
        duration,
    };

    // If assembled is empty the caller provided no viable elements — that is a
    // programming error, not a runtime condition, so we panic here.
    AssemblyOutput::new_unchecked(assembled, report)
}

pub(crate) fn preview_messages(messages: &[ChatMessage]) -> Option<String> {
    const PREVIEW_CHARS: usize = 240;

    let content = messages
        .iter()
        .map(|message| message.text_content())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if content.is_empty() {
        return None;
    }

    let mut preview = content.chars().take(PREVIEW_CHARS).collect::<String>();
    if content.chars().nth(PREVIEW_CHARS).is_some() {
        preview.push('…');
    }
    Some(preview)
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Extract a comma-separated list of skill names from a `SkillsElement` source string.
///
/// Source format produced by `SkillsElement::source()`:
/// - `"skills: name1 (level, ~N tokens), name2 (level, ~N tokens)"` — active skills
/// - `"skills (none active)"` — no active skills
///
/// Returns `Some("name1, name2")` when skills are present, `None` otherwise.
/// The parenthesised level/token annotations are stripped; only the bare names remain.
fn extract_skill_names_preview(source: &str) -> Option<String> {
    // Only process the "skills: ..." form; "skills (none active)" → None.
    let after_prefix = source.strip_prefix("skills: ")?;

    // Each entry is "name (level, ~N tokens)". We split on "), " to separate
    // entries (preserving the inner comma in the parentheses), then take the
    // portion before " (" to isolate the name.
    let names: Vec<&str> = after_prefix
        .split("), ")
        .filter_map(|entry| entry.split(" (").next().filter(|n| !n.is_empty()))
        .collect();

    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Default assembly entry point
// ─────────────────────────────────────────────────────────────────────────────

pub fn assemble(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
) -> AssemblyOutput {
    default_assembly(elements, config, tokeniser)
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;

    // ── Test fixtures ─────────────────────────────────────────────────────────

    /// Deterministic tokeniser: each message costs exactly `cost` tokens.
    struct FixedCostTokeniser {
        cost: usize,
    }

    impl Tokeniser for FixedCostTokeniser {
        fn count_text(&self, _text: &str) -> usize {
            self.cost
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages.len() * self.cost
        }
    }

    struct MockElement {
        name: &'static str,
        priority: i32,
        messages: Vec<ChatMessage>,
        source: &'static str,
    }

    impl ContextElement for MockElement {
        fn name(&self) -> &str {
            self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        fn content(&self) -> Vec<ChatMessage> {
            self.messages.clone()
        }

        fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
            tokeniser.count_messages(&self.messages)
        }

        fn source(&self) -> String {
            self.source.to_string()
        }
    }

    fn prompts_element(system_text: &str) -> Box<dyn ContextElement> {
        Box::new(MockElement {
            name: "prompts",
            priority: 100,
            messages: vec![ChatMessage::new(Role::System, system_text)],
            source: "prompt rules",
        })
    }

    fn history_element(turns: &[(&str, &str)]) -> Box<dyn ContextElement> {
        let messages = turns
            .iter()
            .flat_map(|(user, asst)| {
                [
                    ChatMessage::new(Role::User, *user),
                    ChatMessage::new(Role::Assistant, *asst),
                ]
            })
            .collect();
        Box::new(MockElement {
            name: "history",
            priority: 0,
            messages,
            source: "thread history",
        })
    }

    fn memory_element() -> Box<dyn ContextElement> {
        Box::new(MockElement {
            name: "memory",
            priority: 50,
            messages: vec![ChatMessage::new(Role::User, "some memory content")],
            source: "agent memory",
        })
    }

    fn no_budget_config() -> ContextConfig {
        ContextConfig {
            assembly: "default".to_string(),
            alternatives: Vec::new(),
            timeout: 5,
            budget: None,
        }
    }

    fn budget_config(tokens: usize) -> ContextConfig {
        ContextConfig {
            assembly: "default".to_string(),
            alternatives: Vec::new(),
            timeout: 5,
            budget: Some(tokens),
        }
    }

    // ── Assembly order ────────────────────────────────────────────────────────

    #[test]
    fn system_message_from_prompts_appears_before_history() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            // Provide history first (lower priority) to test reordering.
            history_element(&[("Hello", "Hi!")]),
            prompts_element("You are helpful."),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert!(output.messages.len() >= 2);
        // System message must be first.
        assert_eq!(output.messages[0].role(), Role::System);
        // Conversation messages follow.
        assert_eq!(output.messages[1].role(), Role::User);
        assert_eq!(output.messages[2].role(), Role::Assistant);
    }

    #[test]
    fn empty_prompts_produce_no_system_message() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(MockElement {
                name: "prompts",
                priority: 100,
                messages: vec![], // no rules → no system message
                source: "prompt rules",
            }),
            history_element(&[("Hello", "Hi!")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Only history messages — no system message.
        assert!(
            output.messages.iter().all(|m| m.role() != Role::System),
            "expected no system message when prompts element is empty"
        );
        assert_eq!(output.messages.len(), 2);
    }

    // ── Memory is skipped ─────────────────────────────────────────────────────

    #[test]
    fn memory_element_is_excluded_from_assembled_messages() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            memory_element(),
            history_element(&[("Hello", "Hi!")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Only system + 2 history messages; memory content absent.
        assert_eq!(
            output.messages.len(),
            3,
            "memory content should be excluded"
        );
        // No message should come from the memory element.
        for msg in &output.messages {
            let is_memory = matches!(
                msg.content().first(),
                Some(swage_core::ContentPart::Text { text }) if text.contains("memory content")
            );
            assert!(!is_memory, "memory content appeared in assembled messages");
        }
    }

    // ── Report population ─────────────────────────────────────────────────────

    #[test]
    fn report_assembly_name_is_default() {
        let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert_eq!(output.report.assembly_name, "default");
    }

    #[test]
    fn report_contains_element_report_for_each_active_element() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            history_element(&[("Hi", "Hello")]),
            memory_element(), // skipped — should not appear in element reports
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Two active elements: prompts + history. Memory is excluded.
        assert_eq!(output.report.elements.len(), 2);
        let names: Vec<&str> = output
            .report
            .elements
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"prompts"));
        assert!(names.contains(&"history"));
        assert!(!names.contains(&"memory"));
    }

    #[test]
    fn report_preserves_element_token_accounting_when_no_values_are_rendered() {
        struct CustomAccountingElement;

        impl ContextElement for CustomAccountingElement {
            fn name(&self) -> &str {
                "custom-accounting"
            }

            fn priority(&self) -> i32 {
                100
            }

            fn content(&self) -> Vec<ChatMessage> {
                vec![ChatMessage::new(Role::System, "one message")]
            }

            fn token_count(&self, _tokeniser: &dyn Tokeniser) -> usize {
                73
            }

            fn source(&self) -> String {
                "custom accounting".to_owned()
            }
        }

        let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(CustomAccountingElement)];
        let output = default_assembly_with_variables(
            &elements,
            &no_budget_config(),
            &FixedCostTokeniser { cost: 10 },
            &std::collections::BTreeMap::from([("task".to_owned(), "ignored".to_owned())]),
        );

        assert_eq!(output.report.elements[0].token_count, 73);
    }

    #[test]
    fn report_total_tokens_matches_assembled_message_count() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            history_element(&[("Hi", "Hello")]),
        ];
        // cost=10, 3 messages → 30 tokens.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert_eq!(output.report.total_tokens, output.messages.len() * 10);
    }

    #[test]
    fn report_budget_and_budget_remaining_populated_when_budget_configured() {
        let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
        // 2 messages × 10 = 20 tokens. Budget of 100 → remaining = 80.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(100), &tokeniser);

        assert_eq!(output.report.budget, Some(100));
        assert_eq!(output.report.budget_remaining, Some(80));
    }

    #[test]
    fn report_budget_none_when_not_configured() {
        let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert!(output.report.budget.is_none());
        assert!(output.report.budget_remaining.is_none());
    }

    // ── Budget enforcement and compaction ─────────────────────────────────────

    #[test]
    fn compaction_not_triggered_when_within_budget() {
        let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
        // 2 messages × 10 = 20 tokens. Budget 100 → no compaction needed.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(100), &tokeniser);

        assert!(output.report.compaction_applied.is_empty());
    }

    #[test]
    fn compaction_triggered_when_history_exceeds_budget() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            // 6 history messages × 10 = 60 tokens; prompts adds 10 → total 70.
            history_element(&[
                ("Turn 1 user", "Turn 1 assistant"),
                ("Turn 2 user", "Turn 2 assistant"),
                ("Turn 3 user", "Turn 3 assistant"),
            ]),
        ];
        // Budget of 30: prompts takes 10, leaving only 20 for history.
        // History has 6 messages × 10 = 60, so compaction must drop some.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(30), &tokeniser);

        assert!(
            !output.report.compaction_applied.is_empty(),
            "expected compaction to trigger"
        );
        let record = &output.report.compaction_applied[0];
        assert_eq!(record.actual_strategy, "truncation");
        assert!(record.messages_removed > 0);
    }

    #[test]
    fn most_recent_history_exchange_preserved_after_compaction() {
        let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[
            ("Old turn", "Old reply"),
            ("Recent user", "Recent assistant"),
        ])];
        // Tight budget: only room for 2 messages (the recent exchange).
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(20), &tokeniser);

        // The most recent exchange must remain.
        let has_recent_user = output.messages.iter().any(|m| {
            m.role() == Role::User
                && m.content().iter().any(|p| {
                    matches!(p, swage_core::ContentPart::Text { text } if text.contains("Recent user"))
                })
        });
        assert!(has_recent_user, "most recent user message was removed");
    }

    // ── fail-open assemble() ──────────────────────────────────────────────────

    fn skills_element(source: &'static str, instructions: Option<&str>) -> Box<dyn ContextElement> {
        let messages = match instructions {
            Some(text) => vec![ChatMessage::new(Role::System, text)],
            None => vec![],
        };
        Box::new(MockElement {
            name: "skills",
            priority: 75,
            messages,
            source,
        })
    }

    #[test]
    fn skills_element_appears_between_prompts_and_history() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hello", "Hi!")]),
            skills_element(
                "skills: code-review (space, ~35 tokens)",
                Some("Always review for correctness."),
            ),
            prompts_element("You are helpful."),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Expected order: System (prompts), System (skills), User, Assistant
        assert_eq!(output.messages.len(), 4);
        assert_eq!(output.messages[0].role(), Role::System); // prompts
        assert_eq!(output.messages[1].role(), Role::System); // skills
        assert_eq!(output.messages[2].role(), Role::User); // history
        assert_eq!(output.messages[3].role(), Role::Assistant);
    }

    #[test]
    fn empty_skills_element_appears_in_report_with_zero_tokens() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            skills_element("skills (none active)", None),
            history_element(&[("Hi", "Hello")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Skills element produces no messages — total is prompts + history only.
        assert_eq!(
            output.messages.len(),
            3,
            "empty skills element adds no messages"
        );

        // But the skills element must still appear in the report.
        let skills_report = output.report.elements.iter().find(|e| e.name == "skills");
        assert!(
            skills_report.is_some(),
            "empty skills element should appear in report"
        );
        assert_eq!(skills_report.unwrap().token_count, 0);
    }

    #[test]
    fn skills_element_report_preview_contains_skill_names() {
        let source = "skills: code-review (space, ~35 tokens), persona (agent, ~22 tokens)";
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hi", "Hello")]),
            skills_element(source, Some("Review code. Be a persona.")),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        let skills_report = output
            .report
            .elements
            .iter()
            .find(|e| e.name == "skills")
            .unwrap();
        let preview = skills_report.preview.as_deref().unwrap_or("");
        assert!(
            preview.contains("code-review"),
            "preview must include skill name"
        );
        assert!(
            preview.contains("persona"),
            "preview must include second skill name"
        );
    }

    #[test]
    fn empty_skills_element_report_has_no_preview() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hi", "Hello")]),
            skills_element("skills (none active)", None),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        let skills_report = output
            .report
            .elements
            .iter()
            .find(|e| e.name == "skills")
            .unwrap();
        // No active skills → preview should be None.
        assert!(
            skills_report.preview.is_none(),
            "empty skills should have no preview"
        );
    }

    #[test]
    fn ordinary_element_report_preview_is_a_bounded_content_excerpt() {
        let elements: Vec<Box<dyn ContextElement>> = vec![prompts_element(&"x".repeat(300))];
        let output = default_assembly(
            &elements,
            &no_budget_config(),
            &FixedCostTokeniser { cost: 10 },
        );

        let preview = output.report.elements[0].preview.as_deref().unwrap();
        assert_eq!(preview.chars().count(), 241);
        assert!(preview.ends_with('…'));
        assert!(preview.starts_with('x'));
    }

    #[test]
    fn memory_remains_excluded_when_skills_element_present() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            skills_element(
                "skills: my-skill (space, ~10 tokens)",
                Some("Skill instructions."),
            ),
            memory_element(),
            history_element(&[("Hi", "Hello")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Prompts (1) + skills (1) + history (2) = 4. Memory excluded.
        assert_eq!(output.messages.len(), 4, "memory must remain excluded");

        let report_names: Vec<&str> = output
            .report
            .elements
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(
            !report_names.contains(&"memory"),
            "memory must not appear in report"
        );
        assert!(
            report_names.contains(&"skills"),
            "skills must appear in report"
        );
    }
}
