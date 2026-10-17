//! Commit signing on the host for sandboxed git: the operator's SSH signing
//! key never enters the sandbox.
//!
//! Git in the sandbox is configured to sign with the `swage` binary as its
//! `gpg.ssh.program`, and to name the operator's public key as a literal
//! (`key::…`), so nothing under `~/.ssh` is read inside. The program speaks
//! `ssh-keygen`'s signing interface (`-Y sign -n <namespace> -f <key> <file>`),
//! forwards the payload over the turn's private IPC socket, and writes the
//! signature the gateway answers with beside the file. The gateway signs on
//! the host with `ssh-keygen` against the key the operator configured — a
//! private key file beside the public one, or the operator's agent — refuses
//! any other key, and records every signing with the key named and the turn
//! attributed. A host that cannot sign answers a refusal, and git fails the
//! commit with the cause: never an unsigned commit under a configuration
//! that signs.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use base64::Engine;
use swage_protocol::hosting::{GitSignRequest, GitSignResponse};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const SOCKET_ENV: &str = "SWAGE_GIT_SIGNER_SOCKET";

/// The operator's commit-signing configuration as the host's git reads it:
/// the key named by `user.signingkey`, its public half, and whether commits
/// and tags sign by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigningConfig {
    /// What `user.signingkey` names: a key file the host signs with, or none
    /// when the configuration carries the public key as a literal and the
    /// operator's agent holds the private half.
    pub key_file: Option<PathBuf>,
    /// The public key, one line, as `ssh-keygen` prints it.
    pub public_key: String,
    pub commit_gpgsign: bool,
    pub tag_gpgsign: bool,
}

impl SigningConfig {
    /// Read the operator's signing configuration from the host's git. `None`
    /// when the operator does not sign, or signs with a format this seam
    /// does not carry yet (openpgp), which is said once in the log.
    pub async fn from_host_git() -> Option<Self> {
        let get = |key: &'static str| async move {
            let output = tokio::process::Command::new("git")
                .args(["config", "--get", key])
                .output()
                .await
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let commit_gpgsign = get("commit.gpgsign")
            .await
            .is_some_and(|value| git_bool(&value));
        let tag_gpgsign = get("tag.gpgsign")
            .await
            .is_some_and(|value| git_bool(&value));
        let format = get("gpg.format").await;
        let signing_key = get("user.signingkey").await?;
        if format.as_deref() != Some("ssh") {
            if commit_gpgsign {
                tracing::warn!(
                    format = format.as_deref().unwrap_or("openpgp"),
                    "the operator signs commits with a format sandboxed git cannot forward yet; sandboxed commits are unsigned"
                );
            }
            return None;
        }
        Self::from_signing_key(&signing_key, commit_gpgsign, tag_gpgsign)
    }

    /// The configuration for one `user.signingkey` value: a `key::` literal,
    /// or a path to the public or private key file.
    pub fn from_signing_key(
        signing_key: &str,
        commit_gpgsign: bool,
        tag_gpgsign: bool,
    ) -> Option<Self> {
        if let Some(literal) = signing_key.strip_prefix("key::") {
            return Some(Self {
                key_file: None,
                public_key: literal.trim().to_owned(),
                commit_gpgsign,
                tag_gpgsign,
            });
        }
        let path = expand_home(signing_key);
        let public_key = match std::fs::read_to_string(&path) {
            Ok(text) if looks_like_public_key(&text) => text.trim().to_owned(),
            _ => match std::fs::read_to_string(with_pub_suffix(&path)) {
                Ok(text) if looks_like_public_key(&text) => text.trim().to_owned(),
                _ => {
                    tracing::warn!(key = %path.display(), "the operator's signing key names no readable public key; sandboxed commits are unsigned");
                    return None;
                }
            },
        };
        Some(Self {
            key_file: Some(path),
            public_key,
            commit_gpgsign,
            tag_gpgsign,
        })
    }

    /// The key as the record names it: the file, or the literal's comment.
    fn key_name(&self) -> String {
        match &self.key_file {
            Some(path) => path.display().to_string(),
            None => format!(
                "key::{}",
                self.public_key
                    .split_whitespace()
                    .nth(2)
                    .unwrap_or("(no comment)")
            ),
        }
    }
}

fn git_bool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

fn looks_like_public_key(text: &str) -> bool {
    let first = text.trim_start();
    first.starts_with("ssh-") || first.starts_with("sk-") || first.starts_with("ecdsa-")
}

fn with_pub_suffix(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

fn expand_home(value: &str) -> PathBuf {
    match value.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(value),
        },
        None => PathBuf::from(value),
    }
}

/// Owns a turn-scoped IPC listener that signs on the host; dropping it
/// stops service and removes the socket.
pub struct SignerServer {
    socket: swage_sandbox::SocketManager,
    task: tokio::task::JoinHandle<()>,
    signing: SigningConfig,
}

