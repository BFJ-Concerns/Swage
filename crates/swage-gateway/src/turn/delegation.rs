use super::{
    AgentError, AppState, Arc, CancellationToken, DelegationExecutor, DelegationHandler,
    DispatchContextComposition, GatewayDispatcherRequest, MessageRouter, NewAgentMessage, Uuid,
    WORKER_PLACEMENT_AUTHORITY_CONTEXT_METADATA_KEY, WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY,
    WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY, WORKER_PLACEMENT_CWD_METADATA_KEY,
    WORKFLOW_CALL_TOOLS_METADATA_KEY, add_dispatch_bound_task_definition_ids,
    apply_agent_definition, apply_dispatch_bounds, apply_dispatch_task_bounds,
    apply_workflow_call_tools, available_skills_for_agent, build_agent_config_for_space_workspace,
    build_approval_registrar, build_assembly_elements,
    build_gateway_dispatcher_for_unattended_execution, build_session_environment,
    configuration_hierarchy, dispatch_bound_agent_ids_for_turn, dispatch_bound_metadata,
    dispatch_bound_task_definition_ids_for_turn, effective_tool_permissions_record,
    execute_context_assembly, load_active_skills_for_agent, load_memory_for_assembly,
    load_session_config, load_space_memory, load_trusted_workspace_policy, mpsc,
    operator_guidance_rules, resolve_authorised_worker_directory, resolve_swage_root,
    select_context_assembly, shutdown_sandbox_guard,
};
use crate::turn::workspace::ResolvedWorkerDirectory;
use sha2::{Digest, Sha256};
use swage_agent::{
    AgentEvent, DelegatedAssembly, DelegationContextAssembler, DelegationOutcome,
    DelegationSnapshotRecorder,
};
use swage_context::tokeniser::TiktokenTokeniser;
use swage_core::DelegateCompletionMode;

pub(crate) fn delegate_worktree_owner(sender_turn_id: Uuid, delegate_call_id: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(sender_turn_id.as_bytes());
    digest.update(delegate_call_id.as_bytes());
    let digest = digest.finalize();
    Uuid::from_bytes(digest[..16].try_into().expect("SHA-256 prefix"))
}

/// Add the host-resolved child directory to the completion the dispatcher
/// receives. The delegate request retains its verbatim requested `cwd`; this
/// is the separately recorded outcome of host-side placement.
async fn record_delegation_working_directory(
    state: &AppState,
    result: &mut swage_storage::AgentMessage,
    placement: Option<&ResolvedWorkerDirectory>,
) -> Result<(), AgentError> {
    let Some(placement) = placement else {
        return Ok(());
    };
    let Some(payload) = result.payload.as_object_mut() else {
        return Ok(());
    };
    payload.insert(
        "working_directory".into(),
        serde_json::Value::String(placement.path.to_string_lossy().into_owned()),
    );
    state
        .storage
        .update_agent_message_payload(result.id, result.payload.clone())
        .await?;
    Ok(())
}

struct GatewayDelegatedContextAssembler {
    state: AppState,
    space: swage_storage::Space,
    config: swage_config::ResolvedConfig,
    agent_id: String,
    working_directory: String,
    swage_root: std::path::PathBuf,
    context_event_tx: mpsc::UnboundedSender<crate::websocket::ServerEvent>,
    effective_tool_permissions: serde_json::Value,
    composition: DispatchContextComposition,
}

