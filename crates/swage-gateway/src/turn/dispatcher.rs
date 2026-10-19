use super::{
    AgentConfig, AppState, Arc, GatewayExecutionStrategy, HostLlmHandler, LeakDetector, Mutex,
    Path, PathBuf, RateLimitConfig, RateLimiter, Sandbox, SandboxedExecutor, ServerEvent, Tool,
    ToolDispatcher, TrustedWorkspacePath, TrustedWorkspacePolicy, Uuid,
    WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY, build_gateway_advertisement_filter,
    build_permission_config, load_trusted_workspace_policy, mpsc,
    worker_placement_metadata_present,
};

pub(super) type GatewayToolAdvertisementFilter = dyn Fn(&dyn Tool) -> bool + Send + Sync;
pub(crate) fn build_approval_registrar(
    approval_channels: Arc<
        std::sync::Mutex<std::collections::HashMap<Uuid, crate::state::ApprovalWaiter>>,
    >,
) -> swage_agent::ApprovalChannelRegistrar {
    Arc::new(move |approval_id, sender| {
        approval_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(approval_id, sender.into());
        let channels = Arc::clone(&approval_channels);
        swage_agent::approval::ApprovalChannelRegistration::new(move || {
            channels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&approval_id);
        })
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

/// Where a turn's tools run. `Declared` is the boundary the install states
/// for every turn (`AppState::declared_execution_boundary`): the turn is
/// neither sandboxed per turn nor degraded, and its record says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutionIsolation {
    Isolated,
    Degraded { reason: String },
    Declared { boundary: String },
}

/// The turn-row record of an isolation.
pub(crate) fn turn_isolation(isolation: &ExecutionIsolation) -> swage_storage::TurnIsolation {
    match isolation {
        ExecutionIsolation::Isolated => swage_storage::TurnIsolation::Isolated,
        ExecutionIsolation::Degraded { reason } => swage_storage::TurnIsolation::Degraded {
            reason: reason.clone(),
        },
        ExecutionIsolation::Declared { boundary } => swage_storage::TurnIsolation::Declared {
            boundary: boundary.clone(),
        },
    }
}

/// Record on the turn row the boundary its dispatcher settled at — the
/// statement every later reader of the turn consults, whatever the kind.
pub(crate) async fn record_turn_isolation(
    state: &AppState,
    isolation: &ExecutionIsolation,
    turn_id: Uuid,
) -> Result<(), swage_core::GatewayError> {
    state
        .storage
        .record_turn_isolation(turn_id, &turn_isolation(isolation))
        .await?;
    Ok(())
}

pub(crate) struct GatewayDispatcherRequest<'a> {
    pub(crate) config: &'a swage_config::ResolvedConfig,
    pub(crate) agent_config: &'a AgentConfig,
    pub(crate) session_metadata: Option<&'a serde_json::Value>,
    pub(crate) topic_id: Option<Uuid>,
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

/// The operator's home and the gateway's data root, as the policy needs them:
/// the private paths to mask and the shared caches to open hang off both.
pub(crate) struct PolicyRoots {
    pub(crate) home: PathBuf,
    pub(crate) data_root: PathBuf,
}

impl PolicyRoots {
    /// The roots from the gateway's environment. A gateway with no `HOME`
    /// cannot say where the private paths and caches are, so it builds no
    /// sandbox rather than one with the masks silently missing.
    pub(crate) fn from_environment(data_root: &Path) -> Result<Self, swage_core::SandboxError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| !home.as_os_str().is_empty())
            .ok_or_else(|| swage_core::SandboxError::Unavailable {
                message: "HOME is not set, so the sandbox cannot place its private-path masks \
                          and shared caches; start the gateway with HOME set"
                    .into(),
            })?;
        Ok(Self {
            home,
            data_root: sandbox_path(data_root),
        })
    }
}

/// Under the home, what a sandboxed command never sees: key material, cloud
/// and cluster credentials, plaintext token files. Forge and registry
/// authentication goes through the operator's credential helper, so none of
/// these is needed to build, commit, push or fetch.
const MASKED_HOME_PATHS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".kube",
    ".netrc",
    ".git-credentials",
    ".config/gh/hosts.yml",
    ".config/forgejo",
];

