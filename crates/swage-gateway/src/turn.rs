// TODO Audit turn-processing storage fallbacks
// Review fallback paths in this module that substitute default values when
// stored turn, replay, or context data is degraded. Each path should either
// propagate failure, log explicit degraded-state behaviour, or keep the fallback
// with a test explaining why defaulting is safe.
// ---

use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use std::collections::{BTreeMap, HashMap, HashSet};

use swage_core::{
    ApprovalTimeoutPolicy, ChatMessage, ContentPart, GatewayError, Role, ToolCall, ToolResult,
    build_tool_round_messages,
};
use swage_hook::{HookEvent, HookOutcome, HookRegistry};
use swage_protocol::client::turn::MessageAttachment;
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

use crate::record_durability;
use crate::skill::SkillManager;
use crate::tool_executor::{GatewayExecutionStrategy, SandboxedExecutor};

use crate::{state::AppState, subscription::SubscriptionState, websocket::ServerEvent};

mod configuration;
mod context;
mod delegation;
mod dispatcher;
mod history;
mod operator_guidance;
mod workspace;

pub(crate) use configuration::*;
pub(crate) use context::*;
pub(crate) use delegation::*;
pub(crate) use dispatcher::*;
#[cfg(test)]
use history::compact_tool_replay_messages;
use history::rebuild_thread_history_messages;
pub(crate) use operator_guidance::*;
pub(crate) use workspace::*;

pub(crate) use swage_core::DispatchContextComposition;

/// Internal workflow-only witness that an agent reported a provider failure.
pub(crate) type ProviderFailureObserver = Arc<StdMutex<Option<String>>>;

/// What a caller submits for one turn: the message text plus the content
/// parts sent beside it. Every turn entry point takes this; a `String`
/// converts, so a text-only caller passes its text unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnInput {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
}

impl TurnInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    pub fn with_attachments(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
        }
    }
}

impl From<String> for TurnInput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for TurnInput {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

/// The content parts a stored message carries, parsed against the wire
/// contract. Storage keeps attachments as opaque JSON; a value outside the
/// contract is a corrupt record and is refused loudly here, never dropped.
pub(crate) fn message_attachments(
    stored: &[serde_json::Value],
) -> Result<Vec<MessageAttachment>, GatewayError> {
    stored
        .iter()
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| {
                GatewayError::Storage(swage_core::StorageError::Corruption {
                    message: format!("message attachment outside the wire contract: {error}"),
                })
            })
        })
        .collect()
}

/// The attachment values as storage records them: the wire representation
/// verbatim, so the stored row and the client contract never disagree.
pub(crate) fn attachment_records(attachments: &[MessageAttachment]) -> Vec<serde_json::Value> {
    attachments
        .iter()
        .map(|attachment| serde_json::to_value(attachment).expect("attachments serialise"))
        .collect()
}

/// The model-facing content parts for a submitted message: the text first,
/// then one part per attachment in submission order.
pub(crate) fn content_parts(text: String, attachments: &[MessageAttachment]) -> Vec<ContentPart> {
    let mut parts = Vec::with_capacity(1 + attachments.len());
    parts.push(ContentPart::Text { text });
    parts.extend(attachments.iter().map(|attachment| match attachment {
        MessageAttachment::ImageUrl { url } => ContentPart::ImageUrl { url: url.clone() },
    }));
    parts
}

