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
use crate::tokeniser::{TiktokenTokeniser, Tokeniser};
use futures::future::join_all;
use swage_core::{ChatMessage, Role};
use swage_provider::LlmProvider;

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
                    requested_strategy: "truncation".to_string(),
                    actual_strategy: "truncation".to_string(),
                    messages_removed: 0,
                    tokens_before,
                    tokens_after: tokens_before,
                    source_message_range: None,
                    budget: Some(budget),
                    failure_reason: None,
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

        // Pre-compute the token cost of each droppable message so the loop runs in
        // O(N) rather than O(N²). Per-message counts may slightly undercount framing
        // overhead that only appears in a full list, but the approximation is acceptable
        // for truncation and the difference is at most a handful of tokens per message.
        let droppable_costs: Vec<usize> = droppable
            .iter()
            .map(|&i| tokeniser.count_messages(std::slice::from_ref(&messages[i])))
            .collect();

        let mut running_total = tokens_before;

        for (&idx, &cost) in droppable.iter().zip(droppable_costs.iter()) {
            if running_total <= budget {
                break;
            }
            running_total = running_total.saturating_sub(cost);
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
                requested_strategy: "truncation".to_string(),
                actual_strategy: "truncation".to_string(),
                messages_removed: removed.len(),
                tokens_before,
                tokens_after,
                source_message_range: None,
                budget: Some(budget),
                failure_reason: None,
            },
        )
    }
}

impl Default for TruncationCompactor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Two-phase compaction — intent recording and concurrent execution
// ---------------------------------------------------------------------------

/// A recorded intent to compact a named element using a specific strategy.
///
/// Collected during the first pass of assembly, then dispatched concurrently
/// by [`CompactionExecutor::execute_pending`] in the second pass.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingCompaction {
    /// Name of the element being compacted (e.g. `"history"`, `"memory"`).
    pub element_name: String,
    /// Requested compaction strategy (e.g. `"truncation"`, `"structured_extract"`).
    pub strategy: String,
    /// Maximum token budget for the compacted output.
    pub budget: usize,
    /// Messages to compact.
    pub messages: Vec<ChatMessage>,
    /// Optional slice of the original history that produced these messages —
    /// `(start_index, end_index)` into the turn list.
    pub source_range: Option<(usize, usize)>,
}

/// The outcome of executing one [`PendingCompaction`] intent.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// The original intent that produced this result.
    pub intent: PendingCompaction,
    /// The compacted message list.
    pub compacted: Vec<ChatMessage>,
    /// The strategy that was actually applied — may differ from
    /// `intent.strategy` when fallback to truncation occurred.
    pub actual_strategy: String,
    /// When the requested strategy failed and truncation was used as a fallback,
    /// this carries a human-readable explanation of the failure.
    pub failure_reason: Option<String>,
}

/// Drains a list of [`PendingCompaction`] intents and executes them concurrently.
///
/// Strategies are dispatched based on the `strategy` field of each intent. When
/// a strategy is unavailable or its LLM call fails, the executor falls back to
/// [`TruncationCompactor`] and records the reason in [`CompactionResult::failure_reason`].
pub struct CompactionExecutor;

impl CompactionExecutor {
    /// Construct a new executor.
    pub fn new() -> Self {
        Self
    }

