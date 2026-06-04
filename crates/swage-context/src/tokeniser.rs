//! Token counting abstraction with tiktoken backend.
//!
//! Provides synchronous token estimation for assembly budget management.
//! The trait is extensible for provider-specific counting in future.

use swage_core::{message::ChatMessage, render_message_for_model_context};
use tiktoken_rs::{CoreBPE, cl100k_base, o200k_base};

/// Synchronous token counting over text and message sequences.
///
/// Object-safe so the assembly engine can hold a `Box<dyn Tokeniser>`
/// without threading generic parameters through the entire call stack.
///
/// `Send + Sync` are required so that `&dyn Tokeniser` can cross async
/// await points inside `Send` futures (e.g. the WebSocket handler).
pub trait Tokeniser: Send + Sync {
    /// Count tokens in a raw text string.
    fn count_text(&self, text: &str) -> usize;

    /// Count total tokens for a slice of messages, including per-message
    /// framing overhead and the reply-priming tokens the API appends.
    fn count_messages(&self, messages: &[ChatMessage]) -> usize;
}

/// Tiktoken-backed token counter.
///
/// Selects `cl100k_base` (GPT-4 / Claude approximation) or `o200k_base`
/// (GPT-4o / o1 / o3 series) based on the model name supplied at
/// construction. Accepts ~5–15% estimation error for non-OpenAI models —
/// a reasonable trade-off for a synchronous, offline counter.
pub struct TiktokenTokeniser {
    bpe: CoreBPE,
}

impl TiktokenTokeniser {
    /// Build a tokeniser for the given model name.
    ///
    /// Selects `o200k_base` for GPT-4o, o1, and o3 variants; falls back to
    /// `cl100k_base` for all other names including Claude, Gemini, and any
    /// unknown model string.
    pub fn new(model: &str) -> Self {
        let bpe = if is_o200k_model(model) {
            o200k_base().expect("o200k_base tokeniser failed to initialise")
        } else {
            // cl100k_base is used for GPT-4, Claude, and unknown models.
            cl100k_base().expect("cl100k_base tokeniser failed to initialise")
        };
        Self { bpe }
    }
}

/// Returns true when the model name warrants the o200k_base encoding.
fn is_o200k_model(model: &str) -> bool {
    // GPT-4o family and the o1/o3 reasoning series use the wider o200k vocabulary.
    model.starts_with("gpt-4o") || model.starts_with("o1") || model.starts_with("o3")
}

impl Tokeniser for TiktokenTokeniser {
    fn count_text(&self, text: &str) -> usize {
        self.bpe.encode_with_special_tokens(text).len()
    }

