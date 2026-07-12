use super::{
    AgentConfig, AppState, Arc, GatewayExecutionStrategy, HostLlmHandler, LeakDetector, Mutex,
    Path, PathBuf, RateLimitConfig, RateLimiter, Sandbox, SandboxedExecutor, ServerEvent, Tool,
    ToolDispatcher, TrustedWorkspacePolicy, Uuid, build_gateway_advertisement_filter,
    build_permission_config, load_trusted_workspace_policy, mpsc,
};

pub(super) type GatewayToolAdvertisementFilter = dyn Fn(&dyn Tool) -> bool + Send + Sync;
pub(crate) fn build_approval_registrar(
    approval_channels: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                Uuid,
                tokio::sync::oneshot::Sender<swage_agent::ApprovalDecision>,
            >,
        >,
    >,
) -> swage_agent::ApprovalChannelRegistrar {
    Arc::new(move |approval_id, sender| {
        approval_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(approval_id, sender);
    })
}

pub(crate) struct SandboxGuard {
    sandbox: Arc<dyn Sandbox>,
    shutdown_complete: Mutex<bool>,
}

impl SandboxGuard {
    pub(super) fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self {
            sandbox,
            shutdown_complete: Mutex::new(false),
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), swage_core::SandboxError> {
        let mut shutdown_complete = self.shutdown_complete.lock().await;
        if *shutdown_complete {
            return Ok(());
        }
        self.sandbox.shutdown().await?;
        *shutdown_complete = true;
        Ok(())
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        if !*self.shutdown_complete.get_mut() {
            tracing::warn!("sandbox guard dropped without explicit async shutdown");
        }
    }
}

pub(crate) async fn shutdown_sandbox_guard(guard: &Option<SandboxGuard>, context: &'static str) {
    if let Some(guard) = guard
        && let Err(error) = guard.shutdown().await
    {
        tracing::warn!(error = %error, "{context}");
    }
}

pub(crate) struct GatewayDispatcherBuild {
    pub(crate) dispatcher: ToolDispatcher,
    pub(crate) sandbox_guard: Option<SandboxGuard>,
    pub(crate) isolation: ExecutionIsolation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutionIsolation {
    Isolated,
    Degraded { reason: String },
}

pub(crate) struct GatewayDispatcherRequest<'a> {
    pub(crate) config: &'a swage_config::ResolvedConfig,
    pub(crate) agent_config: &'a AgentConfig,
    pub(crate) session_metadata: Option<&'a serde_json::Value>,
    pub(crate) space_id: Option<Uuid>,
    pub(crate) active_workspace_attachment_id: Option<Uuid>,
    pub(crate) ws_tx: mpsc::UnboundedSender<ServerEvent>,
    pub(crate) session_id: Option<Uuid>,
    pub(crate) turn_id: Option<Uuid>,
}

pub(super) struct TurnSandboxBuild {
    sandbox: Option<Arc<dyn Sandbox>>,
    isolation: ExecutionIsolation,
}

fn require_degraded_execution_opt_in(
    config: &swage_config::ResolvedConfig,
) -> Result<(), swage_core::SandboxError> {
    if config.sandbox.allow_degraded_execution {
        Ok(())
    } else {
        Err(swage_core::SandboxError::Unavailable {
            message: "degraded execution requires sandbox.allow_degraded_execution = true".into(),
        })
    }
}

pub(super) fn sandbox_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn build_sandbox_config(
    agent_config: &AgentConfig,
    workspace_policy: Option<&TrustedWorkspacePolicy>,
) -> Result<swage_sandbox::SandboxConfig, swage_sandbox::SandboxConfigError> {
    let working_directory = sandbox_path(&agent_config.working_directory);
    let mut builder = swage_sandbox::SandboxConfig::builder().working_directory(&working_directory);

    let active_grant = workspace_policy
        .and_then(|policy| policy.active_workspace.as_ref())
        .filter(|grant| sandbox_path(&grant.path) == working_directory);
    let active_can_write = active_grant.is_none_or(|grant| grant.can_write);
    if active_can_write {
        builder = builder
            .read_write_mount(&working_directory)
            .landlock_write_path(&working_directory);
    } else {
        builder = builder
            .read_only_mount(&working_directory)
            .landlock_read_path(&working_directory);
    }

    if let Some(policy) = workspace_policy {
        for grant in &policy.attachments {
            let path = sandbox_path(&grant.path);
            match path.try_exists() {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(
                        path = %grant.path.display(),
                        "skipping missing trusted workspace path in sandbox config"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        path = %grant.path.display(),
                        %error,
                        "skipping trusted workspace path that could not be checked for sandbox config"
                    );
                    continue;
                }
            }
            if grant.can_write {
                builder = builder.read_write_mount(&path).landlock_write_path(&path);
            } else if grant.can_read {
                builder = builder.read_only_mount(&path).landlock_read_path(&path);
            }
        }
    }

    builder.build()
}

