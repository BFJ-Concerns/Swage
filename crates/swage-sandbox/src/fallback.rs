//! Fallback sandbox — child process with IPC but no isolation.
//!
//! On platforms without bubblewrap (macOS, Windows, or Linux without bwrap),
//! agents run as regular child processes. IPC proxying still provides
//! credential isolation — credentials remain in the host process.

use async_trait::async_trait;
use serde_json::Value;
use swage_core::SandboxError;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::config::SandboxConfig;
use crate::ipc::message::{ErrorCategory, IpcMessage, LlmRequest as IpcLlmRequest};
use crate::ipc::transport::IpcTransport;
use crate::llm_handler::HostLlmHandler;
use crate::process::ChildProcessManager;
use crate::resolve_agent_binary;
use crate::sandbox::{Sandbox, ToolCallResult};
use crate::socket::SocketManager;

// ---------------------------------------------------------------------------
// Credential isolation
// ---------------------------------------------------------------------------

/// API key env var names to strip from the child process environment.
///
/// The sandboxed agent must never hold provider credentials. All LLM
/// completions are proxied through the host process via IPC — the agent sends
/// an `LlmRequest`, the host dispatches it to the provider, and the response
/// comes back as `LlmResponse`. Stripping these vars is a defence-in-depth
/// measure: even if the isolation layer is bypassed, the agent cannot call
/// providers directly without keys.
const CREDENTIAL_ENV_VARS: &[&str] = &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_API_KEY"];

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Which sandbox implementation is available on the current platform.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxPlatform {
    /// `bwrap` is installed — full namespace isolation is available.
    Bubblewrap,
    /// `bwrap` is absent or unavailable — fallback mode only.
    FallbackOnly,
}

/// Returns the strongest sandbox implementation available on this platform.
#[cfg(test)]
pub fn detect_platform() -> SandboxPlatform {
    if crate::bubblewrap::find_bwrap().is_ok() {
        SandboxPlatform::Bubblewrap
    } else {
        SandboxPlatform::FallbackOnly
    }
}

// ---------------------------------------------------------------------------
// FallbackSandbox
// ---------------------------------------------------------------------------

/// Fallback sandbox with IPC proxying but no namespace isolation.
///
/// Agents spawn as regular child processes. Credential isolation via IPC
/// proxying still applies — the agent never sees API keys. For development,
/// testing, and unsupported platforms.
///
/// `execute_tool` takes `&self` to satisfy the Sandbox trait, so the
/// transport is protected by a `Mutex` for interior mutability.
pub struct FallbackSandbox {
    config: SandboxConfig,
    llm_handler: HostLlmHandler,
    // Populated after spawn_agent.
    socket_manager: Option<SocketManager>,
    process: Option<ChildProcessManager>,
    // Mutex for interior mutability: execute_tool receives &self but must
    // send/receive on the transport. Populated after spawn_agent.
    transport: Mutex<Option<IpcTransport>>,
}

impl FallbackSandbox {
    /// Create a new fallback sandbox.
    ///
    /// The sandbox is not yet running — call `spawn_agent` to start it.
    pub fn new(config: SandboxConfig, llm_handler: HostLlmHandler) -> Self {
        Self {
            config,
            llm_handler,
            socket_manager: None,
            process: None,
            transport: Mutex::new(None),
        }
    }

    /// Test-only constructor that injects a pre-built transport.
    ///
    /// Allows unit tests to exercise the IPC dispatch loop without spawning a
    /// real child process. Not part of the public API.
    #[cfg(test)]
    pub(crate) fn with_transport(
        config: SandboxConfig,
        llm_handler: HostLlmHandler,
        transport: IpcTransport,
    ) -> Self {
        Self {
            config,
            llm_handler,
            socket_manager: None,
            process: None,
            transport: Mutex::new(Some(transport)),
        }
    }
}

