//! The turn's projection loop: every `AgentEvent` the agent emits, mapped
//! to the client's `ServerEvent` and to the turn's durable record.
//!
//! The loop is the turn's audit seam. Each arm either forwards to the
//! session's socket (gated by the connection's subscription where the event
//! is observability rather than conversation), writes a record, or both;
//! the persistence sites carry their durability class from
//! record_durability.rs — reconstructive snapshots name their loss and
//! escalate only when the loss itself cannot be recorded, the constitutive
//! structured-output write escalates outright, and the derivable activity
//! stamp is accepted with a warning. The escalation is not acted on here:
//! it is returned to the coordinator, which fails the turn loudly once the
//! agent has settled.

use super::{
    AgentEvent, AppState, Arc, Mutex, ProviderFailureObserver, ProviderSnapshotCapturePath,
    ServerEvent, SubscriptionState, Uuid, WorkflowProviderRoundOutcome,
    apply_provider_snapshot_event, mpsc, open_provider_round_record, record_durability,
    settle_provider_round_record,
};
use crate::lifecycle::ActivityTracker;
use swage_config::PricingConfig;
use swage_protocol::client::context::AssemblyReport;

/// What the coordinator resolved before the agent started, borrowed by the
/// loop for the turn's life.
pub(super) struct TurnProjection<'a> {
    pub(super) session_id: Uuid,
    /// The turn row the coordinator prepared; the parent of any delegation
    /// until `TurnStarted` names the agent's own id.
    pub(super) prepared_turn_id: Uuid,
    /// The context snapshot as assembled, extended by the provider-visible
    /// capture as rounds go out and come back.
    pub(super) report_json: &'a mut serde_json::Value,
    pub(super) assembly_report: &'a AssemblyReport,
    pub(super) ws_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub(super) subscription: &'a Arc<Mutex<SubscriptionState>>,
    pub(super) activity_tracker: &'a ActivityTracker,
    pub(super) pricing_config: &'a PricingConfig,
    pub(super) model_name: &'a str,
    pub(super) provider_failure_observer: Option<&'a ProviderFailureObserver>,
}

/// What the loop learned that the coordinator acts on after the agent
/// settles.
pub(super) struct ProjectedTurn {
    /// The turn id `TurnStarted` carried; `None` when the agent never got
    /// that far.
    pub(super) turn_id: Option<Uuid>,
    /// Set when the durability contract's escalation fired: a constitutive
    /// record was lost, or a reconstructive loss could not even be recorded
    /// as a degradation (record_durability.rs). The coordinator fails the
    /// turn loudly rather than leave it looking auditable.
    pub(super) record_integrity_lost: Option<String>,
}

