//! Context transfer policy evaluation for inter-agent delegation.
//!
//! Evaluates the configured context transfer policy at send time, assembling
//! the transferred content to include in the Delegate message payload.
//! The policy is resolved from CommunicationConfig: per-target overrides take
//! precedence, falling back to the default mode.

use swage_config::{ContextTransferConfig, ContextTransferField, ContextTransferMode};
use swage_core::{ChatMessage, CompletionRequest, ContentPart, Role};
use swage_provider::LlmProvider;

/// The resolved context to embed in a Delegate message payload.
///
/// Serialised as part of the message — the delegate message is self-contained
/// after context transfer evaluation.
#[derive(Debug, Clone, Default)]
pub struct TransferredContext {
    /// Context messages to include in the target agent's assembled context.
    /// Empty for mode=None, filtered for mode=Explicit, compacted for mode=Summary.
    pub messages: Vec<ChatMessage>,
}

/// Resolve the effective transfer policy for a specific target agent.
///
/// Checks the per-target map first, falls back to the default policy.
pub fn resolve_policy<'a>(
    config: &'a ContextTransferConfig,
    target_agent_id: &str,
) -> (
    &'a ContextTransferMode,
    &'a [ContextTransferField],
    Option<u32>,
) {
    if let Some(target_config) = config.targets.get(target_agent_id) {
        (
            &target_config.mode,
            &target_config.allowed_fields,
            target_config.summary_budget,
        )
    } else {
        (&config.mode, &config.allowed_fields, config.summary_budget)
    }
}

/// Evaluate the context transfer policy and produce the transferred context.
///
/// - **None**: returns empty context (task description only).
/// - **Explicit**: filters the sender's context messages, keeping only those
///   whose role/type matches an entry in `allowed_fields`. Recognised fields:
///   "system" (system messages), "user" (user messages), "assistant" (assistant messages).
/// - **Summary**: produces a compacted summary of the sender's context.
///   Currently falls back to None until swage-context compaction is wired
///   for cross-agent use. This is fail-open by design.
pub fn evaluate_transfer(
    mode: &ContextTransferMode,
    allowed_fields: &[ContextTransferField],
    _summary_budget: Option<u32>,
    sender_context: &[ChatMessage],
) -> TransferredContext {
    match mode {
        ContextTransferMode::None => TransferredContext::default(),

        ContextTransferMode::Explicit => {
            let messages = sender_context
                .iter()
                .filter(|msg| {
                    allowed_fields.iter().any(|field| {
                        matches!(
                            (field, &msg.role),
                            (ContextTransferField::System, Role::System)
                                | (ContextTransferField::User, Role::User)
                                | (ContextTransferField::Assistant, Role::Assistant)
                        )
                    })
                })
                .cloned()
                .collect();

            TransferredContext { messages }
        }

        ContextTransferMode::Summary => {
            // Summary mode requires an LLM provider — synchronous evaluate
            // cannot call the provider. Use evaluate_transfer_async instead.
            tracing::warn!(
                "context transfer summary mode requires async evaluation with an LLM provider; \
                 falling back to none in synchronous path"
            );
            TransferredContext::default()
        }
    }
}

