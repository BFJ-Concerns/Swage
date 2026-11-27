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

/// Whether a turn's tools run sandboxed, unsandboxed because the
/// configuration switched the sandbox off, or without the isolation a sandbox
/// meant to run would have given, for a stated reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutionIsolation {
    Isolated,
    SandboxDisabled,
    Degraded { reason: String },
}

/// The turn-row record of an isolation.
pub(crate) fn turn_isolation(isolation: &ExecutionIsolation) -> swage_storage::TurnIsolation {
    match isolation {
        ExecutionIsolation::Isolated => swage_storage::TurnIsolation::Isolated,
        ExecutionIsolation::SandboxDisabled => swage_storage::TurnIsolation::SandboxDisabled,
        ExecutionIsolation::Degraded { reason } => swage_storage::TurnIsolation::Degraded {
            reason: reason.clone(),
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
    /// The hook over the model's tool calls the turn's revision attaches at
    /// `tool_call`, resolved by the turn from the revision it took at its
    /// start; none when the revision attaches nothing there.
    pub(crate) call_hook: Option<Arc<dyn swage_tool::dispatch::ToolCallHook>>,
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

/// The operator's home, the gateway's data root and the variables that move
/// places out of the home, as the policy needs them: the private paths to
/// mask and the shared caches to open hang off all three.
pub(crate) struct PolicyRoots {
    pub(crate) home: PathBuf,
    pub(crate) data_root: PathBuf,
    /// The gateway's values of [`LOCATION_VARIABLES`], absolute paths only.
    pub(crate) environment: LocationEnvironment,
}

/// The variables in the gateway's environment that move a cache, a
/// credential store, the temporary directory or the runtime directory
/// away from its place in the home.
const LOCATION_VARIABLES: &[&str] = &[
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "TMPDIR",
    "CARGO_HOME",
    "GOPATH",
    "GOMODCACHE",
    "GOCACHE",
    "UV_CACHE_DIR",
    "npm_config_cache",
    "NPM_CONFIG_CACHE",
    "npm_config_userconfig",
    "NPM_CONFIG_USERCONFIG",
    "GNUPGHOME",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_CONFIG_FILE",
    "AZURE_CONFIG_DIR",
    "CLOUDSDK_CONFIG",
    "KUBECONFIG",
    "GH_CONFIG_DIR",
    "DOCKER_CONFIG",
    "PASSWORD_STORE_DIR",
];

/// Of [`LOCATION_VARIABLES`], those that hold a list of paths, as `PATH`
/// does, rather than one path.
const PATH_LIST_VARIABLES: &[&str] = &["GOPATH", "KUBECONFIG"];

/// The paths each location variable names in the gateway's environment.
/// A relative path names no place a mount can, so it is left out.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocationEnvironment(std::collections::BTreeMap<&'static str, Vec<PathBuf>>);

impl LocationEnvironment {
    fn from_process() -> Self {
        Self(
            LOCATION_VARIABLES
                .iter()
                .filter_map(|variable| {
                    let value = std::env::var_os(variable)?;
                    let paths: Vec<PathBuf> = if PATH_LIST_VARIABLES.contains(variable) {
                        std::env::split_paths(&value).collect()
                    } else {
                        vec![PathBuf::from(value)]
                    };
                    let paths: Vec<PathBuf> = paths
                        .into_iter()
                        .filter(|path| path.is_absolute())
                        .collect();
                    (!paths.is_empty()).then_some((*variable, paths))
                })
                .collect(),
        )
    }

    /// The paths `variable` names, first the one a tool reads first.
    fn paths(&self, variable: &str) -> &[PathBuf] {
        self.0.get(variable).map(Vec::as_slice).unwrap_or_default()
    }

    /// The one path `variable` names, when it names one.
    fn path(&self, variable: &str) -> Option<&Path> {
        self.paths(variable).first().map(PathBuf::as_path)
    }
}

impl<const N: usize> From<[(&'static str, PathBuf); N]> for LocationEnvironment {
    fn from(values: [(&'static str, PathBuf); N]) -> Self {
        let mut environment = Self::default();
        for (variable, path) in values {
            environment.0.entry(variable).or_default().push(path);
        }
        environment
    }
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
            environment: LocationEnvironment::from_process(),
        })
    }

    /// Where `place` is: the path the first of its variables set in the
    /// environment names, or its place in the home where none is set.
    fn resolve(&self, place: &Place) -> PathBuf {
        place
            .moved_by
            .iter()
            .find_map(|(variable, beneath)| {
                self.environment
                    .path(variable)
                    .map(|path| join_beneath(path, beneath))
            })
            .unwrap_or_else(|| self.home.join(place.home))
    }

    /// Every path `place` may be at: its place in the home, and each path
    /// any of its variables names.
    fn every_location(&self, place: &Place) -> Vec<PathBuf> {
        std::iter::once(self.home.join(place.home))
            .chain(place.moved_by.iter().flat_map(|(variable, beneath)| {
                self.environment
                    .paths(variable)
                    .iter()
                    .map(|path| join_beneath(path, beneath))
            }))
            .collect()
    }
}

fn join_beneath(path: &Path, beneath: &str) -> PathBuf {
    if beneath.is_empty() {
        path.to_path_buf()
    } else {
        path.join(beneath)
    }
}

/// A place the policy treats specially, under the home unless the
/// operator's environment moves it: `home` is where it is relative to the
/// home, and each of `moved_by` is a variable that moves it and where the
/// place is beneath the path that variable names (`""` for the path itself).
struct Place {
    home: &'static str,
    moved_by: &'static [(&'static str, &'static str)],
}

const fn at_home(home: &'static str) -> Place {
    Place {
        home,
        moved_by: &[],
    }
}

/// What a sandboxed command never sees: key material, cloud and cluster
/// credentials, plaintext token files, wherever the operator's environment
/// keeps them. A store is masked at its place in the home and at every path
/// its variables name. Forge and registry authentication goes through the
/// operator's SSH agent and their git credential helpers run on the host,
/// so none of these is needed to build, commit, push or fetch; of `~/.ssh`,
/// the files that hold no key reopen (`ssh_files_without_keys`).
const MASKED_PLACES: &[Place] = &[
    at_home(".ssh"),
    Place {
        home: ".gnupg",
        moved_by: &[("GNUPGHOME", "")],
    },
    at_home(".aws"),
    Place {
        home: ".aws/credentials",
        moved_by: &[("AWS_SHARED_CREDENTIALS_FILE", "")],
    },
    Place {
        home: ".aws/config",
        moved_by: &[("AWS_CONFIG_FILE", "")],
    },
    Place {
        home: ".azure",
        moved_by: &[("AZURE_CONFIG_DIR", "")],
    },
    Place {
        home: ".config/gcloud",
        moved_by: &[("CLOUDSDK_CONFIG", "")],
    },
    at_home(".kube"),
    Place {
        home: ".kube/config",
        moved_by: &[("KUBECONFIG", "")],
    },
    at_home(".netrc"),
    at_home(".git-credentials"),
    Place {
        home: ".config/git/credentials",
        moved_by: &[("XDG_CONFIG_HOME", "git/credentials")],
    },
    Place {
        home: ".config/gh/hosts.yml",
        moved_by: &[
            ("GH_CONFIG_DIR", "hosts.yml"),
            ("XDG_CONFIG_HOME", "gh/hosts.yml"),
        ],
    },
    Place {
        home: ".config/forgejo",
        moved_by: &[("XDG_CONFIG_HOME", "forgejo")],
    },
    // Registry tokens.
    Place {
        home: ".cargo/credentials",
        moved_by: &[("CARGO_HOME", "credentials")],
    },
    Place {
        home: ".cargo/credentials.toml",
        moved_by: &[("CARGO_HOME", "credentials.toml")],
    },
    Place {
        home: ".npmrc",
        moved_by: &[("npm_config_userconfig", ""), ("NPM_CONFIG_USERCONFIG", "")],
    },
    at_home(".pypirc"),
    at_home(".gem/credentials"),
    Place {
        home: ".docker/config.json",
        moved_by: &[("DOCKER_CONFIG", "config.json")],
    },
    Place {
        home: ".local/share/keyrings",
        moved_by: &[("XDG_DATA_HOME", "keyrings")],
    },
    Place {
        home: ".password-store",
        moved_by: &[("PASSWORD_STORE_DIR", "")],
    },
];

/// The shared build and package caches a sandboxed command may write, so a
/// dependency fetched once is fetched once, each with the tool that fills
/// it; `.cache` is every tool's. Each is where the operator's environment
/// puts it, which is where the same tool finds it in the sandbox, since its
/// variables are among those the sandbox passes in.
const SHARED_CACHES: &[(Place, Option<&str>)] = &[
    (
        Place {
            home: ".cache",
            moved_by: &[("XDG_CACHE_HOME", "")],
        },
        None,
    ),
    (
        Place {
            home: ".npm",
            moved_by: &[("npm_config_cache", ""), ("NPM_CONFIG_CACHE", "")],
        },
        Some("npm"),
    ),
    // pnpm's store only: its home beside it holds the global binaries on
    // the operator's PATH.
    (
        Place {
            home: ".local/share/pnpm/store",
            moved_by: &[("XDG_DATA_HOME", "pnpm/store")],
        },
        Some("pnpm"),
    ),
    // Tools whose own variable moves their cache out of the shared one
    // above; unmoved, each is inside it.
    (
        Place {
            home: ".cache/uv",
            moved_by: &[("UV_CACHE_DIR", ""), ("XDG_CACHE_HOME", "uv")],
        },
        Some("uv"),
    ),
    (
        Place {
            home: ".cache/go-build",
            moved_by: &[("GOCACHE", ""), ("XDG_CACHE_HOME", "go-build")],
        },
        Some("go"),
    ),
    (
        Place {
            home: "go/pkg",
            moved_by: &[("GOPATH", "pkg")],
        },
        Some("go"),
    ),
    (
        Place {
            home: "go/pkg/mod",
            moved_by: &[("GOMODCACHE", ""), ("GOPATH", "pkg/mod")],
        },
        Some("go"),
    ),
    // Of Cargo's home, the registry's index and crates and git
    // dependencies. The rest is not a cache — `bin/` holds the binaries on
    // the operator's PATH and `config.toml` steers every host cargo — so it
    // stays read-only.
    (
        Place {
            home: ".cargo/registry",
            moved_by: &[("CARGO_HOME", "registry")],
        },
        Some("cargo"),
    ),
    (
        Place {
            home: ".cargo/git",
            moved_by: &[("CARGO_HOME", "git")],
        },
        Some("cargo"),
    ),
];

/// The shared caches a turn's sandbox may write, beside the paths it
/// already writes (`writable`). A cache that does not exist yet is created
/// first where its tool is installed, since the sandbox can write inside a
/// cache but cannot create one in the home; a cache with no tool to fill it
/// is left uncreated. A cache within a writable path, the shared cache
/// included, is written through that path and granted nothing of its own
/// (`within_writable`).
pub(crate) fn shared_caches(
    roots: &PolicyRoots,
    writable: &[PathBuf],
    installed: impl Fn(&str) -> bool,
) -> Vec<PathBuf> {
    let mut caches: Vec<PathBuf> = Vec::new();
    for (cache, tool) in SHARED_CACHES {
        let path = roots.resolve(cache);
        if within_writable(&path, writable.iter().chain(&caches)) {
            continue;
        }
        if !path.is_dir()
            && tool.is_none_or(&installed)
            && let Err(error) = std::fs::create_dir_all(&path)
        {
            tracing::warn!(
                path = %path.display(),
                %error,
                "the shared cache could not be created; sandboxed commands cannot write it"
            );
        }
        if path.is_dir() {
            caches.push(sandbox_path(&path));
        }
    }
    caches
}

/// Whether `path`, or any directory above it, is within a path the sandbox
/// writes. Such a path is the sandbox's to shape: a command can make it,
/// or a directory on the way to it, a link to anywhere, so its target is
/// never granted on its say; what is really there is already writable.
fn within_writable<'a>(path: &Path, writable: impl Iterator<Item = &'a PathBuf> + Clone) -> bool {
    path.ancestors().any(|ancestor| {
        let ancestor = sandbox_path(ancestor);
        writable
            .clone()
            .any(|granted| ancestor.starts_with(granted))
    })
}

/// The temporary directory the operator's environment names, when it is
/// one a sandboxed command needs granted: the sandbox's own `/tmp` hides
/// everything the host keeps under `/tmp`, and the host's root is read-only,
/// so a `TMPDIR` anywhere but `/tmp` itself is written only through a grant.
fn temporary_directory(roots: &PolicyRoots, writable: &[PathBuf]) -> Option<PathBuf> {
    let named = roots.environment.path("TMPDIR")?;
    let directory = sandbox_path(named);
    (directory.is_dir()
        && directory != Path::new("/tmp")
        && !within_writable(named, writable.iter()))
    .then_some(directory)
}

/// Whether a command is installed: an executable of that name on `PATH`.
fn on_path(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(command).is_file())
    })
}

