//! Agent-side IPC connection.
//!
//! Runs inside the sandboxed child process. Receives tool call requests from
//! the host, sends results back, and proxies LLM completion requests through
//! the host.

use serde_json::Value;
use swage_core::{ChatMessage, FinishReason, SandboxError, ToolCall};
use uuid::Uuid;

use crate::ipc::message::{ErrorCategory, IpcMessage, TokenUsage};
use crate::ipc::transport::IpcTransport;

// ---------------------------------------------------------------------------
// Response type
// ---------------------------------------------------------------------------

/// Response data returned from an LLM completion request proxied through the
/// host. The `request_id` is consumed internally for correlation and not
/// exposed to the caller.
pub struct LlmIpcResponse {
    pub content: String,
    pub token_usage: TokenUsage,
    pub finish_reason: FinishReason,
    /// Tool calls requested by the model; absent for non-tool-use completions.
    pub tool_calls: Option<Vec<ToolCall>>,
}

// ---------------------------------------------------------------------------
// AgentConnection
// ---------------------------------------------------------------------------

/// Agent-side half of the host-agent IPC channel.
///
/// Wraps an `IpcTransport` and provides typed send/receive methods suited to
/// the agent's role: sending lifecycle signals and tool results toward the host,
/// and making LLM completion requests that the host proxies to the real provider.
pub struct AgentConnection {
    transport: IpcTransport,
}

impl AgentConnection {
    /// Wrap a transport in an `AgentConnection`.
    pub fn new(transport: IpcTransport) -> Self {
        Self { transport }
    }

    /// Send the `Ready` lifecycle signal.
    ///
    /// Called once after the agent process has initialised its tool registry
    /// and is ready to receive tool call requests from the host.
    pub async fn send_ready(&mut self) -> Result<(), SandboxError> {
        self.transport.send(&IpcMessage::Ready).await
    }

    /// Receive the next message from the host.
    ///
    /// The caller is responsible for dispatching on the variant. In normal
    /// operation the host sends `ToolCallRequest` or `Shutdown` signals;
    /// any other variant should be treated as a protocol error.
    pub async fn receive_message(&mut self) -> Result<IpcMessage, SandboxError> {
        self.transport.receive().await
    }

    /// Send the result of a tool invocation back to the host.
    ///
    /// `request_id` must match the `request_id` from the originating
    /// `ToolCallRequest` so the host can correlate the response.
    pub async fn send_tool_result(
        &mut self,
        request_id: String,
        output: Value,
        duration_ms: u64,
        error: Option<String>,
    ) -> Result<(), SandboxError> {
        self.transport.send(&IpcMessage::ToolCallResult {
            request_id,
            output,
            duration_ms,
            error,
        }).await
    }

    /// Proxy an LLM completion request through the host.
    ///
    /// Generates a `request_id`, sends an `IpcMessage::LlmRequest`, then blocks
    /// until the host returns a matching `IpcMessage::LlmResponse`. If the host
    /// returns an `ErrorMessage` for the same request, that error is surfaced as
    /// a `SandboxError::IpcError`.
    ///
    /// Credentials (API keys) never cross the sandbox boundary — the host
    /// executes the actual provider call.
    pub async fn send_llm_request(
        &mut self,
        messages: Vec<ChatMessage>,
        model: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        tool_definitions: Option<Vec<Value>>,
    ) -> Result<LlmIpcResponse, SandboxError> {
        let request_id = Uuid::new_v4().to_string();

        self.transport.send(&IpcMessage::LlmRequest {
            request_id: request_id.clone(),
            messages,
            model,
            max_tokens,
            temperature,
            tool_definitions,
        }).await?;

        // Await the response, correlating by request_id.
        // The IPC channel is sequential (one outstanding LLM request at a time),
        // but we still validate request_id to catch protocol violations.
        loop {
            let msg = self.transport.receive().await?;
            match msg {
                IpcMessage::LlmResponse {
                    request_id: ref resp_id,
                    ..
                } if resp_id == &request_id => {
                    // Destructure now that we've confirmed the id.
                    if let IpcMessage::LlmResponse { content, token_usage, finish_reason, tool_calls, .. } = msg {
                        return Ok(LlmIpcResponse { content, token_usage, finish_reason, tool_calls });
                    }
                    unreachable!()
                }
                IpcMessage::ErrorMessage { request_id: Some(ref err_req_id), ref description, .. }
                    if err_req_id == &request_id =>
                {
                    return Err(SandboxError::IpcError {
                        message: description.clone(),
                    });
                }
                // An LlmResponse for a different request_id is a protocol error.
                IpcMessage::LlmResponse { request_id: ref other_id, .. } => {
                    return Err(SandboxError::IpcError {
                        message: format!(
                            "received LlmResponse for unexpected request_id '{other_id}', expected '{request_id}'"
                        ),
                    });
                }
                // Any other message while awaiting an LlmResponse is unexpected.
                other => {
                    return Err(SandboxError::IpcError {
                        message: format!(
                            "unexpected message while awaiting LlmResponse: {:?}",
                            other
                        ),
                    });
                }
            }
        }
    }