/// Async variant of [`evaluate_transfer`] that supports summary mode.
///
/// When mode is `Summary`, uses the LLM provider to compact the sender's
/// context into a summary within the configured token budget. Falls back
/// to `None` gracefully if the provider call fails.
pub async fn evaluate_transfer_async(
    mode: &ContextTransferMode,
    allowed_fields: &[ContextTransferField],
    summary_budget: Option<u32>,
    sender_context: &[ChatMessage],
    provider: Option<&dyn LlmProvider>,
) -> TransferredContext {
    match mode {
        // None and Explicit don't need async — delegate to the sync version.
        ContextTransferMode::None | ContextTransferMode::Explicit => {
            evaluate_transfer(mode, allowed_fields, summary_budget, sender_context)
        }

        ContextTransferMode::Summary => {
            let Some(provider) = provider else {
                tracing::warn!(
                    "context transfer summary mode requested but no provider available; \
                     falling back to none"
                );
                return TransferredContext::default();
            };

            if sender_context.is_empty() {
                return TransferredContext::default();
            }

            // Collect text content from the sender's context for summarisation.
            let context_text: String = sender_context
                .iter()
                .flat_map(|m| m.content.iter())
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            // Target a max token output — use the configured budget or a
            // reasonable default. The budget is advisory (max_tokens hint).
            let max_tokens = summary_budget.unwrap_or(512) as u32;

            let request = CompletionRequest {
                messages: vec![ChatMessage::new(
                    Role::User,
                    format!(
                        "Summarise the following conversation context concisely, \
                         preserving key facts, decisions, and context that a \
                         collaborating agent would need:\n\n{context_text}"
                    ),
                )],
                model: None,
                max_tokens: Some(max_tokens),
                temperature: Some(0.3),
                stop_sequences: None,
                metadata: None,
            };

            match provider.complete(request).await {
                Ok(response) => {
                    let summary_message = ChatMessage::new(
                        Role::System,
                        format!(
                            "Context summary from delegating agent: {}",
                            response.content
                        ),
                    );
                    TransferredContext {
                        messages: vec![summary_message],
                    }
                }
                Err(e) => {
                    // Fail-open: log the error and return empty context rather
                    // than blocking the delegation entirely.
                    tracing::warn!(
                        error = %e,
                        "context transfer summary compaction failed; falling back to none"
                    );
                    TransferredContext::default()
                }
            }
        }
    }
}

/// Convenience: resolve policy from config and evaluate in one step.
pub fn transfer_context(
    config: &ContextTransferConfig,
    target_agent_id: &str,
    sender_context: &[ChatMessage],
) -> TransferredContext {
    let (mode, allowed_fields, summary_budget) = resolve_policy(config, target_agent_id);
    evaluate_transfer(mode, allowed_fields, summary_budget, sender_context)
}

