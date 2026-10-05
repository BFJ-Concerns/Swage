//! BubblewrapSandbox — full Sandbox trait implementation for Linux.
//!
//! Spawns agents inside bubblewrap with namespace isolation and IPC-mediated
//! tool dispatch and LLM proxying. Landlock is applied by the child process
//! after bubblewrap has exec'd the agent binary.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use swage_core::SandboxError;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::bubblewrap::command::build_bwrap_command;
use crate::config::SandboxConfig;
use crate::ipc::in_flight::InFlightToolCall;
use crate::ipc::message::{ErrorCategory, IpcMessage, LlmRequest as IpcLlmRequest};
use crate::ipc::transport::IpcTransport;
use crate::llm_handler::HostLlmHandler;
use crate::process::{ChildProcessManager, cleanup_failed_startup_process};
use crate::resolve_agent_binary;
use crate::sandbox::{Sandbox, ToolCallResult};
use crate::socket::SocketManager;

// ---------------------------------------------------------------------------
// Credential isolation
// ---------------------------------------------------------------------------

/// API key env var names to strip from the bwrap process environment.
///
/// Bubblewrap inherits the host's environment by default — without explicit
/// stripping, provider keys would propagate into the sandbox. Removing them
/// here means bwrap never sees them and cannot forward them to the agent.
/// All LLM completions are proxied through the host via IPC instead.
const CREDENTIAL_ENV_VARS: &[&str] = &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_API_KEY"];

// ---------------------------------------------------------------------------
// BubblewrapSandbox
// ---------------------------------------------------------------------------

/// Bubblewrap-based sandbox with full namespace isolation.
///
/// On Linux with `bwrap` installed, agents run inside PID, network, mount,
/// user, IPC, and UTS namespace-isolated sandboxes. Credential isolation via
/// IPC proxying is layered on top — the agent never sees API keys.
///
/// `execute_tool` takes `&self` to satisfy the Sandbox trait, so the
/// transport is protected by a `Mutex` for interior mutability.
pub struct BubblewrapSandbox {
    config: SandboxConfig,
    llm_handler: HostLlmHandler,
    // Populated after spawn_agent.
    socket_manager: Mutex<Option<SocketManager>>,
    process: Mutex<Option<ChildProcessManager>>,
    // Mutex for interior mutability: execute_tool receives &self but must
    // send/receive on the transport. Populated after spawn_agent. Shared so
    // an abandoned tool call can keep it while its cancel is sent.
    transport: Arc<Mutex<Option<IpcTransport>>>,
}

impl BubblewrapSandbox {
    /// Create a new bubblewrap sandbox.
    ///
    /// The sandbox is not yet running — call `spawn_agent` to start it.
    /// Returns `SandboxError::Unavailable` at spawn time if `bwrap` is not
    /// installed on the host.
    pub fn new(config: SandboxConfig, llm_handler: HostLlmHandler) -> Self {
        Self {
            config,
            llm_handler,
            socket_manager: Mutex::new(None),
            process: Mutex::new(None),
            transport: Arc::new(Mutex::new(None)),
        }
    }

    /// Test-only constructor that injects a pre-built transport.
    ///
    /// Allows unit tests to exercise the IPC dispatch loop without spawning a
    /// real bwrap process. Not part of the public API.
    #[cfg(test)]
    pub(crate) fn with_transport(
        config: SandboxConfig,
        llm_handler: HostLlmHandler,
        transport: IpcTransport,
    ) -> Self {
        Self {
            config,
            llm_handler,
            socket_manager: Mutex::new(None),
            process: Mutex::new(None),
            transport: Arc::new(Mutex::new(Some(transport))),
        }
    }
}

