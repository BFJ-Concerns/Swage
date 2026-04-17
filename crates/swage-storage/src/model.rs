use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use swage_core::{Role, StorageError};
use uuid::Uuid;

/// Turn lifecycle state.
///
/// State transitions are consumer-driven — the storage layer records whatever
/// state the consumer sets without enforcing a state machine. Enforcement is the
/// responsibility of the agent runtime (swage-agent).
///
/// Lifecycle: Pending (created) → Processing (LLM call started) → terminal
/// (Completed / Failed / Interrupted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    /// Turn created; LLM call not yet started.
    Pending,
    /// LLM call in progress.
    Processing,
    /// Turn finished successfully.
    Completed,
    /// Turn ended with an error.
    Failed,
    /// Turn was cut short (e.g. by a stop signal).
    Interrupted,
    /// Turn is suspended, waiting for human review of a tool call or
    /// hook-triggered approval request.
    ///
    /// State transitions:
    ///   Processing → AwaitingApproval (approval gate suspends the turn)
    ///   AwaitingApproval → Processing  (approved or rejected — turn resumes)
    ///   AwaitingApproval → Interrupted (timeout with interrupt policy)
    ///
    /// Stored as "awaiting_approval" in the SQLite text column — no ALTER TABLE
    /// needed to introduce this state.
    AwaitingApproval,

    /// Turn is suspended while a tool-as-delegation is in progress.
    ///
    /// The parent agent's turn pauses while the delegate agent executes. On
    /// completion the turn transitions back to Processing. On gateway restart,
    /// turns in this state are transitioned to Interrupted (the in-flight
    /// delegation was lost).
    ///
    /// State transitions:
    ///   Processing → Delegating     (delegate tool invoked)
    ///   Delegating → Processing     (delegation completed — turn resumes)
    ///   Delegating → Interrupted    (gateway restart recovery)
    Delegating,
}

impl std::fmt::Display for TurnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TurnState::Pending => "pending",
            TurnState::Processing => "processing",
            TurnState::Completed => "completed",
            TurnState::Failed => "failed",
            TurnState::Interrupted => "interrupted",
            TurnState::AwaitingApproval => "awaiting_approval",
            TurnState::Delegating => "delegating",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TurnState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(TurnState::Pending),
            "processing" => Ok(TurnState::Processing),
            "completed" => Ok(TurnState::Completed),
            "failed" => Ok(TurnState::Failed),
            "interrupted" => Ok(TurnState::Interrupted),
            "awaiting_approval" => Ok(TurnState::AwaitingApproval),
            "delegating" => Ok(TurnState::Delegating),
            other => Err(format!("unknown turn state: {other}")),
        }
    }
}

/// A project groups conversations and provides configuration scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    /// True for the single built-in fallback project.
    ///
    /// This is structural identity, not presentation. The project may be
    /// renamed without ceasing to be the default project.
    pub is_default: bool,
    /// Filesystem-safe identifier used for the project's directory under
    /// `~/.swage/projects/{slug}/`.
    pub slug: String,
    pub workspace_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A session (conversation) within a project, scoped to a single agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The agent that owns this session. Used to scope queries in multi-agent
    /// deployments so each agent sees only its own sessions.
    pub agent_id: String,
    /// Host-managed opaque metadata associated with the session.
    ///
    /// This stays outside prompt and WebSocket transport surfaces unless a
    /// caller explicitly reads it through storage or a higher-level API.
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A persisted non-local execution host registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionHostRegistration {
    pub host_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: String,
    pub shared_token: String,
    pub workspace_root: String,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

/// A durable onboarding link for an external communication channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelLink {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub project_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub home_target: Option<serde_json::Value>,
    pub linked_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A durable record of a resolved outbound delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub id: Uuid,
    pub session_id: Uuid,
    pub target: serde_json::Value,
    pub content: String,
    pub delivered_at: DateTime<Utc>,
}

/// A durable audit-trail event for operator inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub session_id: Option<Uuid>,
    pub entity_kind: String,
    pub entity_id: String,
    pub event_kind: String,
    pub summary: String,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// A single agent turn within a thread.
///
/// Turns belong to a thread (not directly to a session). Each turn captures
/// one round-trip with the LLM: the user's input, the model's response, and
/// the token counts for billing and context management.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboundAudit {
    /// Final canonical input after `BeforeInbound` hook processing.
    pub canonical_input: String,
    /// Ordered hook names that returned `ContinueModified`.
    pub modified_by_hooks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    /// The thread that owns this turn.
    pub thread_id: Uuid,
    /// The agent that executed this turn.
    pub agent_id: String,
    pub state: TurnState,
    /// The raw user input that started this turn, if captured.
    pub user_input: Option<String>,
    /// Audit metadata for canonical inbound rewriting.
    ///
    /// Stored only when one or more hooks modified the inbound content.
    pub inbound_audit: Option<InboundAudit>,
    /// The model's final text response, once the turn completes.
    pub assistant_response: Option<String>,
    /// Prompt tokens consumed by the LLM call.
    pub input_tokens: Option<i64>,
    /// Completion tokens generated by the LLM call.
    pub output_tokens: Option<i64>,
    /// Computed cost in microcents, derived from provider pricing data.
    /// `None` when pricing was not configured for the model at call time.
    pub cost_microcents: Option<i64>,
    /// Parsed, validated JSON from the agent's response when output schema
    /// validation succeeded. `None` when no schema is configured, or when
    /// validation failed after retry exhaustion.
    pub structured_output: Option<serde_json::Value>,
    /// References the delegating agent's turn for delegation chain tracking.
    /// `None` for direct (user-initiated) turns; `Some(parent_id)` for turns
    /// created by inter-agent delegation. Multi-hop chains (A → B → C) are
    /// traceable by following the parent references.
    pub parent_turn_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// A message within a thread.
///
/// Messages are scoped to a thread and may optionally reference the turn that
/// produced them. `position` is monotonically increasing within the thread so
/// that the full conversation history can be reconstructed in order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    /// The thread this message belongs to.
    pub thread_id: Uuid,
    /// The turn that produced this message, when known.
    ///
    /// Older rows and synthetic thread bootstrap messages may not have a turn.
    pub turn_id: Option<Uuid>,
    pub role: Role,
    pub content: String,
    /// Monotonically increasing position within the thread.
    pub position: i32,
    /// The agent that produced or received this message.
    pub agent_id: String,
    /// Provider-assigned correlation ID for tool result messages (e.g. `call_abc123`).
    ///
    /// `None` for ordinary user/assistant messages.
    pub tool_call_id: Option<String>,
    /// Name of the tool associated with this message, for tool result messages.
    ///
    /// `None` for ordinary user/assistant messages.
    pub tool_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A tool call within a turn — persisted for conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: Uuid,
    pub turn_id: Uuid,
    /// The agent that issued this tool call, for multi-agent query scoping.
    pub agent_id: String,
    /// Provider-assigned identifier for this individual call (e.g. Anthropic `tool_use` id).
    pub call_id: String,
    /// Name of the tool that was called.
    pub tool: String,
    /// Tool arguments serialised as a JSON string.
    pub arguments: String,
    /// Raw output returned by the tool, if it has completed.
    pub result: Option<String>,
    /// Execution outcome ("success", "error", etc.).
    pub status: Option<String>,
    /// Wall-clock execution time in milliseconds.
    pub duration_ms: Option<i64>,
    /// True when the result was cut off due to length limits.
    pub truncated: bool,
    /// Total byte count of the untruncated result, for reporting.
    pub total_bytes: Option<i64>,
    /// Ordering index within the parent turn.
    pub position: i32,
    pub created_at: DateTime<Utc>,
    /// call_id of the enclosing tool call, if this is a sub-tool invocation.
    ///
    /// None for native (top-level) tool calls. Set when a tool such as
    /// python_execute dispatches further tool calls on behalf of the model.
    pub parent_call_id: Option<String>,
}

