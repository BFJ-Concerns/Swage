use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    Connected,
    Disconnected,
    Stale,
}

impl std::fmt::Display for PresenceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            PresenceState::Connected => "connected",
            PresenceState::Disconnected => "disconnected",
            PresenceState::Stale => "stale",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone)]
pub struct ClientPresenceRecord {
    pub session_id: Uuid,
    pub state: PresenceState,
    pub connected_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct DevicePresenceRecord {
    pub device_id: String,
    pub label: Option<String>,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub stale_after_secs: u64,
}

#[derive(Debug, Default)]
pub struct PresenceRuntime {
    pub clients: HashMap<Uuid, ClientPresenceRecord>,
    pub devices: HashMap<String, DevicePresenceRecord>,
}

pub use swage_protocol::hosting::{
    ClientPresenceResponse, DeviceHeartbeatRequest, DevicePresenceResponse,
    PresenceGatewayResponse, PresenceSnapshotResponse,
};

pub async fn snapshot_presence(state: &AppState) -> PresenceSnapshotResponse {
    let now = Utc::now();
    let presence = state.presence.lock().await;
    let clients = presence
        .clients
        .values()
        .map(|record| ClientPresenceResponse {
            session_id: record.session_id,
            state: record.state.to_string(),
            connected_at: record.connected_at,
            last_seen_at: record.last_seen_at,
            disconnected_at: record.disconnected_at,
        })
        .collect::<Vec<_>>();
    let devices = presence
        .devices
        .values()
        .cloned()
        .map(|record| {
            let state = device_state_at(&record, now).to_string();
            DevicePresenceResponse {
                device_id: record.device_id,
                label: record.label,
                state,
                registered_at: record.registered_at,
                last_seen_at: record.last_seen_at,
                disconnected_at: record.disconnected_at,
                stale_after_secs: record.stale_after_secs,
            }
        })
        .collect::<Vec<_>>();
    drop(presence);

    let active_turn_count = state.active_turns.lock().await.len();
    let active_connection_count = state.active_connections.lock().await.len();
    PresenceSnapshotResponse {
        gateway: PresenceGatewayResponse {
            status: if state.shutdown_signal.is_cancelled() {
                "shutting_down".into()
            } else {
                "running".into()
            },
            shutdown_requested: state.shutdown_signal.is_cancelled(),
            seconds_since_activity: state.activity_tracker.seconds_since_last_activity(),
            active_turn_count,
            active_connection_count,
        },
        clients,
        devices,
    }
}

pub async fn record_client_connected(state: &AppState, session_id: Uuid) {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    presence.clients.insert(
        session_id,
        ClientPresenceRecord {
            session_id,
            state: PresenceState::Connected,
            connected_at: Some(now),
            last_seen_at: Some(now),
            disconnected_at: None,
        },
    );
}

pub async fn record_client_seen(state: &AppState, session_id: Uuid) {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    let entry = presence
        .clients
        .entry(session_id)
        .or_insert(ClientPresenceRecord {
            session_id,
            state: PresenceState::Connected,
            connected_at: Some(now),
            last_seen_at: Some(now),
            disconnected_at: None,
        });
    entry.state = PresenceState::Connected;
    entry.connected_at.get_or_insert(now);
    entry.last_seen_at = Some(now);
    entry.disconnected_at = None;
}

pub async fn record_client_disconnected(state: &AppState, session_id: Uuid) {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    let entry = presence
        .clients
        .entry(session_id)
        .or_insert(ClientPresenceRecord {
            session_id,
            state: PresenceState::Disconnected,
            connected_at: None,
            last_seen_at: None,
            disconnected_at: Some(now),
        });
    entry.state = PresenceState::Disconnected;
    entry.disconnected_at = Some(now);
}

pub async fn upsert_device_heartbeat(state: &AppState, request: DeviceHeartbeatRequest) {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    let existing = presence.devices.get(&request.device_id).cloned();
    let record = DevicePresenceRecord {
        device_id: request.device_id.clone(),
        label: request
            .label
            .or_else(|| existing.as_ref().and_then(|value| value.label.clone())),
        registered_at: existing.as_ref().map_or(now, |value| value.registered_at),
        last_seen_at: now,
        disconnected_at: None,
        stale_after_secs: request.stale_after_secs,
    };
    presence.devices.insert(request.device_id, record);
}

pub async fn disconnect_device(state: &AppState, device_id: &str) -> bool {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    if let Some(device) = presence.devices.get_mut(device_id) {
        device.disconnected_at = Some(now);
        return true;
    }
    false
}

fn device_state_at(record: &DevicePresenceRecord, now: DateTime<Utc>) -> PresenceState {
    if record.disconnected_at.is_some() {
        PresenceState::Disconnected
    } else if now
        .signed_duration_since(record.last_seen_at)
        .num_seconds()
        .max(0) as u64
        > record.stale_after_secs
    {
        PresenceState::Stale
    } else {
        PresenceState::Connected
    }
}