/// Drain the agent's events until its sender closes, projecting each one.
pub(super) async fn project_agent_events(
    state: &AppState,
    projection: TurnProjection<'_>,
    mut event_rx: mpsc::UnboundedReceiver<AgentEvent>,
) -> ProjectedTurn {
    let TurnProjection {
        session_id,
        prepared_turn_id,
        report_json,
        assembly_report,
        ws_tx,
        subscription,
        activity_tracker,
        pricing_config,
        model_name,
        provider_failure_observer,
    } = projection;
    let mut turn_id: Option<Uuid> = None;
    let mut record_integrity_lost: Option<String> = None;

    // The loop runs until the agent drops its sender (channel closes) or
    // the agent task completes.
    while let Some(event) = event_rx.recv().await {
        match event {
            AgentEvent::TurnStarted {
                turn_id: started_id,
            } => {
                turn_id = Some(started_id);
                // Record the turn_id in the tracing span now that the agent
                // has created the Turn record.
                tracing::Span::current().record("turn_id", started_id.to_string());
                tracing::debug!("processing turn (agent-delegated)");

                // Persist the context snapshot now that we have a turn_id.
                if let Err(e) = state
                    .storage
                    .store_snapshot(started_id, &report_json.to_string())
                    .await
                {
                    // Reconstructive record (record_durability.rs): the loss
                    // itself is recorded; only a double failure escalates.
                    tracing::warn!(error = %e, "failed to persist context snapshot");
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        started_id,
                        "context_snapshot",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "context snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }

                // Deliver the ContextAssembled event (subscription-gated).
                let assembled_event = ServerEvent::ContextAssembled {
                    turn_id: started_id,
                    report: assembly_report.clone(),
                };
                if subscription.lock().await.should_deliver(&assembled_event) {
                    let _ = ws_tx.send(assembled_event);
                }
            }
            AgentEvent::CompactionApplied {
                turn_id: snapshot_turn_id,
                record,
            } => {
                if let Some(records) = report_json
                    .get_mut("compaction_applied")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    records.push(
                        serde_json::to_value(record).unwrap_or_else(
                            |error| serde_json::json!({ "error": error.to_string() }),
                        ),
                    );
                }
                if let Err(error) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    // Reconstructive record (record_durability.rs).
                    tracing::warn!(%error, "failed to persist compaction record");
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "compaction_snapshot",
                        &error,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "compaction snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }
            }
            AgentEvent::Token { content } => {
                activity_tracker.record_activity();
                let _ = ws_tx.send(ServerEvent::Token { content });
            }
            AgentEvent::ProviderRequestPrepared {
                turn_id: snapshot_turn_id,
                round,
                request,
            } => {
                open_provider_round_record(
                    state,
                    session_id,
                    snapshot_turn_id,
                    round,
                    &mut record_integrity_lost,
                )
                .await;
                apply_provider_snapshot_event(
                    state,
                    report_json,
                    AgentEvent::ProviderRequestPrepared {
                        turn_id: snapshot_turn_id,
                        round,
                        request,
                    },
                    ProviderSnapshotCapturePath::Ordinary,
                )
                .await;
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    // Reconstructive record (record_durability.rs).
                    tracing::warn!(
                        error = %e,
                        "failed to persist provider-visible context snapshot"
                    );
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "provider_visible_snapshot",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "provider-visible snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }
            }
            AgentEvent::ProviderRoundSettled {
                turn_id: round_turn_id,
                round,
                outcome,
            } => {
                let outcome = match outcome {
                    swage_agent::ProviderRoundOutcome::Responded { tool_calls } => {
                        WorkflowProviderRoundOutcome::Responded { tool_calls }
                    }
                    swage_agent::ProviderRoundOutcome::Failed { message } => {
                        WorkflowProviderRoundOutcome::Failed { message }
                    }
                    swage_agent::ProviderRoundOutcome::Interrupted => {
                        WorkflowProviderRoundOutcome::Interrupted
                    }
                };
                settle_provider_round_record(
                    state,
                    session_id,
                    round_turn_id,
                    round,
                    outcome,
                    &mut record_integrity_lost,
                )
                .await;
            }
            AgentEvent::ProviderReasoningCaptured {
                turn_id: snapshot_turn_id,
                round,
                content,
                details,
            } => {
                apply_provider_snapshot_event(
                    state,
                    report_json,
                    AgentEvent::ProviderReasoningCaptured {
                        turn_id: snapshot_turn_id,
                        round,
                        content,
                        details,
                    },
                    ProviderSnapshotCapturePath::Ordinary,
                )
                .await;
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    // Reconstructive record (record_durability.rs).
                    tracing::warn!(
                        error = %e,
                        "failed to persist provider reasoning context snapshot"
                    );
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "provider_reasoning_snapshot",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "provider reasoning snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }
            }
            AgentEvent::ProviderResponseCaptured {
                turn_id: snapshot_turn_id,
                round,
                witness,
            } => {
                apply_provider_snapshot_event(
                    state,
                    report_json,
                    AgentEvent::ProviderResponseCaptured {
                        turn_id: snapshot_turn_id,
                        round,
                        witness,
                    },
                    ProviderSnapshotCapturePath::Ordinary,
                )
                .await;
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    // Reconstructive record (record_durability.rs).
                    tracing::warn!(error = %e, "failed to persist provider response witness");
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "provider_response_witness",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "provider response witness lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }
            }
            AgentEvent::ToolStart {
                call_id,
                tool_name,
                arguments,
                display_summary,
            } => {
                activity_tracker.record_activity();
                let tool_start = ServerEvent::ToolStart {
                    call_id,
                    tool: tool_name,
                    arguments,
                    parent_call_id: None,
                    summary: display_summary,
                };
                if subscription.lock().await.should_deliver(&tool_start) {
                    let _ = ws_tx.send(tool_start);
                }
            }
            AgentEvent::ToolEnd {
                call_id,
                tool_name,
                result,
                status,
                duration_ms,
            } => {
                let tool_end = ServerEvent::ToolEnd {
                    call_id,
                    tool: tool_name,
                    result,
                    status,
                    duration_ms: duration_ms.unwrap_or(0),
                    truncated: false,
                    total_bytes: None,
                    parent_call_id: None,
                };
                if subscription.lock().await.should_deliver(&tool_end) {
                    let _ = ws_tx.send(tool_end);
                }
            }
            AgentEvent::OutputSchemaRetry {
                turn_id,
                attempt,
                max_retries,
                errors,
            } => {
                // Retry progress is observability noise — only deliver to context subscribers.
                let retry_event = ServerEvent::OutputSchemaRetry {
                    turn_id,
                    attempt,
                    max_retries,
                    errors,
                };
                if subscription.lock().await.should_deliver(&retry_event) {
                    let _ = ws_tx.send(retry_event);
                }
            }
            AgentEvent::ProviderRetryStarted {
                turn_id,
                attempt,
                max_retries,
                provider,
                wait_ms,
            } => {
                let retry_event = ServerEvent::ProviderRetryStarted {
                    turn_id,
                    attempt,
                    max_retries,
                    provider,
                    wait_ms,
                };
                if subscription.lock().await.should_deliver(&retry_event) {
                    let _ = ws_tx.send(retry_event);
                }
            }
            AgentEvent::ProviderRetrySettled {
                turn_id,
                attempt,
                max_retries,
                provider,
                success,
            } => {
                let retry_event = ServerEvent::ProviderRetrySettled {
                    turn_id,
                    attempt,
                    max_retries,
                    provider,
                    success,
                };
                if subscription.lock().await.should_deliver(&retry_event) {
                    let _ = ws_tx.send(retry_event);
                }
            }
            AgentEvent::OutputSchemaFailed {
                turn_id,
                attempts,
                errors,
            } => {
                // Schema exhaustion is an error condition — always deliver regardless
                // of subscription state.
                let _ = ws_tx.send(ServerEvent::OutputSchemaFailed {
                    turn_id,
                    attempts,
                    errors,
                });
            }
            AgentEvent::TurnCompleted {
                turn_id,
                input_tokens,
                output_tokens,
                structured_output,
                ..
            } => {
                // Derive cost from the pricing config when available.
                let cost_microcents =
                    pricing_config.compute_cost(model_name, input_tokens, output_tokens);

                tracing::info!(
                    input_tokens = input_tokens,
                    output_tokens = output_tokens,
                    cost_microcents = ?cost_microcents,
                    "turn_complete",
                );

                // Update session activity timestamp. Derivable record
                // (record_durability.rs): activity is reconstructable from
                // turn and message timestamps, so losing this write is
                // accepted with a warning.
                if let Err(e) = state.storage.update_activity(session_id).await {
                    tracing::warn!(error = %e, "failed to update session activity");
                }

                // Populate usage fields only for context-subscribed connections.
                let (ws_input, ws_output, ws_cost) = if subscription
                    .lock()
                    .await
                    .is_subscribed(crate::subscription::ObservabilityChannel::Context)
                {
                    (Some(input_tokens), Some(output_tokens), cost_microcents)
                } else {
                    (None, None, None)
                };

                if let Some(structured_output) = structured_output.as_ref() {
                    // Constitutive record (record_durability.rs): the
                    // structured output is the work, so its loss fails the
                    // turn whether or not the gap event lands — the
                    // contract's worked example of a class-1 seam.
                    use swage_storage::store::TurnStore;
                    if let Err(error) = TurnStore::update_structured_output(
                        state.storage.as_ref(),
                        turn_id,
                        structured_output,
                    )
                    .await
                    {
                        tracing::error!(
                            %error,
                            %turn_id,
                            "failed to persist structured_output"
                        );
                        let _ = record_durability::record_reconstruction_gap(
                            state.storage.as_ref(),
                            Some(session_id),
                            turn_id,
                            "structured_output",
                            &error,
                        )
                        .await;
                        record_integrity_lost.get_or_insert(format!(
                            "structured output could not be persisted: {error}"
                        ));
                    }
                }

                let _ = ws_tx.send(ServerEvent::TurnComplete {
                    turn_id,
                    input_tokens: ws_input,
                    output_tokens: ws_output,
                    cost_microcents: ws_cost,
                    structured_output: structured_output.clone(),
                });
            }
            AgentEvent::Error {
                message,
                provider_failure,
            } => {
                if provider_failure && let Some(observer) = provider_failure_observer {
                    *observer.lock().unwrap_or_else(|error| error.into_inner()) =
                        Some(message.clone());
                }
                // Reconstructive record (record_durability.rs): the turn's
                // Failed state is the constitutive fact; the explanation the
                // person saw enriches it so the conversation can show it
                // after a reload. Its loss is named in the trail, and only a
                // trail that cannot name it fails the turn's record.
                let failed_turn_id = turn_id.unwrap_or(prepared_turn_id);
                if let Err(error) = state
                    .storage
                    .record_turn_failure_message(failed_turn_id, &message)
                    .await
                {
                    tracing::warn!(%error, turn_id = %failed_turn_id, "failed to persist failure_message");
                    if let Err(gap_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        failed_turn_id,
                        "failure_message",
                        &error,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "failure explanation could not be persisted or recorded as missing: {gap_error}"
                        ));
                    }
                }
                let _ = ws_tx.send(ServerEvent::Error {
                    code: "agent_error".to_string(),
                    message,
                });
            }
            AgentEvent::RecordIntegrityLost { reason, .. } => {
                // Constitutive record (record_durability.rs): the coordinator
                // performs the Failed transition after the agent settles.
                record_integrity_lost.get_or_insert(reason);
            }
            // Approval lifecycle events — route to the session-scoped ws_tx
            // so only the client connected to this agent sees the event.
            AgentEvent::DelegationStarted {
                target_agent_id,
                task: _,
            } => {
                // Tool-as-delegation started — emit as a delegation event.
                // We use turn_id for parent_turn_id; child_turn_id is unknown
                // at this point (the child turn hasn't been created yet).
                let _ = ws_tx.send(ServerEvent::DelegationStarted {
                    parent_turn_id: turn_id.unwrap_or(prepared_turn_id),
                    child_turn_id: None,
                    target_agent_id,
                });
            }
            AgentEvent::DelegationCompleted {
                target_agent_id,
                success,
            } => {
                let _ = ws_tx.send(ServerEvent::DelegationCompleted {
                    parent_turn_id: turn_id.unwrap_or(prepared_turn_id),
                    child_turn_id: None,
                    target_agent_id,
                    success,
                });
            }
            event @ (AgentEvent::ApprovalRequired { .. }
            | AgentEvent::ApprovalConfirmed { .. }
            | AgentEvent::ApprovalExpired { .. }) => {
                route_approval_agent_event(event, ws_tx);
            }
        }
    }
    ProjectedTurn {
        turn_id,
        record_integrity_lost,
    }
}

