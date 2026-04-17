use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tokio::sync::{Mutex, broadcast, mpsc};
use uuid::Uuid;

use crate::{state::AppState, subscription::SubscriptionState, turn};
use swage_agent::approval::ApprovalDecisionValue;

// ---------------------------------------------------------------------------
// Client → Server event types
// ---------------------------------------------------------------------------

/// Events sent by the client over the WebSocket connection.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// Send a message to the conversation, triggering a provider turn.
    SendMessage {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_id: Option<Uuid>,
    },
    /// Human responds to a pending approval request.
    ///
    /// The client sends this event when the user approves or rejects an action
    /// that was held for review by the agent's approval gate. The server
    /// resolves the pending `ApprovalRequired` event and unblocks (or cancels)
    /// the agent turn accordingly.
    ApprovalDecision {
        /// Identifies the approval request being resolved.
        approval_id: String,
        /// The human's verdict: `"approved"` or `"rejected"`.
        decision: String,
        /// Optional explanation from the human reviewer.
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
/// Wire-compatible with `swage_agent::event::SchemaValidationError` — defined
/// here so the gateway wire format is not coupled to agent internals. The two
/// structs must be kept structurally identical; deviations should be treated
/// as a protocol mismatch and surfaced in review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaValidationError {
    /// JSON pointer to the failing location (e.g. `"/properties/name"`).
    pub path: String,
    /// Human-readable description of the constraint that was violated.
    pub message: String,
}

/// Events sent by the server to the client over the WebSocket connection.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A token streamed from the provider.
    Token { content: String },
    /// A tool call has begun execution. Correlates with a later ToolEnd via call_id.
    ToolStart {
        call_id: String,
        tool: String,
        arguments: serde_json::Value,
        /// call_id of the enclosing tool if this is a sub-tool invocation.
        ///
        /// Omitted from the serialised JSON when None so existing clients that
        /// do not handle sub-tools remain unaffected.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    /// A tool call has finished. Contains the result and execution metadata.
    ToolEnd {
        call_id: String,
        tool: String,
        result: serde_json::Value,
        /// "success" or "error".
        status: String,
        duration_ms: u64,
        truncated: bool,
        total_bytes: Option<u64>,
        /// call_id of the enclosing tool if this is a sub-tool invocation.
        ///
        /// Omitted from the serialised JSON when None.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    /// Context assembly completed for a turn.
    ///
    /// Contains the full assembly report for diagnostics and token budget
    /// inspection. Only delivered to connections subscribed to the `"context"`
    /// channel unless the report contains errors, in which case it is always
    /// sent so the client can surface assembly failures to the user.
    ///
    /// The report is carried as `serde_json::Value` to decouple the wire
    /// format from swage-context's internal types — the shape matches
    /// `swage_context::report::AssemblyReport` when serialised.
    ContextAssembled {
        turn_id: Uuid,
        report: serde_json::Value,
    },
    /// The provider has finished and the turn has been committed.
    ///
    /// The base event always includes `turn_id`. Usage fields are populated
    /// only for connections subscribed to the `"context"` channel — unsubscribed
    /// clients receive `turn_id` alone (the `skip_serializing_if` annotation
    /// keeps the JSON shape backward-compatible).
    TurnComplete {
        turn_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_microcents: Option<i64>,
        /// Validated, parsed JSON output when an output schema was configured and
        /// validation passed. Absent (serialised as omitted) when no schema is
        /// configured or when validation failed after retry exhaustion.
        #[serde(skip_serializing_if = "Option::is_none")]
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
    /// All schema validation retries were exhausted without a valid response.
    ///
    /// Always delivered regardless of subscription state — this is an error
    /// condition that the client must surface to the user.
    OutputSchemaFailed {
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
        parent_turn_id: Uuid,
        child_turn_id: Option<Uuid>,
        target_agent_id: String,
        success: bool,
    },
    /// A topology run has started for a session.
    TopologyRunStarted {
        session_id: Uuid,
        topology_run_id: Uuid,
        team_id: Uuid,
    },
    /// A topology node has started for a session.
    TopologyNodeStarted {
        session_id: Uuid,
        topology_run_id: Uuid,
        node_execution_id: Uuid,
        node_id: String,
    },
    /// A topology node has finished for a session.
    TopologyNodeCompleted {
        session_id: Uuid,
        topology_run_id: Uuid,
        node_execution_id: Uuid,
        node_id: String,
        success: bool,
    },
    /// A topology run has finished for a session.
    TopologyRunCompleted {
        session_id: Uuid,
        topology_run_id: Uuid,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_output: Option<serde_json::Value>,
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
        approval_id: String,
        /// Turn that triggered the approval gate.
        turn_id: String,
        /// Agent that raised the gate.
        agent_id: String,
        /// Category of the action requiring approval (e.g. `"tool_call"`).
        kind: String,
        /// Name of the tool involved, if the approval is for a tool call.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        /// Parsed arguments of the tool call, for display in the approval UI.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_arguments: Option<serde_json::Value>,
        /// Human-readable description of the action to be approved.
        prompt: String,
        /// Additional context the client may render to assist the decision.
        #[serde(skip_serializing_if = "Option::is_none")]
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
        approval_id: String,
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
        approval_id: String,
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
        approval_id: String,
        /// Machine-readable error code (e.g. `"already_decided"`, `"not_found"`).
        error: String,
        /// Human-readable description of the error.
        message: String,
        /// Current state of the request, if known (e.g. `"approved"`, `"expired"`).
        #[serde(skip_serializing_if = "Option::is_none")]
        current_state: Option<String>,
    },
}

/// Well-known error codes for the `Error` event.
pub mod error_codes {
    pub const PROVIDER_ERROR: &str = "provider_error";
    pub const TURN_IN_PROGRESS: &str = "turn_in_progress";
    pub const INVALID_CONFIG: &str = "invalid_config";
    pub const INTERNAL_ERROR: &str = "internal_error";
    pub const SKILL_NOT_FOUND: &str = "skill_not_found";
}

// ---------------------------------------------------------------------------
// Route handler
// ---------------------------------------------------------------------------

/// Validate preconditions for a WebSocket upgrade.
///
/// Returns `None` if the upgrade can proceed, or `Some(error_response)` if it
/// should be rejected. Extracted so the precondition logic can be tested
/// independently of the WebSocket upgrade machinery.
pub(crate) async fn check_upgrade_preconditions(
    state: &AppState,
    session_id: Uuid,
) -> Option<Response> {
    // Verify the conversation (session) exists in storage.
    match state.storage.get_session(session_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Some(
                (
                    StatusCode::NOT_FOUND,
                    axum::Json(serde_json::json!({
                        "code": "not_found",
                        "message": format!("conversation {session_id} not found")
                    })),
                )
                    .into_response(),
            );
        }
        Err(e) => {
            tracing::error!(%session_id, error = %e, "storage error checking conversation");
            return Some(
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({
                        "code": "internal_error",
                        "message": "failed to check conversation"
                    })),
                )
                    .into_response(),
            );
        }
    }

    // Reject duplicate connections to the same conversation.
    if state.active_connections.lock().await.contains(&session_id) {
        return Some(
            (
                StatusCode::CONFLICT,
                axum::Json(serde_json::json!({
                    "code": "already_connected",
                    "message": format!("conversation {session_id} already has an active connection")
                })),
            )
                .into_response(),
        );
    }

    None
}

