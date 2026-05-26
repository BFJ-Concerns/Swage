//! Selective eviction compaction strategy (experimental).
//!
//! An LLM-backed strategy that removes specific low-value content from
//! conversation history rather than compressing it. The model identifies
//! candidates for removal; the compactor removes them and annotates what
//! was evicted and why.
//!
//! ## Eviction categories
//!
//! | Category | Examples |
//! |----------|---------|
//! | `failed_tool_call` | Tool called, errored, conversation moved on |
//! | `superseded_reasoning` | Agent explored an approach then abandoned it |
//! | `redundant_clarification` | Clarifying question answered elsewhere |
//!
//! ## Conservative by design
//!
//! When the LLM is uncertain whether content should be removed, it is kept.
//! Invalid indices and malformed candidates are silently skipped. The
//! strategy never removes system messages.
//!
//! This strategy is always marked experimental. The result carries
//! `actual_strategy = "selective_evict"`.

use serde::{Deserialize, Serialize};
use swage_core::{ChatMessage, CompletionRequest, Role};
use swage_provider::LlmProvider;

use crate::tokeniser::Tokeniser;

// ─────────────────────────────────────────────────────────────────────────────
// Eviction record
// ─────────────────────────────────────────────────────────────────────────────

/// A record of a single message that was evicted from the history.
///
/// Included in [`SelectiveEvictionOutput::evicted`] for report annotation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvictionAnnotation {
    /// Position of the removed message in the original (pre-eviction) slice.
    pub original_index: usize,

    /// Human-readable explanation from the LLM for why this message was evicted.
    pub reason: String,

    /// Eviction category: `"failed_tool_call"`, `"superseded_reasoning"`,
    /// or `"redundant_clarification"`.
    pub category: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Output
// ─────────────────────────────────────────────────────────────────────────────

/// The output of a selective eviction pass.
#[derive(Debug, Clone)]
pub struct SelectiveEvictionOutput {
    /// The original messages minus any evicted content, in their original order.
    pub messages: Vec<ChatMessage>,

    /// Annotations listing what was removed and why.
    pub evicted: Vec<EvictionAnnotation>,

    /// Always `true` — this strategy is marked experimental.
    pub experimental: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal LLM response type
// ─────────────────────────────────────────────────────────────────────────────

/// A single eviction candidate returned by the LLM.
#[derive(Debug, Deserialize)]
struct EvictionCandidate {
    /// Index into the original message slice to remove.
    index: usize,
    /// Why this message should be removed.
    reason: String,
    /// Category of eviction.
    category: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Compactor
// ─────────────────────────────────────────────────────────────────────────────

/// Experimental compactor that removes specific low-value messages identified
/// by an LLM, rather than compressing or summarising the history.
///
/// Construct with [`SelectiveEvictionCompactor::new`] and call [`SelectiveEvictionCompactor::compact`].
pub struct SelectiveEvictionCompactor;

impl SelectiveEvictionCompactor {
    /// Construct a compactor. Currently stateless; reserved for future options.
    pub fn new() -> Self {
        Self
    }

