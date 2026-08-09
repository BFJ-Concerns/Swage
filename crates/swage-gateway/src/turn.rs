// TODO Audit turn-processing storage fallbacks
// Review fallback paths in this module that substitute default values when
// stored turn, replay, or context data is degraded. Each path should either
// propagate failure, log explicit degraded-state behaviour, or keep the fallback
// with a test explaining why defaulting is safe.
// ---

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{env, mem};

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use std::collections::{BTreeMap, HashMap, HashSet};

use swage_core::{
    ApprovalTimeoutPolicy, ChatMessage, GatewayError, Role, ToolCall, ToolResult,
    build_tool_round_messages,
};
use swage_hook::{HookEvent, HookOutcome, HookRegistry};
use swage_storage::model::{
    AppendMessage, ApprovalKind, ApprovalState, InboundAudit, NewAgentMessage, NewApprovalRequest,
    NewTurn, ToolCallRecord, TurnState,
};

use swage_config::{HierarchyLevel, prompt_rule::PromptRule};
use swage_context::element::ContextElement;
use swage_context::history::HistoryElement;
use swage_context::memory::MemoryElement;
use swage_context::prompts::PromptsElement;
use swage_context::skills::{ActiveSkill, SkillsElement};
use swage_context::tokeniser::TiktokenTokeniser;

use swage_agent::{
    AgentBuilder, AgentConfig, AgentError, AgentEvent, DelegationExecutor, DelegationHandler,
    MessageRouter, TurnRequest,
};
use swage_safety::LeakDetector;
use swage_sandbox::{Sandbox, llm_handler::HostLlmHandler};
use swage_tool::dispatch::ToolDispatcher;
use swage_tool::permission::PermissionConfig;
use swage_tool::permission::RequireApprovalEntry as ToolRequireApprovalEntry;
use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};
use swage_tool::tool::Tool;

use crate::skill::SkillManager;
use crate::tool_executor::{GatewayExecutionStrategy, SandboxedExecutor};

use crate::{state::AppState, subscription::SubscriptionState, websocket::ServerEvent};

mod configuration;
mod context;
mod delegation;
mod dispatcher;
mod history;
mod workspace;

pub(crate) use configuration::*;
pub(crate) use context::*;
pub(crate) use delegation::*;
pub(crate) use dispatcher::*;
#[cfg(test)]
use history::compact_tool_replay_messages;
use history::rebuild_thread_history_messages;
pub(crate) use workspace::*;

// Gateway messaging spans three surfaces: REST endpoints for durable messages
// and turn submission, session-scoped WebSocket events for live approval and
// delegation updates, and in-process routing between running agents.

fn map_turn_creation_error(error: swage_core::StorageError, session_id: Uuid) -> GatewayError {
    match error {
        swage_core::StorageError::ConstraintViolation(_) => {
            GatewayError::TurnInProgress { session_id }
        }
        other => GatewayError::Storage(other),
    }
}

fn append_provider_visible_request(
    report_json: &mut serde_json::Value,
    round: usize,
    request: serde_json::Value,
) {
    provider_visible_array(report_json, "requests").push(serde_json::json!({
        "round": round,
        "request": request,
    }));
}

fn append_provider_visible_capture_error(report_json: &mut serde_json::Value, round: usize) {
    provider_visible_array(report_json, "capture_errors").push(serde_json::json!({
        "round": round,
        "kind": "secret_redaction_unavailable",
    }));
}

async fn redact_provider_visible_request(
    state: &AppState,
    mut request: serde_json::Value,
) -> Result<serde_json::Value, swage_core::StorageError> {
    let secret_values = known_secret_values(state).await?;
    if !secret_values.is_empty() {
        redact_secret_values(&mut request, &secret_values);
    }
    Ok(request)
}

async fn known_secret_values(state: &AppState) -> Result<Vec<String>, swage_core::StorageError> {
    let records = state.storage.list_secrets().await?;

    let mut values = Vec::new();
    for record in records
        .into_iter()
        .filter(|record| record.status == "active")
    {
        let value = match record.backend_kind.as_str() {
            "managed" => Some(
                state
                    .storage
                    .get_managed_secret_value(record.id)
                    .await?
                    .ok_or_else(|| swage_core::StorageError::Corruption {
                        message: format!(
                            "active managed secret '{}' has no stored value",
                            record.id
                        ),
                    })?,
            ),
            "env" => record
                .external_reference
                .as_deref()
                .and_then(|variable| env::var(variable).ok()),
            _ => None,
        };
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            values.push(value);
        }
    }
    Ok(values)
}