/// Input for inserting a new tool call record.
///
/// `id` and `created_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewToolCall {
    pub turn_id: Uuid,
    /// The agent that issued this tool call.
    pub agent_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: String,
    pub result: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<i64>,
    pub truncated: bool,
    pub total_bytes: Option<i64>,
    pub position: i32,
    /// call_id of the enclosing tool call, if this is a sub-tool invocation.
    ///
    /// None for native (top-level) tool calls.
    pub parent_call_id: Option<String>,
}

/// Thread lifecycle state.
///
/// Threads move through a structured state machine — see `can_transition_to`
/// for the permitted edges. Completed is a terminal state with no exits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Idle,
    Processing,
    Completed,
    Interrupted,
}

impl ThreadState {
    /// Whether this state can transition to `target`.
    ///
    /// Permitted edges:
    ///   Idle        → Processing, Completed
    ///   Processing  → Idle, Completed, Interrupted
    ///   Interrupted → Idle
    ///   Completed   → (terminal, no exits)
    pub fn can_transition_to(&self, target: &ThreadState) -> bool {
        matches!(
            (self, target),
            (ThreadState::Idle, ThreadState::Processing)
                | (ThreadState::Idle, ThreadState::Completed)
                | (ThreadState::Processing, ThreadState::Idle)
                | (ThreadState::Processing, ThreadState::Completed)
                | (ThreadState::Processing, ThreadState::Interrupted)
                | (ThreadState::Interrupted, ThreadState::Idle)
        )
    }
}

impl std::fmt::Display for ThreadState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ThreadState::Idle => "idle",
            ThreadState::Processing => "processing",
            ThreadState::Completed => "completed",
            ThreadState::Interrupted => "interrupted",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ThreadState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idle" => Ok(ThreadState::Idle),
            "processing" => Ok(ThreadState::Processing),
            "completed" => Ok(ThreadState::Completed),
            "interrupted" => Ok(ThreadState::Interrupted),
            other => Err(format!("unknown thread state: {other}")),
        }
    }
}

/// A thread within a session, scoped to a single agent.
///
/// A session may contain multiple threads — one per agent invocation. Threads
/// own their turns and messages; the thread's state tracks whether the agent
/// is currently active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: Uuid,
    /// The session this thread belongs to.
    pub session_id: Uuid,
    /// The agent that owns this thread.
    pub agent_id: String,
    pub state: ThreadState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Branch lineage connecting a child thread to its source thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadBranch {
    pub child_thread_id: Uuid,
    pub parent_thread_id: Uuid,
    pub branched_from_message_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// A skill that is currently active for a session.
///
/// Skills are activated by name and persist until explicitly deactivated or
/// until the session is deleted. The `activated_at` timestamp is used to
/// reconstruct the activation order when listing active skills.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSkillRecord {
    pub session_id: Uuid,
    pub skill_name: String,
    pub activated_at: DateTime<Utc>,
}

/// Lifecycle state of a scheduled job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledJobStatus {
    Active,
    Paused,
    Completed,
}

impl std::fmt::Display for ScheduledJobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ScheduledJobStatus::Active => "active",
            ScheduledJobStatus::Paused => "paused",
            ScheduledJobStatus::Completed => "completed",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ScheduledJobStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(ScheduledJobStatus::Active),
            "paused" => Ok(ScheduledJobStatus::Paused),
            "completed" => Ok(ScheduledJobStatus::Completed),
            other => Err(format!("unknown scheduled job status: {other}")),
        }
    }
}

/// Canonical schedule kinds supported by the built-in scheduler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledJobKind {
    OneShot,
    Interval,
    Cron,
}

impl std::fmt::Display for ScheduledJobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ScheduledJobKind::OneShot => "one_shot",
            ScheduledJobKind::Interval => "interval",
            ScheduledJobKind::Cron => "cron",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ScheduledJobKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "one_shot" => Ok(ScheduledJobKind::OneShot),
            "interval" => Ok(ScheduledJobKind::Interval),
            "cron" => Ok(ScheduledJobKind::Cron),
            other => Err(format!("unknown scheduled job kind: {other}")),
        }
    }
}

/// Execution trigger for a scheduled run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledRunTrigger {
    Scheduled,
    Manual,
}

impl std::fmt::Display for ScheduledRunTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ScheduledRunTrigger::Scheduled => "scheduled",
            ScheduledRunTrigger::Manual => "manual",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ScheduledRunTrigger {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scheduled" => Ok(ScheduledRunTrigger::Scheduled),
            "manual" => Ok(ScheduledRunTrigger::Manual),
            other => Err(format!("unknown scheduled run trigger: {other}")),
        }
    }
}

/// Lifecycle state of a scheduled run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledRunState {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for ScheduledRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ScheduledRunState::Pending => "pending",
            ScheduledRunState::Running => "running",
            ScheduledRunState::Completed => "completed",
            ScheduledRunState::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ScheduledRunState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(ScheduledRunState::Pending),
            "running" => Ok(ScheduledRunState::Running),
            "completed" => Ok(ScheduledRunState::Completed),
            "failed" => Ok(ScheduledRunState::Failed),
            other => Err(format!("unknown scheduled run state: {other}")),
        }
    }
}

/// A durable scheduled job definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub message: String,
    pub status: ScheduledJobStatus,
    pub kind: ScheduledJobKind,
    /// Original schedule expression as authored.
    pub schedule_expression: String,
    /// Delivery routing configuration serialised as JSON.
    pub delivery_routing: serde_json::Value,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A recorded execution of a scheduled job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledRun {
    pub id: Uuid,
    pub job_id: Uuid,
    pub trigger: ScheduledRunTrigger,
    pub state: ScheduledRunState,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub turn_id: Option<Uuid>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Lifecycle state of a heartbeat configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatStatus {
    Enabled,
    Disabled,
}

impl std::fmt::Display for HeartbeatStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            HeartbeatStatus::Enabled => "enabled",
            HeartbeatStatus::Disabled => "disabled",
        };
        write!(f, "{s}")
    }
}