/// Where a workspace's git metadata lives when it is not under the
/// workspace: a linked worktree's `.git` is a file naming its gitdir, whose
/// `commondir` names the repository the worktree belongs to, and a
/// submodule's names its directory under the superproject's `.git/modules`.
/// The gitdir, and a worktree's common directory, must be writable for the
/// workspace to commit.
///
/// The pointer is content of the workspace, which the sandbox writes, so
/// it is trusted only as far as the metadata it names points back: a
/// gitdir is listed only when it names this workspace — a worktree's
/// `gitdir` file, or a submodule's `core.worktree` — and a common directory
/// only when the gitdir is one of its `worktrees`. A pointer at another
/// repository's metadata names nothing.
pub(crate) fn git_metadata_outside(workspace: &Path) -> Vec<PathBuf> {
    let dot_git = workspace.join(".git");
    let Ok(pointer) = std::fs::read_to_string(&dot_git) else {
        return Vec::new();
    };
    let Some(gitdir) = pointer.trim().strip_prefix("gitdir:") else {
        return Vec::new();
    };
    let gitdir = sandbox_path(&workspace.join(gitdir.trim()));
    let workspace = sandbox_path(workspace);
    let is_git_metadata = |path: &Path, common: bool| {
        path.join("HEAD").is_file()
            && (!common || path.join("objects").is_dir())
            && !workspace.starts_with(path)
    };
    let mut paths = Vec::new();
    if !gitdir.starts_with(&workspace) {
        if !is_git_metadata(&gitdir, false) || !names_workspace(&gitdir, &workspace) {
            tracing::warn!(
                path = %gitdir.display(),
                "ignoring a workspace .git pointer at git metadata that does not name this workspace"
            );
            return paths;
        }
        paths.push(gitdir.clone());
    }
    if let Ok(common) = std::fs::read_to_string(gitdir.join("commondir")) {
        let common = sandbox_path(&gitdir.join(common.trim()));
        if !common.starts_with(&workspace) && !paths.contains(&common) {
            if !is_git_metadata(&common, true)
                || gitdir.parent() != Some(common.join("worktrees").as_path())
            {
                tracing::warn!(
                    path = %common.display(),
                    "ignoring a worktree's commondir at a directory that is not its repository"
                );
                return paths;
            }
            paths.push(common);
        }
    }
    paths
}

