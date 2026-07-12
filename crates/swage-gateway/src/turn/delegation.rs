use super::{
    AgentError, AppState, Arc, CancellationToken, DelegationExecutor, DelegationHandler,
    GatewayDispatcherRequest, MessageRouter, NewAgentMessage, Uuid,
    build_agent_config_for_space_workspace, build_approval_registrar,
    build_gateway_dispatcher_with_unsandboxed_degradation, delegated_target_instructions_for_space,
    load_session_config, mpsc, shutdown_sandbox_guard,
};
use swage_agent::DelegationOutcome;
use swage_core::DelegateCompletionMode;

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
        // WebSocket stream.
        let (silent_ws_tx, _silent_ws_rx) = mpsc::unbounded_channel();
        let target_dispatcher_build = build_gateway_dispatcher_with_unsandboxed_degradation(
            &self.state,
            GatewayDispatcherRequest {
                config: &target_resolved,
                agent_config: &target_agent_config,
                session_metadata: None,
                space_id: Some(space_id),
                active_workspace_attachment_id,
                ws_tx: silent_ws_tx,
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
                        Ok(result_msg) => {
                            if let Err(error) = detached_router.deliver_message(&result_msg).await {
                                tracing::warn!(
                                    delegate_message_id = %delegate_message_id,
                                    result_message_id = %result_msg.id,
                                    error = ?error,
                                    "failed to deliver detached delegate result"
                                );
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
