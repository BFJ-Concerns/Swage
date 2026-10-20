//! Host-side IPC connection.
//!
//! Manages message exchange between the host process and a sandboxed agent.
//! Handles request-response correlation, timeout enforcement, and the
//! lifecycle handshake (wait for ready, send shutdown).

use serde_json::Value;
use swage_core::SandboxError;
use uuid::Uuid;

use crate::config::SandboxConfig;
use crate::ipc::message::{ErrorCategory, IpcMessage, LlmRequest, LlmResponse, ToolCallResult};
use crate::ipc::transport::IpcTransport;

// ---------------------------------------------------------------------------
// HostConnection
// ---------------------------------------------------------------------------

/// Host-side view of the IPC channel to a sandboxed agent process.
///
/// Wraps an `IpcTransport` and adds:
/// - lifecycle handshakes (`wait_for_ready`, `send_shutdown`)
/// - request-response correlation with timeout enforcement
/// - `ErrorMessage` → `SandboxError` conversion
pub struct HostConnection {
    transport: IpcTransport,
    config: SandboxConfig,
}

impl HostConnection {
    /// Create a new host connection over the given transport.
    pub fn new(transport: IpcTransport, config: &SandboxConfig) -> Self {
        Self {
            transport,
            config: config.clone(),
        }
    }

    /// Block until the agent sends a `Ready` signal.
    ///
    /// Times out after `config.tool_call_timeout`, which represents the
    /// host's patience for agent-side operations (startup included).
    pub async fn wait_for_ready(&mut self) -> Result<(), SandboxError> {
        let msg = self
            .transport
            .receive_timeout(self.config.tool_call_timeout)
            .await?;

        match msg {
            IpcMessage::Ready => Ok(()),
            IpcMessage::ErrorMessage { description, .. } => Err(SandboxError::IpcError {
                message: format!("agent reported error during startup: {description}"),
            }),
            other => Err(SandboxError::IpcError {
                message: format!("expected Ready signal, got unexpected message: {other:?}"),
            }),
        }
    }

    /// Dispatch a tool call to the agent and wait for the result.
    ///
    /// Generates a unique `request_id`, sends `ToolCallRequest`, and waits
    /// for a matching `ToolCallResult` within `config.tool_call_timeout`.
    /// An `ErrorMessage` response is converted to `SandboxError`.
    pub async fn send_tool_call(
        &mut self,
        tool_name: &str,
        params: Value,
    ) -> Result<ToolCallResult, SandboxError> {
        let request_id = Uuid::new_v4().to_string();

        self.transport
            .send(&IpcMessage::ToolCallRequest {
                request_id: request_id.clone(),
                tool_name: tool_name.into(),
                parameters: params,
            })
            .await?;

        let deadline = tokio::time::Instant::now() + self.config.tool_call_timeout;
        loop {
            let response = match self
                .transport
                .receive_timeout(deadline.saturating_duration_since(tokio::time::Instant::now()))
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    self.transport.abandon_tool_call(request_id.clone());
                    self.transport
                        .send(&IpcMessage::ToolCallCancel { request_id })
                        .await?;
                    return Err(error);
                }
            };

