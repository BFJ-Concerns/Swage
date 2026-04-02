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
use swage_provider::LlmProvider;
use swage_storage::Storage;

use crate::config::CompactionThreshold;
use crate::error::AgentError;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check whether the message history exceeds the compaction threshold.
///
/// Uses a rough 4-characters-per-token estimate for `TokenCount` thresholds.
pub fn exceeds_threshold(messages: &[ChatMessage], threshold: &CompactionThreshold) -> bool {
    match threshold {
        CompactionThreshold::TokenCount(max_tokens) => {
            let char_count: usize = messages
                .iter()
                .flat_map(|m| m.content.iter())
                .map(|part| match part {
                    ContentPart::Text { text } => text.len(),
                    // Images are treated as zero for the purposes of this estimate.
                    ContentPart::ImageUrl { .. } => 0,
                })
                .sum();
            // Rough estimate: ~4 characters per token.
            let estimated_tokens = (char_count / 4) as u64;
            estimated_tokens > *max_tokens
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
) -> Result<Vec<ChatMessage>, AgentError> {
    // Fast path: already within bounds.
    if !exceeds_threshold(&messages, threshold) {
        return Ok(messages);
    }

    // Tier 1 — Summarise: condense older half via an LLM call. Falls back
    // gracefully if the provider call fails.
    let messages = try_summarise(messages, provider).await;
    if !exceeds_threshold(&messages, threshold) {
        return Ok(messages);
    }

    // Tier 2 — Truncate: drop oldest non-system messages.
    let messages = truncate(messages, threshold);
    if !exceeds_threshold(&messages, threshold) {
        return Ok(messages);
    }

    // Tier 3 — Aggressive truncate: keep system messages + single most recent
    // message. All dropped messages are already persisted to storage.
    let messages = aggressive_truncate(messages);
    if !exceeds_threshold(&messages, threshold) {
        return Ok(messages);
    }

    Err(AgentError::CompactionFailed(
        "context still exceeds capacity after all compaction tiers".into(),
    ))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Attempt to summarise the older half of the message history with an LLM call.
///
/// Returns the compacted list on success, or the original unchanged list when
/// the provider call fails (allowing tier 2 to handle it).
async fn try_summarise(messages: Vec<ChatMessage>, provider: &dyn LlmProvider) -> Vec<ChatMessage> {
    if messages.len() < 2 {
        return messages;
    }

    let midpoint = messages.len() / 2;
    let old_messages = &messages[..midpoint];
    let new_messages = messages[midpoint..].to_vec();

    // Collect the text content from older messages for the summarisation prompt.
    let old_text: String = old_messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let summarise_request = CompletionRequest {
        messages: vec![ChatMessage::new(
            Role::User,
            format!(
                "Summarise the following conversation concisely, preserving key information:\n\n{old_text}"
            ),
        )],
        model: None,
        max_tokens: Some(512),
        temperature: Some(0.3),
        stop_sequences: None,
        metadata: None,
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

/// Remove the oldest non-system messages, keeping the most recent ones that
/// bring the total below the threshold. System messages are always preserved.
fn truncate(messages: Vec<ChatMessage>, threshold: &CompactionThreshold) -> Vec<ChatMessage> {
    let (system_msgs, non_system): (Vec<_>, Vec<_>) = messages
        .into_iter()
        .partition(|m| m.role == Role::System);

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
                    .flat_map(|m| m.content.iter())
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
                .flat_map(|m| m.content.iter())
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
    let (system_msgs, non_system): (Vec<_>, Vec<_>) = messages
        .into_iter()
        .partition(|m| m.role == Role::System);

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
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;
    use uuid::Uuid;

    use swage_core::{
        CompletionRequest, CompletionResponse, ContentPart, FinishReason, ProviderError, Role,
        StorageError, StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_storage::{
        Message, MessageStore, NewMessage, NewSession, NewThread, NewToolCall, NewTurn, Session,
        SessionStore, Thread, ThreadState, ThreadStore, ToolCallRecord, ToolCallStore, Turn,
        TurnState, TurnStore, ContextSnapshotStore,
    };

    use super::*;
    use crate::config::CompactionThreshold;

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    /// Provider that always returns a canned summary string.
    struct SummaryProvider {
        summary: String,
    }

    impl SummaryProvider {
        fn new(summary: impl Into<String>) -> Arc<Self> {
            Arc::new(Self { summary: summary.into() })
        }
    }

    #[async_trait]
    impl LlmProvider for SummaryProvider {
        fn model_id(&self) -> &str {
            "summary-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
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
            Err(ProviderError::InvalidResponse("provider unavailable in test".into()))
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

    /// Minimal storage stub — satisfies type bounds, never called.
    struct StubStorage;

    #[async_trait]
    impl SessionStore for StubStorage {
        async fn create_session(&self, _: NewSession) -> Result<Session, StorageError> {
            unimplemented!()
        }
        async fn get_session(&self, _: Uuid) -> Result<Option<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_by_project(&self, _: Uuid) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_sessions_by_agent_id(
            &self,
            _: &str,
        ) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn delete_session(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_activity(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl swage_storage::store::TeamStore for StubStorage {
        async fn create_team(
            &self,
            _: swage_storage::NewTeam,
        ) -> Result<swage_storage::Team, StorageError> {
            unimplemented!()
        }
        async fn get_team(&self, _: Uuid) -> Result<Option<swage_storage::Team>, StorageError> {
            unimplemented!()
        }
        async fn get_team_by_name(
            &self,
            _: Uuid,
            _: &str,
        ) -> Result<Option<swage_storage::Team>, StorageError> {
            unimplemented!()
        }
        async fn list_teams_by_project(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::Team>, StorageError> {
            unimplemented!()
        }
        async fn update_team(
            &self,
            _: Uuid,
            _: Option<String>,
            _: Option<String>,
            _: Option<String>,
            _: Option<Vec<String>>,
        ) -> Result<swage_storage::Team, StorageError> {
            unimplemented!()
        }
        async fn delete_team(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ThreadStore for StubStorage {
        async fn create(&self, _: NewThread) -> Result<Thread, StorageError> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_by_session(&self, _: Uuid) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_threads_by_agent_id(&self, _: &str) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn update_state(&self, _: Uuid, _: ThreadState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn delete(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn create_branch(
            &self,
            _: Uuid,
            _: Uuid,
        ) -> Result<swage_storage::ThreadBranch, StorageError> {
            unimplemented!()
        }
        async fn list_branches_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::ThreadBranch>, StorageError> {
            unimplemented!()
        }
        async fn get_branch(
            &self,
            _: Uuid,
        ) -> Result<Option<swage_storage::ThreadBranch>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TurnStore for StubStorage {
        async fn create_turn(&self, _: NewTurn) -> Result<Turn, StorageError> {
            unimplemented!()
        }
        async fn get_turn(&self, _: Uuid) -> Result<Option<Turn>, StorageError> {
            unimplemented!()
        }
        async fn list_turns_by_thread(&self, _: Uuid) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_turn_state(&self, _: Uuid, _: TurnState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn find_in_progress_by_agent(&self, _: &str) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_response(&self, _: Uuid, _: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_tokens(&self, _: Uuid, _: i64, _: i64) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_cost(&self, _: Uuid, _: i64) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_structured_output(&self, _: Uuid, _: &serde_json::Value) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl MessageStore for StubStorage {
        async fn create_message(&self, _: NewMessage) -> Result<Message, StorageError> {
            unimplemented!()
        }
        async fn list_messages_by_thread(&self, _: Uuid) -> Result<Vec<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_latest_for_session(
            &self,
            _: Uuid,
        ) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_message(&self, _: Uuid) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn delete_by_thread(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ToolCallStore for StubStorage {
        async fn create_tool_call(
            &self,
            _: NewToolCall,
        ) -> Result<ToolCallRecord, StorageError> {
            unimplemented!()
        }
        async fn get_tool_call(
            &self,
            _: Uuid,
        ) -> Result<Option<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
        async fn update_tool_call_result(
            &self,
            _: &str,
            _: Uuid,
            _: &str,
            _: &str,
            _: i64,
            _: bool,
            _: Option<i64>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_tool_calls_by_turn(
            &self,
            _: Uuid,
        ) -> Result<Vec<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
    }

    // -----------------------------------------------------------------------

    #[async_trait]
    impl ContextSnapshotStore for StubStorage {
        async fn store_snapshot(&self, _: Uuid, _: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn get_snapshot(&self, _: Uuid) -> Result<Option<String>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl swage_storage::store::SkillActivationStore for StubStorage {
        async fn activate_skill(&self, _: Uuid, _: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn deactivate_skill(&self, _: Uuid, _: &str) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn list_active_skills(&self, _: Uuid) -> Result<Vec<swage_storage::model::ActiveSkillRecord>, StorageError> {
            unimplemented!()
        }
        async fn clear_active_skills(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl swage_storage::store::TopologyTraceStore for StubStorage {
        async fn create_topology_run(
            &self,
            _: swage_storage::NewTopologyRun,
        ) -> Result<swage_storage::TopologyRun, StorageError> {
            unimplemented!()
        }
        async fn get_topology_run(
            &self,
            _: Uuid,
        ) -> Result<Option<swage_storage::TopologyRun>, StorageError> {
            unimplemented!()
        }
        async fn list_topology_runs_by_team(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::TopologyRun>, StorageError> {
            unimplemented!()
        }
        async fn list_topology_runs_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::TopologyRun>, StorageError> {
            unimplemented!()
        }
        async fn update_topology_run(
            &self,
            _: Uuid,
            _: swage_storage::TopologyRunState,
            _: Option<&serde_json::Value>,
            _: Option<&str>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn create_node_execution(
            &self,
            _: swage_storage::NewNodeExecution,
        ) -> Result<swage_storage::NodeExecution, StorageError> {
            unimplemented!()
        }
        async fn get_node_execution(
            &self,
            _: Uuid,
        ) -> Result<Option<swage_storage::NodeExecution>, StorageError> {
            unimplemented!()
        }
        async fn list_node_executions_by_run(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::NodeExecution>, StorageError> {
            unimplemented!()
        }
        async fn update_node_execution(
            &self,
            _: Uuid,
            _: swage_storage::NodeExecutionState,
            _: Option<&serde_json::Value>,
            _: Option<&str>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn create_edge_evaluation(
            &self,
            _: swage_storage::NewEdgeEvaluation,
        ) -> Result<swage_storage::EdgeEvaluation, StorageError> {
            unimplemented!()
        }
        async fn list_edge_evaluations_by_run(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::EdgeEvaluation>, StorageError> {
            unimplemented!()
        }
        async fn create_node_retry(
            &self,
            _: swage_storage::NewNodeRetry,
        ) -> Result<swage_storage::NodeRetry, StorageError> {
            unimplemented!()
        }
        async fn list_node_retries(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::NodeRetry>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl swage_storage::store::ApprovalStore for StubStorage {
        async fn create_approval(
            &self,
            _: swage_storage::model::NewApprovalRequest,
        ) -> Result<swage_storage::model::ApprovalRequest, StorageError> {
            unimplemented!()
        }
        async fn get_approval(
            &self,
            _: Uuid,
        ) -> Result<Option<swage_storage::model::ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_by_agent(
            &self,
            _: &str,
        ) -> Result<Vec<swage_storage::model::ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::model::ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_all_pending(
            &self,
        ) -> Result<Vec<swage_storage::model::ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn update_approval_state(
            &self,
            _: Uuid,
            _: swage_storage::model::ApprovalState,
            _: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError> {
            unimplemented!()
        }
        async fn remember_approval(
            &self,
            _: Uuid,
            _: &str,
            _: &str,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn has_approval_memory(
            &self,
            _: Uuid,
            _: &str,
            _: &str,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl swage_storage::store::AgentMessageStore for StubStorage {
        async fn create_agent_message(
            &self,
            _: swage_storage::model::NewAgentMessage,
        ) -> Result<swage_storage::model::AgentMessage, StorageError> {
            unimplemented!()
        }
        async fn get_agent_message(
            &self,
            _: Uuid,
        ) -> Result<Option<swage_storage::model::AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_for_agent(
            &self,
            _: &str,
        ) -> Result<Vec<swage_storage::model::AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn update_agent_message_status(
            &self,
            _: Uuid,
            _: swage_core::MessageStatus,
            _: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_agent_message_payload(
            &self,
            _: Uuid,
            _: serde_json::Value,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_by_correlation(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::model::AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn expire_stale_messages(
            &self,
            _: std::time::Duration,
        ) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    // Helpers
    // -----------------------------------------------------------------------

    fn text_msg(role: Role, text: &str) -> ChatMessage {
        ChatMessage::new(role, text)
    }

    fn stub_storage() -> Arc<StubStorage> {
        Arc::new(StubStorage)
    }

    // -----------------------------------------------------------------------
    // exceeds_threshold unit tests (synchronous)
    // -----------------------------------------------------------------------

    #[test]
    fn below_message_count_threshold_returns_false() {
        let messages = vec![
            text_msg(Role::User, "hello"),
            text_msg(Role::Assistant, "hi there"),
        ];
        assert!(!exceeds_threshold(&messages, &CompactionThreshold::MessageCount(5)));
    }

    #[test]
    fn at_message_count_threshold_returns_false() {
        // len == threshold is NOT exceeding (strictly greater than).
        let messages = vec![
            text_msg(Role::User, "a"),
            text_msg(Role::User, "b"),
            text_msg(Role::User, "c"),
        ];
        assert!(!exceeds_threshold(&messages, &CompactionThreshold::MessageCount(3)));
    }

    #[test]
    fn above_message_count_threshold_returns_true() {
        let messages = vec![
            text_msg(Role::User, "a"),
            text_msg(Role::User, "b"),
            text_msg(Role::User, "c"),
            text_msg(Role::User, "d"),
        ];
        assert!(exceeds_threshold(&messages, &CompactionThreshold::MessageCount(3)));
    }

    #[test]
    fn token_count_threshold_based_on_char_estimate() {
        // "hello world" = 11 chars → ~2 tokens (11/4 = 2). Threshold of 1 should
        // exceed; threshold of 10 should not.
        let messages = vec![text_msg(Role::User, "hello world")];
        assert!(exceeds_threshold(&messages, &CompactionThreshold::TokenCount(1)));
        assert!(!exceeds_threshold(&messages, &CompactionThreshold::TokenCount(10)));
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
        )
        .await
        .expect("compact should succeed when below threshold");

        assert_eq!(result.len(), original_count);
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

        let result = compact(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
        )
        .await
        .expect("compact should succeed");

        // Summary message (system) + the 2 recent messages = 3 total.
        assert_eq!(result.len(), 3);
        // First message should be the system summary.
        assert_eq!(result[0].role, Role::System);
        let summary_text = match &result[0].content[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!("expected text content in summary"),
        };
        assert!(
            summary_text.contains("Summary of the first two messages"),
            "summary content not found in: {summary_text}"
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

        let result = compact(
            messages,
            &CompactionThreshold::MessageCount(3),
            provider.as_ref(),
            storage.as_ref(),
            Uuid::new_v4(),
        )
        .await
        .expect("compact should succeed via truncation fallback");

        assert!(
            !exceeds_threshold(&result, &CompactionThreshold::MessageCount(3)),
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
        )
        .await;

        match result {
            Err(AgentError::CompactionFailed(_)) => {}
            Ok(msgs) => panic!("expected CompactionFailed, got Ok with {} messages", msgs.len()),
            Err(e) => panic!("expected CompactionFailed, got different error: {e}"),
        }
    }
}
