//! Hosting and operator control-plane contract.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::{channel::ChannelStatusListResponse, client::approval, client::schedule, device};

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PresenceGatewayResponse {
    pub status: String,
    pub shutdown_requested: bool,
    #[ts(type = "number")]
    pub seconds_since_activity: u64,
    pub active_turn_count: usize,
    pub active_connection_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ClientPresenceResponse {
    pub session_id: Uuid,
    pub state: String,
    pub connected_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct MachinePresenceResponse {
    pub machine_id: String,
    pub label: Option<String>,
    pub state: String,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    #[ts(type = "number")]
    pub stale_after_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PresenceSnapshotResponse {
    pub gateway: PresenceGatewayResponse,
    pub clients: Vec<ClientPresenceResponse>,
    pub machines: Vec<MachinePresenceResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct MachineHeartbeatRequest {
    pub machine_id: String,
    pub label: Option<String>,
    #[serde(default = "default_machine_stale_after_secs")]
    #[ts(type = "number")]
    pub stale_after_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ApprovalSummary {
    pub id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub kind: String,
    pub tool_name: Option<String>,
    pub prompt: String,
    pub timeout_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ApprovalListResponse {
    pub approvals: Vec<ApprovalSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct ApprovalListQuery {
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ApprovalDecisionRequest {
    pub decision: approval::ApprovalDecisionValue,
    #[serde(default)]
    pub scope: approval::ApprovalDecisionScope,
    pub reason: Option<String>,
}

/// Ask the operator control plane to undo a recorded configuration case.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct UndoConfigurationCaseRequest {
    pub case_id: Uuid,
}

/// The new change-case record produced by undoing a configuration case.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ConfigurationChangeResponse {
    pub version_id: Uuid,
    pub entity_kind: String,
    pub entity_id: String,
    pub summary: String,
    pub risk_band: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct OperatorGatewaySummary {
    pub status: String,
    pub health: String,
    pub shutdown_requested: bool,
    #[ts(type = "number")]
    pub seconds_since_activity: u64,
    pub active_turn_count: usize,
    pub active_connection_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct OperatorOverviewResponse {
    pub gateway: OperatorGatewaySummary,
    pub sessions: Vec<OperatorSessionSummary>,
    pub approvals: Vec<ApprovalSummary>,
    pub schedules: schedule::ScheduleListResponse,
    pub channels: ChannelStatusListResponse,
    pub devices: Vec<OperatorDeviceSummary>,
}

fn default_machine_stale_after_secs() -> u64 {
    30
}

/// Result of regenerating a space's knowledge-base browse mirror — a
/// disposable markdown projection of the database-authoritative store.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct KnowledgeMirrorRegenerateResponse {
    /// Absolute path of the regenerated mirror directory.
    pub mirror_root: String,
    pub files_written: u32,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct HostingSurface {
    presence: PresenceSnapshotResponse,
    heartbeat: MachineHeartbeatRequest,
    approvals: ApprovalListResponse,
    approval_decision: ApprovalDecisionRequest,
    undo_configuration_case: UndoConfigurationCaseRequest,
    configuration_change: ConfigurationChangeResponse,
    operator_overview: OperatorOverviewResponse,
    devices: device::DeviceListResponse,
    knowledge_mirror: KnowledgeMirrorRegenerateResponse,
}