    /// Identify and remove low-value messages from `messages`.
    ///
    /// Sends `messages` to the LLM with an eviction prompt. The model returns
    /// a list of candidate indices with reasons; the compactor removes them
    /// from the message list. System messages are never removed regardless of
    /// what the LLM suggests.
    ///
    /// When the LLM returns no candidates, `messages` is returned unchanged.
    /// Out-of-range or duplicate indices are silently skipped (conservative).
    ///
    /// Returns `Err` if the LLM call fails or the response cannot be parsed.
    /// The caller is expected to fall back to truncation in that case.
    pub async fn compact(
        &self,
        messages: &[ChatMessage],
        provider: &dyn LlmProvider,
        tokeniser: &dyn Tokeniser,
    ) -> Result<SelectiveEvictionOutput, String> {
        let prompt = build_eviction_prompt(messages);

        let request = CompletionRequest {
            messages: vec![ChatMessage::new(Role::System, &prompt)],
            model: None,
            // A modest budget — we only want a JSON list of indices, not prose.
            max_tokens: Some(512),
            temperature: Some(0.0),
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };

        let response = provider
            .complete(request)
            .await
            .map_err(|e| format!("eviction LLM call failed: {e}"))?;

        let candidates = parse_eviction_response(&response.content)
            .map_err(|e| format!("failed to parse eviction response: {e}"))?;

        let annotations = apply_evictions(messages, &candidates);

        // Build the filtered message list.
        let evicted_indices: std::collections::HashSet<usize> =
            annotations.iter().map(|a| a.original_index).collect();

        let retained: Vec<ChatMessage> = messages
            .iter()
            .enumerate()
            .filter(|(i, _)| !evicted_indices.contains(i))
            .map(|(_, m)| m.clone())
            .collect();

        // Count tokens of the retained set for the caller to verify savings.
        let _retained_tokens = tokeniser.count_messages(&retained);

        Ok(SelectiveEvictionOutput {
            messages: retained,
            evicted: annotations,
            experimental: true,
        })
    }
}

impl Default for SelectiveEvictionCompactor {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Prompt construction
// ─────────────────────────────────────────────────────────────────────────────

/// Build the eviction prompt from conversation messages.
///
/// Numbers each message for index reference, then instructs the LLM to
/// return a JSON array of eviction candidates. The instruction emphasises
/// conservative removal — uncertain content must be kept.
fn build_eviction_prompt(messages: &[ChatMessage]) -> String {
    let mut prompt = String::from(
        "Identify low-value messages in the following conversation that can safely be \
         removed without losing important context. Only identify messages that clearly \
         fall into one of these categories:\n\n\
         - failed_tool_call: A tool was called, it errored, and the conversation moved on.\n\
         - superseded_reasoning: The agent explored an approach then abandoned it for a different one.\n\
         - redundant_clarification: A clarifying question that was already answered elsewhere.\n\n\
         BE CONSERVATIVE: when in doubt, do NOT include the message. Never include system \
         messages. Never include the most recent user or assistant turn.\n\n\
         Respond with a JSON array of objects with fields: \
         \"index\" (number), \"reason\" (string), \"category\" (string). \
         Respond with an empty array [] if nothing should be removed.\n\n\
         Example:\n\
         [{\"index\": 3, \"reason\": \"Tool call failed with error, conversation moved on\", \
         \"category\": \"failed_tool_call\"}]\n\n\
         Conversation:\n\n",
    );

    for (i, message) in messages.iter().enumerate() {
        let role_label = match message.role() {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let text = message
            .content()
            .iter()
            .filter_map(|part| match part {
                swage_core::ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        prompt.push_str(&format!("[{i}] ({role_label}): {text}\n"));
    }

    prompt.push_str("\nJSON output:");
    prompt
}

// ─────────────────────────────────────────────────────────────────────────────
// Response parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the LLM's JSON response into eviction candidates.
///
/// Tolerates markdown code fences. Returns an empty list if the LLM correctly
/// responds with `[]`. Returns `Err` only on malformed JSON.
fn parse_eviction_response(response: &str) -> Result<Vec<EvictionCandidate>, String> {
    let json = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response.trim());
    let json = json.strip_suffix("```").unwrap_or(json).trim();

    serde_json::from_str::<Vec<EvictionCandidate>>(json)
        .map_err(|e| format!("expected JSON array of eviction candidates: {e}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Eviction application
// ─────────────────────────────────────────────────────────────────────────────

/// Convert raw LLM candidates into validated [`EvictionAnnotation`]s.
///
/// Conservative filtering applied here:
/// - System messages are never evicted.
/// - Out-of-range indices are silently skipped.
/// - Duplicate indices are deduplicated (keep first occurrence).
fn apply_evictions(
    messages: &[ChatMessage],
    candidates: &[EvictionCandidate],
) -> Vec<EvictionAnnotation> {
    let mut seen = std::collections::HashSet::new();
    let mut annotations = Vec::new();

    for candidate in candidates {
        let idx = candidate.index;

        // Bounds check.
        if idx >= messages.len() {
            continue;
        }
        // System messages are never evicted.
        if messages[idx].role() == Role::System {
            continue;
        }
        // Deduplicate.
        if !seen.insert(idx) {
            continue;
        }

        annotations.push(EvictionAnnotation {
            original_index: idx,
            reason: candidate.reason.clone(),
            category: candidate.category.clone(),
        });
    }

    // Sort by original index for deterministic, readable reports.
    annotations.sort_by_key(|a| a.original_index);
    annotations
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::pin::Pin;
    use swage_core::{
        ChatMessage, CompletionRequest, CompletionResponse, ContentPart, FinishReason,
        ProviderError, Role, StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;

    use crate::tokeniser::TiktokenTokeniser;

    // ── Mock provider ─────────────────────────────────────────────────────────

    struct MockProvider {
        response: String,
    }

    impl MockProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
            }
        }

        fn failing() -> Self {
            Self {
                response: String::new(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn model_id(&self) -> &str {
            "mock-eviction"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            if self.response.is_empty() {
                return Err(ProviderError::Network {
                    provider: "mock".to_string(),
                    message: "simulated failure".to_string(),
                });
            }
            Ok(CompletionResponse {
                content: self.response.clone(),
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
            unimplemented!("not needed for eviction tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            unimplemented!("not needed for eviction tests")
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make(role: Role, text: &str) -> ChatMessage {
        match role {
            Role::Tool => ChatMessage::tool_result("call_test", text),
            Role::System | Role::User | Role::Assistant => ChatMessage::new(role, text),
        }
    }

    fn tokeniser() -> TiktokenTokeniser {
        TiktokenTokeniser::new("gpt-4")
    }

    fn message_text(msg: &ChatMessage) -> String {
        msg.content()
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    // ── Failed tool call is evicted ───────────────────────────────────────────

    #[tokio::test]
    async fn failed_tool_call_is_removed_from_output() {
        // Message at index 2 is a failed tool call — LLM identifies it.
        let evict_json = r#"[{"index":2,"reason":"Tool call errored, conversation moved on","category":"failed_tool_call"}]"#;
        let provider = MockProvider::new(evict_json);
        let t = tokeniser();

        let messages = vec![
            make(Role::User, "Can you fetch the weather?"),
            make(Role::Assistant, "I'll call the weather tool."),
            make(Role::Tool, "ERROR: weather API unavailable"), // index 2 — evicted
            make(Role::User, "Never mind, just estimate."),
            make(Role::Assistant, "Sure, it's probably warm."),
        ];

        let compactor = SelectiveEvictionCompactor::new();
        let result = compactor.compact(&messages, &provider, &t).await;

        assert!(result.is_ok(), "eviction should succeed");
        let output = result.unwrap();

        assert!(output.experimental, "output must be marked experimental");

        // The failed tool call (index 2) must be gone.
        assert_eq!(
            output.messages.len(),
            4,
            "one message should have been evicted"
        );
        assert!(
            output
                .messages
                .iter()
                .all(|m| { m.role() != Role::Tool || !message_text(m).contains("ERROR") }),
            "the failed tool call message must not appear in output"
        );

        // Report must annotate the eviction.
        assert_eq!(output.evicted.len(), 1, "one eviction should be recorded");
        assert_eq!(output.evicted[0].original_index, 2);
        assert_eq!(output.evicted[0].category, "failed_tool_call");
        assert!(!output.evicted[0].reason.is_empty());
    }

    // ── No evictions when LLM returns empty list ──────────────────────────────

    #[tokio::test]
    async fn empty_eviction_list_returns_messages_unchanged() {
        let provider = MockProvider::new("[]");
        let t = tokeniser();

        let messages = vec![make(Role::User, "Hello"), make(Role::Assistant, "Hi there")];
        let original_count = messages.len();

        let compactor = SelectiveEvictionCompactor::new();
        let result = compactor.compact(&messages, &provider, &t).await;

        assert!(result.is_ok());
        let output = result.unwrap();

        assert_eq!(
            output.messages.len(),
            original_count,
            "no messages should be removed"
        );
        assert!(output.evicted.is_empty(), "eviction list should be empty");
        assert!(output.experimental);
    }

    // ── System messages are never evicted ─────────────────────────────────────

    #[tokio::test]
    async fn system_messages_are_never_evicted() {
        // LLM incorrectly suggests evicting the system message at index 0.
        // The compactor must ignore this — conservative behaviour.
        let evict_json =
            r#"[{"index":0,"reason":"Possibly redundant","category":"redundant_clarification"}]"#;
        let provider = MockProvider::new(evict_json);
        let t = tokeniser();

        let messages = vec![
            make(Role::System, "You are a helpful assistant."), // must never be removed
            make(Role::User, "Hello"),
            make(Role::Assistant, "Hi"),
        ];

        let compactor = SelectiveEvictionCompactor::new();
        let result = compactor.compact(&messages, &provider, &t).await;

        assert!(result.is_ok());
        let output = result.unwrap();

        // System message must remain.
        assert!(
            output.messages.iter().any(|m| m.role() == Role::System),
            "system message must never be evicted"
        );
        assert_eq!(
            output.messages.len(),
            3,
            "system message protection means nothing was actually evicted"
        );
        // Nothing in the eviction report either.
        assert!(output.evicted.is_empty(), "no valid evictions occurred");
    }

    // ── Out-of-range index is silently ignored ────────────────────────────────

    #[tokio::test]
    async fn out_of_range_index_is_ignored_safely() {
        // LLM suggests evicting index 99 but there are only 2 messages.
        let evict_json =
            r#"[{"index":99,"reason":"Old message","category":"superseded_reasoning"}]"#;
        let provider = MockProvider::new(evict_json);
        let t = tokeniser();

        let messages = vec![
            make(Role::User, "Message 0"),
            make(Role::Assistant, "Reply 1"),
        ];

        let compactor = SelectiveEvictionCompactor::new();
        let result = compactor.compact(&messages, &provider, &t).await;

        assert!(result.is_ok());
        let output = result.unwrap();

        // Out-of-range index silently skipped — messages unchanged.
        assert_eq!(output.messages.len(), 2);
        assert!(
            output.evicted.is_empty(),
            "invalid index should not produce an annotation"
        );
    }

    // ── Multiple evictions are handled correctly ──────────────────────────────

    #[tokio::test]
    async fn multiple_evictions_remove_correct_messages() {
        // Evict indices 1 and 3 (zero-indexed: second and fourth messages).
        let evict_json = r#"[
            {"index":1,"reason":"Abandoned approach","category":"superseded_reasoning"},
            {"index":3,"reason":"Tool errored","category":"failed_tool_call"}
        ]"#;
        let provider = MockProvider::new(evict_json);
        let t = tokeniser();

        let messages = vec![
            make(Role::User, "Msg 0 — keep"),
            make(Role::Assistant, "Msg 1 — evict"),
            make(Role::User, "Msg 2 — keep"),
            make(Role::Tool, "Msg 3 — evict"),
            make(Role::Assistant, "Msg 4 — keep"),
        ];

        let compactor = SelectiveEvictionCompactor::new();
        let result = compactor.compact(&messages, &provider, &t).await;

        assert!(result.is_ok());
        let output = result.unwrap();

        assert_eq!(output.messages.len(), 3, "two messages should be evicted");
        assert_eq!(output.evicted.len(), 2, "two eviction annotations expected");

        // Remaining messages should be indices 0, 2, 4.
        let texts: Vec<String> = output.messages.iter().map(message_text).collect();
        assert!(texts[0].contains("Msg 0"));
        assert!(texts[1].contains("Msg 2"));
        assert!(texts[2].contains("Msg 4"));

        // Annotations sorted by original index.
        assert_eq!(output.evicted[0].original_index, 1);
        assert_eq!(output.evicted[1].original_index, 3);
    }

    // ── LLM failure propagates as Err ─────────────────────────────────────────

    #[tokio::test]
    async fn llm_failure_returns_err_string() {
        let provider = MockProvider::failing();
        let t = tokeniser();
        let compactor = SelectiveEvictionCompactor::new();
        let messages = vec![make(Role::User, "hi")];

        let result = compactor.compact(&messages, &provider, &t).await;

        assert!(result.is_err(), "LLM failure should propagate as Err");
        assert!(!result.unwrap_err().is_empty());
    }

    // ── Unit: apply_evictions conservatism ────────────────────────────────────

    #[test]
    fn apply_evictions_deduplicates_candidates() {
        let messages = vec![make(Role::User, "Msg 0"), make(Role::Assistant, "Msg 1")];
        let candidates = vec![
            EvictionCandidate {
                index: 1,
                reason: "First".into(),
                category: "c".into(),
            },
            EvictionCandidate {
                index: 1,
                reason: "Duplicate".into(),
                category: "c".into(),
            },
        ];
        let result = apply_evictions(&messages, &candidates);
        // Duplicate index 1 should appear only once.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].original_index, 1);
        assert_eq!(result[0].reason, "First");
    }

    #[test]
    fn apply_evictions_result_sorted_by_original_index() {
        let messages = (0..5)
            .map(|i| make(Role::User, &format!("Msg {i}")))
            .collect::<Vec<_>>();
        // Candidates in reverse order.
        let candidates = vec![
            EvictionCandidate {
                index: 4,
                reason: "r".into(),
                category: "c".into(),
            },
            EvictionCandidate {
                index: 1,
                reason: "r".into(),
                category: "c".into(),
            },
            EvictionCandidate {
                index: 2,
                reason: "r".into(),
                category: "c".into(),
            },
        ];
        let result = apply_evictions(&messages, &candidates);
        let indices: Vec<usize> = result.iter().map(|a| a.original_index).collect();
        assert_eq!(
            indices,
            vec![1, 2, 4],
            "annotations must be sorted by index"
        );
    }

    // ── Unit: parse_eviction_response handles code fences ─────────────────────

    #[test]
    fn parse_handles_markdown_code_fence() {
        let response = "```json\n[{\"index\":0,\"reason\":\"test\",\"category\":\"cat\"}]\n```";
        let result = parse_eviction_response(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn parse_accepts_empty_array() {
        let result = parse_eviction_response("[]");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn parse_returns_err_on_invalid_json() {
        let result = parse_eviction_response("not json");
        assert!(result.is_err());
    }
}