    /// Acknowledge a `Shutdown` signal from the host.
    ///
    /// Must be sent before the agent process exits so the host knows the
    /// child has cleaned up and is safe to terminate.
    pub async fn send_shutdown_ack(&mut self) -> Result<(), SandboxError> {
        self.transport.send(&IpcMessage::ShutdownAck).await
    }

    /// Send an error notification to the host.
    ///
    /// Use when a tool execution or protocol error occurs that the host needs
    /// to know about. `request_id` should match the originating request when
    /// one is in flight; pass `None` for connection-level errors.
    pub async fn send_error(
        &mut self,
        request_id: Option<String>,
        category: ErrorCategory,
        description: String,
    ) -> Result<(), SandboxError> {
        self.transport.send(&IpcMessage::ErrorMessage {
            request_id,
            category,
            description,
        }).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;
    use swage_core::{ChatMessage, FinishReason, Role};
    use tokio::net::UnixStream;

    use super::AgentConnection;
    use crate::ipc::message::{ErrorCategory, IpcMessage, TokenUsage};
    use crate::ipc::transport::IpcTransport;

    // -- send_ready -----------------------------------------------------------

    #[tokio::test]
    async fn send_ready_sends_ready_signal() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        agent.send_ready().await.unwrap();

        let received = host_transport.receive().await.unwrap();
        assert!(matches!(received, IpcMessage::Ready));
    }

    // -- receive_message ------------------------------------------------------

    #[tokio::test]
    async fn receive_message_returns_tool_call_request() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        // Host sends a tool call request.
        host_transport.send(&IpcMessage::ToolCallRequest {
            request_id: "req-42".into(),
            tool_name: "bash".into(),
            parameters: json!({"command": "ls"}),
        }).await.unwrap();

