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

/// The operator's commit-signing configuration as the host's git reads it
/// for the turn's workspace: the format, the key `user.signingkey` names
/// with its public half — or why the host cannot sign with it — and
/// whether commits and tags sign by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigningConfig {
    /// `gpg.format`: `ssh`, which the host signs with, or another format
    /// (`openpgp`, `x509`) it cannot forward yet and refuses with the cause.
    pub format: String,
    /// The key the host signs with, or the reason it cannot: a signing
    /// configuration whose key the host cannot use still reaches the
    /// sandbox, so git fails the commit with that reason rather than
    /// committing unsigned.
    pub material: Result<KeyMaterial, String>,
    pub commit_gpgsign: bool,
    pub tag_gpgsign: bool,
}

/// The operator's SSH signing key as the host uses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyMaterial {
    /// What `user.signingkey` names: a key file the host signs with, or none
    /// when the configuration carries the public key as a literal and the
    /// operator's agent holds the private half.
    pub key_file: Option<PathBuf>,
    /// The public key, one line, as `ssh-keygen` prints it.
    pub public_key: String,
}

/// The `user.signingkey` literal the sandboxed git is given when the host
/// has no usable key: the host refuses the request it produces with the
/// cause, so the commit fails with it.
const UNAVAILABLE_KEY: &str = "unavailable";

impl SigningConfig {
    /// Read the operator's signing configuration from the host's git as
    /// the turn's workspace sees it — the operator's global configuration,
    /// the repository's own, and any conditional include the workspace
    /// selects. `None` when the operator does not sign: no signing key, no
    /// key command, and neither commits nor tags signing by default. A
    /// configuration that signs but that the host cannot forward — another
    /// format, a key with no readable public half, a key command — is
    /// carried with its reason, said in the log at the turn's start.
    pub async fn from_host_git(workspace: &Path) -> Option<Self> {
        let commit_gpgsign = host_git_bool(workspace, "commit.gpgsign").await;
        let tag_gpgsign = host_git_bool(workspace, "tag.gpgsign").await;
        let signing_key = host_git_config(workspace, &["--get", "user.signingkey"]).await;
        let key_command = host_git_config(workspace, &["--get", "gpg.ssh.defaultKeyCommand"]).await;
        if signing_key.is_none() && key_command.is_none() && !commit_gpgsign && !tag_gpgsign {
            return None;
        }
        let format = host_git_config(workspace, &["--get", "gpg.format"])
            .await
            .unwrap_or_else(|| "openpgp".to_owned());
        let material = if format != "ssh" {
            Err(format!(
                "the operator signs with {format}, which sandboxed git cannot forward yet"
            ))
        } else if let Some(signing_key) = signing_key {
            key_material(&signing_key)
        } else {
            Err("the operator's signing key comes from gpg.ssh.defaultKeyCommand, which sandboxed git cannot forward yet".to_owned())
        };
        if let Err(reason) = &material {
            tracing::warn!(
                %reason,
                "sandboxed commits that sign will fail with this cause"
            );
        }
        Some(Self {
            format,
            material,
            commit_gpgsign,
            tag_gpgsign,
        })
    }

    /// The SSH configuration for one `user.signingkey` value: a `key::`
    /// literal, or a path to the public or private key file.
    pub fn from_signing_key(signing_key: &str, commit_gpgsign: bool, tag_gpgsign: bool) -> Self {
        Self {
            format: "ssh".to_owned(),
            material: key_material(signing_key),
            commit_gpgsign,
            tag_gpgsign,
        }
    }

    /// The key as the record names it: its fingerprint, or why there is
    /// none.
    fn key_identity(&self) -> String {
        match &self.material {
            Ok(material) => material.fingerprint(),
            Err(_) => "(no usable key)".to_owned(),
        }
    }
}

