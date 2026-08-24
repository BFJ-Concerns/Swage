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

/// Check whether the message history exceeds the compaction threshold.
///
/// Token thresholds first use a conservative byte bound, then ask the active
/// provider to count the exact prompt. Providers that cannot count, or whose
/// counting request fails, fall back to the existing character estimate.
pub async fn exceeds_threshold(
    messages: &[ChatMessage],
    threshold: &CompactionThreshold,
    provider: &dyn LlmProvider,
) -> bool {
    match threshold {
        CompactionThreshold::TokenCount(max_tokens) => {
            if token_count_byte_upper_bound(messages).is_some_and(|bound| bound <= *max_tokens) {
                return false;
            }

            match provider.count_tokens(&counting_request(messages)).await {
                Ok(Some(count)) => count > *max_tokens,
                Ok(None) | Err(_) => estimated_tokens(messages) > *max_tokens as usize,
            }
        }
        CompactionThreshold::MessageCount(max_count) => messages.len() > *max_count as usize,
    }
}

/// Compact the message history, applying tiers until context is within bounds.
///
/// Returns the compacted message list. Removed messages are already persisted
/// to storage from earlier writes — this function only trims the active context.
///
/// # Parameters
/// - `messages`: Current active message history.
/// - `threshold`: When context is considered too large.
/// - `provider`: LLM provider used for tier-1 summarisation.
/// - `_storage`: Currently unused — reserved for future tier-3 storage queries.
///   The underscore prefix signals intentional non-use to the compiler. Removed
///   messages already exist in the DB from earlier persistence calls.
/// - `_thread_id`: Currently unused — owning thread identifier, reserved for
///   scoped storage queries in future tier implementations.
pub async fn compact(
    messages: Vec<ChatMessage>,
    threshold: &CompactionThreshold,
    provider: &dyn LlmProvider,
    _storage: &dyn Storage,
    _thread_id: Uuid,
    focus: Option<&str>,
) -> Result<(Vec<ChatMessage>, Option<CompactionRecord>), AgentError> {
    let messages_before = messages.len();
    let tokens_before = estimated_tokens(&messages);
    // Fast path: already within bounds.
    if !exceeds_threshold(&messages, threshold, provider).await {
        return Ok((messages, None));
    }

    // Tier 1 — Summarise: condense older half via an LLM call. Falls back
    // gracefully if the provider call fails.
    let messages = try_summarise(messages, provider, focus).await;
    if !exceeds_threshold(&messages, threshold, provider).await {
        let tokens_after = estimated_tokens(&messages);
        let messages_removed = messages_before.saturating_sub(messages.len());
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
    let messages = truncate(messages, threshold);
    if !exceeds_threshold(&messages, threshold, provider).await {
        let tokens_after = estimated_tokens(&messages);
        let messages_removed = messages_before.saturating_sub(messages.len());
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
    let messages = aggressive_truncate(messages);
    if !exceeds_threshold(&messages, threshold, provider).await {
        let tokens_after = estimated_tokens(&messages);
        let messages_removed = messages_before.saturating_sub(messages.len());
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
/// empty; the provider's configured model remains the authority for tokenizer
/// selection.
fn counting_request(messages: &[ChatMessage]) -> CompletionRequest {
    CompletionRequest {
        messages: messages.to_vec(),
        model: None,
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
/// Returns the compacted list on success, or the original unchanged list when
/// the provider call fails (allowing tier 2 to handle it).
async fn try_summarise(
    messages: Vec<ChatMessage>,
    provider: &dyn LlmProvider,
    focus: Option<&str>,
) -> Vec<ChatMessage> {
    if messages.len() < 2 {
        return messages;
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
            compacted
        }
        // Summarisation failed — return unchanged so tier 2 can take over.
        Err(_) => messages,
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
    use std::sync::{Arc, Mutex};

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

    fn stub_storage() -> Arc<dyn Storage> {
        Arc::new(SqliteStorage::in_memory().expect("test storage should initialise"))
    }

    // -----------------------------------------------------------------------
    // exceeds_threshold unit tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn message_count_threshold_is_unchanged_and_never_counts() {
        let messages = vec![
            text_msg(Role::User, "hello"),
            text_msg(Role::Assistant, "hi there"),
        ];
        let provider = CapturingCountProvider::new(CountOutcome::Count(0));

        assert!(
            exceeds_threshold(
                &messages,
                &CompactionThreshold::MessageCount(1),
                provider.as_ref(),
            )
            .await
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
            !exceeds_threshold(
                &messages,
                &CompactionThreshold::TokenCount(100),
                provider.as_ref(),
            )
            .await
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
            exceeds_threshold(
                &messages,
                &CompactionThreshold::TokenCount(4),
                provider.as_ref(),
            )
            .await
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
            exceeds_threshold(
                &messages,
                &CompactionThreshold::TokenCount(1),
                provider.as_ref(),
            )
            .await
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
            exceeds_threshold(
                &messages,
                &CompactionThreshold::TokenCount(1),
                provider.as_ref(),
            )
            .await
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

        let result = compact(
            messages,
            &CompactionThreshold::MessageCount(10),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
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

        let (result, record) = compact(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
        )
        .await
        .expect("compact should succeed");

        let record = record.expect("compaction should emit an audit record");
        assert!(record.focus.is_none());
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

        let (_, record) = compact(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            Some("Preserve dates and named decisions"),
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

        let (result, _) = compact(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
        )
        .await
        .expect("compact should succeed via truncation fallback");

        assert!(
            !exceeds_threshold(
                &result,
                &CompactionThreshold::MessageCount(3),
                provider.as_ref(),
            )
            .await,
            "result should be within threshold, got {} messages",
            result.len()
        );
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

        let result = compact(
            messages,
            &CompactionThreshold::MessageCount(0),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
            None,
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
