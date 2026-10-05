//! Starting the runtime a turn executes on, and the effort it runs at.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: turning what the coordinator resolved into a running
//! agent — the Codex app-server for a Codex-protocol model, the native agent
//! otherwise — at the effort the turn asks for, clamped to what the
//! effective model accepts, with the resolution written into the turn's
//! audit snapshot and the agent-call attempt recorded before either runtime
//! may execute. Both runtimes emit into the one projection loop
//! (turn/projection.rs); what happens after the runtime finishes stays the
//! coordinator's.

use tokio::task::JoinHandle;

use super::{
    AgentBuilder, AgentConfig, AgentError, AgentEvent, AppState, Arc, CancellationToken,
    ChatMessage, GatewayDelegationHandler, GatewayError, SandboxGuard, ServerEvent, ToolDispatcher,
    TurnRequest, TurnState, Uuid, WorkflowAgentCallResolution, build_approval_registrar,
    build_permission_config, mpsc, record_agent_call_attempt_before_execution,
    record_effort_snapshot, record_terminal_state_and_release, resolve_model_catalogue_entry,
    shutdown_sandbox_guard,
};
use swage_agent::OutboundMessage;

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

/// The effort a turn asks for: the caller's override, else the session's
/// recorded effort, else the selected (or default) model's default effort.
pub(super) fn requested_effort(
    effort_override: Option<swage_core::Effort>,
    session_metadata: Option<&serde_json::Value>,
    selected_model_id: Option<&str>,
    config: &swage_config::ResolvedConfig,
) -> Result<Option<swage_core::Effort>, GatewayError> {
    Ok(match effort_override {
        Some(effort) => Some(effort),
        None => session_metadata
            .and_then(|metadata| metadata.get("effort"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                GatewayError::ConfigError(format!("session effort is invalid: {error}"))
            })?
            .or_else(
                || match selected_model_id.filter(|model_id| !model_id.trim().is_empty()) {
                    Some(model_id) => resolve_model_catalogue_entry(&config.models, model_id)
                        .and_then(|(_, model)| model.default_effort),
                    None => config
                        .models
                        .entries
                        .get(&config.models.default)
                        .and_then(|model| model.default_effort),
                },
            ),
    })
}

/// What the coordinator resolved for starting the turn's runtime: borrowed
/// where the coordinator keeps using a value afterwards, owned where the
/// runtime takes it over.
pub(super) struct RuntimeLaunch<'a> {
    pub(super) config: &'a swage_config::ResolvedConfig,
    pub(super) is_codex_turn: bool,
    pub(super) session_id: Uuid,
    pub(super) thread_id: Uuid,
    pub(super) prepared_turn_id: Uuid,
    pub(super) topic_id: Uuid,
    pub(super) agent_id: &'a str,
    pub(super) model_name: &'a str,
    pub(super) requested_effort: Option<swage_core::Effort>,
    pub(super) agent_call_scope: Option<Uuid>,
    pub(super) sandbox_guard: &'a Option<SandboxGuard>,
    pub(super) report_json: &'a mut serde_json::Value,
    pub(super) ws_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub(super) user_message: String,
    pub(super) assembled_messages: Vec<ChatMessage>,
    pub(super) agent_config: AgentConfig,
    pub(super) dispatcher: ToolDispatcher,
    pub(super) compaction_focus: Option<String>,
    pub(super) event_tx: mpsc::UnboundedSender<AgentEvent>,
    pub(super) cancel_token: CancellationToken,
}

/// Start the runtime the turn executes on — the Codex app-server or the
/// native agent — with the call attempt recorded before either may execute.
/// Both emit their `AgentEvent`s on `event_tx`; the handle resolves when the
/// runtime finishes.
pub(super) async fn launch_turn_runtime(
    state: &AppState,
    launch: RuntimeLaunch<'_>,
) -> Result<JoinHandle<Result<OutboundMessage, AgentError>>, GatewayError> {
    let RuntimeLaunch {
        config,
        is_codex_turn,
        session_id,
        thread_id,
        prepared_turn_id,
        topic_id,
        agent_id,
        model_name,
        requested_effort,
        agent_call_scope,
        sandbox_guard,
        report_json,
        ws_tx,
        user_message,
        assembled_messages,
        agent_config,
        dispatcher,
        compaction_focus,
        event_tx,
        cancel_token,
    } = launch;
    let handle = if is_codex_turn {
        let effort = requested_effort.map(|requested| {
            let capability = config
                .models
                .entries
                .get(&config.models.default)
                .and_then(|model| model.capabilities.effort.clone())
                .or_else(|| crate::codex::effort_capability(model_name))
                .unwrap_or(swage_core::EffortCapability::None);
            swage_core::resolve_effort(requested, &capability)
        });
        if let Some(resolved) = effort.as_ref() {
            record_effort_snapshot(report_json, resolved);
        }
        let resolution = WorkflowAgentCallResolution {
            model: Some(model_name.to_owned()),
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
            sandbox_guard,
        )
        .await?;
        let session_approval_grants = state
            .session_approval_grants
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let permissions = build_permission_config(
            config,
            &state.config,
            Some(topic_id),
            Some(session_id),
            &session_approval_grants,
        );
        let codex_request = crate::codex::CodexTurnRequest {
            session_id,
            thread_id,
            turn_id: prepared_turn_id,
            agent_id: agent_id.to_owned(),
            model: model_name.to_owned(),
            effort,
            user_message,
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
        let provider = crate::startup::selected_model_provider(state, config).await;
        let mut agent_builder = AgentBuilder::new()
            .config(agent_config)
            .provider(Arc::clone(&provider))
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
                let message = error.to_string();
                if let Some(reason) = super::projection::record_failure_explanation(
                    state,
                    session_id,
                    prepared_turn_id,
                    &message,
                )
                .await
                {
                    tracing::error!(%reason, turn_id = %prepared_turn_id, "turn record integrity lost");
                }
                let _ = ws_tx.send(ServerEvent::Error {
                    code: "agent_construction_error".to_string(),
                    message,
                });
                shutdown_sandbox_guard(
                    sandbox_guard,
                    "failed to shut down turn sandbox after agent construction error",
                )
                .await;
                record_terminal_state_and_release(
                    state,
                    session_id,
                    prepared_turn_id,
                    TurnState::Failed,
                )
                .await?;
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
            let capability =
                effort_capability_for_model(config, &config.models.default, provider.as_ref());
            swage_core::resolve_effort(requested, &capability)
        });
        if let Some(resolved) = effort.as_ref() {
            record_effort_snapshot(report_json, resolved);
        }
        // The dispatch record names what the call *asked* for; what the
        // turn actually resolved is only known here, so it is stamped onto
        // the same journal entry as a second field rather than by rewriting
        // the dispatch one. Each turn appends its own attempt, so a call
        // replay re-dispatched keeps the interrupted attempt beside the one
        // that follows it.
        let resolution = WorkflowAgentCallResolution {
            model: Some(model_name.to_owned()),
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
            sandbox_guard,
        )
        .await?;
        let turn_request = TurnRequest {
            execution: swage_agent::context::TurnExecutionContext {
                session_id,
                thread_id,
                turn_id: prepared_turn_id,
                topic_id: Some(topic_id),
            },
            user_message,
            assembled_context: assembled_messages,
            compaction_focus,
            parent_turn_id: None,
            existing_turn_id: Some(prepared_turn_id),
            effort,
            cancellation_token: cancel_token,
        };
        tokio::spawn(async move { agent.process_turn_streaming(turn_request, event_tx).await })
    };
    Ok(handle)
}