    /// Execute all pending compaction intents concurrently.
    ///
    /// `provider` is passed through to LLM-backed strategies (e.g.
    /// `structured_extract`). Currently only `"truncation"` is implemented;
    /// any other strategy name falls back to truncation with a failure note.
    pub async fn execute_pending(
        &self,
        pending: Vec<PendingCompaction>,
        provider: &(dyn LlmProvider + '_),
    ) -> Vec<CompactionResult> {
        let tokeniser = TiktokenTokeniser::new(provider.model_id());
        let compactor = TruncationCompactor::new();

        // Build one future per intent and drive them all concurrently.
        // Since LlmProvider is Sync, &dyn LlmProvider is Send-safe here.
        let futs: Vec<_> = pending
            .into_iter()
            .map(|intent| execute_single(intent, provider, &compactor, &tokeniser))
            .collect();

        join_all(futs).await
    }
}

impl Default for CompactionExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute a single compaction intent, falling back to truncation on failure.
async fn execute_single(
    intent: PendingCompaction,
    provider: &(dyn LlmProvider + '_),
    compactor: &TruncationCompactor,
    tokeniser: &dyn Tokeniser,
) -> CompactionResult {
    match intent.strategy.as_str() {
        "truncation" => {
            let (compacted, _record) =
                compactor.compact(intent.messages.clone(), intent.budget, tokeniser);
            CompactionResult {
                actual_strategy: "truncation".to_string(),
                failure_reason: None,
                compacted,
                intent,
            }
        }

        "structured_extract" => {
            // Short-circuit when messages already fit within budget — avoids an
            // unnecessary LLM call and matches TruncationCompactor's semantics.
            let tokens_before = tokeniser.count_messages(&intent.messages);
            if tokens_before <= intent.budget {
                return CompactionResult {
                    actual_strategy: "structured_extract".to_string(),
                    failure_reason: None,
                    compacted: intent.messages.clone(),
                    intent,
                };
            }
            // Messages exceed budget — run LLM-backed extraction.
            let extractor = crate::structured_extraction::StructuredExtractionCompactor::new();
            match extractor
                .extract(&intent.messages, intent.budget, provider, tokeniser)
                .await
            {
                Ok(output) => CompactionResult {
                    actual_strategy: "structured_extract".to_string(),
                    failure_reason: None,
                    compacted: vec![ChatMessage::new(Role::System, &output.content)],
                    intent,
                },
                Err(e) => {
                    let failure =
                        format!("structured_extract failed: {e}; fell back to truncation");
                    let (compacted, _) =
                        compactor.compact(intent.messages.clone(), intent.budget, tokeniser);
                    CompactionResult {
                        actual_strategy: "truncation".to_string(),
                        failure_reason: Some(failure),
                        compacted,
                        intent,
                    }
                }
            }
        }

        "delta_summarise" => {
            // Short-circuit when messages already fit within budget.
            let tokens_before = tokeniser.count_messages(&intent.messages);
            if tokens_before <= intent.budget {
                return CompactionResult {
                    actual_strategy: "delta_summarise [experimental]".to_string(),
                    failure_reason: None,
                    compacted: intent.messages.clone(),
                    intent,
                };
            }
            // Construct with no prior — first-use semantics for executor context.
            let delta_compactor =
                crate::delta_summarisation::DeltaSummarisationCompactor::new(None);
            match delta_compactor
                .compact(&intent.messages, intent.budget, provider, tokeniser)
                .await
            {
                Ok(output) => CompactionResult {
                    actual_strategy: "delta_summarise [experimental]".to_string(),
                    failure_reason: None,
                    compacted: output.messages,
                    intent,
                },
                Err(e) => {
                    let failure = format!("delta_summarise failed: {e}; fell back to truncation");
                    let (compacted, _) =
                        compactor.compact(intent.messages.clone(), intent.budget, tokeniser);
                    CompactionResult {
                        actual_strategy: "truncation".to_string(),
                        failure_reason: Some(failure),
                        compacted,
                        intent,
                    }
                }
            }
        }

        "selective_evict" => {
            // Selective eviction always runs the LLM — it removes specific low-value
            // messages regardless of budget, rather than trimming to a size target.
            let eviction_compactor = crate::selective_eviction::SelectiveEvictionCompactor::new();
            match eviction_compactor
                .compact(&intent.messages, provider, tokeniser)
                .await
            {
                Ok(output) => CompactionResult {
                    actual_strategy: "selective_evict [experimental]".to_string(),
                    failure_reason: None,
                    compacted: output.messages,
                    intent,
                },
                Err(e) => {
                    let failure = format!("selective_evict failed: {e}; fell back to truncation");
                    let (compacted, _) =
                        compactor.compact(intent.messages.clone(), intent.budget, tokeniser);
                    CompactionResult {
                        actual_strategy: "truncation".to_string(),
                        failure_reason: Some(failure),
                        compacted,
                        intent,
                    }
                }
            }
        }

        other => {
            // Unknown strategy — fall back to truncation and record why.
            let failure = format!(
                "strategy '{}' is not available; fell back to truncation",
                other
            );
            let (compacted, _record) =
                compactor.compact(intent.messages.clone(), intent.budget, tokeniser);
            CompactionResult {
                actual_strategy: "truncation".to_string(),
                failure_reason: Some(failure),
                compacted,
                intent,
            }
        }
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
            && m.content.iter().any(
                |p| matches!(p, swage_core::ContentPart::Text { text } if text.contains("First"))
            )));

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
            make(
                Role::System,
                "Critical system instructions that must never be lost.",
            ),
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
            make(
                Role::Assistant,
                "Most recent assistant reply — protect this",
            ),
        ];
        // Budget so tight that only the recent pair can fit.
        let just_recent = vec![
            make(Role::User, "Most recent user message — protect this"),
            make(
                Role::Assistant,
                "Most recent assistant reply — protect this",
            ),
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
        assert_eq!(record.actual_strategy, "truncation");
        assert_eq!(record.requested_strategy, "truncation");
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
        let msgs = vec![make(Role::User, "Hello"), make(Role::Assistant, "Hi there")];
        let (_, record) = compactor.compact(msgs, 10_000, &t);

        // The tokeniser (not character arithmetic) produces a real estimate.
        assert!(
            record.tokens_before > 0,
            "tokens_before should be positive (tokeniser is being used)"
        );
    }

