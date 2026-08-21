use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use swage_protocol::client::workflow::{AgentCallRequest, AgentSessionRef};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkerPlacement {
    pub path: String,
    pub can_write: bool,
}

/// An opaque continuation token naming a worker session a run already
/// created. Scripts receive it on an `agentStart` handle after the call
/// completes and pass it back whole via the `session` option; its fields are
/// host business. The token's values come from journalled results, so a
/// resumed run rebuilds identical tokens and replay stays deterministic.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCallResult {
    pub output: Value,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    #[serde(default)]
    pub turn_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowCallRequest {
    pub name: String,
    pub args: Value,
}

/// An event a script posts to the run's durable pending queue — a detached
/// child's completion, a failure, an escalation. Events are data for a later
/// wake decision, never instructions to anyone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunEventPost {
    /// `completion`, `failure`, or `escalation`.
    pub kind: String,
    pub payload: Value,
}

/// A wake-gate evaluation over the run's pending events: a cheap model reads
/// the raw events against the script's criteria and judges whether waking
/// the lead is worth it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateRequest {
    /// What would make waking the lead worthwhile, in the script's words.
    pub criteria: String,
    /// Gate model override; the host's configured default when `None`.
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectPayload {
    pub content: String,
    pub metadata: Option<Value>,
    pub target: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectReceipt {
    pub delivered: bool,
    pub target: String,
    pub effect_seq: u64,
}

#[async_trait]
pub trait WorkflowHost: Send + Sync {
    fn record(&self, _kind: &str, _message: String) {}

    fn budget_spent(&self) -> u64 {
        0
    }

    fn budget_remaining(&self) -> Option<u64> {
        None
    }

    async fn run_agent_call(
        &self,
        request: AgentCallRequest,
        resolved_placement: Option<ResolvedWorkerPlacement>,
        cancellation: CancellationToken,
    ) -> Result<AgentCallResult, String>;

    /// Resolve a caller placement without changing the caller's request.
    /// Hosts journal managed provisioning separately and return its durable
    /// resolved path; a plain cwd needs no host operation.
    async fn resolve_worker_placement(
        &self,
        request: &AgentCallRequest,
        _cancellation: CancellationToken,
    ) -> Result<Option<ResolvedWorkerPlacement>, String> {
        Ok(request.cwd.clone().map(|path| ResolvedWorkerPlacement {
            path,
            can_write: true,
        }))
    }

    /// Return this run's provisioned worktrees through a journalled read.
    async fn list_worker_placements(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        Err("WorkerPlacementError: this host does not support placement listing".into())
    }

    async fn deliver(
        &self,
        payload: EffectPayload,
        cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String>;

    /// Run a saved workflow by name as a child run, returning its final output.
    /// Hosts without composition reject; the engine only exposes `workflow()`
    /// to top-level runs, so a child script never reaches this.
    async fn run_workflow_call(
        &self,
        _request: WorkflowCallRequest,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        Err("WorkflowCompositionError: this host does not support workflow composition".into())
    }

    /// Journalled timer for `wait()`'s fallback leg. Resolves `{"fired": true}`
    /// after `ms` on a live run, or instantly on replay of a timer-won wait.
    /// `timer_id` is assigned in script order, so confirmation and
    /// abandonment can name the timer deterministically. Firing does NOT
    /// complete the journal entry — `confirm_timer` does, once the race has
    /// declared the timer the winner.
    async fn timer(
        &self,
        _ms: u64,
        _timer_id: u64,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        Err("WaitTimerError: this host does not support timers".into())
    }

    /// Settles a timer the race declared the winner: completes its journal
    /// entry (a no-op on replay, where it is already completed). `wait()`
    /// awaits this before returning a timer wake, so a completed timer
    /// entry means exactly "the timer won a wait".
    async fn confirm_timer(&self, _timer_id: u64) -> Result<(), String> {
        Err("WaitTimerError: this host does not support timers".into())
    }

    /// Stands down a timer whose `wait()` was won by a completion, so its
    /// journal entry stays `started` and the journal names the real wake.
    /// Fire-and-forget from the script's point of view.
    fn abandon_timer(&self, _timer_id: u64) {}

    /// Append an event to the run's durable pending queue (journalled;
    /// exactly-once under replay). Resolves to the stored event
    /// `{seq, kind, payload}`.
    async fn post_run_event(
        &self,
        _event: RunEventPost,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        Err("RunEventError: this host does not support run events".into())
    }

    /// Evaluate the wake gate over the run's pending events (journalled; a
    /// replayed evaluation returns its recorded verdict). Resolves to
    /// `{wake, reasoning, events, read_up_to_seq}` — the events the gate
    /// read ride along so a waking script can ground the lead without a
    /// second read.
    async fn evaluate_wake_gate(
        &self,
        _request: GateRequest,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        Err("RunEventError: this host does not support the wake gate".into())
    }

    /// Stamp pending events up to `up_to_seq` as delivered by the current
    /// wake (journalled, idempotent). Resolves to `{delivered}`.
    async fn deliver_run_events(
        &self,
        _up_to_seq: u64,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        Err("RunEventError: this host does not support run events".into())
    }

    /// A journalled budget reading. Alongside total `spent` and `remaining`,
    /// split budgets expose `working_remaining`, `closing_remaining`, and
    /// `winding_down`, recorded so replay sees what the live run saw.
    /// The synchronous `budget.spent()`/`budget.remaining()` reads are live
    /// diagnostics — a script that BRANCHES on budget state must branch on
    /// this snapshot, or its replayed control flow can diverge from the
    /// journal near the ceiling.
    async fn budget_snapshot(&self, _cancellation: CancellationToken) -> Result<Value, String> {
        Err("BudgetSnapshotError: this host does not support budget snapshots".into())
    }
}