/// The material a `user.signingkey` value names, or why it cannot be used.
fn key_material(signing_key: &str) -> Result<KeyMaterial, String> {
    if let Some(literal) = signing_key.strip_prefix("key::") {
        return Ok(KeyMaterial {
            key_file: None,
            public_key: literal.trim().to_owned(),
        });
    }
    let path = expand_home(signing_key);
    let public_key = match std::fs::read_to_string(&path) {
        Ok(text) if looks_like_public_key(&text) => text.trim().to_owned(),
        _ => match std::fs::read_to_string(with_pub_suffix(&path)) {
            Ok(text) if looks_like_public_key(&text) => text.trim().to_owned(),
            _ => {
                return Err(format!(
                    "the operator's signing key {} names no readable public key",
                    path.display()
                ));
            }
        },
    };
    Ok(KeyMaterial {
        key_file: Some(path),
        public_key,
    })
}

impl KeyMaterial {
    /// The key's SSH fingerprint — `SHA256:` and the unpadded base64 of the
    /// key blob's digest, as `ssh-keygen -lf` prints it — the identity the
    /// record names, the same for a file and for a literal of one key.
    pub fn fingerprint(&self) -> String {
        use sha2::Digest;
        let blob = self
            .public_key
            .split_whitespace()
            .nth(1)
            .and_then(|encoded| {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .ok()
            });
        match blob {
            Some(blob) => format!(
                "SHA256:{}",
                base64::engine::general_purpose::STANDARD_NO_PAD
                    .encode(sha2::Sha256::digest(&blob))
            ),
            None => "(unreadable key)".to_owned(),
        }
    }

    /// The key as the operator would recognise it: the file, or the
    /// literal's comment.
    fn name(&self) -> String {
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

/// One value of the host's git configuration as the workspace sees it, or
/// `None` where git has none or the read fails.
pub(crate) async fn host_git_config(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("config")
        .args(args)
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// A boolean of the host's git configuration, read as git reads it: a key
/// set with no value is true.
async fn host_git_bool(workspace: &Path, key: &str) -> bool {
    host_git_config(workspace, &["--type=bool", "--get", key])
        .await
        .is_some_and(|value| value == "true")
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
            // One bounded exchange at a time, as the credential helper's.
            // Each stage is bounded on its own, so a signing that waits —
            // a key on a hardware token, a passphrase prompt — ends in a
            // refusal git reports, not an exchange abandoned mid-signing;
            // what the signing stages is cleaned up however it ends.
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "git signer listener stopped");
                        break;
                    }
                };
                let exchange = async {
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    tokio::time::timeout(
                        EXCHANGE_TIMEOUT,
                        (&mut stream).take(4 * 1024 * 1024).read_line(&mut line),
                    )
                    .await
                    .map_err(|_| std::io::Error::other("the request did not arrive in time"))??;
                    let request: GitSignRequest =
                        serde_json::from_str(&line).map_err(std::io::Error::other)?;
                    let response = match tokio::time::timeout(
                        SIGNING_TIMEOUT,
                        sign(&served, &request),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(_) => GitSignResponse {
                            signature: None,
                            refusal: Some(format!(
                                "the host did not sign within {} seconds; a key on a hardware token or behind a passphrase prompt waits longer than a sandboxed commit can",
                                SIGNING_TIMEOUT.as_secs()
                            )),
                        },
                    };
                    let outcome = if response.signature.is_some() {
                        "signed"
                    } else {
                        "refused"
                    };
                    // The signing is attributed before the signature leaves;
                    // a record that cannot be written releases nothing.
                    let (key_name, key_file, public_key) = match &served.material {
                        Ok(material) => (
                            Some(material.name()),
                            material
                                .key_file
                                .as_ref()
                                .map(|path| path.display().to_string()),
                            Some(material.public_key.clone()),
                        ),
                        Err(_) => (None, None, None),
                    };
                    storage
                        .create_audit_event(swage_storage::NewAuditEvent {
                            id: uuid::Uuid::new_v4().to_string(),
                            session_id,
                            entity_kind: "git_signing".into(),
                            entity_id: served.key_identity(),
                            event_kind: "git_signing".into(),
                            summary: format!(
                                "Signing with {} for namespace {}: {outcome}",
                                key_name
                                    .clone()
                                    .unwrap_or_else(|| "no usable key".to_owned()),
                                request.namespace
                            ),
                            metadata: serde_json::json!({
                                "key": key_name,
                                "key_file": key_file,
                                "public_key": public_key,
                                "fingerprint": served.key_identity(),
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
                    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.get_mut().write_all(&body))
                        .await
                        .map_err(|_| std::io::Error::other("the reply was not taken in time"))?
                };
                if let Err(error) = exchange.await {
                    tracing::warn!(%error, "git signing exchange refused");
                }
            }
        });
        Ok(Arc::new(Self {
            socket,
            task,
            signing,
        }))
    }

    /// The signing configuration the sandboxed git runs under: the
    /// operator's format and defaults for signing commits and tags, and
    /// this binary as the signing program — for SSH with the operator's
    /// public key as a literal (nothing under `~/.ssh` is read inside), or
    /// a placeholder the host refuses with the cause when it has no usable
    /// key; for any other format, as a `gpg.program` that refuses, so a
    /// commit that signs fails with the cause rather than going unsigned.
    /// Appended to the git configuration the credential helper owns, so the
    /// two compose in either order.
    pub fn configure(
        &self,
        config: &mut swage_sandbox::SandboxConfig,
    ) -> Result<(), swage_core::SandboxError> {
        config.read_only_paths.push(self.socket.path().to_owned());
        config
            .environment
            .insert(SOCKET_ENV.into(), self.socket.path().display().to_string());
        let binary = swage_sandbox::resolve_agent_binary()?.display().to_string();
        let mut pairs = vec![
            ("gpg.format", self.signing.format.clone()),
            ("commit.gpgsign", self.signing.commit_gpgsign.to_string()),
            ("tag.gpgsign", self.signing.tag_gpgsign.to_string()),
        ];
        if self.signing.format == "ssh" {
            let literal = match &self.signing.material {
                Ok(material) => material.public_key.clone(),
                Err(_) => UNAVAILABLE_KEY.to_owned(),
            };
            pairs.push(("gpg.ssh.program", binary));
            pairs.push(("user.signingkey", format!("key::{literal}")));
        } else {
            pairs.push(("gpg.program", binary));
        }
        crate::git_credential::append_git_config(config, &pairs);
        Ok(())
    }
}