pub(super) async fn build_turn_sandbox(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    agent_config: &AgentConfig,
    workspace_policy: Option<&TrustedWorkspacePolicy>,
) -> Result<TurnSandboxBuild, swage_core::SandboxError> {
    if !config.sandbox.enabled {
        require_degraded_execution_opt_in(config)?;
        return Ok(TurnSandboxBuild {
            sandbox: None,
            isolation: ExecutionIsolation::Degraded {
                reason: "sandbox_disabled_by_configuration".into(),
            },
        });
    }
    if std::env::current_exe().ok().is_some_and(|path| {
        path.parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "deps")
    }) {
        tracing::debug!("skipping per-turn sandbox inside Cargo test harness");
        return Ok(TurnSandboxBuild {
            sandbox: None,
            isolation: ExecutionIsolation::Isolated,
        });
    }
    #[cfg(test)]
    if std::env::var_os("SWAGE_ENABLE_TEST_SANDBOX").is_none() {
        return Ok(TurnSandboxBuild {
            sandbox: None,
            isolation: ExecutionIsolation::Isolated,
        });
    }

    let sandbox_config = build_sandbox_config(agent_config, workspace_policy).map_err(|error| {
        swage_core::SandboxError::SpawnFailed {
            message: format!("failed to build sandbox config: {error}"),
        }
    })?;

    let provider = state.active_provider();
    let make_llm_handler = || {
        let provider = provider.clone();
        HostLlmHandler::new(move |req: swage_core::CompletionRequest| {
            let provider = provider.clone();
            async move {
                provider
                    .complete(req)
                    .await
                    .map_err(|error| swage_core::SandboxError::IpcError {
                        message: error.to_string(),
                    })
            }
        })
    };

    let degradation_reason;
    if swage_sandbox::find_bwrap().is_ok() {
        let mut sandbox =
            swage_sandbox::BubblewrapSandbox::new(sandbox_config.clone(), make_llm_handler());
        match sandbox.spawn_agent().await {
            Ok(()) => {
                return Ok(TurnSandboxBuild {
                    sandbox: Some(Arc::new(sandbox) as Arc<dyn Sandbox>),
                    isolation: ExecutionIsolation::Isolated,
                });
            }
            Err(error) if !config.sandbox.allow_degraded_execution => return Err(error),
            Err(error) => {
                degradation_reason = format!("sandbox_setup_failed: {error}");
                tracing::warn!(error = %error, "sandbox setup failed; using explicitly permitted degraded execution")
            }
        }
    } else {
        require_degraded_execution_opt_in(config)?;
        degradation_reason = "bubblewrap_unavailable".into();
    }

    let mut fallback = swage_sandbox::FallbackSandbox::new(sandbox_config, make_llm_handler());
    fallback.spawn_agent().await?;
    Ok(TurnSandboxBuild {
        sandbox: Some(Arc::new(fallback) as Arc<dyn Sandbox>),
        isolation: ExecutionIsolation::Degraded {
            reason: degradation_reason,
        },
    })
}

