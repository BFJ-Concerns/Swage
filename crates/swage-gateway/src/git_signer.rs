//! Commit signing on the host for sandboxed git: the operator's signing key
//! never enters the sandbox.
//!
//! Git in the sandbox is configured to sign with the `swage` binary as its
//! signing program, which forwards each payload over the turn's private IPC
//! socket and hands git the signature the gateway answers with. For SSH the
//! program speaks `ssh-keygen`'s signing interface (`-Y sign -n <namespace>
//! -f <key> <file>`) and git names the operator's public key as a literal
//! (`key::…`), so nothing under `~/.ssh` is read inside; the gateway signs
//! with `ssh-keygen` against the key the operator configured — a private
//! key file beside the public one, or the operator's agent. For OpenPGP the
//! program speaks `gpg`'s (`--status-fd=2 -bsau <key>`, the payload on
//! standard input) and the gateway signs with the operator's own `gpg`
//! program and key, through their gpg-agent. Any other key is refused, and
//! every signing is recorded with the key named and the turn attributed. A
//! host that cannot sign answers a refusal, and git fails the commit with
//! the cause: never an unsigned commit under a configuration that signs.

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
    /// `gpg.format`: `ssh` or `openpgp`, which the host signs with, or
    /// another format (`x509`) it cannot forward yet and refuses with the
    /// cause.
    pub format: String,
    /// The key the host signs with, or the reason it cannot: a signing
    /// configuration whose key the host cannot use still reaches the
    /// sandbox, so git fails the commit with that reason rather than
    /// committing unsigned.
    pub material: Result<SigningKey, String>,
    pub commit_gpgsign: bool,
    pub tag_gpgsign: bool,
}

/// The key the host signs with, in the operator's format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SigningKey {
    Ssh(KeyMaterial),
    Openpgp(OpenPgpKey),
}

/// The operator's OpenPGP signing key as the host uses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPgpKey {
    /// What git names to `gpg -u`: `user.signingkey`, or the committer's
    /// identity where none is set, as git itself falls back to.
    pub key: String,
    /// The operator's signing program: `gpg.openpgp.program`, `gpg.program`,
    /// or `gpg`.
    pub program: String,
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
        let material = match format.as_str() {
            "ssh" => match signing_key {
                Some(signing_key) => key_material(&signing_key).map(SigningKey::Ssh),
                None => Err("the operator's signing key comes from gpg.ssh.defaultKeyCommand, which sandboxed git cannot forward yet".to_owned()),
            },
            "openpgp" => openpgp_key(workspace, signing_key)
                .await
                .map(SigningKey::Openpgp),
            other => Err(format!(
                "the operator signs with {other}, which sandboxed git cannot forward yet"
            )),
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

    /// Refuse to sign with an OpenPGP program that lies where the turn's
    /// sandboxed commands can write: the host runs it unsandboxed, so one
    /// the sandbox could replace — a `gpg` planted in a writable directory on
    /// `PATH`, say — would run its code outside the sandbox. A commit that
    /// signs then fails with the cause.
    pub fn refusing_programs_under(mut self, writable: &[PathBuf]) -> Self {
        if let Ok(SigningKey::Openpgp(key)) = &self.material {
            let program = Path::new(&key.program);
            let program = program
                .canonicalize()
                .unwrap_or_else(|_| program.to_path_buf());
            if writable.iter().any(|path| {
                program.starts_with(path.canonicalize().unwrap_or_else(|_| path.clone()))
            }) {
                self.material = Err(format!(
                    "the OpenPGP program {} lies where sandboxed commands can write, so the host will not run it",
                    key.program
                ));
            }
        }
        self
    }

    /// The SSH configuration for one `user.signingkey` value: a `key::`
    /// literal, or a path to the public or private key file.
    pub fn from_signing_key(signing_key: &str, commit_gpgsign: bool, tag_gpgsign: bool) -> Self {
        Self {
            format: "ssh".to_owned(),
            material: key_material(signing_key).map(SigningKey::Ssh),
            commit_gpgsign,
            tag_gpgsign,
        }
    }

    /// The key as the record names it: its fingerprint, or why there is
    /// none.
    fn key_identity(&self) -> String {
        match &self.material {
            Ok(SigningKey::Ssh(material)) => material.fingerprint(),
            Ok(SigningKey::Openpgp(key)) => format!("openpgp:{}", key.key),
            Err(_) => "(no usable key)".to_owned(),
        }
    }
}

