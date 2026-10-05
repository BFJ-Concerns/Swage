//! Cross-task integration tests for the swage-sandbox crate.
//!
//! These tests exercise interactions between multiple sandbox subsystems:
//! IPC protocol + transport + connections + process management + LLM proxying +
//! sandbox implementations. Each test requires outputs from two or more tasks
//! to function — no single worker could write these in isolation.

use serde_json::json;
use swage_core::{ChatMessage, CompletionRequest, CompletionResponse, FinishReason, Role};
use swage_sandbox::ipc::agent::AgentConnection;
use swage_sandbox::ipc::host::HostConnection;
use swage_sandbox::ipc::message::IpcMessage;
use swage_sandbox::ipc::transport::IpcTransport;
use swage_sandbox::llm_handler::HostLlmHandler;
use swage_sandbox::{IpcCompletionProvider, SandboxConfig};

// ---------------------------------------------------------------------------
// Test 1: IPC protocol roundtrip over actual Unix domain socket
// Cross-task: message types [0k1z] + transport [yfh2] + host conn [zo0l] + agent conn [f6u5]
// Journey: host dispatches tool call to agent, agent returns result;
//          agent sends LLM request to host, host returns response.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn ipc_roundtrip_tool_call_and_llm_request() {
    // Create a connected Unix socket pair — real socket, not in-memory.
    let (host_stream, agent_stream) = tokio::net::UnixStream::pair().unwrap();

    let config = SandboxConfig::builder()
        .working_directory(std::env::temp_dir())
        .build()
        .unwrap();

    let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
    let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));

    // --- Tool call roundtrip (host → agent → host) ---

    // Host sends a tool call request in a separate task so we can receive on the agent side.
    let host_handle = tokio::spawn(async move {
        host.send_tool_call("echo", json!({"text": "hello sandbox"}))
            .await
    });

    // Agent receives the tool call request.
    let msg = agent.receive_message().await.unwrap();
    match msg {
        IpcMessage::ToolCallRequest {
            request_id,
            tool_name,
            parameters,
        } => {
            assert_eq!(tool_name, "echo");
            assert_eq!(parameters, json!({"text": "hello sandbox"}));

            // Agent sends the result back.
            agent
                .send_tool_result(request_id, json!("hello sandbox"), 42, None)
                .await
                .unwrap();
        }
        other => panic!("expected ToolCallRequest, got {other:?}"),
    }

    // Host receives the tool call result.
    let result = host_handle.await.unwrap().unwrap();
    assert_eq!(result.output, json!("hello sandbox"));
    assert_eq!(result.duration_ms, 42);
    assert!(result.error.is_none());
}

// ---------------------------------------------------------------------------
// Test 2: LLM proxy end-to-end through IPC
// Cross-task: IPC provider [q1lq] + LLM handler [cv6y] + transport [yfh2]
//           + connections [zo0l, f6u5] + messages [0k1z]
// Journey: sandboxed agent requests LLM completion → IPC → host handler
//          dispatches to mock provider → response flows back with usage.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn llm_proxy_completion_through_ipc() {
    let (host_stream, agent_stream) = tokio::net::UnixStream::pair().unwrap();

    let config = SandboxConfig::builder()
        .working_directory(std::env::temp_dir())
        .build()
        .unwrap();

    let mut host_conn = HostConnection::new(IpcTransport::new(host_stream), &config);
    let agent_conn = AgentConnection::new(IpcTransport::new(agent_stream));

    // Mock provider: returns a fixed response for any completion request.
    let llm_handler = HostLlmHandler::new(|_req: CompletionRequest| async {
        Ok(CompletionResponse {
            content: "The answer is 42.".to_string(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cost: Some(0.001),
        })
    });

    // Agent side: use IpcCompletionProvider to make a completion request.
    let agent_handle = tokio::spawn(async move {
        let provider = IpcCompletionProvider::new(agent_conn);
        provider
            .complete(CompletionRequest {
                effort: None,
                messages: vec![ChatMessage::new(Role::User, "What is the meaning of life?")],
                model: None,
                max_tokens: Some(100),
                temperature: None,
                stop_sequences: None,
                metadata: None,
                tool_definitions: vec![],
            })
            .await
    });

    // Host side: receive the LLM request and handle it.
    let llm_request = host_conn.receive_llm_request().await.unwrap();
    let llm_response = llm_handler.handle_request(llm_request).await.unwrap();
    host_conn.send_llm_response(llm_response).await.unwrap();

    // Verify the agent received the response correctly.
    let response = agent_handle.await.unwrap().unwrap();
    assert_eq!(response.content, "The answer is 42.");
    assert_eq!(response.input_tokens, 10);
    assert_eq!(response.output_tokens, 5);
    assert_eq!(response.finish_reason, FinishReason::Stop);
}

// ---------------------------------------------------------------------------
// Test 3: bubblewrap startup with the host socket under /tmp
// Journey: the gateway's runtime directory lies under /tmp, or is too long
//          for a Unix socket path so the socket falls back under /tmp; bwrap
//          overlays /tmp with a tmpfs, yet the agent must still connect.
// ---------------------------------------------------------------------------

/// A stand-in sandbox agent: connects to the host socket, reports `Ready`,
/// and acknowledges `Shutdown`. Connecting is the behaviour under test.
const FAKE_AGENT: &str = r#"#!/usr/bin/env python3
import json, os, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["SWAGE_IPC_SOCKET"])
s.sendall(b'{"kind":"ready"}\n')
for line in s.makefile():
    if json.loads(line).get("kind") == "shutdown":
        s.sendall(b'{"kind":"shutdown_ack"}\n')
        sys.exit(0)
"#;

#[tokio::test]
async fn bubblewrap_agent_connects_when_socket_lies_under_tmp() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use swage_sandbox::{BubblewrapSandbox, Sandbox, find_bwrap};

    if find_bwrap().is_err() {
        eprintln!("skipping: bwrap is not installed");
        return;
    }

    let work = tempfile::tempdir().unwrap();
    let agent = work.path().join("fake-agent");
    std::fs::write(&agent, FAKE_AGENT).unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();

    let tmp_root = tempfile::tempdir_in("/tmp").unwrap();
    let long_root = work.path().join("r".repeat(120));
    let layouts = [
        ("runtime directory under /tmp", tmp_root.path().join("run")),
        ("over-long runtime directory", long_root),
    ];

    // Both layouts run in this one test: they share process-wide env vars.
    unsafe { std::env::set_var("SWAGE_SANDBOX_AGENT_BINARY", &agent) };
    for (layout, runtime_dir) in layouts {
        std::fs::create_dir_all(&runtime_dir).unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir) };

        let config = SandboxConfig::builder()
            .working_directory(work.path())
            .tool_call_timeout(Duration::from_secs(10))
            .shutdown_grace_period(Duration::from_secs(2))
            .build()
            .unwrap();
        let mut sandbox = BubblewrapSandbox::new(
            config,
            HostLlmHandler::new(|_req: CompletionRequest| async {
                Err(swage_core::SandboxError::IpcError {
                    message: "no LLM in this test".into(),
                })
            }),
        );
        let started = sandbox.spawn_agent().await;
        assert!(
            started.is_ok(),
            "{layout}: sandbox agent must connect to the host socket: {started:?}"
        );
        sandbox.shutdown().await.unwrap();
    }
}
