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
}

pub(crate) struct GatewayDispatcherRequest<'a> {
    pub(crate) config: &'a swage_config::ResolvedConfig,
    pub(crate) agent_config: &'a AgentConfig,
    pub(crate) session_metadata: Option<&'a serde_json::Value>,
    pub(crate) bench_id: Option<Uuid>,
    pub(crate) active_workspace_attachment_id: Option<Uuid>,
    pub(crate) ws_tx: mpsc::UnboundedSender<ServerEvent>,
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
) -> Result<Option<Arc<dyn Sandbox>>, swage_core::SandboxError> {
    if !config.sandbox.enabled {
        return Ok(None);
    }
    if std::env::current_exe().ok().is_some_and(|path| {
        path.parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "deps")
    }) {
        tracing::debug!("skipping per-turn sandbox inside Cargo test harness");
        return Ok(None);
    }
    #[cfg(test)]
    if std::env::var_os("SWAGE_ENABLE_TEST_SANDBOX").is_none() {
        return Ok(None);
    }

    let sandbox_config = build_sandbox_config(agent_config, workspace_policy).map_err(|error| {
        swage_core::SandboxError::SpawnFailed {
            message: format!("failed to build sandbox config: {error}"),
        }
    })?;

    let provider = state.active_provider();
    let llm_handler = HostLlmHandler::new(move |req: swage_core::CompletionRequest| {
        let provider = provider.clone();
        async move {
            provider
                .complete(req)
                .await
                .map_err(|error| swage_core::SandboxError::IpcError {
                    message: error.to_string(),
                })
        }
    });

    let mut sandbox = swage_sandbox::BubblewrapSandbox::new(sandbox_config, llm_handler);
    sandbox.spawn_agent().await?;
    Ok(Some(Arc::new(sandbox) as Arc<dyn Sandbox>))
}

pub(crate) async fn build_gateway_dispatcher(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    agent_config: &AgentConfig,
    session_metadata: Option<&serde_json::Value>,
    bench_id: Option<Uuid>,
    active_workspace_attachment_id: Option<Uuid>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
) -> Result<GatewayDispatcherBuild, swage_core::SandboxError> {
    let workspace_policy = match bench_id {
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
        build_permission_config(config, startup_config, bench_id, &session_approval_grants);

    let base_dispatcher = state.active_tool_dispatcher();
    let dispatcher = ToolDispatcher::new(
        base_dispatcher.registry().clone(),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );

    let sandbox =
        build_turn_sandbox(state, config, agent_config, workspace_policy.as_ref()).await?;
    let sandbox_guard = sandbox
        .as_ref()
        .map(|sandbox| SandboxGuard::new(sandbox.clone()));
    let inner_strategy: Option<Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>> =
        sandbox.as_ref().map(|sandbox| {
            Arc::new(SandboxedExecutor::new(sandbox.clone()))
                as Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>
        });

    let execution_host =
        crate::execution_host::resolve_execution_host(state, session_metadata).await;
    let advertisement_filter =
        build_gateway_advertisement_filter(execution_host.as_ref(), agent_config);
    let dispatcher = match advertisement_filter {
        Some(filter) => dispatcher.with_advertisement_filter(filter),
        None => dispatcher,
    };

    Ok(GatewayDispatcherBuild {
        dispatcher: dispatcher.with_execution_strategy(Arc::new(GatewayExecutionStrategy::new(
            inner_strategy,
            ws_tx,
            execution_host,
            state.secret_manager.clone(),
            state.storage.clone(),
        ))),
        sandbox_guard,
    })
}

pub(crate) async fn build_gateway_dispatcher_with_unsandboxed_degradation(
    state: &AppState,
    request: GatewayDispatcherRequest<'_>,
    execution_context: &'static str,
) -> Result<GatewayDispatcherBuild, swage_core::SandboxError> {
    let GatewayDispatcherRequest {
        config,
        agent_config,
        session_metadata,
        bench_id,
        active_workspace_attachment_id,
        ws_tx,
    } = request;

    match build_gateway_dispatcher(
        state,
        config,
        agent_config,
        session_metadata,
        bench_id,
        active_workspace_attachment_id,
        ws_tx.clone(),
    )
    .await
    {
        Ok(build) => Ok(build),
        Err(error) if config.sandbox.enabled => {
            tracing::warn!(
                error = %error,
                execution_context,
                "sandbox setup failed; degrading execution to unsandboxed dispatcher"
            );

            let mut unsandboxed_config = config.clone();
            unsandboxed_config.sandbox.enabled = false;
            build_gateway_dispatcher(
                state,
                &unsandboxed_config,
                agent_config,
                session_metadata,
                bench_id,
                active_workspace_attachment_id,
                ws_tx,
            )
            .await
        }
        Err(error) => Err(error),
    }
}
