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
    OperatorDeviceSummary, OperatorGatewaySummary, OperatorOverviewResponse,
    OperatorSessionSummary, OperatorWorkflowRunSummary,
};

pub async fn operator_overview(state: &AppState) -> Result<OperatorOverviewResponse, GatewayError> {
    let presence = snapshot_presence(state).await;
    let channels = list_channel_statuses(state).await;
    let approvals = list_pending_approvals(state).await?;
    let schedules = list_schedules(state).await?;
    let devices = list_devices(state).await;
    let sessions = list_operator_sessions(state).await?;
    let workflow_runs = list_operator_runs(state).await?;

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
        workflow_runs,
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
    let mut scheduled_turns = HashMap::new();
    for schedule in state.storage.list_schedules().await? {
        for run in state.storage.list_schedule_runs(schedule.id).await? {
            if run.state == swage_protocol::client::schedule::ScheduleRunState::Running
                && let Some(turn_id) = run.turn_id
            {
                scheduled_turns.insert(turn_id, schedule.id);
            }
        }
    }
    let mut sessions = Vec::with_capacity(activities.len());
    let mut worker_endings = HashMap::new();
    let mut read_runs = HashSet::new();

    for activity in activities {
        let session = activity.session;
        if let Some(topic) = scopes.get(&session.topic_id) {
            let current_turn = activity.active_turn.or(activity.latest_turn);
            let origin = current_turn
                .as_ref()
                .and_then(|turn| scheduled_turns.get(&turn.id))
                .map(|schedule_id| SessionOrigin::ScheduleRun {
                    schedule_id: *schedule_id,
                })
                .unwrap_or_else(|| session.origin.clone());
            let running_detected =
                active_turns.contains_key(&session.id) || activity.has_processing_turn;
            if let SessionOrigin::WorkflowWorker { run_id } = origin
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
                        if let Some(worker_turn_id) = journal
                            .iter()
                            .find(|entry| entry.id == worker.journal_entry_id)
                            .and_then(|entry| entry.agent_call.as_ref())
                            .and_then(|call| call.turn_id)
                        {
                            worker_endings.insert(worker_turn_id, ending);
                        }
                    }
                }
            }
            // A held worker can serve several calls in the same session.
            let worker_ending = current_turn
                .as_ref()
                .and_then(|turn| worker_endings.get(&turn.id))
                .copied();
            let has_running_turn = running_detected && worker_ending.is_none();
            let ending = if has_running_turn {
                None
            } else {
                worker_ending.or_else(|| {
                    current_turn.as_ref().and_then(|turn| match turn.state {
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
                agent_id: session.agent_id.clone(),
                parent_session_id: parent_session(state, &origin).await?,
                activity: if has_running_turn {
                    current_turn
                        .as_ref()
                        .map(|turn| turn.state.to_string())
                        .unwrap_or_else(|| "processing".into())
                } else {
                    "idle".into()
                },
                workflow_run_id: match origin {
                    SessionOrigin::WorkflowWorker { run_id } => Some(run_id),
                    _ => None,
                },
                source: match origin {
                    SessionOrigin::ScheduleRun { schedule_id } => {
                        Some(format!("Schedule {schedule_id}"))
                    }
                    SessionOrigin::WorkflowWorker { run_id } => {
                        Some(format!("Workflow run {run_id}"))
                    }
                    _ => None,
                },
            });
        }
    }

    sessions.sort_by_key(|session| {
        (
            !session.has_running_turn,
            std::cmp::Reverse(session.last_activity),
        )
    });
    Ok(sessions)
}

async fn parent_session(
    state: &AppState,
    origin: &SessionOrigin,
) -> Result<Option<uuid::Uuid>, GatewayError> {
    match origin {
        SessionOrigin::Delegation { parent_session_id } => Ok(Some(*parent_session_id)),
        SessionOrigin::WorkflowWorker { run_id } => {
            match state.storage.get_workflow_run(*run_id).await? {
                Some(run) => run_parent_session(state, &run).await,
                None => Ok(None),
            }
        }
        SessionOrigin::ScheduleRun { schedule_id } => {
            // Creation provenance lives in the unified audit trail. Schedules
            // made by a person have no spawning session.
            let events = state
                .storage
                .list_audit_events_filtered(swage_storage::AuditEventFilter {
                    entity_kind: Some("schedule".into()),
                    entity_id: Some(schedule_id.to_string()),
                    event_kind: Some("configuration_change".into()),
                    limit: usize::MAX,
                    ..Default::default()
                })
                .await?;
            Ok(events
                .into_iter()
                .find(|event| {
                    event
                        .metadata
                        .get("action")
                        .and_then(serde_json::Value::as_str)
                        == Some("create_schedule")
                })
                .and_then(|event| event.session_id))
        }
        SessionOrigin::MemoryCuration {
            attended_session_id,
        } => Ok(*attended_session_id),
        SessionOrigin::Direct => Ok(None),
    }
}

async fn run_parent_session(
    state: &AppState,
    run: &swage_storage::WorkflowRunRecord,
) -> Result<Option<uuid::Uuid>, GatewayError> {
    if run.dispatching_session_id.is_some() {
        return Ok(run.dispatching_session_id);
    }
    let mut parent = run.parent_run_id;
    let mut visited = HashSet::new();
    while let Some(id) = parent {
        if !visited.insert(id) {
            return Err(GatewayError::ConfigError(
                "cycle in workflow run parents".into(),
            ));
        }
        let Some(record) = state.storage.get_workflow_run(id).await? else {
            return Ok(None);
        };
        if record.dispatching_session_id.is_some() {
            return Ok(record.dispatching_session_id);
        }
        parent = record.parent_run_id;
    }
    Ok(None)
}

async fn list_operator_runs(
    state: &AppState,
) -> Result<Vec<OperatorWorkflowRunSummary>, GatewayError> {
    let mut runs = Vec::new();
    for topic in state.topic_store.list_scopes().await? {
        for run in state.storage.list_workflow_runs(topic.id).await? {
            if !matches!(run.state.as_str(), "running" | "parked") {
                continue;
            }
            let parent_session_id = run_parent_session(state, &run).await?;
            let agent_id = if let Some(parent_id) = parent_session_id {
                state
                    .storage
                    .get_session(parent_id)
                    .await?
                    .map(|session| session.agent_id)
            } else {
                run.dispatch_bound_agent_ids.last().cloned()
            };
            runs.push(OperatorWorkflowRunSummary {
                id: run.id,
                name: run.script_name.unwrap_or_else(|| "Inline workflow".into()),
                activity: if run.state == "parked" {
                    "Waiting"
                } else {
                    "Executing"
                }
                .into(),
                state: run.state,
                agent_id,
                parent_session_id,
                parent_run_id: run.parent_run_id,
            });
        }
    }
    Ok(runs)
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
