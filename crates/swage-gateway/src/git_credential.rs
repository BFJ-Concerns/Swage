//! HTTP(S) git credentials for sandboxed commands, resolved on the host.
//!
//! A sandboxed git's only credential helper asks the gateway over a socket,
//! and the gateway answers from the operator's own git credential helpers,
//! run on the host with the operator's configuration. The sandbox never
//! reads the files those helpers keep their credentials in, and no secret
//! registered with the gateway is ever an answer: a governed secret never
//! reaches a sandboxed command.

use std::{
    io::{BufRead, Write},
    sync::Arc,
    time::Duration,
};
use swage_protocol::hosting::{GitCredentialRequest, GitCredentialResponse};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const SOCKET_ENV: &str = "SWAGE_GIT_CREDENTIAL_SOCKET";

/// How long one lookup may take, the operator's helper included: long
/// enough for a helper that unlocks a keyring, short enough that a helper
/// waiting on a person cannot hold the sandboxed git for ever.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(15);

/// Owns a turn-scoped IPC listener; dropping it stops service and removes it.
pub struct CredentialServer {
    socket: swage_sandbox::SocketManager,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for CredentialServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl CredentialServer {
    /// Serve lookups for one turn. `workspace` is the turn's working
    /// directory: the operator's configuration is read as it applies in the
    /// repository asking, within the workspace, so a helper their global
    /// configuration selects for it (`includeIf "gitdir:..."`) answers, as
    /// it would on the host.
    pub fn start(
        storage: Arc<dyn swage_storage::Storage>,
        session_id: Option<uuid::Uuid>,
        turn_id: Option<uuid::Uuid>,
        workspace: Option<std::path::PathBuf>,
    ) -> Result<Arc<Self>, swage_core::SandboxError> {
        let (socket, listener) = swage_sandbox::SocketManager::create("git")?;
        let task = tokio::spawn(async move {
            // Each exchange is bounded and sequential, so an abandoned caller
            // cannot leave an unowned connection task holding a credential.
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "git credential listener stopped");
                        break;
                    }
                };
                let exchange = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    (&mut stream).take(16 * 1024).read_line(&mut line).await?;
                    let request: GitCredentialRequest =
                        serde_json::from_str(&line).map_err(std::io::Error::other)?;
                    let response = resolve(&request, workspace.as_deref()).await;
                    let outcome = if response.password.is_some() {
                        "resolved"
                    } else {
                        "absent"
                    };
                    // Attribute every lookup, including an absent host, before
                    // releasing any material. A failed record releases nothing.
                    storage.create_audit_event(swage_storage::NewAuditEvent {
                        id: uuid::Uuid::new_v4().to_string(), session_id,
                        entity_kind: "git_credential".into(), entity_id: request.host.clone(),
                        event_kind: "git_credential_lookup".into(),
                        summary: format!("Git credential for {}: {outcome}", request.host),
                        metadata: serde_json::json!({"host": request.host, "protocol": request.protocol,
                            "turn_id": turn_id, "outcome": outcome}),
                    }).await.map_err(std::io::Error::other)?;
                    let mut body = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
                    body.push(b'\n');
                    stream.get_mut().write_all(&body).await
                })
                .await;
                match exchange {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "git credential exchange refused"),
                    Err(_) => tracing::warn!("git credential exchange timed out"),
                }
            }
        });
        Ok(Arc::new(Self { socket, task }))
    }

    pub fn configure(
        &self,
        config: &mut swage_sandbox::SandboxConfig,
    ) -> Result<(), swage_core::SandboxError> {
        config.read_only_paths.push(self.socket.path().to_owned());
        config
            .environment
            .insert(SOCKET_ENV.into(), self.socket.path().display().to_string());
        // The sandboxed git reads the operator's own configuration; the
        // gateway adds only command-scope entries, which take precedence.
        // Inherited selectors are dropped before they are supplied (the
        // sandbox separately allowlists the host environment); the
        // command-scope entries another host service appended (the
        // signer's) are kept and composed with.
        let appended: Vec<(String, String)> = git_config_entries(config);
        config.environment.retain(|key, _| !key.starts_with("GIT_"));
        let binary = swage_sandbox::resolve_agent_binary()?;
        let quoted = format!("'{}'", binary.display().to_string().replace('\'', "'\\''"));
        // Command scope resets the operator's helpers, which would read
        // credential files the sandbox masks, for the one that asks the host,
        // where those helpers run.
        append_git_config(
            config,
            &[
                ("credential.helper", String::new()),
                ("credential.helper", format!("!{quoted} git-credential")),
                ("credential.useHttpPath", "true".to_owned()),
            ],
        );
        let appended: Vec<(&str, String)> = appended
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        append_git_config(config, &appended);
        config.environment.extend([
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            // Git tries askpass before checking GIT_TERMINAL_PROMPT. Override
            // both inherited programs and core.askPass after a helper refusal.
            ("GIT_ASKPASS".into(), "/bin/false".into()),
            ("SSH_ASKPASS".into(), "/bin/false".into()),
        ]);
        Ok(())
    }
}