/// Axum route handler for `GET /api/conversations/:id/ws`.
///
/// Validates the conversation exists and has no existing live connection,
/// then upgrades to a WebSocket.
pub async fn ws_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(rejection) = check_upgrade_preconditions(&state, session_id).await {
        return rejection;
    }

    // Mark connection as active now that preconditions passed.
    state.active_connections.lock().await.insert(session_id);
    crate::presence::record_client_connected(&state, session_id).await;

    ws.on_upgrade(move |socket| handle_connection(socket, state, session_id))
}

/// Handle an established WebSocket connection for a conversation.
///
/// Splits the socket into a sender and receiver. Incoming client events are
/// dispatched to turn processing; outgoing server events are forwarded from a
/// channel to the WebSocket.
///
/// If the client disconnects while a turn is active, the turn continues in the
/// background and writes its result to storage.
async fn handle_connection(socket: WebSocket, state: AppState, session_id: Uuid) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Per-connection observability subscription state. Initialised with
    // force_on from config so all events are delivered when the gateway is
    // running in development/debug mode without requiring explicit subscriptions.
    let subscription = Arc::new(Mutex::new(SubscriptionState::new(
        state.config.observability.force_on,
    )));

    // Subscribe to the server-wide broadcast channel before spawning the
    // forwarding task so no broadcast events (e.g. Shutdown) are missed.
    let mut bcast_rx = state.broadcast.subscribe();

    // Channel that turn processing uses to send events back to this connection.
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    // Spawn a forwarding task: reads from both the per-turn mpsc channel and
    // the server-wide broadcast channel, writing each event to the WebSocket.
    let mut forward_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // Per-turn events (Token, ToolStart, ToolEnd, TurnComplete, etc.)
                event = rx.recv() => {
                    match event {
                        None => break, // All senders dropped — turn and connection done.
                        Some(event) => {
                            let is_shutdown = matches!(event, ServerEvent::Shutdown { .. });
                            match serde_json::to_string(&event) {
                                Ok(json) => {
                                    if ws_sink.send(Message::Text(json.into())).await.is_err() {
                                        // Client disconnected — stop forwarding.
                                        break;
                                    }
                                    if is_shutdown {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "failed to serialise server event");
                                }
                            }
                        }
                    }
                }

                // Server-wide broadcast events (Shutdown plus session-scoped
                // topology progress). Delivery filtering happens here via the
                // session matcher rather than via per-connection subscriptions.
                result = bcast_rx.recv() => {
                    match result {
                        Ok(event) => {
                            if !broadcast_event_matches_session(session_id, &event) {
                                continue;
                            }
                            let is_shutdown = matches!(event, ServerEvent::Shutdown { .. });
                            match serde_json::to_string(&event) {
                                Ok(json) => {
                                    // Attempt the send; ignore the result for broadcast events.
                                    let _ = ws_sink.send(Message::Text(json.into())).await;
                                    if is_shutdown {
                                        // Server is going down — close after delivering the event.
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "failed to serialise broadcast event");
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            // The broadcast buffer overflowed; skip the missed events.
                            tracing::warn!(n, "broadcast receiver lagged, {n} event(s) dropped");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // Broadcast sender dropped (server shutdown) — close cleanly.
                            break;
                        }
                    }
                }
            }
        }

        // Send a close frame when the forwarding loop exits for any reason.
        let _ = ws_sink.send(Message::Close(None)).await;
    });

    if state.shutdown_signal.is_cancelled() {
        let _ = tx.send(ServerEvent::Shutdown {
            reason: "admin shutdown request".to_string(),
        });
    }

    // Re-emit any pending approvals for this session's agent. A client may
    // connect (or reconnect) after an approval was raised, so we catch them
    // up immediately rather than leaving them blind until the next turn.
    // Fail-open: storage errors are logged and the connection continues.
    // Re-emit pending approvals scoped to this session only. Previous
    // implementation used list_pending_by_agent which leaked approvals from
    // other sessions sharing the same agent_id.
    match state.storage.list_pending_by_session(session_id).await {
        Ok(pending) => {
            for approval in pending {
                let event = ServerEvent::ApprovalRequired {
                    approval_id: approval.id.to_string(),
                    turn_id: approval.turn_id.to_string(),
                    agent_id: approval.agent_id.clone(),
                    kind: approval.kind.to_string(),
                    tool_name: approval.tool_name.clone(),
                    tool_arguments: approval.tool_arguments.clone(),
                    prompt: approval.prompt.clone(),
                    context: approval.context.clone(),
                    timeout_at: approval.timeout_at.to_rfc3339(),
                    timeout_policy: approval.timeout_policy.to_string(),
                };
                // Ignore send errors: if the forward task has already
                // exited there is no client to receive the event.
                let _ = tx.send(event);
            }
        }
        Err(e) => {
            tracing::warn!(
                %session_id,
                error = %e,
                "failed to list pending approvals for re-emission on connect"
            );
        }
    }

    // Main receive loop: read client messages and dispatch.
    loop {
        tokio::select! {
            // If the forwarding task exits early (client disconnected), stop reading.
            _ = &mut forward_task => break,

            msg = ws_stream.next() => {
                match msg {
                    None => break, // Client closed the connection.
                    Some(Err(e)) => {
                        tracing::warn!(%session_id, error = %e, "WebSocket receive error");
                        break;
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Text(text))) => {
                        // Only actual messages reset the idle timer —
                        // an open but silent connection does not count.
                        state.activity_tracker.record_activity();
                        crate::presence::record_client_seen(&state, session_id).await;
                        dispatch_client_message(
                            text.as_str(),
                            &state,
                            session_id,
                            tx.clone(),
                            Arc::clone(&subscription),
                        )
                        .await;
                    }
                    Some(Ok(_)) => {
                        // Ignore binary, ping, and pong frames.
                    }
                }
            }
        }
    }

    // Connection is done — remove from the active set.
    {
        let mut connections = state.active_connections.lock().await;
        connections.remove(&session_id);
    }
    crate::presence::record_client_disconnected(&state, session_id).await;
}

fn broadcast_event_matches_session(session_id: Uuid, event: &ServerEvent) -> bool {
    match event {
        ServerEvent::Shutdown { .. } => true,
        ServerEvent::TopologyRunStarted {
            session_id: target, ..
        }
        | ServerEvent::TopologyNodeStarted {
            session_id: target, ..
        }
        | ServerEvent::TopologyNodeCompleted {
            session_id: target, ..
        }
        | ServerEvent::TopologyRunCompleted {
            session_id: target, ..
        } => *target == session_id,
        _ => false,
    }
}

/// Discover skills available to a session.
///
/// Resolves the Swage root, looks up the session's project slug and agent id,
/// builds the config hierarchy, and delegates to [`crate::skill::SkillManager`].
/// Infrastructure failures are returned to the caller so slash-command and
/// explicit activation flows can surface the real problem instead of treating
/// the outage as an empty skill catalogue.
async fn discover_available_for_session(
    state: &AppState,
    session_id: Uuid,
) -> Result<Vec<swage_config::skill::SkillDefinition>, String> {
    // Fetch session to get project_id and agent_id.
    let session = match state.storage.get_session(session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err(format!(
                "skill discovery failed: session {session_id} was not found"
            ));
        }
        Err(e) => {
            return Err(format!(
                "skill discovery failed while loading session {session_id}: {e}"
            ));
        }
    };

    // Fetch project to get the slug used in path construction.
    let project = match state.project_store.get(session.project_id).await {
        Ok(Some(p)) => p,
        Err(e) => {
            return Err(format!(
                "skill discovery failed while loading project {} for session {session_id}: {e}",
                session.project_id
            ));
        }
        _ => {
            return Err(format!(
                "skill discovery failed: project {} for session {session_id} was not found",
                session.project_id
            ));
        }
    };

    // Resolve the Swage root directory.
    let root = swage_config::discovery::resolve_root()
        .map_err(|e| format!("skill discovery failed for session {session_id}: {e}"))?;

    // Build hierarchy paths that mirror load_resolved_config_from.
    let project_dir = root.join("projects").join(&project.slug);
    let agent_dir = project_dir.join("agents").join(&session.agent_id);

    let (skills, _warnings) =
        crate::skill::SkillManager::discover_available(&root, &project_dir, Some(&agent_dir));
    Ok(skills)
}

