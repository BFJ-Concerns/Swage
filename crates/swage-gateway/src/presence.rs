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
    /// The device connection the record reflects, so a connection that
    /// ends after a newer one took over cannot mark the newer one gone.
    pub connection_id: Uuid,
}

#[derive(Debug, Default)]
pub struct PresenceRuntime {
    pub clients: HashMap<Uuid, ClientPresenceRecord>,
    pub devices: HashMap<String, DevicePresenceRecord>,
}

pub use swage_protocol::hosting::{
    ClientPresenceResponse, DevicePresenceResponse, PresenceGatewayResponse,
    PresenceSnapshotResponse,
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

/// A device was heard from — on connecting, and on every heartbeat it sends
/// over its connection. The record reads connected until `stale_after_secs`
/// pass in silence, and disconnected once its connection ends.
pub async fn upsert_device_heartbeat(
    state: &AppState,
    device_id: &str,
    label: Option<String>,
    stale_after_secs: u64,
    connection_id: Uuid,
) {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    let existing = presence.devices.get(device_id).cloned();
    let record = DevicePresenceRecord {
        device_id: device_id.to_owned(),
        label: label.or_else(|| existing.as_ref().and_then(|value| value.label.clone())),
        registered_at: existing.as_ref().map_or(now, |value| value.registered_at),
        last_seen_at: now,
        disconnected_at: None,
        stale_after_secs,
        connection_id,
    };
    presence.devices.insert(device_id.to_owned(), record);
}

/// Mark a device disconnected. With a `connection_id`, only the record
/// that connection wrote is stamped — a newer connection's record stands;
/// without one (an operator's disconnect) the record is stamped whatever
/// connection it reflects.
pub async fn disconnect_device(
    state: &AppState,
    device_id: &str,
    connection_id: Option<Uuid>,
) -> bool {
    let now = Utc::now();
    let mut presence = state.presence.lock().await;
    if let Some(device) = presence.devices.get_mut(device_id) {
        if connection_id.is_some_and(|id| id != device.connection_id) {
            return false;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        last_seen_at: DateTime<Utc>,
        disconnected_at: Option<DateTime<Utc>>,
    ) -> DevicePresenceRecord {
        DevicePresenceRecord {
            device_id: "device-a".into(),
            label: None,
            registered_at: last_seen_at,
            last_seen_at,
            disconnected_at,
            stale_after_secs: 30,
            connection_id: Uuid::from_u128(1),
        }
    }

    #[test]
    fn a_device_reads_connected_then_stale_in_silence_and_disconnected_once_closed() {
        let now = Utc::now();
        assert_eq!(
            device_state_at(&record(now, None), now),
            PresenceState::Connected
        );
        assert_eq!(
            device_state_at(&record(now, None), now + chrono::Duration::seconds(29)),
            PresenceState::Connected
        );
        assert_eq!(
            device_state_at(&record(now, None), now + chrono::Duration::seconds(31)),
            PresenceState::Stale
        );
        assert_eq!(
            device_state_at(&record(now, Some(now)), now),
            PresenceState::Disconnected
        );
    }
}