fn redact_secret_values(value: &mut serde_json::Value, secret_values: &[String]) {
    match value {
        serde_json::Value::String(text) => {
            let mut redacted = mem::take(text);
            for secret in secret_values {
                redacted = redacted.replace(secret, "[REDACTED_SECRET]");
            }
            *text = redacted;
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_secret_values(value, secret_values);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_secret_values(value, secret_values);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn append_provider_visible_reasoning(
    report_json: &mut serde_json::Value,
    round: usize,
    content: String,
    details: Option<serde_json::Value>,
) {
    provider_visible_array(report_json, "reasoning").push(serde_json::json!({
        "round": round,
        "content": content,
        "details": details,
    }));
}

fn provider_visible_array<'a>(
    report_json: &'a mut serde_json::Value,
    key: &str,
) -> &'a mut Vec<serde_json::Value> {
    if !report_json.is_object() {
        let previous = std::mem::replace(report_json, serde_json::json!({}));
        report_json["assembly_report"] = previous;
    }

    let object = report_json
        .as_object_mut()
        .expect("report_json was normalised to an object");
    let provider_visible = object
        .entry("provider_visible".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !provider_visible.is_object() {
        *provider_visible = serde_json::json!({});
    }

    let provider_visible = provider_visible
        .as_object_mut()
        .expect("provider_visible was normalised to an object");
    let values = provider_visible
        .entry(key.to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !values.is_array() {
        *values = serde_json::json!([]);
    }
    values
        .as_array_mut()
        .expect("provider-visible value was normalised to an array")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxDegradationApproval {
    Approved,
    Rejected,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnAttendance {
    Attended,
    Unattended,
}

struct SandboxDegradationApprovalRequest<'a> {
    config: &'a swage_config::ResolvedConfig,
    ws_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    turn_id: Uuid,
    agent_id: &'a str,
    prompt: String,
    context: serde_json::Value,
    cancel_token: &'a CancellationToken,
}

async fn request_sandbox_degradation_approval(
    state: &AppState,
    request: SandboxDegradationApprovalRequest<'_>,
) -> Result<SandboxDegradationApproval, GatewayError> {
    let SandboxDegradationApprovalRequest {
        config,
        ws_tx,
        turn_id,
        agent_id,
        prompt,
        context,
        cancel_token,
    } = request;
    let timeout = std::time::Duration::from_secs(config.hitl.timeout);
    let timeout_at = chrono::Utc::now()
        + chrono::Duration::from_std(timeout)
            .unwrap_or_else(|_| chrono::Duration::seconds(config.hitl.timeout as i64));
    let timeout_policy = config.hitl.timeout_policy.clone();

    let approval = state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id,
            agent_id: agent_id.to_owned(),
            kind: ApprovalKind::Hook,
            tool_name: None,
            tool_arguments: None,
            hook_name: Some("sandbox_degradation".to_string()),
            prompt: prompt.clone(),
            context: Some(context.clone()),
            timeout_at,
            timeout_policy: timeout_policy.clone(),
        })
        .await?;

    state
        .storage
        .update_turn_state(turn_id, TurnState::AwaitingApproval)
        .await?;

    let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
    state
        .approval_channels
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(approval.id, decision_tx);

    let _ = ws_tx.send(ServerEvent::ApprovalRequired {
        approval_id: approval.id,
        turn_id,
        agent_id: agent_id.to_owned(),
        kind: "hook".to_string(),
        tool_name: None,
        tool_arguments: None,
        prompt,
        context: Some(context),
        timeout_at: timeout_at.to_rfc3339(),
        timeout_policy: timeout_policy.to_string(),
    });

    enum ApprovalWait {
        Decision(swage_agent::ApprovalDecision),
        Timeout,
        Cancelled,
    }

    let wait_result = tokio::select! {
        decision = decision_rx => match decision {
            Ok(decision) => ApprovalWait::Decision(decision),
            Err(_) => ApprovalWait::Timeout,
        },
        _ = tokio::time::sleep(timeout) => ApprovalWait::Timeout,
        _ = cancel_token.cancelled() => ApprovalWait::Cancelled,
    };

    state
        .approval_channels
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&approval.id);

    let outcome = match wait_result {
        ApprovalWait::Decision(decision) if decision.is_approved() => {
            state
                .storage
                .update_approval_state(
                    approval.id,
                    ApprovalState::Approved,
                    decision.reason.as_deref(),
                )
                .await?;
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval.id,
                decision: "approved".to_string(),
                reason: decision.reason,
            });
            SandboxDegradationApproval::Approved
        }
        ApprovalWait::Decision(decision) => {
            state
                .storage
                .update_approval_state(
                    approval.id,
                    ApprovalState::Rejected,
                    decision.reason.as_deref(),
                )
                .await?;
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval.id,
                decision: "rejected".to_string(),
                reason: decision.reason,
            });
            SandboxDegradationApproval::Rejected
        }
        ApprovalWait::Timeout => {
            state
                .storage
                .update_approval_state(
                    approval.id,
                    ApprovalState::Expired,
                    Some("approval timed out"),
                )
                .await?;
            let _ = ws_tx.send(ServerEvent::ApprovalExpired {
                approval_id: approval.id,
                timeout_policy: timeout_policy.to_string(),
            });
            match timeout_policy {
                ApprovalTimeoutPolicy::Reject => SandboxDegradationApproval::Rejected,
                ApprovalTimeoutPolicy::Interrupt => SandboxDegradationApproval::Interrupted,
            }
        }
        ApprovalWait::Cancelled => {
            state
                .storage
                .update_approval_state(approval.id, ApprovalState::Expired, Some("turn cancelled"))
                .await?;
            SandboxDegradationApproval::Interrupted
        }
    };

    if !matches!(outcome, SandboxDegradationApproval::Interrupted) {
        state
            .storage
            .update_turn_state(turn_id, TurnState::Processing)
            .await?;
    }
    Ok(outcome)
}

