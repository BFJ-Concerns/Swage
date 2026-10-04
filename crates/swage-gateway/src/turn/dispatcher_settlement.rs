//! Settling the turn's tool dispatcher against the isolation it can
//! actually get.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: the turn runs at a known isolation or not at all. The
//! dispatcher is built sandboxed when the configuration asks for it; when
//! the sandbox cannot be built, an unattended turn is blocked and an
//! attended one asks the person (turn/sandbox_approval.rs) before it is
//! rebuilt unsandboxed. Every degradation is audited before the dispatcher
//! is handed back, and a failed audit fails the turn — the degraded-execution
//! record is constitutive (record_durability.rs). Whether the turn goes on
//! once the dispatcher has settled stays the coordinator's.

use super::{
    AgentConfig, AppState, CancellationToken, ExecutionIsolation, GatewayDispatcherBuild,
    GatewayDispatcherRequest, GatewayError, SandboxDegradationApproval,
    SandboxDegradationApprovalRequest, ServerEvent, TurnAttendance, TurnState, Uuid,
    build_gateway_dispatcher, mpsc, record_degraded_execution,
    record_failed_turn_and_release_on_error, record_terminal_state_and_release,
    record_turn_isolation, release_active_turn, request_sandbox_degradation_approval,
    shutdown_sandbox_guard,
};

/// What the coordinator resolved for building the turn's dispatcher.
pub(super) struct DispatcherSettlement<'a> {
    pub(super) config: &'a swage_config::ResolvedConfig,
    pub(super) agent_config: &'a AgentConfig,
    pub(super) session_metadata: Option<&'a serde_json::Value>,
    pub(super) topic_id: Uuid,
    pub(super) active_workspace_attachment_id: Option<Uuid>,
    pub(super) ws_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub(super) session_id: Uuid,
    pub(super) prepared_turn_id: Uuid,
    pub(super) agent_id: &'a str,
    pub(super) attendance: TurnAttendance,
    pub(super) cancel_token: &'a CancellationToken,
}

/// How the dispatcher settled: built at an isolation the turn may run at,
/// or the person's decision was interrupted — the turn is then already
/// marked interrupted, its cancellation sent, and its active-turn guard
/// released.
pub(super) enum SettledDispatcher {
    Ready(Box<GatewayDispatcherBuild>),
    Interrupted,
}