/// Whether git metadata outside a workspace points back at it: a linked
/// worktree's gitdir holds a `gitdir` file naming the workspace's `.git`,
/// and a submodule's sets `core.worktree` to the workspace.
fn names_workspace(gitdir: &Path, workspace: &Path) -> bool {
    if let Ok(back) = std::fs::read_to_string(gitdir.join("gitdir")) {
        return sandbox_path(&gitdir.join(back.trim())) == workspace.join(".git");
    }
    std::process::Command::new("git")
        .arg("config")
        .arg("--file")
        .arg(gitdir.join("config"))
        .args(["--get", "core.worktree"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| {
            let worktree = String::from_utf8_lossy(&output.stdout);
            sandbox_path(&gitdir.join(worktree.trim())) == workspace
        })
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
    config: &swage_config::ResolvedConfig,
) -> Result<swage_sandbox::SandboxConfig, swage_core::SandboxError> {
    let working_directory = sandbox_path(&agent_config.working_directory);
    let runtime = crate::settings::tool_runtime_from_config(&config.tools).map_err(|error| {
        swage_core::SandboxError::SpawnFailed {
            message: format!("failed to configure sandbox tools: {error}"),
        }
    })?;
    let host_network = config.sandbox.network == swage_config::SandboxNetwork::Open;
    let mut builder = swage_sandbox::SandboxConfig::builder()
        .working_directory(&working_directory)
        .tool_runtime(runtime)
        .host_network(host_network);
    let mut granted: Vec<PathBuf> = Vec::new();
    let masked = masked_paths(roots);
    let mut writable: Vec<PathBuf> = Vec::new();

    let active_grant = workspace_policy
        .and_then(|policy| policy.active_workspace.as_ref())
        .filter(|grant| sandbox_path(&grant.path) == working_directory);
    let active_can_write = active_grant.is_none_or(|grant| grant.can_write);
    granted.push(working_directory.clone());
    if active_can_write {
        builder = builder.write_path(&working_directory);
        writable.push(working_directory.clone());
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
            writable.push(path.clone());
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
                writable.push(path.clone());
                granted.push(path);
            } else if grant.can_read {
                builder = builder.read_only_path(&path);
                granted.push(path);
            }
        }
    }

    for cache in shared_caches(roots, &writable, on_path) {
        builder = builder.write_path(&cache);
        writable.push(cache);
    }
    if let Some(directory) = temporary_directory(roots, &writable) {
        builder = builder.write_path(directory);
    }
    for private in &masked {
        builder = builder.masked_path(private);
    }
    for path in ssh_files_without_keys(&roots.home) {
        builder = builder.read_only_path(path);
    }
    let ssh_configuration = ssh_configuration(roots);
    // The operator's own files ssh includes as configuration — host entries
    // kept in `~/.ssh/config.d`, say — show at the names ssh opens them by
    // where a mask hides them.
    for path in &ssh_configuration.included {
        if masked.iter().any(|private| path.starts_with(private)) {
            builder = builder.read_only_path(path);
        }
    }
    let ssh_agent = operator_ssh_agent();
    if let Some(socket) = &ssh_agent {
        builder = builder.read_only_path(socket);
    }
    // The shipped skills' files, which their instructions tell the agent to
    // read and run, are compiled into the gateway and written beneath its
    // masked data root: every one is readable, none writable.
    let shipped_skills = crate::shipped_skill_projection::root(&roots.data_root);
    if shipped_skills.is_dir() {
        builder = builder.read_only_path(sandbox_path(&shipped_skills));
    }
    tracing::debug!(?granted, "sandbox policy grants");

    if let Some(directory) = &agent_config.tool_output_directory {
        let directory = sandbox_path(directory);
        builder = builder
            .write_path(&directory)
            .tool_output_directory(directory);
    }
    let mut policy = builder
        .build()
        .map_err(|error| swage_core::SandboxError::SpawnFailed {
            message: format!("failed to build sandbox config: {error}"),
        })?;
    policy.substituted_files = ssh_configuration.substitutes;
    if let Some(socket) = ssh_agent {
        policy
            .environment
            .insert("SSH_AUTH_SOCK".into(), socket.display().to_string());
    }
    // On the host's network a command uses the operator's own proxy, as it
    // would unsandboxed.
    if host_network {
        for key in PROXY_ENVIRONMENT {
            if let Ok(value) = std::env::var(key) {
                policy.environment.insert((*key).into(), value);
            }
        }
    }
    Ok(policy)
}

/// The system's ssh configuration, and the directory its relative includes
/// are read from.
const SYSTEM_SSH_CONFIGURATION: &str = "/etc/ssh/ssh_config";
const SYSTEM_SSH_DIRECTORY: &str = "/etc/ssh";

/// How many configuration files the walk of ssh's includes reads at most.
const SSH_CONFIGURATION_FILE_LIMIT: usize = 256;

/// What the sandbox shows of ssh's configuration beyond `~/.ssh/config`
/// and the system's own file.
struct SshConfiguration {
    /// Every file the configuration includes, at the path ssh opens it by,
    /// except a private key.
    included: Vec<PathBuf>,
    /// The included files the gateway's user does not own, each with a copy
    /// they own to show in its place.
    substitutes: Vec<swage_sandbox::SubstitutedFile>,
}

