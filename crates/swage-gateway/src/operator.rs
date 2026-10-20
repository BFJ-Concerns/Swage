use std::collections::{HashMap, HashSet};

use swage_core::GatewayError;
use swage_protocol::{
    client::workflow::WorkflowRunTerminalAccount, hosting::OperatorSessionEnding,
};
use swage_storage::{ApprovalState, SessionListing, SessionOrigin, TurnState};

use crate::{
    channel_gateway::{ChannelStatusListResponse, list_channel_statuses},
    device::list_devices,
    handlers::{ApprovalListResponse, ApprovalSummary},
    presence::snapshot_presence,
    schedule::list_schedules,
    state::AppState,
};

pub use swage_protocol::hosting::{
    OperatorDeviceSummary, OperatorGatewaySummary, OperatorOverviewResponse, OperatorSessionSummary,
};

pub async fn operator_overview(state: &AppState) -> Result<OperatorOverviewResponse, GatewayError> {
    let presence = snapshot_presence(state).await;
    let channels = list_channel_statuses(state).await;
    let approvals = list_pending_approvals(state).await?;
    let schedules = list_schedules(state).await?;
    let devices = list_devices(state).await;
    let sessions = list_operator_sessions(state).await?;

    let devices: Vec<OperatorDeviceSummary> = devices
        .devices
        .into_iter()
        .map(|device| {
            let device_presence = presence
                .devices
                .iter()
                .find(|record| record.device_id == device.device_id);
            OperatorDeviceSummary {
                device_id: device.device_id,
                label: device
                    .label
                    .or_else(|| device_presence.and_then(|record| record.label.clone())),
                kind: device.kind,
                workspace_root: device.workspace_root,
                capabilities: device.capabilities,
                registered_at: device.registered_at,
                // A registered device with no presence record has never
                // dialled in since the gateway started.
                state: device_presence
                    .map(|record| record.state.clone())
                    .unwrap_or_else(|| "disconnected".to_string()),
                last_seen_at: device_presence.map(|record| record.last_seen_at),
                disconnected_at: device_presence.and_then(|record| record.disconnected_at),
            }
        })
        .collect();

    let active_turn_count = sessions
        .iter()
        .filter(|session| session.has_running_turn)
        .count();
    let gateway = OperatorGatewaySummary {
        status: presence.gateway.status.clone(),
        health: classify_gateway_health(&presence, &channels, &devices, active_turn_count)
            .to_string(),
        shutdown_requested: presence.gateway.shutdown_requested,
        seconds_since_activity: presence.gateway.seconds_since_activity,
        active_turn_count,
        active_connection_count: presence.gateway.active_connection_count,
    };

    Ok(OperatorOverviewResponse {
        gateway,
        sessions,
        approvals: approvals.approvals,
        schedules,
        channels,
        devices,
    })
}

/// A registered device that is not connected — never dialled in, stale, or
/// disconnected — degrades the gateway, as does a degraded channel.
fn classify_gateway_health(
    presence: &crate::presence::PresenceSnapshotResponse,
    channels: &ChannelStatusListResponse,
    devices: &[OperatorDeviceSummary],
    active_turn_count: usize,
) -> &'static str {
    if presence.gateway.shutdown_requested
        || devices.iter().any(|device| device.state != "connected")
        || channels
            .channels
            .iter()
            .any(|channel| channel.status == "degraded")
    {
        "degraded"
    } else if active_turn_count > 0 {
        "busy"
    } else {
        "healthy"
    }
}