#[async_trait]
impl Sandbox for BubblewrapSandbox {
    /// Spawn the agent inside a bubblewrap sandbox.
    ///
    /// Builds the `bwrap` command, strips credential env vars, and waits for
    /// the agent's `Ready` signal before returning.
    // Timeout only cancels `listener.accept()` while startup owns the socket and
    // child process; the timeout arm immediately tears down that child.
    async fn spawn_agent(&mut self) -> Result<(), SandboxError> {
        // Create the IPC socket before spawning so the child can connect.
        let (socket_manager, listener) = SocketManager::create("swage-sandbox")?;
        let socket_path = socket_manager.path().to_path_buf();

        // Determine the agent binary — the host re-execs itself in sandbox mode.
        let agent_binary = resolve_agent_binary()?;

        // Build the bwrap command with namespace isolation and mount config.
        // The socket path is injected via --setenv in the command builder.
        let mut cmd = build_bwrap_command(&self.config, &socket_path, &agent_binary)?;

        // Strip provider API keys — bwrap inherits the parent's environment,
        // so we must remove credentials before it passes them through.
        for var in CREDENTIAL_ENV_VARS {
            cmd.env_remove(var);
        }

        let process = ChildProcessManager::spawn(cmd)?;
        tracing::debug!(pid = process.pid(), "bubblewrap sandbox agent spawned");

        // Accept the agent's incoming connection.
        let (stream, _addr) =
            match tokio::time::timeout(self.config.tool_call_timeout, listener.accept()).await {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => {
                    cleanup_failed_startup_process(
                        process,
                        self.config.shutdown_grace_period,
                        "failed to shut down bubblewrap sandbox after IPC accept error",
                    )
                    .await;
                    return Err(SandboxError::SpawnFailed {
                        message: format!("IPC accept failed: {error}"),
                    });
                }
                Err(_) => {
                    cleanup_failed_startup_process(
                        process,
                        self.config.shutdown_grace_period,
                        "failed to shut down bubblewrap sandbox after IPC accept timeout",
                    )
                    .await;
                    return Err(SandboxError::SpawnFailed {
                        message: "timed out waiting for sandbox agent IPC connection".into(),
                    });
                }
            };

        let mut transport = IpcTransport::new(stream);

        // Wait for the agent to signal it is initialised and ready.
        let msg = match transport
            .receive_timeout(self.config.tool_call_timeout)
            .await
        {
            Ok(message) => message,
            Err(error) => {
                cleanup_failed_startup_process(
                    process,
                    self.config.shutdown_grace_period,
                    "failed to shut down bubblewrap sandbox after startup handshake error",
                )
                .await;
                return Err(error);
            }
        };

        match msg {
            IpcMessage::Ready => {}
            IpcMessage::ErrorMessage { description, .. } => {
                cleanup_failed_startup_process(
                    process,
                    self.config.shutdown_grace_period,
                    "failed to shut down bubblewrap sandbox after startup error message",
                )
                .await;
                return Err(SandboxError::IpcError {
                    message: format!("agent error during startup: {description}"),
                });
            }
            other => {
                cleanup_failed_startup_process(
                    process,
                    self.config.shutdown_grace_period,
                    "failed to shut down bubblewrap sandbox after unexpected startup message",
                )
                .await;
                return Err(SandboxError::IpcError {
                    message: format!("expected Ready signal, got: {other:?}"),
                });
            }
        }

        *self.socket_manager.get_mut() = Some(socket_manager);
        *self.process.get_mut() = Some(process);
        *self.transport.lock().await = Some(transport);

        Ok(())
    }