/// The files ssh's configuration includes, walked from the system's file
/// and the operator's `~/.ssh/config`.
///
/// Inside the sandbox's user namespace the host's root-owned files show as
/// owned by nobody, and OpenSSH refuses an included configuration file owned
/// by anyone but root or the user — the system's drop-ins, or a file the
/// operator's own configuration includes — so ssh, and git over ssh, would
/// stop at the first such include. A copy with the same content, owned by
/// the user, reads as theirs. The copies are kept beneath the gateway's data
/// root, which the sandbox masks, and are refreshed each time a policy is
/// built.
fn ssh_configuration(roots: &PolicyRoots) -> SshConfiguration {
    use std::os::unix::fs::MetadataExt;

    // Safety: geteuid has no preconditions and cannot fail.
    let user = unsafe { libc::geteuid() };
    let user_directory = roots.home.join(".ssh");
    let copies = roots.data_root.join("sandbox").join("ssh-configuration");
    // Each file with the directory its relative includes are read from, and
    // whether it was included (the top-level files are never checked).
    let mut pending = vec![
        (
            PathBuf::from(SYSTEM_SSH_CONFIGURATION),
            PathBuf::from(SYSTEM_SSH_DIRECTORY),
            false,
        ),
        (user_directory.join("config"), user_directory.clone(), false),
    ];
    // Each file walked, by its own path, with whether it shows as included
    // configuration.
    let mut read: Vec<(PathBuf, bool)> = Vec::new();
    let mut configuration = SshConfiguration {
        included: Vec::new(),
        substitutes: Vec::new(),
    };
    // The link's own name in a directory resolved as the sandbox sees it,
    // so a linked file shows where ssh looks for it.
    let show = |included: &mut Vec<PathBuf>, opened: &Path| {
        if let (Some(directory), Some(name)) = (opened.parent(), opened.file_name()) {
            let path = sandbox_path(directory).join(name);
            if !included.contains(&path) {
                included.push(path);
            }
        }
    };
    while let Some((opened, base, included)) = pending.pop() {
        let Ok(file) = opened.canonicalize() else {
            continue;
        };
        // A file already walked is not walked again, but every name an
        // include opens it by shows, as ssh may open it by any of them.
        if let Some((_, shown)) = read.iter().find(|(seen, _)| *seen == file) {
            if included && *shown {
                show(&mut configuration.included, &opened);
            }
            continue;
        }
        if read.len() >= SSH_CONFIGURATION_FILE_LIMIT {
            continue;
        }
        let Ok(content) = std::fs::read(&file) else {
            read.push((file, false));
            continue;
        };
        // A private key is never configuration, whatever an include matches.
        let shown = included && !String::from_utf8_lossy(&content).contains("PRIVATE KEY-----");
        read.push((file.clone(), shown));
        for pattern in ssh_includes(&String::from_utf8_lossy(&content)) {
            let Some(pattern) = expand_ssh_environment(&pattern) else {
                continue;
            };
            let pattern = if let Some(rest) = pattern.strip_prefix("~/") {
                roots.home.join(rest)
            } else {
                base.join(pattern)
            };
            let Ok(matches) = glob::glob(&pattern.to_string_lossy()) else {
                continue;
            };
            for path in matches.filter_map(Result::ok).filter(|path| path.is_file()) {
                pending.push((path, base.clone(), true));
            }
        }
        if !shown {
            continue;
        }
        show(&mut configuration.included, &opened);
        let owned = std::fs::metadata(&file).is_ok_and(|metadata| metadata.uid() == user);
        if owned {
            continue;
        }
        let copy = copies.join(file.strip_prefix("/").unwrap_or(&file));
        match write_owned_copy(&copy, &content) {
            Ok(()) => configuration
                .substitutes
                .push(swage_sandbox::SubstitutedFile {
                    path: file,
                    source: copy,
                }),
            Err(error) => tracing::warn!(
                path = %file.display(),
                %error,
                "the ssh configuration file could not be copied; sandboxed ssh will refuse it"
            ),
        }
    }
    configuration
}

/// The patterns of every `Include` directive in an ssh configuration, each
/// a word as ssh splits it: quotes group, a backslash escapes, and an
/// unquoted `#` starting a word ends the line.
fn ssh_includes(configuration: &str) -> Vec<String> {
    configuration
        .lines()
        .flat_map(|line| {
            let line = line.trim_start();
            let (keyword, rest) = line.split_at(line.find([' ', '\t', '=']).unwrap_or(line.len()));
            if keyword.eq_ignore_ascii_case("include") {
                ssh_words(rest.trim_start_matches([' ', '\t', '=']))
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn ssh_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote: Option<char> = None;
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (None, ' ' | '\t') => words.extend(word.take()),
            (None, '#') if word.is_none() => break,
            (None, '"' | '\'') => {
                quote = Some(character);
                word.get_or_insert_default();
            }
            (Some(open), _) if character == open => quote = None,
            (Some('\''), _) => word.get_or_insert_default().push(character),
            (_, '\\') => {
                if let Some(escaped) = characters.next() {
                    word.get_or_insert_default().push(escaped);
                }
            }
            _ => word.get_or_insert_default().push(character),
        }
    }
    words.extend(word);
    words
}

/// An `Include` pattern with its `${NAME}` references replaced from the
/// gateway's environment, as ssh replaces them from its own; `None` where a
/// reference names an unset variable, which ssh refuses.
fn expand_ssh_environment(pattern: &str) -> Option<String> {
    let mut expanded = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let end = rest[start..].find('}')? + start;
        expanded.push_str(&std::env::var(&rest[start + 2..end]).ok()?);
        rest = &rest[end + 1..];
    }
    expanded.push_str(rest);
    Some(expanded)
}

/// Write `content` to `path` as a file the user owns that only they may
/// write, replacing what is there only when it differs.
fn write_owned_copy(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    use std::os::unix::fs::PermissionsExt;
    let directory = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(directory)?;
    // Staged beside it and renamed over it, so a turn building its policy
    // at the same time never reads half a file.
    let staged = directory.join(format!(".{}.staged", Uuid::new_v4()));
    std::fs::write(&staged, content)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&staged, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// The operator's network proxy settings, which a command on the host's
/// network inherits.
const PROXY_ENVIRONMENT: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// The operator's SSH agent socket, when one is running: a sandboxed ssh
/// authenticates through it and never reads a key.
fn operator_ssh_agent() -> Option<PathBuf> {
    let socket = PathBuf::from(std::env::var_os("SSH_AUTH_SOCK")?);
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(&socket)
        .is_ok_and(|metadata| metadata.file_type().is_socket())
        .then_some(socket)
}

/// The files under the masked `~/.ssh` that hold no key and that ssh needs
/// to reach a forge: its configuration, the known hosts and the public
/// halves of the operator's keys. Each reopens read-only through the mask
/// at its own name, a symbolic link — as dotfile managers leave them —
/// showing the file it points at.
pub(crate) fn ssh_files_without_keys(home: &Path) -> Vec<PathBuf> {
    let directory = sandbox_path(&home.join(".ssh"));
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| directory.join(entry.file_name()))
        .filter(|path| path.is_file())
        .filter(|path| {
            // Exactly ssh's own names: a public half is `<key>.pub`.
            path.extension().is_some_and(|extension| extension == "pub")
                || path.file_name().is_some_and(|name| {
                    name == "config" || name == "known_hosts" || name == "known_hosts2"
                })
        })
        .collect();
    files.sort();
    files
}

