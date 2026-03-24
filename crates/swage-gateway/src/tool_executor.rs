//! Gateway-specific tool execution strategies.
//!
//! Two strategies live here:
//!
//! - **`SandboxedExecutor`** — routes tool calls through a bubblewrap sandbox.
//! - **`GatewayExecutionStrategy`** — wraps an optional inner strategy and
//!   scopes `PYTHON_CONTEXT` around `python_execute` calls so that sub-tool
//!   dispatch inside Python code has access to the WebSocket sender, turn_id,
//!   and parent call_id.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use swage_core::{SandboxError, ToolError};
use swage_sandbox::Sandbox;
use swage_tool::context::ToolContext;
use swage_tool::dispatch::ToolExecutionStrategy;
use swage_tool::tool::Tool;

use crate::tool::python_execute::{PythonExecuteContext, PYTHON_CONTEXT};
use crate::websocket::ServerEvent;

/// Routes tool execution through a bubblewrap sandbox.
///
/// The sandbox looks up and runs the tool by name internally, so the `tool`
/// parameter from the strategy interface is unused. The ToolDispatcher's other
/// pipeline stages (permission check, rate limit, schema validation, leak
/// detection) still run before reaching this strategy.
pub struct SandboxedExecutor {
    sandbox: Arc<dyn Sandbox>,
}

