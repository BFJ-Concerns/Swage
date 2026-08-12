use super::{
    AgentError, AppState, Arc, CancellationToken, DelegationExecutor, DelegationHandler,
    GatewayDispatcherRequest, MessageRouter, NewAgentMessage, Uuid, apply_agent_definition,
    apply_dispatch_bounds, available_skills_for_agent, build_agent_config_for_space_workspace,
    build_approval_registrar, build_assembly_elements,
    build_gateway_dispatcher_for_unattended_execution, build_session_environment,
    delegated_target_instructions_for_space, dispatch_bound_agent_ids_for_turn,
    dispatch_bound_metadata, effective_tool_permissions_record, execute_context_assembly,
    load_active_skills_for_agent, load_memory_for_assembly, load_session_config, load_space_memory,
    mpsc, resolve_swage_root, select_context_assembly, shutdown_sandbox_guard,
};
use swage_agent::{
    DelegatedAssembly, DelegationContextAssembler, DelegationOutcome, DelegationSnapshotRecorder,
};
use swage_context::tokeniser::TiktokenTokeniser;
use swage_core::DelegateCompletionMode;

struct GatewayDelegatedContextAssembler {
    state: AppState,
    space: swage_storage::Space,
    config: swage_config::ResolvedConfig,
    agent_id: String,
    working_directory: String,
    swage_root: Option<std::path::PathBuf>,
    context_event_tx: mpsc::UnboundedSender<crate::websocket::ServerEvent>,
    effective_tool_permissions: serde_json::Value,
}

pub(crate) fn delegated_context_assembler(
    state: AppState,
    space: swage_storage::Space,
    config: swage_config::ResolvedConfig,
    agent_id: String,
    working_directory: String,
    context_event_tx: mpsc::UnboundedSender<crate::websocket::ServerEvent>,
    effective_tool_permissions: serde_json::Value,
) -> Arc<dyn DelegationContextAssembler> {
    Arc::new(GatewayDelegatedContextAssembler {
        state,
        space,
        config,
        agent_id,
        working_directory,
        swage_root: resolve_swage_root(),
        context_event_tx,
        effective_tool_permissions,
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
            self.swage_root.as_deref(),
            false,
            &self.config.declared_skills,
        )
        .await;
        let assembly_name =
            select_context_assembly(&self.state, &self.config, session.id, turn_id).await;
        let memory_content = load_memory_for_assembly(&assembly_name, || async {
            match self.swage_root.as_deref() {
                Some(root) => load_space_memory(root, &self.space.slug).await,
                None => String::new(),
            }
        })
        .await;
        let environment =
            build_session_environment(&self.space, Some(self.working_directory.clone()), session);
        let available_skills = available_skills_for_agent(
            &self.space.slug,
            &self.agent_id,
            self.swage_root.as_deref(),
        );
        let elements = build_assembly_elements(
            &self.config.instructions,
            active_skills,
            delegated_history,
            memory_content.map(|content| (content, self.space.name.clone())),
            environment,
            available_skills,
            self.config.default_model_id(),
        );
        let tokeniser = TiktokenTokeniser::new(self.config.default_model_id());
        let output = execute_context_assembly(
            &assembly_name,
            &self.config,
            &elements,
            &tokeniser,
            self.swage_root.as_deref(),
            &self.space.slug,
            &self.agent_id,
        );
        let mut report = serde_json::to_value(output.report).unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to serialise delegated assembly report");
            serde_json::json!({ "error": "serialisation failed" })
        });
        report["effective_tool_permissions"] = self.effective_tool_permissions.clone();
        let _ = self
            .context_event_tx
            .send(crate::websocket::ServerEvent::ContextAssembled {
                turn_id,
                report: report.clone(),
            });
        Ok(DelegatedAssembly {
            messages: output.messages,
            report,
        })
    }
}

struct GatewayDelegationSnapshotRecorder {
    storage: Arc<dyn swage_storage::Storage>,
}

pub(crate) fn delegation_snapshot_recorder(
    storage: Arc<dyn swage_storage::Storage>,
) -> Arc<dyn DelegationSnapshotRecorder> {
    Arc::new(GatewayDelegationSnapshotRecorder { storage })
}