impl FromStr for HeartbeatStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "enabled" => Ok(HeartbeatStatus::Enabled),
            "disabled" => Ok(HeartbeatStatus::Disabled),
            other => Err(format!("unknown heartbeat status: {other}")),
        }
    }
}

/// A durable heartbeat bound to an existing session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub message: String,
    pub interval_seconds: i64,
    pub status: HeartbeatStatus,
    pub last_beat_at: Option<DateTime<Utc>>,
    pub next_beat_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle state of a heartbeat execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunState {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for HeartbeatRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            HeartbeatRunState::Pending => "pending",
            HeartbeatRunState::Running => "running",
            HeartbeatRunState::Completed => "completed",
            HeartbeatRunState::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl FromStr for HeartbeatRunState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(HeartbeatRunState::Pending),
            "running" => Ok(HeartbeatRunState::Running),
            "completed" => Ok(HeartbeatRunState::Completed),
            "failed" => Ok(HeartbeatRunState::Failed),
            other => Err(format!("unknown heartbeat run state: {other}")),
        }
    }
}

/// A recorded execution of a heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatRun {
    pub id: Uuid,
    pub heartbeat_id: Uuid,
    pub state: HeartbeatRunState,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub turn_id: Option<Uuid>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Lifecycle state of an approval request.
///
/// Transitions: Pending → Approved | Rejected (human decision) or Expired
/// (timeout fired). Stored as snake_case text in SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    /// Waiting for a human decision.
    Pending,
    /// The human approved the request.
    Approved,
    /// The human rejected the request.
    Rejected,
    /// The timeout elapsed before a decision was made.
    Expired,
}

impl std::fmt::Display for ApprovalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ApprovalState::Pending => "pending",
            ApprovalState::Approved => "approved",
            ApprovalState::Rejected => "rejected",
            ApprovalState::Expired => "expired",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ApprovalState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(ApprovalState::Pending),
            "approved" => Ok(ApprovalState::Approved),
            "rejected" => Ok(ApprovalState::Rejected),
            "expired" => Ok(ApprovalState::Expired),
            other => Err(format!("unknown approval state: {other}")),
        }
    }
}

/// The category of gate that triggered the approval request.
///
/// Determines which of the optional `tool_*` / `hook_name` fields on
/// `ApprovalRequest` are populated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    /// Approval was triggered by a tool dispatch gate.
    Tool,
    /// Approval was triggered by a hook outcome.
    Hook,
}

impl std::fmt::Display for ApprovalKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ApprovalKind::Tool => "tool",
            ApprovalKind::Hook => "hook",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ApprovalKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tool" => Ok(ApprovalKind::Tool),
            "hook" => Ok(ApprovalKind::Hook),
            other => Err(format!("unknown approval kind: {other}")),
        }
    }
}

/// A persisted approval request — one row in the `approval_requests` table.
///
/// Created when an agent turn hits an approval gate (tool dispatch or hook).
/// The turn is suspended in `TurnState::AwaitingApproval` until this record
/// reaches a terminal state (Approved / Rejected / Expired).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: Uuid,
    /// The turn that is suspended awaiting this approval.
    pub turn_id: Uuid,
    /// Agent that triggered the gate.
    pub agent_id: String,
    /// Whether this is a tool-dispatch gate or a hook gate.
    pub kind: ApprovalKind,
    /// Populated when `kind == Tool`.
    pub tool_name: Option<String>,
    /// Tool call arguments, if available, for display in the approval UI.
    pub tool_arguments: Option<serde_json::Value>,
    /// Populated when `kind == Hook`.
    pub hook_name: Option<String>,
    /// Human-readable description of what is being approved.
    pub prompt: String,
    /// Additional structured context for the approval UI (e.g. environment).
    pub context: Option<serde_json::Value>,
    pub state: ApprovalState,
    /// Wall-clock time at which this request expires.
    pub timeout_at: DateTime<Utc>,
    /// What happens when the timeout fires without a decision.
    pub timeout_policy: swage_core::ApprovalTimeoutPolicy,
    /// Free-text reason provided by the approver (or by the timeout handler).
    pub decision_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set when the request reaches a terminal state.
    pub decided_at: Option<DateTime<Utc>>,
}

/// Input for creating a new approval request.
///
/// `id`, `state` (`Pending`), and `decided_at` are assigned by the storage
/// layer — callers supply only the fields they control.
#[derive(Debug, Clone)]
pub struct NewApprovalRequest {
    pub turn_id: Uuid,
    pub agent_id: String,
    pub kind: ApprovalKind,
    pub tool_name: Option<String>,
    pub tool_arguments: Option<serde_json::Value>,
    pub hook_name: Option<String>,
    pub prompt: String,
    pub context: Option<serde_json::Value>,
    pub timeout_at: DateTime<Utc>,
    pub timeout_policy: swage_core::ApprovalTimeoutPolicy,
}

