use super::*;

#[derive(Clone)]
pub(crate) struct GatewayDelegationHandler {
    state: AppState,
    active_delegations: swage_agent::ActiveDelegations,
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
        bench_id: Uuid,
        cancel_token: CancellationToken,
    ) -> Result<serde_json::Value, AgentError> {
        let bench = self.state.bench_store.get(bench_id).await?.ok_or_else(|| {
            AgentError::InvalidConfig(format!("delegation bench {bench_id} no longer exists"))
        })?;

        let target_resolved =
            load_session_config(&self.state, &bench.slug, target_agent_id).map_err(|error| {
                AgentError::InvalidConfig(format!(
                    "failed to load config for delegated agent '{target_agent_id}' in bench '{}': {error}",
                    bench.slug
                ))
            })?;

        let active_workspace_attachment_id = self
            .active_workspace_attachment_for_turn(sender_turn_id)
            .await?;
        let target_agent_config = build_agent_config_for_bench_workspace(
            &self.state,
            target_agent_id.to_string(),
            &target_resolved,
            None,
            bench_id,
            sender_turn_id,
            active_workspace_attachment_id,
        )
        .await;
        let target_instructions = delegated_target_instructions_for_bench(
            &target_resolved.instructions,
            &bench.slug,
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
                bench_id: Some(bench_id),
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

        let child_cancel_token = cancel_token.child_token();
        let result_msg = match executor
            .execute_delegation(
                &delegate_msg,
                Some(sender_turn_id),
                bench_id,
                target_agent_config,
                target_instructions,
                target_context_transfer_config,
                Some(target_dispatcher),
                child_cancel_token,
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
        Ok(result_msg.payload.clone())
    }
}

impl GatewayDelegationHandler {
    async fn active_workspace_attachment_for_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Option<Uuid>, AgentError> {
        let Some(turn) = self.state.storage.get_turn(turn_id).await? else {
            return Ok(None);
        };
        let Some(thread) =
            swage_storage::store::ThreadStore::get(self.state.storage.as_ref(), turn.thread_id)
                .await?
        else {
            return Ok(None);
        };
        let Some(session) = self.state.storage.get_session(thread.session_id).await? else {
            return Ok(None);
        };
        Ok(session.active_workspace_attachment_id)
    }
}
