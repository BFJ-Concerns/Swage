//! Agent process entry point — the main loop running inside the sandbox.
//!
//! Connects to the host via the Unix socket path from the SWAGE_IPC_SOCKET
//! environment variable, sends a ready signal, then processes incoming
//! messages: tool call requests are executed locally, LLM requests are
//! proxied through the host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;
use swage_core::SandboxError;
use swage_tool::context::{ToolContext, UserInfo};
use swage_tool::tool::Tool;
use tokio::net::UnixStream;

use crate::ipc::agent::AgentConnection;
use crate::ipc::message::{ErrorCategory, IpcMessage};
use crate::ipc::transport::IpcTransport;

// ---------------------------------------------------------------------------
// Sandbox tool context
// ---------------------------------------------------------------------------

/// Minimal `ToolContext` supplied to tools running inside the sandboxed process.
///
/// The agent process has no access to session storage, so identifiers are
/// fixed sentinel values. Tools should rely on `working_directory` for any
/// path operations.
struct SandboxToolContext {
    working_directory: PathBuf,
    // Allowed paths are empty by default; the Landlock layer enforces kernel-
    // level restrictions, so the context doesn't need to duplicate them.
}

impl ToolContext for SandboxToolContext {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn agent_id(&self) -> &str {
        "sandbox"
    }

    fn session_id(&self) -> &str {
        "sandbox"
    }

    fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    fn allowed_paths(&self) -> &[PathBuf] {
        &[]
    }

    fn user_info(&self) -> Option<&UserInfo> {
        None
    }
}

// ---------------------------------------------------------------------------
// Internal loop — accepts a pre-built connection for testability
// ---------------------------------------------------------------------------

