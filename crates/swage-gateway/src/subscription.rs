//! Per-connection subscription state for the WebSocket observability protocol.
//!
//! Clients opt in to extended event streams by sending `Subscribe` events with
//! channel names. `SubscriptionState` tracks those subscriptions and applies
//! the gating rules that determine which `ServerEvent` variants are delivered
//! to a given connection.

use std::{collections::HashSet, str::FromStr};

use crate::websocket::ServerEvent;

// ---------------------------------------------------------------------------
// ObservabilityChannel
// ---------------------------------------------------------------------------

/// An observability channel that clients can subscribe to.
///
/// Each channel gates a distinct class of extended events. Clients that do
/// not subscribe to a channel receive only the baseline event stream
/// (core turn lifecycle plus always-delivered skill, approval, delegation,
/// topology, and terminal error events).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObservabilityChannel {
    /// Context assembly reports and token-usage-enriched turn completions.
    Context,
    /// Tool execution start and end events.
    Tools,
}

impl FromStr for ObservabilityChannel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "context" => Ok(Self::Context),
            "tools" => Ok(Self::Tools),
            // Unknown channel names are intentionally rejected here so callers
            // can silently ignore them rather than surfacing errors to clients.
            _ => Err(()),
        }
    }
}

// ---------------------------------------------------------------------------
// SubscriptionState
// ---------------------------------------------------------------------------

/// Per-connection subscription state for the observability protocol.
///
/// Tracks which channels a WebSocket connection has opted into and applies
/// gating rules via [`should_deliver`] to decide whether each `ServerEvent`
/// should be forwarded to that connection.
///
/// Construct with [`SubscriptionState::new`]. When `config_force_on` is
/// `true` (enabled via the observability force-on configuration option),
/// all channels are treated as subscribed regardless of explicit
/// subscribe/unsubscribe calls.
pub struct SubscriptionState {
    subscribed: HashSet<ObservabilityChannel>,
    /// When true, behave as if all channels are subscribed.
    force_on: bool,
}

impl SubscriptionState {
    /// Create a new subscription state.
    ///
    /// `config_force_on` enables force-on mode: every channel is considered
    /// subscribed, bypassing individual subscribe/unsubscribe calls.
    pub fn new(config_force_on: bool) -> Self {
        Self {
            subscribed: HashSet::new(),
            force_on: config_force_on,
        }
    }

    /// Add the named channels to the subscription set.
    ///
    /// Unknown channel names are silently ignored so forward-compatible
    /// clients do not break when connected to older servers.
    pub fn subscribe(&mut self, channels: &[String]) {
        for name in channels {
            if let Ok(channel) = name.parse::<ObservabilityChannel>() {
                self.subscribed.insert(channel);
            }
        }
    }

    /// Remove the named channels from the subscription set.
    ///
    /// Unknown channel names are silently ignored.
    pub fn unsubscribe(&mut self, channels: &[String]) {
        for name in channels {
            if let Ok(channel) = name.parse::<ObservabilityChannel>() {
                self.subscribed.remove(&channel);
            }
        }
    }

    /// Return whether the given channel is currently subscribed.
    ///
    /// Always returns `true` when force-on mode is active.
    pub fn is_subscribed(&self, channel: ObservabilityChannel) -> bool {
        self.force_on || self.subscribed.contains(&channel)
    }