/// The command-scope git configuration a sandbox's environment carries, in
/// order, as `GIT_CONFIG_COUNT` and its numbered pairs name it.
fn git_config_entries(config: &swage_sandbox::SandboxConfig) -> Vec<(String, String)> {
    let count: usize = config
        .environment
        .get("GIT_CONFIG_COUNT")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (0..count)
        .filter_map(|index| {
            Some((
                config
                    .environment
                    .get(&format!("GIT_CONFIG_KEY_{index}"))?
                    .clone(),
                config
                    .environment
                    .get(&format!("GIT_CONFIG_VALUE_{index}"))?
                    .clone(),
            ))
        })
        .collect()
}

/// Append command-scope git configuration to a sandbox's environment after
/// the entries already there (`GIT_CONFIG_COUNT` and its numbered pairs),
/// so the services that each own part of the sandboxed git's configuration
/// compose rather than overwrite one another.
pub(crate) fn append_git_config(
    config: &mut swage_sandbox::SandboxConfig,
    pairs: &[(&str, String)],
) {
    let mut count: usize = config
        .environment
        .get("GIT_CONFIG_COUNT")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    for (key, value) in pairs {
        config
            .environment
            .insert(format!("GIT_CONFIG_KEY_{count}"), (*key).to_owned());
        config
            .environment
            .insert(format!("GIT_CONFIG_VALUE_{count}"), value.clone());
        count += 1;
    }
    config
        .environment
        .insert("GIT_CONFIG_COUNT".into(), count.to_string());
}

/// The credential the operator's own helpers give for an HTTP(S) request.
async fn resolve(
    request: &GitCredentialRequest,
    workspace: Option<&std::path::Path>,
) -> GitCredentialResponse {
    if !matches!(request.protocol.as_str(), "https" | "http") || request.host.is_empty() {
        return empty_response();
    }
    let entries = match workspace {
        Some(workspace) => operator_credential_entries(&asking_directory(request, workspace)).await,
        None => Vec::new(),
    };
    operator_helper(request, &entries).await
}

/// Where the operator's configuration is read for a lookup: the directory
/// git asked from, a repository the turn works in, when it lies within the
/// workspace; the workspace otherwise. The sandboxed command names the
/// directory, so one outside the workspace — which would select another
/// project's helpers — is not taken.
fn asking_directory(
    request: &GitCredentialRequest,
    workspace: &std::path::Path,
) -> std::path::PathBuf {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    request
        .directory
        .as_deref()
        .and_then(|directory| std::path::Path::new(directory).canonicalize().ok())
        .filter(|directory| directory.starts_with(&workspace))
        .unwrap_or(workspace)
}

/// The `credential.*` settings of the operator's system and global
/// configuration as they apply in `workspace`, conditional includes
/// evaluated there, in the order git reads them. Only those two scopes are
/// read, never the repository's own configuration, which the sandbox can
/// write. Empty when git cannot read them there.
async fn operator_credential_entries(workspace: &std::path::Path) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for scope in ["--system", "--global"] {
        let output = tokio::process::Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["config", scope, "--includes", "--list", "-z"])
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await;
        let Ok(output) = output else { continue };
        if !output.status.success() {
            continue;
        }
        for entry in output.stdout.split(|byte| *byte == 0) {
            let entry = String::from_utf8_lossy(entry);
            let (key, value) = match entry.split_once('\n') {
                Some((key, value)) => (key, value),
                // A key with no value is a boolean set to true.
                None => (entry.as_ref(), "true"),
            };
            if key.starts_with("credential.") {
                entries.push((key.to_owned(), value.to_owned()));
            }
        }
    }
    entries
}

fn empty_response() -> GitCredentialResponse {
    GitCredentialResponse {
        username: None,
        password: None,
    }
}

