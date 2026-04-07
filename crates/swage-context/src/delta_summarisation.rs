//! Delta summarisation compaction strategy (experimental).
//!
//! An incremental LLM-backed strategy that processes only the messages added
//! since the last compaction, rather than re-extracting from the full history
//! on every pass.
//!
//! ## Strategy
//!
//! - **First use** (no prior result): runs full structured extraction on all
//!   messages. Behaviour identical to [`crate::structured_extraction::StructuredExtractionCompactor`].
//! - **Subsequent uses** (prior result available): slices the history at
//!   [`PriorCompactionResult::boundary_turn_index`], runs structured extraction
//!   on the delta only, and prepends the prior summary messages to the new
//!   output.
//!
//! This strategy is always marked experimental. The result carries
//! `actual_strategy = "delta_summarise"`.

use swage_core::{ChatMessage, Role};
use swage_provider::LlmProvider;

use crate::structured_extraction::StructuredExtractionCompactor;
use crate::tokeniser::Tokeniser;

// ─────────────────────────────────────────────────────────────────────────────
// Prior compaction result
// ─────────────────────────────────────────────────────────────────────────────

/// The summary content and turn boundary from a previous compaction pass.
///
/// Passed to [`DeltaSummarisationCompactor::new`] to enable incremental
/// extraction: only messages after `boundary_turn_index` are sent to the LLM.
#[derive(Debug, Clone)]
pub struct PriorCompactionResult {
    /// The messages produced by the previous compaction (typically one
    /// `Role::System` message containing a structured summary).
    pub content: Vec<ChatMessage>,

    /// The index in the original turn list at which the prior compaction
    /// ended. Messages at this index or later form the delta for the next pass.
    pub boundary_turn_index: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Output
// ─────────────────────────────────────────────────────────────────────────────

/// The output of a delta summarisation pass.
#[derive(Debug, Clone)]
pub struct DeltaCompactionOutput {
    /// The assembled output: prior summary (if any) followed by the delta
    /// extraction result.
    pub messages: Vec<ChatMessage>,

    /// Always `true` — this strategy is marked experimental.
    pub experimental: bool,

    /// Whether a prior compaction result was incorporated. `false` on the
    /// first use of the strategy in a conversation.
    pub prior_used: bool,

    /// Index in the original message slice where the delta began.
    /// `0` when no prior result was available.
    pub delta_start: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Compactor
// ─────────────────────────────────────────────────────────────────────────────

/// Experimental compactor that summarises only the messages added since the
/// last compaction, appending the result to the prior summary.
///
/// Construct with [`DeltaSummarisationCompactor::new`], passing an optional
/// [`PriorCompactionResult`] from the most recent previous pass.
pub struct DeltaSummarisationCompactor {
    prior: Option<PriorCompactionResult>,
}

impl DeltaSummarisationCompactor {
    /// Construct a compactor.
    ///
    /// - `prior = None` → first use; falls back to full structured extraction.
    /// - `prior = Some(p)` → incremental use; extracts only the delta.
    pub fn new(prior: Option<PriorCompactionResult>) -> Self {
        Self { prior }
    }

