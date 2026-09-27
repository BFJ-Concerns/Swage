// TODO Audit turn-processing storage fallbacks
// Review fallback paths in this module that substitute default values when
// stored turn, replay, or context data is degraded. Each path should either
// propagate failure, log explicit degraded-state behaviour, or keep the fallback
// with a test explaining why defaulting is safe.
// ---

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use swage_core::ScopeLocation;

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
use swage_protocol::client::workflow::{WorkflowAgentCallResolution, WorkflowProviderRoundOutcome};
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
mod context_selection;
mod delegation;
mod detached_delivery;
mod dispatcher;
mod history;
mod operator_guidance;
mod projection;
mod provider_record;
mod sandbox_approval;
mod succession;
mod turn_release;
mod workspace;

pub(crate) use configuration::*;
pub(crate) use context::*;
#[cfg(test)]
use context_selection::WORKING_NORMS_FAMILY_FALLBACK;
use context_selection::{execute_context_assembly, select_context_assembly};
pub(crate) use delegation::*;
use detached_delivery::deliver_detached_results;
#[cfg(test)]
use detached_delivery::render_delegation_result;
pub(crate) use dispatcher::*;
#[cfg(test)]
use history::compact_tool_replay_messages;
use history::rebuild_thread_history_messages;
pub(crate) use operator_guidance::*;
#[cfg(test)]
pub(crate) use projection::route_approval_agent_event;
use projection::{ProjectedTurn, TurnProjection, project_agent_events};
pub(crate) use provider_record::*;
use sandbox_approval::{
    SandboxDegradationApproval, SandboxDegradationApprovalRequest,
    request_sandbox_degradation_approval,
};
use turn_release::{
    record_agent_call_attempt_before_execution, record_failed_turn_and_release_on_error,
    record_terminal_state_and_release, release_active_turn,
};
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