/// Async convenience: resolve policy and evaluate with provider support.
pub async fn transfer_context_async(
    config: &ContextTransferConfig,
    target_agent_id: &str,
    sender_context: &[ChatMessage],
    provider: Option<&dyn LlmProvider>,
) -> TransferredContext {
    let (mode, allowed_fields, summary_budget) = resolve_policy(config, target_agent_id);
    evaluate_transfer_async(
        mode,
        allowed_fields,
        summary_budget,
        sender_context,
        provider,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;

    fn sample_context() -> Vec<ChatMessage> {
        vec![
            ChatMessage::new(Role::System, "You are a helpful assistant."),
            ChatMessage::new(Role::User, "Summarise the report."),
            ChatMessage::new(Role::Assistant, "Here is the summary..."),
        ]
    }

    #[test]
    fn mode_none_returns_empty() {
        let result = evaluate_transfer(&ContextTransferMode::None, &[], None, &sample_context());
        assert!(result.messages.is_empty());
    }

    #[test]
    fn mode_explicit_filters_by_role() {
        let result = evaluate_transfer(
            &ContextTransferMode::Explicit,
            &[ContextTransferField::System, ContextTransferField::User],
            None,
            &sample_context(),
        );
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, Role::System);
        assert_eq!(result.messages[1].role, Role::User);
    }

    #[test]
    fn mode_explicit_empty_fields_returns_empty() {
        let result =
            evaluate_transfer(&ContextTransferMode::Explicit, &[], None, &sample_context());
        assert!(result.messages.is_empty());
    }

    #[test]
    fn context_transfer_config_rejects_unknown_allowed_field() {
        let error = serde_json::from_str::<ContextTransferConfig>(
            r#"{"mode":"explicit","allowed_fields":["nonexistent"],"summary_budget":null,"targets":{}}"#,
        )
        .expect_err("unknown field should be rejected");
        assert!(error.to_string().contains("nonexistent"));
    }

    #[test]
    fn mode_summary_sync_falls_back_to_none() {
        let result = evaluate_transfer(
            &ContextTransferMode::Summary,
            &[],
            Some(1000),
            &sample_context(),
        );
        // Synchronous summary falls back to empty (needs async for provider).
        assert!(result.messages.is_empty());
    }

    // --- Async summary mode tests ---

    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;
    use swage_config::{ContextTransferField, ContextTransferTargetConfig};
    use swage_core::{
        CompletionRequest, CompletionResponse, ContentPart, FinishReason, ProviderError,
        StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;

    struct MockSummaryProvider {
        summary: String,
    }

    #[async_trait]
    impl LlmProvider for MockSummaryProvider {
        fn model_id(&self) -> &str {
            "mock-summary"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
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
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!()
        }
        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unimplemented!()
        }
    }

    struct ErrorProvider;

    #[async_trait]
    impl LlmProvider for ErrorProvider {
        fn model_id(&self) -> &str {
            "error"
        }
        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Err(ProviderError::InvalidResponse(
                "provider unavailable".into(),
            ))
        }
        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!()
        }
        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn mode_summary_async_produces_compacted_context() {
        let provider = MockSummaryProvider {
            summary: "The user asked about reports and the assistant provided a summary.".into(),
        };
        let result = evaluate_transfer_async(
            &ContextTransferMode::Summary,
            &[],
            Some(512),
            &sample_context(),
            Some(&provider),
        )
        .await;

        assert_eq!(
            result.messages.len(),
            1,
            "summary should produce one message"
        );
        assert_eq!(result.messages[0].role, Role::System);
        let text = match &result.messages[0].content[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert!(
            text.contains("reports"),
            "summary should contain provider's output"
        );
    }

    #[tokio::test]
    async fn mode_summary_async_no_provider_falls_back_to_none() {
        let result = evaluate_transfer_async(
            &ContextTransferMode::Summary,
            &[],
            Some(512),
            &sample_context(),
            None,
        )
        .await;
        assert!(
            result.messages.is_empty(),
            "no provider should fall back to empty"
        );
    }

    #[tokio::test]
    async fn mode_summary_async_provider_error_falls_back_to_none() {
        let provider = ErrorProvider;
        let result = evaluate_transfer_async(
            &ContextTransferMode::Summary,
            &[],
            Some(512),
            &sample_context(),
            Some(&provider),
        )
        .await;
        assert!(
            result.messages.is_empty(),
            "provider error should fall back to empty"
        );
    }

    #[tokio::test]
    async fn mode_summary_async_empty_context_returns_empty() {
        let provider = MockSummaryProvider {
            summary: "shouldn't be called".into(),
        };
        let result = evaluate_transfer_async(
            &ContextTransferMode::Summary,
            &[],
            Some(512),
            &[],
            Some(&provider),
        )
        .await;
        assert!(result.messages.is_empty());
    }

    #[tokio::test]
    async fn mode_none_async_delegates_to_sync() {
        let result = evaluate_transfer_async(
            &ContextTransferMode::None,
            &[],
            None,
            &sample_context(),
            None,
        )
        .await;
        assert!(result.messages.is_empty());
    }

    #[tokio::test]
    async fn mode_explicit_async_delegates_to_sync() {
        let result = evaluate_transfer_async(
            &ContextTransferMode::Explicit,
            &[ContextTransferField::User],
            None,
            &sample_context(),
            None,
        )
        .await;
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, Role::User);
    }

    #[test]
    fn resolve_policy_uses_per_target_override() {
        let config = ContextTransferConfig {
            mode: ContextTransferMode::None,
            allowed_fields: vec![],
            summary_budget: None,
            targets: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "agent-b".to_string(),
                    ContextTransferTargetConfig {
                        mode: ContextTransferMode::Explicit,
                        allowed_fields: vec![ContextTransferField::System],
                        summary_budget: None,
                    },
                );
                m
            },
        };

        let (mode, fields, _) = resolve_policy(&config, "agent-b");
        assert_eq!(*mode, ContextTransferMode::Explicit);
        assert_eq!(fields, &[ContextTransferField::System]);

        // Unspecified target falls back to default.
        let (mode, _, _) = resolve_policy(&config, "agent-c");
        assert_eq!(*mode, ContextTransferMode::None);
    }

    #[test]
    fn transfer_context_convenience() {
        let config = ContextTransferConfig {
            mode: ContextTransferMode::Explicit,
            allowed_fields: vec![ContextTransferField::System],
            summary_budget: None,
            targets: std::collections::HashMap::new(),
        };

        let result = transfer_context(&config, "any-agent", &sample_context());
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, Role::System);
    }
}