    /// Dispatch a tool call to the sandboxed agent.
    ///
    /// Sends a `ToolCallRequest` and runs a dispatch loop until a
    /// `ToolCallResult` arrives. Interleaved `LlmRequest` messages are
    /// handled transparently: the host proxies each request to the provider
    /// and returns the `LlmResponse` before resuming the wait.
    async fn execute_tool(
        &self,
        tool_name: &str,
        params: Value,
    ) -> Result<ToolCallResult, SandboxError> {
        let guard = Arc::clone(&self.transport).lock_owned().await;
        if guard.is_none() {
            return Err(SandboxError::Unavailable {
                message: "no active agent connection — call spawn_agent first".into(),
            });
        }
        // Dropping `call` before its result arrives — the turn was cancelled —
        // tells the agent to stop the tool.
        let mut call = InFlightToolCall::new(guard, Uuid::new_v4().to_string());
        let request_id = call.request_id().to_owned();
        call.transport()
            .send(&IpcMessage::ToolCallRequest {
                request_id: request_id.clone(),
                tool_name: tool_name.into(),
                parameters: params,
            })
            .await?;

        // Dispatch loop: route interleaved LLM proxy requests and return when
        // the tool call result arrives.
        loop {
            let msg = call
                .transport()
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
                        // A late answer to a call abandoned earlier, which the
                        // agent finished before it read the cancel.
                        if call.transport().take_abandoned_tool_call(&resp_id) {
                            continue;
                        }
                        return Err(SandboxError::IpcError {
                            message: format!(
                                "request ID mismatch: sent {request_id}, got {resp_id}"
                            ),
                        });
                    }
                    call.settle();
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
                            call.transport()
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
                            // Proxy failure — tell the agent so it can surface
                            // the error rather than hanging indefinitely.
                            call.transport()
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
    async fn shutdown(&self) -> Result<(), SandboxError> {
        let transport_opt = self.transport.lock().await.take();

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
        if let Some(mut process) = self.process.lock().await.take() {
            process
                .graceful_shutdown(self.config.shutdown_grace_period)
                .await?;
        }

        // Drop the socket manager — its Drop impl removes the socket file.
        *self.socket_manager.lock().await = None;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::json;
    use swage_core::{CompletionResponse, FinishReason, SandboxError};
    use tokio::net::UnixStream;

    use super::{BubblewrapSandbox, CREDENTIAL_ENV_VARS};
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

    fn startup_timeout_config() -> SandboxConfig {
        SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            .tool_call_timeout(Duration::from_millis(50))
            .shutdown_grace_period(Duration::from_millis(250))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn write_non_connecting_bwrap_script(dir: &Path) -> (PathBuf, PathBuf) {
        let pid_file = dir.join("bwrap.pid");
        let script_path = dir.join("bwrap");
        fs::write(
            &script_path,
            format!(
                "#!/bin/sh\necho $$ > '{}'\n/bin/sleep 60\n",
                pid_file.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).unwrap();
        (script_path, pid_file)
    }

    fn read_pid(pid_file: &Path) -> u32 {
        fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap()
    }

    fn is_pid_running(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
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

    /// Creates a BubblewrapSandbox wired to the host side of a socket pair.
    /// Returns the sandbox and the mock "agent" transport for injecting messages.
    fn sandbox_with_mock_agent() -> (BubblewrapSandbox, IpcTransport) {
        let (host_stream, agent_stream) = UnixStream::pair().unwrap();
        let sandbox = BubblewrapSandbox::with_transport(
            test_config(),
            no_op_llm_handler(),
            IpcTransport::new(host_stream),
        );
        (sandbox, IpcTransport::new(agent_stream))
    }

    #[tokio::test]
    async fn spawn_agent_timeout_kills_spawned_bwrap_child() {
        let _env_guard = crate::SANDBOX_TEST_ENV_LOCK.lock().await;
        let temp_dir = tempfile::tempdir().unwrap();
        let (_script_path, pid_file) = write_non_connecting_bwrap_script(temp_dir.path());
        let previous_path = std::env::var_os("PATH");
        let test_path = match &previous_path {
            Some(path) => {
                let mut paths = vec![temp_dir.path().to_path_buf()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).unwrap()
            }
            None => temp_dir.path().as_os_str().to_owned(),
        };
        unsafe { std::env::set_var("PATH", test_path) };

        let mut sandbox = BubblewrapSandbox::new(startup_timeout_config(), no_op_llm_handler());
        let result = sandbox.spawn_agent().await;

        match previous_path {
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        assert!(
            result.is_err(),
            "spawn should time out when bwrap child never connects"
        );
        let pid = read_pid(&pid_file);
        assert!(
            !is_pid_running(pid),
            "timed-out bubblewrap startup should terminate and reap child process {pid}"
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
    // Platform — spawn without bwrap
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_agent_fails_gracefully_without_bwrap() {
        let _env_guard = crate::SANDBOX_TEST_ENV_LOCK.lock().await;
        // This test only asserts when bwrap is absent — when present, the real
        // spawn path runs. We verify the error variant on the failure path.
        use crate::bubblewrap::find_bwrap;

        if find_bwrap().is_err() {
            let mut sandbox = BubblewrapSandbox::new(test_config(), no_op_llm_handler());
            let result = sandbox.spawn_agent().await;
            assert!(result.is_err(), "spawn should fail without bwrap");
            match result.unwrap_err() {
                SandboxError::Unavailable { .. } => {}
                other => panic!("expected Unavailable, got {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // execute_tool — dispatch loop
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_tool_returns_result_on_tool_call_result() {
        let (sandbox, mut agent) = sandbox_with_mock_agent();

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
        let sandbox = BubblewrapSandbox::new(test_config(), no_op_llm_handler());

        let result = sandbox.execute_tool("echo", json!({})).await;
        assert!(result.is_err(), "expected error before spawn_agent");
        match result.unwrap_err() {
            SandboxError::Unavailable { .. } => {}
            other => panic!("expected SandboxError::Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dropped_execute_tool_cancels_the_call_and_skips_its_late_result() {
        let (sandbox, mut agent) = sandbox_with_mock_agent();
        let sandbox = std::sync::Arc::new(sandbox);

        // The turn is cancelled while the call is in flight: its future drops.
        let abandoned = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            sandbox.execute_tool("shell", json!({"command": "sleep 300"})),
        );
        let abandoned_id = tokio::join!(abandoned, async {
            match agent.receive().await.unwrap() {
                IpcMessage::ToolCallRequest { request_id, .. } => request_id,
                other => panic!("expected ToolCallRequest, got {other:?}"),
            }
        })
        .1;

        match agent
            .receive_timeout(std::time::Duration::from_secs(5))
            .await
            .expect("the agent must be told the call was cancelled")
        {
            IpcMessage::ToolCallCancel { request_id } => assert_eq!(request_id, abandoned_id),
            other => panic!("expected ToolCallCancel, got {other:?}"),
        }

        // The agent answers the cancelled call after the host has moved on;
        // the next call must still receive its own result.
        let next = tokio::spawn({
            let sandbox = std::sync::Arc::clone(&sandbox);
            async move { sandbox.execute_tool("echo", json!({})).await }
        });
        let next_id = match agent.receive().await.unwrap() {
            IpcMessage::ToolCallRequest { request_id, .. } => request_id,
            other => panic!("expected ToolCallRequest, got {other:?}"),
        };
        for (request_id, output) in [(abandoned_id, json!("stale")), (next_id, json!("fresh"))] {
            agent
                .send(&IpcMessage::ToolCallResult {
                    request_id,
                    output,
                    duration_ms: 1,
                    error: None,
                })
                .await
                .unwrap();
        }

        let result = next.await.unwrap().expect("next call must not desync");
        assert_eq!(result.output, json!("fresh"));
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
        let sandbox = BubblewrapSandbox::new(test_config(), no_op_llm_handler());
        sandbox
            .shutdown()
            .await
            .expect("shutdown before spawn must succeed");
    }

    #[tokio::test]
    async fn shutdown_sends_shutdown_and_awaits_ack() {
        let (sandbox, mut agent) = sandbox_with_mock_agent();

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
        let config = SandboxConfig::builder()
            .working_directory(std::env::temp_dir())
            .shutdown_grace_period(Duration::from_millis(50))
            .tool_call_timeout(Duration::from_secs(5))
            .llm_request_timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let (host_stream, _agent_stream) = UnixStream::pair().unwrap();
        let sandbox = BubblewrapSandbox::with_transport(
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
