use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use swage_protocol::client::{settings::Effort, workflow::WorkerIsolation};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowHostError {
    Message(String),
    AgentCall { host_reference: String, message: String },
}

impl std::fmt::Display for WorkflowHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Message(message) => f.write_str(message), Self::AgentCall { message, .. } => f.write_str(message) }
    }
}

impl From<String> for WorkflowHostError { fn from(message: String) -> Self { Self::Message(message) } }

/// An opaque continuation token naming a worker session the run created.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSessionRef {
    pub session_id: Uuid,
    pub thread_id: Uuid,
}

/// A journal-stable workflow child dispatch request. This is host-journal
/// state, not a client wire type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCallRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<WorkerIsolation>,
    pub schema: Option<Value>,
    pub max_attempts: u32,
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elements: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembly: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkerPlacement {
    pub path: String,
    pub can_write: bool,
    pub authority_path: String,
    pub authority_context_id: Uuid,
}

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
    pub kind: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AwaitEventRequest {
    pub kinds: Vec<String>,
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
    ) -> Result<AgentCallResult, WorkflowHostError>;

    /// Resolve placement and admit one agent call through a single host
    /// dispatch. Durable hosts override this to keep the two journal entries
    /// adjacent even when several script promises begin together.
    async fn dispatch_agent_call(
        &self,
        request: AgentCallRequest,
        cancellation: CancellationToken,
    ) -> Result<AgentCallResult, WorkflowHostError> {
        let resolved_placement = self
            .resolve_worker_placement(&request, cancellation.clone())
            .await
            .map_err(|error| {
                if error.starts_with("WorkerPlacementError")
                    || error.starts_with("NondeterministicReplayError")
                {
                    error
                } else {
                    format!("WorkerPlacementError: {error}")
                }
            })?;
        self.run_agent_call(request, resolved_placement, cancellation)
            .await
    }

    /// Resolve a caller placement without changing the caller's request.
    /// Hosts journal managed provisioning separately and return its durable
    /// resolved path; a plain cwd needs no host operation.
    async fn resolve_worker_placement(
        &self,
        request: &AgentCallRequest,
        _cancellation: CancellationToken,
    ) -> Result<Option<ResolvedWorkerPlacement>, String> {
        Ok(request.cwd.clone().map(|path| ResolvedWorkerPlacement {
            authority_path: path.clone(),
            path,
            can_write: true,
            authority_context_id: Uuid::nil(),
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

    /// Return the next matching undelivered event. A live run parks when no
    /// match exists; replay returns the journalled answer.
    async fn await_run_event(
        &self,
        _request: AwaitEventRequest,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        Err("RunEventError: this host does not support awaiting run events".into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_composition_options_are_omitted_from_journal_shape() {
        let request = AgentCallRequest {
            prompt: "work".into(),
            agent: Some("worker".into()),
            task: None,
            model: None,
            effort: None,
            timeout_ms: None,
            cwd: None,
            isolation: None,
            schema: None,
            max_attempts: 3,
            label: None,
            session: None,
            elements: None,
            variables: None,
            assembly: None,
        };

        let value = serde_json::to_value(request).expect("serialise agent request");
        assert!(!value.as_object().unwrap().contains_key("elements"));
        assert!(!value.as_object().unwrap().contains_key("variables"));
        assert!(!value.as_object().unwrap().contains_key("assembly"));
    }
}
