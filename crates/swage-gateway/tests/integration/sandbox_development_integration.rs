#![allow(clippy::unwrap_used)]

use crate::support::ProcessEnvGuard;
use serde_json::json;
use std::sync::Arc;
use swage_gateway::{
    git_credential::CredentialServer,
    secret::{CreateSecretRequest, SecretManager},
};
use swage_sandbox::{EgressSetup, SandboxConfig};
use swage_storage::{SqliteStorage, store::AuditStore};

#[tokio::test(flavor = "multi_thread")]
async fn git_get_resolves_over_ipc_and_unknown_hosts_receive_nothing() {
    let mut env = ProcessEnvGuard::new();
    env.set_managed_secret_key(11);
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let manager = Arc::new(SecretManager::new(storage.clone()));
    manager
        .create_secret(CreateSecretRequest {
            name: "scratch forge".into(),
            material_kind: "git_credential".into(),
            owner_kind: "operator".into(),
            backend_kind: "managed".into(),
            scope_kind: "gateway".into(),
            scope_id: None,
            metadata: Some(json!({"host": "scratch.example", "username": "operator"})),
            value: Some("fixture-forge-token".into()),
            external_reference: None,
        })
        .await
        .unwrap();
    let root = tempfile::Builder::new()
        .prefix("t")
        .tempdir_in(
            std::env::var_os("XDG_RUNTIME_DIR")
                .unwrap_or_else(|| std::env::temp_dir().into_os_string()),
        )
        .unwrap();
    let runtime_root = root.path().join("r");
    std::fs::create_dir(&runtime_root).unwrap();
    env.set_var("XDG_RUNTIME_DIR", &runtime_root);
    let server = CredentialServer::start(manager, storage.clone(), None, None).unwrap();
    let mut policy = SandboxConfig::builder()
        .working_directory(root.path())
        .write_path(root.path())
        .masked_path(&runtime_root)
        .build()
        .unwrap();
    server.configure(&mut policy).unwrap();
    let get = |host: &'static str| {
        let environment = policy.environment.clone();
        async move {
            use tokio::io::AsyncWriteExt;
            let mut command = tokio::process::Command::new("git");
            command
                .args(["credential", "fill"])
                .envs(environment)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = command.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(format!("protocol=https\nhost={host}\n\n").as_bytes())
                .await
                .unwrap();
            child.wait_with_output().await.unwrap()
        }
    };
    let known = get("scratch.example").await;
    assert!(
        known.status.success(),
        "{}",
        String::from_utf8_lossy(&known.stderr)
    );
    let answer = String::from_utf8(known.stdout).unwrap();
    assert!(answer.contains("username=operator\n"), "{answer}");
    assert!(
        answer.contains("password=fixture-forge-token\n"),
        "credential not returned"
    );
    let unknown = get("unlisted.example").await;
    assert!(
        !unknown.status.success(),
        "git must receive no credential for an unknown host"
    );
    assert!(
        unknown.stdout.is_empty(),
        "unknown host leaked a credential"
    );
    // The same policy must carry the helper and its socket into the real
    // namespace, rather than only working in the host's environment.
    use swage_sandbox::Sandbox;
    policy
        .environment
        .insert("HOME".into(), root.path().display().to_string());
    let mut sandbox = swage_sandbox::BubblewrapSandbox::new(
        policy,
        swage_sandbox::llm_handler::HostLlmHandler::new(|_| async {
            unreachable!("git uses no model")
        }),
    );
    let started = sandbox.spawn_agent().await;
    assert!(
        started.is_ok(),
        "sandbox IPC below a mask must remain reachable: {started:?}"
    );
    for (host, expected) in [("scratch.example", true), ("unlisted.example", false)] {
        let result = sandbox.execute_tool("shell", json!({"command": format!(
            "printf 'protocol=https\nhost={host}\n\n' | GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 git credential fill"
        )})).await.unwrap();
        assert!(result.error.is_none(), "{:?}", result.error);
        let output = result.output["output"].as_str().unwrap_or_default();
        assert_eq!(
            output.contains("password=fixture-forge-token"),
            expected,
            "sandboxed git must use only the requested host's credential: {output}"
        );
        assert_eq!(
            result.output["exit_code"] == 0,
            expected,
            "{:?}",
            result.output
        );
    }
    sandbox.shutdown().await.unwrap();
    let events = storage.list_audit_events().await.unwrap();
    let lookups: Vec<_> = events
        .iter()
        .filter(|event| event.event_kind == "git_credential_lookup")
        .collect();
    assert!(
        lookups
            .iter()
            .any(|event| event.metadata["host"] == "unlisted.example"
                && event.metadata["outcome"] == "absent")
    );
    assert!(
        lookups
            .iter()
            .any(|event| event.metadata["host"] == "scratch.example"
                && event.metadata["outcome"] == "resolved")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_kind == "accessed" && event.entity_kind == "secret"),
        "secret resolution must be audited"
    );
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("fixture-forge-token")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_refuses_inherited_askpass_and_audits_governed_credentials() {
    use std::os::unix::fs::PermissionsExt;
    use swage_sandbox::Sandbox;
    use tokio::io::AsyncWriteExt;

    let mut env = ProcessEnvGuard::new();
    env.set_managed_secret_key(11);
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let root = tempfile::tempdir().unwrap();
    env.set_var("XDG_RUNTIME_DIR", root.path());
    let askpass = root.path().join("askpass");
    std::fs::write(
        &askpass,
        "#!/bin/sh\ncase \"$1\" in *Username*) echo inherited-user;; *) echo inherited-password;; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gitconfig = root.path().join("gitconfig");
    std::fs::write(
        &gitconfig,
        format!("[core]\naskPass = {}\n", askpass.display()),
    )
    .unwrap();
    env.set_var("GIT_CONFIG_NOSYSTEM", "1");
    env.set_var("GIT_TERMINAL_PROMPT", "0");
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let manager = Arc::new(SecretManager::new(storage.clone()));
    manager
        .create_secret(CreateSecretRequest {
            name: "scratch forge".into(),
            material_kind: "git_credential".into(),
            owner_kind: "operator".into(),
            backend_kind: "managed".into(),
            scope_kind: "gateway".into(),
            scope_id: None,
            metadata: Some(json!({"host": "scratch.example", "username": "operator"})),
            value: Some("governed-token".into()),
            external_reference: None,
        })
        .await
        .unwrap();
    let server = CredentialServer::start(manager, storage.clone(), None, None).unwrap();

    for source in ["GIT_ASKPASS", "SSH_ASKPASS", "core.askPass"] {
        env.remove_var("GIT_ASKPASS");
        env.remove_var("SSH_ASKPASS");
        if source == "core.askPass" {
            env.set_var("GIT_CONFIG_GLOBAL", &gitconfig);
        } else {
            env.set_var("GIT_CONFIG_GLOBAL", "/dev/null");
            env.set_var(source, &askpass);
        }
        // Local credential-echoing control: Git really obtains both fields
        // from this inherited fallback without any remote connection.
        let mut control = tokio::process::Command::new("git")
            .args(["-c", "credential.helper=", "credential", "fill"])
            .current_dir(root.path())
            .env("GIT_CONFIG_COUNT", "0")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        control
            .stdin
            .take()
            .unwrap()
            .write_all(b"url=https://unlisted.example/repo.git\n\n")
            .await
            .unwrap();
        let output = control.wait_with_output().await.unwrap();
        assert!(output.status.success(), "{source}: {output:?}");
        let answer = String::from_utf8_lossy(&output.stdout);
        assert!(answer.contains("username=inherited-user\n"), "{answer}");
        assert!(answer.contains("password=inherited-password\n"), "{answer}");

        let mut policy = SandboxConfig::builder()
            .working_directory(root.path())
            .write_path(root.path())
            .build()
            .unwrap();
        server.configure(&mut policy).unwrap();
        policy
            .environment
            .insert("HOME".into(), root.path().display().to_string());
        let mut sandbox = swage_sandbox::BubblewrapSandbox::new(
            policy,
            swage_sandbox::llm_handler::HostLlmHandler::new(|_| async {
                unreachable!("git uses no model")
            }),
        );
        sandbox.spawn_agent().await.unwrap();
        for (host, expected) in [("scratch.example", true), ("unlisted.example", false)] {
            let result = sandbox
                .execute_tool(
                    "shell",
                    json!({"command": format!(
                        "printf 'url=https://{host}/repo.git\\n\\n' | git credential fill"
                    )}),
                )
                .await
                .unwrap();
            assert!(result.error.is_none(), "{:?}", result.error);
            let output = result.output["output"].as_str().unwrap_or_default();
            assert_eq!(
                result.output["exit_code"] == 0,
                expected,
                "sandboxed git must refuse unlisted credentials despite {source}: {output}"
            );
            assert!(!output.contains("inherited-user"), "{source}: {output}");
            assert!(!output.contains("inherited-password"), "{source}: {output}");
            assert_eq!(
                output.contains("password=governed-token"),
                expected,
                "{output}"
            );
        }
        sandbox.shutdown().await.unwrap();
    }
    let events = storage.list_audit_events().await.unwrap();
    for (host, outcome) in [
        ("scratch.example", "resolved"),
        ("unlisted.example", "absent"),
    ] {
        let lookups: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event_kind == "git_credential_lookup" && event.metadata["host"] == host
            })
            .collect();
        assert_eq!(lookups.len(), 3, "every {host} lookup must be audited");
        for event in lookups {
            assert_eq!(event.session_id, None);
            assert!(event.metadata["turn_id"].is_null());
            assert_eq!(event.metadata["outcome"], outcome);
        }
    }
    let audit = serde_json::to_string(&events).unwrap();
    assert!(!audit.contains("governed-token"));
    assert!(!audit.contains("inherited-password"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_configuration_excludes_inherited_parameters() {
    check_inherited_git_configuration("parameters", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_fetch_refuses_inherited_parameter_credentials() {
    check_inherited_git_configuration("parameters", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_configuration_excludes_inherited_indexed_entries() {
    check_inherited_git_configuration("indexed", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_fetch_resets_inherited_file_helpers() {
    for source in ["global", "system"] {
        check_inherited_git_configuration(source, true).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_global_file() {
    check_inherited_git_configuration("global_file", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_system_enabled() {
    check_inherited_git_configuration("system_enabled", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_system_default() {
    check_inherited_git_configuration("system_default", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_home_file() {
    check_inherited_git_configuration("home_file", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_xdg_file() {
    check_inherited_git_configuration("xdg_file", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_include_file() {
    check_inherited_git_configuration("include_file", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_git_dir() {
    check_inherited_git_configuration("git_dir", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_http_header() {
    check_inherited_git_configuration("http_header", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_boundary_url_username() {
    check_inherited_git_configuration("url_username", false).await;
}

async fn check_inherited_git_configuration(source: &str, config_first: bool) {
    use std::os::unix::fs::PermissionsExt;
    use swage_sandbox::Sandbox;

    let mut env = ProcessEnvGuard::new();
    env.set_managed_secret_key(11);
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let root = tempfile::tempdir().unwrap();
    env.set_var("XDG_RUNTIME_DIR", root.path());
    env.set_var("GIT_CONFIG_GLOBAL", "/dev/null");
    env.set_var("GIT_CONFIG_SYSTEM", "/dev/null");
    env.remove_var("GIT_CONFIG_NOSYSTEM");
    env.remove_var("GIT_CONFIG_PARAMETERS");
    env.set_var("GIT_CONFIG_COUNT", "0");
    env.set_var("INHERITED_UNLISTED_SETTING", "must-not-cross");
    env.set_var("GIT_FUTURE_SETTING", "must-not-cross");
    env.set_var("HOME", root.path());
    env.set_var("XDG_CONFIG_HOME", root.path().join("xdg"));
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
        env.remove_var(key);
    }
    let helper = root.path().join("marker-helper");
    std::fs::write(
        &helper,
        r#"#!/bin/sh
cat >/dev/null
printf 'username=inherited-user\npassword=inherited-password\n\n'
touch "$(dirname "$0")/marker-invoked"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let marker = format!("!{}", helper.display());
    match source {
        "parameters" => env.set_var(
            "GIT_CONFIG_PARAMETERS",
            format!("'credential.helper=' 'credential.helper={marker}'"),
        ),
        "indexed" => {
            env.set_var("GIT_CONFIG_COUNT", "2");
            env.set_var("GIT_CONFIG_KEY_0", "credential.helper");
            env.set_var("GIT_CONFIG_VALUE_0", "");
            env.set_var("GIT_CONFIG_KEY_1", "credential.helper");
            env.set_var("GIT_CONFIG_VALUE_1", &marker);
        }
        "global_file" | "system_enabled" | "system_default" | "home_file" | "xdg_file"
        | "include_file" | "git_dir" | "http_header" | "url_username" => {
            let file = root.path().join("inherited-config");
            let mut contents = format!("[credential]\nhelper = {marker}\n");
            if source != "url_username" {
                contents += &format!(
                    "[credential \"https://unlisted.invalid\"]\nusername = inherited-user\nhelper = {marker}\n[credential \"https://localhost:18443\"]\nusername = inherited-user\n[http]\nextraHeader = Authorization: inherited-header\n[http \"https://127.0.0.1:18443\"]\nextraHeader = X-Inherited: inherited-scoped-header\n"
                );
            }
            std::fs::write(&file, contents).unwrap();
            match source {
                "system_enabled" | "system_default" => {
                    env.set_var("GIT_CONFIG_SYSTEM", &file);
                    if source == "system_enabled" {
                        env.set_var("GIT_CONFIG_NOSYSTEM", "0");
                    }
                }
                "home_file" => {
                    env.remove_var("GIT_CONFIG_GLOBAL");
                    std::fs::rename(&file, root.path().join(".gitconfig")).unwrap();
                }
                "xdg_file" => {
                    env.remove_var("GIT_CONFIG_GLOBAL");
                    let git = root.path().join("xdg/git");
                    std::fs::create_dir_all(&git).unwrap();
                    std::fs::rename(&file, git.join("config")).unwrap();
                }
                "include_file" => {
                    let including = root.path().join("including-config");
                    std::fs::write(
                        &including,
                        format!("[include]\npath = {}\n", file.display()),
                    )
                    .unwrap();
                    env.set_var("GIT_CONFIG_GLOBAL", including);
                }
                "git_dir" => {
                    let dir = root.path().join("inherited.git");
                    let init = tokio::process::Command::new("git")
                        .args(["init", "--bare"])
                        .arg(&dir)
                        .output()
                        .await
                        .unwrap();
                    assert!(init.status.success());
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(dir.join("config"))
                        .unwrap()
                        .write_all(&std::fs::read(&file).unwrap())
                        .unwrap();
                    env.set_var("GIT_DIR", &dir);
                }
                _ => env.set_var("GIT_CONFIG_GLOBAL", &file),
            }
        }
        "global" | "system" => {
            let file = root.path().join("gitconfig");
            std::fs::write(&file, format!("[credential]\nhelper = {marker}\n")).unwrap();
            env.set_var(
                if source == "global" {
                    "GIT_CONFIG_GLOBAL"
                } else {
                    "GIT_CONFIG_SYSTEM"
                },
                &file,
            );
        }
        _ => unreachable!(),
    }
    if source == "indexed" {
        // Unused and non-numeric suffixes must not survive just because the
        // governed COUNT currently prevents Git from interpreting them.
        env.set_var("GIT_CONFIG_KEY_81", "credential.helper");
        env.set_var("GIT_CONFIG_VALUE_81", &marker);
        env.set_var("GIT_CONFIG_KEY_unused", "credential.helper");
        env.set_var("GIT_CONFIG_VALUE_unused", &marker);
    }

    let control = tokio::process::Command::new("git")
        .args(["config", "--get-all", "credential.helper"])
        .current_dir(root.path())
        .output()
        .await
        .unwrap();
    assert!(control.status.success());
    assert!(
        String::from_utf8_lossy(&control.stdout).contains(&marker),
        "the inherited {source} source must really configure the marker helper"
    );

    let cert = tokio::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-keyout",
            "key.pem",
            "-out",
            "cert.pem",
        ])
        .current_dir(root.path())
        .output()
        .await
        .unwrap();
    assert!(cert.status.success(), "local TLS fixture setup failed");
    // This server and Git run inside the same isolated network namespace.
    // No request can leave it. Only the listed host may authenticate.
    std::fs::write(root.path().join("fetch.py"), r#"
import base64, http.server, json, os, ssl, subprocess, threading

def git(*args):
    return subprocess.run(['git', *args], capture_output=True, text=True, timeout=15)

headers = []
extra_headers = []
class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        auth = self.headers.get('Authorization')
        headers.append(auth)
        extra_headers.append(self.headers.get('X-Inherited'))
        if auth != 'Basic ' + base64.b64encode(b'operator:governed-password').decode():
            self.send_response(401)
            self.send_header('WWW-Authenticate', 'Basic realm="fixture"')
            self.end_headers()
            return
        super().do_GET()

def setup_git(*args):
    environment = dict(os.environ)
    for key in ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR']:
        environment.pop(key, None)
    return subprocess.run(['git', *args], env=environment, capture_output=True, text=True, timeout=15)

assert setup_git('init', 'seed').returncode == 0
assert setup_git('-C', 'seed', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
           '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-m', 'fixture').returncode == 0
assert setup_git('clone', '--bare', 'seed', 'repo.git').returncode == 0
assert setup_git('--git-dir=repo.git', 'update-server-info').returncode == 0
server = http.server.HTTPServer(('127.0.0.1', 18443), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain('cert.pem', 'key.pem')
server.socket = context.wrap_socket(server.socket, server_side=True)
thread = threading.Thread(target=server.serve_forever)
thread.start()
try:
    assert setup_git('init', 'client').returncode == 0
    # Repository settings stay readable, but their helpers must be reset.
    marker = '!' + os.path.join(os.getcwd(), 'marker-helper')
    assert setup_git('-C', 'client', 'config', 'user.name', 'Repository Fixture').returncode == 0
    assert setup_git('-C', 'client', 'config', '--add', 'credential.helper', marker).returncode == 0
    for host in ['127.0.0.1:18443', 'localhost:18443']:
        assert setup_git('-C', 'client', 'config', '--add', f'credential.https://{host}.helper', marker).returncode == 0
    repository_name = setup_git('-C', 'client', 'config', 'user.name').stdout.strip()
    results = []
    for host in ['127.0.0.1', 'localhost', 'url-user@localhost']:
        headers.clear()
        extra_headers.clear()
        fetched = git('-C', 'client', '-c', 'http.sslVerify=false', 'fetch',
                      f'https://{host}:18443/repo.git')
        results.append({'success': fetched.returncode == 0,
                        'authenticated': any(headers), 'extra_header': any(extra_headers), 'error': fetched.stderr,
                        'governed': any(h == 'Basic ' + base64.b64encode(b'operator:governed-password').decode()
                                        for h in headers)})
    credential = subprocess.run(['git', 'credential', 'fill'],
                                input='url=https://url-user@localhost:18443/repo.git\n\n',
                                capture_output=True, text=True, timeout=15)
    username = git('config', '--get-urlmatch', 'credential.username', 'https://unlisted.invalid/repo.git')
    scoped_header = git('config', '--get-urlmatch', 'http.extraHeader', 'https://127.0.0.1:18443/repo.git')
    configured = git('config', '--get-all', 'credential.helper')
    assert configured.returncode == 0
    foreign_keys = ['INHERITED_UNLISTED_SETTING', 'GIT_FUTURE_SETTING', 'GIT_DIR',
                    'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG_SYSTEM']
    print(json.dumps({
        'fetches': results,
        'url_username_resolved': (credential.returncode == 0
                                  and 'username=operator\n' in credential.stdout
                                  and 'password=governed-password\n' in credential.stdout),
        'repository_name': repository_name,
        'foreign_env': [k for k in foreign_keys if k in os.environ],
        'username': username.stdout.strip(),
        'header': scoped_header.stdout.strip(),
        'helpers': configured.stdout.splitlines(),
        'stale': [k for k in os.environ if k.startswith(('GIT_CONFIG_KEY_', 'GIT_CONFIG_VALUE_'))
                  and k.rsplit('_', 1)[-1] not in ['0', '1', '2']],
    }))
finally:
    server.shutdown()
    thread.join()
    server.server_close()
"#).unwrap();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let manager = Arc::new(SecretManager::new(storage.clone()));
    manager
        .create_secret(CreateSecretRequest {
            name: "local forge".into(),
            material_kind: "git_credential".into(),
            owner_kind: "operator".into(),
            backend_kind: "managed".into(),
            scope_kind: "gateway".into(),
            scope_id: None,
            metadata: Some(json!({"host": "localhost:18443", "username": "operator"})),
            value: Some("governed-password".into()),
            external_reference: None,
        })
        .await
        .unwrap();
    let server = CredentialServer::start(manager, storage.clone(), None, None).unwrap();
    let mut policy = SandboxConfig::builder()
        .working_directory(root.path())
        .write_path(root.path())
        .build()
        .unwrap();
    server.configure(&mut policy).unwrap();
    let governed = policy.environment["GIT_CONFIG_VALUE_1"].clone();
    policy
        .environment
        .insert("HOME".into(), root.path().display().to_string());
    let mut sandbox = swage_sandbox::BubblewrapSandbox::new(
        policy,
        swage_sandbox::llm_handler::HostLlmHandler::new(|_| async {
            unreachable!("git uses no model")
        }),
    );
    sandbox.spawn_agent().await.unwrap();
    let result = sandbox
        .execute_tool("shell", json!({"command": "python3 fetch.py"}))
        .await
        .unwrap();
    sandbox.shutdown().await.unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.output["exit_code"], 0, "{:?}", result.output);
    let output = result.output["output"].as_str().unwrap();
    let observed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    let check_config = || {
        assert_eq!(
            observed["repository_name"], "Repository Fixture",
            "workspace repository configuration must stay readable"
        );
        assert_eq!(
            observed["username"], "",
            "inherited {source} must not supply a username"
        );
        assert_eq!(
            observed["header"], "",
            "inherited {source} must not supply an HTTP header"
        );
        let helpers: Vec<_> = observed["helpers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            helpers,
            ["", governed.as_str()],
            "inherited {source} must not appear in raw helper configuration"
        );
        assert_eq!(
            observed["foreign_env"],
            json!([]),
            "unlisted inherited environment must not cross the boundary"
        );
        assert_eq!(
            observed["stale"],
            json!([]),
            "inherited indexed Git configuration must not reach sandboxed git"
        );
    };
    if config_first {
        check_config();
    }
    assert_eq!(
        observed["fetches"][0]["authenticated"], false,
        "unlisted fetch must get no credentials from inherited {source}"
    );
    assert_eq!(observed["fetches"][0]["success"], false);
    for fetched in observed["fetches"].as_array().unwrap() {
        assert_eq!(
            fetched["extra_header"], false,
            "inherited URL-scoped HTTP headers must not reach either host"
        );
    }
    assert_eq!(
        observed["fetches"][1]["governed"], true,
        "listed fetch must authenticate through the governed helper despite {source}"
    );
    assert_eq!(
        observed["fetches"][1]["success"], true,
        "{}",
        observed["fetches"][1]["error"]
    );
    assert_eq!(
        observed["fetches"][2]["governed"], true,
        "listed URL with a username must authenticate through the governed helper"
    );
    assert_eq!(
        observed["fetches"][2]["success"], true,
        "{}",
        observed["fetches"][2]["error"]
    );
    assert_eq!(
        observed["url_username_resolved"], true,
        "listed credential URL username must resolve to the governed account"
    );
    if !config_first {
        check_config();
    }
    assert!(
        !root.path().join("marker-invoked").exists(),
        "inherited {source} helper must never execute"
    );
    let events = storage.list_audit_events().await.unwrap();
    for (host, outcome) in [
        ("127.0.0.1:18443", "absent"),
        ("localhost:18443", "resolved"),
    ] {
        assert!(
            events
                .iter()
                .any(|e| e.event_kind == "git_credential_lookup"
                    && e.metadata["host"] == host
                    && e.metadata["outcome"] == outcome),
            "every fetch credential lookup must be audited: {host} {outcome}"
        );
    }
    let audit = serde_json::to_string(&events).unwrap();
    assert!(!audit.contains("governed-password"));
    assert!(!audit.contains("inherited-password"));
}

struct NoTools;
#[async_trait::async_trait]
impl swage_python::NestedToolHost for NoTools {
    async fn dispatch_tool(
        &self,
        _: &str,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, swage_python::worker_protocol::WorkerToolError> {
        unreachable!("this Python code calls no tools")
    }
}

#[derive(Default)]
struct Connections(std::sync::Mutex<Vec<swage_sandbox::EgressConnection>>);
#[async_trait::async_trait]
impl swage_sandbox::EgressRecorder for Connections {
    async fn record(&self, record: swage_sandbox::EgressConnection) -> Result<(), String> {
        self.0.lock().unwrap().push(record);
        Ok(())
    }
}
struct Refuse;
#[async_trait::async_trait]
impl swage_sandbox::EgressPolicy for Refuse {
    async fn decide(&self, _: &swage_sandbox::EgressRequest) -> swage_sandbox::EgressVerdict {
        swage_sandbox::EgressVerdict::Refused {
            reason: "test refusal".into(),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn python_uses_the_callers_workspace_masks_and_recording_proxy() {
    let mut env = ProcessEnvGuard::new();
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let root = tempfile::Builder::new()
        .prefix("t")
        .tempdir_in(
            std::env::var_os("XDG_RUNTIME_DIR")
                .unwrap_or_else(|| std::env::temp_dir().into_os_string()),
        )
        .unwrap();
    let runtime_root = root.path().join("r");
    std::fs::create_dir(&runtime_root).unwrap();
    env.set_var("XDG_RUNTIME_DIR", &runtime_root);
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    let private = root.path().join("private");
    for path in [&workspace, &outside, &private] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(private.join("key"), "private fixture").unwrap();
    let policy = SandboxConfig::builder()
        .working_directory(&workspace)
        .write_path(&workspace)
        .masked_path(&private)
        .masked_path(&runtime_root)
        .build()
        .unwrap();
    let recorded = Arc::new(Connections::default());
    let egress = EgressSetup {
        policy: Arc::new(Refuse),
        recorder: recorded.clone(),
    };
    let runtime = swage_python::PythonWorkerRuntime::new(Default::default()).unwrap();
    let mut worker = runtime.spawn_worker_for_turn(policy, egress).await.unwrap();
    let code = format!(
        r#"
import os, urllib.request, urllib.error
print('cwd=' + os.getcwd())
fd = os.open('made', os.O_WRONLY | os.O_CREAT, 0o600)
os.write(fd, b'written')
os.close(fd)
try:
    fd = os.open({outside:?} + '/escape', os.O_WRONLY | os.O_CREAT, 0o600)
    os.close(fd)
    print('escaped')
except Exception as error:
    if error.errno not in (1, 13, 30): raise
    print('write refused')
try:
    fd = os.open({private:?} + '/key', os.O_RDONLY)
    print(os.read(fd, 1024))
    os.close(fd)
except Exception as error:
    if error.errno != 2: raise
    print('key hidden')
try:
    urllib.request.urlopen('https://unlisted.example/', timeout=5)
except urllib.error.URLError as error:
    print(str(error))
"#,
        outside = outside.display().to_string(),
        private = private.display().to_string()
    );
    let result = runtime
        .execute_python(&mut worker, &code, &workspace, &NoTools)
        .await
        .unwrap();
    assert!(result.traceback.is_none(), "{:?}", result.traceback);
    assert!(
        result
            .stdout
            .contains(&format!("cwd={}\n", workspace.display())),
        "{}",
        result.stdout
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("made")).unwrap(),
        "written"
    );
    assert!(
        !outside.join("escape").exists(),
        "Python escaped the caller's write set"
    );
    assert!(result.stdout.contains("write refused"), "{}", result.stdout);
    assert!(result.stdout.contains("key hidden"), "{}", result.stdout);
    assert!(result.stdout.contains("403"), "{}", result.stdout);
    let attempts = recorded.0.lock().unwrap();
    assert_eq!(
        attempts.len(),
        1,
        "Python must use the supplied recording proxy"
    );
    assert_eq!(attempts[0].host, "unlisted.example");
    assert!(!attempts[0].verdict.is_allowed());
}

#[tokio::test(flavor = "multi_thread")]
async fn git_path_scoped_credentials_match_only_the_repository_url() {
    let mut env = ProcessEnvGuard::new();
    env.set_managed_secret_key(11);
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let root = tempfile::tempdir().unwrap();
    env.set_var("XDG_RUNTIME_DIR", root.path());
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let manager = Arc::new(SecretManager::new(storage.clone()));
    manager
        .create_secret(CreateSecretRequest {
            name: "repository credential".into(),
            material_kind: "git_credential".into(),
            owner_kind: "operator".into(),
            backend_kind: "managed".into(),
            scope_kind: "gateway".into(),
            scope_id: None,
            metadata: Some(json!({
                "host": "scratch.example", "username": "operator", "path": "owner/repo.git"
            })),
            value: Some("repository-token".into()),
            external_reference: None,
        })
        .await
        .unwrap();
    let server = CredentialServer::start(manager, storage, None, None).unwrap();
    let mut policy = SandboxConfig::builder()
        .working_directory(root.path())
        .build()
        .unwrap();
    server.configure(&mut policy).unwrap();
    for (path, expected) in [("owner/repo.git", true), ("owner/other.git", false)] {
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new("git")
            .args(["credential", "fill"])
            .current_dir(root.path())
            .envs(&policy.environment)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ASKPASS", "/bin/false")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("url=https://scratch.example/{path}\n\n").as_bytes())
            .await
            .unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert_eq!(
            output.status.success(),
            expected,
            "git must resolve only the credential's repository URL ({path}): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if expected {
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("password=repository-token\n")
            );
        } else {
            assert!(
                output.stdout.is_empty(),
                "another repository received credentials"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn python_runtime_beneath_a_mask_starts_and_imports_stdlib() {
    let mut env = ProcessEnvGuard::new();
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("private");
    let runtime_root = private.join("python");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(private.join("secret"), "hidden sibling").unwrap();
    // Assemble an explicit runtime from the installed interpreter, without
    // copying third-party packages or depending on the operator's runtime.
    let fixture = std::process::Command::new("python3")
        .args([
            "-I",
            "-S",
            "-c",
            r#"
import pathlib, shutil, sys, sysconfig
root = pathlib.Path(sys.argv[1])
(root / 'bin').mkdir(parents=True)
shutil.copy2(pathlib.Path(sys.executable).resolve(), root / 'bin' / 'python3')
stdlib = pathlib.Path(sysconfig.get_path('stdlib'))
shutil.copytree(stdlib, root / 'lib' / stdlib.name,
                ignore=shutil.ignore_patterns('site-packages', 'dist-packages', '__pycache__'))
"#,
        ])
        .arg(&runtime_root)
        .output()
        .unwrap();
    assert!(
        fixture.status.success(),
        "runtime fixture: {}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let runtime =
        swage_python::PythonWorkerRuntime::new(swage_config::model::PythonExecuteToolConfig {
            runtime_path: Some(runtime_root.display().to_string()),
            startup_timeout_secs: 3,
            ..Default::default()
        })
        .unwrap();
    let policy = SandboxConfig::builder()
        .working_directory(&workspace)
        .write_path(&workspace)
        .masked_path(&private)
        .build()
        .unwrap();
    let started = runtime
        .spawn_worker_for_turn(
            policy,
            EgressSetup {
                policy: Arc::new(Refuse),
                recorder: Arc::new(Connections::default()),
            },
        )
        .await;
    assert!(
        started.is_ok(),
        "runtime beneath a mask must start: {:?}",
        started.err()
    );
    let mut worker = started.unwrap();
    let code = format!(
        r#"
import fractions, os
print(fractions.Fraction(1, 3) + fractions.Fraction(2, 3))
print(os.path.exists({secret:?}))
try:
    os.open({write_probe:?}, os.O_WRONLY | os.O_CREAT, 0o600)
    print('runtime writable')
except Exception as error:
    if error.errno not in (1, 13, 30): raise
    print('runtime read-only')
"#,
        secret = private.join("secret").display().to_string(),
        write_probe = runtime_root.join("write-probe").display().to_string()
    );
    let result = runtime
        .execute_python(&mut worker, &code, &workspace, &NoTools)
        .await
        .unwrap();
    assert!(result.traceback.is_none(), "{:?}", result.traceback);
    assert_eq!(result.stdout, "1\nFalse\nruntime read-only\n");
}

/// A sandboxed `git commit` under an operator who signs with SSH is signed
/// on the host with the operator's key: the sandbox's git calls the `swage`
/// binary as its signing program, the host signs over the turn's socket,
/// another key is refused, each signing is recorded, and the signature
/// verifies with `ssh-keygen` on the host.
#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_git_commits_are_signed_on_the_host_with_the_operators_key() {
    use swage_gateway::git_signer::{SignerServer, SigningConfig};

    let mut env = ProcessEnvGuard::new();
    env.set_managed_secret_key(13);
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let manager = Arc::new(SecretManager::new(storage.clone()));
    let root = tempfile::Builder::new()
        .prefix("t")
        .tempdir_in(
            std::env::var_os("XDG_RUNTIME_DIR")
                .unwrap_or_else(|| std::env::temp_dir().into_os_string()),
        )
        .unwrap();
    let runtime_root = root.path().join("r");
    std::fs::create_dir(&runtime_root).unwrap();
    env.set_var("XDG_RUNTIME_DIR", &runtime_root);

    // The operator's key, as `user.signingkey` would name it, and a second
    // key that is nobody's configured signing key.
    let keys = root.path().join("keys");
    std::fs::create_dir(&keys).unwrap();
    for name in ["operator", "stranger"] {
        let generated = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
            .arg(keys.join(name))
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
    }
    let operator_pub = keys.join("operator.pub");
    let signing = SigningConfig::from_signing_key(operator_pub.to_str().unwrap(), true, false);
    assert!(signing.material.is_ok(), "{:?}", signing.material);
    let credentials = CredentialServer::start(manager, storage.clone(), None, None).unwrap();
    let signer = SignerServer::start(storage.clone(), None, None, signing).unwrap();
    let mut policy = SandboxConfig::builder()
        .working_directory(root.path())
        .write_path(root.path())
        .masked_path(&runtime_root)
        .masked_path(&keys)
        .build()
        .unwrap();
    credentials.configure(&mut policy).unwrap();
    signer.configure(&mut policy).unwrap();
    assert!(
        policy
            .environment
            .iter()
            .any(|(key, value)| key.starts_with("GIT_CONFIG_VALUE_")
                && value.starts_with("key::ssh-ed25519")),
        "the sandboxed git is configured with the operator's public key as a literal"
    );

    // The signing program, as git would call it, against the host signer.
    let sign = |key_file: std::path::PathBuf, payload: &'static str| {
        let environment = policy.environment.clone();
        let dir = root.path().join(uuid::Uuid::new_v4().to_string());
        async move {
            std::fs::create_dir(&dir).unwrap();
            let file = dir.join("payload");
            std::fs::write(&file, payload).unwrap();
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_swage"))
                .args(["-Y", "sign", "-n", "git", "-f"])
                .arg(&key_file)
                .arg(&file)
                .envs(environment)
                .output()
                .await
                .unwrap();
            (output, file)
        }
    };
    let (signed, file) = sign(operator_pub.clone(), "tree 1\n").await;
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    let signature = std::fs::read_to_string(format!("{}.sig", file.display())).unwrap();
    assert!(signature.contains("BEGIN SSH SIGNATURE"), "{signature}");
    let allowed = root.path().join("allowed_signers");
    std::fs::write(
        &allowed,
        format!(
            "operator {}",
            std::fs::read_to_string(&operator_pub).unwrap()
        ),
    )
    .unwrap();
    let verified = std::process::Command::new("ssh-keygen")
        .args(["-Y", "verify", "-f"])
        .arg(&allowed)
        .args(["-I", "operator", "-n", "git", "-s"])
        .arg(format!("{}.sig", file.display()))
        .stdin(std::fs::File::open(&file).unwrap())
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );

    let (refused, other) = sign(keys.join("stranger.pub"), "tree 2\n").await;
    assert!(
        !refused.status.success(),
        "a key the operator did not configure must not be signed with"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("not the operator's configured signing key"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !other.with_extension("sig").exists()
            && !std::path::Path::new(&format!("{}.sig", other.display())).exists()
    );

    // A real sandboxed commit, with the keys masked inside: git signs through
    // the program, and the host verifies the commit it produced.
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "operator@example.invalid"][..],
        &["config", "user.name", "Operator"][..],
    ] {
        let done = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
    }
    use swage_sandbox::Sandbox;
    policy
        .environment
        .insert("HOME".into(), root.path().display().to_string());
    let mut sandbox = swage_sandbox::BubblewrapSandbox::new(
        policy,
        swage_sandbox::llm_handler::HostLlmHandler::new(|_| async {
            unreachable!("git uses no model")
        }),
    );
    sandbox.spawn_agent().await.expect("sandbox starts");
    let result = sandbox
        .execute_tool(
            "shell",
            json!({"command": format!(
                "cd {} && ls {} | wc -l && git commit -q --allow-empty -m signed && git log -1 --format=%H",
                repo.display(), keys.display()
            )}),
        )
        .await
        .unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    let output = result.output["output"].as_str().unwrap_or_default();
    assert_eq!(
        result.output["exit_code"], 0,
        "the sandboxed commit must succeed: {output}"
    );
    assert!(
        output.trim_start().starts_with('0'),
        "the keys stay masked inside the sandbox: {output}"
    );
    sandbox.shutdown().await.unwrap();
    let shown = std::process::Command::new("git")
        .args([
            "-c",
            &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
            "log",
            "-1",
            "--show-signature",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let shown_text = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown_text.contains("Good \"git\" signature"),
        "{shown_text}\n{}",
        String::from_utf8_lossy(&shown.stderr)
    );

    let events = storage.list_audit_events().await.unwrap();
    let signings: Vec<_> = events
        .iter()
        .filter(|event| event.event_kind == "git_signing")
        .collect();
    assert!(
        signings
            .iter()
            .filter(|e| e.metadata["outcome"] == "signed")
            .count()
            >= 2,
        "{signings:?}"
    );
    assert!(
        signings.iter().any(|e| e.metadata["outcome"] == "refused"),
        "{signings:?}"
    );
    assert!(signings.iter().all(|e| {
        e.metadata["key"]
            .as_str()
            .is_some_and(|k| k.ends_with("operator.pub"))
            && e.metadata["fingerprint"]
                .as_str()
                .is_some_and(|f| f.starts_with("SHA256:"))
            && e.entity_id == e.metadata["fingerprint"]
    }));
}

/// A sandboxed commit carries the operator's author identity as the
/// workspace's git configures it, with no git configuration of the host
/// visible inside; where the host configures none, the sandboxed commit is
/// refused by git itself rather than made under an invented author.
#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_commits_carry_the_operators_author_identity_or_ask_for_one() {
    use swage_gateway::git_identity::CommitIdentity;
    use swage_sandbox::Sandbox;

    let mut env = ProcessEnvGuard::new();
    env.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let root = tempfile::Builder::new()
        .prefix("t")
        .tempdir_in(
            std::env::var_os("XDG_RUNTIME_DIR")
                .unwrap_or_else(|| std::env::temp_dir().into_os_string()),
        )
        .unwrap();
    let git = |repo: &std::path::Path, args: &[&str]| {
        let done = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
        String::from_utf8_lossy(&done.stdout).trim().to_owned()
    };
    let commit_inside = |policy: SandboxConfig, repo: std::path::PathBuf| async move {
        let mut sandbox = swage_sandbox::BubblewrapSandbox::new(
            policy,
            swage_sandbox::llm_handler::HostLlmHandler::new(|_| async {
                unreachable!("git uses no model")
            }),
        );
        sandbox.spawn_agent().await.expect("sandbox starts");
        let result = sandbox
            .execute_tool(
                "shell",
                json!({"command": format!(
                    "cd {} && git commit -q --allow-empty -m inside 2>&1; echo exit=$?",
                    repo.display()
                )}),
            )
            .await
            .unwrap();
        sandbox.shutdown().await.unwrap();
        assert!(result.error.is_none(), "{:?}", result.error);
        result.output["output"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };

    // The operator's identity, configured for the workspace on the host.
    let named = root.path().join("named");
    std::fs::create_dir(&named).unwrap();
    git(&named, &["init", "-q"]);
    git(&named, &["config", "user.name", "Operator Example"]);
    git(
        &named,
        &["config", "user.email", "operator@example.invalid"],
    );
    let identity = CommitIdentity::from_host_git(&named)
        .await
        .expect("the workspace's identity is read on the host");
    let mut policy = SandboxConfig::builder()
        .working_directory(root.path())
        .write_path(root.path())
        .build()
        .unwrap();
    identity.configure(&mut policy);
    policy
        .environment
        .insert("HOME".into(), root.path().display().to_string());
    let output = commit_inside(policy, named.clone()).await;
    assert!(output.trim_end().ends_with("exit=0"), "{output}");
    assert_eq!(
        git(&named, &["log", "-1", "--format=%an <%ae>"]),
        "Operator Example <operator@example.invalid>"
    );

    // No identity on the host for this workspace: git inside asks, and no
    // commit is made.
    let anonymous = root.path().join("anonymous");
    std::fs::create_dir(&anonymous).unwrap();
    git(&anonymous, &["init", "-q"]);
    git(&anonymous, &["config", "user.name", ""]);
    git(&anonymous, &["config", "user.email", ""]);
    assert_eq!(CommitIdentity::from_host_git(&anonymous).await, None);
    let mut policy = SandboxConfig::builder()
        .working_directory(root.path())
        .write_path(root.path())
        .build()
        .unwrap();
    policy
        .environment
        .insert("HOME".into(), root.path().display().to_string());
    let output = commit_inside(policy, anonymous.clone()).await;
    assert!(!output.trim_end().ends_with("exit=0"), "{output}");
    assert!(
        output.contains("tell me who you are") || output.contains("empty ident"),
        "git inside must ask for an identity rather than commit: {output}"
    );
    let commits = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(&anonymous)
        .output()
        .unwrap();
    assert!(
        !commits.status.success() || String::from_utf8_lossy(&commits.stdout).trim() == "0",
        "no commit may exist under an invented author"
    );
}