#[async_trait]
impl Sandbox for FallbackSandbox {
    /// Spawn a regular (non-isolated) child process connected over IPC.
    ///
    /// Creates the Unix socket, spawns the agent binary with `--sandbox-mode`,
    /// strips credential env vars, accepts the connection, and waits for the
    /// agent's `Ready` signal.
    async fn spawn_agent(&mut self) -> Result<(), SandboxError> {
        // Create the IPC socket before spawning so the child can connect.
        let (socket_manager, listener) = SocketManager::create("swage-sandbox")?;
        let socket_path = socket_manager.path().to_path_buf();

        // Re-run the host binary in sandbox mode. In production, the agent
        // entry point is selected by --sandbox-mode and sets up run_agent_loop.
        let agent_binary = resolve_agent_binary()?;

        let mut cmd = tokio::process::Command::new(&agent_binary);
        cmd.arg("--sandbox-mode");
        // Tell the agent where to connect.
        cmd.env("SWAGE_IPC_SOCKET", socket_path.as_os_str());

        // Strip provider API keys — the agent gets completions via IPC proxy,
        // never directly from providers.
        for var in CREDENTIAL_ENV_VARS {
            cmd.env_remove(var);
        }

        let process = ChildProcessManager::spawn(cmd)?;
        tracing::debug!(pid = process.pid(), "fallback sandbox agent spawned");

        // Accept the agent's incoming connection.
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| SandboxError::SpawnFailed {
                message: format!("IPC accept failed: {e}"),
            })?;

        let mut transport = IpcTransport::new(stream);

        // Wait for the agent to signal it is initialised and ready.
        let msg = transport
            .receive_timeout(self.config.tool_call_timeout)
            .await?;

        match msg {
            IpcMessage::Ready => {}
            IpcMessage::ErrorMessage { description, .. } => {
                return Err(SandboxError::IpcError {
                    message: format!("agent error during startup: {description}"),
                });
            }
            other => {
                return Err(SandboxError::IpcError {
                    message: format!("expected Ready signal, got: {other:?}"),
                });
            }
        }

        self.socket_manager = Some(socket_manager);
        self.process = Some(process);
        *self.transport.lock().await = Some(transport);