    // ── PendingCompaction and CompactionExecutor ──────────────────────────────

    mod executor_tests {
        use super::*;
        use async_trait::async_trait;
        use futures::Stream;
        use std::pin::Pin;
        use swage_core::{
            CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
            ToolCompletionRequest, ToolCompletionResponse,
        };
        use swage_provider::LlmProvider;

        /// A provider that always returns an error — used to verify fallback behaviour.
        struct FailingProvider;

        /// A provider that returns a fixed response — used to verify strategy execution.
        struct SuccessProvider {
            response: &'static str,
        }

        #[async_trait]
        impl LlmProvider for SuccessProvider {
            fn model_id(&self) -> &str {
                "success-mock"
            }

            async fn complete(
                &self,
                _req: CompletionRequest,
            ) -> Result<CompletionResponse, ProviderError> {
                Ok(CompletionResponse {
                    content: self.response.to_string(),
                    input_tokens: 10,
                    output_tokens: 5,
                    finish_reason: FinishReason::Stop,
                    cost: None,
                })
            }

            async fn complete_with_tools(
                &self,
                _req: ToolCompletionRequest,
            ) -> Result<ToolCompletionResponse, ProviderError> {
                Err(ProviderError::Network {
                    provider: "mock".to_string(),
                    message: "not needed".to_string(),
                })
            }

            fn stream(
                &self,
                _req: CompletionRequest,
            ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
                Box::pin(futures::stream::empty())
            }
        }

        #[async_trait]
        impl LlmProvider for FailingProvider {
            fn model_id(&self) -> &str {
                "failing-mock"
            }

            async fn complete(
                &self,
                _req: CompletionRequest,
            ) -> Result<CompletionResponse, ProviderError> {
                Err(ProviderError::Network {
                    provider: "mock".to_string(),
                    message: "simulated LLM failure".to_string(),
                })
            }

            async fn complete_with_tools(
                &self,
                _req: ToolCompletionRequest,
            ) -> Result<ToolCompletionResponse, ProviderError> {
                Err(ProviderError::Network {
                    provider: "mock".to_string(),
                    message: "simulated LLM failure".to_string(),
                })
            }

            fn stream(
                &self,
                _req: CompletionRequest,
            ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
                Box::pin(futures::stream::empty())
            }
        }

