//! HTTP(S) git credentials for sandboxed commands, resolved on the host.
//!
//! A sandboxed git's only credential helper asks the gateway over a socket.
//! The gateway answers from a git credential registered with it for the
//! host, and otherwise from the operator's own git credential helpers, run on
//! the host with the operator's configuration. The sandbox never reads the
//! files those helpers keep their credentials in.

use crate::secret::SecretManager;
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

/// Where an answered credential came from, as the audit trail names it.
const SOURCE_REGISTERED: &str = "registered_secret";
const SOURCE_OPERATOR_HELPER: &str = "operator_credential_helper";

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
    pub fn start(
        manager: Arc<SecretManager>,
        storage: Arc<dyn swage_storage::Storage>,
        session_id: Option<uuid::Uuid>,
        turn_id: Option<uuid::Uuid>,
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
                    let resolution = resolve(&manager, storage.as_ref(), &request).await;
                    let (outcome, source) = match &resolution {
                        Ok((response, source)) if response.password.is_some() => {
                            ("resolved", Some(*source))
                        }
                        Ok(_) => ("absent", None),
                        Err(_) => ("failed", None),
                    };
                    // Attribute every lookup, including an absent host, before
                    // releasing any material. A failed record releases nothing.
                    storage.create_audit_event(swage_storage::NewAuditEvent {
                        id: uuid::Uuid::new_v4().to_string(), session_id,
                        entity_kind: "git_credential".into(), entity_id: request.host.clone(),
                        event_kind: "git_credential_lookup".into(),
                        summary: format!("Git credential for {}: {outcome}", request.host),
                        metadata: serde_json::json!({"host": request.host, "protocol": request.protocol,
                            "turn_id": turn_id, "outcome": outcome, "source": source}),
                    }).await.map_err(std::io::Error::other)?;
                    let (response, _) = resolution.map_err(std::io::Error::other)?;
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
        // sandbox separately allowlists the host environment); the ssh
        // command the policy chose, and the command-scope entries another
        // host service appended (the signer's), are kept and composed with.
        let appended: Vec<(String, String)> = git_config_entries(config);
        config
            .environment
            .retain(|key, _| !key.starts_with("GIT_") || key == "GIT_SSH_COMMAND");
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

/// The credential for a request: one registered with the gateway for the
/// host first, then whatever the operator's own helpers give.
async fn resolve(
    manager: &SecretManager,
    storage: &dyn swage_storage::Storage,
    request: &GitCredentialRequest,
) -> Result<(GitCredentialResponse, &'static str), swage_core::GatewayError> {
    if !matches!(request.protocol.as_str(), "https" | "http") || request.host.is_empty() {
        return Ok((empty_response(), SOURCE_REGISTERED));
    }
    if request.protocol == "https"
        && let Some(response) = registered(manager, storage, request).await?
    {
        return Ok((response, SOURCE_REGISTERED));
    }
    Ok((operator_helper(request).await, SOURCE_OPERATOR_HELPER))
}

fn empty_response() -> GitCredentialResponse {
    GitCredentialResponse {
        username: None,
        password: None,
    }
}

async fn registered(
    manager: &SecretManager,
    storage: &dyn swage_storage::Storage,
    request: &GitCredentialRequest,
) -> Result<Option<GitCredentialResponse>, swage_core::GatewayError> {
    for record in storage.list_secrets().await? {
        if record.status != "active"
            || record.material_kind != "git_credential"
            || record.scope_kind != "gateway"
            || !record.metadata["host"]
                .as_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(&request.host))
        {
            continue;
        }
        let username = record.metadata["username"].as_str().unwrap_or("oauth2");
        // The stored account is authoritative for this host and path. A URL
        // username is a hint, not a reason to refuse the governed account;
        // the helper returns the canonical username alongside its password.
        if let Some(path) = record.metadata["path"].as_str()
            && request.path.as_deref() != Some(path)
        {
            continue;
        }
        let secret = manager.resolve(&record.reference).await?;
        if secret.value.contains(['\n', '\r', '\0']) || username.contains(['\n', '\r', '\0']) {
            return Ok(Some(empty_response()));
        }
        return Ok(Some(GitCredentialResponse {
            username: Some(username.into()),
            password: Some(secret.value),
        }));
    }
    Ok(None)
}

/// Ask the operator's own credential helpers, as their git on the host
/// would: `git credential fill` with their system and global configuration,
/// never prompting — the gateway has no terminal, and a helper that would
/// ask a person answers nothing. Configuration and a repository carried in
/// the gateway's own environment are not the operator's configuration and
/// are left out, as the sandboxed git leaves them out. So is the repository
/// path, as git leaves it out unless `credential.useHttpPath` asks for it.
async fn operator_helper(request: &GitCredentialRequest) -> GitCredentialResponse {
    let fields = [
        ("protocol", Some(request.protocol.as_str())),
        ("host", Some(request.host.as_str())),
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
    let child = tokio::process::Command::new("git")
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
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
