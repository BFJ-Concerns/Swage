//! Context compaction — reduces conversation context when it exceeds capacity.
//!
//! Three tiers applied in order, each only if the previous didn't reduce
//! context below the threshold:
//!
//! 1. **Summarise** — condense older turns into a summary via an LLM call.
//! 2. **Truncate** — remove oldest messages beyond the summary.
//! 3. **Aggressive truncate** — keep only system messages and the single most
//!    recent message. All dropped messages are already safe in storage.

use uuid::Uuid;

use swage_core::{ChatMessage, CompletionRequest, ContentPart, Role};
use swage_protocol::client::context::CompactionRecord;
use swage_provider::LlmProvider;
use swage_storage::Storage;

use crate::config::CompactionThreshold;
use crate::error::AgentError;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Byte-equivalent allowance for provider framing around each text message.
///
/// The fast path only applies to text-only messages with no tool payloads.
/// Sixteen bytes per message leaves room for roles and provider framing while
/// keeping clearly small contexts off the counting endpoint.
const PER_MESSAGE_TOKEN_OVERHEAD_BYTES: u64 = 16;

/// Result of checking a prompt against its compaction threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThresholdEvaluation {
    pub exceeds: bool,
    pub native_breach: bool,
}

/// Check whether the message history exceeds the compaction threshold for the
/// model that will receive the completion.
///
/// `threshold_known_exceeded` preserves a previously established native-token
/// breach if a later provider count is unavailable, unless the current
/// text-only prompt has a byte upper bound within the threshold. Estimate-only
/// breaches do not set that flag and continue to fall back to the local estimate.
pub async fn exceeds_threshold_with_model(
    messages: &[ChatMessage],
    threshold: &CompactionThreshold,
    provider: &dyn LlmProvider,
    model: Option<&str>,
    threshold_known_exceeded: bool,
) -> ThresholdEvaluation {
    match threshold {
        CompactionThreshold::TokenCount(max_tokens) => {
            if token_count_byte_upper_bound(messages).is_some_and(|bound| bound <= *max_tokens) {
                return ThresholdEvaluation {
                    exceeds: false,
                    native_breach: false,
                };
            }

            match provider
                .count_tokens(&counting_request(messages, model))
                .await
            {
                Ok(Some(count)) => {
                    let exceeds = count > *max_tokens;
                    ThresholdEvaluation {
                        exceeds,
                        native_breach: exceeds,
                    }
                }
                Ok(None) | Err(_) => ThresholdEvaluation {
                    exceeds: threshold_known_exceeded
                        || estimated_tokens(messages) > *max_tokens as usize,
                    native_breach: threshold_known_exceeded,
                },
            }
        }
        CompactionThreshold::MessageCount(max_count) => ThresholdEvaluation {
            exceeds: messages.len() > *max_count as usize,
            native_breach: false,
        },
    }
}

