use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::context::AssemblyReport;

use super::approval::ApprovalDecisionScope;

// ---------------------------------------------------------------------------
// Client → Server event types
// ---------------------------------------------------------------------------

/// Events sent by the client over the WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum ClientEvent {
    /// Send a message to the conversation, triggering a provider turn.
    SendMessage {
        /// Correlates a queued acknowledgement with this client's send.
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<Uuid>,
        content: String,
        /// Content parts submitted beside the text, in order; absent means
        /// text only. The same contract as `SubmitTurnRequest.attachments`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(optional, as = "Option<_>")]
        attachments: Vec<super::turn::MessageAttachment>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_id: Option<Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        selected_model_id: Option<String>,
    },
    /// Human responds to a pending approval request.
    ///
    /// The client sends this event when the user approves or rejects an action
    /// that was held for review by the agent's approval gate. The server
    /// resolves the pending `ApprovalRequired` event and unblocks (or cancels)
    /// the agent turn accordingly.
    ApprovalDecision {
        /// Identifies the approval request being resolved.
        approval_id: Uuid,
        /// The human's verdict: `"approved"` or `"rejected"`.
        decision: String,
        /// How widely an approved decision should apply.
        #[serde(default, skip_serializing_if = "ApprovalDecisionScope::is_once")]
        #[ts(optional, as = "Option<_>")]
        scope: ApprovalDecisionScope,
        /// Optional explanation from the human reviewer.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Cancel the currently active turn.
    Cancel {},
    /// Subscribe to one or more observability channels by name.
    ///
    /// Valid channel names: `"context"`, `"tools"`. Unknown names are silently
    /// ignored so forward-compatible clients do not break on older servers.
    Subscribe { channels: Vec<String> },
    /// Unsubscribe from one or more observability channels by name.
    Unsubscribe { channels: Vec<String> },
    /// Client requests activating a named skill for the current session.
    ActivateSkill { name: String },
    /// Client requests deactivating a named skill for the current session.
    DeactivateSkill { name: String },
}

// ---------------------------------------------------------------------------
// Server → Client event types
// ---------------------------------------------------------------------------

/// A single JSON Schema validation error from output schema checking.
///
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SchemaValidationError {
    /// JSON pointer to the failing location (e.g. `"/properties/name"`).
    pub path: String,
    /// Human-readable description of the constraint that was violated.
    pub message: String,
}

