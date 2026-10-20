#![allow(clippy::unwrap_used)]

use serde_json::json;
use swage_core::{CompletionRequest, CompletionResponse, FinishReason, SandboxError};
use swage_sandbox::llm_handler::HostLlmHandler;
use swage_sandbox::{BubblewrapSandbox, FallbackSandbox, Sandbox, SandboxConfig, find_bwrap};

use crate::support::ProcessEnvGuard;

fn test_llm_handler() -> HostLlmHandler {
    HostLlmHandler::new(|_req: CompletionRequest| async {
        Ok(CompletionResponse {
            content: "ok".into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    })
}

fn test_config(temp_dir: &tempfile::TempDir) -> SandboxConfig {
    SandboxConfig::builder()
        .working_directory(temp_dir.path())
        .write_path(temp_dir.path())
        .tool_call_timeout(std::time::Duration::from_secs(10))
        .llm_request_timeout(std::time::Duration::from_secs(10))
        .shutdown_grace_period(std::time::Duration::from_secs(5))
        .build()
        .expect("sandbox config should build")
}

#[tokio::test]
async fn bubblewrap_sandbox_real_binary_runs_git_and_standard_device_io_through_shell() {
    let name = "bubblewrap_sandbox_real_binary_runs_git_and_standard_device_io_through_shell";
    if !bubblewrap_check_available(name) {
        return;
    }

    let mut env_guard = ProcessEnvGuard::new();
    let temp_dir = tempfile::tempdir().unwrap();
    let mut sandbox = BubblewrapSandbox::new(test_config(&temp_dir), test_llm_handler());

    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));

    sandbox.spawn_agent().await.unwrap();

    let result = sandbox
        .execute_tool(
            "shell",
            json!({ "command": "export HOME=/tmp GIT_CONFIG_NOSYSTEM=1; for device in /dev/full /dev/null /dev/random /dev/tty /dev/urandom /dev/zero; do test -c \"$device\"; done && test ! -e /dev/kvm && printf device-io >/dev/null && head -c 1 /dev/null >/dev/null && head -c 1 /dev/zero >/dev/null && head -c 1 /dev/urandom >/dev/null && head -c 1 /dev/random >/dev/null && git init repo && git -C repo config user.email test@example.invalid && git -C repo config user.name test && touch repo/note && git -C repo add note && git -C repo commit -m initial" }),
        )
        .await
        .unwrap();

    assert!(
        result.error.is_none(),
        "git init and commit must work in the writable workspace: {:?}",
        result.error
    );
    assert_eq!(result.output["exit_code"], 0, "shell result: {result:?}");

    let full = sandbox
        .execute_tool(
            "shell",
            json!({ "command": "dd if=/dev/zero of=/dev/full bs=1 count=1 status=none" }),
        )
        .await
        .unwrap();
    assert!(full.error.is_none(), "shell dispatch failed: {full:?}");
    assert_ne!(
        full.output["exit_code"], 0,
        "/dev/full must reject writes: {full:?}"
    );
    assert!(
        full.output["output"]
            .as_str()
            .is_some_and(|output| output.contains("No space left on device")),
        "/dev/full failure must remain ENOSPC rather than a sandbox denial: {full:?}"
    );

    // Everything outside the write set is read-only: the root is bound
    // read-only and Landlock grants no write there.
    let denied = sandbox
        .execute_tool(
            "shell",
            json!({ "command": "touch /usr/swage-sandbox-wrote-here 2>&1; echo exit=$?" }),
        )
        .await
        .unwrap();
    assert!(denied.error.is_none(), "shell dispatch failed: {denied:?}");
    assert!(
        denied.output["output"]
            .as_str()
            .is_some_and(|output| output.contains("Read-only file system")
                || output.contains("Permission denied")),
        "a write outside the write set must fail: {denied:?}"
    );

    let tty = sandbox
        .execute_tool(
            "shell",
            json!({ "command": "dd if=/dev/tty of=/dev/null count=0 status=none" }),
        )
        .await
        .unwrap();
    assert!(tty.error.is_none(), "shell dispatch failed: {tty:?}");
    assert_ne!(
        tty.output["exit_code"], 0,
        "headless /dev/tty open must not succeed"
    );
    assert!(
        tty.output["output"].as_str().is_some_and(|output| {
            output.contains("No such device or address") && !output.contains("Permission denied")
        }),
        "/dev/tty must reach its ordinary headless ENXIO rather than a Landlock denial: {tty:?}"
    );
    sandbox.shutdown().await.unwrap();
    eprintln!("bubblewrap check completed: {name}");
}