/// How long a request may take to arrive and a reply to be taken.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the host signs for before refusing: long enough for an agent
/// to answer, short enough that a commit waiting on a hardware token or a
/// passphrase prompt fails with that cause instead of hanging the turn.
const SIGNING_TIMEOUT: Duration = Duration::from_secs(15);

/// Sign on the host, or say why not: the host must have a usable key, the
/// key must be the operator's configured one, and `ssh-keygen` must be able
/// to sign with it — the private file beside the public one, or the
/// operator's agent.
async fn sign(signing: &SigningConfig, request: &GitSignRequest) -> GitSignResponse {
    let refuse = |refusal: String| GitSignResponse {
        signature: None,
        refusal: Some(refusal),
    };
    let material = match &signing.material {
        Ok(material) => material,
        Err(reason) => return refuse(reason.clone()),
    };
    if request.public_key.trim() != material.public_key.trim() {
        return refuse("the requested key is not the operator's configured signing key".into());
    }
    if request.namespace.is_empty() || request.namespace.contains(['\n', '\0']) {
        return refuse("the signing namespace is not a name".into());
    }
    let payload = match base64::engine::general_purpose::STANDARD.decode(&request.payload) {
        Ok(payload) => payload,
        Err(error) => return refuse(format!("the payload could not be read: {error}")),
    };
    let scratch = match Scratch::create() {
        Ok(scratch) => scratch,
        Err(error) => return refuse(format!("the host could not stage the payload: {error}")),
    };
    match sign_in(&scratch.0, material, &request.namespace, &payload).await {
        Ok(signature) => GitSignResponse {
            signature: Some(signature),
            refusal: None,
        },
        Err(refusal) => refuse(refusal),
    }
}

