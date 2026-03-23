//! Prompts element — priority-ordered prompt rules assembled into system instructions.
//!
//! The prompts element replaces the flat `config.instructions` string from era 0.1.
//! It accepts pre-sorted `PromptRule` values (sorted by the config loader) and
//! concatenates their content into a single `Role::System` message. The assembled
//! message is byte-equivalent to what the gateway produced in era 0.1 for the
//! same instruction set.

use swage_config::model::HierarchyLevel;
use swage_config::prompt_rule::PromptRule;
use swage_core::{ChatMessage, Role};

use crate::element::ContextElement;
use crate::tokeniser::Tokeniser;

/// A context element that assembles prompt rules into a system instruction message.
///
/// Rules are concatenated in the order given (priority order from the config loader)
/// and emitted as a single `Role::System` message. If no rules are provided, the
/// element produces no messages, matching era 0.1 behaviour where an empty
/// instructions string produced no system message.
///
/// Priority 100 ensures prompts always appear before history and memory in the
/// assembled context.
pub struct PromptsElement {
    // Pre-sorted prompt rules: highest priority first.
    rules: Vec<PromptRule>,
}

impl PromptsElement {
    /// Create a new prompts element from a sorted list of prompt rules.
    ///
    /// `rules` must already be in the desired order (typically priority descending,
    /// as produced by the config loader). The element concatenates their content
    /// in this order.
    pub fn new(rules: Vec<PromptRule>) -> Self {
        Self { rules }
    }
}

impl ContextElement for PromptsElement {
    fn name(&self) -> &str {
        "prompts"
    }

    /// Priority 100: system instructions always appear before history (0) and
    /// retrieved memory (50).
    fn priority(&self) -> i32 {
        100
    }

    /// Concatenate all rules into a single `Role::System` message.
    ///
    /// Rules are joined with `\n\n` to preserve paragraph separation. Returns
    /// an empty vec if no rules are present — the assembly engine then produces
    /// no system message, matching era 0.1 behaviour for projects without
    /// prompt configuration.
    fn content(&self) -> Vec<ChatMessage> {
        if self.rules.is_empty() {
            return vec![];
        }

        let combined = self.rules.iter()
            .map(|r| r.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        vec![ChatMessage::new(Role::System, &combined)]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        if self.rules.is_empty() {
            return 0;
        }
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        if self.rules.is_empty() {
            return "prompt rules (none)".to_string();
        }

        // Count rules per hierarchy level for the provenance summary.
        let mut counts: Vec<(HierarchyLevel, usize)> = Vec::new();

        for rule in &self.rules {
            if let Some(entry) = counts.iter_mut().find(|(lvl, _)| *lvl == rule.source_level) {
                entry.1 += 1;
            } else {
                counts.push((rule.source_level.clone(), 1));
            }
        }

        let level_summary = counts.iter()
            .map(|(level, count)| format!("{count} {level}"))
            .collect::<Vec<_>>()
            .join(", ");

        let total = self.rules.len();
        format!("prompt rules ({total} total: {level_summary})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use swage_core::Role;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    struct MockTokeniser;

    impl Tokeniser for MockTokeniser {
        fn count_text(&self, _text: &str) -> usize {
            1
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages.len() * 10
        }
    }

    fn make_rule(content: &str, priority: i32, level: HierarchyLevel) -> PromptRule {
        PromptRule {
            priority,
            content: content.to_string(),
            source_level: level,
            file_path: PathBuf::from("test.md"),
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Empty rules produce no messages — no system message in the assembly.
    #[test]
    fn empty_rules_produce_no_messages() {
        let element = PromptsElement::new(vec![]);
        assert!(element.content().is_empty());
    }

    /// A single rule produces one system message with the rule's content.
    #[test]
    fn single_rule_produces_system_message() {
        let element = PromptsElement::new(vec![
            make_rule("You are a helpful assistant.", 0, HierarchyLevel::Global),
        ]);

        let messages = element.content();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);

        match &messages[0].content[0] {
            swage_core::ContentPart::Text { text } => {
                assert_eq!(text, "You are a helpful assistant.");
            }
            _ => panic!("expected text content"),
        }
    }

    /// Multiple rules are concatenated in order with double-newline separators.
    #[test]
    fn multiple_rules_concatenate_in_order() {
        let element = PromptsElement::new(vec![
            make_rule("You are a coding assistant.", 100, HierarchyLevel::Project),
            make_rule("Always be safe and helpful.", 50, HierarchyLevel::Global),
            make_rule("Default behaviour.", 0, HierarchyLevel::Global),
        ]);

        let messages = element.content();
        // All rules → single system message.
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);

        match &messages[0].content[0] {
            swage_core::ContentPart::Text { text } => {
                // Content must appear in the order given (highest priority first).
                let coding_pos = text.find("You are a coding assistant.").unwrap();
                let safety_pos = text.find("Always be safe and helpful.").unwrap();
                let default_pos = text.find("Default behaviour.").unwrap();
                assert!(coding_pos < safety_pos, "project rule should appear before global");
                assert!(safety_pos < default_pos, "higher priority global before lower");
            }
            _ => panic!("expected text content"),
        }
    }

    /// token_count() returns zero for empty rules.
    #[test]
    fn empty_rules_return_zero_tokens() {
        let element = PromptsElement::new(vec![]);
        let tokeniser = MockTokeniser;
        assert_eq!(element.token_count(&tokeniser), 0);
    }

    /// token_count() reflects the combined content (delegates to tokeniser).
    #[test]
    fn token_count_reflects_combined_content() {
        let element = PromptsElement::new(vec![
            make_rule("Rule one.", 10, HierarchyLevel::Global),
            make_rule("Rule two.", 5, HierarchyLevel::Global),
        ]);
        let tokeniser = MockTokeniser;
        // One system message produced, MockTokeniser returns 10 per message.
        assert_eq!(element.token_count(&tokeniser), 10);
    }

    /// source() includes per-level counts for the assembly report.
    #[test]
    fn source_includes_level_counts() {
        let element = PromptsElement::new(vec![
            make_rule("Global rule.", 200, HierarchyLevel::Global),
            make_rule("Project rule.", 100, HierarchyLevel::Project),
            make_rule("Agent rule.", 0, HierarchyLevel::Agent),
        ]);

        let source = element.source();
        assert!(source.contains("3 total"), "source should report 3 total rules");
        assert!(source.contains("Global") || source.contains("global"));
        assert!(source.contains("Project") || source.contains("project"));
    }

    /// source() handles no rules without panicking.
    #[test]
    fn source_handles_empty_rules() {
        let element = PromptsElement::new(vec![]);
        // Should not panic; exact wording is an implementation detail.
        let source = element.source();
        assert!(!source.is_empty());
    }

    /// Priority is 100 — always first in assembly ordering.
    #[test]
    fn priority_is_one_hundred() {
        let element = PromptsElement::new(vec![]);
        assert_eq!(element.priority(), 100);
    }

    /// PromptsElement is usable as dyn ContextElement (object safety check).
    #[test]
    fn prompts_element_is_usable_as_dyn_context_element() {
        let element: Box<dyn ContextElement> = Box::new(PromptsElement::new(vec![
            make_rule("Instructions.", 0, HierarchyLevel::Global),
        ]));

        assert_eq!(element.name(), "prompts");
        assert_eq!(element.priority(), 100);
        assert_eq!(element.content().len(), 1);

        let tokeniser = MockTokeniser;
        assert_eq!(element.token_count(&tokeniser), 10);
    }
}