/// The private paths a turn's sandbox masks, each only where it exists: the
/// operator's secrets wherever their environment keeps them, the host's
/// runtime directory — the operator's session sockets, which a sandboxed
/// command's own runtime directory replaces — and the gateway's own data
/// root — its store, secrets and token — masked unless a grant names it
/// (C64). A grant
/// beneath a mask (the managed scratch workspace lives under the data root)
/// reopens only its own subtree: the mount layer applies the deeper path
/// last, so the grant shows through the mask without the rest; a grant equal
/// to the root, or to any masked private path, binds after the mask and
/// reopens what it names.
fn masked_paths(roots: &PolicyRoots) -> Vec<PathBuf> {
    let mut masked: Vec<PathBuf> = Vec::new();
    for path in MASKED_PLACES
        .iter()
        .flat_map(|place| roots.every_location(place))
        .chain(
            roots
                .environment
                .path("XDG_RUNTIME_DIR")
                .map(Path::to_path_buf),
        )
        .filter(|path| path.exists())
        .map(|path| sandbox_path(&path))
    {
        if !masked.contains(&path) {
            masked.push(path);
        }
    }
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
    egress: Option<swage_sandbox::EgressSetup>,
    credentials: Option<&crate::git_credential::CredentialServer>,
    signer: Option<&crate::git_signer::SignerServer>,
) -> Result<TurnSandboxBuild, swage_core::SandboxError> {
    if let Some(directory) = &agent_config.tool_output_directory {
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(|error| swage_core::SandboxError::SpawnFailed {
                message: format!(
                    "failed to create tool output directory {}: {error}",
                    directory.display()
                ),
            })?;
    }

    if !config.sandbox.enabled {
        return Ok(TurnSandboxBuild {
            sandbox: None,
            isolation: ExecutionIsolation::SandboxDisabled,
            backend: SandboxBackend::None,
        });
    }
    #[cfg(test)]
    if state.test_sandbox_enabled() {
        let roots = PolicyRoots::from_environment(&state.config_root)?;
        let config = build_sandbox_config(agent_config, workspace_policy, &roots, config)?;
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
    egress: Option<swage_sandbox::EgressSetup>,
    credentials: Option<&crate::git_credential::CredentialServer>,
    signer: Option<&crate::git_signer::SignerServer>,
    bubblewrap_available: bool,
    spawn_fallback: bool,
) -> Result<TurnSandboxBuild, swage_core::SandboxError> {
    let roots = PolicyRoots::from_environment(&state.config_root)?;
    let mut sandbox_config = build_sandbox_config(agent_config, workspace_policy, &roots, config)?;

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
            swage_sandbox::BubblewrapSandbox::new(sandbox_config.clone(), make_llm_handler());
        if let Some(egress) = egress.clone() {
            sandbox = sandbox.with_egress(egress);
        }
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
    let mut fallback = swage_sandbox::FallbackSandbox::new(sandbox_config, make_llm_handler());
    if let Some(egress) = egress {
        fallback = fallback.with_egress(egress);
    }
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
        call_hook,
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
    // A plugin's tools are withheld the same way from a scope that does not
    // enable the plugin.
    let base_dispatcher = state.active_tool_dispatcher();
    let registry = Arc::new(
        base_dispatcher
            .registry()
            .for_agent(&agent_config.agent_id)
            .withholding(&state.plugins.withheld_tool_names(topic_id)),
    );
    let dispatcher = ToolDispatcher::new(
        Arc::clone(&registry),
        permissions.clone(),
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );
    // The plugin hooks the turn's revision attaches at `tool_call` see
    // every call the model raises before the gates — a rewrite is what the
    // gates then check, as if the model had raised it.
    let dispatcher = match call_hook {
        Some(hook) => dispatcher.with_call_hook(hook),
        None => dispatcher,
    };

    // Where the person has put sandboxed commands behind the recording
    // proxy, what they may reach and the record of it: the listed hosts,
    // then the judge, every attempt an audit event of this session and
    // turn. Otherwise no proxy runs: the network is the host's, or none.
    let egress = (config.sandbox.network == swage_config::SandboxNetwork::Proxy).then(|| {
        crate::egress::turn_egress_setup(
            &config.sandbox.egress_hosts,
            Some(Arc::new(crate::request_judge::GatewayRequestJudge::new(
                state.clone(),
            ))),
            state.storage.clone(),
            session_id,
            turn_id,
            &agent_config.agent_id,
        )
    });
    let credentials = crate::git_credential::CredentialServer::start(
        state.storage.clone(),
        session_id,
        turn_id,
        Some(agent_config.working_directory.clone()),
    )?;
    // The operator's commit signing, forwarded to the host: present when
    // the operator signs, as the workspace's git configuration says; a
    // configuration that signs nothing leaves sandboxed git signing nothing.
    let signer = match crate::git_signer::SigningConfig::from_host_git(
        &agent_config.working_directory,
    )
    .await
    {
        Some(signing) => {
            // What this turn's sandbox may write, so that the host never
            // runs a signing program the sandbox could have replaced.
            let writable = PolicyRoots::from_environment(&state.config_root)
                .ok()
                .and_then(|roots| {
                    build_sandbox_config(agent_config, workspace_policy.as_ref(), &roots, config)
                        .ok()
                })
                .map(|policy| policy.write_paths)
                .unwrap_or_default();
            Some(crate::git_signer::SignerServer::start(
                state.storage.clone(),
                session_id,
                turn_id,
                signing.refusing_programs_under(&writable),
            )?)
        }
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
    // Python runs under the turn's own policy wherever the turn is
    // sandboxed, and wherever Python must be although the turn's sandbox
    // degraded. A turn whose sandbox the person switched off runs Python on
    // the host as it runs the shell; so does a degraded turn where Python
    // need not be sandboxed.
    let python_config = if config.tools.python_execute.enabled
        && sandbox_build.isolation != ExecutionIsolation::SandboxDisabled
        && (config.tools.python_execute.require_sandbox
            || sandbox_build.backend == SandboxBackend::Bubblewrap)
    {
        let roots = PolicyRoots::from_environment(&state.config_root)?;
        let mut policy =
            build_sandbox_config(agent_config, workspace_policy.as_ref(), &roots, config)?;
        credentials.configure(&mut policy)?;
        if let Some(signer) = &signer {
            signer.configure(&mut policy)?;
        }
        Some(policy)
    } else {
        None
    };
    // A process started on the session's behalf — a plugin's brokered
    // process — runs where the turn's commands run: inside the same sandbox
    // policy under bubblewrap, plainly where the turn itself has no
    // boundary (declared, disabled, or the permitted fallback).
    let process_placement = Arc::new(match sandbox_build.backend {
        SandboxBackend::Bubblewrap | SandboxBackend::Fallback => {
            let roots = PolicyRoots::from_environment(&state.config_root)?;
            let config =
                build_sandbox_config(agent_config, workspace_policy.as_ref(), &roots, config)?;
            if sandbox_build.backend == SandboxBackend::Bubblewrap {
                crate::plugin::process::ProcessPlacement::sandboxed(config)
            } else {
                crate::plugin::process::ProcessPlacement::fallback(config)
            }
        }
        SandboxBackend::None | SandboxBackend::TestHarness => {
            crate::plugin::process::ProcessPlacement::plain()
        }
    });
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

    // Calls a host-side tool makes on the session's behalf — a plugin's
    // `callTool` — run through a dispatcher of the same registry and
    // permissions over a strategy of the same shape, so they meet the gates
    // the model's own calls meet. It carries no session dispatcher of its
    // own: a nested call does not nest further. Nor does it carry the
    // `tool_call` hook: that point sees the model's calls, and a plugin's
    // own call waiting on a hook served by the same isolate — which runs
    // one invocation at a time — would wait on itself.
    let session_dispatcher_base = ToolDispatcher::new(
        registry,
        permissions.clone(),
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );
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
    // A Python program's calls run under the answer already given for the
    // `python_execute` call that runs it — the person's approval, or the
    // turn's pass that asked nothing — so they keep the turn's grants and
    // denials and ask nothing again.
    let nested_permissions = swage_tool::permission::PermissionConfig {
        require_approval: Vec::new(),
        ..permissions
    };
    let nested_dispatcher = ToolDispatcher::new(
        Arc::new(nested_registry),
        nested_permissions,
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
    // A turn the person runs without the sandbox confines nothing, its
    // Python's network included.
    let python_egress =
        egress.filter(|_| sandbox_build.isolation != ExecutionIsolation::SandboxDisabled);
    let python_execution = crate::tool_executor::PythonExecution {
        sandbox: python_config,
        egress: python_egress,
        dispatcher: Arc::new(nested_dispatcher),
        credentials,
        signer,
    };

    let session_dispatcher = Arc::new(
        session_dispatcher_base.with_execution_strategy(Arc::new(
            GatewayExecutionStrategy::new(
                inner_strategy.clone(),
                ws_tx.clone(),
                device.clone(),
                state.secret_manager.clone(),
                state.storage.clone(),
                Some(state.clone()),
            )
            .with_process_placement(Arc::clone(&process_placement)),
        )),
    );
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
            .with_python_execution(python_execution)
            .with_session_dispatcher(session_dispatcher)
            .with_process_placement(process_placement),
        )),
        sandbox_guard,
        isolation: sandbox_build.isolation,
    })
}