/// Under the home, the shared build and package caches a sandboxed command
/// may write, so a dependency fetched once is fetched once.
const SHARED_CACHE_PATHS: &[&str] = &[".cache", ".cargo", ".npm", ".local/share/pnpm", "go/pkg"];

/// Where a workspace's git metadata lives when it is not under the
/// workspace: a linked worktree's `.git` is a file naming its gitdir, whose
/// `commondir` names the repository the worktree belongs to. Both must be
/// writable for the worktree to commit. The pointer is content of the
/// workspace, which the sandbox writes, so only a directory that is git
/// metadata — one holding a `HEAD`, and for the common directory its
/// `objects` too — and that does not contain the workspace is listed; a
/// pointer at anything else names nothing.
pub(crate) fn git_metadata_outside(workspace: &Path) -> Vec<PathBuf> {
    let dot_git = workspace.join(".git");
    let Ok(pointer) = std::fs::read_to_string(&dot_git) else {
        return Vec::new();
    };
    let Some(gitdir) = pointer.trim().strip_prefix("gitdir:") else {
        return Vec::new();
    };
    let gitdir = sandbox_path(&workspace.join(gitdir.trim()));
    let is_git_metadata = |path: &Path, common: bool| {
        path.join("HEAD").is_file()
            && (!common || path.join("objects").is_dir())
            && !workspace.starts_with(path)
    };
    let mut paths = Vec::new();
    if !gitdir.starts_with(workspace) {
        if !is_git_metadata(&gitdir, false) {
            tracing::warn!(
                path = %gitdir.display(),
                "ignoring a workspace .git pointer at a directory that is not git metadata"
            );
            return paths;
        }
        paths.push(gitdir.clone());
    }
    if let Ok(common) = std::fs::read_to_string(gitdir.join("commondir")) {
        let common = sandbox_path(&gitdir.join(common.trim()));
        if !common.starts_with(workspace) && !paths.contains(&common) {
            if !is_git_metadata(&common, true) {
                tracing::warn!(
                    path = %common.display(),
                    "ignoring a worktree's commondir at a directory that is not a repository"
                );
                return paths;
            }
            paths.push(common);
        }
    }
    paths
}

/// The masked private path a placement would reopen, when it would: a
/// worker placed by the model inside a person's grant is the model's
/// reach, not the person's. A placement within the operator's private
/// home paths reopens their keys and credentials and is refused anywhere
/// beneath them; one at the gateway's data root itself reopens the whole
/// instance and is refused, while a subtree beneath the root is the
/// managed workspaces' own home and reopens only itself (C64, C67).
pub(crate) fn private_path_covering(path: &Path, roots: &PolicyRoots) -> Option<PathBuf> {
    let path = sandbox_path(path);
    let data_root = sandbox_path(&roots.data_root);
    masked_paths(roots).into_iter().find(|private| {
        if sandbox_path(private) == data_root {
            path == data_root
        } else {
            path.starts_with(private)
        }
    })
}

/// The policy a turn's sandbox runs under: reads everywhere but the masked
/// private paths and the gateway's data root (unless a grant names it),
/// writes the workspace as its grant allows, the workspace's git metadata,
/// the shared caches, temporary space and any attachment granted write.
pub(crate) fn build_sandbox_config(
    agent_config: &AgentConfig,
    workspace_policy: Option<&TrustedWorkspacePolicy>,
    roots: &PolicyRoots,
) -> Result<swage_sandbox::SandboxConfig, swage_sandbox::SandboxConfigError> {
    let working_directory = sandbox_path(&agent_config.working_directory);
    let mut builder = swage_sandbox::SandboxConfig::builder().working_directory(&working_directory);
    let mut granted: Vec<PathBuf> = Vec::new();
    let masked = masked_paths(roots);

    let active_grant = workspace_policy
        .and_then(|policy| policy.active_workspace.as_ref())
        .filter(|grant| sandbox_path(&grant.path) == working_directory);
    let active_can_write = active_grant.is_none_or(|grant| grant.can_write);
    granted.push(working_directory.clone());
    if active_can_write {
        builder = builder.write_path(&working_directory);
        for path in git_metadata_outside(&working_directory) {
            if !path.exists() {
                continue;
            }
            // The pointer is read from the workspace, which the sandbox
            // writes, so it is never the operator's deliberate reach into a
            // private path (C64): metadata under a mask stays masked, and
            // a directory above one is not granted over it.
            if let Some(private) = masked
                .iter()
                .find(|private| path.starts_with(private) || private.starts_with(&path))
            {
                tracing::warn!(
                    path = %path.display(),
                    private = %private.display(),
                    "ignoring workspace git metadata under a masked private path"
                );
                continue;
            }
            builder = builder.write_path(&path);
            granted.push(path);
        }
    } else {
        builder = builder.read_only_path(&working_directory);
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
                builder = builder.write_path(&path);
                granted.push(path);
            } else if grant.can_read {
                builder = builder.read_only_path(&path);
                granted.push(path);
            }
        }
    }

    for cache in SHARED_CACHE_PATHS {
        let path = roots.home.join(cache);
        if path.is_dir() {
            builder = builder.write_path(sandbox_path(&path));
        }
    }
    for private in &masked {
        builder = builder.masked_path(private);
    }
    tracing::debug!(?granted, "sandbox policy grants");

    builder.build()
}