pub(crate) async fn build_gateway_dispatcher(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    agent_config: &AgentConfig,
    session_metadata: Option<&serde_json::Value>,
    space_id: Option<Uuid>,
    active_workspace_attachment_id: Option<Uuid>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
) -> Result<GatewayDispatcherBuild, swage_core::SandboxError> {
    let workspace_policy = match space_id {
        Some(id) => Some(
            load_trusted_workspace_policy(&state.storage, id, active_workspace_attachment_id).await,
        ),
        None => None,
    };
    let session_approval_grants = state
        .session_approval_grants
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let startup_config = &state.config;
    let permissions =
        build_permission_config(config, startup_config, space_id, &session_approval_grants);

    let base_dispatcher = state.active_tool_dispatcher();
    let dispatcher = ToolDispatcher::new(
        base_dispatcher.registry().clone(),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );

    let sandbox_build =
        build_turn_sandbox(state, config, agent_config, workspace_policy.as_ref()).await?;
    let sandbox = sandbox_build.sandbox;
    let sandbox_guard = sandbox
        .as_ref()
        .map(|sandbox| SandboxGuard::new(sandbox.clone()));
    let inner_strategy: Option<Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>> =
        sandbox.as_ref().map(|sandbox| {
            Arc::new(SandboxedExecutor::new(sandbox.clone()))
                as Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>
        });

    let device = crate::device::resolve_device(state, session_metadata).await;
    let advertisement_filter = build_gateway_advertisement_filter(device.as_ref(), agent_config);
    let dispatcher = match advertisement_filter {
        Some(filter) => dispatcher.with_advertisement_filter(filter),
        None => dispatcher,
    };

    Ok(GatewayDispatcherBuild {
        dispatcher: dispatcher.with_execution_strategy(Arc::new(GatewayExecutionStrategy::new(
            inner_strategy,
            ws_tx,
            device,
            state.secret_manager.clone(),
            state.storage.clone(),
        ))),
        sandbox_guard,
        isolation: sandbox_build.isolation,
    })
}

pub(crate) async fn record_degraded_execution(
    state: &AppState,
    isolation: &ExecutionIsolation,
    execution_context: &'static str,
    session_id: Option<Uuid>,
    turn_id: Option<Uuid>,
) -> Result<(), swage_core::GatewayError> {
    let ExecutionIsolation::Degraded { reason } = isolation else {
        return Ok(());
    };
    let (entity_kind, entity_id) = turn_id.map_or_else(
        || {
            (
                "session",
                session_id.map_or_else(|| execution_context.into(), |id| id.to_string()),
            )
        },
        |id| ("turn", id.to_string()),
    );
    state
        .storage
        .create_audit_event(swage_storage::NewAuditEvent {
            id: format!("sandbox-degradation-{}", Uuid::new_v4()),
            session_id,
            entity_kind: entity_kind.into(),
            entity_id,
            event_kind: "execution_isolation_degraded".into(),
            summary: "Execution isolation was degraded".into(),
            metadata: serde_json::json!({
                "execution_context": execution_context,
                "reason": reason,
            }),
        })
        .await?;
    Ok(())
}

pub(crate) async fn build_gateway_dispatcher_for_unattended_execution(
    state: &AppState,
    request: GatewayDispatcherRequest<'_>,
    execution_context: &'static str,
) -> Result<GatewayDispatcherBuild, swage_core::GatewayError> {
    let GatewayDispatcherRequest {
        config,
        agent_config,
        session_metadata,
        space_id,
        active_workspace_attachment_id,
        ws_tx,
        session_id,
        turn_id,
    } = request;

    let build = build_gateway_dispatcher(
        state,
        config,
        agent_config,
        session_metadata,
        space_id,
        active_workspace_attachment_id,
        ws_tx.clone(),
    )
    .await
    .map_err(|error| swage_core::GatewayError::ConfigError(error.to_string()))?;
    record_degraded_execution(
        state,
        &build.isolation,
        execution_context,
        session_id,
        turn_id,
    )
    .await?;
    Ok(build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::tests::make_state_with_session;

    #[test]
    fn degraded_execution_is_denied_by_default() {
        let config = swage_config::ResolvedConfig::default();
        let error = require_degraded_execution_opt_in(&config)
            .expect_err("degraded execution must fail closed without an opt-in");
        assert!(
            error
                .to_string()
                .contains("allow_degraded_execution = true")
        );
    }

    #[test]
    fn degraded_execution_requires_explicit_opt_in() {
        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.allow_degraded_execution = true;
        require_degraded_execution_opt_in(&config)
            .expect("the conspicuous config opt-in should permit degradation");
    }

    #[tokio::test]
    async fn degraded_execution_is_recorded_in_the_unified_audit_trail() {
        let (state, session_id) = make_state_with_session(Vec::new()).await;
        let turn_id = Uuid::new_v4();
        record_degraded_execution(
            &state,
            &ExecutionIsolation::Degraded {
                reason: "fallback_sandbox_without_namespace_isolation".into(),
            },
            "scheduled turn",
            Some(session_id),
            Some(turn_id),
        )
        .await
        .expect("degradation audit should be durable before execution proceeds");

        let events = state.storage.list_audit_events().await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].entity_kind, "turn");
        assert_eq!(events[0].entity_id, turn_id.to_string());
        assert_eq!(events[0].event_kind, "execution_isolation_degraded");
        assert_eq!(events[0].metadata["execution_context"], "scheduled turn");
    }
}