impl Drop for SignerServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SignerServer {
    pub fn start(
        storage: Arc<dyn swage_storage::Storage>,
        session_id: Option<uuid::Uuid>,
        turn_id: Option<uuid::Uuid>,
        signing: SigningConfig,
    ) -> Result<Arc<Self>, swage_core::SandboxError> {
        let (socket, listener) = swage_sandbox::SocketManager::create("git-sign")?;
        let served = signing.clone();
        let task = tokio::spawn(async move {
            // One bounded exchange at a time, as the credential helper's: an
            // abandoned caller never leaves a signing in flight unowned.
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "git signer listener stopped");
                        break;
                    }
                };
                let exchange = tokio::time::timeout(Duration::from_secs(15), async {
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    (&mut stream)
                        .take(4 * 1024 * 1024)
                        .read_line(&mut line)
                        .await?;
                    let request: GitSignRequest =
                        serde_json::from_str(&line).map_err(std::io::Error::other)?;
                    let response = sign(&served, &request).await;
                    let outcome = if response.signature.is_some() {
                        "signed"
                    } else {
                        "refused"
                    };
                    // The signing is attributed before the signature leaves;
                    // a record that cannot be written releases nothing.
                    storage
                        .create_audit_event(swage_storage::NewAuditEvent {
                            id: uuid::Uuid::new_v4().to_string(),
                            session_id,
                            entity_kind: "git_signing".into(),
                            entity_id: served.key_name(),
                            event_kind: "git_commit_signed".into(),
                            summary: format!(
                                "Signing with {} for namespace {}: {outcome}",
                                served.key_name(),
                                request.namespace
                            ),
                            metadata: serde_json::json!({
                                "key": served.key_name(),
                                "namespace": request.namespace,
                                "turn_id": turn_id,
                                "outcome": outcome,
                                "refusal": response.refusal,
                            }),
                        })
                        .await
                        .map_err(std::io::Error::other)?;
                    let mut body = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
                    body.push(b'\n');
                    stream.get_mut().write_all(&body).await
                })
                .await;
                match exchange {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "git signing exchange refused"),
                    Err(_) => tracing::warn!("git signing exchange timed out"),
                }
            }
        });
        Ok(Arc::new(Self {
            socket,
            task,
            signing,
        }))
    }

    /// The signing configuration the sandboxed git runs under: this binary
    /// as the SSH signing program, the operator's public key as a literal
    /// (nothing under `~/.ssh` is read inside), and the operator's defaults
    /// for signing commits and tags. Appended to the git configuration the
    /// credential helper owns, so the two compose.
    pub fn configure(
        &self,
        config: &mut swage_sandbox::SandboxConfig,
    ) -> Result<(), swage_core::SandboxError> {
        config.read_only_paths.push(self.socket.path().to_owned());
        config
            .environment
            .insert(SOCKET_ENV.into(), self.socket.path().display().to_string());
        let binary = swage_sandbox::resolve_agent_binary()?;
        crate::git_credential::append_git_config(
            config,
            &[
                ("gpg.format", "ssh".to_owned()),
                ("gpg.ssh.program", binary.display().to_string()),
                (
                    "user.signingkey",
                    format!("key::{}", self.signing.public_key),
                ),
                ("commit.gpgsign", self.signing.commit_gpgsign.to_string()),
                ("tag.gpgsign", self.signing.tag_gpgsign.to_string()),
            ],
        );
        Ok(())
    }
}

/// Sign on the host, or say why not: the key must be the operator's
/// configured one, and `ssh-keygen` must be able to sign with it — the
/// private file beside the public one, or the operator's agent.
async fn sign(signing: &SigningConfig, request: &GitSignRequest) -> GitSignResponse {
    let refuse = |refusal: String| GitSignResponse {
        signature: None,
        refusal: Some(refusal),
    };
    if request.public_key.trim() != signing.public_key.trim() {
        return refuse("the requested key is not the operator's configured signing key".into());
    }
    if request.namespace.is_empty() || request.namespace.contains(['\n', '\0']) {
        return refuse("the signing namespace is not a name".into());
    }
    let payload = match base64::engine::general_purpose::STANDARD.decode(&request.payload) {
        Ok(payload) => payload,
        Err(error) => return refuse(format!("the payload could not be read: {error}")),
    };
    let scratch = std::env::temp_dir().join(format!("swage-sign-{}", uuid::Uuid::new_v4()));
    if let Err(error) = tokio::fs::create_dir(&scratch).await {
        return refuse(format!("the host could not stage the payload: {error}"));
    }
    let outcome = sign_in(&scratch, signing, &request.namespace, &payload).await;
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    match outcome {
        Ok(signature) => GitSignResponse {
            signature: Some(signature),
            refusal: None,
        },
        Err(refusal) => refuse(refusal),
    }
}