/// Refuse a submission whose attachments the configured provider cannot
/// consume. The check runs before anything is persisted: a part the model
/// would never see must fail the turn, not ride along silently.
pub(crate) fn check_attachment_support(
    capabilities: &crate::capabilities::WorkflowCapabilities,
    attachments: &[MessageAttachment],
) -> Result<(), GatewayError> {
    for attachment in attachments {
        match attachment {
            MessageAttachment::ImageUrl { url } => {
                if url.trim().is_empty() {
                    return Err(GatewayError::ValidationError(
                        "an image attachment needs a non-empty url".into(),
                    ));
                }
                if !capabilities.vision {
                    return Err(GatewayError::ValidationError(
                        "the configured provider does not accept image attachments".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

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

pub(crate) fn append_provider_visible_request(
    report_json: &mut serde_json::Value,
    round: usize,
    request: serde_json::Value,
) {
    provider_visible_array(report_json, "requests").push(serde_json::json!({
        "round": round,
        "request": request,
    }));
}

pub(crate) fn append_provider_visible_capture_error(
    report_json: &mut serde_json::Value,
    round: usize,
) {
    provider_visible_array(report_json, "capture_errors").push(serde_json::json!({
        "round": round,
        "kind": "secret_redaction_unavailable",
    }));
}

pub(crate) fn append_provider_visible_response(
    report_json: &mut serde_json::Value,
    round: usize,
    witness: swage_core::ProviderResponseWitness,
) {
    provider_visible_array(report_json, "responses").push(serde_json::json!({
        "round": round,
        "witness": witness,
    }));
}

pub(crate) async fn redact_provider_visible_request(
    state: &AppState,
    mut request: serde_json::Value,
) -> Result<serde_json::Value, swage_core::StorageError> {
    let secret_values = known_secret_values(state).await?;
    if !secret_values.is_empty() {
        redact_secret_values(&mut request, &secret_values);
    }
    Ok(request)
}

/// Apply a provider diagnostic event to its durable context report. Both the
/// ordinary and delegated turn paths use this seam so request redaction cannot
/// drift from response and reasoning capture.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ProviderSnapshotCapturePath {
    Ordinary,
    Delegated,
}

pub(crate) async fn apply_provider_snapshot_event(
    state: &AppState,
    report_json: &mut serde_json::Value,
    event: AgentEvent,
    capture_path: ProviderSnapshotCapturePath,
) {
    let turn_id = match &event {
        AgentEvent::ProviderRequestPrepared { turn_id, .. }
        | AgentEvent::ProviderResponseCaptured { turn_id, .. }
        | AgentEvent::ProviderReasoningCaptured { turn_id, .. } => *turn_id,
        _ => return,
    };
    match event {
        AgentEvent::ProviderRequestPrepared { round, request, .. } => {
            match redact_provider_visible_request(state, request).await {
                Ok(request) => append_provider_visible_request(report_json, round, request),
                Err(error) => {
                    match capture_path {
                        ProviderSnapshotCapturePath::Ordinary => tracing::error!(
                            turn_id = %turn_id,
                            round,
                            error = %error,
                            "provider-visible request capture omitted because secret redaction was unavailable"
                        ),
                        ProviderSnapshotCapturePath::Delegated => tracing::error!(
                            turn_id = %turn_id,
                            round,
                            error = %error,
                            "delegated provider-visible request capture omitted because secret redaction was unavailable"
                        ),
                    }
                    append_provider_visible_capture_error(report_json, round);
                }
            }
        }
        AgentEvent::ProviderResponseCaptured { round, witness, .. } => {
            append_provider_visible_response(report_json, round, witness);
        }
        AgentEvent::ProviderReasoningCaptured {
            round,
            content,
            details,
            ..
        } => {
            append_provider_visible_reasoning(report_json, round, content, details);
        }
        _ => unreachable!("non-provider event returned above"),
    }
}

async fn known_secret_values(state: &AppState) -> Result<Vec<String>, swage_core::StorageError> {
    state
        .secret_manager
        .known_values()
        .await
        .map_err(|error| match error {
            GatewayError::Storage(storage) => storage,
            other => swage_core::StorageError::Corruption {
                message: format!("secret redaction set could not be read: {other}"),
            },
        })
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

pub(crate) fn append_provider_visible_reasoning(
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

fn normalise_report_object(report_json: &mut serde_json::Value) {
    if !report_json.is_object() {
        let previous = std::mem::replace(report_json, serde_json::json!({}));
        report_json["assembly_report"] = previous;
    }
}

fn provider_visible_array<'a>(
    report_json: &'a mut serde_json::Value,
    key: &str,
) -> &'a mut Vec<serde_json::Value> {
    normalise_report_object(report_json);

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

/// The effort control the effective model accepts: the catalogue's own
/// declaration when the entry carries one, otherwise the provider adapter's
/// account of the documented control, otherwise no control at all.
pub(crate) fn effort_capability_for_model(
    config: &swage_config::ResolvedConfig,
    catalogue_entry: &str,
    provider: &dyn swage_provider::LlmProvider,
) -> swage_core::EffortCapability {
    let model = config.models.entries.get(catalogue_entry);
    model
        .and_then(|model| model.capabilities.effort.clone())
        .or_else(|| {
            provider.effort_capability(model.map_or(catalogue_entry, |model| &model.model_id))
        })
        .unwrap_or(swage_core::EffortCapability::None)
}

/// Record the resolved effort beside the effective model in the turn's
/// provider-visible snapshot: what was asked, what is sent, and any clamp.
fn record_effort_snapshot(
    report_json: &mut serde_json::Value,
    resolved: &swage_core::ResolvedEffort,
) {
    normalise_report_object(report_json);
    if !report_json["provider_visible"].is_object() {
        report_json["provider_visible"] = serde_json::json!({});
    }
    report_json["provider_visible"]["effort"] =
        serde_json::to_value(resolved).expect("resolved effort serialises");
}

fn record_effective_model_snapshot(
    report_json: &mut serde_json::Value,
    catalogue_entry: &str,
    model: &swage_config::ModelEndpointConfig,
    source: &str,
) {
    normalise_report_object(report_json);

    if !report_json["provider_visible"].is_object() {
        report_json["provider_visible"] = serde_json::json!({});
    }
    report_json["provider_visible"]["effective_model"] = serde_json::json!({
        "catalogue_entry": catalogue_entry,
        "model_id": model.model_id,
        "provider_id": model.provider_id,
        "source": source,
    });
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

/// Render a completed delegation's Result message as thread content.
///
/// Reads the payload the delegation path writes — `response` with optional
/// `structured_output`, or `error` for a failed child — and falls back to
/// the raw JSON for any other shape, so an unrecognised payload is still
/// delivered rather than dropped.
fn render_delegation_result(result: &swage_storage::model::AgentMessage) -> String {
    let sender = &result.sender_agent_id;
    let mut text = match result.payload.get("response").and_then(|v| v.as_str()) {
        Some(response) => format!("Delegated agent {sender} completed:\n{response}"),
        None => match result.payload.get("error").and_then(|v| v.as_str()) {
            Some(error) => format!("Delegated agent {sender} failed: {error}"),
            None => format!(
                "Delegated agent {sender} finished:\n{}",
                serde_json::to_string_pretty(&result.payload)
                    .unwrap_or_else(|_| result.payload.to_string())
            ),
        },
    };
    if let Some(structured) = result.payload.get("structured_output") {
        let rendered =
            serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string());
        text.push_str("\n\nStructured output:\n```json\n");
        text.push_str(&rendered);
        text.push_str("\n```");
    }
    text
}

async fn release_active_turn(state: &AppState, session_id: Uuid) {
    state.active_turns.lock().await.remove(&session_id);
}

async fn release_active_turn_on_error<T, E>(
    state: &AppState,
    session_id: Uuid,
    result: Result<T, E>,
) -> Result<T, E> {
    if result.is_err() {
        release_active_turn(state, session_id).await;
    }
    result
}

async fn record_terminal_state_and_release(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    turn_state: TurnState,
) -> Result<(), GatewayError> {
    release_active_turn_on_error(
        state,
        session_id,
        state.storage.update_turn_state(turn_id, turn_state).await,
    )
    .await?;
    release_active_turn(state, session_id).await;
    Ok(())
}

async fn record_failed_turn_and_release_on_error<T>(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    result: Result<T, GatewayError>,
) -> Result<T, GatewayError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            record_terminal_state_and_release(state, session_id, turn_id, TurnState::Failed)
                .await?;
            Err(error)
        }
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
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        None,
        None,
        None,
        None,
        TurnAttendance::Attended,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_existing_turn(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_existing_turn_and_focus(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    compaction_focus: Option<String>,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn_and_focus_and_model(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        compaction_focus,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_existing_turn_and_focus_and_model(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    compaction_focus: Option<String>,
    requested_model: Option<String>,
) -> Result<(), GatewayError> {
    process_turn_with_composition_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        DispatchContextComposition::default(),
        None,
        compaction_focus,
        requested_model,
        None,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_composition(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    composition: DispatchContextComposition,
    agent_call_scope: Option<Uuid>,
) -> Result<(), GatewayError> {
    process_turn_with_composition_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        composition,
        agent_call_scope,
        None,
        None,
        None,
    )
    .await
}

/// Process a composed turn with a call-scoped model selection. Workflow calls
/// use this to make their journalled model choice explicit without changing
/// the session's durable selection for an ordinary one-off turn.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_composition_and_model(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    composition: DispatchContextComposition,
    agent_call_scope: Option<Uuid>,
    requested_model: Option<String>,
    provider_failure_observer: Option<ProviderFailureObserver>,
) -> Result<(), GatewayError> {
    process_turn_with_composition_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        composition,
        agent_call_scope,
        None,
        requested_model,
        provider_failure_observer,
    )
    .await
}

#[tracing::instrument(
    name = "turn",
    skip(state, input, ws_tx, subscription, composition, compaction_focus),
    fields(turn_id = tracing::field::Empty),
)]
#[allow(clippy::too_many_arguments)]
async fn process_turn_with_composition_and_focus(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: TurnInput,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    composition: DispatchContextComposition,
    agent_call_scope: Option<Uuid>,
    compaction_focus: Option<String>,
    requested_model: Option<String>,
    provider_failure_observer: Option<ProviderFailureObserver>,
) -> Result<(), GatewayError> {
    let TurnInput {
        text: content,
        attachments,
    } = input;
    // An attachment the provider cannot consume fails the turn before any
    // record is written — a part the model never sees must not ride along.
    check_attachment_support(&state.workflow_capabilities, &attachments)?;

    // Step 1: Resolve the space and target thread for this session.
    let mut session = state
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

    // A continued workflow worker reuses its durable session, but this turn
    // is a distinct journalled agent call. Refresh the capture retry identity
    // only after the active-turn guard so a refused concurrent continuation
    // cannot change the scope underneath the turn that is already running.
    if let Some(agent_call_scope) = agent_call_scope {
        let metadata = session
            .metadata
            .get_or_insert_with(|| serde_json::json!({}));
        metadata[AGENT_CALL_SCOPE_METADATA_KEY] =
            serde_json::Value::String(agent_call_scope.to_string());
        session = state
            .storage
            .set_session_metadata(session.id, session.metadata.clone())
            .await?;
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

    // Placement identity is durable, but filesystem authority is not. Check
    // the recorded authority root against today's attachment/location grants
    // and replace the cached write bit before assembly or sandbox creation.
    reauthorise_worker_placement_for_turn(state, &mut session).await?;

    // Load space-specific config. Missing space config falls back to the
    // startup config, but present invalid config fails the turn before any
    // user message is written.
    let config = load_session_config(state, &space_slug, &session.agent_id)?;

    // A durable agent definition, when one exists for this agent, gives the
    // session its character on top of the resolved file config: instructions
    // append, the model preference fills an unselected default, and the
    // permission overlay narrows grants. Resolution failure falls open to
    // the file config — a broken definition must not brick the session.
    let config = match crate::service::resolve_effective_agent_definition(
        state.storage.as_ref(),
        &session.agent_id,
        space_id,
    )
    .await
    {
        Ok(Some(definition)) => apply_agent_definition(config, &definition.definition),
        Ok(None) => config,
        Err(error) => {
            tracing::warn!(
                agent_id = %session.agent_id,
                %error,
                "failed to resolve the agent definition; using file config alone"
            );
            config
        }
    };

    // A worker session carries the full lineage of dispatching contexts in
    // session metadata: the worker's grants are the intersection of its own
    // and every inherited bound, re-resolved on every turn so nested dispatch
    // cannot expand authority and suspension cannot freeze it.
    // Unlike the definition overlay above, resolution failure here fails the
    // turn: falling open would run the worker wider than its dispatcher.
    let bound_agent_ids = dispatch_bound_agent_ids(session.metadata.as_ref())?;
    let config = apply_dispatch_bounds(
        state,
        &space_slug,
        space_id,
        config,
        &bound_agent_ids,
        &format!("worker session {session_id}"),
    )
    .await?;
    let bound_task_definition_ids = dispatch_bound_task_definition_ids(session.metadata.as_ref())?;
    let config = apply_dispatch_task_bounds(
        state,
        config,
        &bound_task_definition_ids,
        &format!("worker session {session_id}"),
    )
    .await?;
    let config = apply_workflow_call_tools(config, session.metadata.as_ref())?;

    // Task names ride child-session metadata, while the definition is
    // resolved afresh for every execution. Missing or unreadable tasks fail
    // closed: silently running a different contract would be false success.
    let (config, task_defines_output_retries) = match session
        .metadata
        .as_ref()
        .and_then(|m| m.get("task_definition"))
        .and_then(serde_json::Value::as_str)
    {
        Some(task_id) => {
            let definition =
                crate::service::resolve_task_definition(state.storage.as_ref(), task_id).await?;
            let decision = crate::experimentation::select_variant(
                &config.experimentation,
                crate::experimentation::LOCATION_TASK_PROMPT,
                &[],
                "definition",
                session_id,
            )
            .map_err(|e| GatewayError::ConfigError(e.to_string()))?;
            if let Err(error) = crate::experimentation::record_selection(
                state,
                crate::experimentation::LOCATION_TASK_PROMPT,
                &decision,
                Some(session_id),
                None,
            )
            .await
            {
                tracing::warn!(%error, task_id, "task prompt variant record unavailable; serving the definition default");
            }
            let defines_output_retries = definition.output_max_retries.is_some();
            (
                apply_task_definition(config, &definition)?,
                defines_output_retries,
            )
        }
        None => (config, false),
    };

    let selected_model_id = requested_model
        .as_deref()
        .or(session.selected_model_id.as_deref());
    if let Some(requested_model) = requested_model.as_deref() {
        resolve_requested_turn_model(&config, requested_model)?;
    }
    ensure_selected_model_available(state, &config, selected_model_id, session_id)?;
    let effective_model_source = match requested_model.as_deref() {
        Some(_) => "request",
        None if session
            .selected_model_id
            .as_deref()
            .filter(|model_id| !model_id.trim().is_empty())
            .is_some_and(|model_id| {
                resolve_model_catalogue_entry(&config.models, model_id).is_some()
            }) =>
        {
            "conversation"
        }
        None => "configured_default",
    };
    let config = effective_session_config(config, selected_model_id);

    // Skill discovery, space memory loading, and assembly search paths all use
    // the root retained when this application state was constructed.
    let swage_root = resolve_swage_root(state);

    let active_skills = load_active_skills_for_agent(
        state,
        session_id,
        &space_slug,
        &session.agent_id,
        Some(swage_root),
        true,
        &config.declared_skills,
        composition.skills.as_deref().unwrap_or_default(),
    )
    .await
    .map_err(GatewayError::ConfigError)?;

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
    let cancel_token = cancellation_override.unwrap_or_default();
    {
        let mut turns = state.active_turns.lock().await;
        turns.insert(session_id, cancel_token.clone());
    }

    // Step 6: Canonicalise inbound content before persisting any thread message.
    let before_inbound = match dispatch_before_inbound(state.hook_registry.as_ref(), &content).await
    {
        Ok(result) => result,
        Err(error) => {
            record_terminal_state_and_release(
                state,
                session_id,
                prepared_turn_id,
                TurnState::Failed,
            )
            .await?;
            return Err(error);
        }
    };

    if !before_inbound.modified_by_hooks.is_empty() {
        record_failed_turn_and_release_on_error(
            state,
            session_id,
            prepared_turn_id,
            state
                .storage
                .update_inbound_audit(
                    prepared_turn_id,
                    &InboundAudit {
                        canonical_input: before_inbound.canonical_content.clone(),
                        modified_by_hooks: before_inbound.modified_by_hooks.clone(),
                    },
                )
                .await
                .map_err(GatewayError::from),
        )
        .await?;
    }

    record_failed_turn_and_release_on_error(
        state,
        session_id,
        prepared_turn_id,
        state
            .storage
            .append_message(AppendMessage {
                thread_id,
                attachments: attachment_records(&attachments),
                turn_id: Some(prepared_turn_id),
                role: Role::User,
                content: before_inbound.canonical_content.clone(),
                agent_id: session.agent_id.clone(),
            })
            .await
            .map_err(GatewayError::from),
    )
    .await?;

    // Step 7: Deliver completed detached delegations. A detached child's
    // Result waits in this agent's mailbox until its parent next turns;
    // each is appended as a durable User-role message *before* history is
    // rebuilt, so the result enters the thread record — visible in the
    // UI, part of the audit trail, and carried to the model as ordinary
    // history content, never as instructions. User role, deliberately:
    // the history rebuild carries Tool-role rows only when they answer a
    // recorded tool call (providers reject orphaned tool results), so a
    // platform-delivered notification rides the same role a channel or
    // scheduled message would, with the rendering naming its real sender.
    // Append-before-claim: a failure between the two re-delivers next
    // turn rather than losing the result, and a lost claim race leaves a
    // duplicate row, not a gap. A storage failure here fails the turn —
    // with the same explicit cleanup as the surrounding error paths, so
    // the thread is not left guarded by a stale Pending turn.
    let delivery_result: Result<(), GatewayError> = async {
        let pending_results =
            swage_agent::mailbox::pending_results(&state.storage, &session.agent_id)
                .await
                .map_err(|error| GatewayError::WebSocketError(error.to_string()))?;
        for result in &pending_results {
            state
                .storage
                .append_message(AppendMessage {
                    thread_id,
                    attachments: Vec::new(),
                    turn_id: Some(prepared_turn_id),
                    role: Role::User,
                    content: render_delegation_result(result),
                    agent_id: session.agent_id.clone(),
                })
                .await?;
            swage_agent::mailbox::claim_result(&state.storage, result.id)
                .await
                .map_err(|error| GatewayError::WebSocketError(error.to_string()))?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = delivery_result {
        record_terminal_state_and_release(state, session_id, prepared_turn_id, TurnState::Failed)
            .await?;
        return Err(error);
    }

    // Steps 8–9: Assemble context via the swage-context assembly engine.
    // Loads thread history and constructs the typed elements used by the
    // selected assembly.
    // Runs fail-open assembly: custom JavaScript configs when configured, built-in
    // default otherwise. The LLM provider is passed for two-phase compaction.
    let history_messages = record_failed_turn_and_release_on_error(
        state,
        session_id,
        prepared_turn_id,
        rebuild_thread_history_messages(state.storage.as_ref(), thread_id).await,
    )
    .await?;

    let context_choices = match select_context_assembly(
        state,
        &config,
        session_id,
        prepared_turn_id,
        composition.assembly.as_deref(),
    )
    .await
    {
        Ok(context_choices) => context_choices,
        Err(error) => {
            record_terminal_state_and_release(
                state,
                session_id,
                prepared_turn_id,
                TurnState::Failed,
            )
            .await?;
            return Err(error);
        }
    };
    let assembly_name = context_choices.assembly_name;
    let memory_content = load_memory_for_assembly(&assembly_name, || async {
        load_space_memory(swage_root, &space_slug).await
    })
    .await;
    let mut session_variables: std::collections::BTreeMap<String, String> =
        record_failed_turn_and_release_on_error(
            state,
            session_id,
            prepared_turn_id,
            state
                .storage
                .list_session_variables(session_id)
                .await
                .map_err(GatewayError::from),
        )
        .await?
        .into_iter()
        .map(|variable| (variable.name, variable.value))
        .collect();
    session_variables.extend(composition.variables.clone().unwrap_or_default());

    // The workspace root doubles as the session's working directory in the
    // orientation baseline, so it is resolved before elements are built.
    let workspace_policy = load_trusted_workspace_policy(
        &state.storage,
        space_id,
        session.active_workspace_attachment_id,
    )
    .await;
    let (workspace_root, mut allowed_paths) = if let Some(path) = session
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("worker_placement_cwd"))
        .and_then(serde_json::Value::as_str)
    {
        let path = std::path::PathBuf::from(path);
        (path.clone(), vec![path])
    } else {
        if attendance == TurnAttendance::Unattended
            && workspace_policy.active_workspace.is_none()
            && !state.config_root.is_absolute()
        {
            record_terminal_state_and_release(
                state,
                session_id,
                prepared_turn_id,
                TurnState::Failed,
            )
            .await?;
            return Err(GatewayError::ConfigError(format!(
                "unattended turn has no deliberate tool root: attach a workspace or configure an absolute managed scratch root (current root: {})",
                state.config_root.display()
            )));
        }
        agent_workspace_from_policy(&state.config_root, &workspace_policy, space_id, session.id)
    };

    let environment =
        build_session_environment(&space, Some(workspace_root.display().to_string()), &session);
    let available_skills =
        available_skills_for_agent(&space_slug, &session.agent_id, Some(swage_root));

    let operator_guidance = operator_guidance_rules(Some(swage_root), &workspace_policy);
    let hierarchy = configuration_hierarchy(swage_root, &space_slug, Some(&session.agent_id));

    let elements = build_assembly_elements(
        &config.instructions,
        operator_guidance,
        active_skills,
        history_messages,
        memory_content.map(|content| (content, space.name.clone())),
        environment,
        available_skills,
        config.default_model_id(),
        context_choices.working_norms_scaffold,
        &hierarchy,
        composition.elements.as_deref(),
    );
    let tokeniser = TiktokenTokeniser::new(config.default_model_id());

    // Assemble context: a configured assembly name selects an operator's JS
    // assembly from the config hierarchy; "default" (or empty) runs the
    // built-in implementation. JS assemblies fail open to the default with
    // the error recorded in the report.
    let output = execute_context_assembly(
        &assembly_name,
        &config,
        &elements,
        &tokeniser,
        Some(swage_root),
        &space_slug,
        &session.agent_id,
        &session_variables,
    );
    let (assembled_messages, mut assembly_report) = (output.messages, output.report);
    assembly_report.tool_scope = Some(serde_json::json!({
        "effective_root": workspace_root,
    }));

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
    if let Some((effective_model_id, effective_model)) =
        resolve_model_catalogue_entry(&config.models, &config.models.default)
    {
        record_effective_model_snapshot(
            &mut report_json,
            effective_model_id,
            effective_model,
            effective_model_source,
        );
    }
    let session_approval_grants = state
        .session_approval_grants
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    append_approved_location_paths(
        &mut allowed_paths,
        &session_approval_grants,
        space_id,
        session_id,
        session.metadata.as_ref(),
    );
    let mut agent_config = build_agent_config_with_workspace(
        session.agent_id.clone(),
        &config,
        output_schema_override,
        &workspace_root,
        allowed_paths,
    );
    if !task_defines_output_retries && let Some(max_retries) = max_output_retries_override {
        agent_config.max_output_retries = max_retries;
    }
    agent_config.approval_prompts_enabled = attendance == TurnAttendance::Attended;

    // Step 9: Resolve tool permissions from config cascade and build the
    // ToolDispatcher with the resolved permission set. When sandboxing is
    // enabled, this also spawns the per-turn sandbox and returns its cleanup
    // guard to this turn scope.
    let is_codex_turn = config.default_provider_protocol() == swage_config::Protocol::Codex;
    let execution_context = match attendance {
        TurnAttendance::Attended => "interactive turn",
        TurnAttendance::Unattended => "unattended turn",
    };
    let dispatcher_build = match build_gateway_dispatcher(
        state,
        GatewayDispatcherRequest {
            config: &config,
            agent_config: &agent_config,
            session_metadata: session.metadata.as_ref(),
            space_id: Some(space_id),
            active_workspace_attachment_id: session.active_workspace_attachment_id,
            ws_tx: ws_tx.clone(),
            session_id: Some(session_id),
            turn_id: Some(prepared_turn_id),
        },
    )
    .await
    {
        Ok(build) => build,
        Err(error) if config.sandbox.enabled && attendance == TurnAttendance::Attended => {
            let detected_degradation = ExecutionIsolation::Degraded {
                reason: format!("sandbox_setup_failed: {error}"),
            };
            if let Err(audit_error) = record_degraded_execution(
                state,
                &detected_degradation,
                execution_context,
                Some(session_id),
                Some(prepared_turn_id),
            )
            .await
            {
                record_terminal_state_and_release(
                    state,
                    session_id,
                    prepared_turn_id,
                    TurnState::Failed,
                )
                .await?;
                return Err(audit_error);
            }
            let approval = record_failed_turn_and_release_on_error(
                state,
                session_id,
                prepared_turn_id,
                request_sandbox_degradation_approval(
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
                .await,
            )
            .await?;
            match approval {
                SandboxDegradationApproval::Approved => {
                    // The approval wait temporarily marks the placeholder as
                    // Processing. Agent::process_turn_streaming takes over
                    // existing placeholders only while they are Pending.
                    record_failed_turn_and_release_on_error(
                        state,
                        session_id,
                        prepared_turn_id,
                        state
                            .storage
                            .update_turn_state(prepared_turn_id, TurnState::Pending)
                            .await
                            .map_err(GatewayError::from),
                    )
                    .await?;
                }
                SandboxDegradationApproval::Rejected => {
                    record_terminal_state_and_release(
                        state,
                        session_id,
                        prepared_turn_id,
                        TurnState::Failed,
                    )
                    .await?;
                    return Err(GatewayError::WebSocketError(format!(
                        "sandbox setup failed and unsandboxed execution was not approved: {error}"
                    )));
                }
                SandboxDegradationApproval::Interrupted => {
                    record_failed_turn_and_release_on_error(
                        state,
                        session_id,
                        prepared_turn_id,
                        state
                            .storage
                            .update_turn_state(prepared_turn_id, TurnState::Interrupted)
                            .await
                            .map_err(GatewayError::from),
                    )
                    .await?;
                    let _ = ws_tx.send(ServerEvent::Cancelled {});
                    // Preserve the client-visible interrupt ordering: the old turn
                    // remains active until its cancellation event has been sent.
                    release_active_turn(state, session_id).await;
                    return Ok(());
                }
            }

            let mut unsandboxed_config = config.clone();
            unsandboxed_config.sandbox.allow_degraded_execution = true;
            record_failed_turn_and_release_on_error(
                state,
                session_id,
                prepared_turn_id,
                build_gateway_dispatcher(
                    state,
                    GatewayDispatcherRequest {
                        config: &unsandboxed_config,
                        agent_config: &agent_config,
                        session_metadata: session.metadata.as_ref(),
                        space_id: Some(space_id),
                        active_workspace_attachment_id: session.active_workspace_attachment_id,
                        ws_tx: ws_tx.clone(),
                        session_id: Some(session_id),
                        turn_id: Some(prepared_turn_id),
                    },
                )
                .await
                .map_err(|fallback_error| GatewayError::WebSocketError(fallback_error.to_string())),
            )
            .await?
        }
        Err(error) => {
            let isolation = ExecutionIsolation::Degraded {
                reason: format!("sandbox_setup_failed: {error}"),
            };
            let _ = ws_tx.send(ServerEvent::Error {
                code: "sandbox_unavailable".into(),
                message: format!("Sandbox setup failed; the turn was not executed: {error}"),
            });
            record_terminal_state_and_release(
                state,
                session_id,
                prepared_turn_id,
                TurnState::Failed,
            )
            .await?;
            record_degraded_execution(
                state,
                &isolation,
                execution_context,
                Some(session_id),
                Some(prepared_turn_id),
            )
            .await?;
            return Err(GatewayError::WebSocketError(format!(
                "sandbox setup failed; execution was blocked: {error}"
            )));
        }
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
        record_terminal_state_and_release(state, session_id, prepared_turn_id, TurnState::Failed)
            .await?;
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
            Some(session_id),
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
        let requested_effort = match effort_override {
            Some(effort) => Some(effort),
            None => session
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("effort"))
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| {
                    GatewayError::ConfigError(format!("session effort is invalid: {error}"))
                })?
                .or_else(|| {
                    match selected_model_id.filter(|model_id| !model_id.trim().is_empty()) {
                        Some(model_id) => resolve_model_catalogue_entry(&config.models, model_id)
                            .and_then(|(_, model)| model.default_effort),
                        None => config
                            .models
                            .entries
                            .get(&config.models.default)
                            .and_then(|model| model.default_effort),
                    }
                }),
        };
        // The intent is resolved once, here, against the effective model's
        // declared capability; adapters express the resolved control. The
        // outcome is written into the turn's audit snapshot so a clamp or an
        // unsupported `off` is reconstructable from the record.
        let effort = requested_effort.map(|requested| {
            let capability = effort_capability_for_model(
                &config,
                &config.models.default,
                state.active_provider().as_ref(),
            );
            swage_core::resolve_effort(requested, &capability)
        });
        if let Some(resolved) = effort.as_ref() {
            record_effort_snapshot(&mut report_json, resolved);
        }
        let turn_request = TurnRequest {
            execution: swage_agent::context::TurnExecutionContext {
                session_id,
                thread_id,
                turn_id: prepared_turn_id,
                space_id: Some(space_id),
            },
            user_message: before_inbound.canonical_content.clone(),
            assembled_context: assembled_messages,
            compaction_focus,
            parent_turn_id: None,
            existing_turn_id: Some(prepared_turn_id),
            effort,
            cancellation_token: cancel_token,
        };
        tokio::spawn(async move { agent.process_turn_streaming(turn_request, event_tx).await })
    };

    // Captures the turn_id emitted by TurnStarted so the agent result match
    // can use it for persistence and logging after the event loop finishes.
    let mut turn_id: Option<Uuid> = None;

    // Set when the durability contract's escalation fires: a constitutive
    // record was lost, or a reconstructive loss could not even be recorded
    // as a degradation (record_durability.rs). Checked after the agent
    // settles — the turn is then failed loudly rather than left looking
    // auditable.
    let mut record_integrity_lost: Option<String> = None;

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
                    // Reconstructive record (record_durability.rs): the loss
                    // itself is recorded; only a double failure escalates.
                    tracing::warn!(error = %e, "failed to persist context snapshot");
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        started_id,
                        "context_snapshot",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "context snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }

                // Deliver the ContextAssembled event (subscription-gated).
                let assembled_event = ServerEvent::ContextAssembled {
                    turn_id: started_id,
                    report: assembly_report.clone(),
                };
                if subscription.lock().await.should_deliver(&assembled_event) {
                    let _ = ws_tx.send(assembled_event);
                }
            }
            AgentEvent::CompactionApplied {
                turn_id: snapshot_turn_id,
                record,
            } => {
                if let Some(records) = report_json
                    .get_mut("compaction_applied")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    records.push(
                        serde_json::to_value(record).unwrap_or_else(
                            |error| serde_json::json!({ "error": error.to_string() }),
                        ),
                    );
                }
                if let Err(error) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    tracing::warn!(%error, "failed to persist compaction record");
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "compaction_snapshot",
                        &error,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "compaction snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
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
                apply_provider_snapshot_event(
                    state,
                    &mut report_json,
                    AgentEvent::ProviderRequestPrepared {
                        turn_id: snapshot_turn_id,
                        round,
                        request,
                    },
                    ProviderSnapshotCapturePath::Ordinary,
                )
                .await;
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    // Reconstructive record (record_durability.rs).
                    tracing::warn!(
                        error = %e,
                        "failed to persist provider-visible context snapshot"
                    );
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "provider_visible_snapshot",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "provider-visible snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }
            }
            AgentEvent::ProviderReasoningCaptured {
                turn_id: snapshot_turn_id,
                round,
                content,
                details,
            } => {
                apply_provider_snapshot_event(
                    state,
                    &mut report_json,
                    AgentEvent::ProviderReasoningCaptured {
                        turn_id: snapshot_turn_id,
                        round,
                        content,
                        details,
                    },
                    ProviderSnapshotCapturePath::Ordinary,
                )
                .await;
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    // Reconstructive record (record_durability.rs).
                    tracing::warn!(
                        error = %e,
                        "failed to persist provider reasoning context snapshot"
                    );
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "provider_reasoning_snapshot",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "provider reasoning snapshot lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }
            }
            AgentEvent::ProviderResponseCaptured {
                turn_id: snapshot_turn_id,
                round,
                witness,
            } => {
                apply_provider_snapshot_event(
                    state,
                    &mut report_json,
                    AgentEvent::ProviderResponseCaptured {
                        turn_id: snapshot_turn_id,
                        round,
                        witness,
                    },
                    ProviderSnapshotCapturePath::Ordinary,
                )
                .await;
                if let Err(e) = state
                    .storage
                    .store_snapshot(snapshot_turn_id, &report_json.to_string())
                    .await
                {
                    tracing::warn!(error = %e, "failed to persist provider response witness");
                    if let Err(audit_error) = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        snapshot_turn_id,
                        "provider_response_witness",
                        &e,
                    )
                    .await
                    {
                        record_integrity_lost.get_or_insert(format!(
                            "provider response witness lost and the loss could not be recorded: {audit_error}"
                        ));
                    }
                }
            }
            AgentEvent::ToolStart {
                call_id,
                tool_name,
                arguments,
                display_summary,
            } => {
                activity_tracker.record_activity();
                let tool_start = ServerEvent::ToolStart {
                    call_id,
                    tool: tool_name,
                    arguments,
                    parent_call_id: None,
                    summary: display_summary,
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

                // Update session activity timestamp. Derivable record
                // (record_durability.rs): activity is reconstructable from
                // turn and message timestamps, so losing this write is
                // accepted with a warning.
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
            AgentEvent::Error {
                message,
                provider_failure,
            } => {
                if provider_failure {
                    if let Some(observer) = provider_failure_observer.as_ref() {
                        *observer.lock().unwrap_or_else(|error| error.into_inner()) =
                            Some(message.clone());
                    }
                }
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
                    // Constitutive record (record_durability.rs): validated
                    // output that reached the client but not the record
                    // makes the trail lie by omission, so the turn fails
                    // loudly below. The degradation event is still
                    // attempted so the trail names the gap even when the
                    // turn row's failure stamp is all that survives.
                    tracing::error!(
                        error = %e,
                        turn_id = %outbound.turn_id,
                        "failed to persist structured_output"
                    );
                    let _ = record_durability::record_reconstruction_gap(
                        state.storage.as_ref(),
                        Some(session_id),
                        outbound.turn_id,
                        "structured_output",
                        &e,
                    )
                    .await;
                    record_integrity_lost
                        .get_or_insert(format!("structured output could not be persisted: {e}"));
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

    // The durability contract's escalation (record_durability.rs): a turn
    // whose record is known-incomplete must not end looking auditable.
    // force_turn_failure, not update_turn_state: the agent has usually
    // already marked the turn Completed, and the ordinary write's
    // first-terminal-wins guard would silently no-op — this transition is
    // the guard's one sanctioned override. The state write may itself fail
    // on the same broken store — then the log and the client error are the
    // loudest surfaces left.
    if let Some(reason) = record_integrity_lost {
        tracing::error!(turn_id = ?turn_id, %reason, "turn record integrity lost");
        if let Err(error) = state
            .storage
            .force_turn_failure(turn_id.unwrap_or(prepared_turn_id))
            .await
        {
            tracing::error!(error = %error, "failed to mark record-degraded turn as failed");
        }
        let _ = ws_tx.send(ServerEvent::Error {
            code: "turn_record_lost".to_string(),
            message: format!("The turn's audit record is incomplete: {reason}"),
        });
    }

    if let Some(guard) = &sandbox_guard
        && let Err(error) = guard.shutdown().await
    {
        tracing::warn!(error = %error, "failed to shut down turn sandbox");
    }

    // Cleanup: remove the cancellation token regardless of outcome.
    release_active_turn(state, session_id).await;

    Ok(())
}

pub(super) struct ContextChoices {
    assembly_name: String,
    working_norms_scaffold: swage_context::working_norms::WorkingNormsScaffold,
}

const WORKING_NORMS_AUTHORED_V1: &str = "authored-v1";
const WORKING_NORMS_FAMILY_FALLBACK: &str = "family-fallback";

async fn select_context_assembly(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    session_id: Uuid,
    turn_id: Uuid,
    call_override: Option<&str>,
) -> Result<ContextChoices, GatewayError> {
    if let Some(call_override) = call_override {
        swage_core::validate_context_assembly_name(call_override)
            .map_err(GatewayError::ConfigError)?;
    }
    // The assembly name is an experiment location: when variants are
    // registered for it, the selection loop chooses which serves this turn
    // (honouring the kill switch and forced pins), and every decision —
    // including "experimentation is off" — lands in the audit trail. The
    // report and every model-facing surface stay function-named; the audit
    // record is the only place the variant id appears.
    let configured_assembly = call_override
        .unwrap_or(&config.context.assembly)
        .trim()
        .to_owned();
    let assembly_variants = config
        .context
        .alternatives
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let decision = crate::experimentation::select_variant(
        &config.experimentation,
        crate::experimentation::LOCATION_CONTEXT_ASSEMBLY,
        &assembly_variants,
        &configured_assembly,
        turn_id,
    )
    .map_err(|error| GatewayError::ConfigError(error.to_string()))?;
    let decision = match crate::experimentation::record_selection(
        state,
        crate::experimentation::LOCATION_CONTEXT_ASSEMBLY,
        &decision,
        Some(session_id),
        Some(turn_id),
    )
    .await
    {
        Ok(()) => decision,
        Err(error) => {
            tracing::error!(
                %error,
                variant = %decision.variant,
                "variant selection could not be recorded; serving the configured default"
            );
            crate::experimentation::VariantDecision {
                variant: configured_assembly.clone(),
                reason: crate::experimentation::SelectionReason::RecordUnavailable,
            }
        }
    };

    // The working-norms text is its own experiment location, drawn beside
    // the assembly choice. The exact-model native scaffold is the default;
    // the established family text remains a registered alternative.
    let norms_variants = [WORKING_NORMS_FAMILY_FALLBACK];
    let norms_decision = crate::experimentation::select_variant(
        &config.experimentation,
        crate::experimentation::LOCATION_WORKING_NORMS,
        &norms_variants,
        WORKING_NORMS_AUTHORED_V1,
        turn_id,
    )
    .map_err(|error| GatewayError::ConfigError(error.to_string()))?;
    let norms_decision = match crate::experimentation::record_selection(
        state,
        crate::experimentation::LOCATION_WORKING_NORMS,
        &norms_decision,
        Some(session_id),
        Some(turn_id),
    )
    .await
    {
        Ok(()) => norms_decision,
        Err(error) => {
            tracing::error!(
                %error,
                variant = %norms_decision.variant,
                "working-norms selection could not be recorded; serving the native scaffold"
            );
            crate::experimentation::VariantDecision {
                variant: WORKING_NORMS_AUTHORED_V1.to_owned(),
                reason: crate::experimentation::SelectionReason::RecordUnavailable,
            }
        }
    };
    let working_norms_scaffold = match norms_decision.variant.as_str() {
        WORKING_NORMS_FAMILY_FALLBACK => {
            swage_context::working_norms::WorkingNormsScaffold::FamilyFallback
        }
        _ => swage_context::working_norms::WorkingNormsScaffold::Native,
    };

    Ok(ContextChoices {
        assembly_name: decision.variant,
        working_norms_scaffold,
    })
}

fn execute_context_assembly(
    assembly_name: &str,
    config: &swage_config::ResolvedConfig,
    elements: &[Box<dyn ContextElement>],
    tokeniser: &TiktokenTokeniser,
    swage_root: Option<&Path>,
    space_slug: &str,
    agent_id: &str,
    variables: &std::collections::BTreeMap<String, String>,
) -> swage_context::report::AssemblyOutput {
    let assembly_name = assembly_name.trim();
    if is_default_context_assembly(assembly_name) {
        return swage_context::assembly::default_assembly_with_variables(
            elements,
            &config.context,
            tokeniser,
            variables,
        );
    }

    let search_levels = swage_root
        .map(|root| configuration_hierarchy(root, space_slug, Some(agent_id)))
        .unwrap_or_default();
    let level_refs: Vec<(swage_config::HierarchyLevel, &Path)> = search_levels
        .iter()
        .map(|(level, path)| (level.clone(), path.as_path()))
        .collect();
    let source_path = swage_context::js_assembly::resolve_js_assembly(assembly_name, &level_refs);
    swage_context::js_assembly::assemble_named(
        assembly_name,
        source_path.as_deref(),
        elements,
        &config.context,
        tokeniser,
        variables,
    )
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