        let msg = agent.receive_message().await.unwrap();
        match msg {
            IpcMessage::ToolCallRequest { request_id, tool_name, .. } => {
                assert_eq!(request_id, "req-42");
                assert_eq!(tool_name, "bash");
            }
            other => panic!("expected ToolCallRequest, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn receive_message_returns_shutdown() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        host_transport.send(&IpcMessage::Shutdown).await.unwrap();

        let msg = agent.receive_message().await.unwrap();
        assert!(matches!(msg, IpcMessage::Shutdown));
    }

    // -- send_tool_result -----------------------------------------------------

    #[tokio::test]
    async fn send_tool_result_success() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        agent.send_tool_result(
            "req-1".into(),
            json!({"lines": ["a", "b"]}),
            17,
            None,
        ).await.unwrap();

        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::ToolCallResult { request_id, output, duration_ms, error } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(output, json!({"lines": ["a", "b"]}));
                assert_eq!(duration_ms, 17);
                assert!(error.is_none());
            }
            other => panic!("expected ToolCallResult, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn send_tool_result_with_error() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        agent.send_tool_result(
            "req-2".into(),
            serde_json::Value::Null,
            3,
            Some("command not found".into()),
        ).await.unwrap();

        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::ToolCallResult { error, .. } => {
                assert_eq!(error, Some("command not found".into()));
            }
            other => panic!("expected ToolCallResult, got {:?}", other),
        }
    }

    // -- send_llm_request -----------------------------------------------------

    #[tokio::test]
    async fn send_llm_request_receives_matching_response() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        // Spawn host behaviour: receive LlmRequest, echo back LlmResponse
        // with the same request_id.
        let host_handle = tokio::spawn(async move {
            let msg = host_transport.receive().await.unwrap();
            match msg {
                IpcMessage::LlmRequest { request_id, .. } => {
                    host_transport.send(&IpcMessage::LlmResponse {
                        request_id,
                        content: "The answer is 42.".into(),
                        token_usage: TokenUsage {
                            prompt_tokens: 10,
                            completion_tokens: 5,
                            total_tokens: 15,
                        },
                        finish_reason: FinishReason::Stop,
                        tool_calls: None,
                    }).await.unwrap();
                }
                other => panic!("host expected LlmRequest, got {:?}", other),
            }
        });

        let response = agent.send_llm_request(
            vec![ChatMessage::new(Role::User, "What is 6 * 7?")],
            None,
            Some(128),
            None,
            None,
        ).await.unwrap();

        host_handle.await.unwrap();

        assert_eq!(response.content, "The answer is 42.");
        assert_eq!(response.token_usage.total_tokens, 15);
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert!(response.tool_calls.is_none());
    }

    #[tokio::test]
    async fn send_llm_request_sends_all_fields() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        let agent_handle = tokio::spawn(async move {
            agent.send_llm_request(
                vec![ChatMessage::new(Role::User, "hello")],
                Some("claude-3-opus".into()),
                Some(256),
                Some(0.7),
                None,
            ).await
        });

        // Verify the host receives all the fields correctly.
        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::LlmRequest { request_id, model, max_tokens, temperature, messages, .. } => {
                assert!(!request_id.is_empty(), "request_id should be auto-generated");
                assert_eq!(model, Some("claude-3-opus".into()));
                assert_eq!(max_tokens, Some(256));
                assert_eq!(temperature, Some(0.7));
                assert_eq!(messages.len(), 1);

                // Send back response to unblock the agent task.
                host_transport.send(&IpcMessage::LlmResponse {
                    request_id,
                    content: "ok".into(),
                    token_usage: TokenUsage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                    },
                    finish_reason: FinishReason::Stop,
                    tool_calls: None,
                }).await.unwrap();
            }
            other => panic!("expected LlmRequest, got {:?}", other),
        }

        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn send_llm_request_returns_error_on_error_message() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        let agent_handle = tokio::spawn(async move {
            agent.send_llm_request(vec![], None, None, None, None).await
        });

        // Host responds with an error for the same request.
        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::LlmRequest { request_id, .. } => {
                host_transport.send(&IpcMessage::ErrorMessage {
                    request_id: Some(request_id),
                    category: ErrorCategory::Provider,
                    description: "provider unavailable".into(),
                }).await.unwrap();
            }
            other => panic!("host expected LlmRequest, got {:?}", other),
        }

        let result = agent_handle.await.unwrap();
        assert!(result.is_err(), "should return error when host sends ErrorMessage");
    }

    // -- send_shutdown_ack ----------------------------------------------------

    #[tokio::test]
    async fn send_shutdown_ack_sends_shutdown_ack() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        agent.send_shutdown_ack().await.unwrap();

        let received = host_transport.receive().await.unwrap();
        assert!(matches!(received, IpcMessage::ShutdownAck));
    }

    // -- send_error -----------------------------------------------------------

    #[tokio::test]
    async fn send_error_sends_error_message() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        agent.send_error(
            Some("req-99".into()),
            ErrorCategory::ToolExecution,
            "tool panicked".into(),
        ).await.unwrap();

        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::ErrorMessage { request_id, category, description } => {
                assert_eq!(request_id, Some("req-99".into()));
                assert_eq!(category, ErrorCategory::ToolExecution);
                assert_eq!(description, "tool panicked");
            }
            other => panic!("expected ErrorMessage, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn send_error_without_request_id() {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        let mut agent = AgentConnection::new(IpcTransport::new(agent_stream));
        let mut host_transport = IpcTransport::new(host_stream);

        agent.send_error(None, ErrorCategory::Connection, "socket error".into()).await.unwrap();

        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::ErrorMessage { request_id, .. } => {
                assert!(request_id.is_none());
            }
            other => panic!("expected ErrorMessage, got {:?}", other),
        }
    }
}
