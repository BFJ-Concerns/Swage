use std::collections::HashMap;

use swage_core::GatewayError;
use swage_storage::{ApprovalState, SessionListing};

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

    let gateway = OperatorGatewaySummary {
        status: presence.gateway.status.clone(),
        health: classify_gateway_health(&presence, &channels, &devices).to_string(),
        shutdown_requested: presence.gateway.shutdown_requested,
        seconds_since_activity: presence.gateway.seconds_since_activity,
        active_turn_count: presence.gateway.active_turn_count,
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
) -> &'static str {
    if presence.gateway.shutdown_requested
        || devices.iter().any(|device| device.state != "connected")
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

    for activity in activities {
        let session = activity.session;
        if let Some(topic) = scopes.get(&session.topic_id) {
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
                has_interrupted_turn: activity.has_interrupted_turn,
                has_active_connection: active_connections.contains(&session.id),
                has_running_turn: active_turns.contains_key(&session.id),
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
