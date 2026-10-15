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