/// Ask the operator's own credential helpers, as their git on the host
/// would: `git credential fill` with their system and global configuration,
/// never prompting — the gateway has no terminal, and a helper that would
/// ask a person answers nothing. Configuration and a repository carried in
/// the gateway's own environment are not the operator's configuration and
/// are left out, as the sandboxed git leaves them out. So is the repository
/// the gateway was started in: the lookup runs from the root directory, so
/// git discovers no repository whose configuration a sandboxed command could
/// have written. The repository path goes with the request, and the host's
/// git keeps it for the operator's helpers only where their
/// `credential.useHttpPath` asks for it.
async fn operator_helper(
    request: &GitCredentialRequest,
    workspace_entries: &[(String, String)],
) -> GitCredentialResponse {
    let fields = [
        ("protocol", Some(request.protocol.as_str())),
        ("host", Some(request.host.as_str())),
        ("path", request.path.as_deref()),
        ("username", request.username.as_deref()),
    ];
    if fields
        .iter()
        .any(|(_, value)| value.is_some_and(|value| value.contains(['\n', '\r', '\0'])))
    {
        return empty_response();
    }
    let mut input = String::new();
    for (key, value) in fields {
        if let Some(value) = value {
            input.push_str(&format!("{key}={value}\n"));
        }
    }
    input.push('\n');
    let mut command = tokio::process::Command::new("git");
    // Run from the root directory, git reads the operator's files without
    // what they select for the workspace, so their settings as they apply
    // there are given at command scope: an empty helper first clears the
    // list the files gave, then the workspace's list follows whole.
    if !workspace_entries.is_empty() {
        let entries = std::iter::once(("credential.helper", ""))
            .chain(
                workspace_entries
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .enumerate();
        let mut count = 0;
        for (index, (key, value)) in entries {
            command
                .env(format!("GIT_CONFIG_KEY_{index}"), key)
                .env(format!("GIT_CONFIG_VALUE_{index}"), value);
            count = index + 1;
        }
        command.env("GIT_CONFIG_COUNT", count.to_string());
    } else {
        command.env_remove("GIT_CONFIG_COUNT");
    }
    let child = command
        .args(["credential", "fill"])
        .current_dir("/")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, "the operator's git credential helpers could not be run");
            return empty_response();
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes()).await;
    }
    let output = match child.wait_with_output().await {
        Ok(output) if output.status.success() => output,
        _ => return empty_response(),
    };
    let mut response = empty_response();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        match line.split_once('=') {
            Some(("username", value)) => response.username = Some(value.to_owned()),
            Some(("password", value)) => response.password = Some(value.to_owned()),
            _ => {}
        }
    }
    if response.password.is_none() {
        return empty_response();
    }
    response
}

pub async fn lookup(
    socket: &std::path::Path,
    request: &GitCredentialRequest,
) -> std::io::Result<GitCredentialResponse> {
    tokio::time::timeout(EXCHANGE_TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        let mut body = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        body.push(b'\n');
        stream.write_all(&body).await?;
        let mut line = String::new();
        BufReader::new(stream)
            .take(64 * 1024)
            .read_line(&mut line)
            .await?;
        serde_json::from_str(&line).map_err(std::io::Error::other)
    })
    .await
    .map_err(std::io::Error::other)?
}

pub async fn run_helper(operation: &str) -> std::io::Result<()> {
    if operation != "get" {
        return Ok(());
    }
    let mut request = GitCredentialRequest {
        protocol: String::new(),
        host: String::new(),
        path: None,
        username: None,
        directory: std::env::current_dir()
            .ok()
            .map(|directory| directory.display().to_string()),
    };
    for line in std::io::stdin().lock().lines().take(128) {
        let line = line?;
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "protocol" => request.protocol = value.into(),
                "host" => request.host = value.into(),
                "path" => request.path = Some(value.into()),
                "username" => request.username = Some(value.into()),
                _ => {}
            }
        }
    }
    let socket = std::env::var_os(SOCKET_ENV)
        .ok_or_else(|| std::io::Error::other("git credential socket is unavailable"))?;
    let response = lookup(std::path::Path::new(&socket), &request).await?;
    if let (Some(username), Some(password)) = (response.username, response.password) {
        let mut out = std::io::stdout().lock();
        writeln!(out, "username={username}\npassword={password}\n")?;
    }
    Ok(())
}
