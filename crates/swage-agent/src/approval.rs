//! Approval gate types for human-in-the-loop tool execution.
//!
//! When the agent encounters a tool that requires approval (either from config
//! permissions or a PreToolUse hook), it pauses execution and waits for a
//! human decision. These types bridge the agent runtime with the gateway's
//! WebSocket-based approval UI.

use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

/// The outcome of a human approval decision, sent back to the agent through
/// a oneshot channel.
#[derive(Debug, Clone)]
pub struct ApprovalDecision {
    pub decision: ApprovalDecisionValue,
    pub scope: ApprovalDecisionScope,
    /// Optional human-readable rationale from the reviewer.
    pub reason: Option<String>,
}

impl ApprovalDecision {
    pub fn is_approved(&self) -> bool {
        self.decision == ApprovalDecisionValue::Approved
    }
}

/// How widely an approval decision should apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionScope {
    /// Approve only the pending request.
    Once,
    /// Approve this request and any later approval gates in the same turn.
    Turn,
}

impl Default for ApprovalDecisionScope {
    fn default() -> Self {
        Self::Once
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionValue {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown approval decision: {value}")]
pub struct ApprovalDecisionParseError {
    value: String,
}

impl ApprovalDecisionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

impl std::fmt::Display for ApprovalDecisionValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ApprovalDecisionValue {
    type Err = ApprovalDecisionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            other => Err(ApprovalDecisionParseError {
                value: other.to_string(),
            }),
        }
    }
}

/// Callback type for registering a oneshot channel with the gateway.
///
/// The agent calls this with an `approval_id` and the sender half of a
/// oneshot channel. The gateway stores the sender in its shared map and
/// fires it when the WebSocket client sends a decision. The agent holds
/// the receiver and awaits the decision (racing against a timeout).
pub type ApprovalChannelRegistrar =
    Arc<dyn Fn(Uuid, tokio::sync::oneshot::Sender<ApprovalDecision>) + Send + Sync>;

/// Internal result from `dispatch_pre_tool_use` — distinguishes between
/// "proceed with execution" and "approval required from hook".
#[derive(Debug)]
pub(crate) enum PreToolUseOutcome {
    /// Hook passed — proceed with tool execution.
    Proceed,
    /// Hook requested human approval before this tool executes.
    RequestApproval {
        prompt: String,
        context: Option<serde_json::Value>,
        timeout: Option<std::time::Duration>,
    },
}

/// The outcome of the approval gate — drives post-gate logic in the
/// tool dispatch loop.
#[derive(Debug)]
pub(crate) enum ApprovalGateOutcome {
    /// Human approved — proceed with tool execution.
    Approved,
    /// Human rejected (or timeout treated as rejection) — deliver this
    /// feedback string as the tool result.
    Rejected { feedback: String },
    /// Timeout with interrupt policy — the turn must be interrupted.
    Interrupted,
}

/// Default approval timeout when neither config nor hook specifies one.
pub(crate) const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 300;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_decision_approved() {
        let d = ApprovalDecision {
            decision: ApprovalDecisionValue::Approved,
            scope: ApprovalDecisionScope::Once,
            reason: None,
        };
        assert!(d.is_approved());
    }

    #[test]
    fn approval_decision_rejected() {
        let d = ApprovalDecision {
            decision: ApprovalDecisionValue::Rejected,
            scope: ApprovalDecisionScope::Once,
            reason: Some("too dangerous".to_string()),
        };
        assert!(!d.is_approved());
    }

    #[test]
    fn approval_decision_debug_and_clone() {
        let d = ApprovalDecision {
            decision: ApprovalDecisionValue::Approved,
            scope: ApprovalDecisionScope::Once,
            reason: Some("looks good".to_string()),
        };
        let cloned = d.clone();
        let debug = format!("{cloned:?}");
        assert!(debug.contains("Approved"));
    }

    #[tokio::test]
    async fn approval_decision_traverses_oneshot_channel() {
        let (tx, rx) = tokio::sync::oneshot::channel::<ApprovalDecision>();
        tx.send(ApprovalDecision {
            decision: ApprovalDecisionValue::Approved,
            scope: ApprovalDecisionScope::Once,
            reason: None,
        })
        .unwrap();
        let received = rx.await.unwrap();
        assert!(received.is_approved());
    }

    #[test]
    fn approval_channel_registrar_is_callable() {
        // Verify the type alias compiles and a closure satisfies it.
        let registrar: ApprovalChannelRegistrar = Arc::new(|_id, _tx| {
            // In production, this inserts into the gateway's approval_channels map.
        });
        let (tx, _rx) = tokio::sync::oneshot::channel::<ApprovalDecision>();
        registrar(Uuid::new_v4(), tx);
    }

    #[test]
    fn pre_tool_use_outcome_proceed() {
        let outcome = PreToolUseOutcome::Proceed;
        assert!(matches!(outcome, PreToolUseOutcome::Proceed));
    }

    #[test]
    fn pre_tool_use_outcome_request_approval() {
        let outcome = PreToolUseOutcome::RequestApproval {
            prompt: "Allow deploy?".to_string(),
            context: Some(serde_json::json!({"env": "prod"})),
            timeout: Some(std::time::Duration::from_secs(300)),
        };
        if let PreToolUseOutcome::RequestApproval {
            prompt,
            context,
            timeout,
        } = outcome
        {
            assert_eq!(prompt, "Allow deploy?");
            assert!(context.is_some());
            assert_eq!(timeout, Some(std::time::Duration::from_secs(300)));
        } else {
            panic!("expected RequestApproval");
        }
    }
}