        Ok(())
    }

    /// Dispatch a tool call to the sandboxed agent.
    ///
    /// Sends a `ToolCallRequest` and runs a dispatch loop until a
    /// `ToolCallResult` arrives. Interleaved `LlmRequest` messages are
    /// handled transparently: the host proxies each request to the provider
    /// and returns the `LlmResponse` before resuming the wait.
    ///
    /// This pattern is necessary because the agent may call the LLM
    /// mid-tool-execution (for reasoning) without the host expecting it.
    async fn execute_tool(
        &self,
        tool_name: &str,
        params: Value,
    ) -> Result<ToolCallResult, SandboxError> {
        let mut guard = self.transport.lock().await;
        let transport = guard.as_mut().ok_or_else(|| SandboxError::Unavailable {
            message: "no active agent connection — call spawn_agent first".into(),
        })?;

        // Send the tool call request.
        let request_id = Uuid::new_v4().to_string();
        transport
            .send(&IpcMessage::ToolCallRequest {
                request_id: request_id.clone(),
                tool_name: tool_name.into(),
                parameters: params,
            })
            .await?;

        // Dispatch loop: route interleaved LLM proxy requests and return when
        // the tool call result arrives.
        loop {
            let msg = transport
                .receive_timeout(self.config.tool_call_timeout)
                .await?;

            match msg {
                IpcMessage::ToolCallResult {
                    request_id: resp_id,
                    output,
                    duration_ms,
                    error,
                } => {
                    if resp_id != request_id {
                        return Err(SandboxError::IpcError {
                            message: format!(
                                "request ID mismatch: sent {request_id}, got {resp_id}"
                            ),
                        });
                    }
                    return Ok(ToolCallResult {
                        output,
                        duration_ms,
                        error,
                    });
                }

                IpcMessage::LlmRequest {
                    request_id: llm_id,
                    messages,
                    model,
                    max_tokens,
                    temperature,
                    tool_definitions,
                } => {
                    // Agent needs an LLM completion mid-tool-call; proxy it.
                    let llm_req = IpcLlmRequest {
                        request_id: llm_id.clone(),
                        messages,
                        model,
                        max_tokens,
                        temperature,
                        tool_definitions,
                    };

                    match self.llm_handler.handle_request(llm_req).await {
                        Ok(llm_resp) => {
                            transport
                                .send(&IpcMessage::LlmResponse {
                                    request_id: llm_resp.request_id,
                                    content: llm_resp.content,
                                    token_usage: llm_resp.token_usage,
                                    finish_reason: llm_resp.finish_reason,
                                    tool_calls: llm_resp.tool_calls,
                                })
                                .await?;
                        }
                        Err(e) => {
                            // Proxy failure — tell the agent so it can
                            // surface the error in the tool result rather
                            // than hanging indefinitely.
                            transport
                                .send(&IpcMessage::ErrorMessage {
                                    request_id: Some(llm_id),
                                    category: ErrorCategory::Provider,
                                    description: format!("LLM proxy error: {e}"),
                                })
                                .await?;
                        }
                    }
                    // Continue the loop — tool result still pending.
                }

                IpcMessage::ErrorMessage { description, .. } => {
                    return Err(SandboxError::IpcError {
                        message: description,
                    });
                }

                other => {
                    return Err(SandboxError::IpcError {
                        message: format!("unexpected message during tool dispatch: {other:?}"),
                    });
                }
            }
        }
    }

    /// Send a shutdown signal, wait for acknowledgement, and clean up.
    ///
    /// Idempotent — silently succeeds if the agent has already terminated or
    /// was never spawned.
    async fn shutdown(&mut self) -> Result<(), SandboxError> {
        // Take the transport out of the Mutex — we have &mut self so there
        // can be no outstanding lock guards.
        let transport_opt = self.transport.get_mut().take();

        if let Some(mut transport) = transport_opt {
            // Signal the agent to shut down gracefully.
            if let Err(e) = transport.send(&IpcMessage::Shutdown).await {
                tracing::warn!("failed to send Shutdown: {e}");
            } else {
                match transport
                    .receive_timeout(self.config.shutdown_grace_period)
                    .await
                {
                    Ok(IpcMessage::ShutdownAck) => {}
                    Ok(other) => {
                        tracing::warn!("unexpected response to Shutdown: {other:?}");
                    }
                    Err(e) => {
                        tracing::warn!("no ShutdownAck within grace period: {e}");
                    }
                }
            }
        }

        // Terminate the child process (SIGTERM then SIGKILL after grace period).
        if let Some(mut process) = self.process.take() {
            process
                .graceful_shutdown(self.config.shutdown_grace_period)
                .await?;
        }

        // Drop the socket manager — its Drop impl removes the socket file.
        self.socket_manager = None;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use swage_core::{CompletionResponse, FinishReason, SandboxError};
    use tokio::net::UnixStream;

    use super::{CREDENTIAL_ENV_VARS, FallbackSandbox, SandboxPlatform, detect_platform};
    use crate::config::SandboxConfig;
    use crate::ipc::message::IpcMessage;
    use crate::ipc::transport::IpcTransport;
    use crate::llm_handler::HostLlmHandler;
    use crate::sandbox::Sandbox;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn test_config() -> SandboxConfig {
        SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            .tool_call_timeout(Duration::from_secs(5))
            .shutdown_grace_period(Duration::from_secs(2))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn no_op_llm_handler() -> HostLlmHandler {
        HostLlmHandler::new(|_req| async {
            Ok::<_, SandboxError>(CompletionResponse {
                content: "mock response".into(),
                input_tokens: 5,
                output_tokens: 3,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        })
    }

    /// Creates a FallbackSandbox wired to the host side of a socket pair.
    /// Returns the sandbox and the mock "agent" transport for injecting messages.
    fn sandbox_with_mock_agent() -> (FallbackSandbox, IpcTransport) {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let sandbox = FallbackSandbox::with_transport(
            test_config(),
            no_op_llm_handler(),
            IpcTransport::new(host_stream),
        );
        (sandbox, IpcTransport::new(agent_stream))
    }

    // -----------------------------------------------------------------------
    // SandboxPlatform
    // -----------------------------------------------------------------------

    #[test]
    fn sandbox_platform_variants_are_distinct() {
        assert_ne!(SandboxPlatform::Bubblewrap, SandboxPlatform::FallbackOnly);
    }

    #[test]
    fn sandbox_platform_is_debug() {
        let debug = format!("{:?}", SandboxPlatform::Bubblewrap);
        assert!(debug.contains("Bubblewrap"));
        let debug = format!("{:?}", SandboxPlatform::FallbackOnly);
        assert!(debug.contains("FallbackOnly"));
    }

    #[test]
    fn detect_platform_returns_a_valid_variant() {
        // The result depends on whether bwrap is installed, but it must be
        // one of the two valid variants — the function must not panic or error.
        let platform = detect_platform();
        assert!(
            matches!(
                platform,
                SandboxPlatform::Bubblewrap | SandboxPlatform::FallbackOnly
            ),
            "detect_platform must return a valid SandboxPlatform, got: {platform:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Credential isolation
    // -----------------------------------------------------------------------

    #[test]
    fn credential_env_vars_list_includes_known_api_keys() {
        assert!(
            CREDENTIAL_ENV_VARS.contains(&"OPENAI_API_KEY"),
            "OPENAI_API_KEY must be stripped from child env"
        );
        assert!(
            CREDENTIAL_ENV_VARS.contains(&"ANTHROPIC_API_KEY"),
            "ANTHROPIC_API_KEY must be stripped from child env"
        );
        assert!(
            CREDENTIAL_ENV_VARS.contains(&"GOOGLE_API_KEY"),
            "GOOGLE_API_KEY must be stripped from child env"
        );
    }

    // -----------------------------------------------------------------------
    // execute_tool — dispatch loop
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_tool_returns_result_on_tool_call_result() {
        let (sandbox, mut agent) = sandbox_with_mock_agent();

        // Agent side: receive the request and respond with a result.
        let sandbox_handle =
            tokio::spawn(
                async move { sandbox.execute_tool("echo", json!({"text": "hello"})).await },
            );

        // Receive ToolCallRequest.
        let msg = agent.receive().await.unwrap();
        let req_id = match &msg {
            IpcMessage::ToolCallRequest { request_id, .. } => request_id.clone(),
            other => panic!("expected ToolCallRequest, got {other:?}"),
        };

        // Send ToolCallResult.
        agent
            .send(&IpcMessage::ToolCallResult {
                request_id: req_id,
                output: json!("hello"),
                duration_ms: 10,
                error: None,
            })
            .await
            .unwrap();

        let result = sandbox_handle.await.unwrap().unwrap();
        assert_eq!(result.output, json!("hello"));
        assert_eq!(result.duration_ms, 10);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn execute_tool_handles_interleaved_llm_request() {
        // Verify the dispatch loop routes an LlmRequest before the ToolCallResult.
        let (sandbox, mut agent) = sandbox_with_mock_agent();

        let sandbox_handle = tokio::spawn(async move {
            sandbox
                .execute_tool("think", json!({"query": "answer"}))
                .await
        });

        // Agent: receive ToolCallRequest.
        let msg = agent.receive().await.unwrap();
        let req_id = match &msg {
            IpcMessage::ToolCallRequest { request_id, .. } => request_id.clone(),
            other => panic!("expected ToolCallRequest, got {other:?}"),
        };

        // Agent: mid-execution, needs an LLM completion.
        agent
            .send(&IpcMessage::LlmRequest {
                request_id: "llm-1".into(),
                messages: vec![],
                model: None,
                max_tokens: Some(100),
                temperature: None,
                tool_definitions: None,
            })
            .await
            .unwrap();

        // Agent: receive the LlmResponse from the host.
        let llm_resp = agent.receive().await.unwrap();
        assert!(
            matches!(llm_resp, IpcMessage::LlmResponse { .. }),
            "host must respond to LlmRequest; got {llm_resp:?}"
        );

        // Agent: now send the final ToolCallResult.
        agent
            .send(&IpcMessage::ToolCallResult {
                request_id: req_id,
                output: json!({"answer": "42"}),
                duration_ms: 50,
                error: None,
            })
            .await
            .unwrap();

        let result = sandbox_handle.await.unwrap().unwrap();
        assert_eq!(result.output, json!({"answer": "42"}));
    }

    #[tokio::test]
    async fn execute_tool_handles_multiple_interleaved_llm_requests() {
        // The dispatch loop must handle more than one interleaved LlmRequest.
        let (sandbox, mut agent) = sandbox_with_mock_agent();

        let sandbox_handle =
            tokio::spawn(async move { sandbox.execute_tool("complex_tool", json!({})).await });

        let msg = agent.receive().await.unwrap();
        let req_id = match &msg {
            IpcMessage::ToolCallRequest { request_id, .. } => request_id.clone(),
            other => panic!("expected ToolCallRequest, got {other:?}"),
        };

        // Two sequential LLM requests before the result.
        for i in 0..2u32 {
            agent
                .send(&IpcMessage::LlmRequest {
                    request_id: format!("llm-{i}"),
                    messages: vec![],
                    model: None,
                    max_tokens: None,
                    temperature: None,
                    tool_definitions: None,
                })
                .await
                .unwrap();

            // Host must respond to each.
            let resp = agent.receive().await.unwrap();
            assert!(matches!(resp, IpcMessage::LlmResponse { .. }));
        }

        agent
            .send(&IpcMessage::ToolCallResult {
                request_id: req_id,
                output: json!("done"),
                duration_ms: 100,
                error: None,
            })
            .await
            .unwrap();

        let result = sandbox_handle.await.unwrap().unwrap();
        assert_eq!(result.output, json!("done"));
    }

    #[tokio::test]
    async fn execute_tool_returns_error_on_error_message() {
        let (sandbox, mut agent) = sandbox_with_mock_agent();

        let sandbox_handle =
            tokio::spawn(async move { sandbox.execute_tool("bad_tool", json!({})).await });

        // Receive the request.
        let _ = agent.receive().await.unwrap();

        // Agent sends an error.
        agent
            .send(&IpcMessage::ErrorMessage {
                request_id: None,
                category: crate::ipc::message::ErrorCategory::ToolExecution,
                description: "tool execution context unavailable".into(),
            })
            .await
            .unwrap();

        let result = sandbox_handle.await.unwrap();
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::IpcError { message } => {
                assert!(message.contains("tool execution context unavailable"));
            }
            other => panic!("expected IpcError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_tool_before_spawn_returns_unavailable() {
        // No transport set — expect an Unavailable error.
        let sandbox = FallbackSandbox::new(test_config(), no_op_llm_handler());

        let result = sandbox.execute_tool("echo", json!({})).await;
        assert!(result.is_err(), "expected error before spawn_agent");
        match result.unwrap_err() {
            SandboxError::Unavailable { .. } => {}
            other => panic!("expected SandboxError::Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_tool_returns_error_on_request_id_mismatch() {
        let (sandbox, mut agent) = sandbox_with_mock_agent();

        let sandbox_handle =
            tokio::spawn(async move { sandbox.execute_tool("echo", json!({})).await });

        // Receive the request (we'll ignore the request_id intentionally).
        let _ = agent.receive().await.unwrap();

        // Send a result with a wrong request ID.
        agent
            .send(&IpcMessage::ToolCallResult {
                request_id: "wrong-id".into(),
                output: json!("oops"),
                duration_ms: 1,
                error: None,
            })
            .await
            .unwrap();

        let result = sandbox_handle.await.unwrap();
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::IpcError { message } => {
                assert!(
                    message.contains("mismatch"),
                    "error should mention mismatch; got: {message}"
                );
            }
            other => panic!("expected IpcError, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // shutdown — idempotency
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn shutdown_before_spawn_is_idempotent() {
        let mut sandbox = FallbackSandbox::new(test_config(), no_op_llm_handler());
        // No process, no connection — should succeed silently.
        sandbox
            .shutdown()
            .await
            .expect("shutdown before spawn must succeed");
    }

    #[tokio::test]
    async fn shutdown_sends_shutdown_and_awaits_ack() {
        let (mut sandbox, mut agent) = sandbox_with_mock_agent();

        let shutdown_handle = tokio::spawn(async move { sandbox.shutdown().await });

        // Agent side: receive Shutdown and respond with Ack.
        let msg = agent.receive().await.unwrap();
        assert!(matches!(msg, IpcMessage::Shutdown));
        agent.send(&IpcMessage::ShutdownAck).await.unwrap();

        shutdown_handle
            .await
            .unwrap()
            .expect("shutdown must succeed");
    }

    #[tokio::test]
    async fn shutdown_tolerates_missing_ack() {
        // If the agent doesn't acknowledge within the grace period, shutdown
        // must still succeed — it will force-kill the process.
        let config = SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            .shutdown_grace_period(Duration::from_millis(50))
            .tool_call_timeout(Duration::from_secs(5))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let (host_stream, _agent_stream) = UnixStream::pair().unwrap();
        let mut sandbox = FallbackSandbox::with_transport(
            config,
            no_op_llm_handler(),
            IpcTransport::new(host_stream),
        );

        // _agent_stream kept alive but sends nothing — host will time out.
        sandbox
            .shutdown()
            .await
            .expect("shutdown must succeed even without ShutdownAck");
    }
}
