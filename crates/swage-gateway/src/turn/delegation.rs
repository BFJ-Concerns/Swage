use super::{
    AgentError, AppState, Arc, CancellationToken, DELEGATED_CHILDREN_METADATA_KEY,
    DelegationExecutor, DelegationHandler, DispatchContextComposition, GatewayDispatcherRequest,
    MessageRouter, NewAgentMessage, ResolvedToolRoot, Uuid,
    WORKER_PLACEMENT_AUTHORITY_CONTEXT_METADATA_KEY, WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY,
    WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY, WORKER_PLACEMENT_CWD_METADATA_KEY,
    WORKFLOW_CALL_TOOLS_METADATA_KEY, add_dispatch_bound_task_definition_ids,
    apply_agent_definition, apply_dispatch_bounds, apply_dispatch_task_bounds,
    apply_workflow_call_tools, available_skills_for_agent, build_agent_config_for_topic_workspace,
    build_approval_registrar, build_assembly_elements,
    build_gateway_dispatcher_for_unattended_execution, build_session_environment,
    configuration_hierarchy, dispatch_bound_agent_ids_for_turn, dispatch_bound_metadata,
    dispatch_bound_task_definition_ids_for_turn, effective_tool_permissions_record,
    execute_context_assembly, load_active_skills_for_agent, load_memory_for_assembly,
    load_session_config, load_topic_memory, load_trusted_workspace_policy, mpsc,
    operator_guidance_rules, resolve_authorised_worker_directory,
    resolve_requested_skills_for_agent, resolve_swage_root, select_context_assembly,
    shutdown_sandbox_guard,
};
use crate::turn::workspace::ResolvedWorkerDirectory;
use sha2::{Digest, Sha256};
use swage_agent::{
    AgentEvent, DelegatedAssembly, DelegatedChildContinuation, DelegationContextAssembler,
    DelegationOutcome, DelegationSnapshotRecorder,
};
use swage_context::tokeniser::TiktokenTokeniser;
use swage_core::DelegateCompletionMode;

pub(super) const DELEGATED_CHILD_SKILLS_METADATA_KEY: &str = "delegated_child_skills";
pub(super) const DELEGATED_CHILD_COMPOSITION_METADATA_KEY: &str = "delegated_child_composition";
pub(super) const DELEGATED_CHILD_REQUESTED_CWD_METADATA_KEY: &str = "delegated_child_requested_cwd";
const DELEGATED_CHILD_ISOLATION_METADATA_KEY: &str = "delegated_child_isolation";

struct ResolvedNamedChild {
    continuation: DelegatedChildContinuation,
    metadata: serde_json::Value,
}

/// Look up a child this session has delegated to under `name`, and load
/// everything it has heard and said so far.
///
/// The agent is fixed by the call that opened the child: a later call naming
/// it must be talking to the same agent, or it is a different child wearing a
/// used name. An unknown name is not an error — it opens a child.
async fn resolve_named_child(
    state: &AppState,
    parent_session_id: Uuid,
    name: &str,
    target_agent_id: &str,
) -> Result<Option<ResolvedNamedChild>, AgentError> {
    let session = state
        .storage
        .get_session(parent_session_id)
        .await?
        .ok_or(AgentError::SessionNotFound(parent_session_id))?;
    let Some(entry) = session
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(DELEGATED_CHILDREN_METADATA_KEY))
        .and_then(|children| children.get(name))
    else {
        return Ok(None);
    };
    let recorded_agent = entry.get("agent_id").and_then(serde_json::Value::as_str);
    if recorded_agent != Some(target_agent_id) {
        return Err(AgentError::InvalidConfig(format!(
            "child {name:?} is {recorded_agent:?}, not {target_agent_id:?}; \
             use a different name to delegate to a different agent"
        )));
    }
    let id = |field: &str| {
        entry
            .get(field)
            .and_then(serde_json::Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| {
                AgentError::InvalidConfig(format!("child {name:?} has no usable {field}"))
            })
    };
    let session_id = id("session_id")?;
    let thread_id = id("thread_id")?;
    let child_session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(AgentError::SessionNotFound(session_id))?;
    if child_session.agent_id != target_agent_id {
        return Err(AgentError::InvalidConfig(format!(
            "child {name:?} session belongs to {:?}, not {target_agent_id:?}",
            child_session.agent_id
        )));
    }
    let turns = state.storage.list_turns_by_thread(thread_id).await?;
    if turns.iter().any(|turn| !turn.state.is_terminal()) {
        return Err(AgentError::InvalidConfig(format!(
            "child {name:?} is still taking its previous turn; named child turns are sequential"
        )));
    }
    let history = super::rebuild_thread_history_messages(state.storage.as_ref(), thread_id)
        .await
        .map_err(|error| {
            AgentError::InvalidConfig(format!("cannot reload child {name:?}: {error}"))
        })?;
    Ok(Some(ResolvedNamedChild {
        continuation: DelegatedChildContinuation {
            session_id,
            thread_id,
            history,
        },
        metadata: child_session
            .metadata
            .unwrap_or_else(|| serde_json::json!({})),
    }))
}

