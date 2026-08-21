//! Durable JavaScript workflows and their read-only run traces.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

use super::settings::Effort;

/// An opaque continuation token naming a worker session a run already
/// created. Scripts receive it on an `agentStart` handle after the call
/// completes and pass it back whole via the `session` option; its fields are
/// host business. The token's values come from journalled results, so a
/// resumed run rebuilds identical tokens and replay stays deterministic.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSessionRef {
    pub session_id: Uuid,
    pub thread_id: Uuid,
}

/// A journal-stable workflow child dispatch request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCallRequest {
    pub prompt: String,
    /// Optional durable definition giving the worker its character. When
    /// absent, the host creates a definition-less ephemeral worker whose
    /// authority still comes from the dispatching context.
    ///
    /// Both serde attributes are load-bearing: journal argument hashes cover
    /// this serialised request, so historical named calls must not acquire an
    /// `agent: null` field when replayed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub model: Option<String>,
    /// Provider reasoning effort for this call. Absent preserves the ambient
    /// provider or model default and the historical journal request shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Host-enforced deadline for this call in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Caller-selected working directory. This remains unresolved in the
    /// journalled request so its argument hash is stable across hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Optional runtime-managed isolation for the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<WorkerIsolation>,
    pub schema: Option<Value>,
    pub max_attempts: u32,
    pub label: Option<String>,
    /// Continue this worker session instead of creating a fresh one: the
    /// next turn lands on the same session and thread, so the worker meets
    /// its own history — a restated brief and the later "proceed" are one
    /// session. `None` is the fresh-session default.
    ///
    /// `skip_serializing_if` is load-bearing beside `default`: journal args
    /// hashes are computed over the serialised request, so an optional
    /// field that serialises as `null` when unset would change every
    /// existing entry's hash and fail interrupted runs on resume. Unset
    /// must serialise to nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionRef>,
    /// Named briefing elements to compose for this child call. When omitted,
    /// the configured element set remains unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elements: Option<Vec<String>>,
    /// Values visible only while composing this child call. These shadow
    /// durable session values without changing the session-variable store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, String>>,
    /// Named context assembly to use for this child call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembly: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowScript {
    pub id: Uuid,
    pub space_id: Uuid,
    pub name: String,
    pub description: String,
    pub head_version: u32,
    pub built_in: bool,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowScriptListQuery {
    /// Agent whose effective skill layer should contribute carried workflows.
    pub agent_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunEvent {
    #[ts(type = "number")]
    pub seq: u64,
    pub kind: String,
    #[ts(type = "unknown")]
    pub payload: Value,
    pub source: WorkflowRunEventSource,
}

/// Attribution is derived by the gateway, never accepted from a poster.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunEventSource {
    pub surface: String,
    pub agent_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PostWorkflowRunEventRequest {
    pub kind: String,
    #[ts(type = "unknown")]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct PostWorkflowRunEventResponse {
    pub event: WorkflowRunEvent,
    pub wake_provisioned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowDecisionEventPayload {
    pub nonce: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "unknown[]")]
    pub dispositions: Option<Vec<Value>>,
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
    pub carrying_skill: Option<String>,
    pub state: WorkflowRunState,
    #[ts(type = "unknown")]
    pub args: Value,
    pub budget: WorkflowBudget,
    #[ts(type = "unknown")]
    pub final_output: Option<Value>,
    #[ts(type = "unknown")]
    pub handover: Option<Value>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowJournalEntryKind {
    AgentCall,
    /// Runtime provisioning of a worker execution location. Its result records
    /// the resolved path so replay can reuse the same resource.
    WorkerPlacement,
    Effect,
    Log,
    Phase,
    WorkflowCall,
    Timer,
    /// A pending-queue operation: an event posted, or events stamped
    /// delivered by a wake.
    Event,
    AwaitEvent,
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

/// Runtime-managed isolation requested for a dispatched worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerIsolation {
    Worktree,
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
    pub agent_definition_id: Option<String>,
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

#[cfg(test)]
mod task_request_tests {
    use super::*;
    #[test]
    fn absent_task_keeps_the_historical_serialised_shape() {
        let request = AgentCallRequest {
            prompt: "work".into(),
            agent: None,
            task: None,
            model: None,
            effort: None,
            timeout_ms: None,
            cwd: None,
            isolation: None,
            schema: None,
            max_attempts: 1,
            label: None,
            session: None,
            elements: None,
            variables: None,
            assembly: None,
        };
        let value = serde_json::to_value(request).unwrap();
        assert!(!value.as_object().unwrap().contains_key("task"));
    }
    #[test]
    fn named_task_is_serialised_verbatim() {
        let value = serde_json::to_value(AgentCallRequest {
            prompt: "work".into(),
            agent: None,
            task: Some("review".into()),
            model: None,
            effort: None,
            timeout_ms: None,
            cwd: None,
            isolation: None,
            schema: None,
            max_attempts: 1,
            label: None,
            session: None,
            elements: None,
            variables: None,
            assembly: None,
        })
        .unwrap();
        assert_eq!(value["task"], "review");
        let request: AgentCallRequest = serde_json::from_value(value).unwrap();
        assert_eq!(request.task.as_deref(), Some("review"));
    }

    #[test]
    fn unknown_effort_is_rejected_at_the_dispatch_boundary() {
        let error = serde_json::from_value::<AgentCallRequest>(serde_json::json!({
            "prompt": "work",
            "model": null,
            "effort": "extreme",
            "schema": null,
            "max_attempts": 1,
            "label": null
        }))
        .expect_err("workflow effort must use the shared vocabulary");

        assert!(error.to_string().contains("unknown variant `extreme`"));
    }
}