pub(crate) fn delegated_context_assembler(
    state: AppState,
    space: swage_storage::Space,
    config: swage_config::ResolvedConfig,
    agent_id: String,
    working_directory: String,
    context_event_tx: mpsc::UnboundedSender<crate::websocket::ServerEvent>,
    effective_tool_permissions: serde_json::Value,
    composition: DispatchContextComposition,
) -> Arc<dyn DelegationContextAssembler> {
    let swage_root = resolve_swage_root(&state).to_path_buf();
    Arc::new(GatewayDelegatedContextAssembler {
        state,
        space,
        config,
        agent_id,
        working_directory,
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
            &self.space.slug,
            &self.agent_id,
            Some(&self.swage_root),
            false,
            &self.config.declared_skills,
        )
        .await;
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
            load_space_memory(&self.swage_root, &self.space.slug).await
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
        let environment =
            build_session_environment(&self.space, Some(self.working_directory.clone()), session);
        let available_skills =
            available_skills_for_agent(&self.space.slug, &self.agent_id, Some(&self.swage_root));
        // Delegated children read the same operator-guidance sources as
        // foreground turns: the trust policy is re-derived here because the
        // assembler holds only the working directory, not the policy.
        let workspace_policy = load_trusted_workspace_policy(
            &self.state.storage,
            self.space.id,
            session.active_workspace_attachment_id,
        )
        .await;
        let operator_guidance = operator_guidance_rules(Some(&self.swage_root), &workspace_policy);
        let hierarchy = configuration_hierarchy(&self.swage_root, &self.space.slug, &self.agent_id);
        let elements = build_assembly_elements(
            &self.config.instructions,
            operator_guidance,
            active_skills,
            delegated_history,
            memory_content.map(|content| (content, self.space.name.clone())),
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
            &self.space.slug,
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
        match event {
            AgentEvent::ProviderRequestPrepared { round, request, .. } => {
                match super::redact_provider_visible_request(&self.state, request).await {
                    Ok(request) => {
                        super::append_provider_visible_request(&mut report, round, request)
                    }
                    Err(error) => {
                        tracing::error!(%turn_id, %round, %error, "delegated provider-visible request capture omitted because secret redaction was unavailable");
                        super::append_provider_visible_capture_error(&mut report, round);
                    }
                }
            }
            AgentEvent::ProviderResponseCaptured { round, witness, .. } => {
                super::append_provider_visible_response(&mut report, round, witness);
            }
            AgentEvent::ProviderReasoningCaptured {
                round,
                content,
                details,
                ..
            } => {
                super::append_provider_visible_reasoning(&mut report, round, content, details);
            }
            _ => unreachable!("non-diagnostic events returned above"),
        }
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
        composition: DispatchContextComposition,
        delegate_call_id: &str,
        sender_agent_id: &str,
        sender_turn_id: Uuid,
        space_id: Uuid,
        cancel_token: CancellationToken,
        completion_mode: DelegateCompletionMode,
    ) -> Result<DelegationOutcome, AgentError> {
        let space = self.state.space_store.get(space_id).await?.ok_or_else(|| {
            AgentError::InvalidConfig(format!("delegation space {space_id} no longer exists"))
        })?;

        let target_resolved =
            load_session_config(&self.state, &space.slug, target_agent_id).map_err(|error| {
                AgentError::InvalidConfig(format!(
                    "failed to load config for delegated agent '{target_agent_id}' in space '{}': {error}",
                    space.slug
                ))
            })?;
        let target_resolved = match crate::service::resolve_effective_agent_definition(
            self.state.storage.as_ref(),
            target_agent_id,
            space_id,
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
        let target_resolved = match task_definition {
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

        let mut bound_agent_ids = dispatch_bound_agent_ids_for_turn(&self.state, sender_turn_id)
            .await
            .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        bound_agent_ids.push(sender_agent_id.to_string());
        let bound_task_definition_ids =
            dispatch_bound_task_definition_ids_for_turn(&self.state, sender_turn_id)
                .await
                .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        let sender_filesystem_authority =
            self.filesystem_authority_for_turn(sender_turn_id).await?;
        let mut session_metadata = dispatch_bound_metadata(&bound_agent_ids)
            .expect("a delegated session always has at least its sender bound");
        add_dispatch_bound_task_definition_ids(&mut session_metadata, &bound_task_definition_ids);
        if let Some(workflow_call_tools) = &sender_filesystem_authority.workflow_call_tools {
            session_metadata[WORKFLOW_CALL_TOOLS_METADATA_KEY] = workflow_call_tools.clone();
        }
        if let Some(task_definition) = task_definition {
            session_metadata["task_definition"] =
                serde_json::Value::String(task_definition.to_string());
        }
        let effective_target_resolved = apply_dispatch_bounds(
            &self.state,
            &space.slug,
            space_id,
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

        let active_workspace_attachment_id =
            sender_filesystem_authority.active_workspace_attachment_id;
        let sender_session_id = sender_filesystem_authority.session_id;
        let mut target_agent_config = build_agent_config_for_space_workspace(
            &self.state,
            target_agent_id.to_string(),
            &target_resolved,
            None,
            space_id,
            sender_turn_id,
            active_workspace_attachment_id,
        )
        .await;
        let mut worktree_owner_id = None;
        let placement = match isolation {
            None => match cwd.as_deref() {
                Some(cwd) => Some(
                    resolve_authorised_worker_directory(
                        &self.state,
                        space_id,
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
                    space_id,
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
                        .map_err(AgentError::InvalidConfig)?,
                    can_write: repo.can_write,
                    authority_path: repo.authority_path,
                    authority_context_id: repo.authority_context_id,
                })
            }
        };
        if let Some(placement) = placement.as_ref() {
            target_agent_config = target_agent_config
                .with_workspace(placement.path.clone(), vec![placement.path.clone()]);
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
                space_id: Some(space_id),
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
                space.clone(),
                target_resolved.clone(),
                target_agent_id.to_string(),
                target_agent_config.working_directory.display().to_string(),
                silent_ws_tx,
                effective_tool_permissions_record(&effective_target_resolved),
                composition.clone(),
            ),
            snapshot_recorder: Some(delegation_snapshot_recorder_for_state(self.state.clone())),
        };

        let mut payload = serde_json::to_value(composition).map_err(|error| {
            AgentError::InvalidConfig(format!(
                "failed to encode delegated context composition: {error}"
            ))
        })?;
        payload["task"] = serde_json::json!(task);
        payload["task_definition"] = serde_json::json!(task_definition);
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
                let mut result_msg = match executor
                    .execute_delegation(
                        &delegate_msg,
                        Some(sender_turn_id),
                        space_id,
                        target_agent_config,
                        target_context_transfer_config,
                        Some(target_dispatcher),
                        session_metadata,
                        child_cancel_token,
                        None,
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
                if let Err(error) = record_delegation_working_directory(
                    &self.state,
                    &mut result_msg,
                    placement.as_ref(),
                )
                .await
                {
                    tracing::warn!(
                        delegate_message_id = %delegate_msg.id,
                        error = ?error,
                        "failed to record delegated working directory"
                    );
                    // The child turn has already completed successfully.
                    // Keep its result deliverable when the supplementary
                    // audit annotation cannot be persisted.
                }
                shutdown_sandbox_guard(
                    &target_sandbox_guard,
                    "failed to shut down delegated sandbox",
                )
                .await;

                if result_msg.payload.get("error").is_none()
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
                Ok(DelegationOutcome::Completed(result_msg.payload.clone()))
            }
            DelegateCompletionMode::Detach => {
                let child_cancel_token = cancel_token.child_token();
                let delegate_message_id = delegate_msg.id;
                let (started_tx, started_rx) = tokio::sync::oneshot::channel();
                let detached_router = router.clone();
                let worktree_host = Arc::clone(&self.state.worktree_host);
                let state = self.state.clone();
                let placement = placement.clone();

                tokio::spawn(async move {
                    let result = executor
                        .execute_delegation(
                            &delegate_msg,
                            Some(sender_turn_id),
                            space_id,
                            target_agent_config,
                            target_context_transfer_config,
                            Some(target_dispatcher),
                            session_metadata,
                            child_cancel_token,
                            Some(started_tx),
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
                        Ok(mut result_msg) => {
                            if let Err(error) = record_delegation_working_directory(
                                &state,
                                &mut result_msg,
                                placement.as_ref(),
                            )
                            .await
                            {
                                tracing::warn!(
                                    delegate_message_id = %delegate_message_id,
                                    error = ?error,
                                    "failed to record detached delegation working directory"
                                );
                                // Recording the supplementary directory must
                                // not turn a completed child turn into a
                                // failed delegation result.
                            }
                            if result_msg.payload.get("error").is_none()
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
                    session.space_id,
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