/// A host-side HTTP server the sandboxed command can only reach through the
/// proxy: it answers every request with its request line.
fn host_http_server() -> (u16, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming().take(1) {
            let mut stream = stream.unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let body = format!("served {}", line.trim_end());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    (port, handle)
}

struct OnlyPort(u16);

#[async_trait::async_trait]
impl swage_sandbox::EgressPolicy for OnlyPort {
    async fn decide(&self, request: &swage_sandbox::EgressRequest) -> swage_sandbox::EgressVerdict {
        if request.port == self.0 {
            swage_sandbox::EgressVerdict::Allowed {
                basis: "the test's one port".into(),
            }
        } else {
            swage_sandbox::EgressVerdict::Refused {
                reason: "not the test's port".into(),
            }
        }
    }
}

#[derive(Default)]
struct Recorded(std::sync::Mutex<Vec<swage_sandbox::EgressConnection>>);

#[async_trait::async_trait]
impl swage_sandbox::EgressRecorder for Recorded {
    async fn record(&self, connection: swage_sandbox::EgressConnection) -> Result<(), String> {
        self.0.lock().unwrap().push(connection);
        Ok(())
    }
}

#[tokio::test]
async fn bubblewrap_sandbox_reads_broadly_masks_private_paths_and_reaches_the_network_only_through_the_recording_proxy()
 {
    let bwrap = match find_bwrap() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("bubblewrap consumer unavailable: {error}");
            return;
        }
    };
    let probe = std::process::Command::new(bwrap)
        .args([
            "--unshare-all",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "true",
        ])
        .output()
        .expect("invoke real bubblewrap namespace probe");
    if !probe.status.success() {
        eprintln!(
            "bubblewrap consumer unavailable: namespace probe {}",
            probe.status
        );
        return;
    }

    // A home with a key directory and a token file to mask, a workspace to
    // write, and a sibling directory outside the write set.
    // Ungranted host /tmp contents are intentionally hidden. Put the home
    // fixture beside the runtime, as in the development sandbox tests, so
    // notes exercise broad host reads rather than private temporary storage.
    let home = tempfile::Builder::new()
        .prefix("t")
        .tempdir_in(
            std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|path| !std::path::Path::new(path).starts_with("/tmp"))
                .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into()),
        )
        .unwrap();
    let ssh = home.path().join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    std::fs::write(ssh.join("id_ed25519"), "PRIVATE KEY").unwrap();
    std::fs::write(
        home.path().join(".netrc"),
        "machine forge login me password secret",
    )
    .unwrap();
    std::fs::write(home.path().join("notes.txt"), "readable").unwrap();
    let workspace = home.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = home.path().join("outside");
    std::fs::create_dir(&outside).unwrap();

    let granted = ssh.join("granted");
    std::fs::create_dir(&granted).unwrap();
    std::fs::write(granted.join("visible"), "granted subtree").unwrap();

    let (port, server) = host_http_server();
    let recorded = std::sync::Arc::new(Recorded::default());
    let config = SandboxConfig::builder()
        .working_directory(&workspace)
        .write_path(&workspace)
        .masked_path(&ssh)
        .read_only_path(&granted)
        .masked_path(home.path().join(".netrc"))
        .tool_call_timeout(std::time::Duration::from_secs(15))
        .llm_request_timeout(std::time::Duration::from_secs(10))
        .shutdown_grace_period(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let mut sandbox = BubblewrapSandbox::new(config, test_llm_handler()).with_egress(
        swage_sandbox::EgressSetup {
            policy: std::sync::Arc::new(OnlyPort(port)),
            recorder: recorded.clone(),
        },
    );
    sandbox.spawn_agent().await.unwrap();

    let shell = |command: String| {
        let sandbox = &sandbox;
        async move {
            let result = sandbox
                .execute_tool("shell", json!({ "command": command }))
                .await
                .unwrap();
            assert!(result.error.is_none(), "shell dispatch failed: {result:?}");
            result.output["output"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        }
    };

    // Keep observations separate: an empty token must not satisfy the
    // assertion about hidden directory entries.
    let notes = shell(format!("cat {}", home.path().join("notes.txt").display())).await;
    assert_eq!(notes.trim(), "readable", "reads beside masks remain broad");
    let siblings = shell(format!(
        "find {} -mindepth 1 -maxdepth 1 ! -name granted -printf '%f\\n'",
        ssh.display(),
    ))
    .await;
    assert_eq!(
        siblings.trim(),
        "",
        "masked sibling entries must stay hidden even if their contents are unreadable"
    );
    let token_bytes = shell(format!(
        "cat {} 2>/dev/null | wc -c",
        home.path().join(".netrc").display()
    ))
    .await;
    assert_eq!(
        token_bytes.trim(),
        "0",
        "the masked token must yield no readable bytes"
    );

    let reopened = shell(format!("cat {}/visible", granted.display())).await;
    assert_eq!(
        reopened.trim(),
        "granted subtree",
        "grant beneath mask must reopen"
    );
    let sibling_state = shell(format!(
        "if test -e {}/id_ed25519; then printf present; else printf absent; fi",
        ssh.display(),
    ))
    .await;
    assert_eq!(
        sibling_state.trim(),
        "absent",
        "the granted subtree must not expose its private sibling's name"
    );

    let sibling_contents = shell(format!(
        "cat {}/id_ed25519 2>/dev/null || true",
        ssh.display(),
    ))
    .await;
    assert_eq!(
        sibling_contents.trim(),
        "",
        "the granted subtree must not reveal its private sibling's contents"
    );

    // Writes: the workspace yes, the sibling outside the write set no.
    let writes = shell(format!(
        "echo made > {ws}/made && cat {ws}/made; touch {out}/nope 2>&1; echo outside=$?",
        ws = workspace.display(),
        out = outside.display(),
    ))
    .await;
    assert!(writes.contains("made"), "{writes}");
    assert!(
        writes.contains("outside=1")
            && (writes.contains("Read-only") || writes.contains("Permission denied")),
        "a write outside the write set must fail: {writes}"
    );

    // The network: the proxy is in the environment, an allowed destination is
    // served through it, a refused one gets 403, and both are recorded. The
    // test server is on the host's loopback, which the bridge spares, so the
    // client is told to proxy it anyway.
    let network = shell(format!(
        "echo proxy=$HTTPS_PROXY; curl -sS --noproxy '' http://127.0.0.1:{port}/served-path; echo; \
         curl -s --noproxy '' -o /dev/null -w 'refused=%{{http_code}}' http://127.0.0.1:1/other"
    ))
    .await;
    assert!(network.contains("proxy=http://127.0.0.1:"), "{network}");
    assert!(
        network.contains("served GET /served-path HTTP/1.1"),
        "{network}"
    );
    assert!(network.contains("refused=403"), "{network}");
    let records = recorded.0.lock().unwrap().clone();
    assert_eq!(records.len(), 2, "{records:?}");
    assert!(
        records[0].verdict.is_allowed() && records[0].port == port,
        "{records:?}"
    );
    assert!(
        !records[1].verdict.is_allowed() && records[1].port == 1,
        "{records:?}"
    );

    sandbox.shutdown().await.unwrap();
    server.join().unwrap();
}

#[tokio::test]
async fn fallback_sandbox_real_binary_strips_credentials_from_child_env() {
    let mut env_guard = ProcessEnvGuard::new();
    let temp_dir = tempfile::tempdir().unwrap();
    let mut sandbox = FallbackSandbox::new(test_config(&temp_dir), test_llm_handler());

    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    env_guard.set_var("OPENAI_API_KEY", "sk-test-host-key");
    env_guard.set_var("ANTHROPIC_API_KEY", "sk-ant-test-host-key");
    env_guard.set_var("GOOGLE_API_KEY", "test-google-key");

    sandbox.spawn_agent().await.unwrap();

    for var_name in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_API_KEY"] {
        let response = sandbox
            .execute_tool("env_read", json!({ "name": var_name }))
            .await
            .unwrap();
        assert_eq!(
            response.output,
            serde_json::Value::Null,
            "{var_name} must not be present in the sandboxed child process"
        );
    }

    sandbox.shutdown().await.unwrap();
}

