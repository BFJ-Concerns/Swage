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
    /// The shipped or operator skill that carries this workflow, when it is
    /// not an authored row of the space. A carried workflow is immutable in
    /// place; a same-named authored row shadows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub carrying_skill: Option<String>,
    pub has_delivery_grant: bool,
    /// Whether this script may read the observations captured by this run.
    pub has_captured_sources_grant: bool,
    pub argument_contract: WorkflowArgumentContract,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The argument contract a workflow declares in its portable metadata.
///
/// Workflows may omit a schema. That is an explicit unconstrained contract,
/// rather than an absent field that callers could mistake for an incomplete
/// listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowArgumentContract {
    Declared {
        #[ts(type = "unknown")]
        schema: Value,
    },
    Unconstrained,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateWorkflowScriptRequest {
    pub name: String,
    pub description: String,
    pub source: String,
    /// Server-side delivery targets granted to this script. Responses expose
    /// only whether a grant exists, never its URLs or secret references.
    #[serde(default)]
    #[ts(optional)]
    #[ts(type = "unknown")]
    pub delivery_grant: Option<Value>,
    #[serde(default)]
    #[ts(optional)]
    pub captured_sources_grant: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct UpdateWorkflowScriptRequest {
    pub description: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    #[ts(type = "unknown")]
    pub delivery_grant: Option<Value>,
    #[ts(optional)]
    pub captured_sources_grant: Option<bool>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
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

/// A decision a running script is waiting on: what is being decided and the
/// closed set of answers it will accept. A hold is raised by the script, is
/// answered by an agent through a granted tool, and is the validated shape
/// over the run's event queue rather than a second queue.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunHold {
    pub id: Uuid,
    pub run_id: Uuid,
    #[ts(type = "number")]
    pub seq: u64,
    /// What is being decided, in the raising script's words. Data for the
    /// answering agent to judge, never instructions to it.
    pub content: String,
    /// The answers this hold accepts. At least two distinct verdicts: a hold
    /// with one permitted answer is no decision and is refused at raise.
    pub verdicts: Vec<String>,
    pub state: WorkflowRunHoldState,
    /// The accepted verdict, once one has been given.
    pub verdict: Option<String>,
    /// Who answered. Derived from the answering session, never a parameter.
    pub answered_by: Option<WorkflowRunEventSource>,
    pub raised_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    /// Answers refused for falling outside the accepted set, oldest first.
    /// They hang on this hold rather than taking journal entries of their
    /// own: an external attempt must not move the script's own sequence.
    pub rejections: Vec<WorkflowRunHoldRejection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunHoldState {
    Open,
    Answered,
    Closed,
}

/// One refused answer, kept so the run record shows what was tried.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRunHoldRejection {
    pub id: Uuid,
    pub attempted_verdict: String,
    pub feedback: String,
    pub source: WorkflowRunEventSource,
    pub rejected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct AnswerWorkflowRunHoldRequest {
    pub hold_id: Uuid,
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct AnswerWorkflowRunHoldResponse {
    pub hold: WorkflowRunHold,
    /// Whether answering made a parked run due again. False when the run was
    /// still holding its engine and will take the answer directly.
    pub wake_provisioned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowDecisionEventPayload {
    pub nonce: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
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

/// The durable account a failed run leaves behind: what stopped it, which
/// call failed, and where the pending event queue stood, so whoever picks
/// the work up starts from the record instead of a re-run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "classification", rename_all = "snake_case")]
pub enum WorkflowRunHandover {
    /// An attributed agent-call failure stopped the run.
    Interrupted {
        failure: String,
        failed_call: WorkflowHandoverFailedCall,
        /// Highest journal seq at interruption — where replay would resume.
        #[ts(type = "number")]
        last_journal_seq: u64,
        #[ts(type = "number")]
        events_delivered_to_failed_call: u64,
        #[ts(type = "number")]
        events_pending: u64,
        events: Vec<WorkflowHandoverEvent>,
    },
    /// The run failed without an attributable agent-call identity.
    InterruptedUnattributed {
        failure: String,
        terminal_error: Option<String>,
    },
    /// The journal read-back failed for a known agent call, so only its
    /// identity could be recovered.
    ReconstructionFailed {
        failure: String,
        failed_call: WorkflowHandoverFailedCallRef,
        terminal_error: Option<String>,
    },
}

/// The failed call as the journal recorded it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowHandoverFailedCall {
    pub journal_entry_id: Uuid,
    #[ts(type = "number")]
    pub seq: u64,
    pub kind: WorkflowJournalEntryKind,
    pub label: Option<String>,
    pub error: String,
}

/// The failed call's bare identity, for when the journal could not be read
/// back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowHandoverFailedCallRef {
    pub journal_entry_id: Uuid,
    #[ts(type = "number")]
    pub seq: u64,
}

/// A queued run event's standing relative to the failed call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowHandoverEventDelivery {
    DeliveredToFailedCall,
    Pending,
}

/// One pending-queue event carried in the handover.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowHandoverEvent {
    pub id: Uuid,
    #[ts(type = "number")]
    pub seq: u64,
    pub kind: String,
    pub journal_entry_id: Uuid,
    pub delivery: WorkflowHandoverEventDelivery,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowRun {
    pub id: Uuid,
    pub space_id: Uuid,
    pub script_id: Option<Uuid>,
    pub script_name: Option<String>,
    pub script_version: Option<u32>,
    pub carrying_skill: Option<String>,
    /// Opaque identity declared by the workflow script for grouping its runs.
    pub task: Option<String>,
    /// Source retained for runs whose workflow was not a saved script version.
    pub source: Option<String>,
    /// SHA-256 of the exact source executed by this run.
    pub source_hash: String,
    pub state: WorkflowRunState,
    #[ts(type = "unknown")]
    pub args: Value,
    pub budget: WorkflowBudget,
    #[ts(type = "unknown")]
    pub final_output: Option<Value>,
    pub handover: Option<WorkflowRunHandover>,
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
    /// The authoritative closing-partition balance when the run settled.
    TerminalBudget,
    /// A decision hold: the script named a decision and its accepted
    /// verdicts and waited. The entry completes with the accepted verdict.
    Hold,
    /// A read-only view of source observations captured by this workflow run.
    CapturedSources,
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
    /// The resolved state of the isolated worktree after the worker settled.
    pub worktree: Option<WorkflowWorktreeCompletion>,
    pub served_model: Option<String>,
}

/// The final Git state of an isolated worker worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowWorktreeCompletion {
    pub base_commit: String,
    pub tip_commit: String,
    pub changed: bool,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowEffectDetail {
    pub effect_kind: String,
    pub target_kind: String,
    pub idempotency_key: Uuid,
}

/// A completed workflow journal entry's client-visible result. Durable
/// storage keeps this as JSON, but clients receive only one of these
/// kind-specific shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowJournalResult {
    AgentCall {
        #[ts(type = "unknown")]
        output: Value,
        input_tokens: u64,
        output_tokens: u64,
        working_directory: Option<String>,
        served_model: Option<String>,
        session_id: Option<Uuid>,
        thread_id: Option<Uuid>,
        turn_id: Option<Uuid>,
        /// The resolved state of the isolated worktree after the worker settled.
        worktree: Option<WorkflowWorktreeCompletion>,
    },
    WorkerPlacement {
        path: String,
        can_write: bool,
        authority_path: String,
        authority_context_id: Uuid,
    },
    WorkerPlacementList {
        paths: Vec<String>,
    },
    Effect {
        delivered: bool,
        target: String,
        effect_seq: u64,
    },
    Log,
    Phase,
    WorkflowCall {
        #[ts(type = "unknown")]
        output: Value,
    },
    Timer {
        fired: bool,
    },
    Event {
        seq: u64,
        event_kind: String,
        #[ts(type = "unknown")]
        payload: Value,
        source: WorkflowRunEventSource,
    },
    EventDelivery {
        delivered: u64,
    },
    AwaitEvent {
        seq: u64,
        event_kind: String,
        #[ts(type = "unknown")]
        payload: Value,
        source: WorkflowRunEventSource,
    },
    Gate {
        wake: bool,
        reasoning: String,
        #[ts(type = "unknown")]
        events: Value,
        read_up_to_seq: u64,
    },
    Budget {
        spent: u64,
        remaining: Option<u64>,
        working_remaining: Option<u64>,
        closing_remaining: Option<u64>,
        winding_down: bool,
    },
    TerminalBudget {
        spent: u64,
        remaining: Option<u64>,
        closing_remaining: Option<u64>,
    },
    Hold {
        hold_id: Uuid,
        content: String,
        verdict: Option<String>,
        answered_by: WorkflowRunEventSource,
    },
    CapturedSources {
        #[ts(type = "unknown")]
        sources: Value,
    },
    WindDown {
        spent: u64,
        remaining: u64,
        working_remaining: u64,
        closing_remaining: u64,
        winding_down: bool,
    },
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
    pub result: Option<WorkflowJournalResult>,
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
    /// The run's decision holds, oldest first, each carrying its accepted
    /// verdict and every answer refused along the way.
    pub holds: Vec<WorkflowRunHold>,
    /// Derived counts over those holds: what the run is still waiting on,
    /// what has been decided, and how often a decider offered something the
    /// hold does not accept.
    pub hold_metrics: WorkflowHoldMetrics,
}

/// Derived counters over a run's decision holds. Nothing here is stored:
/// the hold rows and their rejection rows already hold the history, so the
/// counters cannot drift from it.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct WorkflowHoldMetrics {
    pub holds_open: u32,
    pub holds_answered: u32,
    pub answers_rejected: u32,
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
mod journal_result_tests {
    use super::*;

    #[test]
    fn journal_result_serialises_as_a_kind_tagged_client_contract() {
        let result = WorkflowJournalResult::Timer { fired: true };
        assert_eq!(
            serde_json::to_value(result).expect("serialise journal result"),
            serde_json::json!({"kind": "timer", "fired": true})
        );
    }
}