/// Record a freshly opened child under the name the caller gave it, so a later
/// delegate call naming it returns to this child.
async fn record_named_child(
    state: &AppState,
    parent_session_id: Uuid,
    name: &str,
    target_agent_id: &str,
    child_session_id: Uuid,
    child_thread_id: Uuid,
) -> Result<(), AgentError> {
    let session = state
        .storage
        .get_session(parent_session_id)
        .await?
        .ok_or(AgentError::SessionNotFound(parent_session_id))?;
    let mut metadata = session.metadata.unwrap_or_else(|| serde_json::json!({}));
    if !metadata[DELEGATED_CHILDREN_METADATA_KEY].is_object() {
        metadata[DELEGATED_CHILDREN_METADATA_KEY] = serde_json::json!({});
    }
    metadata[DELEGATED_CHILDREN_METADATA_KEY][name] = serde_json::json!({
        "agent_id": target_agent_id,
        "session_id": child_session_id,
        "thread_id": child_thread_id,
    });
    state
        .storage
        .set_session_metadata(parent_session_id, Some(metadata))
        .await?;
    Ok(())
}

pub(super) async fn reauthorise_named_child_placement(
    state: &AppState,
    topic_id: Uuid,
    sender_turn_id: Uuid,
    sender_session_id: Uuid,
    active_workspace_attachment_id: Option<Uuid>,
    sender_placement: Option<&ResolvedWorkerDirectory>,
    metadata: &serde_json::Value,
) -> Result<Option<ResolvedWorkerDirectory>, AgentError> {
    let Some(path) = metadata
        .get(WORKER_PLACEMENT_CWD_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
    else {
        return Ok(None);
    };
    let authority_path = metadata
        .get(WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            AgentError::InvalidConfig("named child placement is missing its authority path".into())
        })?;
    let authority_context_id = metadata
        .get(WORKER_PLACEMENT_AUTHORITY_CONTEXT_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| {
            AgentError::InvalidConfig(
                "named child placement is missing its authority context".into(),
            )
        })?;
    let authority = resolve_authorised_worker_directory(
        state,
        topic_id,
        sender_turn_id,
        Some(sender_session_id),
        active_workspace_attachment_id,
        sender_placement,
        authority_path,
    )
    .await
    .map_err(AgentError::InvalidConfig)?;
    Ok(Some(ResolvedWorkerDirectory {
        path,
        can_write: authority.can_write,
        authority_path: authority.path,
        authority_context_id,
    }))
}

pub(crate) fn delegate_worktree_owner(sender_turn_id: Uuid, delegate_call_id: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(sender_turn_id.as_bytes());
    digest.update(delegate_call_id.as_bytes());
    let digest = digest.finalize();
    Uuid::from_bytes(digest[..16].try_into().expect("SHA-256 prefix"))
}

struct GatewayDelegatedContextAssembler {
    state: AppState,
    topic: swage_storage::Topic,
    config: swage_config::ResolvedConfig,
    agent_id: String,
    tool_root: ResolvedToolRoot,
    swage_root: std::path::PathBuf,
    context_event_tx: mpsc::UnboundedSender<crate::websocket::ServerEvent>,
    effective_tool_permissions: serde_json::Value,
    composition: DispatchContextComposition,
}

pub(crate) fn delegated_context_assembler(
    state: AppState,
    topic: swage_storage::Topic,
    config: swage_config::ResolvedConfig,
    agent_id: String,
    tool_root: ResolvedToolRoot,
    context_event_tx: mpsc::UnboundedSender<crate::websocket::ServerEvent>,
    effective_tool_permissions: serde_json::Value,
    composition: DispatchContextComposition,
) -> Arc<dyn DelegationContextAssembler> {
    let swage_root = resolve_swage_root(&state).to_path_buf();
    Arc::new(GatewayDelegatedContextAssembler {
        state,
        topic,
        config,
        agent_id,
        tool_root,
        swage_root,
        context_event_tx,
        effective_tool_permissions,
        composition,
    })
}

