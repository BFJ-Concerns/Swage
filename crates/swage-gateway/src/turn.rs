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
use swage_context::memory::MemoryIndexElement;
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
mod dispatcher_settlement;
mod entry_points;
mod history;
mod inbound_hooks;
mod operator_guidance;
mod projection;
mod provider_record;
mod runtime_launch;
mod sandbox_approval;
mod succession;
mod turn_input;
pub(crate) mod turn_release;
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
use dispatcher_settlement::{DispatcherSettlement, SettledDispatcher, settle_turn_dispatcher};
pub use entry_points::process_turn;
pub(crate) use entry_points::*;
#[cfg(test)]
use history::compact_tool_replay_messages;
use history::rebuild_thread_history_messages;
pub(crate) use inbound_hooks::*;
pub(crate) use operator_guidance::*;
#[cfg(test)]
pub(crate) use projection::route_approval_agent_event;
use projection::{ProjectedTurn, TurnProjection, project_agent_events};
pub(crate) use provider_record::*;
#[cfg(test)]
use runtime_launch::effort_capability_for_model;
use runtime_launch::{RuntimeLaunch, launch_turn_runtime, requested_effort};
use sandbox_approval::{
    SandboxDegradationApproval, SandboxDegradationApprovalRequest,
    request_sandbox_degradation_approval,
};
pub use turn_input::TurnInput;
pub(crate) use turn_input::*;
use turn_release::{
    record_agent_call_attempt_before_execution, record_failed_turn_and_release_on_error,
    record_terminal_state_and_release, release_active_turn,
};
pub(crate) use workspace::*;

pub(crate) use swage_core::DispatchContextComposition;

/// Internal workflow-only witness that an agent reported a provider failure.
pub(crate) type ProviderFailureObserver = Arc<StdMutex<Option<String>>>;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnAttendance {
    Attended,
    Unattended,
}