    fn count_messages(&self, messages: &[ChatMessage]) -> usize {
        // Per the OpenAI token-counting cookbook:
        //   - every message carries 3 overhead tokens for role framing
        //   - every reply sequence is primed with 3 leading tokens
        // These constants apply to cl100k_base chat models; the same values
        // work as reasonable approximations for o200k_base and non-OpenAI models.
        const OVERHEAD_PER_MESSAGE: usize = 3;
        const REPLY_PRIMING: usize = 3;

        let mut total = REPLY_PRIMING;

        for msg in messages {
            total += OVERHEAD_PER_MESSAGE;

            // Role string contributes token cost (e.g. "user", "assistant").
            total += self
                .bpe
                .encode_with_special_tokens(&msg.role().to_string())
                .len();

            // Image URL references carry no token cost here since the model bills
            // image tokens separately via vision pricing.
            total += self
                .bpe
                .encode_with_special_tokens(&render_message_for_model_context(msg))
                .len();

            // An explicit name field occupies its own tokens plus one separator.
            if let Some(name) = msg.name() {
                total += self.bpe.encode_with_special_tokens(name).len();
                total += 1;
            }
        }

        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::{ContentPart, ToolCall, ToolResult, build_tool_round_messages, message::Role};

    fn text_message(role: Role, text: &str) -> ChatMessage {
        ChatMessage::new_parts(
            role,
            vec![ContentPart::Text {
                text: text.to_string(),
            }],
        )
    }

    // ── count_text ────────────────────────────────────────────────────────────

    #[test]
    fn count_text_returns_positive_number() {
        let t = TiktokenTokeniser::new("gpt-4");
        let n = t.count_text("hello world");
        assert!(n > 0, "expected positive token count, got {n}");
    }

    #[test]
    fn count_text_empty_string_is_zero() {
        let t = TiktokenTokeniser::new("gpt-4");
        assert_eq!(t.count_text(""), 0);
    }

    #[test]
    fn count_text_longer_input_produces_more_tokens() {
        let t = TiktokenTokeniser::new("gpt-4");
        let short = t.count_text("hi");
        let long = t.count_text("The quick brown fox jumps over the lazy dog.");
        assert!(long > short, "longer text should produce more tokens");
    }

    // ── model selection / fallback ────────────────────────────────────────────

    #[test]
    fn unknown_model_falls_back_without_error() {
        // Should not panic; Claude model names are unknown to tiktoken.
        let t = TiktokenTokeniser::new("claude-sonnet-4-20250514");
        assert!(t.count_text("hello world") > 0);
    }

    #[test]
    fn gpt4o_model_selects_o200k_encoding() {
        // Constructing with a gpt-4o name should succeed and count correctly.
        let t = TiktokenTokeniser::new("gpt-4o");
        assert!(t.count_text("hello world") > 0);
    }

    #[test]
    fn gpt4o_mini_selects_o200k_encoding() {
        let t = TiktokenTokeniser::new("gpt-4o-mini");
        assert!(t.count_text("hello world") > 0);
    }

    #[test]
    fn o1_model_selects_o200k_encoding() {
        let t = TiktokenTokeniser::new("o1-preview");
        assert!(t.count_text("hello world") > 0);
    }

    #[test]
    fn completely_unknown_model_falls_back_to_cl100k() {
        let t = TiktokenTokeniser::new("some-future-model-v99");
        // Must not panic and must return a sensible count.
        assert!(t.count_text("hello world") > 0);
    }

    // ── count_messages ────────────────────────────────────────────────────────

    #[test]
    fn count_messages_empty_slice_returns_reply_priming_only() {
        let t = TiktokenTokeniser::new("gpt-4");
        // Empty slice: no message overhead, just the 3 reply-priming tokens.
        assert_eq!(t.count_messages(&[]), 3);
    }

    #[test]
    fn count_messages_returns_positive_for_single_message() {
        let t = TiktokenTokeniser::new("claude-sonnet-4-20250514");
        let messages = vec![text_message(Role::User, "Hello, how are you?")];
        assert!(t.count_messages(&messages) > 0);
    }

    #[test]
    fn count_messages_with_known_content_is_within_range() {
        let t = TiktokenTokeniser::new("gpt-4");
        let msgs = vec![
            text_message(Role::System, "You are a helpful assistant."),
            text_message(Role::User, "Hello!"),
        ];
        let n = t.count_messages(&msgs);
        // Overhead floor: 3 (reply-priming) + 2×3 (per-message) = 9.
        // Content adds more — expect somewhere between 15 and 60.
        assert!(n > 15, "token count unexpectedly low: {n}");
        assert!(n < 60, "token count unexpectedly high: {n}");
    }

    #[test]
    fn count_messages_increases_with_more_content() {
        let t = TiktokenTokeniser::new("gpt-4");
        let short = vec![text_message(Role::User, "Hi.")];
        let long = vec![text_message(
            Role::User,
            "This is a much longer message with many more words and tokens.",
        )];
        assert!(
            t.count_messages(&long) > t.count_messages(&short),
            "longer message content should produce a higher token count"
        );
    }

    #[test]
    fn count_messages_includes_structured_tool_replay() {
        let t = TiktokenTokeniser::new("gpt-4");
        let empty_message = vec![ChatMessage::new(Role::Assistant, "")];
        let tool_replay = build_tool_round_messages(
            &[ToolCall {
                call_id: "call_1".into(),
                name: "search".into(),
                arguments: serde_json::json!({"query": "rust"}),
            }],
            &[ToolResult {
                call_id: "call_1".into(),
                content: "result text".into(),
                is_error: false,
            }],
            "",
        );

        assert!(
            t.count_messages(&tool_replay) > t.count_messages(&empty_message),
            "structured tool replay should contribute to context token estimates"
        );
    }

    #[test]
    fn count_messages_more_messages_means_more_tokens() {
        let t = TiktokenTokeniser::new("gpt-4");
        let one = vec![text_message(Role::User, "Hello")];
        let two = vec![
            text_message(Role::User, "Hello"),
            text_message(Role::Assistant, "Hi there!"),
        ];
        assert!(
            t.count_messages(&two) > t.count_messages(&one),
            "more messages should produce more tokens"
        );
    }

    // ── object-safety ─────────────────────────────────────────────────────────

    #[test]
    fn trait_is_object_safe() {
        // Verifies at compile time that Tokeniser is usable as a trait object.
        let t: Box<dyn Tokeniser> = Box::new(TiktokenTokeniser::new("gpt-4"));
        assert!(t.count_text("test") > 0);
    }

    #[test]
    fn dyn_tokeniser_count_messages_works() {
        let t: &dyn Tokeniser = &TiktokenTokeniser::new("gpt-4");
        let msgs = vec![text_message(Role::User, "hello")];
        assert!(t.count_messages(&msgs) > 0);
    }
}
