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

/// Scope filter for durable agent-definition reads and deletes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct AgentDefinitionScopeQuery {
    pub space_id: Option<Uuid>,
}

/// Whole definition supplied when creating a durable agent.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateAgentDefinitionRequest {
    pub id: String,
    pub space_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    #[ts(type = "unknown")]
    pub tool_permissions: Option<serde_json::Value>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[ts(type = "number | null")]
    pub output_max_retries: Option<u32>,
}

/// Whole replacement supplied when updating a durable agent definition.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdateAgentDefinitionRequest {
    pub space_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    #[ts(type = "unknown")]
    pub tool_permissions: Option<serde_json::Value>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[ts(type = "number | null")]
    pub output_max_retries: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct AgentDefinitionResponse {
    pub id: String,
    pub space_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    #[ts(type = "unknown")]
    pub tool_permissions: Option<serde_json::Value>,
    pub skills: Vec<String>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[ts(type = "number | null")]
    pub output_max_retries: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct AgentDefinitionListResponse {
    pub definitions: Vec<AgentDefinitionResponse>,
}

/// An agent definition available when creating a session in one space.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct EffectiveAgentDefinitionResponse {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    #[ts(type = "unknown")]
    pub tool_permissions: Option<serde_json::Value>,
    pub skills: Vec<String>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[ts(type = "number | null")]
    pub output_max_retries: Option<u32>,
    pub is_builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct EffectiveAgentDefinitionListResponse {
    pub definitions: Vec<EffectiveAgentDefinitionResponse>,
}

/// The outcome of an agent-definition mutation. The full account of the
/// change — prior and new state under a named action — lives in the audit
/// trail.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct AgentDefinitionMutationResponse {
    pub agent_id: String,
    pub space_id: Option<Uuid>,
    pub summary: String,
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
    operator_overview: OperatorOverviewResponse,
    agent_definition_scope: AgentDefinitionScopeQuery,
    agent_definition_list: AgentDefinitionListResponse,
    effective_agent_definition_list: EffectiveAgentDefinitionListResponse,
    agent_definition_create: CreateAgentDefinitionRequest,
    agent_definition_update: UpdateAgentDefinitionRequest,
    agent_definition_mutation: AgentDefinitionMutationResponse,
    devices: device::DeviceListResponse,
    knowledge_mirror: KnowledgeMirrorRegenerateResponse,
}