/// The operator's OpenPGP key and signing program as the host's git reads
/// them for the workspace. With no `user.signingkey`, git signs as the
/// committer, so the key is the host's committer identity.
async fn openpgp_key(workspace: &Path, signing_key: Option<String>) -> Result<OpenPgpKey, String> {
    let key = match signing_key {
        Some(key) => key,
        None => host_committer_identity(workspace).await.ok_or_else(|| {
            "the operator signs with OpenPGP but neither user.signingkey nor a committer identity is configured".to_owned()
        })?,
    };
    // The host runs this program unsandboxed, so it comes only from the
    // operator's own global or system configuration: the workspace's
    // repository configuration is writable from the sandbox.
    let mut configured = None;
    'scopes: for scope in ["--global", "--system"] {
        for name in ["gpg.openpgp.program", "gpg.program"] {
            if let Some(program) = host_git_config(workspace, &[scope, "--get", name]).await {
                configured = Some(program);
                break 'scopes;
            }
        }
    }
    let named = configured.unwrap_or_else(|| "gpg".to_owned());
    let program = resolve_program(&named).ok_or_else(|| {
        format!("the operator's OpenPGP program {named} is not an executable on the host's PATH")
    })?;
    Ok(OpenPgpKey {
        key,
        program: program.display().to_string(),
    })
}

