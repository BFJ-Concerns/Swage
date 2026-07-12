//! Memory element — space memory content for the assembly engine.
//!
//! The memory element wraps the space's persistent memory string and exposes
//! it as a system message. It is available to the assembly engine but the
//! default assembly does NOT include it in assembled output — this preserves
//! legacy behaviour where memory is tool-accessible rather than prepended.
//! Custom assemblies can include memory at any position.

use swage_core::{ChatMessage, Role};

use crate::element::ContextElement;
use crate::tokeniser::Tokeniser;

/// A context element carrying the space's persistent memory content.
///
/// Produces a single `Role::System` message containing the memory text, or
/// no messages if the memory is empty. Priority 50 places it between
/// instructions (100) and conversation history (0).
pub struct MemoryElement {
    // The raw memory content — empty string means no memory is available.
    content: String,
    // Space name, reported as the element's provenance.
    space_name: String,
}

impl MemoryElement {
    /// Create a new memory element.
    ///
    /// - `content` — the space's memory text (may be empty)
    /// - `space_name` — used in the assembly report's `source` field
    pub fn new(content: String, space_name: String) -> Self {
        Self {
            content,
            space_name,
        }
    }
}

impl ContextElement for MemoryElement {
    fn name(&self) -> &str {
        "memory"
    }

    /// Priority 50: above history (0) but below system instructions (100).
    fn priority(&self) -> i32 {
        50
    }

    /// Produce a single system message if memory is non-empty; empty otherwise.
    ///
    /// The default assembly ignores this element's output to preserve the
    /// legacy prompt shape. Custom assemblies call `content()` to include
    /// memory at their chosen position.
    fn content(&self) -> Vec<ChatMessage> {
        if self.content.is_empty() {
            return vec![];
        }
        vec![ChatMessage::new(Role::System, &self.content)]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        // Estimate via the generated message, not just the raw text, so the
        // count includes per-message framing overhead.
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        format!("space memory: {}", self.space_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// Word-count tokeniser: 10 tokens per message regardless of content.
    struct MockTokeniser;

    impl Tokeniser for MockTokeniser {
        fn count_text(&self, _text: &str) -> usize {
            1
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages.len() * 10
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Non-empty memory produces a single Role::System message.
    #[test]
    fn non_empty_memory_produces_system_message() {
        let element = MemoryElement::new(
            "The user prefers concise answers.".to_string(),
            "my-space".to_string(),
        );

        let messages = element.content();
        assert_eq!(messages.len(), 1, "expected exactly one message");
        assert_eq!(
            messages[0].role(),
            Role::System,
            "memory should produce a system message"
        );
    }

    /// Empty memory produces no messages — the element is a no-op in assembly.
    #[test]
    fn empty_memory_produces_no_messages() {
        let element = MemoryElement::new("".to_string(), "empty-space".to_string());

        assert!(
            element.content().is_empty(),
            "empty memory should produce no messages"
        );
    }

    /// token_count() returns a positive estimate for non-empty memory.
    #[test]
    fn token_count_reflects_memory_content() {
        let element =
            MemoryElement::new("Some memory content.".to_string(), "test-space".to_string());

        let tokeniser = MockTokeniser;
        let count = element.token_count(&tokeniser);
        // MockTokeniser returns 10 per message; we produce 1 message.
        assert_eq!(count, 10);
    }

    /// Empty memory returns zero tokens — no messages to count.
    #[test]
    fn empty_memory_returns_zero_tokens() {
        let element = MemoryElement::new("".to_string(), "test-space".to_string());

        let tokeniser = MockTokeniser;
        assert_eq!(element.token_count(&tokeniser), 0);
    }

    /// Priority is 50 — between instructions and history.
    #[test]
    fn priority_is_fifty() {
        let element = MemoryElement::new("content".to_string(), "test".to_string());
        assert_eq!(element.priority(), 50);
    }

    /// source() includes the space name for provenance in the assembly report.
    #[test]
    fn source_includes_space_name() {
        let element = MemoryElement::new("content".to_string(), "swage-space".to_string());
        assert!(
            element.source().contains("swage-space"),
            "source should include the space name"
        );
    }

    /// MemoryElement is usable as dyn ContextElement (object safety check).
    #[test]
    fn memory_element_is_usable_as_dyn_context_element() {
        let element: Box<dyn ContextElement> = Box::new(MemoryElement::new(
            "Remember: user prefers Rust.".to_string(),
            "proj".to_string(),
        ));

        assert_eq!(element.name(), "memory");
        assert_eq!(element.priority(), 50);
        assert_eq!(element.content().len(), 1);
    }
}
