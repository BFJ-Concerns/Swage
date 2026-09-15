//! Context element trait — typed content providers for the assembly engine.
//!
//! Elements are the building blocks of context. Each element knows how to
//! produce its content and report its token cost. The assembly engine accepts
//! elements as input without knowing concrete types — new element types can
//! be added without modifying the engine.

use swage_core::ChatMessage;

use crate::tokeniser::Tokeniser;

/// A typed content provider for the context assembly engine.
///
/// Each element knows how to produce a slice of `ChatMessage` values and
/// estimate its own token cost. The assembly engine sorts elements by priority
/// and accumulates their content against a token budget without needing to
/// know the concrete element type.
///
/// # Object safety
///
/// The trait is intentionally object-safe so the engine can accept
/// `&[Box<dyn ContextElement>]` — heterogeneous collections of elements
/// without generics propagating through the engine API.
///
/// `Send + Sync` are required so that `Box<dyn ContextElement>` can be held
/// across async await points inside `Send` futures (e.g. the gateway turn
/// handler, which is spawned by axum and must satisfy the `Send` bound).
pub trait ContextElement: Send + Sync {
    /// Short identifier for the element type (e.g. `"history"`, `"prompts"`, `"memory"`).
    ///
    /// Used in the assembly report to identify which element contributed
    /// each block of content.
    fn name(&self) -> &str;

    /// Ordering priority: higher values appear earlier in the assembled context.
    ///
    /// Convention:
    /// - `100` — prompts / system instructions (always first)
    /// - `50`  — memory / retrieved context
    /// - `0`   — message history (fills remaining budget)
    fn priority(&self) -> i32;

    /// Produce the element's content as a sequence of `ChatMessage` values.
    ///
    /// Called once per assembly. Implementations should return a fresh clone
    /// each call — the engine may call `content()` and `token_count()` in
    /// any order.
    fn content(&self) -> Vec<ChatMessage>;

    /// Whether the built-in composer should render session-value slots in this element.
    ///
    /// Most model context is literal, including conversation history. Template-backed
    /// elements opt in so brace-shaped text from users is never interpreted accidentally.
    fn renders_session_values(&self) -> bool {
        false
    }

    /// Estimate the token cost of this element's content.
    ///
    /// The tokeniser is provided by the caller so elements do not need to
    /// own one. Estimates are used for budget enforcement and the assembly
    /// report — a ±15% error is acceptable for non-OpenAI models.
    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize;

    /// Human-readable provenance for the assembly report.
    ///
    /// Examples: `"thread history"`, `"topic: my-topic"`, `"global prompts"`.
    fn source(&self) -> String;

    /// A bounded, safe-to-persist glimpse of this element's content.
    ///
    /// Most elements carry operator, session, or conversation material and
    /// therefore have no safe generic preview. An element may opt in only
    /// when it can construct a useful preview without copying that material.
    fn report_preview(&self) -> Option<String> {
        None
    }
}

/// Return the first sentence of trusted built-in content for a report preview.
///
/// The returned text is an exact prefix of `content`; callers remain
/// responsible for passing only static content that is safe to persist.
pub(crate) fn built_in_first_sentence_preview(content: &str) -> String {
    let sentence_end = content.find(". ").map_or(content.len(), |index| index + 1);
    content[..sentence_end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;

    // ── Test fixtures ────────────────────────────────────────────────────────

    /// Minimal tokeniser mock: counts words in text, 10 tokens per message.
    struct MockTokeniser;

    impl crate::tokeniser::Tokeniser for MockTokeniser {
        fn count_text(&self, text: &str) -> usize {
            text.split_whitespace().count()
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            // Rough proxy: 10 tokens per message regardless of content.
            messages.len() * 10
        }
    }

    /// Minimal element for testing object safety and dyn dispatch.
    struct MockElement {
        name: &'static str,
        priority: i32,
        messages: Vec<ChatMessage>,
        source: String,
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
            self.source.clone()
        }
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    /// Trait is object-safe: a concrete type can be stored as Box<dyn ContextElement>.
    #[test]
    fn context_element_is_object_safe() {
        let element: Box<dyn ContextElement> = Box::new(MockElement {
            name: "mock",
            priority: 42,
            messages: vec![ChatMessage::new(Role::User, "hello")],
            source: "test fixture".to_string(),
        });

        assert_eq!(element.name(), "mock");
        assert_eq!(element.priority(), 42);
        assert_eq!(element.source(), "test fixture");
        assert_eq!(element.content().len(), 1);
    }

    /// The engine can iterate a heterogeneous collection via dyn dispatch.
    #[test]
    fn heterogeneous_element_collection_iterates_via_dyn_dispatch() {
        let high: Box<dyn ContextElement> = Box::new(MockElement {
            name: "prompts",
            priority: 100,
            messages: vec![ChatMessage::new(Role::System, "system prompt")],
            source: "global prompts".to_string(),
        });
        let low: Box<dyn ContextElement> = Box::new(MockElement {
            name: "history",
            priority: 0,
            messages: vec![
                ChatMessage::new(Role::User, "hello"),
                ChatMessage::new(Role::Assistant, "hi"),
            ],
            source: "thread history".to_string(),
        });

        let elements: Vec<Box<dyn ContextElement>> = vec![high, low];

        // Can call all trait methods without knowing the concrete types.
        let names: Vec<&str> = elements.iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["prompts", "history"]);

        let priorities: Vec<i32> = elements.iter().map(|e| e.priority()).collect();
        assert_eq!(priorities, vec![100, 0]);

        let total_messages: usize = elements.iter().map(|e| e.content().len()).sum();
        assert_eq!(total_messages, 3, "prompts has 1, history has 2");
    }

    /// token_count() works through dyn dispatch using the provided tokeniser.
    #[test]
    fn token_count_dispatches_to_provided_tokeniser() {
        let element: Box<dyn ContextElement> = Box::new(MockElement {
            name: "history",
            priority: 0,
            messages: vec![
                ChatMessage::new(Role::User, "hello"),
                ChatMessage::new(Role::Assistant, "world"),
            ],
            source: "thread history".to_string(),
        });

        let tokeniser = MockTokeniser;
        // MockTokeniser returns messages.len() * 10 = 2 * 10 = 20.
        assert_eq!(element.token_count(&tokeniser), 20);
    }

    /// An element with no messages returns zero tokens and empty content.
    #[test]
    fn empty_element_returns_zero_tokens_and_empty_content() {
        let element: Box<dyn ContextElement> = Box::new(MockElement {
            name: "memory",
            priority: 50,
            messages: vec![],
            source: "empty memory".to_string(),
        });

        let tokeniser = MockTokeniser;
        assert_eq!(element.token_count(&tokeniser), 0);
        assert!(element.content().is_empty());
    }
}