#[async_trait::async_trait]
impl DelegationContextAssembler for GatewayDelegatedContextAssembler {
    async fn assemble(
        &self,
        session: &swage_storage::Session,
        turn_id: Uuid,
        delegated_history: Vec<swage_core::ChatMessage>,
    ) -> Result<DelegatedAssembly, AgentError> {
        let active_skills = load_active_skills_for_agent(
            &self.state,
            session.id,
            &self.topic.location(),
            &self.agent_id,
            Some(&self.swage_root),
            false,
            &self.config.declared_skills,
            self.composition.skills.as_deref().unwrap_or_default(),
        )
        .await
        .map_err(AgentError::InvalidConfig)?;
        let context_choices = select_context_assembly(
            &self.state,
            &self.config,
            session.id,
            turn_id,
            self.composition.assembly.as_deref(),
        )
        .await
        .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        let assembly_name = context_choices.assembly_name;
        let memory_content = load_memory_for_assembly(&assembly_name, || async {
            load_topic_memory(&self.swage_root, &self.topic.location()).await
        })
        .await;
        let mut session_variables: std::collections::BTreeMap<String, String> = self
            .state
            .storage
            .list_session_variables(session.id)
            .await
            .map_err(AgentError::StorageFailed)?
            .into_iter()
            .map(|variable| (variable.name, variable.value))
            .collect();
        session_variables.extend(self.composition.variables.clone().unwrap_or_default());
        // Delegated children read the same operator-guidance sources as
        // foreground turns: the trust policy is re-derived here because the
        // assembler retains the resolved tool root, not the policy.
        let workspace_policy = load_trusted_workspace_policy(
            &self.state.storage,
            self.topic.id,
            session.active_workspace_attachment_id,
        )
        .await;
        let environment = build_session_environment(&self.topic, &self.tool_root, session);
        let available_skills = available_skills_for_agent(
            &self.topic.location(),
            &self.agent_id,
            Some(&self.swage_root),
        );
        let operator_guidance = operator_guidance_rules(Some(&self.swage_root), &workspace_policy);
        let hierarchy = configuration_hierarchy(
            &self.swage_root,
            &self.topic.location(),
            Some(&self.agent_id),
        );
        let elements = build_assembly_elements(
            &self.config.instructions,
            operator_guidance,
            active_skills,
            delegated_history,
            memory_content.map(|content| (content, self.topic.name.clone())),
            environment,
            available_skills,
            self.config.default_model_id(),
            context_choices.working_norms_scaffold,
            &hierarchy,
            self.composition.elements.as_deref(),
        );
        let tokeniser = TiktokenTokeniser::new(self.config.default_model_id());
        let output = execute_context_assembly(
            &assembly_name,
            &self.config,
            &elements,
            &tokeniser,
            Some(&self.swage_root),
            &self.topic.location(),
            &self.agent_id,
            &session_variables,
        );
        let mut assembly_report = output.report;
        assembly_report.effective_tool_permissions = Some(self.effective_tool_permissions.clone());
        let report = serde_json::to_value(&assembly_report).unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to serialise delegated assembly report");
            serde_json::json!({ "error": "serialisation failed" })
        });
        let _ = self
            .context_event_tx
            .send(crate::websocket::ServerEvent::ContextAssembled {
                turn_id,
                report: assembly_report,
            });
        Ok(DelegatedAssembly {
            messages: output.messages,
            report,
        })
    }
}

struct GatewayDelegationSnapshotRecorder {
    state: AppState,
}

#[cfg(test)]
struct SnapshotOnlyDelegationRecorder {
    storage: Arc<dyn swage_storage::Storage>,
}

#[cfg(test)]
pub(crate) fn delegation_snapshot_recorder(
    storage: Arc<dyn swage_storage::Storage>,
) -> Arc<dyn DelegationSnapshotRecorder> {
    Arc::new(SnapshotOnlyDelegationRecorder { storage })
}

pub(crate) fn delegation_snapshot_recorder_for_state(
    state: AppState,
) -> Arc<dyn DelegationSnapshotRecorder> {
    Arc::new(GatewayDelegationSnapshotRecorder { state })
}

impl GatewayDelegationSnapshotRecorder {
    async fn persist(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        report: &serde_json::Value,
    ) -> Result<(), AgentError> {
        let storage = self.state.storage.as_ref();
        persist_delegated_snapshot(storage, session_id, turn_id, report).await
    }
}

async fn persist_delegated_snapshot(
    storage: &dyn swage_storage::Storage,
    session_id: Uuid,
    turn_id: Uuid,
    report: &serde_json::Value,
) -> Result<(), AgentError> {
    if let Err(error) = storage.store_snapshot(turn_id, &report.to_string()).await {
        // Reconstructive record (record_durability.rs): record the gap and
        // escalate to a forced turn failure only if that record also fails.
        tracing::warn!(%turn_id, %error, "failed to persist delegated context snapshot");
        if let Err(audit_error) = crate::record_durability::record_reconstruction_gap(
            storage,
            Some(session_id),
            turn_id,
            "delegated_context_snapshot",
            &error,
        )
        .await
        {
            if let Err(force_error) = storage.force_turn_failure(turn_id).await {
                tracing::error!(%turn_id, %force_error, "failed to mark delegated turn failed after record loss");
            }
            return Err(AgentError::InvalidConfig(format!(
                "delegated context snapshot lost and the loss could not be recorded: {audit_error}"
            )));
        }
    }
    Ok(())
}