#[tokio::test]
async fn fallback_sandbox_real_binary_runs_workspace_tools() {
    let mut env_guard = ProcessEnvGuard::new();
    let temp_dir = tempfile::tempdir().unwrap();
    std::fs::write(temp_dir.path().join("note.txt"), "hello from sandbox").unwrap();
    let mut sandbox = FallbackSandbox::new(test_config(&temp_dir), test_llm_handler());

    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));

    sandbox.spawn_agent().await.unwrap();

    let listed = sandbox
        .execute_tool("file_list", json!({ "path": "." }))
        .await
        .unwrap();
    assert!(
        listed.output["output"]
            .as_str()
            .expect("file_list output should be text")
            .contains("note.txt"),
        "file_list should run inside the sandbox child, got: {}",
        listed.output
    );

    let shell = sandbox
        .execute_tool(
            "shell",
            json!({ "command": "printf '%s' \"$PWD\" && test -f note.txt" }),
        )
        .await
        .unwrap();
    assert!(
        shell.error.is_none(),
        "shell should succeed inside the sandbox child, got error: {:?}",
        shell.error
    );
    assert!(
        shell.output["output"]
            .as_str()
            .expect("shell output should be text")
            .contains(temp_dir.path().to_string_lossy().as_ref()),
        "shell should run in the configured sandbox workspace, got: {}",
        shell.output
    );

    sandbox.shutdown().await.unwrap();
}

