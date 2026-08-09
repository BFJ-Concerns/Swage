//! Hook trait, event types, and outcome types.
//!
//! Defines the core abstractions for lifecycle hooks: the `Hook` trait that
//! implementations must satisfy, the `HookEvent` enum covering all lifecycle
//! points, and `HookOutcome` for hook responses.

use async_trait::async_trait;
use serde_json::Value;

/// Determines behaviour when a hook exceeds its timeout or panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureMode {
    /// Continue as if the hook returned `Continue` (permissive).
    FailOpen,
    /// Treat the failure as a `Block` (restrictive).
    FailClosed,
}

/// Lifecycle event dispatched to registered hooks.
///
/// Each variant carries a typed payload with the data relevant to that
/// lifecycle point. Hooks receive these by reference and decide how
/// to respond via `HookOutcome`.
#[derive(Debug, Clone)]
pub enum HookEvent {
    /// A new session has been initialised.
    SessionStart { session_id: String },

    /// A session is ending (logout, clear, etc.).
    SessionEnd { session_id: String },

    /// A tool is about to be executed. Hooks can block this.
    PreToolUse { tool_name: String, params: Value },

    /// A tool has finished executing. Hooks can observe but not block.
    PostToolUse {
        tool_name: String,
        params: Value,
        result: Value,
    },

    /// An inbound message has arrived before processing. Hooks can modify it.
    BeforeInbound { content: String },

    /// A response has been generated before delivery. Hooks can modify it.
    AfterResponse { content: String },

    /// Context compaction is about to occur. Hooks can preserve data.
    PreCompact { context: Value },

    /// An agent has stopped execution.
    AgentStop { agent_id: String },

    /// A message is about to be sent from one agent to another.
    ///
    /// Hooks can block this event to prevent the message from being created.
    /// This fires for all durable message kinds.
    MessageSend {
        sender_agent_id: String,
        recipient_agent_id: String,
        /// Snake_case message kind string (e.g. "delegate", "notify").
        kind: String,
        payload: Value,
    },

    /// A message has been delivered to the recipient agent's mailbox.
    ///
    /// Hooks can block to transition the message to Failed. This event does
    /// not fire for Notify messages (ephemeral, no delivery confirmation).
    /// Enforcement of the Notify exclusion is the caller's responsibility.
    MessageDeliver {
        message_id: String,
        sender_agent_id: String,
        recipient_agent_id: String,
        /// Snake_case message kind string (e.g. "delegate", "cancel").
        kind: String,
        payload: Value,
    },

    /// A message has been received and is about to be processed by the agent.
    ///
    /// Hooks can block to reject the message before the agent acts on it,
    /// transitioning the message to Failed.
    MessageReceive {
        message_id: String,
        sender_agent_id: String,
        recipient_agent_id: String,
        /// Snake_case message kind string (e.g. "delegate", "result").
        kind: String,
        payload: Value,
    },
}

impl HookEvent {
    /// Returns the variant name as a string, suitable for regex matching
    /// in the registry's event filter.
    pub fn event_name(&self) -> &str {
        match self {
            HookEvent::SessionStart { .. } => "SessionStart",
            HookEvent::SessionEnd { .. } => "SessionEnd",
            HookEvent::PreToolUse { .. } => "PreToolUse",
            HookEvent::PostToolUse { .. } => "PostToolUse",
            HookEvent::BeforeInbound { .. } => "BeforeInbound",
            HookEvent::AfterResponse { .. } => "AfterResponse",
            HookEvent::PreCompact { .. } => "PreCompact",
            HookEvent::AgentStop { .. } => "AgentStop",
            HookEvent::MessageSend { .. } => "MessageSend",
            HookEvent::MessageDeliver { .. } => "MessageDeliver",
            HookEvent::MessageReceive { .. } => "MessageReceive",
        }
    }
}

/// Result of hook execution, determining how the pipeline proceeds.
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// Continue execution unchanged.
    Continue,
    /// Continue with a modified event replacing the original in the chain.
    ContinueModified(Box<HookEvent>),
    /// Block execution with a reason.
    Block(String),
    /// Pause execution and request human approval before proceeding.
    ///
    /// Priority in the dispatch chain sits between `Block` (higher) and
    /// `Continue` (lower): if any hook blocks, approval requests from
    /// lower-priority hooks are never surfaced. If one hook requests approval
    /// and another continues, the approval request takes effect.
    RequestApproval {
        /// Human-readable explanation of why this action needs review.
        prompt: String,
        /// Structured data the reviewer may inspect (tool name, parameters,
        /// risk level, etc.). `None` when the prompt alone is sufficient.
        context: Option<Value>,
        /// Per-request timeout override. When `None`, the agent's configured
        /// HITL timeout applies.
        timeout: Option<std::time::Duration>,
    },
}

/// Async trait for lifecycle hook implementations.
///
/// Hooks intercept lifecycle events and return an outcome that controls
/// whether the pipeline continues, is modified, or is blocked. All
/// implementations must be `Send + Sync` to support concurrent dispatch.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use swage_hook::{Hook, HookEvent, HookOutcome};
///
/// struct LoggingHook;
///
/// #[async_trait]
/// impl Hook for LoggingHook {
///     fn name(&self) -> &str {
///         "logging"
///     }
///
///     async fn execute(&self, event: &HookEvent) -> HookOutcome {
///         println!("event: {}", event.event_name());
///         HookOutcome::Continue
///     }
/// }
/// ```
#[async_trait]
pub trait Hook: Send + Sync {
    /// Returns the hook's name for identification in logs and diagnostics.
    fn name(&self) -> &str;

    /// Executes the hook in response to a lifecycle event.
    async fn execute(&self, event: &HookEvent) -> HookOutcome;
}
