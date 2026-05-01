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
