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
pub struct MachinePresenceRecord {
    pub machine_id: String,
    pub label: Option<String>,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub stale_after_secs: u64,
}

#[derive(Debug, Default)]
pub struct PresenceRuntime {
    pub clients: HashMap<Uuid, ClientPresenceRecord>,
    pub machines: HashMap<String, MachinePresenceRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresenceGatewayResponse {
    pub status: String,
    pub shutdown_requested: bool,
    pub seconds_since_activity: u64,
    pub active_turn_count: usize,
    pub active_connection_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientPresenceResponse {
    pub session_id: Uuid,
    pub state: String,
    pub connected_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MachinePresenceResponse {
    pub machine_id: String,
    pub label: Option<String>,
    pub state: String,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub stale_after_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresenceSnapshotResponse {
    pub gateway: PresenceGatewayResponse,
    pub clients: Vec<ClientPresenceResponse>,
    pub machines: Vec<MachinePresenceResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MachineHeartbeatRequest {
    pub machine_id: String,
    pub label: Option<String>,
    #[serde(default = "default_machine_stale_after_secs")]
    pub stale_after_secs: u64,
}

fn default_machine_stale_after_secs() -> u64 {
    30
}

pub async fn snapshot_presence(state: &AppState) -> PresenceSnapshotResponse {
    let now = Utc::now();
    let presence = state.presence.lock().await;
    let clients = presence
        .clients
        .values()
        .cloned()
        .map(|record| ClientPresenceResponse {
            session_id: record.session_id,
            state: record.state.to_string(),
            connected_at: record.connected_at,
            last_seen_at: record.last_seen_at,
            disconnected_at: record.disconnected_at,
        })
        .collect::<Vec<_>>();
    let machines = presence
        .machines
        .values()
        .cloned()
        .map(|record| {
            let state = machine_state_at(&record, now).to_string();
            MachinePresenceResponse {
                machine_id: record.machine_id,
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
        machines,
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

pub async fn upsert_machine_heartbeat(state: &AppState, request: MachineHeartbeatRequest) {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    let existing = presence.machines.get(&request.machine_id).cloned();
    let record = MachinePresenceRecord {
        machine_id: request.machine_id.clone(),
        label: request
            .label
            .or_else(|| existing.as_ref().and_then(|value| value.label.clone())),
        registered_at: existing.as_ref().map_or(now, |value| value.registered_at),
        last_seen_at: now,
        disconnected_at: None,
        stale_after_secs: request.stale_after_secs,
    };
    presence.machines.insert(request.machine_id, record);
}

pub async fn disconnect_machine(state: &AppState, machine_id: &str) -> bool {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    if let Some(machine) = presence.machines.get_mut(machine_id) {
        machine.disconnected_at = Some(now);
        return true;
    }
    false
}

fn machine_state_at(record: &MachinePresenceRecord, now: DateTime<Utc>) -> PresenceState {
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