/// A program as the host would run it: an absolute path as given, a bare
/// name looked up on `PATH`. A relative path names nothing fixed.
fn resolve_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    if program.contains('/') {
        return None;
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

/// `Name <email>`, as git names the committer to `gpg -u`.
async fn host_committer_identity(workspace: &Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["var", "GIT_COMMITTER_IDENT"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ident = String::from_utf8_lossy(&output.stdout);
    // The identity is followed by a timestamp and a zone.
    let ident = ident.trim().rsplitn(3, ' ').nth(2)?.to_owned();
    (!ident.is_empty()).then_some(ident)
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

/// One value of the host's git configuration as the workspace sees it.
async fn host_git_config(workspace: &Path, args: &[&str]) -> Option<String> {
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
                            status: None,
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
                        Ok(SigningKey::Ssh(material)) => (
                            Some(material.name()),
                            material
                                .key_file
                                .as_ref()
                                .map(|path| path.display().to_string()),
                            Some(material.public_key.clone()),
                        ),
                        Ok(SigningKey::Openpgp(key)) => (Some(key.key.clone()), None, None),
                        Err(_) => (None, None, None),
                    };
                    let namespace = match &request {
                        GitSignRequest::Ssh { namespace, .. } => Some(namespace.clone()),
                        GitSignRequest::Openpgp { .. } => None,
                    };
                    storage
                        .create_audit_event(swage_storage::NewAuditEvent {
                            id: uuid::Uuid::new_v4().to_string(),
                            session_id,
                            entity_kind: "git_signing".into(),
                            entity_id: served.key_identity(),
                            event_kind: "git_signing".into(),
                            summary: format!(
                                "Signing with {}{}: {outcome}",
                                key_name
                                    .clone()
                                    .unwrap_or_else(|| "no usable key".to_owned()),
                                namespace
                                    .as_deref()
                                    .map(|namespace| format!(" for namespace {namespace}"))
                                    .unwrap_or_default()
                            ),
                            metadata: serde_json::json!({
                                "key": key_name,
                                "key_file": key_file,
                                "public_key": public_key,
                                "fingerprint": served.key_identity(),
                                "format": served.format,
                                "namespace": namespace,
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
    /// key; for OpenPGP with the operator's key named, and for a format the
    /// host cannot forward as a program the host refuses, so a commit that
    /// signs fails with the cause rather than going unsigned.
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
        match (self.signing.format.as_str(), &self.signing.material) {
            ("ssh", material) => {
                let literal = match material {
                    Ok(SigningKey::Ssh(material)) => material.public_key.clone(),
                    _ => UNAVAILABLE_KEY.to_owned(),
                };
                pairs.push(("gpg.ssh.program", binary));
                pairs.push(("user.signingkey", format!("key::{literal}")));
            }
            (_, material) => {
                // Every other format's program is this one: OpenPGP signs
                // on the host, and a format the host cannot forward is
                // refused there with its cause.
                for program in ["gpg.program", "gpg.openpgp.program", "gpg.x509.program"] {
                    pairs.push((program, binary.clone()));
                }
                if let Ok(SigningKey::Openpgp(key)) = material {
                    pairs.push(("user.signingkey", key.key.clone()));
                }
            }
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
/// request must name the operator's configured key in their format, and the
/// operator's signer must sign with it — `ssh-keygen` with the private file
/// beside the public one or the operator's agent, or their `gpg`.
async fn sign(signing: &SigningConfig, request: &GitSignRequest) -> GitSignResponse {
    let refuse = |refusal: String| GitSignResponse {
        signature: None,
        status: None,
        refusal: Some(refusal),
    };
    let key = match &signing.material {
        Ok(key) => key,
        Err(reason) => return refuse(reason.clone()),
    };
    let decode = |payload: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|error| format!("the payload could not be read: {error}"))
    };
    match (request, key) {
        (
            GitSignRequest::Ssh {
                namespace,
                public_key,
                payload,
            },
            SigningKey::Ssh(material),
        ) => {
            if public_key.trim() != material.public_key.trim() {
                return refuse(
                    "the requested key is not the operator's configured signing key".into(),
                );
            }
            if namespace.is_empty() || namespace.contains(['\n', '\0']) {
                return refuse("the signing namespace is not a name".into());
            }
            let payload = match decode(payload) {
                Ok(payload) => payload,
                Err(refusal) => return refuse(refusal),
            };
            let scratch = match Scratch::create() {
                Ok(scratch) => scratch,
                Err(error) => {
                    return refuse(format!("the host could not stage the payload: {error}"));
                }
            };
            match sign_in(&scratch.0, material, namespace, &payload).await {
                Ok(signature) => GitSignResponse {
                    signature: Some(signature),
                    status: None,
                    refusal: None,
                },
                Err(refusal) => refuse(refusal),
            }
        }
        (
            GitSignRequest::Openpgp {
                key: requested,
                payload,
            },
            SigningKey::Openpgp(key),
        ) => {
            if requested.trim() != key.key.trim() {
                return refuse(
                    "the requested key is not the operator's configured signing key".into(),
                );
            }
            let payload = match decode(payload) {
                Ok(payload) => payload,
                Err(refusal) => return refuse(refusal),
            };
            match sign_openpgp(key, &payload).await {
                Ok((signature, status)) => GitSignResponse {
                    signature: Some(signature),
                    status: Some(status),
                    refusal: None,
                },
                Err(refusal) => refuse(refusal),
            }
        }
        _ => refuse(format!(
            "the request does not sign in the operator's format, {}",
            signing.format
        )),
    }
}

/// Sign with the operator's own OpenPGP program, as their git on the host
/// would call it. Git reads the status lines for the signature's creation;
/// a signer that wrote none did not sign.
async fn sign_openpgp(key: &OpenPgpKey, payload: &[u8]) -> Result<(String, String), String> {
    let mut child = tokio::process::Command::new(&key.program)
        .args(["--status-fd=2", "-bsau", &key.key])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("{} could not run on the host: {error}", key.program))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload)
            .await
            .map_err(|error| format!("the payload could not reach {}: {error}", key.program))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("{} did not finish: {error}", key.program))?;
    let status = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() || !status.contains("[GNUPG:] SIG_CREATED ") {
        let said: Vec<&str> = status
            .lines()
            .filter(|line| !line.starts_with("[GNUPG:]"))
            .collect();
        return Err(format!(
            "the host could not sign with OpenPGP key {}: {}",
            key.key,
            said.join("; ").trim()
        ));
    }
    Ok((String::from_utf8_lossy(&output.stdout).into_owned(), status))
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
/// sandbox behaves as git expects. With `gpg`'s, `-bsau <key>` goes to the
/// host with the payload from standard input, and any other operation is
/// `gpg`'s own. Returns the exit code.
pub async fn run_signer(args: &[std::ffi::OsString]) -> std::io::Result<i32> {
    let text: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let Some(at) = text.iter().position(|arg| arg == "-Y") else {
        return run_gpg_signer(args, &text).await;
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
        &GitSignRequest::Ssh {
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

/// `gpg`'s side of [`run_signer`]: git signs with `--status-fd=<n> -bsau
/// <key>`, the payload on standard input, and reads the signature from
/// standard output and the status lines from the named descriptor.
async fn run_gpg_signer(args: &[std::ffi::OsString], text: &[String]) -> std::io::Result<i32> {
    let Some(key) = text
        .iter()
        .position(|arg| arg == "-bsau")
        .and_then(|at| text.get(at + 1))
    else {
        let status = tokio::process::Command::new("gpg")
            .args(args)
            .status()
            .await?;
        return Ok(status.code().unwrap_or(1));
    };
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut payload)?;
    let socket = std::env::var_os(SOCKET_ENV)
        .ok_or_else(|| std::io::Error::other("git signing socket is unavailable"))?;
    let response = request_signature(
        Path::new(&socket),
        &GitSignRequest::Openpgp {
            key: key.clone(),
            payload: base64::engine::general_purpose::STANDARD.encode(&payload),
        },
    )
    .await?;
    let Some(signature) = response.signature else {
        eprintln!(
            "swage: commit not signed: {}",
            response
                .refusal
                .unwrap_or_else(|| "the host refused to sign".into())
        );
        return Ok(1);
    };
    use std::io::Write;
    let status = response.status.unwrap_or_default();
    let to_stdout = text.iter().any(|arg| arg == "--status-fd=1");
    std::io::stdout().write_all(signature.as_bytes())?;
    if to_stdout {
        std::io::stdout().write_all(status.as_bytes())?;
    } else {
        std::io::stderr().write_all(status.as_bytes())?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(key: SigningKey) -> KeyMaterial {
        match key {
            SigningKey::Ssh(material) => material,
            other => panic!("not an SSH key: {other:?}"),
        }
    }

    #[test]
    fn a_signing_key_literal_needs_no_file_and_a_path_reads_its_public_half() {
        let literal =
            SigningConfig::from_signing_key("key::ssh-ed25519 AAAAC3 fixture", true, false);
        let material = ssh(literal.material.clone().expect("a literal key configures"));
        assert_eq!(material.key_file, None);
        assert_eq!(material.public_key, "ssh-ed25519 AAAAC3 fixture");
        assert!(literal.commit_gpgsign && !literal.tag_gpgsign);
        assert_eq!(material.name(), "key::fixture");

        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("key");
        std::fs::write(&private, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        std::fs::write(dir.path().join("key.pub"), "ssh-ed25519 AAAAC3 fixture\n").unwrap();
        let by_private =
            ssh(
                SigningConfig::from_signing_key(private.to_str().unwrap(), false, true)
                    .material
                    .expect("a private key path finds its public half"),
            );
        assert_eq!(by_private.public_key, "ssh-ed25519 AAAAC3 fixture");
        assert_eq!(by_private.key_file.as_deref(), Some(private.as_path()));
        let by_public = ssh(SigningConfig::from_signing_key(
            dir.path().join("key.pub").to_str().unwrap(),
            false,
            false,
        )
        .material
        .expect("a public key path reads itself"));
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
            &GitSignRequest::Ssh {
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
        let material = ssh(signing.material.expect("the public key is read"));
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

    /// An operator who signs with OpenPGP is read with their key, and
    /// without a key git signs as the committer. The program never comes
    /// from the repository's configuration, which the sandbox can write: a
    /// program the repository names is ignored, and the one used is an
    /// absolute path.
    #[tokio::test]
    async fn an_openpgp_configuration_names_the_key_and_program_git_would_use() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("git runs");
            assert!(output.status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Operator"]);
        git(&["config", "user.email", "operator@example.invalid"]);
        git(&["config", "commit.gpgsign", "true"]);
        git(&["config", "gpg.format", "openpgp"]);
        git(&["config", "user.signingkey", "0xFIXTURE"]);
        let planted = dir.path().join("planted-gpg");
        std::fs::write(&planted, "#!/bin/sh\n").unwrap();
        git(&["config", "gpg.openpgp.program", planted.to_str().unwrap()]);
        git(&["config", "gpg.program", planted.to_str().unwrap()]);

        let signing = SigningConfig::from_host_git(dir.path())
            .await
            .expect("the workspace signs");
        assert_eq!(signing.format, "openpgp");
        match signing.material {
            Ok(SigningKey::Openpgp(key)) => {
                assert_eq!(key.key, "0xFIXTURE");
                assert_ne!(Path::new(&key.program), planted.as_path());
                assert!(Path::new(&key.program).is_absolute(), "{}", key.program);
            }
            // A host with no gpg at all refuses; the planted program is
            // still not chosen.
            Err(reason) => assert!(!reason.contains("planted-gpg"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            host_committer_identity(dir.path()).await.as_deref(),
            Some("Operator <operator@example.invalid>")
        );
    }

    /// A program inside the turn's write set is refused, so the host never
    /// runs one the sandbox could have replaced.
    #[test]
    fn a_signing_program_the_sandbox_can_write_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cache").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("gpg"), "#!/bin/sh\n").unwrap();
        let signing = |program: PathBuf| SigningConfig {
            format: "openpgp".into(),
            material: Ok(SigningKey::Openpgp(OpenPgpKey {
                key: "0xOPERATOR".into(),
                program: program.display().to_string(),
            })),
            commit_gpgsign: true,
            tag_gpgsign: false,
        };
        let writable = vec![dir.path().join("cache")];

        let refused = signing(bin.join("gpg")).refusing_programs_under(&writable);
        assert!(
            refused
                .material
                .as_ref()
                .is_err_and(|reason| reason.contains("where sandboxed commands can write")),
            "{:?}",
            refused.material
        );
        let elsewhere = dir.path().join("system-gpg");
        let kept = signing(elsewhere).refusing_programs_under(&writable);
        assert!(kept.material.is_ok());
    }

    /// The host signs only with the operator's key, in their format.
    #[tokio::test]
    async fn the_host_refuses_another_key_or_another_format() {
        let signing = SigningConfig {
            format: "openpgp".into(),
            material: Ok(SigningKey::Openpgp(OpenPgpKey {
                key: "0xOPERATOR".into(),
                program: "/nonexistent/gpg".into(),
            })),
            commit_gpgsign: true,
            tag_gpgsign: false,
        };
        let payload = base64::engine::general_purpose::STANDARD.encode(b"tree");
        let other_key = sign(
            &signing,
            &GitSignRequest::Openpgp {
                key: "0xSTRANGER".into(),
                payload: payload.clone(),
            },
        )
        .await;
        assert!(
            other_key
                .refusal
                .as_deref()
                .is_some_and(|refusal| refusal.contains("not the operator's configured")),
            "{other_key:?}"
        );
        let other_format = sign(
            &signing,
            &GitSignRequest::Ssh {
                namespace: "git".into(),
                public_key: "ssh-ed25519 AAAA".into(),
                payload,
            },
        )
        .await;
        assert!(
            other_format
                .refusal
                .as_deref()
                .is_some_and(|refusal| refusal.contains("operator's format")),
            "{other_format:?}"
        );
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