/// Events sent by the server to the client over the WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum ServerEvent {
    /// A token streamed from the provider.
    Token { content: String },
    /// A tool call has begun execution. Correlates with a later ToolEnd via call_id.
    ToolStart {
        call_id: String,
        tool: String,
        #[ts(type = "Record<string, unknown>")]
        arguments: serde_json::Value,
        /// call_id of the enclosing tool if this is a sub-tool invocation.
        ///
        /// Omitted from the serialised JSON when None so existing clients that
        /// do not handle sub-tools remain unaffected.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
        /// One-line headline for the call, from the tool's declared display
        /// hints. Omitted when the tool declares none.
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// A tool call has finished. Contains the result and execution metadata.
    ToolEnd {
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        execution: Option<super::conversation::ToolExecution>,
        call_id: String,
        tool: String,
        #[ts(type = "string | Record<string, unknown> | null")]
        result: serde_json::Value,
        /// "success" or "error".
        status: String,
        #[ts(type = "number")]
        duration_ms: u64,
        truncated: bool,
        #[ts(optional, type = "number | null")]
        total_bytes: Option<u64>,
        /// call_id of the enclosing tool if this is a sub-tool invocation.
        ///
        /// Omitted from the serialised JSON when None.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    /// The server dropped `dropped` signals to this client because it fell
    /// behind. Clients showing surfaces refetch what they have open, as
    /// after a reconnect: a dropped signal may have been the only one.
    Lagged {
        #[ts(type = "number")]
        dropped: u64,
    },
    /// A surface changed: it was created, or a module was written or added.
    /// Clients showing surfaces refetch — the event is the signal, not the
    /// content.
    SurfaceUpdated {
        surface_id: Uuid,
        /// The module written; absent for surface-level changes (creation).
        #[serde(skip_serializing_if = "Option::is_none")]
        module_id: Option<Uuid>,
    },
    /// The shared surface library changed: an entry was saved or removed.
    /// Clients showing the catalogue refetch it — the event is the signal,
    /// not the content.
    LibraryUpdated { entry_id: Uuid },
    /// A message was recorded on a conversation by something other than the
    /// person at this client — a schedule's brief, a delegation's completion
    /// notice, a notice from the platform — so open views append it where it
    /// belongs without reloading. Carries the recorded message itself, the
    /// same view the conversation's detail would return.
    MessageAppended {
        session_id: Uuid,
        message: super::conversation::MessageView,
    },
    /// A message arrived while the session's turn was running and is held
    /// for the next turn; open views show it as queued.
    MessageQueued {
        session_id: Uuid,
        queued: super::conversation::QueuedMessageView,
    },
    /// The session's queue became the next turn's input: the queued
    /// messages are now recorded messages (each announced) and the queue is
    /// empty.
    /// The queue ran as the next turn: the held messages whose ids are
    /// listed are now recorded rows (each announced on its own); a message
    /// queued after the drain took its batch is not among them and stays
    /// held.
    QueueDrained {
        session_id: Uuid,
        turn_id: Uuid,
        queued_ids: Vec<Uuid>,
    },
    /// Context assembly completed for a turn.
    ///
    /// Contains the full assembly report for diagnostics and token budget
    /// inspection. Only delivered to connections subscribed to the `"context"`
    /// channel unless the report contains errors, in which case it is always
    /// sent so the client can surface assembly failures to the user.
    ContextAssembled {
        turn_id: Uuid,
        report: AssemblyReport,
    },
    /// The provider has finished and the turn has been committed.
    ///
    /// The base event always includes `turn_id`. Usage fields are populated
    /// only for connections subscribed to the `"context"` channel — unsubscribed
    /// clients receive `turn_id` alone (the `skip_serializing_if` annotation
    /// keeps the JSON shape backward-compatible).
    TurnComplete {
        turn_id: Uuid,
        /// Output-limit cause when the reply is incomplete.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        incomplete_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        cost_microcents: Option<i64>,
        /// Validated, parsed JSON output when an output schema was configured and
        /// validation passed. Absent (serialised as omitted) when no schema is
        /// configured or when validation failed after retry exhaustion.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "unknown")]
        structured_output: Option<serde_json::Value>,
    },
    /// A schema validation attempt failed and the agent will retry.
    ///
    /// Only delivered to connections subscribed to the `"context"` channel —
    /// these are observability events that provide retry progress to developers
    /// and diagnostic tooling. Normal clients that have not subscribed do not
    /// receive intermediate retry noise.
    OutputSchemaRetry {
        turn_id: Uuid,
        /// 1-indexed attempt number (first retry = 1).
        attempt: u32,
        /// Maximum configured retries for this turn.
        max_retries: u32,
        /// Validation errors from this attempt.
        errors: Vec<SchemaValidationError>,
    },
    /// Provider retry progress for the active turn.
    ProviderRetryStarted {
        turn_id: Uuid,
        attempt: u32,
        max_retries: u32,
        provider: String,
        #[ts(type = "number")]
        wait_ms: u64,
    },
    /// Provider retry wait has settled.
    ProviderRetrySettled {
        turn_id: Uuid,
        attempt: u32,
        max_retries: u32,
        provider: String,
        success: bool,
    },
    /// All schema validation retries were exhausted without a valid response.
    ///
    /// Always delivered regardless of subscription state — this is an error
    /// condition that the client must surface to the user.
    OutputSchemaFailed {
        failure_message: String,
        turn_id: Uuid,
        /// Total number of attempts made (including the initial attempt).
        attempts: u32,
        /// Validation errors from the final attempt.
        errors: Vec<SchemaValidationError>,
    },
    /// A delegation from one agent to another has started.
    /// Always delivered — clients need delegation lifecycle events to track
    /// sub-agent progress regardless of subscription state.
    DelegationStarted {
        parent_turn_id: Uuid,
        child_turn_id: Option<Uuid>,
        target_agent_id: String,
    },
    /// A delegation has completed (successfully or with failure).
    /// Always delivered — matches DelegationStarted semantics.
    DelegationCompleted {
        /// Output-limit cause when the child response is incomplete.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        incomplete_reason: Option<String>,
        parent_turn_id: Uuid,
        child_turn_id: Option<Uuid>,
        target_agent_id: String,
        success: bool,
    },
    /// A detached workflow run has started.
    WorkflowRunStarted {
        event: super::workflow::WorkflowRunStartedEvent,
    },
    /// A workflow journal entry changed state.
    WorkflowEntryStarted {
        event: super::workflow::WorkflowEntryEvent,
    },
    WorkflowEntryCompleted {
        event: super::workflow::WorkflowEntryEvent,
    },
    /// A detached workflow run reached a terminal state.
    WorkflowRunCompleted {
        event: super::workflow::WorkflowRunCompletedEvent,
    },
    /// An error occurred during or before turn processing.
    Error { code: String, message: String },
    /// A pending turn was successfully cancelled.
    Cancelled {},
    /// The server is shutting down. Clients should clean up and disconnect.
    ///
    /// Always delivered regardless of subscription state — a client that
    /// misses this event will simply see the connection close without a
    /// graceful handshake.
    Shutdown { reason: String },
    /// A skill was successfully activated for the current session.
    ///
    /// Includes metadata so the client can display the skill without a
    /// separate lookup.
    SkillActivated {
        name: String,
        display_name: String,
        description: String,
        /// Token budget consumed by this skill's instructions in the context window.
        token_count: usize,
    },
    /// A skill was already active — no state change occurred.
    SkillAlreadyActive { name: String },
    /// A skill was successfully deactivated for the current session.
    SkillDeactivated { name: String },

    /// A tool call (or other action) requires human approval before proceeding.
    ///
    /// Delivered when the agent's dispatch pipeline raises an approval gate.
    /// The client should present the approval UI and respond with a
    /// `ClientEvent::ApprovalDecision` before `timeout_at` elapses.
    ApprovalRequired {
        /// Unique identifier for this approval request.
        approval_id: Uuid,
        /// Turn that triggered the approval gate.
        turn_id: Uuid,
        /// Agent that raised the gate.
        agent_id: String,
        /// Category of the action requiring approval (e.g. `"tool_call"`).
        kind: String,
        /// Name of the tool involved, if the approval is for a tool call.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        /// Parsed arguments of the tool call, for display in the approval UI.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "Record<string, unknown>")]
        tool_arguments: Option<serde_json::Value>,
        /// Human-readable description of the action to be approved.
        prompt: String,
        /// Additional context the client may render to assist the decision.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "Record<string, unknown>")]
        context: Option<serde_json::Value>,
        /// ISO 8601 timestamp after which the approval is considered expired.
        timeout_at: String,
        /// Policy applied on expiry: `"reject"` or `"interrupt"`.
        timeout_policy: String,
    },
    /// The approval decision has been recorded and forwarded to the agent.
    ///
    /// Sent to the client that submitted the decision as an acknowledgement,
    /// and broadcast to other clients on the same session for consistency.
    ApprovalConfirmed {
        /// Identifier of the resolved approval request.
        approval_id: Uuid,
        /// The recorded decision: `"approved"` or `"rejected"`.
        decision: String,
        /// Optional explanation provided by the human reviewer.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// An approval request timed out before a decision was submitted.
    ///
    /// The `timeout_policy` field indicates the action that was taken
    /// automatically as a result of the expiry.
    ApprovalExpired {
        /// Identifier of the expired approval request.
        approval_id: Uuid,
        /// Policy that was applied: `"reject"` or `"interrupt"`.
        timeout_policy: String,
    },
    /// A submitted approval decision was rejected by the server.
    ///
    /// Possible causes include attempting to decide an already-resolved request
    /// or submitting an unknown `approval_id`. The `error` field carries a
    /// machine-readable code; `message` gives a human-readable explanation.
    ApprovalDecisionError {
        /// Identifier of the approval request that could not be decided.
        approval_id: Uuid,
        /// Machine-readable error code (e.g. `"already_decided"`, `"not_found"`).
        error: String,
        /// Human-readable description of the error.
        message: String,
        /// Current state of the request, if known (e.g. `"approved"`, `"expired"`).
        #[serde(skip_serializing_if = "Option::is_none")]
        current_state: Option<String>,
    },
    /// A gateway event this client version does not understand.
    ///
    /// This is constructed by the Rust client after a tagged event fails to
    /// deserialise; the gateway never emits it directly.
    Unknown {
        event_type: String,
        #[ts(type = "unknown")]
        payload: serde_json::Value,
    },
}