/// Forward an approval lifecycle `AgentEvent` to the WebSocket sender for
/// this session.
///
/// Approval events are session-scoped: they go only to the client connected
/// to the agent that raised the gate. Calling `state.broadcast.send` instead
/// would deliver to every connected client — this function deliberately uses
/// the per-turn `ws_tx` channel to preserve that isolation.
///
/// Returns `true` when the event was an approval variant and was forwarded;
/// `false` for all other event types.
pub(crate) fn route_approval_agent_event(
    event: AgentEvent,
    ws_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) -> bool {
    match event {
        AgentEvent::ApprovalRequired {
            approval_id,
            turn_id,
            agent_id,
            kind,
            tool_name,
            tool_arguments,
            hook_name: _, // hook identity is internal — the approval UI doesn't need it
            prompt,
            context,
            timeout_at,
            timeout_policy,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalRequired {
                approval_id,
                turn_id,
                agent_id,
                kind,
                tool_name,
                tool_arguments,
                prompt,
                context,
                timeout_at,
                timeout_policy: timeout_policy.to_string(),
            });
            true
        }
        AgentEvent::ApprovalConfirmed {
            approval_id,
            decision,
            reason,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id,
                decision: decision.to_string(),
                reason,
            });
            true
        }
        AgentEvent::ApprovalExpired {
            approval_id,
            timeout_policy,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalExpired {
                approval_id,
                timeout_policy: timeout_policy.to_string(),
            });
            true
        }
        _ => false,
    }
}
