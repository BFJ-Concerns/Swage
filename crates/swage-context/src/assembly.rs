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
/// | 103      | `"memory"` — the operator profile and memory index |
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

    let active: Vec<&dyn ContextElement> = sorted
        .into_iter()
        .filter(|e| default_assembly_carries(e.name()))
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

        // Elements opt in only when they can construct a useful preview
        // without copying operator, session, or conversation material into
        // the persistent report. Generic assembly cannot know every literal
        // secret an element may contain, so it must not derive one itself.
        let preview = element
            .report_preview()
            .or_else(|| safe_metadata_preview(element.name(), &source));

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
        declared_omissions: Vec::new(),
        effective_tool_permissions: None,
        tool_scope: None,
        turn_graph: None,
        provider_visible: None,
        duration,
    };

    // If assembled is empty the caller provided no viable elements — that is a
    // programming error, not a runtime condition, so we panic here.
    AssemblyOutput::new_unchecked(assembled, report)
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
/// Whether the default assembly carries this element's content into the
/// assembled context. Every element the builder supplies is carried; this is
/// the single statement of the default assembly's disposition — the baseline
/// floor a named assembly is held to reads it rather than restating the list.
pub fn default_assembly_carries(_name: &str) -> bool {
    true
}

pub fn safe_metadata_preview(name: &str, source: &str) -> Option<String> {
    (name == "skills")
        .then(|| extract_skill_names_preview(source))
        .flatten()
}

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
#[path = "assembly/contract.rs"]
mod tests;
