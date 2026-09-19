//! The one approval a turn raises for itself: permission to run with
//! degraded isolation when the sandbox the configuration asked for cannot be
//! built. Raised only on an attended turn — an unattended one refuses the
//! fallback outright (the coordinator's decision, made before this is
//! reached) — it is an ordinary hook-kind approval row, so the client sees it
//! through the same `ApprovalRequired` event and answers it through the same
//! decision route as a tool approval. The turn rests in `AwaitingApproval`
//! while the person decides and returns to `Processing` on any decision that
//! lets it continue; a timeout follows the configured timeout policy and a
//! cancelled wait leaves the turn interrupted.
//!
//! Every write here is a constitutive record (record_durability.rs): the
//! approval row, its decision, and the turn's state transitions are the
//! work, so each propagates its storage error with `?` and the turn fails
//! loudly — never a warn-and-continue.

use super::{
    AppState, ApprovalKind, ApprovalState, ApprovalTimeoutPolicy, CancellationToken, GatewayError,
    NewApprovalRequest, ServerEvent, TurnState, Uuid, mpsc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SandboxDegradationApproval {
    Approved,
    Rejected,
    Interrupted,
}

pub(super) struct SandboxDegradationApprovalRequest<'a> {
    pub(super) config: &'a swage_config::ResolvedConfig,
    pub(super) ws_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub(super) turn_id: Uuid,
    pub(super) agent_id: &'a str,
    pub(super) prompt: String,
    pub(super) context: serde_json::Value,
    pub(super) cancel_token: &'a CancellationToken,
}

pub(super) async fn request_sandbox_degradation_approval(
    state: &AppState,
    request: SandboxDegradationApprovalRequest<'_>,
) -> Result<SandboxDegradationApproval, GatewayError> {
    let SandboxDegradationApprovalRequest {
        config,
        ws_tx,
        turn_id,
        agent_id,
        prompt,
        context,
        cancel_token,
    } = request;
    let timeout = std::time::Duration::from_secs(config.hitl.timeout);
    let timeout_at = chrono::Utc::now()
        + chrono::Duration::from_std(timeout)
            .unwrap_or_else(|_| chrono::Duration::seconds(config.hitl.timeout as i64));
    let timeout_policy = config.hitl.timeout_policy.clone();

    let approval = state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id,
            agent_id: agent_id.to_owned(),
            kind: ApprovalKind::Hook,
            tool_name: None,
            tool_arguments: None,
            hook_name: Some("sandbox_degradation".to_string()),
            prompt: prompt.clone(),
            context: Some(context.clone()),
            timeout_at,
            timeout_policy: timeout_policy.clone(),
        })
        .await?;

    state
        .storage
        .update_turn_state(turn_id, TurnState::AwaitingApproval)
        .await?;

    let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
    state
        .approval_channels
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(approval.id, decision_tx);

    let _ = ws_tx.send(ServerEvent::ApprovalRequired {
        approval_id: approval.id,
        turn_id,
        agent_id: agent_id.to_owned(),
        kind: "hook".to_string(),
        tool_name: None,
        tool_arguments: None,
        prompt,
        context: Some(context),
        timeout_at: timeout_at.to_rfc3339(),
        timeout_policy: timeout_policy.to_string(),
    });

    enum ApprovalWait {
        Decision(swage_agent::ApprovalDecision),
        Timeout,
        Cancelled,
    }

    let wait_result = tokio::select! {
        decision = decision_rx => match decision {
            Ok(decision) => ApprovalWait::Decision(decision),
            Err(_) => ApprovalWait::Timeout,
        },
        _ = tokio::time::sleep(timeout) => ApprovalWait::Timeout,
        _ = cancel_token.cancelled() => ApprovalWait::Cancelled,
    };

    state
        .approval_channels
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&approval.id);

    let outcome = match wait_result {
        ApprovalWait::Decision(decision) if decision.is_approved() => {
            state
                .storage
                .update_approval_state(
                    approval.id,
                    ApprovalState::Approved,
                    decision.reason.as_deref(),
                )
                .await?;
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval.id,
                decision: "approved".to_string(),
                reason: decision.reason,
            });
            SandboxDegradationApproval::Approved
        }
        ApprovalWait::Decision(decision) => {
            state
                .storage
                .update_approval_state(
                    approval.id,
                    ApprovalState::Rejected,
                    decision.reason.as_deref(),
                )
                .await?;
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval.id,
                decision: "rejected".to_string(),
                reason: decision.reason,
            });
            SandboxDegradationApproval::Rejected
        }
        ApprovalWait::Timeout => {
            state
                .storage
                .update_approval_state(
                    approval.id,
                    ApprovalState::Expired,
                    Some("approval timed out"),
                )
                .await?;
            let _ = ws_tx.send(ServerEvent::ApprovalExpired {
                approval_id: approval.id,
                timeout_policy: timeout_policy.to_string(),
            });
            match timeout_policy {
                ApprovalTimeoutPolicy::Reject => SandboxDegradationApproval::Rejected,
                ApprovalTimeoutPolicy::Interrupt => SandboxDegradationApproval::Interrupted,
            }
        }
        ApprovalWait::Cancelled => {
            state
                .storage
                .update_approval_state(approval.id, ApprovalState::Expired, Some("turn cancelled"))
                .await?;
            SandboxDegradationApproval::Interrupted
        }
    };

    if !matches!(outcome, SandboxDegradationApproval::Interrupted) {
        state
            .storage
            .update_turn_state(turn_id, TurnState::Processing)
            .await?;
    }
    Ok(outcome)
}