// Because process_turn is spawned as a tokio task, it won't inherit the
// HTTP request span. The instrument macro creates a self-contained "turn"
// span so all log entries within the turn share the same turn_id and
// session_id. turn_id is Empty at span creation and recorded once the
// Turn record exists.
#[tracing::instrument(
    name = "turn",
    skip(state, input, ws_tx, subscription, composition, compaction_focus),
    fields(turn_id = tracing::field::Empty),
)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn process_turn_with_composition_and_focus(
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
        origin,
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
        let recorded = record_failed_turn_and_release_on_error(
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
                    origin,
                    content: before_inbound.canonical_content.clone(),
                    agent_id: session.agent_id.clone(),
                })
                .await
                .map_err(GatewayError::from),
        )
        .await?;
        // A person's message reached their own view as they sent it; a row
        // the platform composed — a schedule's brief, a workflow's prompt —
        // reaches every open view here, so nobody learns of it on reload.
        if origin != swage_storage::MessageOrigin::User {
            let view = record_failed_turn_and_release_on_error(
                state,
                session_id,
                prepared_turn_id,
                crate::service::message_view(
                    recorded,
                    crate::service::pending_turn_state(),
                    Vec::new(),
                ),
            )
            .await?;
            crate::service::announce_appended_message(state, session_id, view);
        }
    }

    // Step 7: Deliver completed detached delegations (turn/detached_delivery.rs).
    // A storage failure here fails the turn — with the same explicit cleanup
    // as the surrounding error paths, so the thread is not left guarded by a
    // stale Pending turn.
    let delivery_result = deliver_detached_results(
        state,
        session_id,
        &session.agent_id,
        thread_id,
        prepared_turn_id,
    )
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
    let memory_index = if session_carries_memory(&session) {
        Some(
            crate::memory::load_memory_index(swage_root, &scope)
                .await
                .into_element(),
        )
    } else {
        None
    };
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
    let worker_placement = worker_placement_path(session.metadata.as_ref());
    if attendance == TurnAttendance::Unattended
        && worker_placement.is_none()
        && workspace_policy.active_workspace.is_none()
        && !state.config_root.is_absolute()
    {
        record_terminal_state_and_release(state, session_id, prepared_turn_id, TurnState::Failed)
            .await?;
        return Err(GatewayError::ConfigError(format!(
            "unattended turn has no deliberate tool root: attach a workspace or configure an absolute managed scratch root (current root: {})",
            state.config_root.display()
        )));
    }
    let (tool_root, mut allowed_paths) = resolve_turn_tool_root(
        &state.config_root,
        &workspace_policy,
        &scope,
        session.id,
        worker_placement,
    );

    let environment = build_session_environment(&topic, &tool_root, &session);
    let available_skills = available_skills_for_agent(&scope, &session.agent_id, Some(swage_root));

    let operator_guidance = operator_guidance_rules(Some(swage_root), &workspace_policy);
    let hierarchy = configuration_hierarchy(swage_root, &scope, Some(&session.agent_id));

    let elements = build_assembly_elements(
        &config.instructions,
        operator_guidance,
        active_skills,
        history_messages,
        memory_index,
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
    // Over the compaction threshold, or after the thread's recent turns kept
    // hitting walls, the turn runs on a successor: the same elements rendered
    // afresh, the older history summarised against them or diagnosed from
    // outside (turn/succession.rs). Otherwise the first assembly stands.
    let output = succession::succeed_over_threshold(
        succession::SuccessionSite {
            provider: state.provider.as_ref(),
            threshold: &compaction_threshold(&config),
            model: Some(config.default_model_id()).filter(|model| !model.is_empty()),
            focus: compaction_focus.as_deref(),
            variables: &session_variables,
            tokeniser: &tokeniser,
            degradation: succession::recent_degradation(state.storage.as_ref(), thread_id).await,
            diagnosis_models: succession::diagnosis_models(&config),
        },
        elements,
        output,
        assemble,
    )
    .await;
    let (assembled_messages, mut assembly_report) = (output.messages, output.report);
    assembly_report.tool_scope = Some(serde_json::json!({
        "effective_root": tool_root.path,
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
        &tool_root.path,
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
    let dispatcher_build = match settle_turn_dispatcher(
        state,
        DispatcherSettlement {
            config: &config,
            agent_config: &agent_config,
            session_metadata: session.metadata.as_ref(),
            topic_id,
            active_workspace_attachment_id: session.active_workspace_attachment_id,
            ws_tx: &ws_tx,
            session_id,
            prepared_turn_id,
            agent_id: &session.agent_id,
            attendance,
            cancel_token: &cancel_token,
        },
    )
    .await?
    {
        SettledDispatcher::Ready(build) => *build,
        SettledDispatcher::Interrupted => return Ok(()),
    };
    let sandbox_guard = dispatcher_build.sandbox_guard;
    let dispatcher = dispatcher_build.dispatcher;

    // Step 10: Both runtimes emit AgentEvents into the same projection loop.
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let activity_tracker = state.activity_tracker.clone();
    let pricing_config = state.active_config().pricing.clone();
    let requested_effort = requested_effort(
        effort_override,
        session.metadata.as_ref(),
        selected_model_id.as_deref(),
        &config,
    )?;
    let model_name = config.default_model_id().to_string();
    let agent_handle = launch_turn_runtime(
        state,
        RuntimeLaunch {
            config: &config,
            is_codex_turn,
            session_id,
            thread_id,
            prepared_turn_id,
            topic_id,
            agent_id: &session.agent_id,
            model_name: &model_name,
            requested_effort,
            agent_call_scope,
            sandbox_guard: &sandbox_guard,
            report_json: &mut report_json,
            ws_tx: &ws_tx,
            user_message: before_inbound.canonical_content.clone(),
            assembled_messages,
            agent_config,
            dispatcher,
            compaction_focus,
            event_tx,
            cancel_token,
        },
    )
    .await?;

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
            let message = "agent task panicked".to_string();
            let failed_turn_id = turn_id.unwrap_or(prepared_turn_id);
            if let Some(reason) =
                projection::record_failure_explanation(state, session_id, failed_turn_id, &message)
                    .await
            {
                tracing::error!(%reason, turn_id = %failed_turn_id, "turn record integrity lost");
            }
            let _ = ws_tx.send(ServerEvent::Error {
                code: "internal_error".to_string(),
                message,
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

    // A turn that ended on its own hands the session to whatever queued
    // behind it; a stopped turn leaves the queue standing (C60).
    let final_state = state
        .storage
        .get_turn(turn_id.unwrap_or(prepared_turn_id))
        .await
        .ok()
        .flatten()
        .map(|turn| turn.state)
        .unwrap_or(TurnState::Completed);
    if crate::turn_queue::drains_queue_after(final_state) {
        crate::turn_queue::spawn_drain(state.clone(), session_id);
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

#[cfg(test)]
pub(crate) mod tests;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
