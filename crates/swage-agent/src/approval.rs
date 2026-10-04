//! Approval gate types for human-in-the-loop tool execution.
//!
//! When the agent encounters a tool that requires approval (either from config
//! permissions or a PreToolUse hook), it pauses execution and waits for a
//! human decision. These types bridge the agent runtime with the gateway's
//! WebSocket-based approval UI.

use std::sync::Arc;
use uuid::Uuid;

pub use swage_protocol::client::approval::{
    ApprovalDecision, ApprovalDecisionParseError, ApprovalDecisionScope, ApprovalDecisionValue,
};

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
    /// Human approved — proceed with tool execution. The scope is carried
    /// so the tool result can tell the model how the call was approved.
    Approved { scope: ApprovalDecisionScope },
    /// Approval timed out — deliver the existing timeout feedback with an expired status.
    Expired { feedback: String },
    /// Human rejected — deliver this
    /// feedback string as the tool result.
    Rejected { feedback: String },
    /// Timeout with interrupt policy — the turn must be interrupted.
    Interrupted,
}

/// Model-facing tool result for a call the person denied. The decision is
/// stated in fixed words; the UI's reason string describes which control was
/// used, not anything the person said, so it never reaches the model.
pub(crate) fn user_denied_feedback() -> String {
    String::from("The user denied this tool call. It did not run, and no approval is pending.")
}

/// A dispatch's payload and approval feedback travel separately so only the
/// model-facing content carries the feedback. Storage metadata and hooks use
/// the payload unchanged.
pub(crate) struct ToolDispatchResult {
    pub content: String,
    pub is_error: bool,
    pub status: String,
    pub approval_note: Option<String>,
    pub execution_prevented: bool,
}

impl From<(String, bool, String)> for ToolDispatchResult {
    fn from((content, is_error, status): (String, bool, String)) -> Self {
        Self {
            content,
            is_error,
            status,
            approval_note: None,
            execution_prevented: false,
        }
    }
}

impl ToolDispatchResult {
    pub fn model_content(&self, content: &str) -> String {
        match &self.approval_note {
            Some(note) => format!("{note}\n{content}"),
            None => content.to_owned(),
        }
    }
}

/// Record how the person approved the call without altering its payload.
/// Success confirms execution; validation or access failures prevent it.
/// Other errors do not establish whether execution began.
pub(crate) fn with_user_approval_note(
    mut result: ToolDispatchResult,
    scope: ApprovalDecisionScope,
) -> ToolDispatchResult {
    let how = match scope {
        ApprovalDecisionScope::Once => "once",
        ApprovalDecisionScope::Session => "for this session",
        ApprovalDecisionScope::Permanent | ApprovalDecisionScope::PermanentGlobal => "permanently",
    };
    if result.status == "rejected" {
        // A later gate on the same call denied it; that denial is the outcome.
        return result;
    }
    let outcome = if result.status == "success" {
        "; it ran after approval"
    } else if result.execution_prevented {
        "; it did not run"
    } else {
        ""
    };
    let note = format!("[The user approved this tool call {how}{outcome}.]");
    result.approval_note = Some(match result.approval_note {
        Some(previous) => format!("{note}\n{previous}"),
        None => note,
    });
    result
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
        assert!(matches!(d.decision, ApprovalDecisionValue::Approved));
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
