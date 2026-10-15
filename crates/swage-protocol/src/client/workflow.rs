//! Durable JavaScript workflows and their read-only run traces.

use crate::client::settings::Effort;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowScript {
    pub id: Uuid,
    pub topic_id: Uuid,
    pub name: String,
    pub description: String,
    pub head_version: u32,
    /// The shipped or operator skill that carries this workflow, when it is
    /// not an authored row of the topic. A carried workflow is immutable in
    /// place; a same-named authored row shadows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
#[ts(optional_fields = nullable)]
pub enum WorkflowArgumentContract {
    Declared {
        #[ts(type = "unknown")]
        schema: Value,
    },
    Unconstrained,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct CreateWorkflowScriptRequest {
    pub name: String,
    pub description: String,
    pub source: String,
    /// Server-side delivery targets granted to this script. Responses expose
    /// only whether a grant exists, never its URLs or secret references.
    #[serde(default)]
    #[ts(optional, type = "unknown | null")]
    pub delivery_grant: Option<Value>,
    #[serde(default)]
    pub captured_sources_grant: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct UpdateWorkflowScriptRequest {
    pub description: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    #[ts(optional, type = "unknown | null")]
    pub delivery_grant: Option<Value>,
    pub captured_sources_grant: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowScriptListResponse {
    pub scripts: Vec<WorkflowScript>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRouteBudget {
    /// The token ceiling for calls resolved to this provider entry.
    pub max_total_tokens: Option<u64>,
    /// Tokens retained for closing calls on this provider entry after the
    /// workflow has entered wind-down for that route.
    pub reserved_closing_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRouteBudgetReading {
    pub spent: u64,
    pub remaining: Option<u64>,
    pub closing_remaining: Option<u64>,
    pub usage_unavailable_calls: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowBudget {
    pub max_agent_calls: Option<u32>,
    /// Per-provider-entry token ledgers. A model resolves to the provider
    /// entry named by its catalogue configuration, and calls on one entry
    /// never consume another entry's allowance.
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub per_route: std::collections::BTreeMap<String, WorkflowRouteBudget>,
    /// Fallback ceiling for a resolved provider route without a `per_route`
    /// entry.
    pub max_total_tokens: Option<u64>,
    /// Tokens held back from ordinary work for calls made after the run has
    /// observed its wind-down boundary. This allowance is part of, not in
    /// addition to, `max_total_tokens`.
    pub reserved_closing_tokens: Option<u64>,
    pub max_concurrent_agent_calls: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct StartWorkflowRunRequest {
    #[ts(type = "unknown")]
    pub args: Value,
    pub budget: Option<WorkflowBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct StartInlineWorkflowRunRequest {
    pub source: String,
    #[ts(type = "unknown")]
    pub args: Value,
    pub budget: Option<WorkflowBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct WorkflowRunEventSource {
    pub surface: String,
    pub agent_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PostWorkflowRunEventRequest {
    pub kind: String,
    #[ts(type = "unknown")]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PostWorkflowRunEventResponse {
    pub event: WorkflowRunEvent,
    pub wake_provisioned: bool,
}

/// A decision a running script is waiting on: what is being decided and the
/// closed set of answers it will accept. A hold is raised by the script, is
/// answered by an agent through a granted tool, and is the validated shape
/// over the run's event queue rather than a second queue.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct WorkflowRunHoldRejection {
    pub id: Uuid,
    pub attempted_verdict: String,
    pub feedback: String,
    pub source: WorkflowRunEventSource,
    pub rejected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct AnswerWorkflowRunHoldRequest {
    pub hold_id: Uuid,
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct AnswerWorkflowRunHoldResponse {
    pub hold: WorkflowRunHold,
    /// Whether answering made a parked run due again. False when the run was
    /// still holding its engine and will take the answer directly.
    pub wake_provisioned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub enum WorkflowRunHandover {
    AuthorError {
        failure: String,
    },
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
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct WorkflowHandoverEvent {
    pub id: Uuid,
    #[ts(type = "number")]
    pub seq: u64,
    pub kind: String,
    pub journal_entry_id: Uuid,
    pub delivery: WorkflowHandoverEventDelivery,
}

/// How a run ended. Every ending path names itself here, so no reader has to
/// infer a run's end from silence or from the absence of a result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "ending", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum WorkflowRunEnding {
    /// The script returned its result.
    Returned,
    /// The script threw, or the engine failed the run.
    Threw { error: String },
    /// Invalid workflow input refused before its worker spawned.
    AuthorError { error: String },
    /// An operator or a parent run cancelled it.
    Cancelled,
    /// The working token allowance ran out and the run ended under
    /// wind-down rather than on its own terms.
    WoundDown { error: String },
    /// The gateway died with the run in flight and the run could not be
    /// resumed. The reason is why the resume was refused.
    InterruptedWithoutResume { reason: String },
}

/// One worker's standing in the run's terminal account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowWorkerAccount {
    pub journal_entry_id: Uuid,
    #[ts(type = "number")]
    pub seq: u64,
    pub label: Option<String>,
    pub agent_definition_id: Option<String>,
    /// The directory the worker ran in — the worktree kept rather than torn
    /// down when the run did not return, so its work is still on disk.
    pub placement: Option<String>,
    /// The final Git state recorded when an isolated worker settled.
    pub worktree: Option<WorkflowWorktreeCompletion>,
    /// Absent for a worker that settled on its own account.
    pub termination: Option<WorkflowWorkerTermination>,
}

/// Every worker the run dispatched, sorted by how it stood when the run
/// ended. A worker appears in exactly one list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowWorkerPartition {
    pub done: Vec<WorkflowWorkerAccount>,
    pub failed: Vec<WorkflowWorkerAccount>,
    pub cut_short: Vec<WorkflowWorkerAccount>,
    pub unsettled: Vec<WorkflowWorkerAccount>,
}

/// A completed worker's output recovered from a run that never returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowPartialResult {
    pub journal_entry_id: Uuid,
    #[ts(type = "number")]
    pub seq: u64,
    pub label: Option<String>,
    #[ts(type = "unknown")]
    pub output: Value,
    /// Where the work was done, still on disk.
    pub placement: Option<String>,
}

/// The completed work a run that never returned left behind. The type is the
/// partial marking: these outputs are the run's recoverable work, never its
/// result, and `reason` says what stopped the run from producing one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowPartialResults {
    pub reason: String,
    pub results: Vec<WorkflowPartialResult>,
}

/// The one terminal record of a run: what ended it, where every worker stood,
/// and — when the script never returned — the completed work still recoverable
/// from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRunTerminalAccount {
    pub ending: WorkflowRunEnding,
    pub workers: WorkflowWorkerPartition,
    /// Present when a run ended without returning and has completed worker output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_results: Option<WorkflowPartialResults>,
    pub settled_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<WorkflowRunCancellation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRun {
    pub id: Uuid,
    pub topic_id: Uuid,
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
    /// The run's terminal account: how it ended and where its workers stood.
    /// Absent only while the run is still going.
    pub terminal_account: Option<WorkflowRunTerminalAccount>,
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
    /// A held worker was opened, closed, or read for staleness. The request
    /// names which of the three; the result carries the worker's identity and,
    /// for a staleness read, the recorded idle gap and route threshold.
    Worker,
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
    /// The run's own end stopped this entry's work. Distinct from `failed`,
    /// which is the work's own failure.
    CutShort,
}

/// What a worker call cost, or why that is not known.
///
/// A call whose provider reported no usage is not a call that spent nothing:
/// the two are different facts and the record keeps them apart. Absence is
/// always the `unavailable` variant carrying its reason, never zero counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "availability", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum WorkflowCallUsage {
    Known {
        input_tokens: u64,
        output_tokens: u64,
    },
    Unavailable {
        reason: WorkflowUsageUnavailableReason,
    },
}

impl WorkflowCallUsage {
    /// Counts when the provider reported them; `None` when it did not.
    pub fn known_total(&self) -> Option<u64> {
        match self {
            Self::Known {
                input_tokens,
                output_tokens,
            } => Some(input_tokens.saturating_add(*output_tokens)),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Why a call's usage is not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowUsageUnavailableReason {
    /// The turn completed but the provider reported no token counts.
    ProviderReportedNoUsage,
    /// The call never settled, so no usage was ever collected.
    CallDidNotSettle,
}

/// Why a worker stopped.
///
/// `cut_short` is the run's own end reaching into a worker that was still
/// working. It is not a failure of the worker, and the terminal run account
/// partitions on the same cause — this is where that cause is named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "termination", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum WorkflowWorkerTermination {
    /// The worker is still working; nothing has terminated it.
    Outstanding,
    /// The worker ran to its own end, successfully or not.
    Settled,
    /// The worker's own work failed.
    Failed { message: String },
    /// The run ended while this worker was still working.
    CutShort {
        cause: WorkflowCutShortCause,
        /// The settling error written over the worker, where the terminal
        /// account has one to preserve.
        detail: Option<String>,
    },
    /// Nothing ever recorded an outcome for this worker — the run's end left
    /// no process alive to settle it.
    Unsettled,
}

/// The run-end circumstance that cut a worker short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCutShortCause {
    /// The run was cancelled while the worker was in flight.
    RunCancelled,
    /// The run returned while the worker was still in flight.
    RunReturned,
    /// The run failed while the worker was still in flight.
    RunFailed,
    /// The run reached its closing boundary while the worker was in flight.
    RunWoundDown,
}

/// One provider request round inside a worker call.
///
/// A round with no `completed_at` is still outstanding: the request went to
/// the provider and no response has come back. A settled round names what
/// came back, so a tool loop (rounds settling with tool calls), a hung
/// request (an outstanding round), and a slow response (a settled round with
/// a long span) are told apart from the record alone. A round the
/// interruption caught also ends — at the moment the request became
/// unanswerable rather than at a response — and its outcome says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowProviderRound {
    /// Which attempt of the call ran this round, counting from 1. A call the
    /// run's end interrupted and replay re-dispatched runs its rounds again
    /// under a later attempt; the number is what keeps the two apart.
    pub attempt: u32,
    pub round: u32,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub outcome: WorkflowProviderRoundOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum WorkflowProviderRoundOutcome {
    /// The request is outstanding — no response has arrived.
    Outstanding,
    /// A response arrived, asking for this many tool calls (zero when the
    /// model answered instead of calling tools).
    Responded {
        tool_calls: u32,
    },
    Failed {
        message: String,
    },
    /// The request went out and nothing can answer it any more: the attempt
    /// that made it stopped, because the call was re-dispatched under a later
    /// one or because the run's own end cut the worker short. Distinct from
    /// `outstanding`, which is a request that may still come back. The
    /// round's end time is when that became true, not when a response
    /// arrived — none did.
    Interrupted,
}

/// What a worker was asked to do, recorded when it was dispatched.
///
/// Written before any provider work begins, so a worker that dies mid-run
/// still leaves evidence of the call it was serving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowAgentCallDispatch {
    pub prompt: String,
    pub agent_definition_id: Option<String>,
    /// The model the call named, before the turn resolved it.
    pub requested_model: Option<String>,
    /// The effort the call named, before the turn resolved it.
    pub requested_effort: Option<Effort>,
    /// Where the worker runs, once the placement was provisioned.
    pub placement: Option<WorkflowWorkerPlacementRecord>,
    /// The call-scoped tool grants after intersection with the dispatching
    /// context's own authority. `None` means the call narrowed nothing.
    pub grants: Option<Vec<String>>,
    /// Every attempt the call ran, oldest first, each stamped once its turn
    /// resolved the options it actually ran with. A call interrupted and
    /// re-dispatched by replay has more than one, and each names its own
    /// turn — so the record keeps the interrupted attempt rather than only
    /// the one that settled. Empty until the first turn resolves.
    pub attempts: Vec<WorkflowAgentCallResolution>,
}

/// Where a dispatched worker was placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowWorkerPlacementRecord {
    pub path: String,
    pub can_write: bool,
}

/// The options a dispatched worker's turn resolved for itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowAgentCallResolution {
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub session_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowAgentCallDetail {
    pub agent_definition_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub liveness: Option<WorkflowWorkerLiveness>,
    /// The resolved state of the isolated worktree after the worker settled.
    pub worktree: Option<WorkflowWorktreeCompletion>,
    pub served_model: Option<String>,
    pub provider_route: Option<String>,
    /// What the call spent, or why that is not known. A call the provider
    /// reported no usage for reads as unavailable with a reason, so budget
    /// reconstruction never reads absence as zero spend.
    pub usage: WorkflowCallUsage,
    pub termination: WorkflowWorkerTermination,
    /// What the call was dispatched with. Absent only for calls journalled
    /// before the dispatch record existed.
    pub dispatch: Option<WorkflowAgentCallDispatch>,
    /// The provider rounds this call's turn ran, oldest first. `None` where
    /// the projection does not carry them — the live entry event, which is
    /// sent as the entry settles — so an absent round record is never read
    /// as a call that made no provider request. The run detail always
    /// carries them.
    pub provider_rounds: Option<Vec<WorkflowProviderRound>>,
}

/// Earliest liveness evidence for one workflow worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowWorkerLiveness {
    pub started_at: DateTime<Utc>,
    pub first_evidence_at: Option<DateTime<Utc>>,
}

/// The final Git state of an isolated worker worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowWorktreeCompletion {
    pub base_commit: String,
    pub tip_commit: String,
    pub changed: bool,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub enum WorkflowJournalResult {
    AgentCall {
        #[ts(type = "unknown")]
        output: Value,
        /// What the call spent, or why that is not known.
        usage: WorkflowCallUsage,
        working_directory: Option<String>,
        served_model: Option<String>,
        provider_route: Option<String>,
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
    WorkerOpen {
        worker_id: Uuid,
        #[ts(type = "unknown")]
        creation: Value,
    },
    WorkerClose {
        worker_id: Uuid,
        closed_by: String,
    },
    WorkerStaleness {
        stale: Option<bool>,
        idle_ms: u64,
        threshold_ms: Option<u64>,
        route: Option<String>,
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
        #[serde(default)]
        #[ts(optional, as = "Option<_>")]
        per_route: std::collections::BTreeMap<String, WorkflowRouteBudgetReading>,
        usage_unavailable_calls: u32,
    },
    TerminalBudget {
        spent: u64,
        remaining: Option<u64>,
        closing_remaining: Option<u64>,
        #[serde(default)]
        #[ts(optional, as = "Option<_>")]
        per_route: std::collections::BTreeMap<String, WorkflowRouteBudgetReading>,
        usage_unavailable_calls: u32,
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
        closing_limit: Option<u64>,
        winding_down: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct WorkflowHoldMetrics {
    pub holds_open: u32,
    pub holds_answered: u32,
    pub answers_rejected: u32,
}

/// Derived counters over a run's wake machinery. `events_pending` counts
/// undelivered queue entries now; the rest count journalled history.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowWakeMetrics {
    pub gate_evaluations: u32,
    pub gate_wakes: u32,
    pub gate_suppressions: u32,
    pub events_posted: u32,
    pub events_pending: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRunListResponse {
    pub runs: Vec<WorkflowRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRunStartedEvent {
    pub run: WorkflowRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowEntryEvent {
    pub run_id: Uuid,
    pub entry: WorkflowJournalEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
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

/// A run's observed state when its start request returns.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRunStartResult {
    #[serde(flatten)]
    pub run: WorkflowRun,
    pub status_as_of: DateTime<Utc>,
    pub workers_dispatched: u32,
}

/// The recorded request which cancelled a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRunCancellation {
    pub source: String,
    pub session_id: Option<Uuid>,
    pub actor: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// An ended run awaiting acknowledgement by a lead or schedule gate.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRunEndingNotice {
    pub run_id: Uuid,
    pub script_name: Option<String>,
    pub state: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct WorkflowRunStatusRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 100))]
    pub acknowledge_run_ids: Option<Vec<Uuid>>,
}

impl WorkflowRunStatusRequest {
    pub fn json_schema() -> Value {
        serde_json::to_value(schemars::schema_for!(Self)).expect("status request schema serialises")
    }
}