    /// Return whether a server event should be delivered to this connection.
    ///
    /// Gating rules:
    ///
    /// | Event | Condition |
    /// |-------|-----------|
    /// | `ContextAssembled` | Requires `Context` subscription. Exception: if the
    /// |   | report contains errors, always delivered (client must surface failures). |
    /// | `ToolStart` / `ToolEnd` | Requires `Tools` subscription. |
    /// | `OutputSchemaRetry` | Requires `Context` subscription (observability noise). |
    /// | `TurnComplete` | Always delivered. Usage fields (input_tokens, etc.)
    /// |   | are populated at construction time based on `Context` subscription. |
    /// | `OutputSchemaFailed` | Always delivered — error condition the client must surface. |
    /// | `Token` / `Error` / `Cancelled` / `Shutdown` | Always delivered. |
    pub fn should_deliver(&self, event: &ServerEvent) -> bool {
        match event {
            ServerEvent::ContextAssembled { report, .. } => {
                // Always deliver when the report has errors — the client needs
                // to know that assembly failed even if it has not subscribed.
                let has_errors = report
                    .get("errors")
                    .and_then(|e| e.as_array())
                    .is_some_and(|arr| !arr.is_empty());

                has_errors || self.is_subscribed(ObservabilityChannel::Context)
            }

            ServerEvent::ToolStart { .. } | ServerEvent::ToolEnd { .. } => {
                self.is_subscribed(ObservabilityChannel::Tools)
            }

            // Retry progress is observability noise — only deliver to subscribers.
            ServerEvent::OutputSchemaRetry { .. } => {
                self.is_subscribed(ObservabilityChannel::Context)
            }

            // TurnComplete always delivers the base event. Usage fields
            // (input_tokens, output_tokens, cost_microcents, structured_output) are
            // populated by the turn loop only when the connection is context-subscribed —
            // the gating happens at construction time, not delivery time.
            ServerEvent::TurnComplete { .. } => true,

            // OutputSchemaFailed is an error condition — always surface it so the
            // client can inform the user that structured output could not be produced.
            ServerEvent::OutputSchemaFailed { .. } => true,

            // Approval events are always delivered — the client must present the
            // approval UI or surface the outcome regardless of subscription state.
            ServerEvent::ApprovalRequired { .. }
            | ServerEvent::ApprovalConfirmed { .. }
            | ServerEvent::ApprovalExpired { .. }
            | ServerEvent::ApprovalDecisionError { .. } => true,

            // Inter-agent communication events are always delivered.
            ServerEvent::DelegationStarted { .. } | ServerEvent::DelegationCompleted { .. } => true,

            // Topology progress events are always delivered to the matching session.
            ServerEvent::TopologyRunStarted { .. }
            | ServerEvent::TopologyNodeStarted { .. }
            | ServerEvent::TopologyNodeCompleted { .. }
            | ServerEvent::TopologyRunCompleted { .. } => true,

            // Baseline events are always delivered — subscription cannot suppress them.
            ServerEvent::Token { .. }
            | ServerEvent::Error { .. }
            | ServerEvent::Cancelled {}
            | ServerEvent::SkillActivated { .. }
            | ServerEvent::SkillAlreadyActive { .. }
            | ServerEvent::SkillDeactivated { .. }
            | ServerEvent::Shutdown { .. } => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    // -----------------------------------------------------------------------
    // ObservabilityChannel from_str
    // -----------------------------------------------------------------------

    #[test]
    fn channel_parses_context() {
        assert_eq!(
            "context".parse::<ObservabilityChannel>().unwrap(),
            ObservabilityChannel::Context
        );
    }

    #[test]
    fn channel_parses_tools() {
        assert_eq!(
            "tools".parse::<ObservabilityChannel>().unwrap(),
            ObservabilityChannel::Tools
        );
    }

    #[test]
    fn channel_rejects_unknown_names() {
        assert!("metrics".parse::<ObservabilityChannel>().is_err());
        assert!("".parse::<ObservabilityChannel>().is_err());
        assert!("Context".parse::<ObservabilityChannel>().is_err()); // case-sensitive
    }

    // -----------------------------------------------------------------------
    // subscribe / unsubscribe / is_subscribed
    // -----------------------------------------------------------------------

    #[test]
    fn new_state_has_no_subscriptions() {
        let state = SubscriptionState::new(false);
        assert!(!state.is_subscribed(ObservabilityChannel::Context));
        assert!(!state.is_subscribed(ObservabilityChannel::Tools));
    }

    #[test]
    fn subscribe_adds_known_channels() {
        let mut state = SubscriptionState::new(false);
        state.subscribe(&["context".to_string(), "tools".to_string()]);
        assert!(state.is_subscribed(ObservabilityChannel::Context));
        assert!(state.is_subscribed(ObservabilityChannel::Tools));
    }

    #[test]
    fn subscribe_ignores_unknown_channel_names() {
        let mut state = SubscriptionState::new(false);
        // Should not panic or add anything for unknown names.
        state.subscribe(&["metrics".to_string(), "unknown".to_string()]);
        assert!(!state.is_subscribed(ObservabilityChannel::Context));
        assert!(!state.is_subscribed(ObservabilityChannel::Tools));
    }

    #[test]
    fn unsubscribe_removes_channels() {
        let mut state = SubscriptionState::new(false);
        state.subscribe(&["context".to_string(), "tools".to_string()]);
        state.unsubscribe(&["context".to_string()]);
        assert!(!state.is_subscribed(ObservabilityChannel::Context));
        assert!(state.is_subscribed(ObservabilityChannel::Tools));
    }

    #[test]
    fn unsubscribe_ignores_unknown_names() {
        let mut state = SubscriptionState::new(false);
        state.subscribe(&["context".to_string()]);
        // Should not panic or affect known subscriptions.
        state.unsubscribe(&["metrics".to_string()]);
        assert!(state.is_subscribed(ObservabilityChannel::Context));
    }

    #[test]
    fn unsubscribe_when_not_subscribed_is_a_no_op() {
        let mut state = SubscriptionState::new(false);
        // Should not panic.
        state.unsubscribe(&["context".to_string()]);
        assert!(!state.is_subscribed(ObservabilityChannel::Context));
    }

    // -----------------------------------------------------------------------
    // Force-on mode
    // -----------------------------------------------------------------------

    #[test]
    fn force_on_treats_all_channels_as_subscribed() {
        let state = SubscriptionState::new(true);
        assert!(state.is_subscribed(ObservabilityChannel::Context));
        assert!(state.is_subscribed(ObservabilityChannel::Tools));
    }

    #[test]
    fn force_on_delivers_gated_events_without_explicit_subscription() {
        let state = SubscriptionState::new(true);
        let tool_start = ServerEvent::ToolStart {
            call_id: "c1".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: None,
        };
        assert!(state.should_deliver(&tool_start));

        let context = ServerEvent::ContextAssembled {
            turn_id: Uuid::new_v4(),
            report: serde_json::json!({"errors": []}),
        };
        assert!(state.should_deliver(&context));
    }

    // -----------------------------------------------------------------------
    // should_deliver — gating rules
    // -----------------------------------------------------------------------

    fn unsubscribed() -> SubscriptionState {
        SubscriptionState::new(false)
    }

    fn subscribed_to(channels: &[&str]) -> SubscriptionState {
        let mut s = SubscriptionState::new(false);
        let names: Vec<String> = channels.iter().map(|n| n.to_string()).collect();
        s.subscribe(&names);
        s
    }

    // Always-delivered baseline events

    #[test]
    fn token_always_delivered() {
        let state = unsubscribed();
        let event = ServerEvent::Token {
            content: "hi".to_string(),
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn error_always_delivered() {
        let state = unsubscribed();
        let event = ServerEvent::Error {
            code: "internal_error".to_string(),
            message: "oops".to_string(),
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn cancelled_always_delivered() {
        let state = unsubscribed();
        let event = ServerEvent::Cancelled {};
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn shutdown_always_delivered() {
        let state = unsubscribed();
        let event = ServerEvent::Shutdown {
            reason: "idle".to_string(),
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn turn_complete_always_delivered() {
        let state = unsubscribed();
        let event = ServerEvent::TurnComplete {
            turn_id: Uuid::new_v4(),
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: None,
        };
        assert!(state.should_deliver(&event));
    }

    // ContextAssembled gating

    #[test]
    fn context_assembled_gated_without_context_subscription() {
        let state = unsubscribed();
        let event = ServerEvent::ContextAssembled {
            turn_id: Uuid::new_v4(),
            report: serde_json::json!({"errors": []}),
        };
        assert!(!state.should_deliver(&event));
    }

    #[test]
    fn context_assembled_delivered_with_context_subscription() {
        let state = subscribed_to(&["context"]);
        let event = ServerEvent::ContextAssembled {
            turn_id: Uuid::new_v4(),
            report: serde_json::json!({"errors": []}),
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn context_assembled_always_delivered_when_report_has_errors() {
        // Even without a context subscription, errors bypass the gate so the
        // client can surface assembly failures to the user.
        let state = unsubscribed();
        let event = ServerEvent::ContextAssembled {
            turn_id: Uuid::new_v4(),
            report: serde_json::json!({
                "errors": [{
                    "assembly_name": "custom",
                    "error_kind": "timeout",
                    "message": "assembly timed out",
                    "fallback_used": "default"
                }]
            }),
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn context_assembled_error_bypass_checks_non_empty_errors_array() {
        // An empty errors array should not bypass the subscription gate.
        let state = unsubscribed();
        let event = ServerEvent::ContextAssembled {
            turn_id: Uuid::new_v4(),
            report: serde_json::json!({"errors": []}),
        };
        assert!(
            !state.should_deliver(&event),
            "empty errors array should not bypass gate"
        );
    }

    // ToolStart / ToolEnd gating

    #[test]
    fn tool_start_gated_without_tools_subscription() {
        let state = unsubscribed();
        let event = ServerEvent::ToolStart {
            call_id: "c1".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: None,
        };
        assert!(!state.should_deliver(&event));
    }

    #[test]
    fn tool_start_delivered_with_tools_subscription() {
        let state = subscribed_to(&["tools"]);
        let event = ServerEvent::ToolStart {
            call_id: "c1".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: None,
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn tool_end_gated_without_tools_subscription() {
        let state = unsubscribed();
        let event = ServerEvent::ToolEnd {
            call_id: "c1".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("ok"),
            status: "success".to_string(),
            duration_ms: 10,
            truncated: false,
            total_bytes: None,
            parent_call_id: None,
        };
        assert!(!state.should_deliver(&event));
    }

    #[test]
    fn tool_end_delivered_with_tools_subscription() {
        let state = subscribed_to(&["tools"]);
        let event = ServerEvent::ToolEnd {
            call_id: "c1".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("ok"),
            status: "success".to_string(),
            duration_ms: 10,
            truncated: false,
            total_bytes: None,
            parent_call_id: None,
        };
        assert!(state.should_deliver(&event));
    }

    // Cross-channel isolation: subscribing to one channel does not grant the other

    #[test]
    fn context_subscription_does_not_grant_tools() {
        let state = subscribed_to(&["context"]);
        let event = ServerEvent::ToolStart {
            call_id: "c1".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: None,
        };
        assert!(!state.should_deliver(&event));
    }

    #[test]
    fn tools_subscription_does_not_grant_context_assembled() {
        let state = subscribed_to(&["tools"]);
        let event = ServerEvent::ContextAssembled {
            turn_id: Uuid::new_v4(),
            report: serde_json::json!({"errors": []}),
        };
        assert!(!state.should_deliver(&event));
    }

    // OutputSchemaRetry / OutputSchemaFailed gating [z4ov]

    #[test]
    fn output_schema_retry_gated_without_context_subscription() {
        let state = unsubscribed();
        let event = ServerEvent::OutputSchemaRetry {
            turn_id: Uuid::new_v4(),
            attempt: 1,
            max_retries: 3,
            errors: vec![],
        };
        assert!(!state.should_deliver(&event));
    }

    #[test]
    fn output_schema_retry_delivered_with_context_subscription() {
        let state = subscribed_to(&["context"]);
        let event = ServerEvent::OutputSchemaRetry {
            turn_id: Uuid::new_v4(),
            attempt: 1,
            max_retries: 3,
            errors: vec![],
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn output_schema_retry_delivered_with_force_on() {
        let state = SubscriptionState::new(true);
        let event = ServerEvent::OutputSchemaRetry {
            turn_id: Uuid::new_v4(),
            attempt: 1,
            max_retries: 3,
            errors: vec![],
        };
        assert!(state.should_deliver(&event));
    }

    #[test]
    fn output_schema_failed_always_delivered_unsubscribed() {
        let state = unsubscribed();
        let event = ServerEvent::OutputSchemaFailed {
            turn_id: Uuid::new_v4(),
            attempts: 3,
            errors: vec![],
        };
        assert!(
            state.should_deliver(&event),
            "OutputSchemaFailed must always be delivered"
        );
    }

    #[test]
    fn output_schema_failed_always_delivered_subscribed() {
        let state = subscribed_to(&["context"]);
        let event = ServerEvent::OutputSchemaFailed {
            turn_id: Uuid::new_v4(),
            attempts: 3,
            errors: vec![],
        };
        assert!(state.should_deliver(&event));
    }
}
