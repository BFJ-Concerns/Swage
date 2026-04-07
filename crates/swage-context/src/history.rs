//! History element — conversation message history from a thread.
//!
//! The history element carries the thread's chronological message sequence.
//! It is the primary target for truncation compaction when the assembled
//! context exceeds the configured token budget: the compactor trims the oldest
//! messages from the history while preserving the most recent exchange.

use swage_core::ChatMessage;

use crate::element::ContextElement;
use crate::tokeniser::Tokeniser;

/// A context element containing conversation history from a thread.
///
/// Messages are stored in chronological order and returned unchanged via
/// `content()`. Priority 0 places history after higher-priority elements
/// such as system instructions (100) and retrieved memory (50).
pub struct HistoryElement {
    // Thread messages in chronological order — oldest first.
    messages: Vec<ChatMessage>,
}

impl HistoryElement {
    /// Create a new history element from a slice of thread messages.
    ///
    /// `messages` should be in chronological order (oldest first), matching
    /// the order the assembly engine will place them in the final context.
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self { messages }
    }
}

impl ContextElement for HistoryElement {
    fn name(&self) -> &str {
        "history"
    }

    /// Priority 0: history fills whatever budget remains after higher-priority
    /// elements (instructions, memory) have been accounted for.
    fn priority(&self) -> i32 {
        0
    }

    /// Return thread messages unchanged and in the original chronological order.
    fn content(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.messages)
    }

    fn source(&self) -> String {
        "thread history".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// Simple tokeniser mock: 10 tokens per message.
    struct MockTokeniser;

    impl Tokeniser for MockTokeniser {
        fn count_text(&self, _text: &str) -> usize {
            1
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages.len() * 10
        }
    }

    fn make_history() -> Vec<ChatMessage> {
        vec![
            ChatMessage::new(Role::User, "Hello"),
            ChatMessage::new(Role::Assistant, "Hi there!"),
            ChatMessage::new(Role::User, "How are you?"),
        ]
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// content() returns all messages unchanged and in the original order.
    #[test]
    fn content_returns_messages_unchanged() {
        let messages = make_history();
        let element = HistoryElement::new(messages.clone());

        let output = element.content();
        assert_eq!(output.len(), messages.len());
        for (out, original) in output.iter().zip(messages.iter()) {
            assert_eq!(out.role, original.role);
        }
    }

    /// token_count() delegates to the tokeniser's count_messages method.
    #[test]
    fn token_count_delegates_to_tokeniser() {
        let element = HistoryElement::new(make_history()); // 3 messages
        let tokeniser = MockTokeniser;
        // MockTokeniser: 3 messages × 10 = 30
        assert_eq!(element.token_count(&tokeniser), 30);
    }

    /// Empty history returns an empty content vec and zero tokens.
    #[test]
    fn empty_history_returns_empty_content_and_zero_tokens() {
        let element = HistoryElement::new(vec![]);
        let tokeniser = MockTokeniser;

        assert!(element.content().is_empty());
        assert_eq!(element.token_count(&tokeniser), 0);
    }

    /// Priority is 0 — lowest priority, filled last in assembly.
    #[test]
    fn priority_is_zero() {
        let element = HistoryElement::new(make_history());
        assert_eq!(element.priority(), 0);
    }

    /// name() and source() return the expected identifiers.
    #[test]
    fn name_and_source_are_correct() {
        let element = HistoryElement::new(make_history());
        assert_eq!(element.name(), "history");
        assert_eq!(element.source(), "thread history");
    }

    /// HistoryElement can be stored and used as dyn ContextElement.
    #[test]
    fn history_element_is_usable_as_dyn_context_element() {
        let element: Box<dyn ContextElement> = Box::new(HistoryElement::new(make_history()));

        assert_eq!(element.name(), "history");
        assert_eq!(element.content().len(), 3);

        let tokeniser = MockTokeniser;
        assert_eq!(element.token_count(&tokeniser), 30);
    }
}