#[tokio::test]
async fn fallback_sandbox_real_binary_rejects_oversized_ipc_payloads() {
    let mut env_guard = ProcessEnvGuard::new();
    let temp_dir = tempfile::tempdir().unwrap();
    let mut sandbox = FallbackSandbox::new(test_config(&temp_dir), test_llm_handler());

    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));

    sandbox.spawn_agent().await.unwrap();

    let err = sandbox
        .execute_tool("echo", json!({ "blob": "x".repeat(1_100_000) }))
        .await
        .unwrap_err();
    match err {
        SandboxError::IpcError { message } => {
            assert!(message.contains("byte limit"), "got: {message}");
        }
        other => panic!("expected SandboxError::IpcError, got {other:?}"),
    }

    sandbox.shutdown().await.unwrap();
}

/// Host PID of the process whose command line is exactly `argv` and whose PID
/// in its own namespace (the last `NSpid` field) is `namespace_pid`.
fn host_pid_of(argv: &[&str], namespace_pid: &str) -> Option<u32> {
    std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .find_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            let args: Vec<&[u8]> = cmdline
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .collect();
            if args != argv.iter().map(|a| a.as_bytes()).collect::<Vec<_>>() {
                return None;
            }
            let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
            let ns_pid = status.lines().find_map(|l| l.strip_prefix("NSpid:"))?;
            (ns_pid.split_whitespace().last()? == namespace_pid).then_some(pid)
        })
}

/// Whether `pid` still names a live (not yet exited) process.
fn is_live(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        // The state field follows the parenthesised command name.
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| !rest.starts_with('Z') && !rest.starts_with('X'))
    })
}

