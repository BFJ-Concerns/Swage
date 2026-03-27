//! Context transfer policy evaluation for inter-agent delegation.
//!
//! Evaluates the configured context transfer policy at send time, assembling
//! the transferred content to include in the Delegate message payload.
//! The policy is resolved from CommunicationConfig: per-target overrides take
//! precedence, falling back to the default mode.

use swage_config::{
    ContextTransferConfig, ContextTransferMode, ContextTransferTargetConfig,
};
use swage_core::ChatMessage;

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
) -> (&'a ContextTransferMode, &'a [String], Option<u32>) {
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
    allowed_fields: &[String],
    _summary_budget: Option<u32>,
    sender_context: &[ChatMessage],
) -> TransferredContext {
    match mode {
        ContextTransferMode::None => TransferredContext::default(),

        ContextTransferMode::Explicit => {
            // Filter context messages by allowed field names.
            // Fields map to ChatMessage roles for now. Silently omit unknown fields.
            let messages = sender_context
                .iter()
                .filter(|msg| {
                    let role_str = msg.role.to_string().to_lowercase();
                    allowed_fields.iter().any(|f| f.to_lowercase() == role_str)
                })
                .cloned()
                .collect();

            TransferredContext { messages }
        }

        ContextTransferMode::Summary => {
            // Summary mode requires the compaction engine. For now, fall back
            // to None with a warning logged — fail-open by design.
            tracing::warn!(
                "context transfer summary mode not yet wired to compaction; falling back to none"
            );
            TransferredContext::default()
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
        let result = evaluate_transfer(
            &ContextTransferMode::None,
            &[],
            None,
            &sample_context(),
        );
        assert!(result.messages.is_empty());
    }

    #[test]
    fn mode_explicit_filters_by_role() {
        let result = evaluate_transfer(
            &ContextTransferMode::Explicit,
            &["system".to_string(), "user".to_string()],
            None,
            &sample_context(),
        );
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, Role::System);
        assert_eq!(result.messages[1].role, Role::User);
    }

    #[test]
    fn mode_explicit_empty_fields_returns_empty() {
        let result = evaluate_transfer(
            &ContextTransferMode::Explicit,
            &[],
            None,
            &sample_context(),
        );
        assert!(result.messages.is_empty());
    }

    #[test]
    fn mode_explicit_unknown_fields_silently_ignored() {
        let result = evaluate_transfer(
            &ContextTransferMode::Explicit,
            &["nonexistent".to_string()],
            None,
            &sample_context(),
        );
        assert!(result.messages.is_empty());
    }

    #[test]
    fn mode_summary_falls_back_to_none() {
        let result = evaluate_transfer(
            &ContextTransferMode::Summary,
            &[],
            Some(1000),
            &sample_context(),
        );
        // Summary mode falls back to empty until compaction is wired.
        assert!(result.messages.is_empty());
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
                        allowed_fields: vec!["system".to_string()],
                        summary_budget: None,
                    },
                );
                m
            },
        };

        let (mode, fields, _) = resolve_policy(&config, "agent-b");
        assert_eq!(*mode, ContextTransferMode::Explicit);
        assert_eq!(fields, &["system".to_string()]);

        // Unspecified target falls back to default.
        let (mode, _, _) = resolve_policy(&config, "agent-c");
        assert_eq!(*mode, ContextTransferMode::None);
    }

    #[test]
    fn transfer_context_convenience() {
        let config = ContextTransferConfig {
            mode: ContextTransferMode::Explicit,
            allowed_fields: vec!["system".to_string()],
            summary_budget: None,
            targets: std::collections::HashMap::new(),
        };

        let result = transfer_context(&config, "any-agent", &sample_context());
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, Role::System);
    }
}