            match response {
                IpcMessage::ToolCallResult {
                    request_id: resp_id,
                    output,
                    duration_ms,
                    error,
                } => {
                    if resp_id != request_id {
                        if self.transport.take_abandoned_tool_call(&resp_id) {
                            continue;
                        }
                        return Err(SandboxError::IpcError {
                            message: format!(
                                "request ID mismatch: sent {request_id}, received {resp_id}"
                            ),
                        });
                    }
                    return Ok(ToolCallResult {
                        output,
                        duration_ms,
                        error,
                    });
                }
                IpcMessage::ErrorMessage { description, .. } => {
                    return Err(SandboxError::IpcError {
                        message: description,
                    });
                }
                other => {
                    return Err(SandboxError::IpcError {
                        message: format!("unexpected response to ToolCallRequest: {other:?}"),
                    });
                }
            }
        }
    }

    /// Receive the next `LlmRequest` from the agent.
    ///
    /// The agent sends this when it needs an LLM completion proxied through
    /// the host (credentials never enter the sandbox). The returned
    /// `LlmRequest` includes the `request_id` for correlation.
    pub async fn receive_llm_request(&mut self) -> Result<LlmRequest, SandboxError> {
        let msg = self
            .transport
            .receive_timeout(self.config.llm_request_timeout)
            .await?;

        match msg {
            IpcMessage::LlmRequest {
                request_id,
                messages,
                model,
                max_tokens,
                temperature,
                tool_definitions,
            } => Ok(LlmRequest {
                request_id,
                messages,
                model,
                max_tokens,
                temperature,
                tool_definitions,
            }),
            IpcMessage::ErrorMessage { description, .. } => Err(SandboxError::IpcError {
                message: description,
            }),
            other => Err(SandboxError::IpcError {
                message: format!("expected LlmRequest, got: {other:?}"),
            }),
        }
    }

    /// Send an `LlmResponse` back to the agent.
    ///
    /// The `response.request_id` must match the `request_id` from the
    /// corresponding `LlmRequest` for the agent to correlate them.
    pub async fn send_llm_response(&mut self, response: LlmResponse) -> Result<(), SandboxError> {
        self.transport
            .send(&IpcMessage::LlmResponse {
                request_id: response.request_id,
                content: response.content,
                token_usage: response.token_usage,
                finish_reason: response.finish_reason,
                tool_calls: response.tool_calls,
            })
            .await
    }

    /// Send a `ToolCallRequest` and return the generated `request_id`.
    ///
    /// Lower-level variant of `send_tool_call` for callers that need to
    /// implement their own response loop (e.g. to handle interleaved LLM
    /// proxy requests). Use with `receive_any` to build dispatch loops.
    pub async fn begin_tool_call(
        &mut self,
        tool_name: &str,
        params: Value,
    ) -> Result<String, SandboxError> {
        let request_id = Uuid::new_v4().to_string();
        self.transport
            .send(&IpcMessage::ToolCallRequest {
                request_id: request_id.clone(),
                tool_name: tool_name.into(),
                parameters: params,
            })
            .await?;
        Ok(request_id)
    }

    /// Receive the next message from the agent, using `tool_call_timeout`.
    ///
    /// Intended for sandbox implementations that need to dispatch interleaved
    /// `LlmRequest` messages while waiting for a `ToolCallResult`. Called in
    /// a loop after `begin_tool_call` to drive the dispatch protocol.
    pub async fn receive_any(&mut self) -> Result<IpcMessage, SandboxError> {
        self.transport
            .receive_timeout(self.config.tool_call_timeout)
            .await
    }

    /// Send an `ErrorMessage` to the agent.
    ///
    /// Used when a host-side error (e.g. LLM provider failure) must be
    /// surfaced to the agent for a specific in-flight request. The agent
    /// can then return an error from `send_llm_request` to its caller.
    pub async fn send_error_response(
        &mut self,
        request_id: Option<String>,
        category: ErrorCategory,
        description: String,
    ) -> Result<(), SandboxError> {
        self.transport
            .send(&IpcMessage::ErrorMessage {
                request_id,
                category,
                description,
            })
            .await
    }

    /// Send a `Shutdown` signal and wait for the agent to acknowledge.
    ///
    /// The agent is expected to send `ShutdownAck` before terminating.
    /// Times out after `config.shutdown_grace_period`.
    pub async fn send_shutdown(&mut self) -> Result<(), SandboxError> {
        self.transport.send(&IpcMessage::Shutdown).await?;

        let ack = self
            .transport
            .receive_timeout(self.config.shutdown_grace_period)
            .await?;

        match ack {
            IpcMessage::ShutdownAck => Ok(()),
            IpcMessage::ErrorMessage { description, .. } => Err(SandboxError::IpcError {
                message: format!("agent error during shutdown: {description}"),
            }),
            other => Err(SandboxError::IpcError {
                message: format!("expected ShutdownAck, got: {other:?}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use swage_core::SandboxError;
    use tokio::net::UnixStream;

    use super::HostConnection;
    use crate::config::SandboxConfig;
    use crate::ipc::message::{ErrorCategory, IpcMessage, LlmRequest, LlmResponse, TokenUsage};
    use crate::ipc::transport::IpcTransport;

    fn default_config() -> SandboxConfig {
        SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            .tool_call_timeout(Duration::from_secs(5))
            .shutdown_grace_period(Duration::from_secs(2))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    // --- wait_for_ready: receives Ready signal ---

    #[tokio::test]
    async fn wait_for_ready_succeeds_on_ready_signal() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        agent_transport.send(&IpcMessage::Ready).await.unwrap();

        host.wait_for_ready().await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_ready_times_out_on_no_signal() {
        // Keep agent end alive but silent.
        let (host_stream, _agent) = UnixStream::pair().unwrap();
        let config = SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            // Very short startup timeout so the test runs quickly.
            .tool_call_timeout(Duration::from_millis(80))
            .shutdown_grace_period(Duration::from_secs(2))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);

        let result = host.wait_for_ready().await;
        assert!(result.is_err(), "expected timeout error");
    }

    // --- send_tool_call: request-response correlation ---

    #[tokio::test]
    async fn send_tool_call_returns_matching_result() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        let host_handle =
            tokio::spawn(
                async move { host.send_tool_call("echo", json!({"text": "hello"})).await },
            );

        // Agent receives the request and sends a result.
        let msg = agent_transport.receive().await.unwrap();
        let request_id = match &msg {
            IpcMessage::ToolCallRequest { request_id, .. } => request_id.clone(),
            other => panic!("expected ToolCallRequest, got {other:?}"),
        };
        agent_transport
            .send(&IpcMessage::ToolCallResult {
                request_id,
                output: json!("hello"),
                duration_ms: 12,
                error: None,
            })
            .await
            .unwrap();

        let result = host_handle.await.unwrap().unwrap();
        assert_eq!(result.output, json!("hello"));
        assert_eq!(result.duration_ms, 12);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn send_tool_call_propagates_error_in_result() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        let host_handle =
            tokio::spawn(async move { host.send_tool_call("fail_tool", json!({})).await });

        let msg = agent_transport.receive().await.unwrap();
        let request_id = match &msg {
            IpcMessage::ToolCallRequest { request_id, .. } => request_id.clone(),
            other => panic!("expected ToolCallRequest, got {other:?}"),
        };
        // Agent returns an error result (tool ran but failed).
        agent_transport
            .send(&IpcMessage::ToolCallResult {
                request_id,
                output: serde_json::Value::Null,
                duration_ms: 3,
                error: Some("tool not found".into()),
            })
            .await
            .unwrap();

        let result = host_handle.await.unwrap().unwrap();
        assert_eq!(result.error.unwrap(), "tool not found");
    }

    #[tokio::test]
    async fn send_tool_call_converts_error_message_to_sandbox_error() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        let host_handle =
            tokio::spawn(async move { host.send_tool_call("badtool", json!({})).await });

        let msg = agent_transport.receive().await.unwrap();
        let request_id = match &msg {
            IpcMessage::ToolCallRequest { request_id, .. } => Some(request_id.clone()),
            other => panic!("expected ToolCallRequest, got {other:?}"),
        };
        // Agent responds with an IPC-level error.
        agent_transport
            .send(&IpcMessage::ErrorMessage {
                request_id,
                category: ErrorCategory::ToolExecution,
                description: "execution context unavailable".into(),
            })
            .await
            .unwrap();

        let result = host_handle.await.unwrap();
        assert!(result.is_err(), "ErrorMessage should become SandboxError");
        match result.unwrap_err() {
            SandboxError::IpcError { message } => {
                assert!(message.contains("execution context unavailable"));
            }
            other => panic!("expected IpcError, got {other:?}"),
        }
    }

    // --- receive_llm_request / send_llm_response ---

    #[tokio::test]
    async fn llm_request_receive_and_response_send() {
        use swage_core::FinishReason;

        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        // Agent sends an LlmRequest.
        agent_transport
            .send(&IpcMessage::LlmRequest {
                request_id: "llm-42".into(),
                messages: vec![],
                model: Some("claude-3".into()),
                max_tokens: Some(256),
                temperature: None,
                tool_definitions: None,
            })
            .await
            .unwrap();

        // Host receives it.
        let req: LlmRequest = host.receive_llm_request().await.unwrap();
        assert_eq!(req.request_id, "llm-42");
        assert_eq!(req.max_tokens, Some(256));

        // Host sends a response, preserving the request_id.
        host.send_llm_response(LlmResponse {
            request_id: req.request_id.clone(),
            content: "Answer here.".into(),
            token_usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        })
        .await
        .unwrap();

        // Agent receives the response.
        let response_msg = agent_transport.receive().await.unwrap();
        match response_msg {
            IpcMessage::LlmResponse {
                request_id,
                content,
                ..
            } => {
                assert_eq!(request_id, "llm-42");
                assert_eq!(content, "Answer here.");
            }
            other => panic!("expected LlmResponse, got {other:?}"),
        }
    }

    // --- send_shutdown: lifecycle handshake ---

    #[tokio::test]
    async fn send_shutdown_waits_for_ack() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        let host_handle = tokio::spawn(async move { host.send_shutdown().await });

        // Agent receives Shutdown and acknowledges.
        let msg = agent_transport.receive().await.unwrap();
        assert!(matches!(msg, IpcMessage::Shutdown));
        agent_transport
            .send(&IpcMessage::ShutdownAck)
            .await
            .unwrap();

        host_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn send_shutdown_times_out_when_no_ack() {
        let (host_stream, _agent) = UnixStream::pair().unwrap();
        let config = SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            .tool_call_timeout(Duration::from_secs(5))
            .shutdown_grace_period(Duration::from_millis(80))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);

        let result = host.send_shutdown().await;
        assert!(
            result.is_err(),
            "expected timeout error when no ShutdownAck"
        );
    }

    // --- begin_tool_call: sends request, returns request_id ----------------

    #[tokio::test]
    async fn begin_tool_call_sends_request_and_returns_request_id() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        let returned_id = host
            .begin_tool_call("bash", serde_json::json!({"cmd": "ls"}))
            .await
            .unwrap();

        assert!(!returned_id.is_empty(), "request_id should be non-empty");

        let msg = agent_transport.receive().await.unwrap();
        match msg {
            IpcMessage::ToolCallRequest {
                request_id,
                tool_name,
                parameters,
            } => {
                assert_eq!(
                    request_id, returned_id,
                    "agent request_id must match returned id"
                );
                assert_eq!(tool_name, "bash");
                assert_eq!(parameters["cmd"], "ls");
            }
            other => panic!("expected ToolCallRequest, got {other:?}"),
        }
    }

    // --- receive_any: receives the next available message -------------------

    #[tokio::test]
    async fn receive_any_returns_next_message() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        // Agent sends a message before the host calls receive_any.
        agent_transport.send(&IpcMessage::Ready).await.unwrap();

        let msg = host.receive_any().await.unwrap();
        assert!(matches!(msg, IpcMessage::Ready));
    }

    #[tokio::test]
    async fn receive_any_times_out_on_idle_socket() {
        let (host_stream, _agent) = UnixStream::pair().unwrap();
        let config = SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            .tool_call_timeout(Duration::from_millis(80)) // very short for test speed
            .shutdown_grace_period(Duration::from_secs(2))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);

        let result = host.receive_any().await;
        assert!(
            result.is_err(),
            "receive_any should time out on idle socket"
        );
    }

    // --- send_error_response: sends ErrorMessage to agent -------------------

    #[tokio::test]
    async fn send_error_response_sends_error_message() {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let config = default_config();
        let mut host = HostConnection::new(IpcTransport::new(host_stream), &config);
        let mut agent_transport = IpcTransport::new(agent_stream);

        host.send_error_response(
            Some("req-77".into()),
            ErrorCategory::Provider,
            "provider rate-limited".into(),
        )
        .await
        .unwrap();

        let msg = agent_transport.receive().await.unwrap();
        match msg {
            IpcMessage::ErrorMessage {
                request_id,
                category,
                description,
            } => {
                assert_eq!(request_id, Some("req-77".into()));
                assert_eq!(category, ErrorCategory::Provider);
                assert!(description.contains("rate-limited"));
            }
            other => panic!("expected ErrorMessage, got {other:?}"),
        }
    }
}