/// Distilled view of which permission grants the space has actually accepted
/// across its trusted workspace attachments. Untrusted attachments contribute
/// nothing.

#[derive(Debug, Clone)]
pub(crate) struct BeforeInboundDispatchResult {
    pub canonical_content: String,
    pub modified_by_hooks: Vec<String>,
}

fn map_hook_block(reason: String) -> GatewayError {
    GatewayError::ConfigError(format!(
        "BeforeInbound hook blocked inbound content: {reason}"
    ))
}

pub(crate) async fn dispatch_before_inbound(
    hook_registry: Option<&Arc<HookRegistry>>,
    raw_content: &str,
) -> Result<BeforeInboundDispatchResult, GatewayError> {
    let Some(hook_registry) = hook_registry else {
        return Ok(BeforeInboundDispatchResult {
            canonical_content: raw_content.to_string(),
            modified_by_hooks: Vec::new(),
        });
    };

    let traced = hook_registry
        .dispatch_traced(HookEvent::BeforeInbound {
            content: raw_content.to_string(),
        })
        .await;

    match traced.outcome {
        HookOutcome::Continue => Ok(BeforeInboundDispatchResult {
            canonical_content: raw_content.to_string(),
            modified_by_hooks: traced.modified_by_hooks,
        }),
        HookOutcome::ContinueModified(modified) => match *modified {
            HookEvent::BeforeInbound { content } => Ok(BeforeInboundDispatchResult {
                canonical_content: content,
                modified_by_hooks: traced.modified_by_hooks,
            }),
            _ => Ok(BeforeInboundDispatchResult {
                canonical_content: raw_content.to_string(),
                modified_by_hooks: traced.modified_by_hooks,
            }),
        },
        HookOutcome::Block(reason) => Err(map_hook_block(reason)),
        HookOutcome::RequestApproval { .. } => Err(GatewayError::ConfigError(
            "BeforeInbound hooks cannot request approval".to_string(),
        )),
    }
}

/// Orchestrate a complete agent turn: user message → provider stream → storage.
///
/// # Flow
/// 1. Look up the session's default thread (created alongside the session).
/// 2. Guard: reject if a turn is already Processing for this thread.
/// 3. Resolve the space for the session; load per-space config and build ToolContext.
/// 4. Create a Turn record (state=Pending), immediately advance to Processing.
/// 5. Store the user message with thread_id and position.
/// 6. Build message history from the thread and send a streaming request to the provider.
/// 7. For each chunk: forward Token events to the WebSocket, accumulate content.
/// 8. On Done: store the assistant message, mark the turn Completed, send TurnComplete.
/// 9. On Cancel: store any partial response, mark the turn Interrupted, send Cancelled.
/// 10. On provider Error: mark the turn Failed, send an Error event.
/// 11. Cleanup: remove the CancellationToken from active_turns.
// Because process_turn is spawned as a tokio task, it won't inherit the
// HTTP request span. The instrument macro creates a self-contained "turn"
// span so all log entries within the turn share the same turn_id and
// session_id. turn_id is Empty at span creation and recorded once the
// Turn record exists.
pub async fn process_turn(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    content: String,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn(
        state,
        session_id,
        target_thread_id,
        content,
        ws_tx,
        subscription,
        output_schema_override,
        None,
        None,
        TurnAttendance::Attended,
    )
    .await
}

