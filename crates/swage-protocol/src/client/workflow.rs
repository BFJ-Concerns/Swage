//! Durable JavaScript workflows and their read-only run traces.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowScript {
    pub id: Uuid,
    pub space_id: Uuid,
    pub name: String,
    pub description: String,
    pub head_version: u32,
    pub has_delivery_grant: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateWorkflowScriptRequest {
    pub name: String,
    pub description: String,
    pub source: String,
    /// Server-side delivery targets granted to this script. Responses expose
    /// only whether a grant exists, never its URLs or secret references.
    #[serde(default)]
    #[ts(type = "unknown")]
    pub delivery_grant: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdateWorkflowScriptRequest {
    pub description: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    #[ts(type = "unknown")]
    pub delivery_grant: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowScriptListResponse {
    pub scripts: Vec<WorkflowScript>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowBudget {
    pub max_agent_calls: Option<u32>,
    pub max_total_tokens: Option<u64>,
    /// Tokens held back from ordinary work for calls made after the run has
    /// observed its wind-down boundary. This allowance is part of, not in
    /// addition to, `max_total_tokens`.
    pub reserved_closing_tokens: Option<u64>,
    pub max_concurrent_agent_calls: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct StartWorkflowRunRequest {
    #[ts(type = "unknown")]
    pub args: Value,
    pub budget: Option<WorkflowBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct StartInlineWorkflowRunRequest {
    pub source: String,
    #[ts(type = "unknown")]
    pub args: Value,
    pub budget: Option<WorkflowBudget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunState {
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    /// Sleeping on a durable wait with its engine torn down; the waker
    /// resumes it by journal replay when it is due.
    Parked,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRun {
    pub id: Uuid,
    pub space_id: Uuid,
    pub script_id: Option<Uuid>,
    pub script_name: Option<String>,
    pub script_version: Option<u32>,
    pub state: WorkflowRunState,
    #[ts(type = "unknown")]
    pub args: Value,
    pub budget: WorkflowBudget,
    #[ts(type = "unknown")]
    pub final_output: Option<Value>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowJournalEntryKind {
    AgentCall,
    Effect,
    Log,
    Phase,
    WorkflowCall,
    Timer,
    /// A pending-queue operation: an event posted, or events stamped
    /// delivered by a wake.
    Event,
    /// A wake-gate evaluation: a cheap model judged the pending events; its
    /// verdict and reasoning are the entry's result.
    Gate,
    /// A journalled budget reading — what the script saw when it checked
    /// its allowance, replayed rather than re-read.
    Budget,
    /// The working allowance was exhausted. The triggering agent call was
    /// not started; replay returns the same wind-down signal at this point.
    WindDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowJournalEntryStatus {
    Started,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowAgentCallDetail {
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowEffectDetail {
    pub effect_kind: String,
    pub target_kind: String,
    pub idempotency_key: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowJournalEntry {
    pub id: Uuid,
    pub seq: u64,
    pub completed_order: Option<u64>,
    pub kind: WorkflowJournalEntryKind,
    pub status: WorkflowJournalEntryStatus,
    pub label: Option<String>,
    #[ts(type = "unknown")]
    pub request: Value,
    #[ts(type = "unknown")]
    pub result: Option<Value>,
    pub error_message: Option<String>,
    pub agent_call: Option<WorkflowAgentCallDetail>,
    pub effect: Option<WorkflowEffectDetail>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunDetailResponse {
    pub run: WorkflowRun,
    pub journal: Vec<WorkflowJournalEntry>,
    /// The run's own wake economics, derived from the journal and event
    /// queue — the falsification signal for a gated long run: a sustained
    /// high wake rate means the design's economics are failing.
    pub wake_metrics: WorkflowWakeMetrics,
}

/// Derived counters over a run's wake machinery. `events_pending` counts
/// undelivered queue entries now; the rest count journalled history.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowWakeMetrics {
    pub gate_evaluations: u32,
    pub gate_wakes: u32,
    pub gate_suppressions: u32,
    pub events_posted: u32,
    pub events_pending: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunListResponse {
    pub runs: Vec<WorkflowRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunStartedEvent {
    pub run: WorkflowRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowEntryEvent {
    pub run_id: Uuid,
    pub entry: WorkflowJournalEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunCompletedEvent {
    pub run: WorkflowRun,
}