impl NewApprovalRequest {
    pub fn validate(&self) -> Result<(), StorageError> {
        match self.kind {
            ApprovalKind::Tool => {
                if self.tool_name.is_none() {
                    return Err(StorageError::ConstraintViolation(
                        "tool approvals require tool_name".into(),
                    ));
                }
                if self.hook_name.is_some() {
                    return Err(StorageError::ConstraintViolation(
                        "tool approvals must not set hook_name".into(),
                    ));
                }
            }
            ApprovalKind::Hook => {
                if self.hook_name.is_none() && self.tool_name.is_none() {
                    return Err(StorageError::ConstraintViolation(
                        "hook approvals require hook_name or tool_name".into(),
                    ));
                }
                if self.tool_arguments.is_some() && self.tool_name.is_none() {
                    return Err(StorageError::ConstraintViolation(
                        "hook approvals must not set tool_arguments without tool_name".into(),
                    ));
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Agent messages — inter-agent communication
// ---------------------------------------------------------------------------

/// A persisted inter-agent message — one row in the `agent_messages` table.
///
/// Created by the sending agent and held in the storage layer until the
/// recipient agent polls its mailbox. The `status` field tracks delivery
/// lifecycle; `correlation_id` links Result and Cancel messages back to the
/// originating Delegate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: Uuid,
    /// The agent that originated this message.
    pub sender_agent_id: String,
    /// The intended recipient agent.
    pub recipient_agent_id: String,
    /// Semantic category of the message — drives routing and hook events.
    pub kind: swage_core::MessageKind,
    /// References the originating Delegate message. Required on Result and
    /// Cancel messages; None for Delegate and Notify.
    pub correlation_id: Option<Uuid>,
    /// Message-specific content as a JSON object.
    pub payload: serde_json::Value,
    /// Current delivery state (Pending → Delivered | Failed).
    pub status: swage_core::MessageStatus,
    /// Populated when `status == Failed` with a human-readable error description.
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set when `status` transitions to `Delivered`.
    pub delivered_at: Option<DateTime<Utc>>,
}

/// Input for creating a new inter-agent message.
///
/// `id`, `status` (`Pending`), `failure_reason`, `created_at`, and
/// `delivered_at` are assigned by the storage layer — callers supply only
/// the fields they control.
#[derive(Debug, Clone)]
pub struct NewAgentMessage {
    pub sender_agent_id: String,
    pub recipient_agent_id: String,
    pub kind: swage_core::MessageKind,
    /// Required for Result and Cancel; None for Delegate and Notify.
    pub correlation_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

impl NewAgentMessage {
    pub fn validate(&self) -> Result<(), StorageError> {
        match self.kind {
            swage_core::MessageKind::Result | swage_core::MessageKind::Cancel => {
                if self.correlation_id.is_none() {
                    return Err(StorageError::ConstraintViolation(format!(
                        "{} messages require correlation_id",
                        self.kind
                    )));
                }
            }
            swage_core::MessageKind::Delegate
            | swage_core::MessageKind::Notify
            | swage_core::MessageKind::Shutdown => {
                if self.correlation_id.is_some() {
                    return Err(StorageError::ConstraintViolation(format!(
                        "{} messages must not set correlation_id",
                        self.kind
                    )));
                }
            }
        }

        Ok(())
    }
}

/// A team — the organisational unit for multi-agent orchestration.
///
/// Teams belong to a project and define a member list plus an entry agent
/// (the agent that receives user messages). Topology definitions reference
/// teams to bind agents to tasks in a directed graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: Uuid,
    pub project_id: Uuid,
    /// Unique name within the project.
    pub name: String,
    pub description: String,
    /// The agent that receives user messages for this team.
    pub entry_agent_id: String,
    /// Member agent IDs as a JSON-serialised array.
    pub members: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Creation types — inputs for insert operations (no auto-generated fields)
// ---------------------------------------------------------------------------

/// Input for creating a new team.
#[derive(Debug, Clone)]
pub struct NewTeam {
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    pub entry_agent_id: String,
    pub members: Vec<String>,
}

/// Topology run lifecycle state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyRunState {
    Pending,
    Running,
    Completed,
    Failed,
    Interrupted,
}

impl std::fmt::Display for TopologyRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TopologyRunState::Pending => "pending",
            TopologyRunState::Running => "running",
            TopologyRunState::Completed => "completed",
            TopologyRunState::Failed => "failed",
            TopologyRunState::Interrupted => "interrupted",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TopologyRunState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(TopologyRunState::Pending),
            "running" => Ok(TopologyRunState::Running),
            "completed" => Ok(TopologyRunState::Completed),
            "failed" => Ok(TopologyRunState::Failed),
            "interrupted" => Ok(TopologyRunState::Interrupted),
            other => Err(format!("unknown topology run state: {other}")),
        }
    }
}

/// A persisted topology run for a team conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyRun {
    pub id: Uuid,
    pub team_id: Uuid,
    pub session_id: Uuid,
    /// The user- or delegate-triggered turn that started this topology run.
    pub source_turn_id: Option<Uuid>,
    pub state: TopologyRunState,
    /// Final structured output for the topology, if one was produced.
    pub final_output: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Input for creating a topology run.
#[derive(Debug, Clone)]
pub struct NewTopologyRun {
    pub team_id: Uuid,
    pub session_id: Uuid,
    pub source_turn_id: Option<Uuid>,
    pub state: TopologyRunState,
    pub final_output: Option<serde_json::Value>,
    pub error_message: Option<String>,
}

/// Node execution lifecycle state within a topology run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeExecutionState {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl std::fmt::Display for NodeExecutionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            NodeExecutionState::Pending => "pending",
            NodeExecutionState::Running => "running",
            NodeExecutionState::Completed => "completed",
            NodeExecutionState::Failed => "failed",
            NodeExecutionState::Skipped => "skipped",
        };
        write!(f, "{s}")
    }
}

impl FromStr for NodeExecutionState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(NodeExecutionState::Pending),
            "running" => Ok(NodeExecutionState::Running),
            "completed" => Ok(NodeExecutionState::Completed),
            "failed" => Ok(NodeExecutionState::Failed),
            "skipped" => Ok(NodeExecutionState::Skipped),
            other => Err(format!("unknown node execution state: {other}")),
        }
    }
}

/// Stable persisted node kind for topology execution traces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyNodeRecordKind {
    Llm,
    Deterministic,
    Continuation,
}

impl std::fmt::Display for TopologyNodeRecordKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TopologyNodeRecordKind::Llm => "llm",
            TopologyNodeRecordKind::Deterministic => "deterministic",
            TopologyNodeRecordKind::Continuation => "continuation",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TopologyNodeRecordKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "llm" => Ok(TopologyNodeRecordKind::Llm),
            "deterministic" => Ok(TopologyNodeRecordKind::Deterministic),
            "continuation" => Ok(TopologyNodeRecordKind::Continuation),
            other => Err(format!("unknown topology node kind: {other}")),
        }
    }
}

/// A single node execution within a topology run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeExecution {
    pub id: Uuid,
    pub topology_run_id: Uuid,
    /// Stable identifier from the topology definition.
    pub node_id: String,
    /// Semantic node kind such as `llm`, `deterministic`, or `continuation`.
    pub node_kind: TopologyNodeRecordKind,
    pub agent_id: Option<String>,
    pub task_name: Option<String>,
    pub state: NodeExecutionState,
    pub input_json: Option<serde_json::Value>,
    pub output_json: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Input for creating a node execution record.
#[derive(Debug, Clone)]
pub struct NewNodeExecution {
    pub topology_run_id: Uuid,
    pub node_id: String,
    pub node_kind: TopologyNodeRecordKind,
    pub agent_id: Option<String>,
    pub task_name: Option<String>,
    pub state: NodeExecutionState,
    pub input_json: Option<serde_json::Value>,
    pub output_json: Option<serde_json::Value>,
    pub error_message: Option<String>,
}

/// Edge-condition evaluation emitted during topology traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeEvaluation {
    pub id: Uuid,
    pub topology_run_id: Uuid,
    pub source_node_execution_id: Uuid,
    pub edge_id: String,
    pub target_node_id: String,
    pub condition_expression: Option<String>,
    pub matched: bool,
    pub payload_json: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// Input for creating an edge evaluation record.
#[derive(Debug, Clone)]
pub struct NewEdgeEvaluation {
    pub topology_run_id: Uuid,
    pub source_node_execution_id: Uuid,
    pub edge_id: String,
    pub target_node_id: String,
    pub condition_expression: Option<String>,
    pub matched: bool,
    pub payload_json: Option<serde_json::Value>,
}