    /// Compact `messages` using delta summarisation.
    ///
    /// # First use (`prior = None`)
    ///
    /// Runs full structured extraction on all messages. The output contains a
    /// single system message with the complete summary.
    ///
    /// # Subsequent uses (`prior = Some`)
    ///
    /// Identifies messages from `prior.boundary_turn_index` onwards (the
    /// delta), runs structured extraction on those messages only, and
    /// prepends the prior summary. When there are no new messages (delta is
    /// empty), the prior content is returned unchanged — no LLM call is made.
    ///
    /// Returns `Err` if the LLM call fails or the response cannot be parsed.
    /// The caller is expected to fall back to truncation in that case.
    pub async fn compact(
        &self,
        messages: &[ChatMessage],
        budget: usize,
        provider: &dyn LlmProvider,
        tokeniser: &dyn Tokeniser,
    ) -> Result<DeltaCompactionOutput, String> {
        let extractor = StructuredExtractionCompactor::new();

        match &self.prior {
            None => {
                // First use: full extraction on all messages.
                let output = extractor
                    .extract(messages, budget, provider, tokeniser)
                    .await
                    .map_err(|e| format!("extraction failed: {e}"))?;

                let summary = ChatMessage::new(Role::System, &output.content);

                Ok(DeltaCompactionOutput {
                    messages: vec![summary],
                    experimental: true,
                    prior_used: false,
                    delta_start: 0,
                })
            }

            Some(prior) => {
                // Subsequent use: extract only the delta.
                let boundary = prior.boundary_turn_index.min(messages.len());
                let delta = &messages[boundary..];

                if delta.is_empty() {
                    // No new messages since the last compaction — return the
                    // prior content unchanged without any LLM call.
                    return Ok(DeltaCompactionOutput {
                        messages: prior.content.clone(),
                        experimental: true,
                        prior_used: true,
                        delta_start: boundary,
                    });
                }

                // Extract structured records from the delta only.
                let output = extractor
                    .extract(delta, budget, provider, tokeniser)
                    .await
                    .map_err(|e| format!("delta extraction failed: {e}"))?;

                // Prepend prior messages, then append the new delta summary.
                let mut result = prior.content.clone();
                result.push(ChatMessage::new(Role::System, &output.content));

                Ok(DeltaCompactionOutput {
                    messages: result,
                    experimental: true,
                    prior_used: true,
                    delta_start: boundary,
                })
            }
        }
    }
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