async fn sign_in(
    scratch: &Path,
    signing: &SigningConfig,
    namespace: &str,
    payload: &[u8],
) -> Result<String, String> {
    let payload_path = scratch.join("payload");
    tokio::fs::write(&payload_path, payload)
        .await
        .map_err(|error| format!("the host could not stage the payload: {error}"))?;
    let key_path = match &signing.key_file {
        Some(path) => path.clone(),
        None => {
            // A literal key: the public half on disk for ssh-keygen, which
            // then looks the private half up in the operator's agent.
            let path = scratch.join("key.pub");
            tokio::fs::write(&path, format!("{}\n", signing.public_key))
                .await
                .map_err(|error| format!("the host could not stage the key: {error}"))?;
            path
        }
    };
    let output = tokio::process::Command::new("ssh-keygen")
        .args(["-Y", "sign", "-n", namespace, "-f"])
        .arg(&key_path)
        .arg(&payload_path)
        .output()
        .await
        .map_err(|error| format!("ssh-keygen could not run on the host: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "the host could not sign with {}: {}",
            signing.key_name(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    tokio::fs::read_to_string(with_sig_suffix(&payload_path))
        .await
        .map_err(|error| format!("the host wrote no signature: {error}"))
}

fn with_sig_suffix(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

/// Ask the turn's signer over its socket.
pub async fn request_signature(
    socket: &Path,
    request: &GitSignRequest,
) -> std::io::Result<GitSignResponse> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut stream = UnixStream::connect(socket).await?;
        let mut body = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        body.push(b'\n');
        stream.write_all(&body).await?;
        let mut line = String::new();
        BufReader::new(stream)
            .take(256 * 1024)
            .read_line(&mut line)
            .await?;
        serde_json::from_str(&line).map_err(std::io::Error::other)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// The sandbox side: this binary invoked by git as its SSH signing program,
/// with `ssh-keygen`'s arguments. `-Y sign` goes to the host over the
/// socket; any other operation (`verify`, `find-principals`, …) is
/// `ssh-keygen`'s own and runs it unchanged, so verification inside the
/// sandbox behaves as git expects. Returns the exit code.
pub async fn run_signer(args: &[std::ffi::OsString]) -> std::io::Result<i32> {
    let text: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let operation = text
        .iter()
        .position(|arg| arg == "-Y")
        .and_then(|at| text.get(at + 1))
        .cloned()
        .unwrap_or_default();
    if operation != "sign" {
        let status = tokio::process::Command::new("ssh-keygen")
            .args(args)
            .status()
            .await?;
        return Ok(status.code().unwrap_or(1));
    }
    let mut namespace = None;
    let mut key_file = None;
    let mut file = None;
    let mut at = 0;
    while at < text.len() {
        match text[at].as_str() {
            "-Y" | "-O" => at += 2,
            "-n" => {
                namespace = text.get(at + 1).cloned();
                at += 2;
            }
            "-f" => {
                key_file = text.get(at + 1).cloned();
                at += 2;
            }
            operand if !operand.starts_with('-') => {
                file = Some(operand.to_owned());
                at += 1;
            }
            _ => at += 1,
        }
    }
    let (Some(namespace), Some(key_file), Some(file)) = (namespace, key_file, file) else {
        eprintln!("swage: signing needs -n <namespace>, -f <key> and a file");
        return Ok(2);
    };
    let public_key = std::fs::read_to_string(&key_file)?;
    let payload = std::fs::read(&file)?;
    let socket = std::env::var_os(SOCKET_ENV)
        .ok_or_else(|| std::io::Error::other("git signing socket is unavailable"))?;
    let response = request_signature(
        Path::new(&socket),
        &GitSignRequest {
            namespace,
            public_key: public_key.trim().to_owned(),
            payload: base64::engine::general_purpose::STANDARD.encode(&payload),
        },
    )
    .await?;
    match response.signature {
        Some(signature) => {
            std::fs::write(with_sig_suffix(Path::new(&file)), signature)?;
            Ok(0)
        }
        None => {
            eprintln!(
                "swage: commit not signed: {}",
                response
                    .refusal
                    .unwrap_or_else(|| "the host refused to sign".into())
            );
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signing_key_literal_needs_no_file_and_a_path_reads_its_public_half() {
        let literal =
            SigningConfig::from_signing_key("key::ssh-ed25519 AAAAC3 fixture", true, false)
                .expect("a literal key configures");
        assert_eq!(literal.key_file, None);
        assert_eq!(literal.public_key, "ssh-ed25519 AAAAC3 fixture");
        assert!(literal.commit_gpgsign && !literal.tag_gpgsign);
        assert_eq!(literal.key_name(), "key::fixture");

        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("key");
        std::fs::write(&private, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        std::fs::write(dir.path().join("key.pub"), "ssh-ed25519 AAAAC3 fixture\n").unwrap();
        let by_private = SigningConfig::from_signing_key(private.to_str().unwrap(), false, true)
            .expect("a private key path finds its public half");
        assert_eq!(by_private.public_key, "ssh-ed25519 AAAAC3 fixture");
        assert_eq!(by_private.key_file.as_deref(), Some(private.as_path()));
        let by_public = SigningConfig::from_signing_key(
            dir.path().join("key.pub").to_str().unwrap(),
            false,
            false,
        )
        .expect("a public key path reads itself");
        assert_eq!(by_public.public_key, "ssh-ed25519 AAAAC3 fixture");
        assert!(
            SigningConfig::from_signing_key(
                dir.path().join("absent").to_str().unwrap(),
                true,
                true
            )
            .is_none(),
            "a key with no readable public half configures nothing"
        );
    }
}