        #[tokio::test]
        async fn execute_pending_returns_results_for_truncation_strategy() {
            let executor = CompactionExecutor::new();
            let provider = FailingProvider;
            let msgs = vec![make(Role::User, "Hello"), make(Role::Assistant, "Hi")];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                strategy: "truncation".to_string(),
                budget: 10_000,
                messages: msgs.clone(),
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].actual_strategy, "truncation");
            assert!(results[0].failure_reason.is_none());
            assert!(!results[0].compacted.is_empty());
        }

        #[tokio::test]
        async fn execute_pending_fallback_on_unknown_strategy() {
            let executor = CompactionExecutor::new();
            let provider = FailingProvider;
            let msgs = vec![
                make(Role::User, "Some message"),
                make(Role::Assistant, "Some reply"),
            ];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                // Use a strategy name that is genuinely unrecognised.
                strategy: "nonexistent_strategy".to_string(),
                budget: 10_000,
                messages: msgs,
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            // Unknown strategy — must fall back to truncation.
            assert_eq!(results[0].actual_strategy, "truncation");
            assert!(
                results[0].failure_reason.is_some(),
                "failure_reason should explain why truncation was used"
            );
        }

        #[tokio::test]
        async fn execute_pending_handles_multiple_intents_concurrently() {
            let executor = CompactionExecutor::new();
            let provider = FailingProvider;

            let pending = vec![
                PendingCompaction {
                    element_name: "history".to_string(),
                    strategy: "truncation".to_string(),
                    budget: 10_000,
                    messages: vec![make(Role::User, "A"), make(Role::Assistant, "B")],
                    source_range: None,
                },
                PendingCompaction {
                    element_name: "memory".to_string(),
                    strategy: "structured_extract".to_string(),
                    budget: 500,
                    messages: vec![make(Role::User, "C"), make(Role::Assistant, "D")],
                    source_range: Some((0, 2)),
                },
            ];

            let results = executor.execute_pending(pending, &provider).await;
            assert_eq!(results.len(), 2);
            // All intents must produce a result (even when strategy falls back).
            for r in &results {
                assert!(!r.compacted.is_empty());
            }
        }

        #[tokio::test]
        async fn pending_compaction_carries_source_range() {
            let intent = PendingCompaction {
                element_name: "history".to_string(),
                strategy: "truncation".to_string(),
                budget: 1000,
                messages: vec![],
                source_range: Some((3, 7)),
            };
            assert_eq!(intent.source_range, Some((3, 7)));
        }

        // ── structured_extract strategy ───────────────────────────────────────

        #[tokio::test]
        async fn execute_pending_structured_extract_within_budget_labels_strategy() {
            // Messages already fit within budget — LLM is not called; strategy
            // label propagates so the report reflects the intended strategy.
            let executor = CompactionExecutor::new();
            let provider = FailingProvider; // provider never called in this path
            let msgs = vec![
                make(Role::User, "Some message"),
                make(Role::Assistant, "Some reply"),
            ];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                strategy: "structured_extract".to_string(),
                budget: 10_000,
                messages: msgs.clone(),
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            // Short-circuit: within budget → strategy label preserved, no failure.
            assert_eq!(results[0].actual_strategy, "structured_extract");
            assert!(
                results[0].failure_reason.is_none(),
                "no failure expected when messages fit within budget"
            );
            assert_eq!(
                results[0].compacted.len(),
                msgs.len(),
                "messages should be returned unchanged"
            );
        }

        #[tokio::test]
        async fn execute_pending_structured_extract_falls_back_on_llm_failure() {
            // Messages exceed a tiny budget; the LLM call fails → fall back to truncation.
            let executor = CompactionExecutor::new();
            let provider = FailingProvider;
            let msgs = vec![
                make(
                    Role::User,
                    "Some message with enough text to exceed a tiny budget",
                ),
                make(Role::Assistant, "A reply that also adds to the token count"),
            ];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                strategy: "structured_extract".to_string(),
                budget: 1, // impossibly small — messages exceed this
                messages: msgs,
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            // LLM call failed → fell back to truncation.
            assert_eq!(results[0].actual_strategy, "truncation");
            assert!(
                results[0].failure_reason.is_some(),
                "failure_reason must describe the extraction error"
            );
        }

        // ── delta_summarise strategy ──────────────────────────────────────────

        #[tokio::test]
        async fn execute_pending_delta_summarise_within_budget_labels_strategy() {
            // Messages fit within budget — short-circuit, no LLM call.
            let executor = CompactionExecutor::new();
            let provider = FailingProvider; // not called in this path
            let msgs = vec![make(Role::User, "Hello"), make(Role::Assistant, "Hi")];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                strategy: "delta_summarise".to_string(),
                budget: 10_000,
                messages: msgs.clone(),
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            // Experimental marker must be present in the strategy label.
            assert_eq!(results[0].actual_strategy, "delta_summarise [experimental]");
            assert!(results[0].failure_reason.is_none());
        }

        #[tokio::test]
        async fn execute_pending_delta_summarise_falls_back_on_llm_failure() {
            // Messages exceed budget; LLM fails → fall back to truncation.
            let executor = CompactionExecutor::new();
            let provider = FailingProvider;
            let msgs = vec![
                make(
                    Role::User,
                    "Message with enough text to exceed a tiny budget for sure",
                ),
                make(
                    Role::Assistant,
                    "Reply with enough text to also exceed the budget",
                ),
            ];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                strategy: "delta_summarise".to_string(),
                budget: 1, // impossibly small
                messages: msgs,
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].actual_strategy, "truncation");
            assert!(results[0].failure_reason.is_some());
        }

        // ── selective_evict strategy ──────────────────────────────────────────

        #[tokio::test]
        async fn execute_pending_selective_evict_labels_strategy_on_success() {
            // Provider returns an empty eviction list (nothing to remove) — strategy succeeds.
            let executor = CompactionExecutor::new();
            let provider = SuccessProvider { response: "[]" }; // no evictions
            let msgs = vec![make(Role::User, "Hello"), make(Role::Assistant, "Hi")];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                strategy: "selective_evict".to_string(),
                budget: 10_000,
                messages: msgs.clone(),
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            // Experimental marker must be present in the strategy label.
            assert_eq!(results[0].actual_strategy, "selective_evict [experimental]");
            assert!(results[0].failure_reason.is_none());
        }

        #[tokio::test]
        async fn execute_pending_selective_evict_falls_back_on_llm_failure() {
            // LLM call fails → fall back to truncation.
            let executor = CompactionExecutor::new();
            let provider = FailingProvider;
            let msgs = vec![make(Role::User, "Hello"), make(Role::Assistant, "Hi")];

            let pending = vec![PendingCompaction {
                element_name: "history".to_string(),
                strategy: "selective_evict".to_string(),
                budget: 10_000,
                messages: msgs,
                source_range: None,
            }];

            let results = executor.execute_pending(pending, &provider).await;

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].actual_strategy, "truncation");
            assert!(results[0].failure_reason.is_some());
        }
    }
}
