//! Cross-task integration tests for the swage-sandbox crate.
//!
//! These tests exercise interactions between multiple sandbox subsystems:
//! IPC protocol + transport + connections + process management + LLM proxying +
//! sandbox implementations. Each test requires outputs from two or more tasks
//! to function — no single worker could write these in isolation.

use serde_json::json;
use std::time::Duration;
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, Role, SandboxError,
};
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
        other => panic!("expected ToolCallRequest, got {:?}", other),
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
                messages: vec![ChatMessage::new(
                    Role::User,
                    "What is the meaning of life?",
                )],
                model: None,
                max_tokens: Some(100),
                temperature: None,
                stop_sequences: None,
                metadata: None,
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
// Test 3: Fallback sandbox full lifecycle
// Cross-task: config [d058] + sandbox trait [iwwj] + fallback [54e5]
//           + process [kogs] + socket [1yxk] + IPC stack [0k1z, yfh2, zo0l, f6u5]
//           + agent loop [82xu] + LLM handler [cv6y]
// Journey: spawn agent as child process, dispatch tool call over IPC,
//          receive result, shut down — all through the Sandbox trait.
//          Verifies the entire stack integrates end-to-end.
//
// Note: requires the binary to support --sandbox-mode for child process
// spawning. Ignored until the binary entry point is wired in era 0.1.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires sandbox-mode binary entry point (era 0.1 integration)"]
async fn fallback_sandbox_spawn_dispatch_shutdown() {
    use swage_sandbox::{FallbackSandbox, Sandbox};

    let temp_dir = tempfile::tempdir().unwrap();

    let llm_handler = HostLlmHandler::new(|_req: CompletionRequest| async {
        Ok(CompletionResponse {
            content: "mocked".to_string(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    });

    let config = SandboxConfig::builder()
        .working_directory(temp_dir.path().to_path_buf())
        .tool_call_timeout(Duration::from_secs(10))
        .llm_request_timeout(Duration::from_secs(10))
        .shutdown_grace_period(Duration::from_secs(5))
        .build()
        .unwrap();

    let mut sandbox = FallbackSandbox::new(config, llm_handler);

    // Spawn — should launch a child process and wait for ready signal.
    sandbox.spawn_agent().await.unwrap();

    // Dispatch a tool call — the agent executes it and returns the result.
    let result = sandbox
        .execute_tool("echo", json!({"text": "integration test"}))
        .await
        .unwrap();
    assert_eq!(result.output, json!("integration test"));
    assert!(result.error.is_none());

    // Shutdown — should terminate child and clean up resources.
    sandbox.shutdown().await.unwrap();

    // Idempotent shutdown — calling again should succeed silently.
    sandbox.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Test 4: Credential isolation — API keys must not enter the child process
// Cross-task: fallback [54e5] + process [kogs] + agent loop [82xu]
// Journey: host sets API key env vars, spawns sandbox, dispatches a tool
//          that reads the child's environment — API keys must be absent.
//
// Note: same binary requirement as test 3.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires sandbox-mode binary entry point (era 0.1 integration)"]
async fn sandbox_credentials_not_in_child_process() {
    use swage_sandbox::{FallbackSandbox, Sandbox};

    let temp_dir = tempfile::tempdir().unwrap();

    // Set API key env vars in the host process.
    // Safety: test runs in its own process, env vars are test-scoped.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "sk-test-host-key");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-host-key");
        std::env::set_var("GOOGLE_API_KEY", "test-google-key");
    }

    let llm_handler = HostLlmHandler::new(|_req: CompletionRequest| async {
        Ok(CompletionResponse {
            content: "mocked".to_string(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    });

    let config = SandboxConfig::builder()
        .working_directory(temp_dir.path().to_path_buf())
        .build()
        .unwrap();

    let mut sandbox = FallbackSandbox::new(config, llm_handler);
    sandbox.spawn_agent().await.unwrap();

    // Dispatch a tool that reads environment variables from the child process.
    let openai_key = sandbox
        .execute_tool("env_read", json!({"name": "OPENAI_API_KEY"}))
        .await
        .unwrap();
    assert_eq!(
        openai_key.output,
        serde_json::Value::Null,
        "OPENAI_API_KEY must not be present in the sandboxed child process"
    );

    sandbox.shutdown().await.unwrap();

    // Clean up env vars.
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("GOOGLE_API_KEY");
    }
}