/// Start a shell command that spawns a long-lived descendant (a `sleep` with
/// its own command line) and appends to a marker file every 200 ms, abandon
/// the call as a cancelled turn does (by dropping its future), then require
/// the marker to stop growing, the descendant's PID to be gone, and the
/// sandbox to serve the next call.
async fn assert_abandoned_shell_command_stops(sandbox: &dyn Sandbox, workspace: &std::path::Path) {
    let marker = workspace.join("marker");
    // A duration unique to this run, so the descendant's command line names
    // only this test's process.
    let sleep_arg = format!(
        "300.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    );
    let command = format!(
        "sleep {sleep_arg} & echo $! > descendant.pid; \
         while true; do date +%s%N >> marker; sleep 0.2; done"
    );
    let mut call = Box::pin(sandbox.execute_tool("shell", json!({ "command": command })));
    // Capture the descendant while the call is still pending, so its PID is
    // read before any cancellation can have fired.
    let descendant = tokio::select! {
        result = call.as_mut() => panic!("the looping command must still be running, got {result:?}"),
        descendant = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let found = std::fs::read_to_string(workspace.join("descendant.pid"))
                    .ok()
                    .and_then(|pid| host_pid_of(&["sleep", &sleep_arg], pid.trim()));
                if let Some(pid) = found.filter(|_| marker.exists()) {
                    return pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }) => descendant.expect("the command and its descendant must start"),
    };
    assert!(
        is_live(descendant),
        "the descendant must be running before the call is abandoned"
    );
    // Abandon the call as a cancelled turn does: drop its pending future.
    drop(call);

    // Allow the cancel to reach the agent and the process group to die.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let settled_len = std::fs::metadata(&marker).unwrap().len();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(
        std::fs::metadata(&marker).unwrap().len(),
        settled_len,
        "the abandoned command must stop appending to its marker"
    );
    assert!(
        !is_live(descendant),
        "the abandoned command's descendant (pid {descendant}) must be gone"
    );

    let next = sandbox
        .execute_tool("shell", json!({ "command": "printf after-cancel" }))
        .await
        .expect("the sandbox must serve the call after a cancelled one");
    assert!(
        next.output["output"]
            .as_str()
            .is_some_and(|output| output.contains("after-cancel")),
        "the next call must receive its own result, got: {}",
        next.output
    );
}

#[tokio::test]
async fn bubblewrap_sandbox_real_binary_stops_abandoned_shell_command() {
    let name = "bubblewrap_sandbox_real_binary_stops_abandoned_shell_command";
    if !bubblewrap_check_available(name) {
        return;
    }

    let mut env_guard = ProcessEnvGuard::new();
    let temp_dir = tempfile::tempdir().unwrap();
    let mut sandbox = BubblewrapSandbox::new(test_config(&temp_dir), test_llm_handler());
    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    sandbox.spawn_agent().await.unwrap();

    assert_abandoned_shell_command_stops(&sandbox, temp_dir.path()).await;

    sandbox.shutdown().await.unwrap();
    eprintln!("bubblewrap check completed: {name}");
}

#[tokio::test]
async fn fallback_sandbox_real_binary_stops_abandoned_shell_command() {
    let mut env_guard = ProcessEnvGuard::new();
    let temp_dir = tempfile::tempdir().unwrap();
    let mut sandbox = FallbackSandbox::new(test_config(&temp_dir), test_llm_handler());
    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    sandbox.spawn_agent().await.unwrap();

    assert_abandoned_shell_command_stops(&sandbox, temp_dir.path()).await;

    sandbox.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_file_mutations_in_sandbox_child_refuse_without_attachment_authority() {
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    for bubblewrap in [false, true] {
        let name = "native_file_mutations_in_sandbox_child_refuse_without_attachment_authority";
        if bubblewrap && !bubblewrap_check_available(name) {
            continue;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("existing.txt"), "original").unwrap();
        // The working directory is deliberately writable at the OS boundary:
        // only the tool's missing attachment authority can refuse this write.
        let mut sandbox: Box<dyn Sandbox> = if bubblewrap {
            Box::new(BubblewrapSandbox::new(
                test_config(&root),
                test_llm_handler(),
            ))
        } else {
            Box::new(FallbackSandbox::new(test_config(&root), test_llm_handler()))
        };
        sandbox.spawn_agent().await.unwrap();
        let write = sandbox
            .execute_tool(
                "file_write",
                json!({"path": "nested/probe.txt", "content": "bad"}),
            )
            .await
            .unwrap();
        let edit = sandbox.execute_tool("file_edit", json!({"changes": [
            {"path": "existing.txt", "edits": [{"type": "replace", "old_text": "original", "new_text": "bad"}]}
        ]})).await.unwrap();
        // Shut the child down before asserting so a failing regression also
        // releases its process and socket resources.
        sandbox.shutdown().await.unwrap();
        for result in [write, edit] {
            assert!(
                result
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("cannot establish workspace write access")),
                "sandbox child without attachment authority must refuse mutations: {result:?}"
            );
        }
        assert!(!root.path().join("nested").exists());
        assert_eq!(
            std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
            "original"
        );
        if bubblewrap {
            eprintln!("bubblewrap check completed: {name}");
        }
        eprintln!(
            "native file mutation refusal passed: {}",
            if bubblewrap { "bubblewrap" } else { "fallback" }
        );
    }
}