/// The private paths a turn's sandbox masks, each only where it exists: the
/// operator's secrets under the home, and the gateway's own data root — its
/// store, secrets and token — masked unless a grant names it (C64). A grant
/// beneath a mask (the managed scratch workspace lives under the data root)
/// reopens only its own subtree: the mount layer applies the deeper path
/// last, so the grant shows through the mask without the rest; a grant equal
/// to the root, or to any masked private path, binds after the mask and
/// reopens what it names.
fn masked_paths(roots: &PolicyRoots) -> Vec<PathBuf> {
    let mut masked: Vec<PathBuf> = MASKED_HOME_PATHS
        .iter()
        .map(|private| roots.home.join(private))
        .filter(|path| path.exists())
        .map(|path| sandbox_path(&path))
        .collect();
    if roots.data_root.exists() {
        masked.push(roots.data_root.clone());
    }
    masked
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
        .and_then(|metadata| metadata.get(WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY))
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
    egress: swage_sandbox::EgressSetup,
    credentials: Option<&crate::git_credential::CredentialServer>,
    signer: Option<&crate::git_signer::SignerServer>,
) -> Result<TurnSandboxBuild, swage_core::SandboxError> {
    // An install that declares its boundary (a container image, for one)
    // runs every turn inside it: no per-turn sandbox is built, the host
    // sandbox settings are not consulted, and the turn's record names the
    // boundary rather than "isolated" or "degraded".
    if let Some(boundary) = state.declared_execution_boundary.as_ref() {
        return Ok(TurnSandboxBuild {
            sandbox: None,
            isolation: ExecutionIsolation::Declared {
                boundary: boundary.clone(),
            },
            backend: SandboxBackend::None,
        });
    }
    if !config.sandbox.enabled {
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
        let roots = PolicyRoots::from_environment(&state.config_root)?;
        let config =
            build_sandbox_config(agent_config, workspace_policy, &roots).map_err(|error| {
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
    if std::env::current_exe().is_ok_and(|path| {
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
        egress,
        credentials,
        signer,
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
                if !self.config.working_directory_is_writable() {
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
    egress: swage_sandbox::EgressSetup,
    credentials: Option<&crate::git_credential::CredentialServer>,
    signer: Option<&crate::git_signer::SignerServer>,
    bubblewrap_available: bool,
    spawn_fallback: bool,
) -> Result<TurnSandboxBuild, swage_core::SandboxError> {
    let roots = PolicyRoots::from_environment(&state.config_root)?;
    let mut sandbox_config =
        build_sandbox_config(agent_config, workspace_policy, &roots).map_err(|error| {
            swage_core::SandboxError::SpawnFailed {
                message: format!("failed to build sandbox config: {error}"),
            }
        })?;

    if let Some(credentials) = credentials {
        credentials.configure(&mut sandbox_config)?;
    }
    if let Some(signer) = signer {
        signer.configure(&mut sandbox_config)?;
    }

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
            swage_sandbox::BubblewrapSandbox::new(sandbox_config.clone(), make_llm_handler())
                .with_egress(egress.clone());
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

    // The fallback has no mount layer, so it hides nothing: the private
    // paths the policy masks are readable to this turn, and the record
    // of its degraded isolation says which.
    let degradation_reason = degraded_reason(degradation_reason, &sandbox_config.masked_paths);
    let mut fallback =
        swage_sandbox::FallbackSandbox::new(sandbox_config, make_llm_handler()).with_egress(egress);
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

/// The degraded isolation's reason with the private paths left unmasked
/// named, so a reader of the turn's record learns what was open.
fn degraded_reason(reason: String, masked_paths: &[PathBuf]) -> String {
    if masked_paths.is_empty() {
        return reason;
    }
    tracing::warn!(
        unmasked = ?masked_paths,
        "degraded sandbox cannot mask private paths; they are readable to this turn"
    );
    let unmasked = masked_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{reason}; unmasked: {unmasked}")
}

pub(crate) async fn build_gateway_dispatcher(
    state: &AppState,
    request: GatewayDispatcherRequest<'_>,
) -> Result<GatewayDispatcherBuild, swage_core::SandboxError> {
    let GatewayDispatcherRequest {
        config,
        agent_config,
        session_metadata,
        topic_id,
        active_workspace_attachment_id,
        ws_tx,
        session_id,
        turn_id,
    } = request;
    #[cfg(test)]
    if state.take_dispatcher_build_failure() {
        return Err(swage_core::SandboxError::SpawnFailed {
            message: "injected dispatcher build failure".into(),
        });
    }

    let mut workspace_policy = match topic_id {
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
    let permissions = build_permission_config(
        config,
        startup_config,
        topic_id,
        session_id,
        &session_approval_grants,
    );

    // A tool a platform agent holds is withheld from every other agent here,
    // before permissions are consulted: no grant, not even `*`, offers it.
    let base_dispatcher = state.active_tool_dispatcher();
    let dispatcher = ToolDispatcher::new(
        Arc::new(base_dispatcher.registry().for_agent(&agent_config.agent_id)),
        permissions.clone(),
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );

    // What this turn's sandboxed commands may reach, and the record of it:
    // the install's hosts, then the judge, every attempt an audit event of
    // this session and turn.
    let egress = crate::egress::turn_egress_setup(
        &config.sandbox.egress_hosts,
        Some(Arc::new(crate::request_judge::GatewayRequestJudge::new(
            state.clone(),
        ))),
        state.storage.clone(),
        session_id,
        turn_id,
        &agent_config.agent_id,
    );
    let credentials = crate::git_credential::CredentialServer::start(
        state.secret_manager.clone(),
        state.storage.clone(),
        session_id,
        turn_id,
    )?;
    // The operator's commit signing, forwarded to the host: present when
    // the operator signs, as the workspace's git configuration says; a
    // configuration that signs nothing leaves sandboxed git signing nothing.
    let signer = match crate::git_signer::SigningConfig::from_host_git(
        &agent_config.working_directory,
    )
    .await
    {
        Some(signing) => Some(crate::git_signer::SignerServer::start(
            state.storage.clone(),
            session_id,
            turn_id,
            signing,
        )?),
        None => None,
    };
    let sandbox_build = build_turn_sandbox(
        state,
        config,
        agent_config,
        workspace_policy.as_ref(),
        egress.clone(),
        Some(&credentials),
        signer.as_deref(),
    )
    .await?;
    tracing::debug!(backend = ?sandbox_build.backend, "selected turn sandbox backend");
    let python_config = if config.tools.python_execute.enabled
        && (config.tools.python_execute.require_sandbox
            || sandbox_build.backend == SandboxBackend::Bubblewrap)
    {
        let roots = PolicyRoots::from_environment(&state.config_root)?;
        let mut policy = build_sandbox_config(agent_config, workspace_policy.as_ref(), &roots)
            .map_err(|error| swage_core::SandboxError::SpawnFailed {
                message: error.to_string(),
            })?;
        credentials.configure(&mut policy)?;
        if let Some(signer) = &signer {
            signer.configure(&mut policy)?;
        }
        Some(policy)
    } else {
        None
    };
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

    let mut nested_registry = swage_tool::registry::ToolRegistry::new();
    for name in dispatcher
        .registry()
        .list()
        .into_iter()
        .filter(|name| *name != "python_execute")
    {
        nested_registry
            .register_builtin(dispatcher.registry().get(name).expect("registered tool"))
            .map_err(|error| swage_core::SandboxError::SpawnFailed {
                message: error.to_string(),
            })?;
    }
    let nested_dispatcher = ToolDispatcher::new(
        Arc::new(nested_registry),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    )
    .with_execution_strategy(Arc::new(GatewayExecutionStrategy::new(
        inner_strategy.clone(),
        ws_tx.clone(),
        device.clone(),
        state.secret_manager.clone(),
        state.storage.clone(),
        Some(state.clone()),
    )));
    let python_execution = crate::tool_executor::PythonExecution {
        sandbox: python_config,
        egress,
        dispatcher: Arc::new(nested_dispatcher),
        credentials,
        signer,
    };

    Ok(GatewayDispatcherBuild {
        dispatcher: dispatcher.with_execution_strategy(Arc::new(
            GatewayExecutionStrategy::new(
                inner_strategy,
                ws_tx,
                device,
                state.secret_manager.clone(),
                state.storage.clone(),
                Some(state.clone()),
            )
            .with_python_execution(python_execution),
        )),
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
    let session_id = request.session_id;
    let turn_id = request.turn_id;
    let build = build_gateway_dispatcher(state, request)
        .await
        .map_err(|error| swage_core::GatewayError::ConfigError(error.to_string()))?;
    // The request's turn is the one the audit attributes the build to — for a
    // delegated child, the sender's — so no turn row is stamped here: a turn's
    // own boundary is written where its dispatcher settles for it
    // (dispatcher_settlement.rs), never from a build made on its behalf.
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

    // Environment changes are confined to a subprocess, away from other tests.
    fn run_dispatcher_environment_case(name: &str, no_home: bool) -> bool {
        if std::env::var_os("SWAGE_DISPATCHER_REGRESSION_CHILD").is_some() {
            return true;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--nocapture"])
            .env("SWAGE_DISPATCHER_REGRESSION_CHILD", "1");
        if no_home {
            command
                .env_remove("HOME")
                .env_remove("SWAGE_EXECUTION_BOUNDARY");
        } else {
            let runtime = swage_python::runtime_resolver::ResolvedPythonRuntime::resolve(
                &swage_config::model::PythonExecuteToolConfig::default(),
            )
            .unwrap();
            command
                .env("SWAGE_TEST_PYTHON", runtime.executable_path)
                .env("PATH", "")
                .env("SWAGE_EXECUTION_BOUNDARY", "container");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "environment case failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    async fn environment_case_dispatcher(config: swage_config::ResolvedConfig, python: bool) {
        let root = tempfile::tempdir().unwrap();
        let (state, session_id) = crate::turn::tests::make_state_with_storage_and_root(
            Vec::new(),
            Arc::new(MockStorage::new()),
            Some(root.path().to_owned()),
        )
        .await;
        let tool: Arc<dyn Tool> = if python {
            let runtime = Arc::new(
                swage_python::PythonWorkerRuntime::new(config.tools.python_execute.clone())
                    .unwrap(),
            );
            let pool = swage_python::WorkerPool::new(runtime.clone()).unwrap();
            Arc::new(crate::tool::python_execute::PythonExecuteTool::new(
                runtime,
                pool,
                state.storage.clone(),
            ))
        } else {
            Arc::new(crate::tool::shell::ShellTool {
                timeout: std::time::Duration::from_secs(5),
                max_output_bytes: 1024,
            })
        };
        *state.runtime_tool_dispatcher.write().unwrap() =
            Arc::new(ToolDispatcher::allow_all(vec![tool]));
        let agent_config =
            AgentConfig::new("gateway").with_workspace(root.path(), vec![root.path().to_owned()]);
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let build = build_gateway_dispatcher(
            &state,
            GatewayDispatcherRequest {
                config: &config,
                agent_config: &agent_config,
                session_metadata: None,
                topic_id: None,
                active_workspace_attachment_id: None,
                ws_tx,
                session_id: Some(session_id),
                turn_id: Some(Uuid::new_v4()),
            },
        )
        .await
        .expect("unsandboxed dispatcher must build without HOME");
        assert!(build.sandbox_guard.is_none());
        let mut ctx = swage_tool::context::MockToolContext::new("gateway", session_id.to_string());
        ctx.working_directory = root.path().to_owned();
        ctx.allowed_paths = vec![root.path().to_owned()];
        ctx.turn_id = Some(Uuid::new_v4());
        ctx.call_id = Some("environment-check".into());
        let (name, params) = if python {
            assert!(swage_sandbox::find_bwrap().is_err());
            (
                "python_execute",
                serde_json::json!({"code": "print(6 * 7)"}),
            )
        } else {
            ("shell", serde_json::json!({"command": "printf 42"}))
        };
        let result = build
            .dispatcher
            .dispatch(name, params, &ctx, "gateway")
            .await;
        assert!(result.is_ok(), "unsandboxed tool must run: {result:?}");
        assert_eq!(
            result.unwrap().result["output"].as_str().unwrap().trim(),
            "42"
        );
    }

    #[test]
    fn no_home_unsandboxed_dispatcher_runs_tools() {
        if !run_dispatcher_environment_case(
            "turn::dispatcher::tests::no_home_unsandboxed_dispatcher_runs_tools",
            true,
        ) {
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for python_enabled in [true, false] {
                let mut config = swage_config::ResolvedConfig::default();
                config.sandbox.enabled = false;
                config.tools.python_execute.require_sandbox = false;
                config.tools.python_execute.enabled = python_enabled;
                config.dangerously_skip_permissions = true;
                environment_case_dispatcher(config, false).await;
            }
        });
    }

    #[test]
    fn declared_boundary_python_runs_without_bubblewrap() {
        if !run_dispatcher_environment_case(
            "turn::dispatcher::tests::declared_boundary_python_runs_without_bubblewrap",
            false,
        ) {
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut config = swage_config::ResolvedConfig::default();
            config.tools.python_execute.require_sandbox = false;
            config.tools.python_execute.enabled = true;
            config.tools.python_execute.warm_target = 0;
            config.tools.python_execute.runtime_path =
                Some(std::env::var("SWAGE_TEST_PYTHON").unwrap());
            config.dangerously_skip_permissions = true;
            assert!(config.sandbox.enabled);
            environment_case_dispatcher(config, true).await;
        });
    }

    #[test]
    fn degraded_python_records_http_without_bubblewrap() {
        degraded_python_case(
            "turn::dispatcher::tests::degraded_python_records_http_without_bubblewrap",
            false,
        );
    }

    #[test]
    fn degraded_python_required_sandbox_refuses_without_bubblewrap() {
        degraded_python_case(
            "turn::dispatcher::tests::degraded_python_required_sandbox_refuses_without_bubblewrap",
            true,
        );
    }

    fn degraded_python_case(name: &str, require_sandbox: bool) {
        if std::env::var_os("SWAGE_IPC_SOCKET").is_some() {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                swage_sandbox::run_agent_loop(Vec::new()).await.unwrap();
            });
            return;
        }
        if std::env::var_os("SWAGE_DEGRADED_PYTHON_CHILD").is_none() {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            // Move out of Cargo's deps directory so the real backend selection
            // runs, including starting the fallback agent over IPC.
            let binary = root.path().join("dispatcher-test");
            std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
            let agent = root.path().join("agent");
            std::fs::write(
                &agent,
                format!(
                    "#!/bin/sh\nexec \"$SWAGE_TEST_DISPATCHER_BINARY\" --exact {name} --nocapture\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).unwrap();
            let runtime = swage_python::runtime_resolver::ResolvedPythonRuntime::resolve(
                &swage_config::model::PythonExecuteToolConfig::default(),
            )
            .unwrap();
            let output = std::process::Command::new(&binary)
                .args(["--exact", name, "--nocapture"])
                .env("SWAGE_DEGRADED_PYTHON_CHILD", "1")
                .env("SWAGE_TEST_DISPATCHER_BINARY", &binary)
                .env("SWAGE_SANDBOX_AGENT_BINARY", agent)
                .env("SWAGE_ENABLE_TEST_SANDBOX", "1")
                .env("SWAGE_TEST_PYTHON", runtime.executable_path)
                .env("PATH", "")
                .env_remove("SWAGE_EXECUTION_BOUNDARY")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "degraded Python case failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            assert!(swage_sandbox::find_bwrap().is_err());
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            let (state, session_id) = crate::turn::tests::make_state_with_storage_and_root(
                Vec::new(),
                Arc::new(MockStorage::new()),
                Some(root.path().join("config")),
            )
            .await;
            let mut config = swage_config::ResolvedConfig::default();
            config.sandbox.enabled = true;
            config.sandbox.allow_degraded_execution = true;
            config.sandbox.egress_hosts = vec!["127.0.0.1".into()];
            config.tools.python_execute.enabled = true;
            config.tools.python_execute.require_sandbox = require_sandbox;
            config.tools.python_execute.warm_target = 0;
            config.tools.python_execute.runtime_path =
                Some(std::env::var("SWAGE_TEST_PYTHON").unwrap());
            config.dangerously_skip_permissions = true;
            let runtime = Arc::new(
                swage_python::PythonWorkerRuntime::new(config.tools.python_execute.clone()).unwrap(),
            );
            let pool = swage_python::WorkerPool::new(runtime.clone()).unwrap();
            let tool: Arc<dyn Tool> = Arc::new(crate::tool::python_execute::PythonExecuteTool::new(
                runtime,
                pool,
                state.storage.clone(),
            ));
            *state.runtime_tool_dispatcher.write().unwrap() =
                Arc::new(ToolDispatcher::allow_all(vec![tool]));
            let agent_config = AgentConfig::new("gateway")
                .with_workspace(&workspace, vec![workspace.clone()]);
            let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
            let turn_id = Uuid::new_v4();
            let build = build_gateway_dispatcher(
                &state,
                GatewayDispatcherRequest {
                    config: &config,
                    agent_config: &agent_config,
                    session_metadata: None,
                    topic_id: None,
                    active_workspace_attachment_id: None,
                    ws_tx,
                    session_id: Some(session_id),
                    turn_id: Some(turn_id),
                },
            )
            .await
            .expect("opted-in fallback dispatcher must build");
            let ExecutionIsolation::Degraded { reason } = &build.isolation else {
                panic!("{:?}", build.isolation);
            };
            assert!(
                reason.starts_with("bubblewrap_unavailable"),
                "{reason}"
            );
            assert!(build.sandbox_guard.is_some());
            let server = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = server.local_addr().unwrap();
            let fixture = tokio::spawn(async move {
                let (stream, _) = server.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                assert_eq!(line, "GET /recorded HTTP/1.1\r\n");
                reader.get_mut().write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture",
                ).await.unwrap();
            });
            let outside = root.path().join("outside-workspace");
            let code = format!(
                "import os, pathlib, urllib.request\nos.environ['no_proxy'] = ''\nos.environ['NO_PROXY'] = ''\npathlib.Path({:?}).write_text('unsandboxed')\nprint(urllib.request.urlopen('http://{address}/recorded', timeout=3).read().decode())",
                outside.to_str().unwrap(),
            );
            let mut ctx = swage_tool::context::MockToolContext::new("gateway", session_id.to_string());
            ctx.working_directory = workspace.clone();
            ctx.allowed_paths = vec![workspace];
            ctx.turn_id = Some(turn_id);
            ctx.call_id = Some("degraded-python".into());
            let result = build.dispatcher.dispatch(
                "python_execute", serde_json::json!({"code": code}), &ctx, "gateway",
            ).await;
            if require_sandbox {
                let error = result.expect_err("required Python sandbox must refuse fallback");
                assert!(error.to_string().contains("bubblewrap is required"), "{error}");
                assert!(!outside.exists());
                fixture.abort();
            } else {
                assert!(result.is_ok(), "degraded Python must run: {result:?}");
                assert_eq!(result.unwrap().result["output"].as_str().unwrap().trim(), "fixture");
                assert_eq!(std::fs::read_to_string(outside).unwrap(), "unsandboxed");
                tokio::time::timeout(std::time::Duration::from_secs(5), fixture)
                    .await.unwrap().unwrap();
            }
            let events = state.storage.list_audit_events().await.unwrap();
            let records: Vec<_> = events.iter()
                .filter(|event| event.event_kind == "egress_connection").collect();
            if require_sandbox {
                assert!(records.is_empty());
            } else {
                assert_eq!(records.len(), 1, "the turn must record Python HTTP egress");
                assert_eq!(records[0].session_id, Some(session_id));
                assert_eq!(records[0].entity_id, turn_id.to_string());
                assert_eq!(records[0].metadata["host"], "127.0.0.1");
                assert_eq!(records[0].metadata["port"], address.port());
                assert_eq!(records[0].metadata["method"], "GET");
                assert_eq!(records[0].metadata["verdict"], "allowed");
            }
            build.sandbox_guard.as_ref().unwrap().shutdown().await.unwrap();
        });
    }

    /// An egress setup for tests of backend selection: nothing is reachable
    /// and nothing is recorded, since no command runs.
    fn test_egress() -> swage_sandbox::EgressSetup {
        struct Closed;
        #[async_trait::async_trait]
        impl swage_sandbox::EgressPolicy for Closed {
            async fn decide(
                &self,
                _: &swage_sandbox::EgressRequest,
            ) -> swage_sandbox::EgressVerdict {
                swage_sandbox::EgressVerdict::Refused {
                    reason: "test".into(),
                }
            }
        }
        struct Dropped;
        #[async_trait::async_trait]
        impl swage_sandbox::EgressRecorder for Dropped {
            async fn record(&self, _: swage_sandbox::EgressConnection) -> Result<(), String> {
                Ok(())
            }
        }
        swage_sandbox::EgressSetup {
            policy: Arc::new(Closed),
            recorder: Arc::new(Dropped),
        }
    }

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
    async fn a_declared_boundary_runs_every_turn_inside_it_and_records_no_degradation() {
        let (mut state, session_id) = make_state_with_session(Vec::new()).await;
        state.declared_execution_boundary = Some("container".into());
        let workspace = tempfile::tempdir().unwrap();
        let agent_config = AgentConfig::new("gateway")
            .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
        // Even with the host sandbox switched off: the declaration decides.
        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.enabled = false;

        let build = build_turn_sandbox(
            &state,
            &config,
            &agent_config,
            None,
            test_egress(),
            None,
            None,
        )
        .await
        .expect("a declared boundary needs no sandbox of its own");
        assert_eq!(
            build.isolation,
            ExecutionIsolation::Declared {
                boundary: "container".into()
            }
        );
        assert_eq!(build.backend, SandboxBackend::None);

        record_degraded_execution(
            &state,
            &build.isolation,
            "interactive turn",
            Some(session_id),
            Some(Uuid::new_v4()),
        )
        .await
        .unwrap();
        assert!(
            state.storage.list_audit_events().await.unwrap().is_empty(),
            "a declared boundary is neither isolated nor degraded: nothing to audit"
        );
        assert_eq!(
            turn_isolation(&build.isolation),
            swage_storage::TurnIsolation::Declared {
                boundary: "container".into()
            }
        );
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

        let build = build_turn_sandbox(
            &state,
            &config,
            &agent_config,
            None,
            test_egress(),
            None,
            None,
        )
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
            test_egress(),
            None,
            None,
            false,
            false,
        )
        .await
        .expect("the explicit opt-in should select the fallback sandbox");
        assert_eq!(build.backend, SandboxBackend::Fallback);
        assert!(build.sandbox.is_some());
        // The reason names the private paths the fallback leaves readable,
        // so the record says what was open.
        let ExecutionIsolation::Degraded { reason } = &build.isolation else {
            panic!("{:?}", build.isolation);
        };
        assert!(reason.starts_with("bubblewrap_unavailable"), "{reason}");
        assert!(reason.contains("; unmasked: "), "{reason}");

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
                && event.metadata["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.starts_with("bubblewrap_unavailable"))
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
                topic_id: None,
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
    async fn explicit_sandbox_disablement_constructs_dispatcher_without_degraded_opt_in() {
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
                topic_id: None,
                active_workspace_attachment_id: None,
                ws_tx,
                session_id: Some(session_id),
                turn_id: Some(Uuid::new_v4()),
            },
            "delegated turn",
        )
        .await;

        let build = result.expect("explicit sandbox disablement should construct a dispatcher");
        assert_eq!(
            build.isolation,
            ExecutionIsolation::Degraded {
                reason: "sandbox_disabled_by_configuration".into(),
            }
        );
        assert_eq!(state.storage.list_audit_events().await.unwrap().len(), 1);
    }
}
