//! Skills assembly element — active skill instructions as a context element.
//!
//! Wraps a set of active skill definitions and produces their instructions
//! as `ChatMessage` values for the assembly engine. Agent-level skills are
//! ordered first, followed by user-activated skills in activation order.

use swage_config::model::HierarchyLevel;
use swage_core::{ChatMessage, Role};

use crate::element::ContextElement;
use crate::tokeniser::Tokeniser;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A single skill that is currently active for a conversation turn.
///
/// `is_agent_level` drives ordering: agent-level skills appear before
/// user-activated skills regardless of `activation_order`. Within each
/// group, `activation_order` determines the sequence.
#[derive(Debug, Clone)]
pub struct ActiveSkill {
    /// Canonical skill identifier (directory name).
    pub name: String,
    /// The markdown body of the SKILL.md file — the skill's instructions.
    pub instructions: String,
    /// Which config hierarchy level this skill was sourced from.
    pub level: HierarchyLevel,
    /// True when the skill was activated at the agent level (always-on).
    /// False when the user activated it during the session.
    pub is_agent_level: bool,
    /// Relative position within the user-activated group.
    /// Lower values appear first. Ignored for agent-level skills.
    pub activation_order: u32,
}

// ── Element ───────────────────────────────────────────────────────────────────

/// A context element carrying the instructions of all currently active skills.
///
/// Agent-level skills appear first (in the order they were loaded), followed
/// by user-activated skills ordered by `activation_order`. All instructions
/// are concatenated into a single `Role::System` message. If no skills are
/// active, the element produces no messages — the assembly engine treats it
/// as a zero-token no-op.
///
/// Priority 75 places skills below system prompts (100) but above space
/// memory (50), ensuring skill instructions are seen before retrieved context
/// but after core identity instructions.
pub struct SkillsElement {
    // The full list of active skills, stored in insertion order.
    // Ordering for content() is applied at call time, not at construction.
    skills: Vec<ActiveSkill>,
}

impl SkillsElement {
    /// Create a new skills element from a list of active skills.
    ///
    /// The list may be in any order — `content()` applies agent-first ordering
    /// when producing messages.
    pub fn new(skills: Vec<ActiveSkill>) -> Self {
        Self { skills }
    }

    /// Return the skills sorted for assembly: agent-level first (stable),
    /// then user-activated in ascending `activation_order`.
    fn sorted_skills(&self) -> Vec<&ActiveSkill> {
        let mut agent: Vec<&ActiveSkill> =
            self.skills.iter().filter(|s| s.is_agent_level).collect();
        let mut user: Vec<&ActiveSkill> =
            self.skills.iter().filter(|s| !s.is_agent_level).collect();

        // Stable sort within groups preserves insertion order as a tiebreaker.
        user.sort_by_key(|s| s.activation_order);

        agent.append(&mut user);
        agent
    }
}

impl ContextElement for SkillsElement {
    fn name(&self) -> &str {
        "skills"
    }

    /// Priority 75: below system prompts (100) but above space memory (50).
    fn priority(&self) -> i32 {
        75
    }