/// Compact a history using the tokenizer and completion model selected for the
/// turn.
///
/// Set `threshold_known_exceeded` when the caller has just established that
/// the prompt exceeds its native tokenizer threshold. This preserves that
/// breach if a later count is transiently unavailable.
pub async fn compact_with_model(
    messages: Vec<ChatMessage>,
    threshold: &CompactionThreshold,
    provider: &dyn LlmProvider,
    _storage: &dyn Storage,
    _thread_id: Uuid,
    focus: Option<&str>,
    model: Option<&str>,
    threshold_known_exceeded: bool,
) -> Result<(Vec<ChatMessage>, Option<CompactionRecord>), AgentError> {
    let mut native_breach = threshold_known_exceeded;
    if !native_breach {
        let evaluation =
            exceeds_threshold_with_model(messages.as_slice(), threshold, provider, model, false)
                .await;
        native_breach |= evaluation.native_breach;
        if !evaluation.exceeds {
            return Ok((messages, None));
        }
    }
    let tokens_before = estimated_tokens(&messages);

    // Tier 1 — Summarise: condense older half via an LLM call. Falls back
    // gracefully if the provider call fails.
    let (messages, mut messages_removed) = try_summarise(messages, provider, focus).await;
    let evaluation =
        exceeds_threshold_with_model(&messages, threshold, provider, model, native_breach).await;
    native_breach |= evaluation.native_breach;
    if !evaluation.exceeds {
        let tokens_after = estimated_tokens(&messages);
        return Ok((
            messages,
            Some(compaction_record(
                "summarisation",
                messages_removed,
                tokens_before,
                tokens_after,
                focus,
            )),
        ));
    }

    // Tier 2 — Truncate: drop oldest non-system messages.
    let messages_before_tier = messages.len();
    let messages = truncate(messages, threshold);
    messages_removed += messages_before_tier.saturating_sub(messages.len());
    let evaluation =
        exceeds_threshold_with_model(&messages, threshold, provider, model, native_breach).await;
    native_breach |= evaluation.native_breach;
    if !evaluation.exceeds {
        let tokens_after = estimated_tokens(&messages);
        return Ok((
            messages,
            Some(compaction_record(
                "truncation",
                messages_removed,
                tokens_before,
                tokens_after,
                focus,
            )),
        ));
    }

    // Tier 3 — Aggressive truncate: keep system messages + single most recent
    // message. All dropped messages are already persisted to storage.
    let messages_before_tier = messages.len();
    let messages = aggressive_truncate(messages);
    messages_removed += messages_before_tier.saturating_sub(messages.len());
    let evaluation =
        exceeds_threshold_with_model(&messages, threshold, provider, model, native_breach).await;
    if !evaluation.exceeds {
        let tokens_after = estimated_tokens(&messages);
        return Ok((
            messages,
            Some(compaction_record(
                "aggressive_truncation",
                messages_removed,
                tokens_before,
                tokens_after,
                focus,
            )),
        ));
    }

    Err(AgentError::CompactionFailed(
        "context still exceeds capacity after all compaction tiers".into(),
    ))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return a safe byte upper bound for a text-only prompt.
///
/// A BPE token occupies at least one byte. Image and tool payloads have
/// provider-specific token costs outside this bound, so they deliberately
/// bypass the fast path and are counted by the provider instead.
fn token_count_byte_upper_bound(messages: &[ChatMessage]) -> Option<u64> {
    messages.iter().try_fold(0_u64, |total, message| {
        if message.tool_calls().is_some() || message.tool_replay().is_some() {
            return None;
        }

        let text_bytes = message.content().iter().try_fold(0_u64, |sum, part| {
            let ContentPart::Text { text } = part else {
                return None;
            };
            Some(sum.saturating_add(text.len() as u64))
        })?;

        Some(
            total
                .saturating_add(text_bytes)
                .saturating_add(PER_MESSAGE_TOKEN_OVERHEAD_BYTES),
        )
    })
}

/// Build the provider request used only to count the active prompt.
///
/// Counting generates no response, so generation settings and tools are left
/// empty; the completion model is retained for tokenizer selection.
fn counting_request(messages: &[ChatMessage], model: Option<&str>) -> CompletionRequest {
    CompletionRequest {
        messages: messages.to_vec(),
        model: model.map(str::to_owned),
        effort: None,
        max_tokens: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    }
}

/// Attempt to summarise the older half of the message history with an LLM call.
///
/// Returns the compacted list and the number of source messages it removed.
/// A provider failure returns the original list and zero removals so tier 2 can
/// continue without attributing a list change that did not happen.
async fn try_summarise(
    messages: Vec<ChatMessage>,
    provider: &dyn LlmProvider,
    focus: Option<&str>,
) -> (Vec<ChatMessage>, usize) {
    if messages.len() < 2 {
        return (messages, 0);
    }

    let midpoint = messages.len() / 2;
    let old_messages = &messages[..midpoint];
    let new_messages = messages[midpoint..].to_vec();

    // Collect the text content from older messages for the summarisation prompt.
    let old_text: String = old_messages
        .iter()
        .flat_map(|m| m.content().iter())
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let focus_instruction = focus
        .map(|focus| format!("\n\nFocus instructions:\n{focus}"))
        .unwrap_or_default();
    let summarise_request = CompletionRequest {
        effort: None,
        messages: vec![ChatMessage::new(
            Role::User,
            format!(
                "Summarise the following conversation concisely, preserving key information:{focus_instruction}\n\nConversation:\n{old_text}"
            ),
        )],
        model: None,
        max_tokens: Some(512),
        temperature: Some(0.3),
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    };

    match provider.complete(summarise_request).await {
        Ok(response) => {
            let summary = ChatMessage::new(
                Role::System,
                format!("Summary of earlier conversation: {}", response.content),
            );
            let mut compacted = vec![summary];
            compacted.extend(new_messages);
            (compacted, midpoint)
        }
        // Summarisation failed — return unchanged so tier 2 can take over.
        Err(_) => (messages, 0),
    }
}

fn estimated_tokens(messages: &[ChatMessage]) -> usize {
    let character_count: usize = messages
        .iter()
        .flat_map(|message| message.content())
        .map(|part| match part {
            ContentPart::Text { text } => text.len(),
            ContentPart::ImageUrl { .. } => 0,
        })
        .sum();

    character_count / 4
}

fn compaction_record(
    actual_strategy: &str,
    messages_removed: usize,
    tokens_before: usize,
    tokens_after: usize,
    focus: Option<&str>,
) -> CompactionRecord {
    CompactionRecord {
        element: "agent_history".to_string(),
        requested_strategy: "summarisation".to_string(),
        actual_strategy: actual_strategy.to_string(),
        messages_removed,
        tokens_before,
        tokens_after,
        source_message_range: None,
        budget: None,
        failure_reason: None,
        focus: focus.map(str::to_owned),
        diagnosis: None,
    }
}

/// Remove the oldest non-system messages, keeping the most recent ones that
/// bring the total below the threshold. System messages are always preserved.
fn truncate(messages: Vec<ChatMessage>, threshold: &CompactionThreshold) -> Vec<ChatMessage> {
    let (system_msgs, non_system): (Vec<_>, Vec<_>) =
        messages.into_iter().partition(|m| m.role() == Role::System);

    let target_keep = match threshold {
        CompactionThreshold::MessageCount(max) => {
            // Keep enough recent messages so total < max.
            (*max as usize).saturating_sub(system_msgs.len())
        }
        CompactionThreshold::TokenCount(max) => {
            // Estimate how many recent messages fit within the token budget.
            let avg_tokens_per_msg = {
                let total_chars: usize = non_system
                    .iter()
                    .flat_map(|m| m.content().iter())
                    .map(|p| {
                        if let ContentPart::Text { text } = p {
                            text.len()
                        } else {
                            0
                        }
                    })
                    .sum();
                (total_chars / non_system.len().max(1) / 4).max(1)
            };
            let system_tokens: u64 = system_msgs
                .iter()
                .flat_map(|m| m.content().iter())
                .map(|p| {
                    if let ContentPart::Text { text } = p {
                        text.len() as u64 / 4
                    } else {
                        0
                    }
                })
                .sum();
            let budget = (*max).saturating_sub(system_tokens) as usize;
            budget / avg_tokens_per_msg
        }
    };

    let start = non_system.len().saturating_sub(target_keep);
    let mut result = system_msgs;
    result.extend_from_slice(&non_system[start..]);
    result
}

/// Keep only system messages and the single most recent non-system message.
///
/// This is the last-resort tier before declaring the context uncompactable.
fn aggressive_truncate(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let (system_msgs, non_system): (Vec<_>, Vec<_>) =
        messages.into_iter().partition(|m| m.role() == Role::System);

    let mut result = system_msgs;
    if let Some(last) = non_system.into_iter().last() {
        result.push(last);
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use futures::Stream;
    use uuid::Uuid;

    use swage_core::{
        CompletionRequest, CompletionResponse, ContentPart, FinishReason, ProviderError, Role,
        StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_storage::{SqliteStorage, Storage};

    use super::*;
    use crate::config::CompactionThreshold;

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    /// Provider that always returns a canned summary string.
    struct SummaryProvider {
        summary: String,
        requests: Mutex<Vec<CompletionRequest>>,
    }

    impl SummaryProvider {
        fn new(summary: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                summary: summary.into(),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for SummaryProvider {
        fn model_id(&self) -> &str {
            "summary-model"
        }

        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            self.requests.lock().expect("request lock").push(request);
            Ok(CompletionResponse {
                content: self.summary.clone(),
                input_tokens: 10,
                output_tokens: 20,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not called in compaction tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unimplemented!("not called in compaction tests")
        }
    }

    /// Provider that always fails — forces tier 1 to fall through.
    struct ErrorProvider;

    #[async_trait]
    impl LlmProvider for ErrorProvider {
        fn model_id(&self) -> &str {
            "error-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Err(ProviderError::InvalidResponse(
                "provider unavailable in test".into(),
            ))
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!()
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unimplemented!()
        }
    }

    #[derive(Clone, Copy)]
    enum CountOutcome {
        Count(u64),
        Unavailable,
        Failed,
    }

    /// Count-only provider that captures the exact request it receives.
    struct CapturingCountProvider {
        outcome: CountOutcome,
        count_requests: Mutex<Vec<CompletionRequest>>,
    }

    impl CapturingCountProvider {
        fn new(outcome: CountOutcome) -> Arc<Self> {
            Arc::new(Self {
                outcome,
                count_requests: Mutex::new(Vec::new()),
            })
        }
    }

    /// A provider that detects an initial oversized prompt but then loses its
    /// native count while summarisation is unavailable.
    struct TransientCountFailureProvider {
        count_calls: AtomicUsize,
    }

    impl TransientCountFailureProvider {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                count_calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for TransientCountFailureProvider {
        fn model_id(&self) -> &str {
            "transient-count-failure"
        }

        async fn count_tokens(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Option<u64>, ProviderError> {
            if self.count_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(Some(100))
            } else {
                Err(ProviderError::InvalidResponse(
                    "native count unavailable".into(),
                ))
            }
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Err(ProviderError::InvalidResponse(
                "summarisation unavailable".into(),
            ))
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("compaction does not request tools")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unreachable!("compaction does not stream")
        }
    }

    /// Provider whose first post-compaction native recount establishes a
    /// breach, then becomes unavailable while an image-only prompt remains.
    struct RecountBreachProvider {
        count_calls: AtomicUsize,
    }

    impl RecountBreachProvider {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                count_calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for RecountBreachProvider {
        fn model_id(&self) -> &str {
            "recount-breach"
        }

        async fn count_tokens(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Option<u64>, ProviderError> {
            match self.count_calls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(None),
                1 => Ok(Some(100)),
                _ => Err(ProviderError::InvalidResponse(
                    "native recount unavailable".into(),
                )),
            }
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Err(ProviderError::InvalidResponse(
                "summarisation unavailable".into(),
            ))
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("compaction does not request tools")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unreachable!("compaction does not stream")
        }
    }

    #[async_trait]
    impl LlmProvider for CapturingCountProvider {
        fn model_id(&self) -> &str {
            "counting-model"
        }

        async fn count_tokens(
            &self,
            request: &CompletionRequest,
        ) -> Result<Option<u64>, ProviderError> {
            self.count_requests
                .lock()
                .expect("count request lock")
                .push(request.clone());
            match self.outcome {
                CountOutcome::Count(count) => Ok(Some(count)),
                CountOutcome::Unavailable => Ok(None),
                CountOutcome::Failed => Err(ProviderError::InvalidResponse(
                    "counting unavailable in test".into(),
                )),
            }
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unreachable!("count-only provider")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("count-only provider")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unreachable!("count-only provider")
        }
    }

    // Helpers
    // -----------------------------------------------------------------------

    fn text_msg(role: Role, text: &str) -> ChatMessage {
        ChatMessage::new(role, text)
    }

    fn image_msg(role: Role, url: &str) -> ChatMessage {
        ChatMessage::new_parts(role, vec![ContentPart::ImageUrl { url: url.into() }])
    }

    fn stub_storage() -> Arc<dyn Storage> {
        Arc::new(SqliteStorage::in_memory().expect("test storage should initialise"))
    }

    // -----------------------------------------------------------------------
    // exceeds_threshold unit tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn below_message_count_threshold_returns_false() {
        let messages = vec![
            text_msg(Role::User, "hello"),
            text_msg(Role::Assistant, "hi there"),
        ];
        let provider = CapturingCountProvider::new(CountOutcome::Count(0));

        assert!(
            !exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::MessageCount(5),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
    }

    #[tokio::test]
    async fn at_message_count_threshold_returns_false() {
        // len == threshold is NOT exceeding (strictly greater than).
        let messages = vec![
            text_msg(Role::User, "a"),
            text_msg(Role::User, "b"),
            text_msg(Role::User, "c"),
        ];
        let provider = CapturingCountProvider::new(CountOutcome::Count(0));

        assert!(
            !exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::MessageCount(3),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
    }

    #[tokio::test]
    async fn above_message_count_threshold_returns_true() {
        let messages = vec![
            text_msg(Role::User, "a"),
            text_msg(Role::User, "b"),
            text_msg(Role::User, "c"),
            text_msg(Role::User, "d"),
        ];
        let provider = CapturingCountProvider::new(CountOutcome::Count(0));

        assert!(
            exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::MessageCount(3),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
    }

    #[tokio::test]
    async fn token_count_threshold_based_on_char_estimate() {
        // "hello world" = 11 chars -> ~2 tokens (11/4 = 2). Threshold of 1
        // should exceed; threshold of 10 should not.
        let messages = vec![text_msg(Role::User, "hello world")];
        let provider = CapturingCountProvider::new(CountOutcome::Unavailable);

        assert!(
            exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::TokenCount(1),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
        assert!(
            !exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::TokenCount(10),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );

        // The estimate sums all characters before dividing. Three short
        // messages therefore contain an estimated two tokens (9/4), rather
        // than zero tokens from flooring each message independently.
        let short_messages = vec![
            text_msg(Role::User, "abc"),
            text_msg(Role::Assistant, "abc"),
            text_msg(Role::User, "abc"),
        ];
        assert!(
            exceeds_threshold_with_model(
                &short_messages,
                &CompactionThreshold::TokenCount(1),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
    }

    #[tokio::test]
    async fn message_count_threshold_is_unchanged_and_never_counts() {
        let messages = vec![
            text_msg(Role::User, "hello"),
            text_msg(Role::Assistant, "hi there"),
        ];
        let provider = CapturingCountProvider::new(CountOutcome::Count(0));

        assert!(
            exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::MessageCount(1),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
        assert!(
            provider
                .count_requests
                .lock()
                .expect("count request lock")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn byte_bound_screen_skips_provider_counting() {
        let messages = vec![text_msg(Role::User, "small")];
        let provider = CapturingCountProvider::new(CountOutcome::Count(999));

        assert!(
            !exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::TokenCount(100),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
        assert!(
            provider
                .count_requests
                .lock()
                .expect("count request lock")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn token_count_uses_provider_request_and_decision_over_estimate() {
        // Eight characters estimate to two tokens, below this threshold; the
        // provider's real count is five, so the result proves it was consulted.
        let messages = vec![text_msg(Role::User, "abcdefgh")];
        let provider = CapturingCountProvider::new(CountOutcome::Count(5));

        assert!(
            exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::TokenCount(4),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );

        let requests = provider.count_requests.lock().expect("count request lock");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.messages, messages);
        assert!(request.model.is_none());
        assert!(request.effort.is_none());
        assert!(request.max_tokens.is_none());
        assert!(request.temperature.is_none());
        assert!(request.stop_sequences.is_none());
        assert!(request.metadata.is_none());
        assert!(request.tool_definitions.is_empty());
    }

    #[tokio::test]
    async fn unavailable_provider_counting_falls_back_to_estimate() {
        let messages = vec![text_msg(Role::User, "abcdefgh")];
        let provider = CapturingCountProvider::new(CountOutcome::Unavailable);

        assert!(
            exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::TokenCount(1),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
        assert_eq!(
            provider
                .count_requests
                .lock()
                .expect("count request lock")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn failed_provider_counting_falls_back_to_estimate() {
        let messages = vec![text_msg(Role::User, "abcdefgh")];
        let provider = CapturingCountProvider::new(CountOutcome::Failed);

        assert!(
            exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::TokenCount(1),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds
        );
        assert_eq!(
            provider
                .count_requests
                .lock()
                .expect("count request lock")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn counting_uses_the_selected_completion_model() {
        let messages = vec![text_msg(Role::User, "abcdefgh")];
        let provider = CapturingCountProvider::new(CountOutcome::Count(10));

        assert!(
            exceeds_threshold_with_model(
                &messages,
                &CompactionThreshold::TokenCount(1),
                provider.as_ref(),
                Some("selected-completion-model"),
                false,
            )
            .await
            .exceeds
        );
        let requests = provider.count_requests.lock().expect("count request lock");
        assert_eq!(
            requests[0].model.as_deref(),
            Some("selected-completion-model")
        );
    }

    // -----------------------------------------------------------------------
    // compact integration tests (async)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn compact_below_threshold_returns_unchanged() {
        let messages = vec![
            text_msg(Role::User, "first message"),
            text_msg(Role::Assistant, "response"),
        ];
        let original_count = messages.len();
        let storage = stub_storage();
        let provider = SummaryProvider::new("irrelevant");

        let result = compact_with_model(
            messages,
            &CompactionThreshold::MessageCount(10),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            false,
        )
        .await
        .expect("compact should succeed when below threshold");

        assert_eq!(result.0.len(), original_count);
        assert!(result.1.is_none());
    }

    #[tokio::test]
    async fn summarise_tier_replaces_old_messages_with_summary() {
        // 4 messages, threshold of 3 → tier 1 runs; oldest 2 replaced by summary.
        let messages = vec![
            text_msg(Role::User, "message one"),
            text_msg(Role::Assistant, "message two"),
            text_msg(Role::User, "message three"),
            text_msg(Role::Assistant, "message four"),
        ];
        let storage = stub_storage();
        let provider = SummaryProvider::new("Summary of the first two messages");

        let (result, record) = compact_with_model(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            false,
        )
        .await
        .expect("compact should succeed");

        let record = record.expect("compaction should emit an audit record");
        assert!(record.focus.is_none());
        assert_eq!(record.messages_removed, 2);
        let requests = provider.requests.lock().expect("request lock");
        let prompt = match &requests[0].messages[0].content()[0] {
            ContentPart::Text { text } => text,
            _ => panic!("expected text summarisation prompt"),
        };
        assert!(!prompt.contains("Focus instructions:"));

        // Summary message (system) + the 2 recent messages = 3 total.
        assert_eq!(result.len(), 3);
        // First message should be the system summary.
        assert_eq!(result[0].role(), Role::System);
        let summary_text = match &result[0].content()[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!("expected text content in summary"),
        };
        assert!(
            summary_text.contains("Summary of the first two messages"),
            "summary content not found in: {summary_text}"
        );
    }

    #[tokio::test]
    async fn compaction_focus_shapes_prompt_and_record() {
        let messages = vec![
            text_msg(Role::User, "old user detail"),
            text_msg(Role::Assistant, "old assistant detail"),
            text_msg(Role::User, "recent user detail"),
            text_msg(Role::Assistant, "recent assistant detail"),
        ];
        let storage = stub_storage();
        let provider = SummaryProvider::new("focused summary");

        let (_, record) = compact_with_model(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            Some("Preserve dates and named decisions"),
            None,
            false,
        )
        .await
        .expect("focused compaction should succeed");

        let record = record.expect("compaction should emit an audit record");
        let requests = provider.requests.lock().expect("request lock");
        let prompt = match &requests[0].messages[0].content()[0] {
            ContentPart::Text { text } => text,
            _ => panic!("expected text summarisation prompt"),
        };
        assert!(prompt.contains("Focus instructions:\nPreserve dates and named decisions"));
        assert_eq!(
            record.focus.as_deref(),
            Some("Preserve dates and named decisions")
        );
    }

    #[tokio::test]
    async fn summarise_failure_falls_through_to_truncate() {
        // 5 messages, threshold of 3, provider errors → tier 1 fails, tier 2 truncates.
        let messages = vec![
            text_msg(Role::User, "m1"),
            text_msg(Role::Assistant, "m2"),
            text_msg(Role::User, "m3"),
            text_msg(Role::Assistant, "m4"),
            text_msg(Role::User, "m5"),
        ];
        let storage = stub_storage();
        let provider = Arc::new(ErrorProvider);

        let (result, record) = compact_with_model(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            false,
        )
        .await
        .expect("compact should succeed via truncation fallback");

        let record = record.expect("truncation should emit an audit record");
        assert_eq!(record.actual_strategy, "truncation");
        assert_eq!(record.messages_removed, 2);

        assert!(
            !exceeds_threshold_with_model(
                &result,
                &CompactionThreshold::MessageCount(3),
                provider.as_ref(),
                None,
                false,
            )
            .await
            .exceeds,
            "result should be within threshold, got {} messages",
            result.len()
        );
    }

    #[tokio::test]
    async fn known_native_breach_is_not_cleared_when_recount_fails() {
        // Image parts have no byte upper bound, so no tier can reach a prompt
        // that is provably within the threshold and every check falls through
        // to the failing native recount.
        let messages = vec![
            image_msg(Role::User, "https://example.test/a"),
            image_msg(Role::Assistant, "https://example.test/b"),
            image_msg(Role::User, "https://example.test/c"),
            image_msg(Role::Assistant, "https://example.test/d"),
        ];
        let storage = stub_storage();
        let provider = TransientCountFailureProvider::new();

        let error = compact_with_model(
            messages,
            &CompactionThreshold::TokenCount(20),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            true,
        )
        .await
        .expect_err("a failed native recount cannot clear the known breach");

        assert!(matches!(error, AgentError::CompactionFailed(_)));
    }

    #[tokio::test]
    async fn post_compaction_native_breach_survives_a_later_failed_recount() {
        let messages = vec![
            text_msg(Role::User, &"a".repeat(100)),
            image_msg(Role::Assistant, "https://example.test/a"),
            image_msg(Role::User, "https://example.test/b"),
        ];
        let storage = stub_storage();
        let provider = RecountBreachProvider::new();

        let error = compact_with_model(
            messages,
            &CompactionThreshold::TokenCount(20),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            false,
        )
        .await
        .expect_err("a native breach established during compaction must remain latched");

        assert!(matches!(error, AgentError::CompactionFailed(_)));
    }

    #[tokio::test]
    async fn known_native_breach_is_cleared_when_compaction_proves_the_prompt_safe() {
        let messages = vec![
            text_msg(Role::User, &"a".repeat(100)),
            text_msg(Role::Assistant, &"b".repeat(100)),
            text_msg(Role::User, &"c".repeat(100)),
            text_msg(Role::Assistant, &"d".repeat(100)),
        ];
        let storage = stub_storage();
        let provider = SummaryProvider::new("brief");

        let (compacted, record) = compact_with_model(
            messages,
            &CompactionThreshold::TokenCount(300),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            true,
        )
        .await
        .expect("a text-only compacted prompt below the byte bound must not require recounting");

        assert_eq!(compacted.len(), 3);
        assert_eq!(
            record.expect("summarisation is recorded").actual_strategy,
            "summarisation"
        );
    }

    #[tokio::test]
    async fn failed_recount_without_native_breach_keeps_estimate_fallback() {
        let messages = vec![
            text_msg(Role::User, &"a".repeat(21)),
            text_msg(Role::Assistant, &"b".repeat(21)),
            text_msg(Role::User, &"c".repeat(21)),
            text_msg(Role::Assistant, &"d".repeat(21)),
        ];
        let storage = stub_storage();
        // SummaryProvider does not implement native token counting, so both
        // evaluations use the estimate fallback and never establish a breach.
        let provider = SummaryProvider::new("brief");

        let (result, record) = compact_with_model(
            messages,
            &CompactionThreshold::TokenCount(20),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            false,
        )
        .await
        .expect("the estimate fallback must remain available without a native breach");

        let record = record.expect("compaction records the summary that fit by estimate");
        assert_eq!(record.actual_strategy, "summarisation");
        assert_eq!(record.messages_removed, 2);
        assert!(record.tokens_before > 20);
        assert!(record.tokens_after <= 20);
        assert_eq!(result.len(), 3);
    }

    #[tokio::test]
    async fn all_tiers_exhausted_returns_compaction_failed() {
        // Threshold of 0 means any non-empty list exceeds it. The system message
        // is always preserved through truncation, so we'll still have at least
        // one message after all tiers → CompactionFailed.
        let messages = vec![
            text_msg(Role::System, "system instructions"),
            text_msg(Role::User, "some message"),
            text_msg(Role::Assistant, "some response"),
        ];
        let storage = stub_storage();
        let provider = Arc::new(ErrorProvider);

        let result = compact_with_model(
            messages,
            &CompactionThreshold::MessageCount(0),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
            None,
            false,
        )
        .await;

        match result {
            Err(AgentError::CompactionFailed(_)) => {}
            Ok((msgs, _)) => panic!(
                "expected CompactionFailed, got Ok with {} messages",
                msgs.len()
            ),
            Err(e) => panic!("expected CompactionFailed, got different error: {e}"),
        }
    }
}