/// Parse and dispatch a raw text frame from the client.
///
/// Subscribe/Unsubscribe update per-connection [`SubscriptionState`].
/// SendMessage intercepts slash commands before spawning a turn.
/// ActivateSkill and DeactivateSkill delegate to [`crate::skill::SkillManager`].
async fn dispatch_client_message(
    text: &str,
    state: &AppState,
    session_id: Uuid,
    tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
) {
    use crate::skill::{SkillActivationResult, SkillError, SkillManager, SlashCommandResult};
    use swage_context::tokeniser::TiktokenTokeniser;

    let event = match serde_json::from_str::<ClientEvent>(text) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(%session_id, error = %e, raw = text, "unparseable client event");
            let _ = tx.send(ServerEvent::Error {
                code: error_codes::INTERNAL_ERROR.to_string(),
                message: format!("invalid event: {e}"),
            });
            return;
        }
    };

    match event {
        ClientEvent::Subscribe { channels } => {
            // Update subscription state — unknown channel names are silently ignored.
            subscription.lock().await.subscribe(&channels);
        }
        ClientEvent::Unsubscribe { channels } => {
            subscription.lock().await.unsubscribe(&channels);
        }

        ClientEvent::SendMessage { content, thread_id } => {
            // Check for a slash command before deciding whether to spawn a turn.
            match crate::skill::parse_slash_command(&content) {
                SlashCommandResult::SkillActivation {
                    skill_name,
                    remaining_text,
                } => {
                    let available = match discover_available_for_session(state, session_id).await {
                        Ok(available) => available,
                        Err(message) => {
                            tracing::warn!(%session_id, %message, "skill discovery failed");
                            let _ = tx.send(ServerEvent::Error {
                                code: error_codes::INTERNAL_ERROR.to_string(),
                                message,
                            });
                            return;
                        }
                    };
                    let is_known = available.iter().any(|s| s.name == skill_name);

                    if is_known {
                        // Activate the skill and forward any trailing text as a turn.
                        let tok = TiktokenTokeniser::new(&state.config.provider.model);
                        match SkillManager::activate(
                            session_id,
                            &skill_name,
                            &available,
                            state.storage.as_ref(),
                            &tok,
                        )
                        .await
                        {
                            Ok(SkillActivationResult::Activated {
                                name,
                                display_name,
                                description,
                                token_count,
                            }) => {
                                let _ = tx.send(ServerEvent::SkillActivated {
                                    name,
                                    display_name,
                                    description,
                                    token_count,
                                });
                            }
                            Ok(SkillActivationResult::AlreadyActive) => {
                                let _ = tx.send(ServerEvent::SkillAlreadyActive {
                                    name: skill_name.clone(),
                                });
                            }
                            Err(SkillError::NotFound(n)) => {
                                tracing::warn!(%session_id, skill = %skill_name, "slash command skill not found");
                                let _ = tx.send(ServerEvent::Error {
                                    code: error_codes::SKILL_NOT_FOUND.to_string(),
                                    message: format!("skill '{n}' not found"),
                                });
                            }
                            Err(e) => {
                                tracing::warn!(%session_id, skill = %skill_name, error = ?e, "slash command skill activation failed");
                                let _ = tx.send(ServerEvent::Error {
                                    code: error_codes::INTERNAL_ERROR.to_string(),
                                    message: "skill activation failed".to_string(),
                                });
                            }
                        }

                        // If the slash command had trailing text, forward it as a turn message.
                        if let Some(text) = remaining_text {
                            let state = state.clone();
                            let sub = Arc::clone(&subscription);
                            tokio::spawn(async move {
                                if let Err(e) = turn::process_turn(
                                    &state,
                                    session_id,
                                    thread_id,
                                    text,
                                    tx.clone(),
                                    sub,
                                    None,
                                )
                                .await
                                {
                                    let (code, message) = classify_gateway_error(&e);
                                    let _ = tx.send(ServerEvent::Error { code, message });
                                }
                            });
                        }
                        // Bare `/skill-name` (no trailing text) completes with activation only.
                    } else {
                        // Unknown skill — pass the original message through unchanged.
                        let state = state.clone();
                        let sub = Arc::clone(&subscription);
                        tokio::spawn(async move {
                            if let Err(e) = turn::process_turn(
                                &state,
                                session_id,
                                thread_id,
                                content,
                                tx.clone(),
                                sub,
                                None,
                            )
                            .await
                            {
                                let (code, message) = classify_gateway_error(&e);
                                let _ = tx.send(ServerEvent::Error { code, message });
                            }
                        });
                    }
                }
                SlashCommandResult::NotACommand => {
                    // Regular message — spawn the turn as normal.
                    let state = state.clone();
                    let sub = Arc::clone(&subscription);
                    tokio::spawn(async move {
                        if let Err(e) = turn::process_turn(
                            &state,
                            session_id,
                            thread_id,
                            content,
                            tx.clone(),
                            sub,
                            None,
                        )
                        .await
                        {
                            let (code, message) = classify_gateway_error(&e);
                            let _ = tx.send(ServerEvent::Error { code, message });
                        }
                    });
                }
            }
        }

        ClientEvent::Cancel {} => {
            if let Err(e) = turn::cancel_turn(state, session_id).await {
                tracing::warn!(%session_id, error = %e, "cancel_turn failed");
                let _ = tx.send(ServerEvent::Error {
                    code: error_codes::INTERNAL_ERROR.to_string(),
                    message: e.to_string(),
                });
            }
        }

        ClientEvent::ActivateSkill { name } => {
            let available = match discover_available_for_session(state, session_id).await {
                Ok(available) => available,
                Err(message) => {
                    tracing::warn!(%session_id, skill = %name, %message, "skill discovery failed");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message,
                    });
                    return;
                }
            };
            let tok = TiktokenTokeniser::new(&state.config.provider.model);

            match SkillManager::activate(
                session_id,
                &name,
                &available,
                state.storage.as_ref(),
                &tok,
            )
            .await
            {
                Ok(SkillActivationResult::Activated {
                    name,
                    display_name,
                    description,
                    token_count,
                }) => {
                    let _ = tx.send(ServerEvent::SkillActivated {
                        name,
                        display_name,
                        description,
                        token_count,
                    });
                }
                Ok(SkillActivationResult::AlreadyActive) => {
                    let _ = tx.send(ServerEvent::SkillAlreadyActive { name });
                }
                Err(SkillError::NotFound(n)) => {
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::SKILL_NOT_FOUND.to_string(),
                        message: format!("skill '{n}' not found"),
                    });
                }
                Err(e) => {
                    tracing::error!(%session_id, skill = %name, error = ?e, "skill activation error");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "skill activation failed".to_string(),
                    });
                }
            }
        }

        ClientEvent::DeactivateSkill { name } => {
            let available = match discover_available_for_session(state, session_id).await {
                Ok(available) => available,
                Err(message) => {
                    tracing::warn!(%session_id, skill = %name, %message, "skill discovery failed");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message,
                    });
                    return;
                }
            };

            match SkillManager::deactivate(session_id, &name, &available, state.storage.as_ref())
                .await
            {
                Ok(()) => {
                    let _ = tx.send(ServerEvent::SkillDeactivated { name });
                }
                Err(SkillError::AgentLevelCannotDeactivate(n)) => {
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: format!("Cannot deactivate agent-level skill '{n}'"),
                    });
                }
                Err(e) => {
                    tracing::error!(%session_id, skill = %name, error = ?e, "skill deactivation error");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "skill deactivation failed".to_string(),
                    });
                }
            }
        }

        ClientEvent::ApprovalDecision {
            approval_id,
            decision,
            reason,
        } => {
            // Step 1: Parse the approval_id as a UUID.
            let approval_uuid = match approval_id.parse::<Uuid>() {
                Ok(id) => id,
                Err(_) => {
                    let _ = tx.send(ServerEvent::ApprovalDecisionError {
                        approval_id,
                        error: "invalid_decision".to_string(),
                        message: "approval_id is not a valid UUID.".to_string(),
                        current_state: None,
                    });
                    return;
                }
            };

            // Step 2: Validate that the decision string is one of the two legal values.
            if decision != "approved" && decision != "rejected" {
                let _ = tx.send(ServerEvent::ApprovalDecisionError {
                    approval_id: approval_uuid.to_string(),
                    error: "invalid_decision".to_string(),
                    message: format!(
                        "decision must be \"approved\" or \"rejected\", got \"{decision}\"."
                    ),
                    current_state: None,
                });
                return;
            }

            // Step 3: Look up the approval record in storage.
            let approval = match state.storage.get_approval(approval_uuid).await {
                Ok(Some(a)) => a,
                Ok(None) => {
                    let _ = tx.send(ServerEvent::ApprovalDecisionError {
                        approval_id: approval_uuid.to_string(),
                        error: "not_found".to_string(),
                        message: "No approval request found with that ID.".to_string(),
                        current_state: None,
                    });
                    return;
                }
                Err(e) => {
                    tracing::error!(%session_id, %approval_uuid, err = %e, "storage error looking up approval");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "Failed to look up approval request.".to_string(),
                    });
                    return;
                }
            };

            // Step 4: Atomically transition state from Pending → decided.
            //
            // `update_approval_state` uses an optimistic WHERE state='pending'
            // guard and returns false if the record was already resolved —
            // either by a concurrent client or by the timeout mechanism.
            let new_state = if decision == "approved" {
                swage_storage::ApprovalState::Approved
            } else {
                swage_storage::ApprovalState::Rejected
            };
            match state
                .storage
                .update_approval_state(approval_uuid, new_state, reason.as_deref())
                .await
            {
                Ok(true) => {} // Recorded successfully.
                Ok(false) => {
                    // Concurrent decision or expiry beat us to it.
                    let _ = tx.send(ServerEvent::ApprovalDecisionError {
                        approval_id: approval_uuid.to_string(),
                        error: "already_decided".to_string(),
                        message: "This approval request has already been resolved.".to_string(),
                        current_state: Some(approval.state.to_string()),
                    });
                    return;
                }
                Err(e) => {
                    tracing::error!(%session_id, %approval_uuid, err = %e, "storage error updating approval state");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "Failed to record approval decision.".to_string(),
                    });
                    return;
                }
            }

            // Step 5: Notify the waiting agent task via the in-process oneshot.
            //
            // Remove (not just read) the sender so only one decision fires through.
            // If no sender is present the agent either timed out or never registered —
            // proceed to emit the confirmed event anyway for UI consistency.
            {
                let mut channels = state
                    .approval_channels
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(sender) = channels.remove(&approval_uuid) {
                    let Ok(decision_value) = ApprovalDecisionValue::from_str(&decision) else {
                        tracing::error!(%session_id, %approval_uuid, decision = %decision, "invalid approval decision after validation");
                        let _ = tx.send(ServerEvent::Error {
                            code: error_codes::INTERNAL_ERROR.to_string(),
                            message: "Failed to record approval decision.".to_string(),
                        });
                        return;
                    };
                    let _ = sender.send(swage_agent::ApprovalDecision {
                        decision: decision_value,
                        reason: reason.clone(),
                    });
                }
            }

            // Step 6: Broadcast confirmation to the client.
            let _ = tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval_uuid.to_string(),
                decision,
                reason,
            });
        }
    }
}