/// Refuse a submission whose attachments the selected provider cannot
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
                if !capabilities.vision() {
                    return Err(GatewayError::ValidationError(
                        "the selected provider does not accept image attachments".into(),
                    ));
                }
                if !capabilities.accepts_image_reference(url) {
                    return Err(GatewayError::ValidationError(
                        "the selected provider cannot resolve this image reference: attach the \
                         image inline as a base64 data URL"
                            .into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Resolve the request's effective model and admit its attachments against
/// that model's provider protocol. Every entry point uses this seam before it
/// persists a turn, so the admission decision cannot drift from execution.
pub(crate) fn resolve_turn_model_and_attachments(
    state: &AppState,
    config: swage_config::ResolvedConfig,
    requested_model: Option<&str>,
    session_selected_model: Option<&str>,
    session_id: Uuid,
    attachments: &[MessageAttachment],
) -> Result<(swage_config::ResolvedConfig, &'static str, Option<String>), GatewayError> {
    let selected_model_id = requested_model.or(session_selected_model);
    if let Some(requested_model) = requested_model {
        resolve_requested_turn_model(&config, requested_model)?;
    }
    ensure_selected_model_available(state, &config, selected_model_id, session_id)?;
    let effective_model_source = match requested_model {
        Some(_) => "request",
        None if session_selected_model
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
    let workflow_capabilities =
        crate::capabilities::WorkflowCapabilities::for_protocol(config.default_provider_protocol());
    check_attachment_support(&workflow_capabilities, attachments)?;

    Ok((
        config,
        effective_model_source,
        selected_model_id.map(str::to_owned),
    ))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnAttendance {
    Attended,
    Unattended,
}

/// Distilled view of which permission grants the topic has actually accepted
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
/// 3. Resolve the topic for the session; load per-topic config and build ToolContext.
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
        false,
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
        false,
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
    reuse_persisted_input: bool,
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
        reuse_persisted_input,
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
    reuse_persisted_input: bool,
) -> Result<(), GatewayError> {
    let TurnInput {
        text: content,
        attachments,
    } = input;

    // Step 1: Resolve the topic and target thread for this session.
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
    // Allowing a second turn before the previous one is terminal would
    // interleave history.
    let existing_turns = state.storage.list_turns_by_thread(thread_id).await?;
    if existing_turns.iter().any(|t| {
        if existing_turn_id.is_some_and(|current_turn_id| t.id == current_turn_id) {
            return false;
        }
        !t.state.is_terminal()
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

    // Step 3: Resolve the topic for this session so we can load per-topic config
    // and build the ToolContext passed to each tool invocation.
    let topic_id = session.topic_id;

    let topic = state
        .topic_store
        .get(topic_id)
        .await?
        .ok_or(GatewayError::TopicNotFound { id: topic_id })?;
    let scope = topic.location();

    // Placement identity is durable, but filesystem authority is not. Check
    // the recorded authority root against today's attachment/location grants
    // and replace the cached write bit before assembly or sandbox creation.
    reauthorise_worker_placement_for_turn(state, &mut session).await?;

    // Load topic-specific config. Missing topic config falls back to the
    // startup config, but present invalid config fails the turn before any
    // user message is written.
    let config = load_session_config(state, &scope, &session.agent_id)?;

    // A durable agent definition, when one exists for this agent, gives the
    // session its character on top of the resolved file config: instructions
    // append, the model preference fills an unselected default, and the
    // permission overlay narrows grants. Resolution failure falls open to
    // the file config — a broken definition must not brick the session.
    let config = match crate::service::resolve_effective_agent_definition(
        state.storage.as_ref(),
        &session.agent_id,
        topic_id,
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
        &scope,
        topic_id,
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

    let (config, effective_model_source, selected_model_id) = resolve_turn_model_and_attachments(
        state,
        config,
        requested_model.as_deref(),
        session.selected_model_id.as_deref(),
        session_id,
        &attachments,
    )?;

    // Skill discovery, topic memory loading, and assembly search paths all use
    // the root retained when this application state was constructed.
    let swage_root = resolve_swage_root(state);

    let active_skills = load_active_skills_for_agent(
        state,
        session_id,
        &scope,
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

    // A turn is active execution: an archived session it lands on returns
    // to the recent list now, before anything else the turn does. Always
    // asked, not gated on the row read above — a sweep between that read
    // and the registration would otherwise archive a running session.
    // Visibility only — the inactivity sweep hid the row and this unhides it.
    match state.storage.unarchive_session(session_id).await {
        Ok(_) => session.archived_at = None,
        Err(error) => tracing::warn!(%error, "failed to unarchive the session for its turn"),
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

    if !reuse_persisted_input {
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
    }

    // Step 7: Deliver completed detached delegations (turn/detached_delivery.rs).
    // A storage failure here fails the turn — with the same explicit cleanup
    // as the surrounding error paths, so the thread is not left guarded by a
    // stale Pending turn.
    let delivery_result =
        deliver_detached_results(state, &session.agent_id, thread_id, prepared_turn_id).await;
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
        load_topic_memory(swage_root, &scope).await
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
        topic_id,
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
        agent_workspace_from_policy(&state.config_root, &workspace_policy, topic_id, session.id)
    };

    let environment =
        build_session_environment(&topic, Some(workspace_root.display().to_string()), &session);
    let available_skills = available_skills_for_agent(&scope, &session.agent_id, Some(swage_root));

    let operator_guidance = operator_guidance_rules(Some(swage_root), &workspace_policy);
    let hierarchy = configuration_hierarchy(swage_root, &scope, Some(&session.agent_id));

    let elements = build_assembly_elements(
        &config.instructions,
        operator_guidance,
        active_skills,
        history_messages,
        memory_content.map(|content| (content, topic.name.clone())),
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
    let assemble = |elements: &[Box<dyn ContextElement>]| {
        execute_context_assembly(
            &assembly_name,
            &config,
            elements,
            &tokeniser,
            Some(swage_root),
            &scope,
            &session.agent_id,
            &session_variables,
        )
    };
    let output = assemble(&elements);
    // Over the compaction threshold the turn runs on a successor: the same
    // elements rendered afresh, the older history summarised against them
    // (turn/succession.rs). Within it, the first assembly stands.
    let output = succession::succeed_over_threshold(
        succession::SuccessionSite {
            provider: state.provider.as_ref(),
            threshold: &compaction_threshold(&config),
            model: Some(config.default_model_id()).filter(|model| !model.is_empty()),
            focus: compaction_focus.as_deref(),
            variables: &session_variables,
            tokeniser: &tokeniser,
        },
        elements,
        output,
        assemble,
    )
    .await;
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
        topic_id,
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
            topic_id: Some(topic_id),
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
                        topic_id: Some(topic_id),
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
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let activity_tracker = state.activity_tracker.clone();
    let pricing_config = state.active_config().pricing.clone();
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
                match selected_model_id
                    .as_deref()
                    .filter(|model_id| !model_id.trim().is_empty())
                {
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
    let model_name = config.default_model_id().to_string();
    let agent_handle = if is_codex_turn {
        let effort = requested_effort.map(|requested| {
            let capability = config
                .models
                .entries
                .get(&config.models.default)
                .and_then(|model| model.capabilities.effort.clone())
                .or_else(|| crate::codex::effort_capability(&model_name))
                .unwrap_or(swage_core::EffortCapability::None);
            swage_core::resolve_effort(requested, &capability)
        });
        if let Some(resolved) = effort.as_ref() {
            record_effort_snapshot(&mut report_json, resolved);
        }
        let resolution = WorkflowAgentCallResolution {
            model: Some(model_name.clone()),
            effort: effort
                .as_ref()
                .map(|resolved| resolved.clamped_to.unwrap_or(resolved.requested)),
            session_id: Some(session_id),
            thread_id: Some(thread_id),
            turn_id: Some(prepared_turn_id),
        };
        record_agent_call_attempt_before_execution(
            state,
            session_id,
            prepared_turn_id,
            agent_call_scope,
            &resolution,
            &sandbox_guard,
        )
        .await?;
        let session_approval_grants = state
            .session_approval_grants
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let permissions = build_permission_config(
            &config,
            &state.config,
            Some(topic_id),
            Some(session_id),
            &session_approval_grants,
        );
        let codex_request = crate::codex::CodexTurnRequest {
            session_id,
            thread_id,
            turn_id: prepared_turn_id,
            agent_id: session.agent_id.clone(),
            model: model_name.clone(),
            effort,
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
        // The dispatch record names what the call *asked* for; what the
        // turn actually resolved is only known here, so it is stamped onto
        // the same journal entry as a second field rather than by rewriting
        // the dispatch one. Each turn appends its own attempt, so a call
        // replay re-dispatched keeps the interrupted attempt beside the one
        // that follows it.
        let resolution = WorkflowAgentCallResolution {
            model: Some(model_name.clone()),
            effort: effort
                .as_ref()
                .map(|resolved| resolved.clamped_to.unwrap_or(resolved.requested)),
            session_id: Some(session_id),
            thread_id: Some(thread_id),
            turn_id: Some(prepared_turn_id),
        };
        record_agent_call_attempt_before_execution(
            state,
            session_id,
            prepared_turn_id,
            agent_call_scope,
            &resolution,
            &sandbox_guard,
        )
        .await?;
        let turn_request = TurnRequest {
            execution: swage_agent::context::TurnExecutionContext {
                session_id,
                thread_id,
                turn_id: prepared_turn_id,
                topic_id: Some(topic_id),
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

    // Step 11: Project every AgentEvent to the client and the record
    // (turn/projection.rs) until the agent's sender closes.
    let ProjectedTurn {
        turn_id,
        record_integrity_lost,
    } = project_agent_events(
        state,
        TurnProjection {
            session_id,
            prepared_turn_id,
            report_json: &mut report_json,
            assembly_report: &assembly_report,
            ws_tx: &ws_tx,
            subscription: &subscription,
            activity_tracker: &activity_tracker,
            pricing_config: &pricing_config,
            model_name: &model_name,
            provider_failure_observer: provider_failure_observer.as_ref(),
        },
        event_rx,
    )
    .await;

    // Wait for the agent task to finish and surface any panic or error.
    match agent_handle.await {
        Ok(Ok(_outbound)) => {}
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

#[cfg(test)]
pub(crate) mod tests;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