/// Record in the audit trail that a build's tools run without a sandbox:
/// `execution_sandbox_disabled` where the configuration switched it off,
/// `execution_isolation_degraded` with its reason where a sandbox meant to
/// run could not. A sandboxed build records nothing here.
pub(crate) async fn record_unsandboxed_execution(
    state: &AppState,
    isolation: &ExecutionIsolation,
    execution_context: &'static str,
    session_id: Option<Uuid>,
    turn_id: Option<Uuid>,
) -> Result<(), swage_core::GatewayError> {
    let (id_prefix, event_kind, summary, metadata) = match isolation {
        ExecutionIsolation::Isolated => return Ok(()),
        ExecutionIsolation::SandboxDisabled => (
            "sandbox-disabled",
            "execution_sandbox_disabled",
            "Execution ran with the sandbox switched off by configuration",
            serde_json::json!({ "execution_context": execution_context }),
        ),
        ExecutionIsolation::Degraded { reason } => (
            "sandbox-degradation",
            "execution_isolation_degraded",
            "Execution isolation was degraded",
            serde_json::json!({
                "execution_context": execution_context,
                "reason": reason,
            }),
        ),
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
            id: format!("{id_prefix}-{}", Uuid::new_v4()),
            session_id,
            entity_kind: entity_kind.into(),
            entity_id,
            event_kind: event_kind.into(),
            summary: summary.into(),
            metadata,
        })
        .await?;
    Ok(())
}

/// The dispatcher's call hook over the `tool_call` attachments a turn
/// resolved, in the revision's order, each rewrite recorded to the audit
/// trail under `session_id`; none when nothing attached serves.
pub(crate) fn call_hook_for(
    state: &AppState,
    hooks: Vec<(
        swage_protocol::client::turn_graph::HookMode,
        crate::plugin::slot::PluginSlot,
    )>,
    session_id: Option<Uuid>,
) -> Option<Arc<dyn swage_tool::dispatch::ToolCallHook>> {
    (!hooks.is_empty()).then(|| {
        Arc::new(crate::plugin::slot::PluginCallHook::new(
            hooks,
            Some(crate::plugin::slot::audit_rewrite_recorder(
                state.clone(),
                session_id,
            )),
        )) as Arc<dyn swage_tool::dispatch::ToolCallHook>
    })
}

/// The tools the model will be offered on this turn, by name, read before
/// the turn's dispatcher exists — context is assembled first, and text in it
/// that names a tool must not name one the model cannot call. The gates are
/// the ones the turn applies: the tool registered for the agent
/// (`ToolRegistry::for_agent`), permitted by the resolved config with this
/// session's approval grants (`build_permission_config`, as the dispatcher
/// build and the Codex launch build it), and admitted by the device
/// advertisement filter for the session's host
/// (`build_gateway_advertisement_filter`). A granted tool that still needs
/// approval is offered by the agent runtime, which pauses for the approval,
/// and withheld by the Codex runtime (`codex::dynamic_tools`), so under the
/// Codex protocol only an outright `Allowed` answer offers it. Where the
/// dispatcher already exists and the agent runtime will run it, ask it
/// (`ToolDispatcher::offered_tools`) instead.
pub(crate) async fn turn_offered_tools(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    agent_config: &AgentConfig,
    session_metadata: Option<&serde_json::Value>,
    topic_id: Option<Uuid>,
    session_id: Option<Uuid>,
    session_approval_grants: &std::collections::HashSet<crate::state::ApprovalGrantKey>,
) -> swage_tool::registry::OfferedTools {
    use swage_tool::permission::{PermissionCheckResult, check_permission};

    let registry = state
        .active_tool_dispatcher()
        .registry()
        .for_agent(&agent_config.agent_id);
    let permissions = build_permission_config(
        config,
        &state.config,
        topic_id,
        session_id,
        session_approval_grants,
    );
    let approval_offers = config.default_provider_protocol() != swage_config::Protocol::Codex;
    let device = crate::device::resolve_device(state, session_metadata).await;
    let filter = build_gateway_advertisement_filter(device.as_ref(), agent_config);
    let names = registry
        .list()
        .into_iter()
        .filter(|name| match check_permission(&permissions, name) {
            Ok(PermissionCheckResult::Allowed) => true,
            Ok(PermissionCheckResult::RequiresApproval { .. }) => approval_offers,
            Err(_) => false,
        })
        .filter(|name| {
            registry
                .get(name)
                .is_some_and(|tool| filter.as_ref().is_none_or(|filter| filter(tool.as_ref())))
        })
        .map(str::to_string)
        .collect();
    registry.offered(names)
}