#[tracing::instrument(
    name = "turn",
    skip(state, content, ws_tx, subscription),
    fields(turn_id = tracing::field::Empty),
)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_existing_turn(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    content: String,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
) -> Result<(), GatewayError> {
    // Step 1: Resolve the space and target thread for this session.
    let session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let thread = crate::thread_selection::resolve_thread_for_session(
        state.storage.as_ref(),
        session_id,
        target_thread_id,
    )
    .await?;
    let thread_id = thread.id;

    // Step 2: Guard — reject if a turn is already active for this thread.
    // Pending, Processing, AwaitingApproval, and Delegating all represent
    // in-progress work; allowing a second turn would interleave history.
    let existing_turns = state.storage.list_turns_by_thread(thread_id).await?;
    if existing_turns.iter().any(|t| {
        if existing_turn_id.is_some_and(|current_turn_id| t.id == current_turn_id) {
            return false;
        }
        matches!(
            t.state,
            TurnState::Pending
                | TurnState::Processing
                | TurnState::AwaitingApproval
                | TurnState::Delegating
        )
    }) {
        return Err(GatewayError::TurnInProgress { session_id });
    }

    // Step 3: Resolve the space for this session so we can load per-space config
    // and build the ToolContext passed to each tool invocation.
    let space_id = session.space_id;

    let space = state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
    let space_slug = space.slug.clone();

    // Load space-specific config. Missing space config falls back to the
    // startup config, but present invalid config fails the turn before any
    // user message is written.
    let config = load_session_config(state, &space_slug, &session.agent_id)?;
    ensure_selected_model_available(
        state,
        &config,
        session.selected_model_id.as_deref(),
        session_id,
    )?;
    let config = effective_session_config(config, session.selected_model_id.as_deref());

    // Resolve the swage root once — skill discovery, space memory loading,
    // and assembly search-path construction all depend on it.
    //
    // Fail-open: a warning is logged and each consumer falls back gracefully.
    let swage_root_opt = resolve_swage_root();

    let active_skills = load_active_skills_for_agent(
        state,
        session_id,
        &space_slug,
        &session.agent_id,
        swage_root_opt.as_deref(),
        true,
    )
    .await;

    // Step 4: Ensure the turn exists before any hook execution so the raw
    // input is recorded durably even when inbound processing blocks the turn.
    let prepared_turn_id = match existing_turn_id {
        Some(turn_id) => turn_id,
        None => {
            state
                .storage
                .create_turn(NewTurn {
                    thread_id,
                    agent_id: session.agent_id.clone(),
                    state: TurnState::Pending,
                    user_input: Some(content.clone()),
                    parent_turn_id: None,
                })
                .await
                .map_err(|error| map_turn_creation_error(error, session_id))?
                .id
        }
    };

    // Step 5: Set up the cancellation token before any turn processing begins.
    let cancel_token = CancellationToken::new();
    {
        let mut turns = state.active_turns.lock().await;
        turns.insert(session_id, cancel_token.clone());
    }

    // Step 6: Canonicalise inbound content before persisting any thread message.
    let before_inbound = match dispatch_before_inbound(state.hook_registry.as_ref(), &content).await
    {
        Ok(result) => result,
        Err(error) => {
            state
                .storage
                .update_turn_state(prepared_turn_id, TurnState::Failed)
                .await?;
            let mut turns = state.active_turns.lock().await;
            turns.remove(&session_id);
            return Err(error);
        }
    };

    if !before_inbound.modified_by_hooks.is_empty() {
        state
            .storage
            .update_inbound_audit(
                prepared_turn_id,
                &InboundAudit {
                    canonical_input: before_inbound.canonical_content.clone(),
                    modified_by_hooks: before_inbound.modified_by_hooks.clone(),
                },
            )
            .await?;
    }

    state
        .storage
        .append_message(AppendMessage {
            thread_id,
            turn_id: Some(prepared_turn_id),
            role: Role::User,
            content: before_inbound.canonical_content.clone(),
            agent_id: session.agent_id.clone(),
        })
        .await?;

    // Steps 8–9: Assemble context via the swage-context assembly engine.
    // Loads thread history, space memory, and constructs typed elements.
    // Runs fail-open assembly: custom Python configs when configured, built-in
    // default otherwise. The LLM provider is passed for two-phase compaction.
    let history_messages =
        rebuild_thread_history_messages(state.storage.as_ref(), thread_id).await?;

    // Load space memory from the filesystem. Returns an empty string when
    // no memory directory or files exist — MemoryElement handles empty content.
    let memory_content = match swage_root_opt.as_deref() {
        Some(root) => load_space_memory(root, &space_slug).await,
        None => String::new(),
    };

    let elements = build_assembly_elements(
        &config.instructions,
        active_skills,
        history_messages,
        memory_content,
        space.name.clone(),
    );
    let tokeniser = TiktokenTokeniser::new(config.default_model_id());

    // Assemble context with the built-in default implementation.
    let output = swage_context::assembly::assemble(&elements, &config.context, &tokeniser);
    let (assembled_messages, assembly_report) = (output.messages, output.report);

    tracing::debug!(
        total_tokens = assembly_report.total_tokens,
        elements = assembly_report.elements.len(),
        "context assembled"
    );

    // Serialise the assembly report for deferred emission.
    // The ContextAssembled event and snapshot persistence require a turn_id,
    // which is not available until the agent creates the Turn record and emits
    // TurnStarted. We prepare the report now and emit it in the event loop.
    let mut report_json = serde_json::to_value(&assembly_report).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to serialise assembly report");
        serde_json::json!({ "error": "serialisation failed" })
    });

    let workspace_policy = load_trusted_workspace_policy(
        &state.storage,
        space_id,
        session.active_workspace_attachment_id,
    )
    .await;
    let (workspace_root, mut allowed_paths) =
        agent_workspace_from_policy(&workspace_policy, space_id, session.id);
    let session_approval_grants = state
        .session_approval_grants
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    append_approved_location_paths(&mut allowed_paths, &session_approval_grants, space_id);
    let mut agent_config = build_agent_config_with_workspace(
        session.agent_id.clone(),
        &config,
        output_schema_override,
        &workspace_root,
        allowed_paths,
    );
    if let Some(max_retries) = max_output_retries_override {
        agent_config.max_output_retries = max_retries;
    }

    // Step 9: Resolve tool permissions from config cascade and build the
    // ToolDispatcher with the resolved permission set. When sandboxing is
    // enabled, this also spawns the per-turn sandbox and returns its cleanup
    // guard to this turn scope.
    let is_codex_turn = config.default_provider_protocol() == swage_config::Protocol::Codex;
    let mut dispatcher_config = config.clone();
    if is_codex_turn {
        // Bridged tools remain gateway-hosted. Codex-native actions are
        // isolated by the App Server sandbox policy instead.
        dispatcher_config.sandbox.enabled = false;
    }
    let dispatcher_build = match build_gateway_dispatcher(
        state,
        &dispatcher_config,
        &agent_config,
        session.metadata.as_ref(),
        Some(space_id),
        session.active_workspace_attachment_id,
        ws_tx.clone(),
    )
    .await
    {
        Ok(build) => build,
        Err(error)
            if dispatcher_config.sandbox.enabled && attendance == TurnAttendance::Attended =>
        {
            let detected_degradation = ExecutionIsolation::Degraded {
                reason: format!("sandbox_setup_failed: {error}"),
            };
            if let Err(audit_error) = record_degraded_execution(
                state,
                &detected_degradation,
                "interactive turn",
                Some(session_id),
                Some(prepared_turn_id),
            )
            .await
            {
                state
                    .storage
                    .update_turn_state(prepared_turn_id, TurnState::Failed)
                    .await?;
                state.active_turns.lock().await.remove(&session_id);
                return Err(audit_error);
            }
            let approval = request_sandbox_degradation_approval(
                state,
                SandboxDegradationApprovalRequest {
                    config: &config,
                    ws_tx: &ws_tx,
                    turn_id: prepared_turn_id,
                    agent_id: &session.agent_id,
                    prompt:
                        "Sandbox setup failed. Approve running this turn without sandbox isolation?"
                            .to_string(),
                    context: serde_json::json!({
                        "degradation": "unsandboxed_execution",
                        "error": error.to_string(),
                    }),
                    cancel_token: &cancel_token,
                },
            )
            .await?;
            match approval {
                SandboxDegradationApproval::Approved => {}
                SandboxDegradationApproval::Rejected => {
                    state
                        .storage
                        .update_turn_state(prepared_turn_id, TurnState::Failed)
                        .await?;
                    let mut turns = state.active_turns.lock().await;
                    turns.remove(&session_id);
                    return Err(GatewayError::WebSocketError(format!(
                        "sandbox setup failed and unsandboxed execution was not approved: {error}"
                    )));
                }
                SandboxDegradationApproval::Interrupted => {
                    state
                        .storage
                        .update_turn_state(prepared_turn_id, TurnState::Interrupted)
                        .await?;
                    let _ = ws_tx.send(ServerEvent::Cancelled {});
                    let mut turns = state.active_turns.lock().await;
                    turns.remove(&session_id);
                    return Ok(());
                }
            }

            let mut unsandboxed_config = config.clone();
            unsandboxed_config.sandbox.allow_degraded_execution = true;
            build_gateway_dispatcher(
                state,
                &unsandboxed_config,
                &agent_config,
                session.metadata.as_ref(),
                Some(space_id),
                session.active_workspace_attachment_id,
                ws_tx.clone(),
            )
            .await
            .map_err(|fallback_error| GatewayError::WebSocketError(fallback_error.to_string()))?
        }
        Err(error) => {
            let isolation = ExecutionIsolation::Degraded {
                reason: format!("sandbox_setup_failed: {error}"),
            };
            let _ = ws_tx.send(ServerEvent::Error {
                code: "sandbox_unavailable".into(),
                message: format!("Sandbox setup failed; the turn was not executed: {error}"),
            });
            state
                .storage
                .update_turn_state(prepared_turn_id, TurnState::Failed)
                .await?;
            state.active_turns.lock().await.remove(&session_id);
            record_degraded_execution(
                state,
                &isolation,
                "unattended turn",
                Some(session_id),
                Some(prepared_turn_id),
            )
            .await?;
            return Err(GatewayError::WebSocketError(format!(
                "sandbox setup failed; execution was blocked: {error}"
            )));
        }
    };
    let execution_context = match attendance {
        TurnAttendance::Attended => "interactive turn",
        TurnAttendance::Unattended => "unattended turn",
    };
    if let Err(error) = record_degraded_execution(
        state,
        &dispatcher_build.isolation,
        execution_context,
        Some(session_id),
        Some(prepared_turn_id),
    )
    .await
    {
        shutdown_sandbox_guard(
            &dispatcher_build.sandbox_guard,
            "failed to shut down sandbox after degradation audit failure",
        )
        .await;
        state
            .storage
            .update_turn_state(prepared_turn_id, TurnState::Failed)
            .await?;
        state.active_turns.lock().await.remove(&session_id);
        return Err(error);
    }
    let sandbox_guard = dispatcher_build.sandbox_guard;
    let dispatcher = dispatcher_build.dispatcher;

    // Step 10: Both runtimes emit AgentEvents into the same projection loop.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let activity_tracker = state.activity_tracker.clone();
    let pricing_config = state.active_config().pricing.clone();
    let model_name = config.default_model_id().to_string();
    let agent_handle = if is_codex_turn {
        let session_approval_grants = state
            .session_approval_grants
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let permissions = build_permission_config(
            &config,
            &state.config,
            Some(space_id),
            &session_approval_grants,
        );
        let codex_request = crate::codex::CodexTurnRequest {
            session_id,
            thread_id,
            turn_id: prepared_turn_id,
            agent_id: session.agent_id.clone(),
            model: model_name.clone(),
            user_message: before_inbound.canonical_content.clone(),
            assembled_context: assembled_messages,
            agent_config,
            permissions,
            dispatcher: Arc::new(dispatcher),
            event_tx,
            cancellation: cancel_token,
        };
        let codex_state = state.clone();
        tokio::spawn(async move { crate::codex::run_turn(codex_state, codex_request).await })
    } else {
        let mut agent_builder = AgentBuilder::new()
            .config(agent_config)
            .provider(state.active_provider())
            .storage(state.storage.clone())
            .tool_dispatcher(dispatcher)
            .approval_registrar(build_approval_registrar(state.approval_channels.clone()))
            .delegation_handler(Arc::new(GatewayDelegationHandler::new(state.clone())))
            .context_transfer_config(config.communication.context_transfer.clone());
        if let Some(hook_registry) = state.hook_registry.clone() {
            agent_builder = agent_builder.hook_registry(hook_registry);
        }
        let agent = match agent_builder.build() {
            Ok(agent) => agent,
            Err(error) => {
                tracing::error!(%error, "failed to build agent for turn");
                let _ = ws_tx.send(ServerEvent::Error {
                    code: "agent_construction_error".to_string(),
                    message: error.to_string(),
                });
                if let Err(update_error) = state
                    .storage
                    .update_turn_state(prepared_turn_id, TurnState::Failed)
                    .await
                {
                    tracing::warn!(
                        turn_id = %prepared_turn_id,
                        error = %update_error,
                        "failed to mark turn failed after agent construction error"
                    );
                }
                shutdown_sandbox_guard(
                    &sandbox_guard,
                    "failed to shut down turn sandbox after agent construction error",
                )
                .await;
                state.active_turns.lock().await.remove(&session_id);
                return Err(GatewayError::WebSocketError(format!(
                    "agent construction failed: {error}"
                )));
            }
        };
        let turn_request = TurnRequest {
            execution: swage_agent::context::TurnExecutionContext {
                session_id,
                thread_id,
                turn_id: prepared_turn_id,
                space_id: Some(space_id),
            },
            user_message: before_inbound.canonical_content.clone(),
            assembled_context: assembled_messages,
            parent_turn_id: None,
            existing_turn_id: Some(prepared_turn_id),
            cancellation_token: cancel_token,
        };
        tokio::spawn(async move { agent.process_turn_streaming(turn_request, event_tx).await })
    };

    // Captures the turn_id emitted by TurnStarted so the agent result match
    // can use it for persistence and logging after the event loop finishes.
    let mut turn_id: Option<Uuid> = None;

    // Step 11: Forward AgentEvent → ServerEvent. The loop runs until the
    // agent drops event_tx (channel closes) or the agent task completes.
    while let Some(event) = event_rx.recv().await {
        match event {
            AgentEvent::TurnStarted {
                turn_id: started_id,
            } => {
                turn_id = Some(started_id);
                // Record the turn_id in the tracing span now that the agent
                // has created the Turn record.
                tracing::Span::current().record("turn_id", started_id.to_string());
                tracing::debug!("processing turn (agent-delegated)");

                // Persist the context snapshot now that we have a turn_id.
                if let Err(e) = state
                    .storage
                    .store_snapshot(started_id, &report_json.to_string())
                    .await
                {
                    tracing::warn!(error = %e, "failed to persist context snapshot");
                }

                // Deliver the ContextAssembled event (subscription-gated).
                let assembled_event = ServerEvent::ContextAssembled {
                    turn_id: started_id,
                    report: report_json.clone(),
                };
                if subscription.lock().await.should_deliver(&assembled_event) {
                    let _ = ws_tx.send(assembled_event);
                }
            }
            AgentEvent::Token { content } => {
                activity_tracker.record_activity();
                let _ = ws_tx.send(ServerEvent::Token { content });
            }
            AgentEvent::ProviderRequestPrepared {
                turn_id: snapshot_turn_id,
                round,
                request,
            } => {
                match redact_provider_visible_request(state, request).await {
                    Ok(request) => {
                        append_provider_visible_request(&mut report_json, round, request);
                    }
                    Err(error) => {
                        // Never persist an unredacted provider request. The
                        // turn may continue, but the snapshot must record why
                        // this round's provider-visible capture is absent.
                        tracing::error!(
                            turn_id = %snapshot_turn_id,
                            round,
                            error = %error,
                            "provider-visible request capture omitted because secret redaction was unavailable"
                        );
                        append_provider_visible_capture_error(&mut report_json, round);
                    }
                }
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        "failed to persist provider-visible context snapshot"
                    );
                }
            }
            AgentEvent::ProviderReasoningCaptured {
                turn_id: snapshot_turn_id,
                round,
                content,
                details,
            } => {
                append_provider_visible_reasoning(&mut report_json, round, content, details);
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        "failed to persist provider reasoning context snapshot"
                    );
                }
            }
            AgentEvent::ToolStart {
                call_id,
                tool_name,
                arguments,
            } => {
                activity_tracker.record_activity();
                let tool_start = ServerEvent::ToolStart {
                    call_id,
                    tool: tool_name,
                    arguments,
                    parent_call_id: None,
                };
                if subscription.lock().await.should_deliver(&tool_start) {
                    let _ = ws_tx.send(tool_start);
                }
            }
            AgentEvent::ToolEnd {
                call_id,
                tool_name,
                result,
                status,
                duration_ms,
            } => {
                let tool_end = ServerEvent::ToolEnd {
                    call_id,
                    tool: tool_name,
                    result,
                    status,
                    duration_ms: duration_ms.unwrap_or(0),
                    truncated: false,
                    total_bytes: None,
                    parent_call_id: None,
                };
                if subscription.lock().await.should_deliver(&tool_end) {
                    let _ = ws_tx.send(tool_end);
                }
            }
            AgentEvent::OutputSchemaRetry {
                turn_id,
                attempt,
                max_retries,
                errors,
            } => {
                // Retry progress is observability noise — only deliver to context subscribers.
                let retry_event = ServerEvent::OutputSchemaRetry {
                    turn_id,
                    attempt,
                    max_retries,
                    errors,
                };
                if subscription.lock().await.should_deliver(&retry_event) {
                    let _ = ws_tx.send(retry_event);
                }
            }
            AgentEvent::ProviderRetryStarted {
                turn_id,
                attempt,
                max_retries,
                provider,
                wait_ms,
            } => {
                let retry_event = ServerEvent::ProviderRetryStarted {
                    turn_id,
                    attempt,
                    max_retries,
                    provider,
                    wait_ms,
                };
                if subscription.lock().await.should_deliver(&retry_event) {
                    let _ = ws_tx.send(retry_event);
                }
            }
            AgentEvent::ProviderRetrySettled {
                turn_id,
                attempt,
                max_retries,
                provider,
                success,
            } => {
                let retry_event = ServerEvent::ProviderRetrySettled {
                    turn_id,
                    attempt,
                    max_retries,
                    provider,
                    success,
                };
                if subscription.lock().await.should_deliver(&retry_event) {
                    let _ = ws_tx.send(retry_event);
                }
            }
            AgentEvent::OutputSchemaFailed {
                turn_id,
                attempts,
                errors,
            } => {
                // Schema exhaustion is an error condition — always deliver regardless
                // of subscription state.
                let _ = ws_tx.send(ServerEvent::OutputSchemaFailed {
                    turn_id,
                    attempts,
                    errors,
                });
            }
            AgentEvent::TurnCompleted {
                turn_id,
                input_tokens,
                output_tokens,
                structured_output,
                ..
            } => {
                // Derive cost from the pricing config when available.
                let cost_microcents =
                    pricing_config.compute_cost(&model_name, input_tokens, output_tokens);

                tracing::info!(
                    input_tokens = input_tokens,
                    output_tokens = output_tokens,
                    cost_microcents = ?cost_microcents,
                    "turn_complete",
                );

                // Update session activity timestamp.
                if let Err(e) = state.storage.update_activity(session_id).await {
                    tracing::warn!(error = %e, "failed to update session activity");
                }

                // Populate usage fields only for context-subscribed connections.
                let (ws_input, ws_output, ws_cost) = if subscription
                    .lock()
                    .await
                    .is_subscribed(crate::subscription::ObservabilityChannel::Context)
                {
                    (Some(input_tokens), Some(output_tokens), cost_microcents)
                } else {
                    (None, None, None)
                };

                let _ = ws_tx.send(ServerEvent::TurnComplete {
                    turn_id,
                    input_tokens: ws_input,
                    output_tokens: ws_output,
                    cost_microcents: ws_cost,
                    structured_output: structured_output.clone(),
                });
            }
            AgentEvent::Error { message } => {
                let _ = ws_tx.send(ServerEvent::Error {
                    code: "agent_error".to_string(),
                    message,
                });
            }
            // Approval lifecycle events — route to the session-scoped ws_tx
            // so only the client connected to this agent sees the event.
            AgentEvent::DelegationStarted {
                target_agent_id,
                task: _,
            } => {
                // Tool-as-delegation started — emit as a delegation event.
                // We use turn_id for parent_turn_id; child_turn_id is unknown
                // at this point (the child turn hasn't been created yet).
                let _ = ws_tx.send(ServerEvent::DelegationStarted {
                    parent_turn_id: turn_id.unwrap_or(prepared_turn_id),
                    child_turn_id: None,
                    target_agent_id,
                });
            }
            AgentEvent::DelegationCompleted {
                target_agent_id,
                success,
            } => {
                let _ = ws_tx.send(ServerEvent::DelegationCompleted {
                    parent_turn_id: turn_id.unwrap_or(prepared_turn_id),
                    child_turn_id: None,
                    target_agent_id,
                    success,
                });
            }
            event @ (AgentEvent::ApprovalRequired { .. }
            | AgentEvent::ApprovalConfirmed { .. }
            | AgentEvent::ApprovalExpired { .. }) => {
                route_approval_agent_event(event, &ws_tx);
            }
        }
    }

    // Wait for the agent task to finish and surface any panic or error.
    match agent_handle.await {
        Ok(Ok(outbound)) => {
            // Agent completed successfully — persist structured_output if the
            // turn produced validated schema output.
            if let Some(ref output) = outbound.structured_output {
                use swage_storage::store::TurnStore;
                if let Err(e) =
                    TurnStore::update_structured_output(&*state.storage, outbound.turn_id, output)
                        .await
                {
                    // Non-fatal: the turn completed; only the persisted record is affected.
                    tracing::warn!(
                        error = %e,
                        turn_id = %outbound.turn_id,
                        "failed to persist structured_output"
                    );
                }
            }
        }
        Ok(Err(AgentError::Cancelled)) => {
            // Cancellation is cooperative — the agent already set
            // TurnState::Interrupted and ThreadState::Idle in storage.
            let _ = ws_tx.send(ServerEvent::Cancelled {});
        }
        Ok(Err(AgentError::OutputSchemaFailed { .. })) => {
            // The agent already emitted AgentEvent::OutputSchemaFailed through
            // the event channel (forwarded above). Log for server visibility but
            // do not send an additional error to the client.
            tracing::info!(turn_id = ?turn_id, "output schema validation exhausted all retries");
        }
        Ok(Err(agent_err)) => {
            // The agent already sent an Error event through the channel
            // for most failure paths. Log for server-side visibility but
            // avoid double-sending to the client — the event loop already
            // forwarded the AgentEvent::Error.
            tracing::error!(error = %agent_err, "agent turn failed");
        }
        Err(join_err) => {
            tracing::error!(error = %join_err, "agent task panicked");
            let _ = ws_tx.send(ServerEvent::Error {
                code: "internal_error".to_string(),
                message: "agent task panicked".to_string(),
            });
        }
    }

    if let Some(guard) = &sandbox_guard
        && let Err(error) = guard.shutdown().await
    {
        tracing::warn!(error = %error, "failed to shut down turn sandbox");
    }

    // Cleanup: remove the cancellation token regardless of outcome.
    {
        let mut turns = state.active_turns.lock().await;
        turns.remove(&session_id);
    }

    Ok(())
}

