use super::{
    AgentConfig, AppState, Arc, GatewayExecutionStrategy, HostLlmHandler, LeakDetector, Mutex,
    Path, PathBuf, RateLimitConfig, RateLimiter, Sandbox, SandboxedExecutor, ServerEvent, Tool,
    ToolDispatcher, TrustedWorkspacePath, TrustedWorkspacePolicy, Uuid,
    build_gateway_advertisement_filter, build_permission_config, load_trusted_workspace_policy,
    mpsc, worker_placement_metadata_present,
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
    backend: SandboxBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxBackend {
    None,
    Bubblewrap,
    Fallback,
    TestHarness,
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

pub(crate) fn build_sandbox_config(
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

pub(crate) fn apply_worker_placement_permission(
    policy: &mut TrustedWorkspacePolicy,
    agent_config: &AgentConfig,
    session_metadata: Option<&serde_json::Value>,
) {
    if !worker_placement_metadata_present(session_metadata) {
        return;
    }
    let can_write = session_metadata
        .and_then(|metadata| metadata.get("worker_placement_can_write"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    policy.active_workspace = Some(TrustedWorkspacePath {
        path: agent_config.working_directory.clone(),
        is_trusted: true,
        can_read: true,
        can_write,
    });
    policy.attachments.clear();
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
            backend: SandboxBackend::None,
        });
    }
    #[cfg(test)]
    if state.test_sandbox_enabled() {
        let config = build_sandbox_config(agent_config, workspace_policy).map_err(|error| {
            swage_core::SandboxError::SpawnFailed {
                message: format!("failed to build test sandbox config: {error}"),
            }
        })?;
        return Ok(TurnSandboxBuild {
            sandbox: Some(Arc::new(PolicyEnforcingTestSandbox { config })),
            isolation: ExecutionIsolation::Isolated,
            backend: SandboxBackend::TestHarness,
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
            backend: SandboxBackend::TestHarness,
        });
    }
    #[cfg(test)]
    if std::env::var_os("SWAGE_ENABLE_TEST_SANDBOX").is_none() {
        return Ok(TurnSandboxBuild {
            sandbox: None,
            isolation: ExecutionIsolation::Isolated,
            backend: SandboxBackend::TestHarness,
        });
    }

    build_turn_sandbox_for_backend_selection(
        state,
        config,
        agent_config,
        workspace_policy,
        swage_sandbox::find_bwrap().is_ok(),
        true,
    )
    .await
}

#[cfg(test)]
struct PolicyEnforcingTestSandbox {
    config: swage_sandbox::SandboxConfig,
}

#[cfg(test)]
#[async_trait::async_trait]
impl Sandbox for PolicyEnforcingTestSandbox {
    async fn spawn_agent(&mut self) -> Result<(), swage_core::SandboxError> {
        Ok(())
    }

    async fn execute_tool(
        &self,
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<swage_sandbox::ToolCallResult, swage_core::SandboxError> {
        let path = params
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(|path| self.config.working_directory.join(path));
        let result = match (tool_name, path) {
            ("file_read", Some(path)) => std::fs::read_to_string(path)
                .map(|content| serde_json::json!({"output": content}))
                .map_err(|error| error.to_string()),
            ("file_write", Some(path)) => {
                if !self
                    .config
                    .read_write_mounts
                    .contains(&self.config.working_directory)
                {
                    Err("write refused by read-only sandbox mount".into())
                } else {
                    let content = params
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    std::fs::write(path, content)
                        .map(|()| serde_json::json!({"output": "write completed"}))
                        .map_err(|error| error.to_string())
                }
            }
            _ => Err(format!("unsupported test sandbox tool {tool_name}")),
        };
        Ok(match result {
            Ok(output) => swage_sandbox::ToolCallResult {
                output,
                duration_ms: 0,
                error: None,
            },
            Err(error) => swage_sandbox::ToolCallResult {
                output: serde_json::Value::Null,
                duration_ms: 0,
                error: Some(error),
            },
        })
    }

    async fn shutdown(&self) -> Result<(), swage_core::SandboxError> {
        Ok(())
    }
}

async fn build_turn_sandbox_for_backend_selection(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    agent_config: &AgentConfig,
    workspace_policy: Option<&TrustedWorkspacePolicy>,
    bubblewrap_available: bool,
    spawn_fallback: bool,
) -> Result<TurnSandboxBuild, swage_core::SandboxError> {
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
    if bubblewrap_available {
        let mut sandbox =
            swage_sandbox::BubblewrapSandbox::new(sandbox_config.clone(), make_llm_handler());
        match sandbox.spawn_agent().await {
            Ok(()) => {
                return Ok(TurnSandboxBuild {
                    sandbox: Some(Arc::new(sandbox) as Arc<dyn Sandbox>),
                    isolation: ExecutionIsolation::Isolated,
                    backend: SandboxBackend::Bubblewrap,
                });
            }
            Err(error) if !config.sandbox.allow_degraded_execution => {
                tracing::warn!(
                    error = %error,
                    "sandbox setup failed; refusing degraded execution without explicit permission"
                );
                return Err(error);
            }
            Err(error) => {
                degradation_reason = format!("sandbox_setup_failed: {error}");
                tracing::warn!(error = %error, "sandbox setup failed; using explicitly permitted degraded execution")
            }
        }
    } else {
        if let Err(error) = require_degraded_execution_opt_in(config) {
            tracing::warn!(
                error = %error,
                reason = "bubblewrap_unavailable",
                "sandbox unavailable; refusing degraded execution without explicit permission"
            );
            return Err(error);
        }
        degradation_reason = "bubblewrap_unavailable".into();
    }

    let mut fallback = swage_sandbox::FallbackSandbox::new(sandbox_config, make_llm_handler());
    if spawn_fallback {
        fallback.spawn_agent().await?;
    }
    Ok(TurnSandboxBuild {
        sandbox: Some(Arc::new(fallback) as Arc<dyn Sandbox>),
        isolation: ExecutionIsolation::Degraded {
            reason: degradation_reason,
        },
        backend: SandboxBackend::Fallback,
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
    #[cfg(test)]
    if state.take_dispatcher_build_failure() {
        return Err(swage_core::SandboxError::SpawnFailed {
            message: "injected dispatcher build failure".into(),
        });
    }

    let mut workspace_policy = match space_id {
        Some(id) => Some(
            load_trusted_workspace_policy(&state.storage, id, active_workspace_attachment_id).await,
        ),
        None => None,
    };
    if let Some(policy) = workspace_policy.as_mut() {
        apply_worker_placement_permission(policy, agent_config, session_metadata);
    }
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
    tracing::debug!(backend = ?sandbox_build.backend, "selected turn sandbox backend");
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
            Some(state.clone()),
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
    use crate::turn::tests::{MockStorage, make_state_with_session, make_state_with_storage};

    fn codex_config() -> swage_config::ResolvedConfig {
        let mut config = swage_config::ResolvedConfig::default();
        config.providers.entries.insert(
            "codex".into(),
            swage_config::ProviderEndpointConfig {
                protocol: swage_config::Protocol::Codex,
                ..Default::default()
            },
        );
        config.models.default = "codex".into();
        config.models.entries.insert(
            "codex".into(),
            swage_config::ModelEndpointConfig {
                provider_id: "codex".into(),
                model_id: "fake-codex".into(),
                ..Default::default()
            },
        );
        config
    }

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

    #[tokio::test]
    async fn disabled_gateway_sandbox_remains_degraded() {
        let (state, _session_id) = make_state_with_session(Vec::new()).await;
        let workspace = tempfile::tempdir().unwrap();
        let agent_config = AgentConfig::new("gateway")
            .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.enabled = false;
        config.sandbox.allow_degraded_execution = true;

        let build = build_turn_sandbox(&state, &config, &agent_config, None)
            .await
            .expect("explicitly opted-in non-Codex execution should remain available");

        assert_eq!(
            build.isolation,
            ExecutionIsolation::Degraded {
                reason: "sandbox_disabled_by_configuration".into(),
            }
        );
    }

    #[tokio::test]
    async fn unavailable_bubblewrap_selects_and_records_opted_in_fallback_sandbox() {
        let (state, session_id) = make_state_with_session(Vec::new()).await;
        let workspace = tempfile::tempdir().unwrap();
        let agent_config = AgentConfig::new("gateway")
            .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.allow_degraded_execution = true;

        // Do not spawn the child here: Cargo's unit-test binary is not the
        // sandbox agent entrypoint. Backend construction and selection are the
        // behaviour under test; FallbackSandbox's real child process has its
        // own integration coverage in sandbox_entrypoint_integration.rs.
        let build = build_turn_sandbox_for_backend_selection(
            &state,
            &config,
            &agent_config,
            None,
            false,
            false,
        )
        .await
        .expect("the explicit opt-in should select the fallback sandbox");
        assert_eq!(build.backend, SandboxBackend::Fallback);
        assert!(build.sandbox.is_some());
        assert_eq!(
            build.isolation,
            ExecutionIsolation::Degraded {
                reason: "bubblewrap_unavailable".into(),
            }
        );

        let turn_id = Uuid::new_v4();
        record_degraded_execution(
            &state,
            &build.isolation,
            "scheduled turn",
            Some(session_id),
            Some(turn_id),
        )
        .await
        .expect("fallback selection should be recorded before execution");
        let events = state.storage.list_audit_events().await.unwrap();
        assert!(events.iter().any(|event| {
            event.entity_id == turn_id.to_string()
                && event.metadata["reason"] == "bubblewrap_unavailable"
        }));
    }

    #[tokio::test]
    async fn unattended_degraded_dispatcher_fails_closed_when_audit_write_fails() {
        let storage = Arc::new(MockStorage::new());
        let (state, session_id) = make_state_with_storage(Vec::new(), storage.clone()).await;
        storage.fail_audit_writes();

        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.enabled = false;
        config.sandbox.allow_degraded_execution = true;
        let workspace = tempfile::tempdir().unwrap();
        let agent_config = AgentConfig::new("gateway")
            .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();

        let result = build_gateway_dispatcher_for_unattended_execution(
            &state,
            GatewayDispatcherRequest {
                config: &config,
                agent_config: &agent_config,
                session_metadata: None,
                space_id: None,
                active_workspace_attachment_id: None,
                ws_tx,
                session_id: Some(session_id),
                turn_id: Some(Uuid::new_v4()),
            },
            "scheduled turn",
        )
        .await;
        let Err(error) = result else {
            panic!("degraded execution must not receive a dispatcher without its audit record");
        };

        assert!(error.to_string().contains("injected audit write failure"));
    }

    #[tokio::test]
    async fn gateway_runtime_codex_dispatcher_requires_degraded_execution_opt_in() {
        let (state, session_id) = make_state_with_session(Vec::new()).await;
        let workspace = tempfile::tempdir().unwrap();
        let agent_config = AgentConfig::new("gateway")
            .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
        let mut config = codex_config();
        config.sandbox.enabled = false;
        assert!(!config.sandbox.allow_degraded_execution);
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();

        let result = build_gateway_dispatcher_for_unattended_execution(
            &state,
            GatewayDispatcherRequest {
                config: &config,
                agent_config: &agent_config,
                session_metadata: None,
                space_id: None,
                active_workspace_attachment_id: None,
                ws_tx,
                session_id: Some(session_id),
                turn_id: Some(Uuid::new_v4()),
            },
            "delegated turn",
        )
        .await;

        let Err(error) = result else {
            panic!("gateway-runtime Codex execution must not inherit the App Server policy bypass");
        };
        assert!(
            error
                .to_string()
                .contains("allow_degraded_execution = true")
        );
        assert!(state.storage.list_audit_events().await.unwrap().is_empty());
    }
}
