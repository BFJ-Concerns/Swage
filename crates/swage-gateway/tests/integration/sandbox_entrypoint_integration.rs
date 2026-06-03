#![allow(clippy::unwrap_used)]

use serde_json::json;
use swage_core::{CompletionRequest, CompletionResponse, FinishReason, SandboxError};
use swage_sandbox::llm_handler::HostLlmHandler;
use swage_sandbox::{FallbackSandbox, Sandbox, SandboxConfig};

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
        .tool_call_timeout(std::time::Duration::from_secs(10))
        .llm_request_timeout(std::time::Duration::from_secs(10))
        .shutdown_grace_period(std::time::Duration::from_secs(5))
        .build()
        .expect("sandbox config should build")
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