/// Build the turn's tool dispatcher at an isolation the turn may run at.
/// A failed build blocks the turn, unless the turn is attended and asked
/// for a sandbox — then the person decides whether it runs unsandboxed.
/// Every degradation is audited before the dispatcher is handed back.
pub(super) async fn settle_turn_dispatcher(
    state: &AppState,
    settlement: DispatcherSettlement<'_>,
) -> Result<SettledDispatcher, GatewayError> {
    let DispatcherSettlement {
        config,
        agent_config,
        session_metadata,
        topic_id,
        active_workspace_attachment_id,
        ws_tx,
        session_id,
        prepared_turn_id,
        agent_id,
        attendance,
        cancel_token,
    } = settlement;
    let execution_context = match attendance {
        TurnAttendance::Attended => "interactive turn",
        TurnAttendance::Unattended => "unattended turn",
    };
    let dispatcher_build = match build_gateway_dispatcher(
        state,
        GatewayDispatcherRequest {
            config,
            agent_config,
            session_metadata,
            topic_id: Some(topic_id),
            active_workspace_attachment_id,
            ws_tx: ws_tx.clone(),
            session_id: Some(session_id),
            turn_id: Some(prepared_turn_id),
        },
    )
    .await
    {
        Ok(build) => build,
        Err(error) if config.sandbox.enabled && attendance == TurnAttendance::Attended => {
            let detected_degradation = ExecutionIsolation::Degraded {
                reason: format!("sandbox_setup_failed: {error}"),
            };
            if let Err(audit_error) = record_degraded_execution(
                state,
                &detected_degradation,
                execution_context,
                Some(session_id),
                Some(prepared_turn_id),
            )
            .await
            {
                record_terminal_state_and_release(
                    state,
                    session_id,
                    prepared_turn_id,
                    TurnState::Failed,
                )
                .await?;
                return Err(audit_error);
            }
            let approval = record_failed_turn_and_release_on_error(
                state,
                session_id,
                prepared_turn_id,
                request_sandbox_degradation_approval(
                    state,
                    SandboxDegradationApprovalRequest {
                    config,
                    ws_tx,
                    turn_id: prepared_turn_id,
                    agent_id,
                    prompt:
                        "Sandbox setup failed. Approve running this turn without sandbox isolation?"
                            .to_string(),
                    context: serde_json::json!({
                        "degradation": "unsandboxed_execution",
                        "error": error.to_string(),
                    }),
                    cancel_token,
                    },
                )
                .await,
            )
            .await?;
            match approval {
                SandboxDegradationApproval::Approved => {
                    // The approval wait temporarily marks the placeholder as
                    // Processing. Agent::process_turn_streaming takes over
                    // existing placeholders only while they are Pending.
                    record_failed_turn_and_release_on_error(
                        state,
                        session_id,
                        prepared_turn_id,
                        state
                            .storage
                            .update_turn_state(prepared_turn_id, TurnState::Pending)
                            .await
                            .map_err(GatewayError::from),
                    )
                    .await?;
                }
                SandboxDegradationApproval::Rejected => {
                    record_terminal_state_and_release(
                        state,
                        session_id,
                        prepared_turn_id,
                        TurnState::Failed,
                    )
                    .await?;
                    return Err(GatewayError::WebSocketError(format!(
                        "sandbox setup failed and unsandboxed execution was not approved: {error}"
                    )));
                }
                SandboxDegradationApproval::Interrupted => {
                    record_failed_turn_and_release_on_error(
                        state,
                        session_id,
                        prepared_turn_id,
                        state
                            .storage
                            .update_turn_state(prepared_turn_id, TurnState::Interrupted)
                            .await
                            .map_err(GatewayError::from),
                    )
                    .await?;
                    let _ = ws_tx.send(ServerEvent::Cancelled {});
                    // Preserve the client-visible interrupt ordering: the old turn
                    // remains active until its cancellation event has been sent.
                    release_active_turn(state, session_id).await;
                    return Ok(SettledDispatcher::Interrupted);
                }
            }

            let mut unsandboxed_config = config.clone();
            unsandboxed_config.sandbox.allow_degraded_execution = true;
            record_failed_turn_and_release_on_error(
                state,
                session_id,
                prepared_turn_id,
                build_gateway_dispatcher(
                    state,
                    GatewayDispatcherRequest {
                        config: &unsandboxed_config,
                        agent_config,
                        session_metadata,
                        topic_id: Some(topic_id),
                        active_workspace_attachment_id,
                        ws_tx: ws_tx.clone(),
                        session_id: Some(session_id),
                        turn_id: Some(prepared_turn_id),
                    },
                )
                .await
                .map_err(|fallback_error| GatewayError::WebSocketError(fallback_error.to_string())),
            )
            .await?
        }
        Err(error) => {
            let isolation = ExecutionIsolation::Degraded {
                reason: format!("sandbox_setup_failed: {error}"),
            };
            let message = format!("Sandbox setup failed; the turn was not executed: {error}");
            if let Some(reason) = super::projection::record_failure_explanation(
                state,
                session_id,
                prepared_turn_id,
                &message,
            )
            .await
            {
                tracing::error!(%reason, turn_id = %prepared_turn_id, "turn record integrity lost");
            }
            let _ = ws_tx.send(ServerEvent::Error {
                code: "sandbox_unavailable".into(),
                message,
            });
            record_terminal_state_and_release(
                state,
                session_id,
                prepared_turn_id,
                TurnState::Failed,
            )
            .await?;
            record_degraded_execution(
                state,
                &isolation,
                execution_context,
                Some(session_id),
                Some(prepared_turn_id),
            )
            .await?;
            return Err(GatewayError::WebSocketError(format!(
                "sandbox setup failed; execution was blocked: {error}"
            )));
        }
    };
    if let Err(error) = record_degraded_execution(
        state,
        &dispatcher_build.isolation,
        execution_context,
        Some(session_id),
        Some(prepared_turn_id),
    )
    .await
    {
        shutdown_sandbox_guard(
            &dispatcher_build.sandbox_guard,
            "failed to shut down sandbox after degradation audit failure",
        )
        .await;
        record_terminal_state_and_release(state, session_id, prepared_turn_id, TurnState::Failed)
            .await?;
        return Err(error);
    }
    // The turn's own statement of where it runs, written before anything
    // runs there: like the degradation audit, constitutive of the record.
    if let Err(error) =
        record_turn_isolation(state, &dispatcher_build.isolation, prepared_turn_id).await
    {
        shutdown_sandbox_guard(
            &dispatcher_build.sandbox_guard,
            "failed to shut down sandbox after isolation record failure",
        )
        .await;
        record_terminal_state_and_release(state, session_id, prepared_turn_id, TurnState::Failed)
            .await?;
        return Err(error);
    }
    Ok(SettledDispatcher::Ready(Box::new(dispatcher_build)))
}