pub(crate) async fn build_gateway_dispatcher_for_unattended_execution(
    state: &AppState,
    request: GatewayDispatcherRequest<'_>,
    execution_context: &'static str,
) -> Result<GatewayDispatcherBuild, swage_core::GatewayError> {
    let session_id = request.session_id;
    let turn_id = request.turn_id;
    // A build made on a turn's behalf takes the revision serving now: it
    // has no turn snapshot of its own to read.
    let request = match request.call_hook {
        Some(_) => request,
        None => {
            let hooks = crate::plugin::slot::hooks_for_turn(
                state,
                &state.turn_graph.active().graph,
                swage_protocol::client::turn_graph::HookPoint::ToolCall,
                &crate::plugin::slot::slot_context(
                    session_id,
                    &request.agent_config.agent_id,
                    &request.agent_config.working_directory,
                ),
                request.topic_id,
            );
            GatewayDispatcherRequest {
                call_hook: call_hook_for(state, hooks.attached, session_id),
                ..request
            }
        }
    };
    let build = build_gateway_dispatcher(state, request)
        .await
        .map_err(|error| swage_core::GatewayError::ConfigError(error.to_string()))?;
    // The request's turn is the one the audit attributes the build to — for a
    // delegated child, the sender's — so no turn row is stamped here: a turn's
    // own boundary is written where its dispatcher settles for it
    // (dispatcher_settlement.rs), never from a build made on its behalf.
    record_unsandboxed_execution(
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
            command.env_remove("HOME");
        } else {
            let runtime = swage_python::runtime_resolver::ResolvedPythonRuntime::resolve(
                &swage_config::model::PythonExecuteToolConfig::default(),
            )
            .unwrap();
            command
                .env("SWAGE_TEST_PYTHON", runtime.executable_path)
                .env("PATH", "");
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
                call_hook: None,
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
    fn sandbox_disabled_python_runs_without_bubblewrap() {
        if !run_dispatcher_environment_case(
            "turn::dispatcher::tests::sandbox_disabled_python_runs_without_bubblewrap",
            false,
        ) {
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            // The person switched the sandbox off: Python runs as the shell
            // does, with the shipped `require_sandbox` left as it is.
            let mut config = swage_config::ResolvedConfig::default();
            config.sandbox.enabled = false;
            assert!(config.tools.python_execute.require_sandbox);
            config.tools.python_execute.enabled = true;
            config.tools.python_execute.warm_target = 0;
            config.tools.python_execute.runtime_path =
                Some(std::env::var("SWAGE_TEST_PYTHON").unwrap());
            config.dangerously_skip_permissions = true;
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
            config.sandbox.network = swage_config::SandboxNetwork::Proxy;
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
                    call_hook: None,
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
    fn test_egress() -> Option<swage_sandbox::EgressSetup> {
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
        Some(swage_sandbox::EgressSetup {
            policy: Arc::new(Closed),
            recorder: Arc::new(Dropped),
        })
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
        record_unsandboxed_execution(
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
    async fn disabled_sandbox_persists_its_degraded_reason() {
        let (state, session_id) = make_state_with_session(Vec::new()).await;
        let workspace = tempfile::tempdir().unwrap();
        let agent_config = AgentConfig::new("gateway")
            .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.enabled = false;
        let turn_id =
            crate::automated_turn::create_placeholder_turn(&state, session_id, "gateway", "Hello")
                .await
                .unwrap();

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
        .unwrap();
        record_turn_isolation(&state, &build.isolation, turn_id)
            .await
            .unwrap();

        let turn = state.storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(
            turn.execution_isolation,
            Some(swage_storage::TurnIsolation::SandboxDisabled),
            "a sandbox switched off by configuration is recorded as that, not as degraded"
        );
        assert_eq!(build.backend, SandboxBackend::None);
    }

    #[tokio::test]
    async fn disabled_gateway_sandbox_needs_no_degraded_execution_opt_in() {
        let (state, _session_id) = make_state_with_session(Vec::new()).await;
        let workspace = tempfile::tempdir().unwrap();
        let agent_config = AgentConfig::new("gateway")
            .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.enabled = false;
        assert!(!config.sandbox.allow_degraded_execution);

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
        .expect("switching the sandbox off is the choice itself; no opt-in is needed");

        assert_eq!(build.isolation, ExecutionIsolation::SandboxDisabled);
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
        record_unsandboxed_execution(
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
                call_hook: None,
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
                call_hook: None,
            },
            "delegated turn",
        )
        .await;

        let build = result.expect("explicit sandbox disablement should construct a dispatcher");
        assert_eq!(build.isolation, ExecutionIsolation::SandboxDisabled);
        let events = state.storage.list_audit_events().await.unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_kind.as_str())
                .collect::<Vec<_>>(),
            ["execution_sandbox_disabled"],
            "a sandbox switched off on purpose is recorded as that and never as degraded"
        );
    }

    /// A cache, a credential store, the temporary directory or the runtime
    /// directory the operator's environment moves out of the home is where
    /// the policy opens or masks it.
    #[test]
    fn places_the_environment_moves_are_opened_and_masked_where_it_puts_them() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let elsewhere = root.path().join("elsewhere");
        let cache = elsewhere.join("cache");
        let tool_caches = [
            ("UV_CACHE_DIR", elsewhere.join("uv")),
            ("GOCACHE", elsewhere.join("go-build")),
            ("GOMODCACHE", elsewhere.join("go-mod")),
        ];
        let config = elsewhere.join("config");
        let data = elsewhere.join("data");
        let gnupg = elsewhere.join("gnupg");
        let aws = elsewhere.join("aws-credentials");
        let kube = [elsewhere.join("kube-a"), elsewhere.join("kube-b")];
        let temporary = elsewhere.join("tmp");
        let runtime = elsewhere.join("run");
        for directory in [
            &home.join(".gnupg"),
            &cache,
            &config.join("gh"),
            &config.join("forgejo"),
            &data.join("keyrings"),
            &gnupg,
            &temporary,
            &runtime,
        ]
        .into_iter()
        .chain(tool_caches.iter().map(|(_, path)| path))
        {
            std::fs::create_dir_all(directory).unwrap();
        }
        for file in [&config.join("gh/hosts.yml"), &aws, &kube[0], &kube[1]] {
            std::fs::write(file, "secret").unwrap();
        }
        let workspace = home.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut environment: LocationEnvironment = [
            ("XDG_CACHE_HOME", cache.clone()),
            ("XDG_CONFIG_HOME", config.clone()),
            ("XDG_DATA_HOME", data.clone()),
            ("GNUPGHOME", gnupg.clone()),
            ("AWS_SHARED_CREDENTIALS_FILE", aws.clone()),
            ("KUBECONFIG", kube[0].clone()),
            ("KUBECONFIG", kube[1].clone()),
            ("TMPDIR", temporary.clone()),
            ("XDG_RUNTIME_DIR", runtime.clone()),
        ]
        .into();
        for (variable, path) in &tool_caches {
            environment.0.insert(variable, vec![path.clone()]);
        }
        let roots = PolicyRoots {
            home: home.clone(),
            data_root: home.join(".swage"),
            environment,
        };
        let agent_config =
            AgentConfig::new("worker").with_workspace(&workspace, vec![workspace.clone()]);
        let policy = build_sandbox_config(
            &agent_config,
            None,
            &roots,
            &swage_config::ResolvedConfig::default(),
        )
        .unwrap();

        for writable in [&cache, &temporary]
            .into_iter()
            .chain(tool_caches.iter().map(|(_, path)| path))
        {
            assert!(
                policy.write_paths.contains(&sandbox_path(writable)),
                "{writable:?} is writable: {:?}",
                policy.write_paths
            );
        }
        assert!(
            !home.join(".cache").exists(),
            "the cache the environment moved is not made in the home"
        );
        for masked in [
            &config.join("gh/hosts.yml"),
            &config.join("forgejo"),
            &data.join("keyrings"),
            &gnupg,
            &home.join(".gnupg"),
            &aws,
            &kube[0],
            &kube[1],
            &runtime,
        ] {
            assert!(
                policy.masked_paths.contains(&sandbox_path(masked)),
                "{masked:?} is masked: {:?}",
                policy.masked_paths
            );
        }
    }

    /// A link a command plants where a cache would be, inside a cache or a
    /// workspace it already writes, never turns into a grant of the link's
    /// target on the next turn.
    #[test]
    fn a_link_planted_in_a_writable_path_is_never_granted_its_target() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = home.join("project");
        let ssh = home.join(".ssh");
        for directory in [&home.join(".cache"), &home.join("go/pkg"), &workspace, &ssh] {
            std::fs::create_dir_all(directory).unwrap();
        }
        for planted in [
            home.join(".cache/uv"),
            home.join(".cache/go-build"),
            home.join("go/pkg/mod"),
            workspace.join("tmp"),
        ] {
            std::os::unix::fs::symlink(&ssh, planted).unwrap();
        }
        let roots = PolicyRoots {
            home: home.clone(),
            data_root: home.join(".swage"),
            environment: [("TMPDIR", workspace.join("tmp"))].into(),
        };
        let agent_config =
            AgentConfig::new("worker").with_workspace(&workspace, vec![workspace.clone()]);
        let policy = build_sandbox_config(
            &agent_config,
            None,
            &roots,
            &swage_config::ResolvedConfig::default(),
        )
        .unwrap();

        assert!(
            !policy.write_paths.contains(&sandbox_path(&ssh)),
            "{:?}",
            policy.write_paths
        );
        assert!(policy.masked_paths.contains(&sandbox_path(&ssh)));
        assert!(
            policy
                .write_paths
                .contains(&sandbox_path(&home.join(".cache")))
        );
    }

    /// ssh's configuration and known hosts linked into `~/.ssh` from
    /// elsewhere, as dotfile managers leave them, reopen at their own names;
    /// a key does not.
    #[test]
    fn ssh_files_linked_into_place_reopen_at_their_own_names() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let dotfiles = root.path().join("dotfiles");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(&dotfiles).unwrap();
        for name in ["config", "known_hosts"] {
            std::fs::write(dotfiles.join(name), "Host forge\n").unwrap();
            std::os::unix::fs::symlink(dotfiles.join(name), home.join(".ssh").join(name)).unwrap();
        }
        std::fs::write(dotfiles.join("id_ed25519"), "key").unwrap();
        std::os::unix::fs::symlink(dotfiles.join("id_ed25519"), home.join(".ssh/id_ed25519"))
            .unwrap();

        let ssh = sandbox_path(&home.join(".ssh"));
        assert_eq!(
            ssh_files_without_keys(&home),
            vec![ssh.join("config"), ssh.join("known_hosts")]
        );
    }

    /// In the sandbox a policy builds, ssh reads the operator's
    /// configuration linked into place, and the root-owned files it
    /// includes, as it does outside; and a command
    /// writes the cache and the temporary directory the environment names
    /// and a runtime directory of its own, with the host's out of sight.
    #[tokio::test]
    async fn a_sandboxed_command_finds_ssh_caches_and_runtime_where_the_environment_puts_them() {
        let Ok(bwrap) = swage_sandbox::find_bwrap() else {
            eprintln!("skipping: bwrap is not installed");
            return;
        };
        if !on_path("ssh") {
            eprintln!("skipping: ssh is not installed");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let dotfiles = root.path().join("dotfiles");
        let cache = root.path().join("cache");
        let temporary = root.path().join("tmp");
        let runtime = root.path().join("run");
        let workspace = home.join("project");
        for directory in [
            &home.join(".ssh"),
            &dotfiles,
            &cache,
            &temporary,
            &runtime,
            &workspace,
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        // The system's configuration is root-owned, and includes its
        // drop-ins: an include the sandbox must show as the user's, here
        // reached through a link at a quoted path with a space in it.
        // In the workspace, so the sandbox sees the link itself.
        let linked = workspace.join("system ssh");
        std::os::unix::fs::symlink(SYSTEM_SSH_CONFIGURATION, &linked).unwrap();
        // The operator's own host entries, kept in `~/.ssh/config.d` and
        // included from the configuration; one linked into place.
        std::fs::create_dir_all(home.join(".ssh/config.d")).unwrap();
        std::fs::write(
            home.join(".ssh/config.d/extra"),
            "Host extra\n  Port 3333\n",
        )
        .unwrap();
        std::fs::write(dotfiles.join("linked"), "Host linked\n  Port 4444\n").unwrap();
        std::os::unix::fs::symlink(dotfiles.join("linked"), home.join(".ssh/config.d/linked"))
            .unwrap();
        // One file linked under two names, each included from its own host.
        std::fs::write(dotfiles.join("shared"), "Port 5555\n").unwrap();
        for name in ["first", "second"] {
            std::os::unix::fs::symlink(dotfiles.join("shared"), home.join(".ssh").join(name))
                .unwrap();
        }
        std::fs::write(
            dotfiles.join("ssh_config"),
            format!(
                "Include {}/*\nHost forge\n  Port 2222\nInclude \"{}\"\n\
                 Host first\n  Include {}\nHost second\n  Include {}\n",
                home.join(".ssh/config.d").display(),
                linked.display(),
                home.join(".ssh/first").display(),
                home.join(".ssh/second").display(),
            ),
        )
        .unwrap();
        std::os::unix::fs::symlink(dotfiles.join("ssh_config"), home.join(".ssh/config")).unwrap();
        std::fs::write(runtime.join("session.sock"), "").unwrap();
        let roots = PolicyRoots {
            home: home.clone(),
            data_root: home.join(".swage"),
            environment: [
                ("XDG_CACHE_HOME", cache.clone()),
                ("TMPDIR", temporary.clone()),
                ("XDG_RUNTIME_DIR", runtime.clone()),
            ]
            .into(),
        };
        let agent_config =
            AgentConfig::new("worker").with_workspace(&workspace, vec![workspace.clone()]);
        let policy = build_sandbox_config(
            &agent_config,
            None,
            &roots,
            &swage_config::ResolvedConfig::default(),
        )
        .unwrap();
        // `.host` is a name the systemd drop-in, where installed, matches.
        let hosts = ["forge", ".host", "extra", "linked", "first", "second"];
        let mut outside = Vec::new();
        for host in hosts {
            let output = tokio::process::Command::new("ssh")
                .arg("-F")
                .arg(home.join(".ssh/config"))
                .args(["-G", host])
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .unwrap();
            assert!(output.status.success());
            outside.push(output.stdout);
        }
        let script = format!(
            "ssh -F {config} -G forge > {cache}/forge && ssh -F {config} -G .host > {cache}/.host \
             && ssh -F {config} -G extra > {cache}/extra \
             && ssh -F {config} -G linked > {cache}/linked \
             && ssh -F {config} -G first > {cache}/first \
             && ssh -F {config} -G second > {cache}/second && touch {cache}/written && TMPDIR={temporary} mktemp >/dev/null \
             && mkdir \"$XDG_RUNTIME_DIR/pnpm\" && test ! -e {runtime}/session.sock",
            config = home.join(".ssh/config").display(),
            cache = cache.display(),
            temporary = temporary.display(),
            runtime = runtime.display(),
        );
        let mut command = swage_sandbox::policy_command(&bwrap, &policy, &[], &[], None).unwrap();
        let output = command
            .arg("--")
            .args(["sh", "-c", &script])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(cache.join("written").is_file());
        for (host, outside) in hosts.into_iter().zip(&outside) {
            let inside = std::fs::read(cache.join(host)).unwrap();
            assert_eq!(
                String::from_utf8_lossy(&inside),
                String::from_utf8_lossy(outside),
                "ssh resolves {host} in the sandbox as it does outside"
            );
        }
        assert!(String::from_utf8_lossy(&outside[0]).contains("port 2222"));
        assert!(String::from_utf8_lossy(&outside[2]).contains("port 3333"));
        assert!(String::from_utf8_lossy(&outside[3]).contains("port 4444"));
        assert!(String::from_utf8_lossy(&outside[4]).contains("port 5555"));
        assert!(String::from_utf8_lossy(&outside[5]).contains("port 5555"));
    }

    /// A file the operator's configuration includes shows in the sandbox,
    /// and a private key an include happens to match does not.
    #[test]
    fn an_included_private_key_is_never_shown_as_configuration() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let included = home.join(".ssh/config.d");
        std::fs::create_dir_all(&included).unwrap();
        std::fs::write(
            home.join(".ssh/config"),
            format!("Include {}/*\n", included.display()),
        )
        .unwrap();
        std::fs::write(included.join("hosts"), "Host forge\n").unwrap();
        std::fs::write(
            included.join("id_forge"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nkey\n-----END OPENSSH PRIVATE KEY-----\n",
        )
        .unwrap();
        let roots = PolicyRoots {
            home: home.clone(),
            data_root: home.join(".swage"),
            environment: Default::default(),
        };

        let shown = ssh_configuration(&roots).included;

        let included = sandbox_path(&included);
        assert!(shown.contains(&included.join("hosts")), "{shown:?}");
        assert!(!shown.contains(&included.join("id_forge")), "{shown:?}");
    }

    #[test]
    fn ssh_includes_are_read_from_every_include_directive() {
        assert_eq!(
            ssh_includes(
                "# Include commented\nInclude /etc/ssh/ssh_config.d/*.conf\n\
                 Host forge\n  include=\"config.d/a\" ~/b\nIncludeFoo x\n"
            ),
            vec!["/etc/ssh/ssh_config.d/*.conf", "config.d/a", "~/b"]
        );
        assert_eq!(
            ssh_includes("Include \"/opt/my configs/a.conf\" 'b c' d\\ e ${HOME}/f # g\n"),
            vec!["/opt/my configs/a.conf", "b c", "d e", "${HOME}/f"]
        );
    }

    #[test]
    fn an_ssh_include_reads_environment_references_from_the_environment() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            expand_ssh_environment("${HOME}/.config/ssh").as_deref(),
            Some(format!("{home}/.config/ssh").as_str())
        );
        assert_eq!(
            expand_ssh_environment("${SWAGE_SURELY_UNSET_VARIABLE}/x"),
            None
        );
    }
}
