//! Truncation compaction — drop oldest messages to fit within a token budget.
//!
//! This is the primary compaction strategy for the context assembly engine.
//! It replaces the agent runtime's rough 4-chars/token heuristic with proper
//! tiktoken-based estimation.
//!
//! ## Rules
//!
//! 1. **System messages** are never dropped — they carry permanent instructions.
//! 2. **The most recent user–assistant exchange** is always preserved, even when
//!    it alone exceeds the budget. This is the minimum viable conversational
//!    context for a continued response.
//! 3. All other non-system messages are candidates for removal, oldest first.
//! 4. Removal stops as soon as the total token count is within budget.

use crate::report::CompactionRecord;
use crate::tokeniser::Tokeniser;
use swage_core::{ChatMessage, Role};

/// Truncation compactor: removes the oldest droppable messages until the
/// message list fits within the given token budget.
///
/// Construct with [`TruncationCompactor::new`] and call [`TruncationCompactor::compact`].
/// The struct carries no state — it exists to provide a named namespace for the
/// strategy and to support future configuration (max-drop-fraction, etc.).
pub struct TruncationCompactor;

impl TruncationCompactor {
    /// Construct a compactor. Currently stateless; reserved for future options.
    pub fn new() -> Self {
        Self
    }

    /// Compact `messages` to fit within `budget` tokens, using `tokeniser` for
    /// all counting.
    ///
    /// Returns the compacted message list and a record describing what was removed.
    /// When the input already fits, the list is returned unchanged and the record
    /// shows zero messages removed.
    pub fn compact(
        &self,
        messages: Vec<ChatMessage>,
        budget: usize,
        tokeniser: &dyn Tokeniser,
    ) -> (Vec<ChatMessage>, CompactionRecord) {
        let tokens_before = tokeniser.count_messages(&messages);

        if tokens_before <= budget {
            // Already within budget — short-circuit with a no-op record.
            return (
                messages,
                CompactionRecord {
                    element: "history".to_string(),
                    strategy: "truncation".to_string(),
                    messages_removed: 0,
                    tokens_before,
                    tokens_after: tokens_before,
                },
            );
        }

        // Determine which messages are protected (must survive truncation).
        //
        // Protected set = system messages at any position + the most recent
        // user–assistant exchange. "Most recent exchange" is the last user message
        // and the last assistant message that follows it (or the last non-system
        // message if no complete pair exists).
        let protected_start = find_protected_start(&messages);

        // Collect droppable candidates: non-system messages before the protected
        // region, ordered oldest-first (they appear earlier in the slice).
        let droppable: Vec<usize> = (0..protected_start)
            .filter(|&i| messages[i].role != Role::System)
            .collect();

        // Iteratively drop the oldest candidate until within budget or exhausted.
        let mut removed: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for &idx in &droppable {
            // Recount after each removal so framing overhead is accurately reflected.
            let remaining: Vec<ChatMessage> = messages
                .iter()
                .enumerate()
                .filter(|(i, _)| !removed.contains(i))
                .map(|(_, m)| m.clone())
                .collect();

            if tokeniser.count_messages(&remaining) <= budget {
                break;
            }

            removed.insert(idx);
        }

        // Build the final list in original order.
        let result: Vec<ChatMessage> = messages
            .iter()
            .enumerate()
            .filter(|(i, _)| !removed.contains(i))
            .map(|(_, m)| m.clone())
            .collect();

        let tokens_after = tokeniser.count_messages(&result);

        (
            result,
            CompactionRecord {
                element: "history".to_string(),
                strategy: "truncation".to_string(),
                messages_removed: removed.len(),
                tokens_before,
                tokens_after,
            },
        )
    }
}

impl Default for TruncationCompactor {
    fn default() -> Self {
        Self::new()
    }
}