#[async_trait::async_trait]
impl DelegationSnapshotRecorder for GatewayDelegationSnapshotRecorder {
    async fn record(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        report: &serde_json::Value,
    ) -> Result<(), AgentError> {
        if let Err(error) = self
            .storage
            .store_snapshot(turn_id, &report.to_string())
            .await
        {
            // Reconstructive record (record_durability.rs): record the gap and
            // escalate to a forced turn failure only if that record also fails.
            tracing::warn!(%turn_id, %error, "failed to persist delegated context snapshot");
            if let Err(audit_error) = crate::record_durability::record_reconstruction_gap(
                self.storage.as_ref(),
                Some(session_id),
                turn_id,
                "delegated_context_snapshot",
                &error,
            )
            .await
            {
                if let Err(force_error) = self.storage.force_turn_failure(turn_id).await {
                    tracing::error!(%turn_id, %force_error, "failed to mark delegated turn failed after record loss");
                }
                return Err(AgentError::InvalidConfig(format!(
                    "delegated context snapshot lost and the loss could not be recorded: {audit_error}"
                )));
            }
        }
        Ok(())
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
        context: Option<serde_json::Value>,
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
        let target_resolved = match self
            .state
            .storage
            .resolve_agent_definition(target_agent_id, space_id)
            .await
        {
            Ok(Some(definition)) => apply_agent_definition(target_resolved, &definition),
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

        let mut bound_agent_ids = dispatch_bound_agent_ids_for_turn(&self.state, sender_turn_id)
            .await
            .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        bound_agent_ids.push(sender_agent_id.to_string());
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
        let session_metadata = dispatch_bound_metadata(&bound_agent_ids)
            .expect("a delegated session always has at least its sender bound");

        let active_workspace_attachment_id = self
            .active_workspace_attachment_for_turn(sender_turn_id)
            .await?;
        let target_agent_config = build_agent_config_for_space_workspace(
            &self.state,
            target_agent_id.to_string(),
            &target_resolved,
            None,
            space_id,
            sender_turn_id,
            active_workspace_attachment_id,
        )
        .await;
        let target_instructions = delegated_target_instructions_for_space(
            &target_resolved.instructions,
            &space.slug,
            target_agent_id,
        );
        let target_context_transfer_config = target_resolved.communication.context_transfer.clone();

        // Delegated child agents execute within the gateway runtime, but their
        // internal tool activity is not surfaced on the parent conversation's
        // WebSocket stream. Shared assembly still emits ContextAssembled into
        // this silent channel; the stored child-turn snapshot is the delegated
        // assembly's audit record.
        let (silent_ws_tx, _silent_ws_rx) = mpsc::unbounded_channel();
        let target_dispatcher_build = build_gateway_dispatcher_for_unattended_execution(
            &self.state,
            GatewayDispatcherRequest {
                config: &effective_target_resolved,
                agent_config: &target_agent_config,
                session_metadata: None,
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
            context_assembler: Some(delegated_context_assembler(
                self.state.clone(),
                space.clone(),
                target_resolved.clone(),
                target_agent_id.to_string(),
                target_agent_config.working_directory.display().to_string(),
                silent_ws_tx,
                effective_tool_permissions_record(&effective_target_resolved),
            )),
            snapshot_recorder: Some(delegation_snapshot_recorder(self.state.storage.clone())),
        };

        let mut payload = serde_json::json!({
            "task": task,
        });
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
                        space_id,
                        target_agent_config,
                        target_instructions,
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
                shutdown_sandbox_guard(
                    &target_sandbox_guard,
                    "failed to shut down delegated sandbox",
                )
                .await;

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

                tokio::spawn(async move {
                    let result = executor
                        .execute_delegation(
                            &delegate_msg,
                            Some(sender_turn_id),
                            space_id,
                            target_agent_config,
                            target_instructions,
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
                        Ok(_result_msg) => {}
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
    pub(super) async fn active_workspace_attachment_for_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Option<Uuid>, AgentError> {
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
        Ok(session.active_workspace_attachment_id)
    }
}