impl SandboxedExecutor {
    /// Wrap a sandbox instance for use as a tool execution strategy.
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl ToolExecutionStrategy for SandboxedExecutor {
    async fn execute(
        &self,
        _tool: &dyn Tool,
        tool_name: &str,
        params: &Value,
        _ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        match self.sandbox.execute_tool(tool_name, params.clone()).await {
            Ok(result) => match result.error {
                // The sandboxed tool itself reported a failure.
                Some(message) => Err(ToolError::ExecutionFailed {
                    name: tool_name.to_owned(),
                    message,
                }),
                None => Ok(result.output),
            },
            Err(SandboxError::IpcError { message }) => Err(ToolError::ExecutionFailed {
                name: tool_name.to_owned(),
                message: format!("sandbox communication error: {message}"),
            }),
            Err(other) => Err(ToolError::ExecutionFailed {
                name: tool_name.to_owned(),
                message: other.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// GatewayExecutionStrategy — PYTHON_CONTEXT scoping + optional sandbox
// ---------------------------------------------------------------------------

/// Composite execution strategy for the gateway turn loop.
///
/// Intercepts `python_execute` calls to scope the `PYTHON_CONTEXT` task-local,
/// which gives the `PythonExecuteTool` access to the WebSocket sender, turn_id,
/// and parent call_id it needs for sub-tool event emission and persistence.
///
/// For all other tools, delegates to an optional inner strategy (typically
/// `SandboxedExecutor`) or falls back to direct `tool.execute()`.
pub struct GatewayExecutionStrategy {
    /// Optional inner strategy — e.g. `SandboxedExecutor` when sandbox is enabled.
    inner: Option<Arc<dyn ToolExecutionStrategy>>,
    /// WebSocket sender for sub-tool lifecycle events inside Python.
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
}

impl GatewayExecutionStrategy {
    pub fn new(
        inner: Option<Arc<dyn ToolExecutionStrategy>>,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Self {
        Self { inner, ws_tx }
    }
}

#[async_trait]
impl ToolExecutionStrategy for GatewayExecutionStrategy {
    async fn execute(
        &self,
        tool: &dyn Tool,
        tool_name: &str,
        params: &Value,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        if tool_name == "python_execute" {
            // Extract call_id and turn_id from the ToolContext. The agent
            // populates these on AgentToolContext before dispatching.
            let call_id = ctx.call_id().unwrap_or("unknown").to_string();
            let turn_id = ctx.turn_id().unwrap_or_else(Uuid::nil);

            let py_ctx = PythonExecuteContext {
                ws_tx: self.ws_tx.clone(),
                turn_id,
                call_id,
            };

            // Scope the task-local around the tool execution so
            // PythonExecuteTool::execute() can read it.
            PYTHON_CONTEXT
                .scope(py_ctx, async {
                    // Still delegate to inner strategy if present (e.g. sandbox),
                    // otherwise call the tool directly.
                    match &self.inner {
                        Some(strategy) => strategy.execute(tool, tool_name, params, ctx).await,
                        None => tool.execute(params.clone(), ctx).await,
                    }
                })
                .await
        } else {
            // Non-Python tools: delegate to inner strategy or direct execution.
            match &self.inner {
                Some(strategy) => strategy.execute(tool, tool_name, params, ctx).await,
                None => tool.execute(params.clone(), ctx).await,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use swage_sandbox::ToolCallResult;
    use swage_tool::context::MockToolContext;
    use tokio::sync::Mutex;

    // -- Mock sandbox ---------------------------------------------------------

    /// Canned responses for the mock sandbox.
    enum MockResponse {
        Success(Value),
        ToolError(String),
        IpcError(String),
        Unavailable(String),
    }

    struct MockSandbox {
        /// The response to return from execute_tool.
        response: Mutex<MockResponse>,
        /// Records the (tool_name, params) from the last call.
        last_call: Mutex<Option<(String, Value)>>,
    }

    impl MockSandbox {
        fn new(response: MockResponse) -> Self {
            Self {
                response: Mutex::new(response),
                last_call: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Sandbox for MockSandbox {
        async fn spawn_agent(&mut self) -> Result<(), SandboxError> {
            Ok(())
        }

        async fn execute_tool(
            &self,
            tool_name: &str,
            params: Value,
        ) -> Result<ToolCallResult, SandboxError> {
            *self.last_call.lock().await = Some((tool_name.to_owned(), params.clone()));

            match &*self.response.lock().await {
                MockResponse::Success(output) => Ok(ToolCallResult {
                    output: output.clone(),
                    duration_ms: 0,
                    error: None,
                }),
                MockResponse::ToolError(msg) => Ok(ToolCallResult {
                    output: Value::Null,
                    duration_ms: 0,
                    error: Some(msg.clone()),
                }),
                MockResponse::IpcError(msg) => Err(SandboxError::IpcError {
                    message: msg.clone(),
                }),
                MockResponse::Unavailable(msg) => Err(SandboxError::Unavailable {
                    message: msg.clone(),
                }),
            }
        }

        async fn shutdown(&mut self) -> Result<(), SandboxError> {
            Ok(())
        }
    }

    // -- Dummy tool (unused by sandbox path, but required by the trait) --------

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str { "dummy" }
        fn description(&self) -> &str { "unused" }
        fn parameters(&self) -> Value { json!({"type": "object"}) }
        async fn execute(&self, _params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            panic!("sandbox path should never call tool.execute() directly")
        }
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
    }

    // -- Tests ----------------------------------------------------------------

    #[tokio::test]
    async fn routes_call_through_sandbox_and_returns_output() {
        let sandbox = Arc::new(MockSandbox::new(
            MockResponse::Success(json!({"result": "ok"})),
        ));
        let executor = SandboxedExecutor::new(sandbox.clone());

        let result = executor
            .execute(&DummyTool, "echo", &json!({"text": "hi"}), &ctx())
            .await
            .expect("should succeed");

        assert_eq!(result, json!({"result": "ok"}));

        // Verify the sandbox received the correct tool name and params.
        let last = sandbox.last_call.lock().await;
        let (name, params) = last.as_ref().expect("sandbox should have been called");
        assert_eq!(name, "echo");
        assert_eq!(params, &json!({"text": "hi"}));
    }

    #[tokio::test]
    async fn tool_error_maps_to_execution_failed() {
        let sandbox = Arc::new(MockSandbox::new(
            MockResponse::ToolError("division by zero".to_string()),
        ));
        let executor = SandboxedExecutor::new(sandbox);

        let err = executor
            .execute(&DummyTool, "calc", &json!({}), &ctx())
            .await
            .expect_err("tool error should propagate");

        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("division by zero"));
    }

    #[tokio::test]
    async fn ipc_error_maps_with_prefix() {
        let sandbox = Arc::new(MockSandbox::new(
            MockResponse::IpcError("pipe broken".to_string()),
        ));
        let executor = SandboxedExecutor::new(sandbox);

        let err = executor
            .execute(&DummyTool, "shell", &json!({}), &ctx())
            .await
            .expect_err("IPC error should propagate");

        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(
            err.to_string().contains("sandbox communication error: pipe broken"),
            "expected prefixed message, got: {err}",
        );
    }

    #[tokio::test]
    async fn other_sandbox_error_maps_to_execution_failed() {
        let sandbox = Arc::new(MockSandbox::new(
            MockResponse::Unavailable("bwrap not installed".to_string()),
        ));
        let executor = SandboxedExecutor::new(sandbox);

        let err = executor
            .execute(&DummyTool, "shell", &json!({}), &ctx())
            .await
            .expect_err("unavailable error should propagate");

        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("bwrap not installed"));
    }
}