#[async_trait::async_trait]
impl DelegationSnapshotRecorder for GatewayDelegationSnapshotRecorder {
    async fn record(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        report: &serde_json::Value,
    ) -> Result<(), AgentError> {
        self.persist(session_id, turn_id, report).await
    }

    async fn record_event(&self, session_id: Uuid, event: AgentEvent) -> Result<(), AgentError> {
        let turn_id = match &event {
            AgentEvent::ProviderRequestPrepared { turn_id, .. }
            | AgentEvent::ProviderResponseCaptured { turn_id, .. }
            | AgentEvent::ProviderReasoningCaptured { turn_id, .. } => *turn_id,
            _ => return Ok(()),
        };
        let Some(snapshot) = self.state.storage.get_snapshot(turn_id).await? else {
            return Err(AgentError::InvalidConfig(format!(
                "delegated turn {turn_id} has no context snapshot for provider request"
            )));
        };
        let mut report: serde_json::Value = serde_json::from_str(&snapshot).map_err(|error| {
            AgentError::InvalidConfig(format!(
                "delegated context snapshot is invalid JSON: {error}"
            ))
        })?;
        super::apply_provider_snapshot_event(
            &self.state,
            &mut report,
            event,
            super::ProviderSnapshotCapturePath::Delegated,
        )
        .await;
        self.record(session_id, turn_id, &report).await
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl DelegationSnapshotRecorder for SnapshotOnlyDelegationRecorder {
    async fn record(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        report: &serde_json::Value,
    ) -> Result<(), AgentError> {
        persist_delegated_snapshot(self.storage.as_ref(), session_id, turn_id, report).await
    }
}

#[derive(Clone)]
pub(crate) struct GatewayDelegationHandler {
    state: AppState,
    pub(super) active_delegations: swage_agent::ActiveDelegations,
}

impl GatewayDelegationHandler {
    pub(crate) fn new(state: AppState) -> Self {
        Self {
            state,
            active_delegations: swage_agent::active_delegations(),
        }
    }
}

fn delegate_message_payload(
    composition: DispatchContextComposition,
    task: &str,
    task_definition: Option<&str>,
) -> Result<serde_json::Value, AgentError> {
    let mut payload = serde_json::to_value(composition).map_err(|error| {
        AgentError::InvalidConfig(format!(
            "failed to encode delegated context composition: {error}"
        ))
    })?;
    payload["task"] = serde_json::json!(task);
    payload["task_definition"] = serde_json::json!(task_definition);
    Ok(payload)
}

#[async_trait::async_trait]
impl DelegationHandler for GatewayDelegationHandler {
    async fn handle_delegation(
        &self,
        target_agent_id: &str,
        task: &str,
        task_definition: Option<&str>,
        context: Option<serde_json::Value>,
        cwd: Option<String>,
        isolation: Option<swage_protocol::client::workflow::WorkerIsolation>,
        skills: Option<Vec<String>>,
        child: Option<&str>,
        composition: DispatchContextComposition,
        delegate_call_id: &str,
        sender_agent_id: &str,
        sender_turn_id: Uuid,
        topic_id: Uuid,
        cancel_token: CancellationToken,
        completion_mode: DelegateCompletionMode,
    ) -> Result<DelegationOutcome, AgentError> {
        let topic = self.state.topic_store.get(topic_id).await?.ok_or_else(|| {
            AgentError::InvalidConfig(format!("delegation topic {topic_id} no longer exists"))
        })?;
        let sender_filesystem_authority =
            self.filesystem_authority_for_turn(sender_turn_id).await?;
        let active_workspace_attachment_id =
            sender_filesystem_authority.active_workspace_attachment_id;
        let sender_session_id = sender_filesystem_authority.session_id;
        let existing_named_child = match child {
            Some(name) => {
                resolve_named_child(&self.state, sender_session_id, name, target_agent_id).await?
            }
            None => None,
        };
        let read_child_option = |key: &str| -> Result<Option<serde_json::Value>, AgentError> {
            existing_named_child
                .as_ref()
                .map(|child| {
                    child.metadata.get(key).cloned().ok_or_else(|| {
                        AgentError::InvalidConfig(format!(
                            "named child is missing its fixed {key} creation record"
                        ))
                    })
                })
                .transpose()
        };
        let task_definition = match existing_named_child.as_ref() {
            Some(child) => child
                .metadata
                .get("task_definition")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            None => task_definition.map(str::to_owned),
        };
        let cwd = match read_child_option(DELEGATED_CHILD_REQUESTED_CWD_METADATA_KEY)? {
            Some(value) => serde_json::from_value(value).map_err(|error| {
                AgentError::InvalidConfig(format!("named child has invalid fixed cwd: {error}"))
            })?,
            None => cwd,
        };
        let isolation = match read_child_option(DELEGATED_CHILD_ISOLATION_METADATA_KEY)? {
            Some(value) => serde_json::from_value(value).map_err(|error| {
                AgentError::InvalidConfig(format!(
                    "named child has invalid fixed isolation: {error}"
                ))
            })?,
            None => isolation,
        };
        let skills = match read_child_option(DELEGATED_CHILD_SKILLS_METADATA_KEY)? {
            Some(value) => serde_json::from_value(value).map_err(|error| {
                AgentError::InvalidConfig(format!("named child has invalid fixed skills: {error}"))
            })?,
            None => skills,
        };
        let composition = match read_child_option(DELEGATED_CHILD_COMPOSITION_METADATA_KEY)? {
            Some(value) => serde_json::from_value(value).map_err(|error| {
                AgentError::InvalidConfig(format!(
                    "named child has invalid fixed context composition: {error}"
                ))
            })?,
            None => composition,
        };

        let target_resolved =
            load_session_config(&self.state, &topic.location(), target_agent_id).map_err(|error| {
                AgentError::InvalidConfig(format!(
                    "failed to load config for delegated agent '{target_agent_id}' in topic '{}': {error}",
                    topic.slug
                ))
            })?;
        let target_resolved = match crate::service::resolve_effective_agent_definition(
            self.state.storage.as_ref(),
            target_agent_id,
            topic_id,
        )
        .await
        {
            Ok(Some(definition)) => apply_agent_definition(target_resolved, &definition.definition),
            Ok(None) => target_resolved,
            Err(error) => {
                tracing::warn!(
                    agent_id = target_agent_id,
                    %error,
                    "failed to resolve delegated agent definition; using file config alone"
                );
                target_resolved
            }
        };
        let target_resolved = match task_definition.as_deref() {
            Some(id) => {
                let definition =
                    crate::service::resolve_task_definition(self.state.storage.as_ref(), id)
                        .await
                        .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
                let decision = crate::experimentation::select_variant(
                    &target_resolved.experimentation,
                    crate::experimentation::LOCATION_TASK_PROMPT,
                    &[],
                    "definition",
                    sender_turn_id,
                )
                .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
                if let Err(error) = crate::experimentation::record_selection(
                    &self.state,
                    crate::experimentation::LOCATION_TASK_PROMPT,
                    &decision,
                    None,
                    Some(sender_turn_id),
                )
                .await
                {
                    tracing::warn!(%error, task_id = id, "task prompt variant record unavailable; serving the definition default");
                }
                super::apply_task_definition(target_resolved, &definition)
                    .map_err(|error| AgentError::InvalidConfig(error.to_string()))?
            }
            None => target_resolved,
        };
        resolve_requested_skills_for_agent(
            &topic.location(),
            target_agent_id,
            Some(resolve_swage_root(&self.state)),
            skills.as_deref().unwrap_or_default(),
        )
        .map_err(AgentError::InvalidConfig)?;

        let mut bound_agent_ids = dispatch_bound_agent_ids_for_turn(&self.state, sender_turn_id)
            .await
            .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        bound_agent_ids.push(sender_agent_id.to_string());
        let bound_task_definition_ids =
            dispatch_bound_task_definition_ids_for_turn(&self.state, sender_turn_id)
                .await
                .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        let mut session_metadata = dispatch_bound_metadata(&bound_agent_ids)
            .expect("a delegated session always has at least its sender bound");
        add_dispatch_bound_task_definition_ids(&mut session_metadata, &bound_task_definition_ids);
        if let Some(workflow_call_tools) = &sender_filesystem_authority.workflow_call_tools {
            session_metadata[WORKFLOW_CALL_TOOLS_METADATA_KEY] = workflow_call_tools.clone();
        }
        if let Some(task_definition) = task_definition.as_deref() {
            session_metadata["task_definition"] =
                serde_json::Value::String(task_definition.to_string());
        }
        session_metadata[DELEGATED_CHILD_REQUESTED_CWD_METADATA_KEY] = serde_json::json!(cwd);
        session_metadata[DELEGATED_CHILD_ISOLATION_METADATA_KEY] = serde_json::json!(isolation);
        session_metadata[DELEGATED_CHILD_SKILLS_METADATA_KEY] = serde_json::json!(skills);
        session_metadata[DELEGATED_CHILD_COMPOSITION_METADATA_KEY] =
            serde_json::to_value(&composition).map_err(|error| {
                AgentError::InvalidConfig(format!(
                    "failed to record named child context composition: {error}"
                ))
            })?;
        let effective_target_resolved = apply_dispatch_bounds(
            &self.state,
            &topic.location(),
            topic_id,
            target_resolved.clone(),
            &bound_agent_ids,
            &format!("delegated agent {target_agent_id:?}"),
        )
        .await
        .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        let effective_target_resolved = apply_dispatch_task_bounds(
            &self.state,
            effective_target_resolved,
            &bound_task_definition_ids,
            &format!("delegated agent {target_agent_id:?}"),
        )
        .await
        .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        let effective_target_resolved =
            apply_workflow_call_tools(effective_target_resolved, Some(&session_metadata))
                .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;

        if let Some(existing) = existing_named_child.as_ref() {
            session_metadata = existing.metadata.clone();
        }
        let existing_child = existing_named_child.map(|child| child.continuation);
        let (mut target_agent_config, mut tool_root) = build_agent_config_for_topic_workspace(
            &self.state,
            target_agent_id.to_string(),
            &target_resolved,
            None,
            &topic,
            sender_turn_id,
            active_workspace_attachment_id,
        )
        .await;
        let mut worktree_owner_id = None;
        let placement = if existing_child.is_some() {
            reauthorise_named_child_placement(
                &self.state,
                topic_id,
                sender_turn_id,
                sender_session_id,
                active_workspace_attachment_id,
                sender_filesystem_authority.placement.as_ref(),
                &session_metadata,
            )
            .await?
        } else {
            match isolation {
                None => match cwd.as_deref() {
                    Some(cwd) => Some(
                        resolve_authorised_worker_directory(
                            &self.state,
                            topic_id,
                            sender_turn_id,
                            Some(sender_session_id),
                            active_workspace_attachment_id,
                            sender_filesystem_authority.placement.as_ref(),
                            cwd,
                        )
                        .await
                        .map_err(AgentError::InvalidConfig)?,
                    ),
                    None => sender_filesystem_authority.placement.clone(),
                },
                Some(swage_protocol::client::workflow::WorkerIsolation::Worktree) => {
                    let repo = cwd.as_deref().ok_or_else(|| {
                        AgentError::InvalidConfig(
                            "delegate isolation=worktree requires cwd to name the git repository"
                                .into(),
                        )
                    })?;
                    let repo = resolve_authorised_worker_directory(
                        &self.state,
                        topic_id,
                        sender_turn_id,
                        Some(sender_session_id),
                        active_workspace_attachment_id,
                        sender_filesystem_authority.placement.as_ref(),
                        repo,
                    )
                    .await
                    .map_err(AgentError::InvalidConfig)?;
                    let owner_id = delegate_worktree_owner(sender_turn_id, delegate_call_id);
                    worktree_owner_id = Some(owner_id);
                    Some(ResolvedWorkerDirectory {
                        path: self
                            .state
                            .worktree_host
                            .provision(owner_id, 0, &repo.path)
                            .await
                            .map_err(AgentError::InvalidConfig)?
                            .path,
                        can_write: repo.can_write,
                        authority_path: repo.authority_path,
                        authority_context_id: repo.authority_context_id,
                    })
                }
            }
        };
        if let Some(placement) = placement.as_ref() {
            target_agent_config = target_agent_config
                .with_workspace(placement.path.clone(), vec![placement.path.clone()]);
            tool_root = ResolvedToolRoot::worker_placement(placement.path.clone());
        }
        target_agent_config.approval_prompts_enabled = false;
        let target_context_transfer_config = target_resolved.communication.context_transfer.clone();

        // Delegated child agents execute within the gateway runtime, but their
        // internal tool activity is not surfaced on the parent conversation's
        // WebSocket stream. Shared assembly still emits ContextAssembled into
        // this silent channel; the stored child-turn snapshot is the delegated
        // assembly's audit record.
        let (silent_ws_tx, _silent_ws_rx) = mpsc::unbounded_channel();
        if let Some(placement) = placement.as_ref() {
            session_metadata[WORKER_PLACEMENT_CWD_METADATA_KEY] =
                serde_json::Value::String(placement.path.to_string_lossy().into_owned());
            session_metadata[WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY] =
                serde_json::Value::Bool(placement.can_write);
            session_metadata[WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY] =
                serde_json::Value::String(placement.authority_path.to_string_lossy().into_owned());
            session_metadata[WORKER_PLACEMENT_AUTHORITY_CONTEXT_METADATA_KEY] =
                serde_json::Value::String(placement.authority_context_id.to_string());
        }
        let target_dispatcher_build = build_gateway_dispatcher_for_unattended_execution(
            &self.state,
            GatewayDispatcherRequest {
                config: &effective_target_resolved,
                agent_config: &target_agent_config,
                session_metadata: Some(&session_metadata),
                topic_id: Some(topic_id),
                active_workspace_attachment_id,
                ws_tx: silent_ws_tx.clone(),
                session_id: None,
                turn_id: Some(sender_turn_id),
            },
            "delegated turn",
        )
        .await
        .map_err(|error| {
            AgentError::InvalidConfig(format!("failed to build delegated sandbox: {error}"))
        })?;
        let target_sandbox_guard = target_dispatcher_build.sandbox_guard;
        let target_dispatcher = target_dispatcher_build.dispatcher;

        let router = Arc::new(MessageRouter::new(self.state.storage.clone(), None));
        let executor = DelegationExecutor {
            storage: self.state.storage.clone(),
            provider: self.state.active_provider(),
            hook_registry: self.state.hook_registry.clone(),
            leak_detector: None,
            approval_registrar: Some(build_approval_registrar(
                self.state.approval_channels.clone(),
            )),
            delegation_handler: Some(Arc::new(self.clone())),
            router: router.clone(),
            active_delegations: self.active_delegations.clone(),
            context_assembler: delegated_context_assembler(
                self.state.clone(),
                topic.clone(),
                target_resolved.clone(),
                target_agent_id.to_string(),
                tool_root,
                silent_ws_tx,
                effective_tool_permissions_record(&effective_target_resolved),
                composition.clone(),
            ),
            snapshot_recorder: Some(delegation_snapshot_recorder_for_state(self.state.clone())),
        };

        let mut payload = delegate_message_payload(composition, task, task_definition.as_deref())?;
        payload["placement"] = serde_json::json!({
            "cwd": placement.as_ref().map(|placement| &placement.path),
            "requested_cwd": cwd,
            "isolation": isolation,
        });
        if let Some(owner_id) = worktree_owner_id {
            payload["placement"]["owner_id"] = serde_json::json!(owner_id);
        }
        if let Some(context) = context {
            payload["context"] = context;
        }

        let delegate_msg = match router
            .send_message(NewAgentMessage {
                sender_agent_id: sender_agent_id.to_string(),
                recipient_agent_id: target_agent_id.to_string(),
                kind: swage_core::MessageKind::Delegate,
                correlation_id: None,
                payload,
            })
            .await
        {
            Ok(message) => message,
            Err(error) => {
                shutdown_sandbox_guard(
                    &target_sandbox_guard,
                    "failed to shut down delegated sandbox after message creation error",
                )
                .await;
                return Err(error);
            }
        };

        if let Err(error) = router.deliver_message(&delegate_msg).await {
            shutdown_sandbox_guard(
                &target_sandbox_guard,
                "failed to shut down delegated sandbox after message delivery error",
            )
            .await;
            return Err(error);
        }

        match completion_mode {
            DelegateCompletionMode::Wait => {
                let child_cancel_token = cancel_token.child_token();
                let result_msg = match executor
                    .execute_delegation(
                        &delegate_msg,
                        Some(sender_turn_id),
                        Some(sender_session_id),
                        topic_id,
                        target_agent_config,
                        target_context_transfer_config,
                        Some(target_dispatcher),
                        session_metadata,
                        child_cancel_token,
                        None,
                        existing_child,
                        placement
                            .as_ref()
                            .map(|placement| placement.path.to_string_lossy().into_owned()),
                    )
                    .await
                {
                    Ok(result_msg) => result_msg,
                    Err(error) => {
                        shutdown_sandbox_guard(
                            &target_sandbox_guard,
                            "failed to shut down delegated sandbox after delegation error",
                        )
                        .await;
                        let _ = router
                            .fail_message(delegate_msg.id, &error.to_string())
                            .await;
                        return Err(error);
                    }
                };
                shutdown_sandbox_guard(
                    &target_sandbox_guard,
                    "failed to shut down delegated sandbox",
                )
                .await;

                if result_msg.payload.get("error").is_none()
                    && child.is_none()
                    && let Some(owner_id) = worktree_owner_id
                {
                    self.state.worktree_host.cleanup_owner(owner_id).await;
                }

                if let Err(error) = router.deliver_message(&result_msg).await {
                    shutdown_sandbox_guard(
                        &target_sandbox_guard,
                        "failed to shut down delegated sandbox after result delivery error",
                    )
                    .await;
                    return Err(error);
                }
                if let Some(name) = child {
                    let child_id = |field: &str| {
                        result_msg
                            .payload
                            .get(field)
                            .and_then(serde_json::Value::as_str)
                            .and_then(|value| Uuid::parse_str(value).ok())
                    };
                    if let (Some(child_session_id), Some(child_thread_id)) =
                        (child_id("child_session_id"), child_id("child_thread_id"))
                        && let Err(error) = record_named_child(
                            &self.state,
                            sender_session_id,
                            name,
                            target_agent_id,
                            child_session_id,
                            child_thread_id,
                        )
                        .await
                    {
                        // The child answered; losing the name only costs the
                        // caller the ability to come back to it, so the
                        // result still stands.
                        tracing::warn!(%error, child = name, "could not record the named child");
                    }
                }
                Ok(DelegationOutcome::Completed(result_msg.payload.clone()))
            }
            DelegateCompletionMode::Detach => {
                let child_cancel_token = cancel_token.child_token();
                let delegate_message_id = delegate_msg.id;
                let (started_tx, started_rx) = tokio::sync::oneshot::channel();
                let detached_router = router.clone();
                let worktree_host = Arc::clone(&self.state.worktree_host);
                let placement = placement.clone();
                let retain_worktree = child.is_some();

                tokio::spawn(async move {
                    let result = executor
                        .execute_delegation(
                            &delegate_msg,
                            Some(sender_turn_id),
                            Some(sender_session_id),
                            topic_id,
                            target_agent_config,
                            target_context_transfer_config,
                            Some(target_dispatcher),
                            session_metadata,
                            child_cancel_token,
                            Some(started_tx),
                            existing_child,
                            placement
                                .as_ref()
                                .map(|placement| placement.path.to_string_lossy().into_owned()),
                        )
                        .await;

                    shutdown_sandbox_guard(
                        &target_sandbox_guard,
                        "failed to shut down detached delegated sandbox",
                    )
                    .await;

                    match result {
                        // execute_delegation has persisted the Result as Pending.
                        // The parent's next turn owns its delivery: Step 7 appends
                        // it to the parent thread before claiming it Delivered.
                        Ok(result_msg) => {
                            if result_msg.payload.get("error").is_none()
                                && !retain_worktree
                                && let Some(owner_id) = worktree_owner_id
                            {
                                worktree_host.cleanup_owner(owner_id).await;
                            }
                        }
                        Err(error) => {
                            let _ = detached_router
                                .fail_message(delegate_message_id, &error.to_string())
                                .await;
                            tracing::warn!(
                                delegate_message_id = %delegate_message_id,
                                error = ?error,
                                "detached delegation failed"
                            );
                        }
                    }
                });

                let detached = started_rx.await.map_err(|_| {
                    AgentError::InvalidConfig(
                        "detached delegation stopped before child turn startup".to_string(),
                    )
                })?;
                if let Some(name) = child
                    && let Err(error) = record_named_child(
                        &self.state,
                        sender_session_id,
                        name,
                        target_agent_id,
                        detached.child_session_id,
                        detached.child_thread_id,
                    )
                    .await
                {
                    tracing::warn!(%error, child = name, "could not record the named child");
                }
                Ok(DelegationOutcome::Detached(detached))
            }
        }
    }
}

impl GatewayDelegationHandler {
    #[cfg(test)]
    pub(super) async fn active_workspace_attachment_for_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Option<Uuid>, AgentError> {
        Ok(self
            .filesystem_authority_for_turn(turn_id)
            .await?
            .active_workspace_attachment_id)
    }

    async fn filesystem_authority_for_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<DispatchFilesystemAuthority, AgentError> {
        let turn = self.state.storage.get_turn(turn_id).await?.ok_or_else(|| {
            swage_core::StorageError::NotFound {
                entity: "turn".into(),
                id: turn_id.to_string(),
            }
        })?;
        let thread =
            swage_storage::store::ThreadStore::get(self.state.storage.as_ref(), turn.thread_id)
                .await?
                .ok_or(AgentError::ThreadNotFound(turn.thread_id))?;
        let session = self
            .state
            .storage
            .get_session(thread.session_id)
            .await?
            .ok_or(AgentError::SessionNotFound(thread.session_id))?;
        let placement = match session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(WORKER_PLACEMENT_CWD_METADATA_KEY))
            .and_then(serde_json::Value::as_str)
        {
            Some(path) => {
                let path = tokio::fs::canonicalize(path).await.map_err(|error| {
                    AgentError::InvalidConfig(format!(
                        "cannot resolve dispatching worker placement {path:?}: {error}"
                    ))
                })?;
                let authority_path = session
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get(WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY))
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        AgentError::InvalidConfig(
                            "dispatching worker placement is missing its authority path".into(),
                        )
                    })?;
                let authority_context_id = session
                    .metadata
                    .as_ref()
                    .and_then(|metadata| {
                        metadata.get(WORKER_PLACEMENT_AUTHORITY_CONTEXT_METADATA_KEY)
                    })
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .ok_or_else(|| {
                        AgentError::InvalidConfig(
                            "dispatching worker placement is missing its authority context".into(),
                        )
                    })?;
                let authority = resolve_authorised_worker_directory(
                    &self.state,
                    session.topic_id,
                    authority_context_id,
                    Some(session.id),
                    session.active_workspace_attachment_id,
                    None,
                    authority_path,
                )
                .await
                .map_err(|error| {
                    AgentError::InvalidConfig(format!(
                        "dispatching worker placement is no longer authorised: {error}"
                    ))
                })?;
                Some(ResolvedWorkerDirectory {
                    path,
                    can_write: authority.can_write,
                    authority_path: authority.path,
                    authority_context_id,
                })
            }
            None => None,
        };
        Ok(DispatchFilesystemAuthority {
            session_id: session.id,
            active_workspace_attachment_id: session.active_workspace_attachment_id,
            placement,
            workflow_call_tools: session
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get(WORKFLOW_CALL_TOOLS_METADATA_KEY))
                .cloned(),
        })
    }
}

struct DispatchFilesystemAuthority {
    session_id: Uuid,
    active_workspace_attachment_id: Option<Uuid>,
    placement: Option<ResolvedWorkerDirectory>,
    workflow_call_tools: Option<serde_json::Value>,
}
