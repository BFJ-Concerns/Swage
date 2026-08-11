use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCallRequest {
    pub prompt: String,
    pub agent: String,
    pub model: Option<String>,
    pub schema: Option<Value>,
    pub max_attempts: u32,
    pub label: Option<String>,
    /// Continue this worker session instead of creating a fresh one: the
    /// next turn lands on the same session and thread, so the worker meets
    /// its own history — a restated brief and the later "proceed" are one
    /// session. `None` is the fresh-session default.
    #[serde(default)]
    pub session: Option<AgentSessionRef>,
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
        cancellation: CancellationToken,
    ) -> Result<AgentCallResult, String>;

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
}
