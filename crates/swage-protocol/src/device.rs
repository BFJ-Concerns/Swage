//! Contract spoken by remote execution devices.

use std::{collections::HashMap, path::PathBuf};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, JsonSchema)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
    #[ts(type = "Record<string, unknown>")]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, JsonSchema)]
pub struct SerializableToolContext {
    pub agent_id: String,
    pub session_id: String,
    pub working_directory: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
    pub user_info: Option<UserInfo>,
    pub call_id: Option<String>,
    pub turn_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct DeviceResponse {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: Option<String>,
    pub workspace_root: String,
    pub workspace_roots: Vec<String>,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct DeviceListResponse {
    pub devices: Vec<DeviceResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct RegisterDeviceRequest {
    pub device_id: String,
    pub label: Option<String>,
    #[serde(default = "default_remote_device_kind")]
    pub kind: String,
    pub base_url: String,
    pub shared_token_secret_ref: String,
    pub workspace_root: String,
    #[serde(default)]
    pub workspace_roots: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct RemoteToolExecutionRequest {
    pub shared_token: String,
    pub tool_name: String,
    #[ts(type = "unknown")]
    pub params: Value,
    pub context: SerializableToolContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct RemoteToolExecutionResponse {
    #[ts(type = "unknown")]
    pub output: Value,
}

fn default_remote_device_kind() -> String {
    "attached_device".to_string()
}

#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct DeviceSurface {
    register: RegisterDeviceRequest,
    list: DeviceListResponse,
    execute: RemoteToolExecutionRequest,
    execute_response: RemoteToolExecutionResponse,
}