/// Map a `GatewayError` to a (code, message) pair suitable for a client Error event.
fn classify_gateway_error(err: &swage_core::GatewayError) -> (String, String) {
    use swage_core::GatewayError;
    match err {
        GatewayError::TurnInProgress { .. } => {
            (error_codes::TURN_IN_PROGRESS.to_string(), err.to_string())
        }
        GatewayError::Provider(_) => (error_codes::PROVIDER_ERROR.to_string(), err.to_string()),
        GatewayError::ConfigError(_) => (error_codes::INVALID_CONFIG.to_string(), err.to_string()),
        _ => (error_codes::INTERNAL_ERROR.to_string(), err.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use tokio::sync::Mutex;

    use swage_provider::LlmProvider;
    use swage_storage::model::Session;

    use super::*;
    use crate::turn::tests::{MockProvider, MockStorage};

    // -----------------------------------------------------------------------
    // Event serialisation / deserialisation round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn client_event_send_message_deserialises() {
        let json = r#"{"type": "send_message", "content": "hello"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::SendMessage { content, .. } if content == "hello"));
    }

    #[test]
    fn client_event_cancel_deserialises() {
        let json = r#"{"type": "cancel"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::Cancel {}));
    }

    #[test]
    fn server_event_token_serialises() {
        let event = ServerEvent::Token {
            content: "hi".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "token");
        assert_eq!(json["content"], "hi");
    }

    #[test]
    fn server_event_turn_complete_serialises() {
        let id = Uuid::new_v4();
        let event = ServerEvent::TurnComplete {
            turn_id: id,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "turn_complete");
        assert_eq!(json["turn_id"], id.to_string());
        // Optional fields should be absent when None.
        assert!(json.get("input_tokens").is_none());
        assert!(json.get("output_tokens").is_none());
        assert!(json.get("cost_microcents").is_none());
        assert!(json.get("structured_output").is_none());
    }

    #[test]
    fn server_event_turn_complete_with_usage_serialises() {
        let id = Uuid::new_v4();
        let event = ServerEvent::TurnComplete {
            turn_id: id,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_microcents: Some(1500),
            structured_output: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "turn_complete");
        assert_eq!(json["turn_id"], id.to_string());
        assert_eq!(json["input_tokens"], 100);
        assert_eq!(json["output_tokens"], 50);
        assert_eq!(json["cost_microcents"], 1500);
    }

    #[test]
    fn server_event_error_serialises() {
        let event = ServerEvent::Error {
            code: "provider_error".to_string(),
            message: "overloaded".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "provider_error");
        assert_eq!(json["message"], "overloaded");
    }

    #[test]
    fn server_event_cancelled_serialises() {
        let event = ServerEvent::Cancelled {};
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "cancelled");
    }

    #[test]
    fn server_event_tool_start_serialises() {
        let event = ServerEvent::ToolStart {
            call_id: "toolu_01abc".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({"command": "ls -la"}),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_start");
        assert_eq!(json["call_id"], "toolu_01abc");
        assert_eq!(json["tool"], "shell");
        assert_eq!(json["arguments"]["command"], "ls -la");
    }

    #[test]
    fn server_event_tool_start_omits_parent_call_id_when_none() {
        let event = ServerEvent::ToolStart {
            call_id: "toolu_native".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        // parent_call_id must be absent entirely, not serialised as null,
        // so existing clients that don't handle nesting remain unaffected.
        assert!(
            !json.as_object().unwrap().contains_key("parent_call_id"),
            "parent_call_id should be absent when None, got: {json}"
        );
    }

    #[test]
    fn server_event_tool_start_includes_parent_call_id_when_some() {
        let event = ServerEvent::ToolStart {
            call_id: "sub_01".to_string(),
            tool: "shell".to_string(),
            arguments: serde_json::json!({}),
            parent_call_id: Some("py_call_01".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["parent_call_id"], "py_call_01");
    }

    #[test]
    fn server_event_tool_end_serialises() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_01abc".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("file1\nfile2\nfile3"),
            status: "success".to_string(),
            duration_ms: 142,
            truncated: false,
            total_bytes: Some(25),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_end");
        assert_eq!(json["call_id"], "toolu_01abc");
        assert_eq!(json["tool"], "shell");
        assert_eq!(json["result"], "file1\nfile2\nfile3");
        assert_eq!(json["status"], "success");
        assert_eq!(json["duration_ms"], 142);
        assert_eq!(json["truncated"], false);
        assert_eq!(json["total_bytes"], 25);
    }

    #[test]
    fn server_event_tool_end_omits_parent_call_id_when_none() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_native".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("ok"),
            status: "success".to_string(),
            duration_ms: 10,
            truncated: false,
            total_bytes: None,
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("parent_call_id"),
            "parent_call_id should be absent when None"
        );
    }

    #[test]
    fn server_event_tool_end_includes_parent_call_id_when_some() {
        let event = ServerEvent::ToolEnd {
            call_id: "sub_01".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("output"),
            status: "success".to_string(),
            duration_ms: 5,
            truncated: false,
            total_bytes: None,
            parent_call_id: Some("py_call_01".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["parent_call_id"], "py_call_01");
    }

    #[test]
    fn server_event_tool_end_serialises_without_total_bytes() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_02".to_string(),
            tool: "file_read".to_string(),
            result: serde_json::json!({"content": "hello"}),
            status: "success".to_string(),
            duration_ms: 5,
            truncated: false,
            total_bytes: None,
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_end");
        assert!(json["total_bytes"].is_null());
        // Structured result (object) serialises correctly.
        assert_eq!(json["result"]["content"], "hello");
    }

    #[test]
    fn server_event_tool_end_truncated() {
        let event = ServerEvent::ToolEnd {
            call_id: "toolu_03".to_string(),
            tool: "shell".to_string(),
            result: serde_json::json!("partial output..."),
            status: "success".to_string(),
            duration_ms: 30_000,
            truncated: true,
            total_bytes: Some(1_048_576),
            parent_call_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json["truncated"].as_bool().unwrap());
        assert_eq!(json["total_bytes"], 1_048_576);
    }

    // -----------------------------------------------------------------------
    // Observability event serialisation / deserialisation (ogpt)
    // -----------------------------------------------------------------------

    #[test]
    fn client_event_subscribe_deserialises() {
        let json = r#"{"type": "subscribe", "channels": ["context", "tools"]}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        match event {
            ClientEvent::Subscribe { channels } => {
                assert_eq!(channels, vec!["context", "tools"]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn client_event_subscribe_empty_channels_deserialises() {
        let json = r#"{"type": "subscribe", "channels": []}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::Subscribe { channels } if channels.is_empty()));
    }

    #[test]
    fn client_event_unsubscribe_deserialises() {
        let json = r#"{"type": "unsubscribe", "channels": ["tools"]}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        match event {
            ClientEvent::Unsubscribe { channels } => {
                assert_eq!(channels, vec!["tools"]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn server_event_context_assembled_serialises() {
        let turn_id = Uuid::new_v4();
        let report = serde_json::json!({
            "assembly_name": "default",
            "total_tokens": 512,
            "errors": [],
        });
        let event = ServerEvent::ContextAssembled {
            turn_id,
            report: report.clone(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "context_assembled");
        assert_eq!(json["turn_id"], turn_id.to_string());
        assert_eq!(json["report"]["assembly_name"], "default");
        assert_eq!(json["report"]["total_tokens"], 512);
    }

    #[test]
    fn server_event_context_assembled_round_trips() {
        let turn_id = Uuid::new_v4();
        let report = serde_json::json!({
            "assembly_name": "custom",
            "errors": [{"assembly_name": "custom", "error_kind": "timeout", "message": "timed out", "fallback_used": "default"}],
        });
        let event = ServerEvent::ContextAssembled { turn_id, report };
        let serialised = serde_json::to_string(&event).unwrap();
        // Verify type tag is present in the raw JSON string.
        assert!(
            serialised.contains(r#""type":"context_assembled""#),
            "expected context_assembled type tag, got: {serialised}",
        );
    }

    #[test]
    fn server_event_shutdown_serialises() {
        let event = ServerEvent::Shutdown {
            reason: "idle timeout".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "shutdown");
        assert_eq!(json["reason"], "idle timeout");
    }

    #[test]
    fn server_event_shutdown_round_trips() {
        let event = ServerEvent::Shutdown {
            reason: "admin request".to_string(),
        };
        let serialised = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialised).unwrap();
        assert_eq!(parsed["type"], "shutdown");
        assert_eq!(parsed["reason"], "admin request");
    }

    #[test]
    fn topology_run_started_serialises() {
        let event = ServerEvent::TopologyRunStarted {
            session_id: Uuid::nil(),
            topology_run_id: Uuid::new_v4(),
            team_id: Uuid::new_v4(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "topology_run_started");
        assert!(json["session_id"].is_string());
        assert!(json["topology_run_id"].is_string());
        assert!(json["team_id"].is_string());
    }

    #[test]
    fn topology_run_completed_omits_final_output_when_absent() {
        let event = ServerEvent::TopologyRunCompleted {
            session_id: Uuid::nil(),
            topology_run_id: Uuid::new_v4(),
            success: false,
            final_output: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "topology_run_completed");
        assert_eq!(json["success"], false);
        assert!(!json.as_object().unwrap().contains_key("final_output"));
    }

    #[test]
    fn broadcast_event_matches_only_its_session() {
        let session_id = Uuid::new_v4();
        let other_session_id = Uuid::new_v4();
        let event = ServerEvent::TopologyNodeStarted {
            session_id,
            topology_run_id: Uuid::new_v4(),
            node_execution_id: Uuid::new_v4(),
            node_id: "plan".into(),
        };

        assert!(broadcast_event_matches_session(session_id, &event));
        assert!(!broadcast_event_matches_session(other_session_id, &event));
        assert!(broadcast_event_matches_session(
            other_session_id,
            &ServerEvent::Shutdown {
                reason: "idle".into(),
            },
        ));
    }

    // -----------------------------------------------------------------------
    // Skill event serialisation / deserialisation [e65s]
    // -----------------------------------------------------------------------

    #[test]
    fn client_event_activate_skill_deserialises() {
        let json = r#"{"type": "activate_skill", "name": "code-review"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::ActivateSkill { name } if name == "code-review"));
    }

    #[test]
    fn client_event_deactivate_skill_deserialises() {
        let json = r#"{"type": "deactivate_skill", "name": "code-review"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ClientEvent::DeactivateSkill { name } if name == "code-review"));
    }

    #[test]
    fn server_event_skill_activated_serialises() {
        let event = ServerEvent::SkillActivated {
            name: "code-review".to_string(),
            display_name: "Code Review".to_string(),
            description: "Reviews code for quality issues".to_string(),
            token_count: 512,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "skill_activated");
        assert_eq!(json["name"], "code-review");
        assert_eq!(json["display_name"], "Code Review");
        assert_eq!(json["description"], "Reviews code for quality issues");
        assert_eq!(json["token_count"], 512);
    }

    #[test]
    fn server_event_skill_already_active_serialises() {
        let event = ServerEvent::SkillAlreadyActive {
            name: "code-review".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "skill_already_active");
        assert_eq!(json["name"], "code-review");
    }

    #[test]
    fn server_event_skill_deactivated_serialises() {
        let event = ServerEvent::SkillDeactivated {
            name: "code-review".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "skill_deactivated");
        assert_eq!(json["name"], "code-review");
    }

    // -----------------------------------------------------------------------
    // Skill dispatch tests [5cpd]
    // -----------------------------------------------------------------------

    static SWAGE_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[tokio::test]
    async fn activate_skill_with_missing_session_surfaces_discovery_error() {
        let state = make_empty_state();
        let session_id = Uuid::new_v4();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let json = r#"{"type":"activate_skill","name":"ghost-skill"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an event");
        match event {
            ServerEvent::Error { code, message } => {
                assert_eq!(code, error_codes::INTERNAL_ERROR);
                assert!(message.contains("skill discovery failed"));
                assert!(message.contains("session"));
            }
            other => panic!("expected Error event, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn slash_command_with_missing_session_surfaces_discovery_error() {
        let state = make_empty_state();
        let session_id = Uuid::new_v4();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let json = r#"{"type":"send_message","content":"/code-review"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an event");
        match event {
            ServerEvent::Error { code, message } => {
                assert_eq!(code, error_codes::INTERNAL_ERROR);
                assert!(message.contains("skill discovery failed"));
                assert!(message.contains("session"));
            }
            other => panic!("expected Error event, got: {other:?}"),
        }
    }

    /// ActivateSkill for an unknown skill name sends a skill_not_found error event.
    ///
    /// The mock storage has no skill root on disk, so discover_available returns
    /// an empty set, causing SkillManager::activate to return NotFound.
    #[tokio::test(flavor = "current_thread")]
    async fn activate_skill_event_for_unknown_skill_sends_not_found_error() {
        let _guard = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let swage_home = tempfile::TempDir::new().expect("temp swage home");
        unsafe { std::env::set_var("SWAGE_HOME", swage_home.path()) };

        let (state, session_id) = make_state_with_session().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let json = r#"{"type":"activate_skill","name":"ghost-skill"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an event");
        match event {
            ServerEvent::Error { code, .. } => {
                assert_eq!(
                    code, "skill_not_found",
                    "expected skill_not_found error code"
                );
            }
            other => panic!("expected Error event, got: {other:?}"),
        }

        unsafe { std::env::remove_var("SWAGE_HOME") };
    }

    /// DeactivateSkill for a skill not in available (and not in storage) sends SkillDeactivated.
    ///
    /// Since the skill is not in the available set there's no agent-level check,
    /// and storage.deactivate_skill is a no-op that returns Ok(false) → success.
    #[tokio::test(flavor = "current_thread")]
    async fn deactivate_skill_event_sends_skill_deactivated() {
        let _guard = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let swage_home = tempfile::TempDir::new().expect("temp swage home");
        unsafe { std::env::set_var("SWAGE_HOME", swage_home.path()) };

        let (state, session_id) = make_state_with_session().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let json = r#"{"type":"deactivate_skill","name":"code-review"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an event");
        assert!(
            matches!(event, ServerEvent::SkillDeactivated { name } if name == "code-review"),
            "expected SkillDeactivated event",
        );

        unsafe { std::env::remove_var("SWAGE_HOME") };
    }

    /// A plain (non-slash) SendMessage still dispatches normally — no skill events.
    #[tokio::test]
    async fn plain_send_message_does_not_emit_skill_event() {
        let (state, session_id) = make_state_with_session().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let json = r#"{"type":"send_message","content":"hello world"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        // The turn spawned by MockProvider will eventually send an event, but
        // we're only verifying no *skill* event appears first.
        // Give the spawned turn a moment to produce events, then drain.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        while let Ok(event) = rx.try_recv() {
            assert!(
                !matches!(
                    event,
                    ServerEvent::SkillActivated { .. } | ServerEvent::SkillAlreadyActive { .. }
                ),
                "unexpected skill event for plain message: {event:?}",
            );
        }
    }

    // -----------------------------------------------------------------------
    // Precondition tests (tested directly to avoid WebSocket upgrade ceremony)
    //
    // axum's WebSocketUpgrade extractor validates the actual TCP connection
    // in integration scenarios; for unit testing the precondition logic we
    // call check_upgrade_preconditions directly instead.
    // -----------------------------------------------------------------------

    /// Build an AppState with no sessions.
    fn make_empty_state() -> AppState {
        let storage = Arc::new(MockStorage::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![]));
        let storage_arc: Arc<dyn swage_storage::store::Storage> =
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>;
        let project_store: Arc<dyn swage_storage::store::ProjectStore> =
            storage as Arc<dyn swage_storage::store::ProjectStore>;
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        AppState {
            storage: storage_arc.clone(),
            project_store,
            provider,
            voice_provider: Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                "voice is unavailable for tests",
            )) as Arc<dyn swage_provider::VoiceProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            presence: Arc::new(Mutex::new(crate::presence::PresenceRuntime::default())),
            execution_hosts: Arc::new(Mutex::new(
                crate::execution_host::ExecutionHostRuntime::default(),
            )),
            pairing_codes: Arc::new(Mutex::new(HashMap::new())),
            secret_manager: Arc::new(crate::secret::SecretManager::new(Arc::clone(&storage_arc))),
        }
    }

    /// Build an AppState with a pre-inserted session.
    async fn make_state_with_session() -> (AppState, Uuid) {
        use swage_storage::store::ProjectStore;

        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                agent_id: "gateway".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                metadata: None,
            },
        );

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![]));
        let storage_arc: Arc<dyn swage_storage::store::Storage> =
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>;
        let project_store: Arc<dyn swage_storage::store::ProjectStore> =
            storage as Arc<dyn swage_storage::store::ProjectStore>;
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        let state = AppState {
            storage: storage_arc.clone(),
            project_store,
            provider,
            voice_provider: Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                "voice is unavailable for tests",
            )) as Arc<dyn swage_provider::VoiceProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            presence: Arc::new(Mutex::new(crate::presence::PresenceRuntime::default())),
            execution_hosts: Arc::new(Mutex::new(
                crate::execution_host::ExecutionHostRuntime::default(),
            )),
            pairing_codes: Arc::new(Mutex::new(HashMap::new())),
            secret_manager: Arc::new(crate::secret::SecretManager::new(Arc::clone(&storage_arc))),
        };

        (state, session_id)
    }

    #[tokio::test]
    async fn preconditions_reject_non_existent_conversation() {
        let state = make_empty_state();
        let missing_id = Uuid::new_v4();

        let response = check_upgrade_preconditions(&state, missing_id)
            .await
            .expect("should return Some rejection");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn preconditions_reject_duplicate_connection() {
        let (state, session_id) = make_state_with_session().await;

        // Pre-mark this session as already connected.
        state.active_connections.lock().await.insert(session_id);

        let response = check_upgrade_preconditions(&state, session_id)
            .await
            .expect("should return Some rejection");

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn preconditions_pass_for_valid_unconnected_session() {
        let (state, session_id) = make_state_with_session().await;

        let result = check_upgrade_preconditions(&state, session_id).await;
        assert!(result.is_none(), "valid session should pass preconditions");
    }

    // -----------------------------------------------------------------------
    // Schema validation events [z4ov]
    // -----------------------------------------------------------------------

    #[test]
    fn schema_validation_error_serialises() {
        let err = SchemaValidationError {
            path: "/properties/name".to_string(),
            message: "required property missing".to_string(),
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["path"], "/properties/name");
        assert_eq!(json["message"], "required property missing");
    }

    #[test]
    fn turn_complete_with_structured_output_serialises() {
        let id = Uuid::new_v4();
        let event = ServerEvent::TurnComplete {
            turn_id: id,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: Some(serde_json::json!({"x": 1})),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "turn_complete");
        assert_eq!(json["structured_output"]["x"], 1);
    }

    #[test]
    fn turn_complete_without_structured_output_omits_field() {
        let id = Uuid::new_v4();
        let event = ServerEvent::TurnComplete {
            turn_id: id,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("structured_output"),
            "structured_output should be absent when None, got: {json}",
        );
    }

    #[test]
    fn output_schema_retry_serialises() {
        let turn_id = Uuid::new_v4();
        let event = ServerEvent::OutputSchemaRetry {
            turn_id,
            attempt: 1,
            max_retries: 3,
            errors: vec![SchemaValidationError {
                path: "/name".to_string(),
                message: "required".to_string(),
            }],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "output_schema_retry");
        assert_eq!(json["turn_id"], turn_id.to_string());
        assert_eq!(json["attempt"], 1);
        assert_eq!(json["max_retries"], 3);
        assert_eq!(json["errors"][0]["path"], "/name");
        assert_eq!(json["errors"][0]["message"], "required");
    }

    #[test]
    fn output_schema_failed_serialises() {
        let turn_id = Uuid::new_v4();
        let event = ServerEvent::OutputSchemaFailed {
            turn_id,
            attempts: 3,
            errors: vec![SchemaValidationError {
                path: "/name".to_string(),
                message: "required".to_string(),
            }],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "output_schema_failed");
        assert_eq!(json["turn_id"], turn_id.to_string());
        assert_eq!(json["attempts"], 3);
        assert_eq!(json["errors"][0]["path"], "/name");
        assert_eq!(json["errors"][0]["message"], "required");
    }

    #[test]
    fn output_schema_retry_empty_errors_serialises() {
        // Retry with no specific validation errors (e.g. parse failure).
        let turn_id = Uuid::new_v4();
        let event = ServerEvent::OutputSchemaRetry {
            turn_id,
            attempt: 2,
            max_retries: 3,
            errors: vec![],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "output_schema_retry");
        assert_eq!(json["errors"], serde_json::json!([]));
    }

    // -----------------------------------------------------------------------
    // ApprovalDecision client event deserialisation [viuf]
    // -----------------------------------------------------------------------

    #[test]
    fn client_event_approval_decision_approved_deserialises() {
        let json =
            r#"{"type": "approval_decision", "approval_id": "apr_01", "decision": "approved"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        match event {
            ClientEvent::ApprovalDecision {
                approval_id,
                decision,
                reason,
            } => {
                assert_eq!(approval_id, "apr_01");
                assert_eq!(decision, "approved");
                assert!(reason.is_none(), "reason should be None when absent");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn client_event_approval_decision_rejected_with_reason_deserialises() {
        let json = r#"{"type": "approval_decision", "approval_id": "apr_02", "decision": "rejected", "reason": "too risky"}"#;
        let event: ClientEvent = serde_json::from_str(json).unwrap();
        match event {
            ClientEvent::ApprovalDecision {
                approval_id,
                decision,
                reason,
            } => {
                assert_eq!(approval_id, "apr_02");
                assert_eq!(decision, "rejected");
                assert_eq!(reason.as_deref(), Some("too risky"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Approval event serialisation [c79w]
    // -----------------------------------------------------------------------

    #[test]
    fn server_event_approval_required_serialises() {
        let event = ServerEvent::ApprovalRequired {
            approval_id: "apr_01".to_string(),
            turn_id: "tur_01".to_string(),
            agent_id: "gateway".to_string(),
            kind: "tool_call".to_string(),
            tool_name: Some("shell".to_string()),
            tool_arguments: Some(serde_json::json!({"command": "rm -rf /"})),
            prompt: "Allow shell execution?".to_string(),
            context: Some(serde_json::json!({"risk": "high"})),
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: "reject".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "approval_required");
        assert_eq!(json["approval_id"], "apr_01");
        assert_eq!(json["turn_id"], "tur_01");
        assert_eq!(json["agent_id"], "gateway");
        assert_eq!(json["kind"], "tool_call");
        assert_eq!(json["tool_name"], "shell");
        assert_eq!(json["tool_arguments"]["command"], "rm -rf /");
        assert_eq!(json["prompt"], "Allow shell execution?");
        assert_eq!(json["context"]["risk"], "high");
        assert_eq!(json["timeout_at"], "2026-01-01T00:00:00Z");
        assert_eq!(json["timeout_policy"], "reject");
    }

    #[test]
    fn server_event_approval_required_omits_optional_fields_when_none() {
        let event = ServerEvent::ApprovalRequired {
            approval_id: "apr_02".to_string(),
            turn_id: "tur_02".to_string(),
            agent_id: "gateway".to_string(),
            kind: "generic".to_string(),
            tool_name: None,
            tool_arguments: None,
            prompt: "Proceed?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: "interrupt".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "approval_required");
        let obj = json.as_object().unwrap();
        assert!(
            !obj.contains_key("tool_name"),
            "tool_name should be absent when None"
        );
        assert!(
            !obj.contains_key("tool_arguments"),
            "tool_arguments should be absent when None"
        );
        assert!(
            !obj.contains_key("context"),
            "context should be absent when None"
        );
    }

    #[test]
    fn server_event_approval_confirmed_serialises() {
        let event = ServerEvent::ApprovalConfirmed {
            approval_id: "apr_03".to_string(),
            decision: "approved".to_string(),
            reason: Some("looks safe".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "approval_confirmed");
        assert_eq!(json["approval_id"], "apr_03");
        assert_eq!(json["decision"], "approved");
        assert_eq!(json["reason"], "looks safe");
    }

    #[test]
    fn server_event_approval_confirmed_omits_reason_when_none() {
        let event = ServerEvent::ApprovalConfirmed {
            approval_id: "apr_04".to_string(),
            decision: "rejected".to_string(),
            reason: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "approval_confirmed");
        assert!(
            !json.as_object().unwrap().contains_key("reason"),
            "reason should be absent when None"
        );
    }

    #[test]
    fn server_event_approval_expired_serialises() {
        let event = ServerEvent::ApprovalExpired {
            approval_id: "apr_05".to_string(),
            timeout_policy: "reject".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "approval_expired");
        assert_eq!(json["approval_id"], "apr_05");
        assert_eq!(json["timeout_policy"], "reject");
    }

    #[test]
    fn server_event_approval_decision_error_serialises() {
        let event = ServerEvent::ApprovalDecisionError {
            approval_id: "apr_06".to_string(),
            error: "already_decided".to_string(),
            message: "Approval was already resolved.".to_string(),
            current_state: Some("approved".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "approval_decision_error");
        assert_eq!(json["approval_id"], "apr_06");
        assert_eq!(json["error"], "already_decided");
        assert_eq!(json["message"], "Approval was already resolved.");
        assert_eq!(json["current_state"], "approved");
    }

    #[test]
    fn server_event_approval_decision_error_omits_current_state_when_none() {
        let event = ServerEvent::ApprovalDecisionError {
            approval_id: "apr_07".to_string(),
            error: "not_found".to_string(),
            message: "Approval request not found.".to_string(),
            current_state: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "approval_decision_error");
        assert!(
            !json.as_object().unwrap().contains_key("current_state"),
            "current_state should be absent when None"
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_client_message — ApprovalDecision handler
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn approval_decision_invalid_uuid_sends_error() {
        let state = make_empty_state();
        let session_id = Uuid::new_v4();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let json =
            r#"{"type":"approval_decision","approval_id":"not-a-uuid","decision":"approved"}"#;
        dispatch_client_message(json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an error event");
        match event {
            ServerEvent::ApprovalDecisionError {
                error, approval_id, ..
            } => {
                assert_eq!(error, "invalid_decision");
                assert_eq!(approval_id, "not-a-uuid");
            }
            other => panic!("expected ApprovalDecisionError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_decision_invalid_value_sends_error() {
        let state = make_empty_state();
        let session_id = Uuid::new_v4();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let id = Uuid::new_v4();
        let json =
            format!(r#"{{"type":"approval_decision","approval_id":"{id}","decision":"maybe"}}"#);
        dispatch_client_message(&json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an error event");
        match event {
            ServerEvent::ApprovalDecisionError { error, .. } => {
                assert_eq!(error, "invalid_decision");
            }
            other => panic!("expected ApprovalDecisionError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_decision_not_found_sends_error() {
        let state = make_empty_state();
        let session_id = Uuid::new_v4();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));

        let id = Uuid::new_v4();
        let json =
            format!(r#"{{"type":"approval_decision","approval_id":"{id}","decision":"approved"}}"#);
        dispatch_client_message(&json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive an error event");
        match event {
            ServerEvent::ApprovalDecisionError {
                error, approval_id, ..
            } => {
                assert_eq!(error, "not_found");
                assert_eq!(approval_id, id.to_string());
            }
            other => panic!("expected ApprovalDecisionError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_decision_already_decided_sends_error() {
        use swage_storage::{ApprovalKind, ApprovalState, NewApprovalRequest};

        let state = make_empty_state();
        let session_id = Uuid::new_v4();

        // Create a pending approval and immediately mark it approved.
        let approval = state
            .storage
            .create_approval(NewApprovalRequest {
                turn_id: Uuid::new_v4(),
                agent_id: "gateway".to_string(),
                kind: ApprovalKind::Tool,
                tool_name: Some("shell".to_string()),
                tool_arguments: None,
                hook_name: None,
                prompt: "Allow shell?".to_string(),
                context: None,
                timeout_at: chrono::Utc::now(),
                timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
            })
            .await
            .expect("create approval");

        // Pre-decide the approval so the handler sees it as already resolved.
        state
            .storage
            .update_approval_state(approval.id, ApprovalState::Approved, None)
            .await
            .expect("update state");

        let (tx, mut rx) = mpsc::unbounded_channel();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));
        let json = format!(
            r#"{{"type":"approval_decision","approval_id":"{}","decision":"rejected"}}"#,
            approval.id
        );
        dispatch_client_message(&json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive error event");
        match event {
            ServerEvent::ApprovalDecisionError {
                error,
                current_state,
                ..
            } => {
                assert_eq!(error, "already_decided");
                assert!(
                    current_state.is_some(),
                    "current_state should reflect the existing state"
                );
            }
            other => panic!("expected ApprovalDecisionError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_decision_success_sends_confirmed() {
        use swage_storage::{ApprovalKind, NewApprovalRequest};

        let state = make_empty_state();
        let session_id = Uuid::new_v4();

        // Create a pending approval.
        let approval = state
            .storage
            .create_approval(NewApprovalRequest {
                turn_id: Uuid::new_v4(),
                agent_id: "gateway".to_string(),
                kind: ApprovalKind::Tool,
                tool_name: Some("write_file".to_string()),
                tool_arguments: None,
                hook_name: None,
                prompt: "Write to /tmp?".to_string(),
                context: None,
                timeout_at: chrono::Utc::now(),
                timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
            })
            .await
            .expect("create approval");

        let (tx, mut rx) = mpsc::unbounded_channel();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));
        let json = format!(
            r#"{{"type":"approval_decision","approval_id":"{}","decision":"approved","reason":"Looks safe"}}"#,
            approval.id
        );
        dispatch_client_message(&json, &state, session_id, tx, sub).await;

        let event = rx.recv().await.expect("should receive ApprovalConfirmed");
        match event {
            ServerEvent::ApprovalConfirmed {
                approval_id,
                decision,
                reason,
            } => {
                assert_eq!(approval_id, approval.id.to_string());
                assert_eq!(decision, "approved");
                assert_eq!(reason, Some("Looks safe".to_string()));
            }
            other => panic!("expected ApprovalConfirmed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_decision_fires_oneshot_channel() {
        use swage_storage::{ApprovalKind, NewApprovalRequest};

        let state = make_empty_state();
        let session_id = Uuid::new_v4();

        // Create a pending approval.
        let approval = state
            .storage
            .create_approval(NewApprovalRequest {
                turn_id: Uuid::new_v4(),
                agent_id: "gateway".to_string(),
                kind: ApprovalKind::Tool,
                tool_name: Some("deploy".to_string()),
                tool_arguments: None,
                hook_name: None,
                prompt: "Deploy to prod?".to_string(),
                context: None,
                timeout_at: chrono::Utc::now(),
                timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
            })
            .await
            .expect("create approval");

        // Register a oneshot channel so the handler can notify a waiting agent.
        let (oneshot_tx, oneshot_rx) =
            tokio::sync::oneshot::channel::<swage_agent::ApprovalDecision>();
        state
            .approval_channels
            .lock()
            .unwrap()
            .insert(approval.id, oneshot_tx);

        let (tx, _rx) = mpsc::unbounded_channel();
        let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
            false,
        )));
        let json = format!(
            r#"{{"type":"approval_decision","approval_id":"{}","decision":"rejected","reason":"Too risky"}}"#,
            approval.id
        );
        dispatch_client_message(&json, &state, session_id, tx, sub).await;

        // The oneshot should have fired with the rejection.
        let decision = oneshot_rx.await.expect("oneshot should have fired");
        assert_eq!(
            decision.decision,
            swage_agent::approval::ApprovalDecisionValue::Rejected
        );
        assert_eq!(decision.reason, Some("Too risky".to_string()));
    }
}