// Probe the same namespace boundary the real child needs. A present but
// unusable executable is a named fallback, not evidence of sandbox execution.
fn bubblewrap_check_available(name: &str) -> bool {
    let available = find_bwrap()
        .map_err(|error| error.to_string())
        .and_then(|path| {
            std::process::Command::new(path)
                .args(["--unshare-all", "--ro-bind", "/", "/", "--", "/bin/true"])
                .output()
                .map_err(|error| error.to_string())
        })
        .and_then(|output| {
            if output.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            }
        });
    match available {
        Ok(()) => {
            eprintln!("bubblewrap check executing: {name}");
            true
        }
        Err(reason) => {
            eprintln!("bubblewrap check fallback: {name}: {reason}");
            false
        }
    }
}

#[test]
fn bubblewrap_disabled_reports_each_named_fallback() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let bwrap = fixture.path().join("bwrap");
    std::fs::write(
        &bwrap,
        "#!/bin/sh\necho disabled-for-reporting-test >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(fixture.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    for name in [
        "bubblewrap_sandbox_real_binary_runs_git_and_standard_device_io_through_shell",
        "bubblewrap_sandbox_real_binary_stops_abandoned_shell_command",
        "native_file_mutations_in_sandbox_child_refuse_without_attachment_authority",
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("sandbox_entrypoint_integration::{name}"),
                "--nocapture",
            ])
            .env("PATH", &path)
            .output()
            .unwrap();
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!("{log}");
        assert!(output.status.success(), "child test failed: {log}");
        assert!(
            log.contains(&format!("bubblewrap check fallback: {name}:")),
            "missing named fallback: {log}"
        );
        assert!(
            log.contains("disabled-for-reporting-test"),
            "missing fallback cause: {log}"
        );
        assert!(
            !log.contains("bubblewrap check completed:"),
            "disabled check claimed completion: {log}"
        );
    }
}

#[tokio::test]
async fn regression_read_only_workspace_shell_returns_output_and_readable_runtime_artifact() {
    let mut env_guard = ProcessEnvGuard::new();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("reference"), "keep").unwrap();
    let config = SandboxConfig::builder()
        .working_directory(workspace.path())
        .read_only_path(workspace.path())
        .write_path(runtime.path())
        .tool_output_directory(runtime.path())
        .build()
        .unwrap();
    env_guard.set_var("SWAGE_SANDBOX_AGENT_BINARY", env!("CARGO_BIN_EXE_swage"));
    let mut sandbox = BubblewrapSandbox::new(config, test_llm_handler());
    sandbox.spawn_agent().await.unwrap();
    let result = sandbox
        .execute_tool("shell", json!({"command":"ls -la; seq 1 30000"}))
        .await;
    let read = match &result {
        Ok(result) if result.error.is_none() => Some(
            sandbox
                .execute_tool(
                    "file_read",
                    result.output["stdout_follow_up"]["arguments"].clone(),
                )
                .await,
        ),
        _ => None,
    };
    sandbox.shutdown().await.unwrap();
    let result = result.unwrap();
    assert!(
        result.error.is_none(),
        "read-only workspace lost shell output: {result:?}"
    );
    assert_eq!(result.output["exit_code"], 0);
    let artifact = std::path::Path::new(result.output["stdout_artifact_path"].as_str().unwrap());
    assert!(artifact.starts_with(runtime.path()), "{result:?}");
    assert!(
        std::fs::read_to_string(artifact)
            .unwrap()
            .ends_with("30000\n")
    );
    let read = read.unwrap().unwrap();
    assert!(
        read.error.is_none(),
        "runtime artefact was not readable: {read:?}"
    );
    assert!(
        read.output["output"]
            .as_str()
            .unwrap()
            .contains("reference")
    );
    let entries: Vec<_> = std::fs::read_dir(workspace.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("reference")]);
}
