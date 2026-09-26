//! Hosting and operator control-plane contract.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    channel::ChannelStatusListResponse,
    client::{approval, schedule},
    device,
};

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PresenceGatewayResponse {
    pub status: String,
    pub shutdown_requested: bool,
    #[ts(type = "number")]
    pub seconds_since_activity: u64,
    pub active_turn_count: usize,
    pub active_connection_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ClientPresenceResponse {
    pub session_id: Uuid,
    pub state: String,
    pub connected_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DevicePresenceResponse {
    pub device_id: String,
    pub label: Option<String>,
    pub state: String,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    #[ts(type = "number")]
    pub stale_after_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PresenceSnapshotResponse {
    pub gateway: PresenceGatewayResponse,
    pub clients: Vec<ClientPresenceResponse>,
    pub devices: Vec<DevicePresenceResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DeviceHeartbeatRequest {
    pub device_id: String,
    pub label: Option<String>,
    #[serde(default = "default_device_stale_after_secs")]
    #[ts(optional, as = "Option<f64>")]
    pub stale_after_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct ApprovalListResponse {
    pub approvals: Vec<ApprovalSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ApprovalListQuery {
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ApprovalDecisionRequest {
    pub decision: approval::ApprovalDecisionValue,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub scope: approval::ApprovalDecisionScope,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct OperatorSessionSummary {
    pub id: Uuid,
    /// Absent for a session in the general scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<Uuid>,
    /// The topic's name, or "General" for a session without one.
    pub topic_name: String,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_preview: Option<String>,
    pub has_interrupted_turn: bool,
    pub has_active_connection: bool,
    pub has_running_turn: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct OperatorOverviewResponse {
    pub gateway: OperatorGatewaySummary,
    pub sessions: Vec<OperatorSessionSummary>,
    pub approvals: Vec<ApprovalSummary>,
    pub schedules: schedule::ScheduleListResponse,
    pub channels: ChannelStatusListResponse,
    pub devices: Vec<OperatorDeviceSummary>,
}

/// One durable configuration change as recorded at the mutation boundary.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ConfigurationHistoryEvent {
    pub id: String,
    pub resource_kind: String,
    pub resource_id: String,
    pub action: String,
    pub source: String,
    pub session_id: Option<Uuid>,
    #[ts(type = "unknown")]
    pub prior_state: serde_json::Value,
    #[ts(type = "unknown")]
    pub new_state: serde_json::Value,
    pub recorded_at: DateTime<Utc>,
}

/// One recorded experiment selection, exposed only in operator history.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct VariantSelectionHistoryEvent {
    pub id: String,
    pub location_id: String,
    pub variant: String,
    pub reason: String,
    pub turn_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub recorded_at: DateTime<Utc>,
}

/// One turn whose persisted assembly report can be opened in the context inspector.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct AssemblyHistoryEntry {
    pub turn_id: Uuid,
    pub session_id: Uuid,
    pub topic_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub assembly_name: String,
}

/// Read-only history reconstructed from the local audit and context records.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct OperatorHistoryResponse {
    pub configuration_changes: Vec<ConfigurationHistoryEvent>,
    pub variant_selections: Vec<VariantSelectionHistoryEvent>,
    pub assembly_reports: Vec<AssemblyHistoryEntry>,
}

/// Scope filter for durable agent-definition reads and deletes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct AgentDefinitionScopeQuery {
    pub topic_id: Option<Uuid>,
}

/// Whole definition supplied when creating a durable agent.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct CreateAgentDefinitionRequest {
    pub id: String,
    pub topic_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    #[ts(type = "unknown")]
    pub tool_permissions: Option<serde_json::Value>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub skills: Vec<String>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[ts(optional, type = "number | null")]
    pub output_max_retries: Option<u32>,
}

/// Whole replacement supplied when updating a durable agent definition.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct UpdateAgentDefinitionRequest {
    pub topic_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    #[ts(type = "unknown")]
    pub tool_permissions: Option<serde_json::Value>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub skills: Vec<String>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[ts(optional, type = "number | null")]
    pub output_max_retries: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct AgentDefinitionResponse {
    pub id: String,
    pub topic_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    #[ts(type = "unknown")]
    pub tool_permissions: Option<serde_json::Value>,
    pub skills: Vec<String>,
    #[ts(type = "unknown")]
    pub output_schema: Option<serde_json::Value>,
    #[ts(optional, type = "number | null")]
    pub output_max_retries: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct AgentDefinitionListResponse {
    pub definitions: Vec<AgentDefinitionResponse>,
}

/// Where an effective agent definition comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum EffectiveAgentDefinitionSourceScope {
    BuiltIn,
    Global,
    Topic,
}

/// An agent definition available when creating a session in one topic.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
    #[ts(optional, type = "number | null")]
    pub output_max_retries: Option<u32>,
    pub is_builtin: bool,
    pub source_scope: EffectiveAgentDefinitionSourceScope,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct EffectiveAgentDefinitionListResponse {
    pub definitions: Vec<EffectiveAgentDefinitionResponse>,
}

/// The outcome of an agent-definition mutation. The full account of the
/// change — prior and new state under a named action — lives in the audit
/// trail.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct AgentDefinitionMutationResponse {
    pub agent_id: String,
    pub topic_id: Option<Uuid>,
    pub summary: String,
}

fn default_device_stale_after_secs() -> u64 {
    30
}

/// Result of regenerating a topic's knowledge-base browse mirror — a
/// disposable markdown projection of the database-authoritative store.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct KnowledgeMirrorRegenerateResponse {
    /// Absolute path of the regenerated mirror directory.
    pub mirror_root: String,
    pub files_written: u32,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct HostingSurface {
    presence: PresenceSnapshotResponse,
    heartbeat: DeviceHeartbeatRequest,
    approvals: ApprovalListResponse,
    approval_decision: ApprovalDecisionRequest,
    operator_overview: OperatorOverviewResponse,
    operator_history: OperatorHistoryResponse,
    agent_definition_scope: AgentDefinitionScopeQuery,
    agent_definition_list: AgentDefinitionListResponse,
    effective_agent_definition_list: EffectiveAgentDefinitionListResponse,
    agent_definition_create: CreateAgentDefinitionRequest,
    agent_definition_update: UpdateAgentDefinitionRequest,
    agent_definition_mutation: AgentDefinitionMutationResponse,
    devices: device::DeviceListResponse,
    knowledge_mirror: KnowledgeMirrorRegenerateResponse,
}