/// Cancel the active turn for a session.
///
/// Returns an error if there is no active turn to cancel.
pub async fn cancel_turn(state: &AppState, session_id: Uuid) -> Result<(), GatewayError> {
    let turns = state.active_turns.lock().await;
    match turns.get(&session_id) {
        Some(token) => {
            token.cancel();
            Ok(())
        }
        None => Err(GatewayError::WebSocketError(format!(
            "no active turn for session {session_id}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Approval event routing
// ---------------------------------------------------------------------------

/// Forward an approval lifecycle `AgentEvent` to the WebSocket sender for
/// this session.
///
/// Approval events are session-scoped: they go only to the client connected
/// to the agent that raised the gate. Calling `state.broadcast.send` instead
/// would deliver to every connected client — this function deliberately uses
/// the per-turn `ws_tx` channel to preserve that isolation.
///
/// Returns `true` when the event was an approval variant and was forwarded;
/// `false` for all other event types.
pub(crate) fn route_approval_agent_event(
    event: AgentEvent,
    ws_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) -> bool {
    match event {
        AgentEvent::ApprovalRequired {
            approval_id,
            turn_id,
            agent_id,
            kind,
            tool_name,
            tool_arguments,
            hook_name: _, // hook identity is internal — the approval UI doesn't need it
            prompt,
            context,
            timeout_at,
            timeout_policy,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalRequired {
                approval_id,
                turn_id,
                agent_id,
                kind,
                tool_name,
                tool_arguments,
                prompt,
                context,
                timeout_at,
                timeout_policy: timeout_policy.to_string(),
            });
            true
        }
        AgentEvent::ApprovalConfirmed {
            approval_id,
            decision,
            reason,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id,
                decision: decision.to_string(),
                reason,
            });
            true
        }
        AgentEvent::ApprovalExpired {
            approval_id,
            timeout_policy,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalExpired {
                approval_id,
                timeout_policy: timeout_policy.to_string(),
            });
            true
        }
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