    /// Produce a single system message containing all active skill instructions.
    ///
    /// Agent-level skills appear first, then user-activated skills in
    /// activation order. Instructions are separated by `\n\n---\n\n` to give
    /// the model a clear visual boundary between skills.
    ///
    /// Returns an empty vec when no skills are active.
    fn content(&self) -> Vec<ChatMessage> {
        if self.skills.is_empty() {
            return vec![];
        }

        let combined = self
            .sorted_skills()
            .iter()
            .map(|s| s.instructions.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        vec![ChatMessage::new(Role::System, &combined)]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        if self.skills.is_empty() {
            return 0;
        }
        // Count via the generated message so framing overhead is included.
        tokeniser.count_messages(&self.content())
    }

    /// Human-readable provenance listing each active skill with its level and
    /// token cost.
    ///
    /// Example: `"skills: code-review (space, 142 tokens), persona (agent, 89 tokens)"`
    fn source(&self) -> String {
        if self.skills.is_empty() {
            return "skills (none active)".to_string();
        }

        // Build per-skill descriptions using the tokeniser-free text estimate.
        // We use count_text on the raw instructions so we avoid constructing a
        // full ChatMessage just for the source string.
        let skill_summaries: Vec<String> = self
            .sorted_skills()
            .iter()
            .map(|s| {
                // Token count is reported as an estimate from the raw instruction
                // text. This avoids creating a Tokeniser dependency on source(),
                // which matches the ContextElement trait contract (no tokeniser
                // parameter). Character-based approximation: ~4 chars per token.
                let approx_tokens = (s.instructions.len() / 4).max(1);
                let level = s.level.to_string().to_lowercase();
                format!("{} ({}, ~{} tokens)", s.name, level, approx_tokens)
            })
            .collect();

        format!("skills: {}", skill_summaries.join(", "))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;

    // ── Fixtures ──────────────────────────────────────────────────────────────

    /// Simple tokeniser: 10 tokens per message regardless of content.
    struct MockTokeniser;

    impl crate::tokeniser::Tokeniser for MockTokeniser {
        fn count_text(&self, _text: &str) -> usize {
            1
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages.len() * 10
        }
    }

    fn agent_skill(name: &str, instructions: &str) -> ActiveSkill {
        ActiveSkill {
            name: name.to_string(),
            instructions: instructions.to_string(),
            level: HierarchyLevel::Agent,
            is_agent_level: true,
            activation_order: 0,
        }
    }

    fn user_skill(name: &str, instructions: &str, order: u32) -> ActiveSkill {
        ActiveSkill {
            name: name.to_string(),
            instructions: instructions.to_string(),
            level: HierarchyLevel::Space,
            is_agent_level: false,
            activation_order: order,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Empty skill list produces no messages — zero-token no-op in assembly.
    #[test]
    fn empty_skills_produce_no_messages() {
        let element = SkillsElement::new(vec![]);
        assert!(element.content().is_empty());
    }

    /// Empty skill list returns zero tokens.
    #[test]
    fn empty_skills_return_zero_tokens() {
        let element = SkillsElement::new(vec![]);
        let tokeniser = MockTokeniser;
        assert_eq!(element.token_count(&tokeniser), 0);
    }

    /// A single skill produces one system message containing its instructions.
    #[test]
    fn single_skill_produces_system_message() {
        let element = SkillsElement::new(vec![agent_skill(
            "persona",
            "You are a friendly assistant.",
        )]);

        let messages = element.content();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), Role::System);

        match &messages[0].content()[0] {
            swage_core::ContentPart::Text { text } => {
                assert!(text.contains("You are a friendly assistant."));
            }
            _ => panic!("expected text content"),
        }
    }

    /// Two skills produce a single system message with both instructions joined
    /// by the separator.
    #[test]
    fn two_skills_produce_single_message_with_both_instructions() {
        let element = SkillsElement::new(vec![
            agent_skill("skill-a", "Instruction A."),
            agent_skill("skill-b", "Instruction B."),
        ]);

        let messages = element.content();
        assert_eq!(messages.len(), 1, "all skills → single system message");

        match &messages[0].content()[0] {
            swage_core::ContentPart::Text { text } => {
                assert!(
                    text.contains("Instruction A."),
                    "first skill instructions present"
                );
                assert!(
                    text.contains("Instruction B."),
                    "second skill instructions present"
                );
                assert!(text.contains("---"), "separator is present between skills");
            }
            _ => panic!("expected text content"),
        }
    }

    /// Agent-level skills appear before user-activated skills regardless of
    /// the order they were passed to the constructor.
    #[test]
    fn agent_level_skills_appear_before_user_activated() {
        let element = SkillsElement::new(vec![
            // User skill submitted first — should still appear after agent skill.
            user_skill("user-skill", "User instructions.", 0),
            agent_skill("agent-skill", "Agent instructions."),
        ]);

        let messages = element.content();
        assert_eq!(messages.len(), 1);

        match &messages[0].content()[0] {
            swage_core::ContentPart::Text { text } => {
                let agent_pos = text.find("Agent instructions.").unwrap();
                let user_pos = text.find("User instructions.").unwrap();
                assert!(
                    agent_pos < user_pos,
                    "agent skill must precede user skill in assembled output"
                );
            }
            _ => panic!("expected text content"),
        }
    }

    /// User-activated skills are ordered by activation_order (ascending).
    #[test]
    fn user_skills_ordered_by_activation_order() {
        let element = SkillsElement::new(vec![
            // Deliberately reversed to verify sorting.
            user_skill("second", "Second instructions.", 2),
            user_skill("first", "First instructions.", 1),
            user_skill("third", "Third instructions.", 3),
        ]);

        let messages = element.content();
        assert_eq!(messages.len(), 1);

        match &messages[0].content()[0] {
            swage_core::ContentPart::Text { text } => {
                let first_pos = text.find("First instructions.").unwrap();
                let second_pos = text.find("Second instructions.").unwrap();
                let third_pos = text.find("Third instructions.").unwrap();
                assert!(first_pos < second_pos, "first before second");
                assert!(second_pos < third_pos, "second before third");
            }
            _ => panic!("expected text content"),
        }
    }

    /// token_count() returns a positive estimate for non-empty skills.
    #[test]
    fn token_count_returns_positive_for_non_empty_skills() {
        let element = SkillsElement::new(vec![agent_skill(
            "persona",
            "You are a friendly assistant.",
        )]);
        let tokeniser = MockTokeniser;
        // MockTokeniser returns 10 per message; we produce 1 message.
        assert_eq!(element.token_count(&tokeniser), 10);
    }

    /// Priority is 75 — between prompts (100) and memory (50).
    #[test]
    fn priority_is_seventy_five() {
        let element = SkillsElement::new(vec![]);
        assert_eq!(element.priority(), 75);
    }

    /// name() is "skills".
    #[test]
    fn name_is_skills() {
        let element = SkillsElement::new(vec![]);
        assert_eq!(element.name(), "skills");
    }

    /// source() returns a non-empty string for the empty case without panicking.
    #[test]
    fn source_handles_no_skills_gracefully() {
        let element = SkillsElement::new(vec![]);
        let source = element.source();
        assert!(!source.is_empty(), "source should never be empty");
    }

    /// source() includes each skill's name for non-empty element.
    #[test]
    fn source_includes_skill_names() {
        let element = SkillsElement::new(vec![
            agent_skill("code-review", "Review code carefully."),
            user_skill("persona", "Be friendly.", 1),
        ]);

        let source = element.source();
        assert!(
            source.contains("code-review"),
            "source should include skill name 'code-review'"
        );
        assert!(
            source.contains("persona"),
            "source should include skill name 'persona'"
        );
    }

    /// source() includes the hierarchy level for each skill.
    #[test]
    fn source_includes_hierarchy_level() {
        let element = SkillsElement::new(vec![agent_skill("my-skill", "Agent-level skill.")]);

        let source = element.source();
        // HierarchyLevel::Agent displays as "agent" (lowercased in source).
        assert!(
            source.to_lowercase().contains("agent"),
            "source should include the hierarchy level: {source:?}"
        );
    }

    /// SkillsElement is usable as dyn ContextElement (object safety check).
    #[test]
    fn skills_element_is_usable_as_dyn_context_element() {
        let element: Box<dyn ContextElement> = Box::new(SkillsElement::new(vec![agent_skill(
            "persona",
            "Instructions.",
        )]));

        assert_eq!(element.name(), "skills");
        assert_eq!(element.priority(), 75);
        assert_eq!(element.content().len(), 1);

        let tokeniser = MockTokeniser;
        assert_eq!(element.token_count(&tokeniser), 10);
    }
}
