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
                base_url: device.base_url,
                workspace_root: device.workspace_root,
                capabilities: device.capabilities,
                registered_at: device.registered_at,
                state: device_presence
                    .map(|record| record.state.clone())
                    .unwrap_or_else(|| "connected".to_string()),
                last_seen_at: device_presence.map(|record| record.last_seen_at),
                disconnected_at: device_presence.and_then(|record| record.disconnected_at),
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
            .devices
            .iter()
            .any(|device| device.state != "connected")
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
    let topics = state.topic_store.list_all().await?;
    let active_connections = state.active_connections.lock().await.clone();
    let active_turns = state.active_turns.lock().await.clone();
    let mut sessions = Vec::new();

    for topic in topics {
        for session in state.storage.list_by_topic(topic.id).await? {
            let preview = state
                .storage
                .get_latest_for_session(session.id)
                .await?
                .map(|message| message.content);
            let has_interrupted_turn = has_interrupted_turn(state, session.id).await?;

            sessions.push(OperatorSessionSummary {
                id: session.id,
                topic_id: session.topic_id,
                topic_name: topic.name.clone(),
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
