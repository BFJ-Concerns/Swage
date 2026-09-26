//! Contract spoken by remote execution devices.
//!
//! A device registers once — or redeems a pairing code — and from then on
//! reaches the gateway only by dialling out to it: the device opens the
//! connection, advertises what it can run, and the gateway invokes
//! operations over that same connection. Nothing listens on the device.
//! Every operation carries a correlation id; cancellation and heartbeats
//! are frames of their own, so the gateway observes an operation's result,
//! its cancellation, and the device's liveness from one record.

use std::{collections::HashMap, path::PathBuf};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
    #[ts(type = "Record<string, unknown>")]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SerializableToolContext {
    pub agent_id: String,
    pub session_id: String,
    pub working_directory: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
    pub user_info: Option<UserInfo>,
    pub call_id: Option<String>,
    pub turn_id: Option<Uuid>,
}

/// A registered device as the gateway lists it. `capabilities` are what the
/// device last advertised on connecting; empty until it first dials in.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DeviceResponse {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub workspace_root: String,
    pub workspace_roots: Vec<String>,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DeviceListResponse {
    pub devices: Vec<DeviceResponse>,
}

/// Register a device: its identity, the secret its connection credential
/// resolves against, and the workspace it may operate in. What it can run
/// is not registered — the device advertises that each time it connects.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct RegisterDeviceRequest {
    pub device_id: String,
    pub label: Option<String>,
    #[serde(default = "default_remote_device_kind")]
    #[ts(optional, as = "Option<_>")]
    pub kind: String,
    /// The gateway secret whose value the device presents as its shared
    /// token when it connects.
    pub shared_token_secret_ref: String,
    pub workspace_root: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub workspace_roots: Vec<String>,
}

/// Frames a device sends over the connection it dialled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum DeviceFrame {
    /// The first frame after connecting: who the device is, its credential,
    /// and what it can run. The gateway answers with `GatewayFrame::Welcome`
    /// or `GatewayFrame::Rejected` and closes.
    Hello {
        device_id: String,
        shared_token: String,
        label: Option<String>,
        capabilities: Vec<String>,
    },
    /// Sent every `heartbeat_interval_secs` from the welcome; the gateway
    /// marks a device stale when they stop and disconnected when the
    /// connection closes.
    Heartbeat {},
    /// The operation `correlation_id` finished with `output`.
    Result {
        correlation_id: Uuid,
        #[ts(type = "unknown")]
        output: Value,
    },
    /// The operation `correlation_id` failed on the device.
    Failure {
        correlation_id: Uuid,
        message: String,
    },
    /// The operation `correlation_id` was stopped at the gateway's request.
    Cancelled { correlation_id: Uuid },
}

/// Frames the gateway sends to a connected device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum GatewayFrame {
    /// The hello was accepted; the device is connected.
    Welcome {
        device_id: String,
        #[ts(type = "number")]
        heartbeat_interval_secs: u64,
    },
    /// The hello was refused; the gateway closes after this frame.
    Rejected { message: String },
    /// Run `tool_name` with `params` in `context`, answering with a
    /// `Result`, `Failure`, or `Cancelled` frame carrying `correlation_id`.
    Operation {
        correlation_id: Uuid,
        tool_name: String,
        #[ts(type = "unknown")]
        params: Value,
        context: Box<SerializableToolContext>,
    },
    /// Stop the operation `correlation_id` if it is still running; the
    /// device answers `Cancelled`, or `Result`/`Failure` if it had already
    /// finished.
    Cancel { correlation_id: Uuid },
}

fn default_remote_device_kind() -> String {
    "attached_device".to_string()
}

#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct DeviceSurface {
    register: RegisterDeviceRequest,
    list: DeviceListResponse,
    device_frame: DeviceFrame,
    gateway_frame: GatewayFrame,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_tagged_by_kind_and_round_trip() {
        let operation = GatewayFrame::Operation {
            correlation_id: Uuid::from_u128(7),
            tool_name: "shell".into(),
            params: serde_json::json!({ "command": "ls" }),
            context: Box::new(SerializableToolContext {
                agent_id: "gateway".into(),
                session_id: "s".into(),
                working_directory: PathBuf::from("/work"),
                allowed_paths: vec![],
                user_info: None,
                call_id: None,
                turn_id: None,
            }),
        };
        let json = serde_json::to_value(&operation).unwrap();
        assert_eq!(json["kind"], "operation");
        assert_eq!(
            serde_json::from_value::<GatewayFrame>(json).unwrap(),
            operation
        );

        let heartbeat: DeviceFrame = serde_json::from_str(r#"{"kind":"heartbeat"}"#).unwrap();
        assert_eq!(heartbeat, DeviceFrame::Heartbeat {});
    }
}