/// Retry history for a node execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRetry {
    pub id: Uuid,
    pub node_execution_id: Uuid,
    pub attempt_number: i32,
    pub failure_kind: String,
    pub error_message: String,
    pub recovery_guidance: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Input for recording a node retry attempt.
#[derive(Debug, Clone)]
pub struct NewNodeRetry {
    pub node_execution_id: Uuid,
    pub attempt_number: i32,
    pub failure_kind: String,
    pub error_message: String,
    pub recovery_guidance: Option<String>,
}

/// Input for creating a new project.
#[derive(Debug, Clone)]
pub struct NewProject {
    pub name: String,
    /// Filesystem-safe identifier for the project directory.
    pub slug: String,
    pub workspace_path: Option<String>,
}

/// Input for creating a new session.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub project_id: Uuid,
    /// The agent that owns this session.
    pub agent_id: String,
    /// Host-managed opaque metadata to persist with the session.
    pub metadata: Option<serde_json::Value>,
}

/// Input for persisting or replacing an execution host registration.
#[derive(Debug, Clone)]
pub struct NewExecutionHostRegistration {
    pub host_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: String,
    pub shared_token: String,
    pub workspace_root: String,
    pub capabilities: Vec<String>,
}

/// Input for creating or replacing a durable channel onboarding link.
#[derive(Debug, Clone)]
pub struct NewChannelLink {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub project_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub home_target: Option<serde_json::Value>,
}

/// Input for recording a durable outbound delivery.
#[derive(Debug, Clone)]
pub struct NewDeliveryRecord {
    pub session_id: Uuid,
    pub target: serde_json::Value,
    pub content: String,
}

/// Input for creating a new thread.
///
/// `id`, `state`, `created_at`, and `updated_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewThread {
    pub session_id: Uuid,
    pub agent_id: String,
}

/// Input for creating a new turn within a thread.
///
/// `id`, `created_at`, and `completed_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewTurn {
    pub thread_id: Uuid,
    pub agent_id: String,
    pub state: TurnState,
    /// Raw user input captured at turn creation time.
    pub user_input: Option<String>,
    /// Optional parent turn for delegation chain tracking. `None` for
    /// user-initiated turns.
    pub parent_turn_id: Option<Uuid>,
}

/// Input for creating a durable scheduled job.
#[derive(Debug, Clone)]
pub struct NewScheduledJob {
    pub session_id: Uuid,
    pub agent_id: String,
    pub message: String,
    pub status: ScheduledJobStatus,
    pub kind: ScheduledJobKind,
    pub schedule_expression: String,
    pub delivery_routing: serde_json::Value,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}

/// Partial update for an existing scheduled job.
#[derive(Debug, Clone, Default)]
pub struct UpdateScheduledJob {
    pub message: Option<String>,
    pub status: Option<ScheduledJobStatus>,
    pub kind: Option<ScheduledJobKind>,
    pub schedule_expression: Option<String>,
    pub delivery_routing: Option<serde_json::Value>,
    pub next_run_at: Option<Option<DateTime<Utc>>>,
    pub last_run_at: Option<Option<DateTime<Utc>>>,
}

/// Input for recording a scheduled run.
#[derive(Debug, Clone)]
pub struct NewScheduledRun {
    pub job_id: Uuid,
    pub trigger: ScheduledRunTrigger,
    pub state: ScheduledRunState,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub turn_id: Option<Uuid>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Input for creating a durable heartbeat.
#[derive(Debug, Clone)]
pub struct NewHeartbeat {
    pub session_id: Uuid,
    pub agent_id: String,
    pub message: String,
    pub interval_seconds: i64,
    pub status: HeartbeatStatus,
    pub last_beat_at: Option<DateTime<Utc>>,
    pub next_beat_at: Option<DateTime<Utc>>,
}

/// Partial update for an existing heartbeat.
#[derive(Debug, Clone, Default)]
pub struct UpdateHeartbeat {
    pub message: Option<String>,
    pub interval_seconds: Option<i64>,
    pub status: Option<HeartbeatStatus>,
    pub last_beat_at: Option<Option<DateTime<Utc>>>,
    pub next_beat_at: Option<Option<DateTime<Utc>>>,
}

/// Input for recording a heartbeat run.
#[derive(Debug, Clone)]
pub struct NewHeartbeatRun {
    pub heartbeat_id: Uuid,
    pub state: HeartbeatRunState,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub turn_id: Option<Uuid>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Input for creating a new message within a thread.
///
/// `id` and `created_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub thread_id: Uuid,
    /// The turn that produced this message, when known.
    pub turn_id: Option<Uuid>,
    pub role: Role,
    pub content: String,
    /// Monotonically increasing position within the thread.
    pub position: i32,
    /// The agent that produced or received this message.
    pub agent_id: String,
}

/// Input for appending a new message at the end of a thread.
///
/// The storage layer assigns the next monotonic position atomically.
#[derive(Debug, Clone)]
pub struct AppendMessage {
    pub thread_id: Uuid,
    /// The turn that produced this message, when known.
    pub turn_id: Option<Uuid>,
    pub role: Role,
    pub content: String,
    /// The agent that produced or received this message.
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- ToolCallRecord serialisation ---

    #[test]
    fn tool_call_record_serialises_full() {
        let id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let now = Utc::now();
        let record = ToolCallRecord {
            id,
            turn_id,
            agent_id: "agent-1".to_string(),
            call_id: "toolu_01".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
            result: Some("file1\nfile2".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(42),
            truncated: false,
            total_bytes: Some(20),
            position: 0,
            created_at: now,
            parent_call_id: None,
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.turn_id, turn_id);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.call_id, "toolu_01");
        assert_eq!(back.tool, "shell");
        assert_eq!(back.arguments, r#"{"command":"ls"}"#);
        assert_eq!(back.result, Some("file1\nfile2".to_string()));
        assert_eq!(back.status, Some("success".to_string()));
        assert_eq!(back.duration_ms, Some(42));
        assert!(!back.truncated);
        assert_eq!(back.total_bytes, Some(20));
        assert_eq!(back.position, 0);
    }

    #[test]
    fn tool_call_record_serialises_with_none_fields() {
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "agent-2".to_string(),
            call_id: "toolu_02".to_string(),
            tool: "file_read".to_string(),
            arguments: r#"{"path":"/tmp/x"}"#.to_string(),
            result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            position: 1,
            created_at: Utc::now(),
            parent_call_id: None,
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert!(back.result.is_none());
        assert!(back.status.is_none());
        assert!(back.duration_ms.is_none());
        assert!(back.total_bytes.is_none());
        assert_eq!(back.position, 1);
    }

    #[test]
    fn new_tool_call_is_debug_clone() {
        let original = NewToolCall {
            turn_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            call_id: "toolu_03".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"pwd"}"#.to_string(),
            result: Some("ok".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(10),
            truncated: false,
            total_bytes: Some(2),
            position: 0,
            parent_call_id: None,
        };
        let cloned = original.clone();
        // Debug: just ensure it formats without panic.
        let _ = format!("{cloned:?}");
        assert_eq!(original.tool, cloned.tool);
    }

    #[test]
    fn tool_call_record_carries_parent_call_id() {
        let parent_id = "toolu_parent".to_string();
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            call_id: "toolu_sub".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            position: 0,
            created_at: Utc::now(),
            parent_call_id: Some(parent_id.clone()),
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.parent_call_id, Some(parent_id));
    }