/// A private directory the payload and signature are staged in, removed
/// when the signing ends — completed, refused, or dropped by a timeout.
struct Scratch(PathBuf);

impl Scratch {
    fn create() -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(format!("swage-sign-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn sign_in(
    scratch: &Path,
    material: &KeyMaterial,
    namespace: &str,
    payload: &[u8],
) -> Result<String, String> {
    let payload_path = scratch.join("payload");
    tokio::fs::write(&payload_path, payload)
        .await
        .map_err(|error| format!("the host could not stage the payload: {error}"))?;
    let key_path = match &material.key_file {
        Some(path) => path.clone(),
        None => {
            // A literal key: the public half on disk for ssh-keygen, which
            // then looks the private half up in the operator's agent.
            let path = scratch.join("key.pub");
            tokio::fs::write(&path, format!("{}\n", material.public_key))
                .await
                .map_err(|error| format!("the host could not stage the key: {error}"))?;
            path
        }
    };
    // The signing is the host's for exactly as long as the exchange is:
    // a signing abandoned by its timeout takes ssh-keygen with it.
    let output = tokio::process::Command::new("ssh-keygen")
        .args(["-Y", "sign", "-n", namespace, "-f"])
        .arg(&key_path)
        .arg(&payload_path)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("ssh-keygen could not run on the host: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "the host could not sign with {}: {}",
            material.name(),
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

/// Whether a command line is git calling its signing program: `ssh-keygen`'s
/// `-Y`, or `gpg`'s status and detached-signature arguments.
pub fn is_signing_invocation(args: &[std::ffi::OsString]) -> bool {
    args.get(1).is_some_and(|arg| arg == "-Y")
        || args.iter().any(|arg| {
            let arg = arg.to_string_lossy();
            arg == "-bsau" || arg.starts_with("--status-fd")
        })
}

/// The sandbox side: this binary invoked by git as its signing program.
/// With `ssh-keygen`'s arguments, `-Y sign` goes to the host over the
/// socket, and any other operation (`verify`, `find-principals`, …) is
/// `ssh-keygen`'s own and runs it unchanged, so verification inside the
/// sandbox behaves as git expects. With `gpg`'s arguments — the operator
/// signs with a format the host cannot forward — it refuses, so the commit
/// fails with the cause. Returns the exit code.
pub async fn run_signer(args: &[std::ffi::OsString]) -> std::io::Result<i32> {
    let text: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let Some(at) = text.iter().position(|arg| arg == "-Y") else {
        eprintln!(
            "swage: commit not signed: the operator signs with OpenPGP, which sandboxed git cannot forward yet"
        );
        return Ok(1);
    };
    let operation = text.get(at + 1).cloned().unwrap_or_default();
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
            SigningConfig::from_signing_key("key::ssh-ed25519 AAAAC3 fixture", true, false);
        let material = literal.material.clone().expect("a literal key configures");
        assert_eq!(material.key_file, None);
        assert_eq!(material.public_key, "ssh-ed25519 AAAAC3 fixture");
        assert!(literal.commit_gpgsign && !literal.tag_gpgsign);
        assert_eq!(material.name(), "key::fixture");

        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("key");
        std::fs::write(&private, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        std::fs::write(dir.path().join("key.pub"), "ssh-ed25519 AAAAC3 fixture\n").unwrap();
        let by_private = SigningConfig::from_signing_key(private.to_str().unwrap(), false, true)
            .material
            .expect("a private key path finds its public half");
        assert_eq!(by_private.public_key, "ssh-ed25519 AAAAC3 fixture");
        assert_eq!(by_private.key_file.as_deref(), Some(private.as_path()));
        let by_public = SigningConfig::from_signing_key(
            dir.path().join("key.pub").to_str().unwrap(),
            false,
            false,
        )
        .material
        .expect("a public key path reads itself");
        assert_eq!(by_public.public_key, "ssh-ed25519 AAAAC3 fixture");
    }

    /// A signing configuration whose key the host cannot use still reaches
    /// the sandbox: the signer is configured with a placeholder key and
    /// the operator's defaults, and refuses with the cause, so git fails
    /// the commit with it instead of committing unsigned.
    #[tokio::test]
    async fn an_unusable_key_is_carried_as_a_refusal_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent");
        let signing = SigningConfig::from_signing_key(absent.to_str().unwrap(), true, true);
        let reason = signing
            .material
            .clone()
            .expect_err("a key with no readable public half is a refusal");
        assert!(reason.contains("names no readable public key"), "{reason}");
        assert_eq!(signing.key_identity(), "(no usable key)");

        let refused = sign(
            &signing,
            &GitSignRequest {
                namespace: "git".into(),
                public_key: UNAVAILABLE_KEY.into(),
                payload: base64::engine::general_purpose::STANDARD.encode(b"tree"),
            },
        )
        .await;
        assert_eq!(refused.signature, None);
        assert_eq!(refused.refusal.as_deref(), Some(reason.as_str()));
    }

    /// The host reads the operator's configuration as the workspace sees it,
    /// and a boolean set with no value is true, as git reads it.
    #[tokio::test]
    async fn host_signing_configuration_is_read_for_the_workspace_with_gits_booleans() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        let public = dir.path().join("operator.pub");
        std::fs::write(&public, "ssh-ed25519 AAAAC3 operator\n").unwrap();
        git(&["config", "gpg.format", "ssh"]);
        git(&["config", "user.signingkey", public.to_str().unwrap()]);
        // A bare `gpgsign` under `[commit]` is true to git.
        let config = repo.join(".git").join("config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[commit]\n\tgpgsign\n");
        std::fs::write(&config, text).unwrap();

        let signing = SigningConfig::from_host_git(&repo)
            .await
            .expect("the workspace's configuration signs");
        assert_eq!(signing.format, "ssh");
        assert!(signing.commit_gpgsign, "a bare boolean reads as true");
        assert!(!signing.tag_gpgsign);
        let material = signing.material.expect("the public key is read");
        assert_eq!(material.public_key, "ssh-ed25519 AAAAC3 operator");

        let unconfigured = dir.path().join("plain");
        std::fs::create_dir(&unconfigured).unwrap();
        let plain = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&unconfigured)
            .output()
            .unwrap();
        assert!(plain.status.success());
        // Another workspace with no signing configuration of its own reads
        // the operator's global configuration alone, whatever that is.
        let _ = SigningConfig::from_host_git(&unconfigured).await;
    }

    /// A key's identity in the record is its fingerprint, the same for the
    /// key file and for a literal of the same key, and what ssh-keygen
    /// prints for it.
    #[test]
    fn a_keys_recorded_identity_is_its_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let generated = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "fixture", "-f"])
            .arg(dir.path().join("key"))
            .output()
            .expect("ssh-keygen runs");
        assert!(generated.status.success());
        let public = std::fs::read_to_string(dir.path().join("key.pub")).unwrap();
        let printed = std::process::Command::new("ssh-keygen")
            .args(["-lf"])
            .arg(dir.path().join("key.pub"))
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&printed.stdout);
        let expected = printed
            .split_whitespace()
            .nth(1)
            .expect("ssh-keygen prints the fingerprint second");

        let by_file = key_material(dir.path().join("key").to_str().unwrap()).unwrap();
        let by_literal = key_material(&format!("key::{}", public.trim())).unwrap();
        assert_eq!(by_file.fingerprint(), expected);
        assert_eq!(by_literal.fingerprint(), expected);
        assert_ne!(
            by_file.name(),
            by_literal.name(),
            "the names differ; the identity does not"
        );
    }
}