async fn list_operator_sessions(
    state: &AppState,
) -> Result<Vec<OperatorSessionSummary>, GatewayError> {
    let scopes: HashMap<_, _> = state
        .topic_store
        .list_scopes()
        .await?
        .into_iter()
        .map(|topic| (topic.id, topic))
        .collect();
    let active_connections = state.active_connections.lock().await.clone();
    let active_turns = state.active_turns.lock().await.clone();
    let activities = state
        .storage
        .list_recent_sessions(SessionListing {
            include_archived: true,
            ..SessionListing::default()
        })
        .await?;
    let mut sessions = Vec::with_capacity(activities.len());
    let mut worker_endings = HashMap::new();
    let mut read_runs = HashSet::new();

    for activity in activities {
        let session = activity.session;
        if let Some(topic) = scopes.get(&session.topic_id) {
            let mut latest_turn = None;
            let mut has_processing_turn = false;
            for thread in state.storage.list_by_session(session.id).await? {
                for turn in state.storage.list_turns_by_thread(thread.id).await? {
                    has_processing_turn |= matches!(
                        turn.state,
                        TurnState::Processing | TurnState::Delegating | TurnState::AwaitingApproval
                    );
                    if latest_turn
                        .as_ref()
                        .is_none_or(|latest: &swage_storage::Turn| {
                            turn.created_at > latest.created_at
                        })
                    {
                        latest_turn = Some(turn);
                    }
                }
            }
            let running_detected = active_turns.contains_key(&session.id) || has_processing_turn;
            if let SessionOrigin::WorkflowWorker { run_id } = session.origin
                && read_runs.insert(run_id)
                && let Some(run) = state.storage.get_workflow_run(run_id).await?
                && let Some(account) = run.terminal_account
            {
                let account: WorkflowRunTerminalAccount =
                    serde_json::from_value(account).map_err(|error| {
                        GatewayError::ConfigError(format!("invalid run account: {error}"))
                    })?;
                let journal = crate::workflow::journal_with_live_workers(
                    state,
                    run_id,
                    state.storage.list_workflow_journal(run_id).await?,
                )
                .await
                .map_err(GatewayError::ConfigError)?;
                let cancelled_workers = if matches!(
                    account.ending,
                    swage_protocol::client::workflow::WorkflowRunEnding::Cancelled
                ) {
                    &account.workers.cut_short[..]
                } else {
                    &[]
                };
                for (workers, ending) in [
                    (&account.workers.failed[..], OperatorSessionEnding::Failed),
                    (cancelled_workers, OperatorSessionEnding::Cancelled),
                ] {
                    for worker in workers {
                        if let Some(child_id) = journal
                            .iter()
                            .find(|entry| entry.id == worker.journal_entry_id)
                            .and_then(|entry| entry.agent_call.as_ref())
                            .and_then(|call| call.session_id)
                        {
                            worker_endings.insert(child_id, ending);
                        }
                    }
                }
            }
            let has_running_turn = running_detected && !worker_endings.contains_key(&session.id);
            let ending = if has_running_turn {
                None
            } else {
                worker_endings.get(&session.id).copied().or_else(|| {
                    latest_turn.as_ref().and_then(|turn| match turn.state {
                        TurnState::Cancelled => Some(OperatorSessionEnding::Cancelled),
                        TurnState::Failed => Some(OperatorSessionEnding::Failed),
                        TurnState::Interrupted => Some(OperatorSessionEnding::Interrupted),
                        _ => None,
                    })
                })
            };
            sessions.push(OperatorSessionSummary {
                id: session.id,
                topic_id: topic.client_topic_id(),
                topic_name: if topic.is_general {
                    "General".to_string()
                } else {
                    topic.name.clone()
                },
                created_at: session.created_at,
                last_activity: session.updated_at,
                message_preview: activity.latest_message,
                has_interrupted_turn: matches!(ending, Some(OperatorSessionEnding::Interrupted)),
                has_active_connection: active_connections.contains(&session.id),
                has_running_turn,
                ending,
            });
        }
    }

    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_activity));
    Ok(sessions)
}

async fn list_pending_approvals(state: &AppState) -> Result<ApprovalListResponse, GatewayError> {
    let approvals = state.storage.list_all_pending().await?;
    let summaries = approvals
        .into_iter()
        .filter(|approval| approval.state == ApprovalState::Pending)
        .map(|approval| ApprovalSummary {
            id: approval.id,
            turn_id: approval.turn_id,
            agent_id: approval.agent_id,
            kind: approval.kind.to_string(),
            tool_name: approval.tool_name,
            prompt: approval.prompt,
            timeout_at: Some(approval.timeout_at.to_rfc3339()),
        })
        .collect();
    Ok(ApprovalListResponse {
        approvals: summaries,
    })
}