    #[test]
    fn new_tool_call_carries_parent_call_id() {
        let sub = NewToolCall {
            turn_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            call_id: "sub_01".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{}"#.to_string(),
            result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            position: 0,
            parent_call_id: Some("parent_01".to_string()),
        };
        assert_eq!(sub.parent_call_id.as_deref(), Some("parent_01"));

        // Top-level calls use None.
        let top = NewToolCall {
            parent_call_id: None,
            ..sub.clone()
        };
        assert!(top.parent_call_id.is_none());
    }

    // --- Project slug field ---

    #[test]
    fn project_slug_field_present_and_serialises() {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let project = Project {
            id,
            name: "my-project".to_string(),
            is_default: false,
            slug: "my-project".to_string(),
            workspace_path: None,
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string(&project).expect("serialisation should succeed");
        assert!(
            json.contains("\"slug\":\"my-project\""),
            "slug should appear in JSON"
        );
    }

    #[test]
    fn new_project_slug_field_present() {
        let np = NewProject {
            name: "test".to_string(),
            slug: "test".to_string(),
            workspace_path: None,
        };
        assert_eq!(np.slug, "test");
    }

    // --- Topology trace models ---

    #[test]
    fn topology_run_state_display_from_str_round_trip() {
        let states = [
            TopologyRunState::Pending,
            TopologyRunState::Running,
            TopologyRunState::Completed,
            TopologyRunState::Failed,
            TopologyRunState::Interrupted,
        ];

        for state in states {
            let text = state.to_string();
            let parsed = TopologyRunState::from_str(&text).expect("state should parse");
            assert_eq!(parsed, state);
        }
    }

    #[test]
    fn node_execution_state_display_from_str_round_trip() {
        let states = [
            NodeExecutionState::Pending,
            NodeExecutionState::Running,
            NodeExecutionState::Completed,
            NodeExecutionState::Failed,
            NodeExecutionState::Skipped,
        ];

        for state in states {
            let text = state.to_string();
            let parsed = NodeExecutionState::from_str(&text).expect("state should parse");
            assert_eq!(parsed, state);
        }
    }

    #[test]
    fn topology_run_serialises_final_output() {
        let run = TopologyRun {
            id: Uuid::new_v4(),
            team_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            source_turn_id: Some(Uuid::new_v4()),
            state: TopologyRunState::Completed,
            final_output: Some(serde_json::json!({"result": "done"})),
            error_message: None,
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
        };

        let json = serde_json::to_string(&run).expect("serialisation should succeed");
        let back: TopologyRun =
            serde_json::from_str(&json).expect("deserialisation should succeed");
        assert_eq!(back.state, TopologyRunState::Completed);
        assert_eq!(
            back.final_output,
            Some(serde_json::json!({"result": "done"}))
        );
    }

    #[test]
    fn node_execution_edge_evaluation_and_retry_serialise() {
        let execution = NodeExecution {
            id: Uuid::new_v4(),
            topology_run_id: Uuid::new_v4(),
            node_id: "plan".into(),
            node_kind: TopologyNodeRecordKind::Llm,
            agent_id: Some("planner".into()),
            task_name: Some("draft-plan".into()),
            state: NodeExecutionState::Completed,
            input_json: Some(serde_json::json!({"prompt": "hello"})),
            output_json: Some(serde_json::json!({"decision": "continue"})),
            error_message: None,
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
        };
        let edge = EdgeEvaluation {
            id: Uuid::new_v4(),
            topology_run_id: execution.topology_run_id,
            source_node_execution_id: execution.id,
            edge_id: "plan->review".into(),
            target_node_id: "review".into(),
            condition_expression: Some("decision == 'continue'".into()),
            matched: true,
            payload_json: Some(serde_json::json!({"branch": "continue"})),
            created_at: Utc::now(),
        };
        let retry = NodeRetry {
            id: Uuid::new_v4(),
            node_execution_id: execution.id,
            attempt_number: 2,
            failure_kind: "schema_validation".into(),
            error_message: "missing field".into(),
            recovery_guidance: Some("return valid JSON".into()),
            created_at: Utc::now(),
        };

        let execution_json = serde_json::to_string(&execution).expect("execution serialises");
        let edge_json = serde_json::to_string(&edge).expect("edge serialises");
        let retry_json = serde_json::to_string(&retry).expect("retry serialises");

        let execution_back: NodeExecution =
            serde_json::from_str(&execution_json).expect("execution deserialises");
        let edge_back: EdgeEvaluation =
            serde_json::from_str(&edge_json).expect("edge deserialises");
        let retry_back: NodeRetry = serde_json::from_str(&retry_json).expect("retry deserialises");

        assert_eq!(execution_back.node_id, "plan");
        assert!(edge_back.matched);
        assert_eq!(retry_back.attempt_number, 2);
    }

    // --- ApprovalState round-trips ---

    #[test]
    fn approval_state_pending_displays_as_pending() {
        assert_eq!(ApprovalState::Pending.to_string(), "pending");
    }

    #[test]
    fn approval_state_display_from_str_round_trip() {
        let states = [
            ApprovalState::Pending,
            ApprovalState::Approved,
            ApprovalState::Rejected,
            ApprovalState::Expired,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: ApprovalState = s.parse().expect("round-trip should succeed");
            assert_eq!(state, &parsed, "round-trip failed for {s}");
        }
    }

    #[test]
    fn approval_state_from_str_unknown_errors() {
        assert!("bogus".parse::<ApprovalState>().is_err());
    }

    // --- ApprovalKind round-trips ---

    #[test]
    fn approval_kind_display_from_str_round_trip() {
        let kinds = [ApprovalKind::Tool, ApprovalKind::Hook];
        for kind in &kinds {
            let s = kind.to_string();
            let parsed: ApprovalKind = s.parse().expect("round-trip should succeed");
            assert_eq!(kind, &parsed, "round-trip failed for {s}");
        }
    }

    #[test]
    fn approval_kind_from_str_unknown_errors() {
        assert!("bogus".parse::<ApprovalKind>().is_err());
    }

    // --- ApprovalRequest struct constructibility ---

    #[test]
    fn approval_request_is_constructible() {
        use swage_core::ApprovalTimeoutPolicy;

        let id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let now = Utc::now();
        let req = ApprovalRequest {
            id,
            turn_id,
            agent_id: "agent-1".to_string(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".to_string()),
            tool_arguments: Some(serde_json::json!({"cmd": "ls"})),
            hook_name: None,
            prompt: "Approve shell?".to_string(),
            context: None,
            state: ApprovalState::Pending,
            timeout_at: now,
            timeout_policy: ApprovalTimeoutPolicy::Reject,
            decision_reason: None,
            created_at: now,
            decided_at: None,
        };
        assert_eq!(req.id, id);
        assert_eq!(req.kind, ApprovalKind::Tool);
        assert_eq!(req.state, ApprovalState::Pending);
    }

    #[test]
    fn new_approval_request_is_constructible() {
        use swage_core::ApprovalTimeoutPolicy;

        let turn_id = Uuid::new_v4();
        let now = Utc::now();
        let req = NewApprovalRequest {
            turn_id,
            agent_id: "agent-2".to_string(),
            kind: ApprovalKind::Hook,
            tool_name: None,
            tool_arguments: None,
            hook_name: Some("pre_exec".to_string()),
            prompt: "Approve hook?".to_string(),
            context: Some(serde_json::json!({"step": "init"})),
            timeout_at: now,
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        };
        assert_eq!(req.turn_id, turn_id);
        assert_eq!(req.kind, ApprovalKind::Hook);
    }

    #[test]
    fn turn_state_pending_displays_as_pending() {
        assert_eq!(TurnState::Pending.to_string(), "pending");
    }

    #[test]
    fn turn_state_processing_displays_as_processing() {
        assert_eq!(TurnState::Processing.to_string(), "processing");
    }

    #[test]
    fn turn_state_display_from_str_round_trip() {
        // All variants must survive a Display→FromStr round-trip.
        let states = [
            TurnState::Pending,
            TurnState::Processing,
            TurnState::Completed,
            TurnState::Failed,
            TurnState::Interrupted,
            TurnState::AwaitingApproval,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: TurnState = s.parse().expect("round-trip should succeed");
            assert_eq!(state, &parsed);
        }
    }

    #[test]
    fn turn_state_awaiting_approval_displays_as_awaiting_approval() {
        assert_eq!(TurnState::AwaitingApproval.to_string(), "awaiting_approval");
    }

    #[test]
    fn turn_state_awaiting_approval_parses_from_str() {
        let parsed: TurnState = "awaiting_approval"
            .parse()
            .expect("awaiting_approval should parse");
        assert_eq!(parsed, TurnState::AwaitingApproval);
    }

    #[test]
    fn turn_state_from_str_unknown() {
        let result = "bogus".parse::<TurnState>();
        assert!(result.is_err());
    }

    // --- ThreadState transitions ---

    #[test]
    fn thread_state_valid_transitions() {
        // Idle can go to Processing or Completed.
        assert!(ThreadState::Idle.can_transition_to(&ThreadState::Processing));
        assert!(ThreadState::Idle.can_transition_to(&ThreadState::Completed));
        // Processing can go to Idle, Completed, or Interrupted.
        assert!(ThreadState::Processing.can_transition_to(&ThreadState::Idle));
        assert!(ThreadState::Processing.can_transition_to(&ThreadState::Completed));
        assert!(ThreadState::Processing.can_transition_to(&ThreadState::Interrupted));
        // Interrupted can only go back to Idle.
        assert!(ThreadState::Interrupted.can_transition_to(&ThreadState::Idle));
    }

    #[test]
    fn thread_state_invalid_transitions() {
        // Idle cannot stay Idle or go to Interrupted.
        assert!(!ThreadState::Idle.can_transition_to(&ThreadState::Idle));
        assert!(!ThreadState::Idle.can_transition_to(&ThreadState::Interrupted));
        // Processing cannot loop to itself.
        assert!(!ThreadState::Processing.can_transition_to(&ThreadState::Processing));
        // Completed is terminal — no exits.
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Idle));
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Processing));
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Interrupted));
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Completed));
        // Interrupted cannot go to Processing or Completed directly.
        assert!(!ThreadState::Interrupted.can_transition_to(&ThreadState::Processing));
        assert!(!ThreadState::Interrupted.can_transition_to(&ThreadState::Completed));
        assert!(!ThreadState::Interrupted.can_transition_to(&ThreadState::Interrupted));
    }

    #[test]
    fn thread_state_display_from_str_round_trip() {
        let states = [
            ThreadState::Idle,
            ThreadState::Processing,
            ThreadState::Completed,
            ThreadState::Interrupted,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: ThreadState = s.parse().expect("round-trip should succeed");
            assert_eq!(state, &parsed);
        }
    }

    #[test]
    fn thread_state_from_str_unknown() {
        let result = "bogus".parse::<ThreadState>();
        assert!(result.is_err());
    }

    // --- Thread serialisation ---

    #[test]
    fn thread_serialises_and_deserialises() {
        let id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let now = Utc::now();
        let thread = Thread {
            id,
            session_id,
            agent_id: "agent-1".to_string(),
            state: ThreadState::Idle,
            created_at: now,
            updated_at: now,
        };

        let json = serde_json::to_string(&thread).expect("serialisation should succeed");
        let back: Thread = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.session_id, session_id);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.state, ThreadState::Idle);
    }

    // --- Message: thread ownership + agent_id + tool fields ---

    #[test]
    fn message_serialises_full() {
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();
        let msg = Message {
            id,
            thread_id,
            turn_id: Some(Uuid::new_v4()),
            role: Role::User,
            content: "hello".to_string(),
            position: 0,
            agent_id: "agent-1".to_string(),
            tool_call_id: Some("call_abc123".to_string()),
            tool_name: Some("shell".to_string()),
            created_at: now,
        };

        let json = serde_json::to_string(&msg).expect("serialisation should succeed");
        let back: Message = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.thread_id, thread_id);
        assert_eq!(back.role, Role::User);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.tool_call_id, Some("call_abc123".to_string()));
        assert_eq!(back.tool_name, Some("shell".to_string()));
        assert_eq!(back.position, 0);
    }

    #[test]
    fn message_tool_fields_optional() {
        // tool_call_id and tool_name may both be None for non-tool messages.
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();
        let msg = Message {
            id,
            thread_id,
            turn_id: None,
            role: Role::Assistant,
            content: "sure".to_string(),
            position: 1,
            agent_id: "agent-1".to_string(),
            tool_call_id: None,
            tool_name: None,
            created_at: now,
        };

        let json = serde_json::to_string(&msg).expect("serialisation should succeed");
        let back: Message = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert!(back.tool_call_id.is_none());
        assert!(back.tool_name.is_none());
    }

    #[test]
    fn new_message_carries_thread_id_and_agent_id() {
        let thread_id = Uuid::new_v4();
        let nm = NewMessage {
            thread_id,
            turn_id: None,
            role: Role::User,
            content: "test".to_string(),
            position: 0,
            agent_id: "agent-x".to_string(),
        };
        assert_eq!(nm.thread_id, thread_id);
        assert_eq!(nm.agent_id, "agent-x");
    }

    #[test]
    fn new_message_is_debug_clone() {
        let original = NewMessage {
            thread_id: Uuid::new_v4(),
            turn_id: Some(Uuid::new_v4()),
            role: Role::User,
            content: "hi".to_string(),
            position: 0,
            agent_id: "agent-y".to_string(),
        };
        let cloned = original.clone();
        let _ = format!("{cloned:?}");
        assert_eq!(original.thread_id, cloned.thread_id);
        assert_eq!(original.agent_id, cloned.agent_id);
    }

    #[test]
    fn new_thread_is_debug_clone() {
        let session_id = Uuid::new_v4();
        let original = NewThread {
            session_id,
            agent_id: "agent-x".to_string(),
        };
        let cloned = original.clone();
        let _ = format!("{cloned:?}");
        assert_eq!(original.session_id, cloned.session_id);
        assert_eq!(original.agent_id, cloned.agent_id);
    }

    // --- Session agent_id ---

    #[test]
    fn session_includes_agent_id() {
        let id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let now = Utc::now();
        let session = Session {
            id,
            project_id,
            agent_id: "my-agent".to_string(),
            metadata: None,
            created_at: now,
            updated_at: now,
        };

        let json = serde_json::to_string(&session).expect("serialisation should succeed");
        let back: Session = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.agent_id, "my-agent");
    }

    #[test]
    fn new_session_includes_agent_id() {
        let ns = NewSession {
            project_id: Uuid::new_v4(),
            agent_id: "builder-agent".to_string(),
            metadata: None,
        };
        assert_eq!(ns.agent_id, "builder-agent");
    }

    // --- Turn serialisation ---

    #[test]
    fn turn_serialises_with_all_fields() {
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();
        let turn = Turn {
            id,
            thread_id,
            agent_id: "agent-1".to_string(),
            state: TurnState::Processing,
            parent_turn_id: None,
            user_input: Some("hello".to_string()),
            inbound_audit: None,
            assistant_response: Some("hi".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
            cost_microcents: None,
            structured_output: None,
            created_at: now,
            completed_at: None,
        };

        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.thread_id, thread_id);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.state, TurnState::Processing);
        assert_eq!(back.user_input, Some("hello".to_string()));
        assert_eq!(back.assistant_response, Some("hi".to_string()));
        assert_eq!(back.input_tokens, Some(10));
        assert_eq!(back.output_tokens, Some(20));
        assert!(back.completed_at.is_none());
    }

    #[test]
    fn new_turn_carries_thread_and_agent() {
        let thread_id = Uuid::new_v4();
        let turn = NewTurn {
            thread_id,
            agent_id: "agent-2".to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        };
        assert_eq!(turn.thread_id, thread_id);
        assert_eq!(turn.agent_id, "agent-2");
    }

    // --- Turn.cost_microcents ---

    #[test]
    fn turn_cost_microcents_field_is_optional() {
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();

        let turn_with_cost = Turn {
            id,
            thread_id,
            agent_id: "agent-1".to_string(),
            state: TurnState::Completed,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_microcents: Some(42),
            structured_output: None,
            created_at: now,
            completed_at: Some(now),
        };
        assert_eq!(turn_with_cost.cost_microcents, Some(42));

        let turn_no_cost = Turn {
            cost_microcents: None,
            ..turn_with_cost.clone()
        };
        assert_eq!(turn_no_cost.cost_microcents, None);
    }

    #[test]
    fn turn_serialises_with_cost_microcents() {
        let turn = Turn {
            id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            state: TurnState::Completed,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_microcents: Some(999),
            structured_output: None,
            created_at: Utc::now(),
            completed_at: None,
        };
        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        assert!(
            json.contains("cost_microcents"),
            "cost_microcents field should appear in JSON"
        );
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");
        assert_eq!(back.cost_microcents, Some(999));
    }

    // --- Turn.structured_output ---

    #[test]
    fn turn_serialises_with_structured_output_present() {
        let schema_result = serde_json::json!({"answer": "Paris", "confidence": 0.97});
        let turn = Turn {
            id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            state: TurnState::Completed,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: Some("the answer is Paris".to_string()),
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: Some(schema_result.clone()),
            created_at: Utc::now(),
            completed_at: None,
        };

        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        assert!(
            json.contains("structured_output"),
            "structured_output should appear in JSON"
        );
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");
        assert_eq!(back.structured_output, Some(schema_result));
    }

    #[test]
    fn turn_serialises_with_structured_output_absent() {
        let turn = Turn {
            id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            state: TurnState::Pending,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");
        assert!(back.structured_output.is_none());
    }

    // --- AgentMessage constructibility ---

    #[test]
    fn agent_message_is_constructible() {
        use swage_core::{MessageKind, MessageStatus};
        let id = Uuid::new_v4();
        let now = Utc::now();
        let msg = AgentMessage {
            id,
            sender_agent_id: "agent-a".to_string(),
            recipient_agent_id: "agent-b".to_string(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "summarise"}),
            status: MessageStatus::Pending,
            failure_reason: None,
            created_at: now,
            delivered_at: None,
        };
        assert_eq!(msg.id, id);
        assert_eq!(msg.kind, MessageKind::Delegate);
        assert_eq!(msg.status, MessageStatus::Pending);
    }

    #[test]
    fn agent_message_serialises_and_deserialises() {
        use swage_core::{MessageKind, MessageStatus};
        let now = Utc::now();
        let msg = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: "a".to_string(),
            recipient_agent_id: "b".to_string(),
            kind: MessageKind::Notify,
            correlation_id: Some(Uuid::new_v4()),
            payload: serde_json::json!({"content": "hello"}),
            status: MessageStatus::Delivered,
            failure_reason: None,
            created_at: now,
            delivered_at: Some(now),
        };
        let json = serde_json::to_string(&msg).expect("serialisation should succeed");
        let back: AgentMessage =
            serde_json::from_str(&json).expect("deserialisation should succeed");
        assert_eq!(back.sender_agent_id, "a");
        assert_eq!(back.recipient_agent_id, "b");
        assert_eq!(back.kind, MessageKind::Notify);
        assert_eq!(back.status, MessageStatus::Delivered);
    }

    #[test]
    fn new_agent_message_is_constructible() {
        use swage_core::MessageKind;
        let sender = Uuid::new_v4();
        let _ = sender; // correlation_id is Option<Uuid>, not necessarily sender
        let nm = NewAgentMessage {
            sender_agent_id: "agent-a".to_string(),
            recipient_agent_id: "agent-b".to_string(),
            kind: MessageKind::Cancel,
            correlation_id: Some(Uuid::new_v4()),
            payload: serde_json::json!({}),
        };
        assert_eq!(nm.sender_agent_id, "agent-a");
        assert_eq!(nm.kind, MessageKind::Cancel);
        assert!(nm.correlation_id.is_some());
    }
}
