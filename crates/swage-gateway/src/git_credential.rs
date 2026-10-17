//! HTTPS git credentials resolved on the host through governed secrets.

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
                let exchange = tokio::time::timeout(Duration::from_secs(5), async {
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    (&mut stream).take(16 * 1024).read_line(&mut line).await?;
                    let request: GitCredentialRequest =
                        serde_json::from_str(&line).map_err(std::io::Error::other)?;
                    let resolution = resolve(&manager, storage.as_ref(), &request).await;
                    let outcome = match &resolution {
                        Ok(response) if response.password.is_some() => "resolved",
                        Ok(_) => "absent",
                        Err(_) => "failed",
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
                    let response = resolution.map_err(std::io::Error::other)?;
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
        // This is the complete Git environment owned by the gateway. Drop
        // existing selectors and command configuration before supplying it;
        // the sandbox separately allowlists the inherited host environment.
        config.environment.retain(|key, _| !key.starts_with("GIT_"));
        let binary = swage_sandbox::resolve_agent_binary()?;
        let quoted = format!("'{}'", binary.display().to_string().replace('\'', "'\\''"));
        config.environment.extend([
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            // Command scope resets ordinary and URL-scoped repository helpers.
            ("GIT_CONFIG_COUNT".into(), "3".into()),
            ("GIT_CONFIG_KEY_0".into(), "credential.helper".into()),
            ("GIT_CONFIG_VALUE_0".into(), String::new()),
            ("GIT_CONFIG_KEY_1".into(), "credential.helper".into()),
            (
                "GIT_CONFIG_VALUE_1".into(),
                format!("!{quoted} git-credential"),
            ),
            ("GIT_CONFIG_KEY_2".into(), "credential.useHttpPath".into()),
            ("GIT_CONFIG_VALUE_2".into(), "true".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            // Git tries askpass before checking GIT_TERMINAL_PROMPT. Override
            // both inherited programs and core.askPass after a helper refusal.
            ("GIT_ASKPASS".into(), "/bin/false".into()),
            ("SSH_ASKPASS".into(), "/bin/false".into()),
        ]);
        Ok(())
    }
}

async fn resolve(
    manager: &SecretManager,
    storage: &dyn swage_storage::Storage,
    request: &GitCredentialRequest,
) -> Result<GitCredentialResponse, swage_core::GatewayError> {
    let empty = || GitCredentialResponse {
        username: None,
        password: None,
    };
    if request.protocol != "https" || request.host.is_empty() {
        return Ok(empty());
    }
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
            return Ok(empty());
        }
        return Ok(GitCredentialResponse {
            username: Some(username.into()),
            password: Some(secret.value),
        });
    }
    Ok(empty())
}

pub async fn lookup(
    socket: &std::path::Path,
    request: &GitCredentialRequest,
) -> std::io::Result<GitCredentialResponse> {
    tokio::time::timeout(Duration::from_secs(5), async {
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