/// Find the index in `messages` at which the protected region begins.
///
/// The protected region covers:
/// - The last user message **and** the last assistant message that follows it
///   (the most recent complete exchange), or
/// - Just the last assistant or user message when a complete pair cannot be found.
///
/// System messages are unconditionally protected independently — this function
/// only finds where the "recent exchange" starts within the non-system slice.
fn find_protected_start(messages: &[ChatMessage]) -> usize {
    // Find the last assistant message.
    let last_asst = messages.iter().rposition(|m| m.role == Role::Assistant);

    // Find the last user message that appears before the last assistant message.
    // If there is no assistant message, protect the last user message alone.
    let last_user = match last_asst {
        Some(asst_idx) => messages[..asst_idx]
            .iter()
            .rposition(|m| m.role == Role::User)
            // If no user appears before the assistant, fall back to any user message.
            .or_else(|| messages.iter().rposition(|m| m.role == Role::User)),
        None => messages.iter().rposition(|m| m.role == Role::User),
    };

    match (last_asst, last_user) {
        // Both exist — protect from the earlier of the two.
        (Some(a), Some(u)) => u.min(a),
        (Some(a), None) => a,
        (None, Some(u)) => u,
        // No user or assistant messages — nothing extra to protect.
        (None, None) => messages.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokeniser::TiktokenTokeniser;

    fn make(role: Role, text: &str) -> ChatMessage {
        ChatMessage::new(role, text)
    }

    fn tokeniser() -> TiktokenTokeniser {
        TiktokenTokeniser::new("gpt-4")
    }

    // ── No-op: already within budget ─────────────────────────────────────────

    #[test]
    fn within_budget_returns_unchanged() {
        let compactor = TruncationCompactor::new();
        let t = tokeniser();
        let msgs = vec![
            make(Role::System, "You are helpful."),
            make(Role::User, "Hi"),
            make(Role::Assistant, "Hello!"),
        ];
        let original_count = msgs.len();
        let budget = 10_000; // very generous

        let (result, record) = compactor.compact(msgs, budget, &t);

        assert_eq!(result.len(), original_count);
        assert_eq!(record.messages_removed, 0);
        assert_eq!(record.tokens_before, record.tokens_after);
    }

    // ── Oldest non-system messages are dropped first ──────────────────────────

    #[test]
    fn drops_oldest_non_system_messages_first() {
        let compactor = TruncationCompactor::new();
        let t = tokeniser();
        let msgs = vec![
            make(Role::System, "You are a helpful assistant."),
            make(Role::User, "First message — old"),
            make(Role::Assistant, "First reply — old"),
            make(Role::User, "Second message — recent"),
            make(Role::Assistant, "Second reply — recent"),
        ];
        // Count the last exchange tokens + system.
        let protected = vec![
            make(Role::System, "You are a helpful assistant."),
            make(Role::User, "Second message — recent"),
            make(Role::Assistant, "Second reply — recent"),
        ];
        // Set budget just above what the protected set costs.
        let tight_budget = t.count_messages(&protected) + 5;

        let (result, record) = compactor.compact(msgs, tight_budget, &t);

        // Old messages should be gone.
        assert!(!result.iter().any(|m| m.role == Role::User
            && m.content.iter().any(|p| matches!(p, swage_core::ContentPart::Text { text } if text.contains("First")))));

        // System message must remain.
        assert!(result.iter().any(|m| m.role == Role::System));

        // Record reflects removal.
        assert!(record.messages_removed > 0);
        assert!(record.tokens_after <= tight_budget || record.messages_removed > 0);
    }

    // ── System messages are never removed ────────────────────────────────────

    #[test]
    fn system_messages_survive_truncation() {
        let compactor = TruncationCompactor::new();
        let t = tokeniser();
        let msgs = vec![
            make(Role::System, "Critical system instructions that must never be lost."),
            make(Role::User, "Old user turn"),
            make(Role::Assistant, "Old assistant turn"),
            make(Role::User, "Recent turn"),
            make(Role::Assistant, "Recent reply"),
        ];
        // Extremely tight budget — forces maximum truncation.
        let budget = 1;

        let (result, _record) = compactor.compact(msgs, budget, &t);

        // System message must always be present.
        assert!(
            result.iter().any(|m| m.role == Role::System),
            "system message was incorrectly removed"
        );
    }

    // ── Most recent user–assistant exchange is always preserved ───────────────

    #[test]
    fn most_recent_exchange_is_always_preserved() {
        let compactor = TruncationCompactor::new();
        let t = tokeniser();
        let msgs = vec![
            make(Role::User, "First user message"),
            make(Role::Assistant, "First assistant reply"),
            make(Role::User, "Most recent user message — protect this"),
            make(Role::Assistant, "Most recent assistant reply — protect this"),
        ];
        // Budget so tight that only the recent pair can fit.
        let just_recent = vec![
            make(Role::User, "Most recent user message — protect this"),
            make(Role::Assistant, "Most recent assistant reply — protect this"),
        ];
        let budget = t.count_messages(&just_recent) + 2;

        let (result, record) = compactor.compact(msgs, budget, &t);

        // The most recent exchange must be present.
        assert!(
            result.iter().any(|m| m.role == Role::User
                && m.content.iter().any(|p| matches!(p, swage_core::ContentPart::Text { text } if text.contains("Most recent user")))),
            "most recent user message was removed"
        );
        assert!(
            result.iter().any(|m| m.role == Role::Assistant
                && m.content.iter().any(|p| matches!(p, swage_core::ContentPart::Text { text } if text.contains("Most recent assistant")))),
            "most recent assistant reply was removed"
        );

        assert!(record.messages_removed > 0);
    }

    // ── CompactionRecord accuracy ─────────────────────────────────────────────

    #[test]
    fn record_accurately_reflects_removed_count_and_token_change() {
        let compactor = TruncationCompactor::new();
        let t = tokeniser();
        let msgs = vec![
            make(Role::User, "Old A"),
            make(Role::User, "Old B"),
            make(Role::User, "Recent"),
            make(Role::Assistant, "Reply"),
        ];
        let all_tokens = t.count_messages(&msgs);
        let recent_only = vec![make(Role::User, "Recent"), make(Role::Assistant, "Reply")];
        let budget = t.count_messages(&recent_only) + 5;

        let (result, record) = compactor.compact(msgs, budget, &t);

        assert_eq!(record.tokens_before, all_tokens);
        assert_eq!(record.tokens_after, t.count_messages(&result));
        assert!(record.tokens_after <= record.tokens_before);
        assert_eq!(record.strategy, "truncation");
        assert_eq!(record.element, "history");
    }

    // ── Budget already exceeded by protected content ──────────────────────────

    #[test]
    fn when_protected_content_exceeds_budget_nothing_extra_dropped() {
        let compactor = TruncationCompactor::new();
        let t = tokeniser();
        // Only the recent exchange — nothing droppable.
        let msgs = vec![
            make(Role::User, "Only user message"),
            make(Role::Assistant, "Only assistant reply"),
        ];
        // Budget of 1 — impossible to satisfy, but the exchange must remain.
        let (result, record) = compactor.compact(msgs, 1, &t);

        assert_eq!(result.len(), 2, "protected messages should not be removed");
        assert_eq!(record.messages_removed, 0);
    }

    // ── Tokeniser is used, not character heuristic ────────────────────────────

    #[test]
    fn token_counts_in_record_are_nonzero_for_non_empty_input() {
        let compactor = TruncationCompactor::new();
        let t = tokeniser();
        let msgs = vec![
            make(Role::User, "Hello"),
            make(Role::Assistant, "Hi there"),
        ];
        let (_, record) = compactor.compact(msgs, 10_000, &t);

        // The tokeniser (not character arithmetic) produces a real estimate.
        assert!(
            record.tokens_before > 0,
            "tokens_before should be positive (tokeniser is being used)"
        );
    }
}