        /// Constructs a mock that returns an LLM error on every call.
        fn failing() -> Self {
            Self {
                response: String::new(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn model_id(&self) -> &str {
            "mock-delta"
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
            unimplemented!("not needed for delta summarisation tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            unimplemented!("not needed for delta summarisation tests")
        }
    }

    /// A mock provider that panics if `complete` is called — used to verify
    /// that the no-delta path skips the LLM.
    struct PanickingProvider;

    #[async_trait]
    impl LlmProvider for PanickingProvider {
        fn model_id(&self) -> &str {
            "mock-panicking"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            panic!("LLM should not be called when there is no delta")
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
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            unimplemented!()
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make(role: Role, text: &str) -> ChatMessage {
        ChatMessage::new(role, text)
    }

    fn tokeniser() -> TiktokenTokeniser {
        TiktokenTokeniser::new("gpt-4")
    }

    /// Valid extraction JSON that the mock provider returns.
    fn extraction_json() -> &'static str {
        r#"[{"name":"key_facts","items":[{"content":"Delta item","source_turns":[2]}]}]"#
    }

    fn message_text(msg: &ChatMessage) -> String {
        msg.content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    // ── First-use path: no prior result ──────────────────────────────────────

    #[tokio::test]
    async fn no_prior_runs_full_extraction_on_all_messages() {
        let provider = MockProvider::new(extraction_json());
        let t = tokeniser();
        let messages = vec![
            make(Role::User, "First message"),
            make(Role::Assistant, "First reply"),
            make(Role::User, "Second message"),
        ];

        let compactor = DeltaSummarisationCompactor::new(None);
        let result = compactor.compact(&messages, 1000, &provider, &t).await;

        assert!(result.is_ok(), "first-use extraction should succeed");
        let output = result.unwrap();

        // Always experimental.
        assert!(output.experimental, "output must be marked experimental");
        // No prior on first use.
        assert!(
            !output.prior_used,
            "prior_used should be false on first use"
        );
        // Output contains the extraction summary.
        assert!(
            !output.messages.is_empty(),
            "output must contain at least one message"
        );
        // Delta starts at the beginning when there is no prior boundary.
        assert_eq!(output.delta_start, 0, "delta_start must be 0 on first use");
        // The output message is a system message.
        assert_eq!(output.messages[0].role, Role::System);
    }

    // ── Subsequent-use path: prior result present ─────────────────────────────

    #[tokio::test]
    async fn with_prior_processes_only_new_messages() {
        let provider = MockProvider::new(extraction_json());
        let t = tokeniser();

        // Prior covers the first 2 messages; the third (index 2) is the delta.
        let prior_text = "## Prior Summary\n\nDecision: use Rust.";
        let prior = PriorCompactionResult {
            content: vec![make(Role::System, prior_text)],
            boundary_turn_index: 2,
        };

        let all_messages = vec![
            make(Role::User, "Old message 0"),
            make(Role::Assistant, "Old reply 1"),
            make(Role::User, "New message 2 — delta starts here"),
        ];

        let compactor = DeltaSummarisationCompactor::new(Some(prior));
        let result = compactor.compact(&all_messages, 1000, &provider, &t).await;

        assert!(result.is_ok(), "delta compaction with prior should succeed");
        let output = result.unwrap();

        assert!(output.experimental, "output must be marked experimental");
        assert!(
            output.prior_used,
            "prior_used must be true when prior was available"
        );
        assert_eq!(
            output.delta_start, 2,
            "delta_start must equal boundary_turn_index"
        );

        // Output must include both the prior summary and the new delta.
        assert!(
            output.messages.len() >= 2,
            "output must have prior summary + delta extraction"
        );

        // First message is the prior summary.
        let first_text = message_text(&output.messages[0]);
        assert!(
            first_text.contains("Prior Summary"),
            "first output message should be the prior summary"
        );

        // Second message is the new delta summary.
        let second_text = message_text(&output.messages[1]);
        assert!(
            !second_text.is_empty(),
            "second output message (delta) should not be empty"
        );
    }

    // ── Prior boundary at end of messages — no delta ──────────────────────────

    #[tokio::test]
    async fn prior_at_end_returns_prior_without_llm_call() {
        // PanickingProvider ensures no LLM call happens when there is no delta.
        let t = tokeniser();
        let prior_msg = make(Role::System, "## Prior\n\nAll known so far.");
        let prior = PriorCompactionResult {
            content: vec![prior_msg.clone()],
            // Boundary is at the end — no new messages.
            boundary_turn_index: 3,
        };

        let all_messages = vec![
            make(Role::User, "Old 0"),
            make(Role::Assistant, "Old 1"),
            make(Role::User, "Old 2"),
            // No message at index 3 — delta is empty.
        ];

        let compactor = DeltaSummarisationCompactor::new(Some(prior));
        let result = compactor
            .compact(&all_messages, 1000, &PanickingProvider, &t)
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.prior_used);
        assert!(output.experimental);
        // Prior returned unchanged — no new content appended.
        assert_eq!(output.messages.len(), 1);
        assert_eq!(message_text(&output.messages[0]), message_text(&prior_msg));
    }

    // ── LLM failure propagates as Err ─────────────────────────────────────────

    #[tokio::test]
    async fn llm_failure_returns_err_string() {
        let provider = MockProvider::failing();
        let t = tokeniser();
        let compactor = DeltaSummarisationCompactor::new(None);
        let messages = vec![make(Role::User, "hi")];
        let result = compactor.compact(&messages, 1000, &provider, &t).await;

        assert!(result.is_err(), "LLM failure should propagate as Err");
        let err = result.unwrap_err();
        assert!(!err.is_empty(), "error message should describe the failure");
    }

    // ── boundary_turn_index beyond message slice is clamped ───────────────────

    #[tokio::test]
    async fn boundary_beyond_message_count_treated_as_empty_delta() {
        // If the stored boundary is beyond the current message count (e.g.
        // messages were truncated externally), it is clamped — no panic.
        let t = tokeniser();
        let prior = PriorCompactionResult {
            content: vec![make(Role::System, "Prior summary.")],
            // 999 is way beyond the 2-message slice.
            boundary_turn_index: 999,
        };

        let messages = vec![
            make(Role::User, "Only message 0"),
            make(Role::Assistant, "Only reply 1"),
        ];

        let compactor = DeltaSummarisationCompactor::new(Some(prior));
        let result = compactor
            .compact(&messages, 1000, &PanickingProvider, &t)
            .await;

        // Clamped boundary → empty delta → prior returned unchanged.
        assert!(
            result.is_ok(),
            "out-of-range boundary should be handled safely"
        );
        let output = result.unwrap();
        assert_eq!(
            output.messages.len(),
            1,
            "only prior message should be returned"
        );
        assert!(output.prior_used);
    }
}
