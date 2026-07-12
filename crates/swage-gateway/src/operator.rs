use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::ApprovalState;

use crate::{
    channel_gateway::{ChannelStatusListResponse, list_channel_statuses},
    device::list_devices,
    handlers::{ApprovalListResponse, ApprovalSummary},
    presence::snapshot_presence,
    schedule::list_schedules,
    state::AppState,
};

#[derive(Debug, Clone, Serialize)]
pub struct OperatorGatewaySummary {
    pub status: String,
    pub health: String,
    pub shutdown_requested: bool,
    pub seconds_since_activity: u64,
    pub active_turn_count: usize,
    pub active_connection_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorSessionSummary {
    pub id: Uuid,
    pub space_id: Uuid,
    pub space_name: String,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_preview: Option<String>,
    pub has_interrupted_turn: bool,
    pub has_active_connection: bool,
    pub has_running_turn: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorDeviceSummary {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: Option<String>,
    pub workspace_root: String,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
    pub state: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorOverviewResponse {
    pub gateway: OperatorGatewaySummary,
    pub sessions: Vec<OperatorSessionSummary>,
    pub approvals: Vec<ApprovalSummary>,
    pub schedules: swage_protocol::client::schedule::ScheduleListResponse,
    pub channels: ChannelStatusListResponse,
    pub devices: Vec<OperatorDeviceSummary>,
}

pub async fn operator_overview(state: &AppState) -> Result<OperatorOverviewResponse, GatewayError> {
    let presence = snapshot_presence(state).await;
    let channels = list_channel_statuses(state).await;
    let approvals = list_pending_approvals(state).await?;
    let schedules = list_schedules(state).await?;
    let devices = list_devices(state).await;
    let sessions = list_operator_sessions(state).await?;

    let gateway = OperatorGatewaySummary {
        status: presence.gateway.status.clone(),
        health: classify_gateway_health(&presence, &channels).to_string(),
        shutdown_requested: presence.gateway.shutdown_requested,
        seconds_since_activity: presence.gateway.seconds_since_activity,
        active_turn_count: presence.gateway.active_turn_count,
        active_connection_count: presence.gateway.active_connection_count,
    };

    let devices = devices
        .devices
        .into_iter()
        .map(|device| {
            let machine_presence = presence
                .machines
                .iter()
                .find(|machine| machine.machine_id == device.device_id);
            OperatorDeviceSummary {
                device_id: device.device_id,
                label: device
                    .label
                    .or_else(|| machine_presence.and_then(|machine| machine.label.clone())),
                kind: device.kind,
                base_url: device.base_url,
                workspace_root: device.workspace_root,
                capabilities: device.capabilities,
                registered_at: device.registered_at,
                state: machine_presence
                    .map(|machine| machine.state.clone())
                    .unwrap_or_else(|| "connected".to_string()),
                last_seen_at: machine_presence.map(|machine| machine.last_seen_at),
                disconnected_at: machine_presence.and_then(|machine| machine.disconnected_at),
            }
        })
        .collect();

    Ok(OperatorOverviewResponse {
        gateway,
        sessions,
        approvals: approvals.approvals,
        schedules,
        channels,
        devices,
    })
}

fn classify_gateway_health(
    presence: &crate::presence::PresenceSnapshotResponse,
    channels: &ChannelStatusListResponse,
) -> &'static str {
    if presence.gateway.shutdown_requested
        || presence
            .machines
            .iter()
            .any(|machine| machine.state != "connected")
        || channels
            .channels
            .iter()
            .any(|channel| channel.status == "degraded")
    {
        "degraded"
    } else if presence.gateway.active_turn_count > 0 {
        "busy"
    } else {
        "healthy"
    }
}

async fn list_operator_sessions(
    state: &AppState,
) -> Result<Vec<OperatorSessionSummary>, GatewayError> {
    let spaces = state.space_store.list_all().await?;
    let active_connections = state.active_connections.lock().await.clone();
    let active_turns = state.active_turns.lock().await.clone();
    let mut sessions = Vec::new();

    for space in spaces {
        for session in state.storage.list_by_space(space.id).await? {
            let preview = state
                .storage
                .get_latest_for_session(session.id)
                .await?
                .map(|message| message.content);
            let has_interrupted_turn = has_interrupted_turn(state, session.id).await?;

            sessions.push(OperatorSessionSummary {
                id: session.id,
                space_id: session.space_id,
                space_name: space.name.clone(),
                created_at: session.created_at,
                last_activity: session.updated_at,
                message_preview: preview,
                has_interrupted_turn,
                has_active_connection: active_connections.contains(&session.id),
                has_running_turn: active_turns.contains_key(&session.id),
            });
        }
    }

    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_activity));
    Ok(sessions)
}

async fn has_interrupted_turn(state: &AppState, session_id: Uuid) -> Result<bool, GatewayError> {
    let threads = state.storage.list_by_session(session_id).await?;
    for thread in threads {
        let turns = state.storage.list_turns_by_thread(thread.id).await?;
        if turns
            .into_iter()
            .any(|turn| turn.state == swage_storage::TurnState::Interrupted)
        {
            return Ok(true);
        }
    }
    Ok(false)
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