/// Core message loop shared by the public entry point and unit tests.
///
/// Sends `Ready`, then dispatches incoming messages until `Shutdown` arrives:
///
/// - `ToolCallRequest` → look up tool, execute, send `ToolCallResult`
/// - `Shutdown` → send `ShutdownAck`, return `Ok`
/// - Anything else → send `ErrorMessage(Protocol)`, continue
///
/// This function is `pub(crate)` so tests can inject a pre-connected
/// `AgentConnection` without touching the filesystem or environment.
pub(crate) async fn run_loop(
    mut conn: AgentConnection,
    tools: Vec<Box<dyn Tool>>,
) -> Result<(), SandboxError> {
    let ctx = SandboxToolContext {
        working_directory: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };

    // Index tools by name for O(1) lookup per request.
    let tool_map: HashMap<String, Box<dyn Tool>> = tools
        .into_iter()
        .map(|t| (t.name().to_owned(), t))
        .collect();

    // Signal readiness before entering the dispatch loop.
    conn.send_ready().await?;

    loop {
        match conn.receive_message().await? {
            IpcMessage::ToolCallRequest {
                request_id,
                tool_name,
                parameters,
            } => {
                match tool_map.get(&tool_name) {
                    Some(tool) => {
                        let start = Instant::now();
                        let result = tool.execute(parameters, &ctx).await;
                        let duration_ms = start.elapsed().as_millis() as u64;

                        match result {
                            Ok(output) => {
                                conn.send_tool_result(request_id, output, duration_ms, None)
                                    .await?;
                            }
                            Err(e) => {
                                // Tool ran but errored — send a result with the error
                                // field set so the host can surface the failure.
                                conn.send_tool_result(
                                    request_id,
                                    Value::Null,
                                    duration_ms,
                                    Some(e.to_string()),
                                )
                                .await?;
                            }
                        }
                    }
                    None => {
                        conn.send_error(
                            Some(request_id),
                            ErrorCategory::ToolExecution,
                            format!("unknown tool: {tool_name}"),
                        )
                        .await?;
                    }
                }
            }

            IpcMessage::Shutdown => {
                conn.send_shutdown_ack().await?;
                break;
            }

            // Any other variant (Ready, ShutdownAck, LlmRequest, etc.) is
            // unexpected in the inbound direction — report it and continue.
            unexpected => {
                conn.send_error(
                    None,
                    ErrorCategory::Protocol,
                    format!("unexpected message in agent loop: {unexpected:?}"),
                )
                .await?;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the agent's main loop inside a sandboxed child process.
///
/// Reads `SWAGE_IPC_SOCKET` from the environment, connects to the host's
/// Unix domain socket, then delegates to [`run_loop`]. Returns when the host
/// sends a `Shutdown` signal.
///
/// `tools` is the set of tools the agent can execute. The caller constructs
/// them before calling this function — typically the binary's sandbox-mode
/// entry point.
pub async fn run_agent_loop(tools: Vec<Box<dyn Tool>>) -> Result<(), SandboxError> {
    let socket_path = std::env::var("SWAGE_IPC_SOCKET").map_err(|_| SandboxError::IpcError {
        message: "SWAGE_IPC_SOCKET environment variable not set".into(),
    })?;

    let stream = UnixStream::connect(&socket_path)
        .await
        .map_err(|e| SandboxError::IpcError {
            message: format!("failed to connect to host socket at {socket_path}: {e}"),
        })?;

    let conn = AgentConnection::new(IpcTransport::new(stream));

    run_loop(conn, tools).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use swage_core::ToolError;
    use swage_tool::context::ToolContext;
    use swage_tool::tool::Tool;
    use tokio::net::UnixStream;

    use super::run_loop;
    use crate::ipc::agent::AgentConnection;
    use crate::ipc::message::{ErrorCategory, IpcMessage};
    use crate::ipc::transport::IpcTransport;

    // -- Test helpers ---------------------------------------------------------

    fn make_connection_pair() -> (AgentConnection, IpcTransport) {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        (
            AgentConnection::new(IpcTransport::new(agent_stream)),
            IpcTransport::new(host_stream),
        )
    }

    /// Echo tool — returns its input parameters unchanged.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Returns its parameters unchanged."
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            Ok(params)
        }
    }

    /// Failing tool — always returns an error.
    struct FailingTool;

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "fail"
        }
        fn description(&self) -> &str {
            "Always errors."
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(
            &self,
            _params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            Err(ToolError::ExecutionFailed {
                name: "fail".into(),
                message: "intentional failure".into(),
            })
        }
    }

    // -- Tests ----------------------------------------------------------------

    #[tokio::test]
    async fn loop_sends_ready_on_start() {
        let (conn, mut host) = make_connection_pair();

        let loop_handle = tokio::spawn(async move { run_loop(conn, vec![]).await });

        // The first message from the agent should be Ready.
        let first_msg = host.receive().await.unwrap();
        assert!(matches!(first_msg, IpcMessage::Ready));

        // Send Shutdown to end the loop cleanly.
        host.send(&IpcMessage::Shutdown).await.unwrap();
        let ack = host.receive().await.unwrap();
        assert!(matches!(ack, IpcMessage::ShutdownAck));

        loop_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loop_handles_shutdown_cleanly() {
        let (conn, mut host) = make_connection_pair();

        let loop_handle = tokio::spawn(async move { run_loop(conn, vec![]).await });

        // Consume Ready.
        let _ = host.receive().await.unwrap();

        // Send Shutdown — loop should send ShutdownAck and exit.
        host.send(&IpcMessage::Shutdown).await.unwrap();

        let ack = host.receive().await.unwrap();
        assert!(matches!(ack, IpcMessage::ShutdownAck));

        let result = loop_handle.await.unwrap();
        assert!(result.is_ok(), "loop should return Ok after shutdown");
    }

    #[tokio::test]
    async fn loop_executes_tool_and_sends_result() {
        let (conn, mut host) = make_connection_pair();

        let loop_handle =
            tokio::spawn(async move { run_loop(conn, vec![Box::new(EchoTool)]).await });

        // Consume Ready.
        let _ = host.receive().await.unwrap();

        // Send a tool call request.
        host.send(&IpcMessage::ToolCallRequest {
            request_id: "req-1".into(),
            tool_name: "echo".into(),
            parameters: json!({"text": "hello sandbox"}),
        })
        .await
        .unwrap();

        // Agent should execute and send back the result.
        let result_msg = host.receive().await.unwrap();
        match result_msg {
            IpcMessage::ToolCallResult {
                request_id,
                output,
                error,
                ..
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(output, json!({"text": "hello sandbox"}));
                assert!(error.is_none());
            }
            other => panic!("expected ToolCallResult, got {:?}", other),
        }

        // Shut down.
        host.send(&IpcMessage::Shutdown).await.unwrap();
        let _ = host.receive().await.unwrap();
        loop_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loop_sends_error_message_for_unknown_tool() {
        let (conn, mut host) = make_connection_pair();

        let loop_handle = tokio::spawn(async move { run_loop(conn, vec![]).await });

        let _ = host.receive().await.unwrap();

        host.send(&IpcMessage::ToolCallRequest {
            request_id: "req-2".into(),
            tool_name: "nonexistent_tool".into(),
            parameters: json!({}),
        })
        .await
        .unwrap();

        let err_msg = host.receive().await.unwrap();
        match err_msg {
            IpcMessage::ErrorMessage {
                request_id,
                category,
                description,
            } => {
                assert_eq!(request_id, Some("req-2".into()));
                assert_eq!(category, ErrorCategory::ToolExecution);
                assert!(description.contains("nonexistent_tool"));
            }
            other => panic!("expected ErrorMessage, got {:?}", other),
        }

        host.send(&IpcMessage::Shutdown).await.unwrap();
        let _ = host.receive().await.unwrap();
        loop_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loop_sends_tool_result_with_error_on_execution_failure() {
        let (conn, mut host) = make_connection_pair();

        let loop_handle =
            tokio::spawn(async move { run_loop(conn, vec![Box::new(FailingTool)]).await });

        let _ = host.receive().await.unwrap();

        host.send(&IpcMessage::ToolCallRequest {
            request_id: "req-3".into(),
            tool_name: "fail".into(),
            parameters: json!({}),
        })
        .await
        .unwrap();

        // Tool execution failed — agent should still send a ToolCallResult
        // with the error field set, so the host can surface the failure.
        let result_msg = host.receive().await.unwrap();
        match result_msg {
            IpcMessage::ToolCallResult {
                request_id, error, ..
            } => {
                assert_eq!(request_id, "req-3");
                assert!(
                    error.is_some(),
                    "error field should be set on execution failure"
                );
                assert!(error.unwrap().contains("intentional failure"));
            }
            other => panic!("expected ToolCallResult, got {:?}", other),
        }

        host.send(&IpcMessage::Shutdown).await.unwrap();
        let _ = host.receive().await.unwrap();
        loop_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loop_reports_protocol_error_for_unexpected_message() {
        let (conn, mut host) = make_connection_pair();

        let loop_handle = tokio::spawn(async move { run_loop(conn, vec![]).await });

        let _ = host.receive().await.unwrap();

        // Send an unexpected message type (Ready is only valid agent→host).
        host.send(&IpcMessage::Ready).await.unwrap();

        let err_msg = host.receive().await.unwrap();
        match err_msg {
            IpcMessage::ErrorMessage { category, .. } => {
                assert_eq!(category, ErrorCategory::Protocol);
            }
            other => panic!("expected ErrorMessage, got {:?}", other),
        }

        host.send(&IpcMessage::Shutdown).await.unwrap();
        let _ = host.receive().await.unwrap();
        loop_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loop_measures_tool_duration() {
        let (conn, mut host) = make_connection_pair();

        let loop_handle =
            tokio::spawn(async move { run_loop(conn, vec![Box::new(EchoTool)]).await });

        let _ = host.receive().await.unwrap();

        host.send(&IpcMessage::ToolCallRequest {
            request_id: "req-time".into(),
            tool_name: "echo".into(),
            parameters: json!({"x": 1}),
        })
        .await
        .unwrap();

        let result = host.receive().await.unwrap();
        match result {
            // Verify duration_ms field is present and is a reasonable value.
            IpcMessage::ToolCallResult { duration_ms, .. } => {
                // An echo tool should complete far under any sane timeout.
                assert!(
                    duration_ms < 5_000,
                    "duration_ms should be reasonable, got {duration_ms}"
                );
            }
            other => panic!("expected ToolCallResult, got {:?}", other),
        }

        host.send(&IpcMessage::Shutdown).await.unwrap();
        let _ = host.receive().await.unwrap();
        loop_handle.await.unwrap().unwrap();
    }
}
