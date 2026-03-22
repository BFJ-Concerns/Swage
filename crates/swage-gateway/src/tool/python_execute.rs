//! PythonExecuteTool — the `python_execute` tool for Programmatic Tool Calling.
//!
//! Bridges the gateway's `Tool` trait to `swage_python::execute::execute_code`.
//! When the model invokes `python_execute`, agent-generated Python code runs
//! in an embedded interpreter with access to all registered tools via the
//! `swage` namespace.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

use swage_python::runtime::PythonRuntime;
use swage_storage::store::Storage;

use super::{Tool, ToolExecutionResult, ToolRegistry};
use crate::tool::python_dispatch::GatewayToolDispatcher;
use crate::websocket::ServerEvent;

// ---------------------------------------------------------------------------
// Per-invocation context
// ---------------------------------------------------------------------------

/// Context that the turn executor provides for each `python_execute` call.
///
/// This carries per-invocation data (ws_tx, turn_id, call_id) that the Tool
/// trait's `execute()` method doesn't receive. The turn executor sets this
/// via the `PYTHON_CONTEXT` task-local before calling `execute()`.
pub struct PythonExecuteContext {
    /// WebSocket sender for sub-tool lifecycle events.
    pub ws_tx: mpsc::UnboundedSender<ServerEvent>,
    /// Turn that this execution belongs to.
    pub turn_id: Uuid,
    /// call_id of this `python_execute` invocation (becomes parent_call_id
    /// for sub-tool calls).
    pub call_id: String,
}

tokio::task_local! {
    /// Per-invocation context for PythonExecuteTool.
    ///
    /// Set by `run_turn` in turn.rs before each `python_execute` invocation.
    /// Read by `PythonExecuteTool::execute()` to create the
    /// `GatewayToolDispatcher` with the correct turn and WebSocket context.
    pub static PYTHON_CONTEXT: PythonExecuteContext;
}

// ---------------------------------------------------------------------------
// PythonExecuteTool
// ---------------------------------------------------------------------------

/// Executes agent-generated Python code with access to Swage's built-in tools.
///
/// Registered in `build_tool_registry` only when a Python runtime is
/// available. If the runtime isn't found at startup, this tool is simply
/// absent from the model's tool list (graceful degradation).
pub struct PythonExecuteTool {
    /// The embedded Python interpreter.
    runtime: Arc<PythonRuntime>,
    /// Wall-clock timeout for Python execution.
    timeout: Duration,
    /// Shared tool registry — the GatewayToolDispatcher uses this to look
    /// up tools that Python code calls via `swage.shell()` etc.
    tool_registry: Arc<ToolRegistry>,
    /// Shared storage for persisting sub-tool call records.
    storage: Arc<dyn Storage>,
}

impl PythonExecuteTool {
    pub fn new(
        runtime: Arc<PythonRuntime>,
        timeout: Duration,
        tool_registry: Arc<ToolRegistry>,
        storage: Arc<dyn Storage>,
    ) -> Self {
        Self {
            runtime,
            timeout,
            tool_registry,
            storage,
        }
    }
}

#[async_trait]
impl Tool for PythonExecuteTool {
    fn name(&self) -> &str {
        "python_execute"
    }

    fn description(&self) -> &str {
        "Execute Python code with access to Swage tool functions. \
         Use this when a task benefits from chaining multiple tool calls \
         with variables, loops, or conditionals."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Python source code to execute."
                }
            },
            "required": ["code"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        // Extract the code argument.
        let code = match arguments.get("code").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return ToolExecutionResult {
                    content: "Missing required argument: 'code'".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        // Check runtime availability.
        if !self.runtime.is_available() {
            return ToolExecutionResult {
                content: "Python runtime not available".into(),
                is_error: true,
                truncated: false,
                total_bytes: None,
            };
        }

        // Read the per-invocation context from the task-local.
        // If it's not set (shouldn't happen in normal flow), fall back to
        // a no-op dispatcher that can't emit WebSocket events or persist.
        let dispatcher: Arc<dyn swage_python::dispatch::ToolDispatcher> =
            match PYTHON_CONTEXT.try_with(|ctx| {
                Arc::new(GatewayToolDispatcher::new(
                    Arc::clone(&self.tool_registry),
                    tokio::runtime::Handle::current(),
                    ctx.ws_tx.clone(),
                    Arc::clone(&self.storage),
                    ctx.turn_id,
                    ctx.call_id.clone(),
                )) as Arc<dyn swage_python::dispatch::ToolDispatcher>
            }) {
                Ok(d) => d,
                Err(_) => {
                    warn!("python_execute invoked without PYTHON_CONTEXT — sub-tool calls will not emit events");
                    // Fall back to a stub that still dispatches tools but
                    // without WebSocket events or storage persistence.
                    Arc::new(FallbackDispatcher {
                        registry: Arc::clone(&self.tool_registry),
                    })
                }
            };

        // Run the Python code on a blocking thread. execute_code internally
        // spawns its own thread, but we wrap it in spawn_blocking so we
        // don't block the tokio worker.
        let timeout = self.timeout;
        let result = tokio::task::spawn_blocking(move || {
            swage_python::execute::execute_code(&code, timeout, dispatcher)
        })
        .await
        .unwrap_or_else(|e| swage_python::execute::ExecutionResult {
            output: format!("Internal error: Python execution task panicked: {e}"),
            timed_out: false,
        });

        ToolExecutionResult {
            content: result.output,
            is_error: result.timed_out,
            truncated: false,
            total_bytes: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Fallback dispatcher (no WebSocket / storage)
// ---------------------------------------------------------------------------

/// Minimal dispatcher used when the task-local context is missing.
///
/// Dispatches tool calls through the registry but cannot emit WebSocket
/// events or persist call records. Only used as a safety fallback.
struct FallbackDispatcher {
    registry: Arc<ToolRegistry>,
}

impl swage_python::dispatch::ToolDispatcher for FallbackDispatcher {
    fn dispatch(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> swage_python::dispatch::DispatchResult {
        let tool = match self.registry.get(tool_name) {
            Some(t) => t,
            None => {
                return swage_python::dispatch::DispatchResult {
                    content: format!("Tool '{tool_name}' is not registered."),
                    is_error: true,
                };
            }
        };

        // Bridge async → sync. Use handle.spawn() since this runs on a
        // plain OS thread (from execute_code) without a tokio context.
        let handle = tokio::runtime::Handle::current();
        let exec = handle
            .block_on(handle.spawn(
                async move { tool.execute(arguments).await },
            ))
            .expect("tool execution should not panic");

        swage_python::dispatch::DispatchResult {
            content: exec.content,
            is_error: exec.is_error,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolRegistry};
    use std::sync::Arc;
    use std::time::Duration;

    use swage_python::runtime::PythonRuntime;
    use swage_storage::SqliteStorage;

    fn make_tool() -> PythonExecuteTool {
        let runtime = Arc::new(
            PythonRuntime::new(None).expect("system Python should be available in dev"),
        );
        let registry = Arc::new(ToolRegistry::new());
        let storage =
            Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;

        PythonExecuteTool::new(runtime, Duration::from_secs(5), registry, storage)
    }

    #[test]
    fn tool_metadata() {
        let tool = make_tool();
        assert_eq!(tool.name(), "python_execute");
        assert!(tool.description().contains("Python"));
        let params = tool.parameters();
        assert!(params["properties"]["code"].is_object());
        assert_eq!(params["required"][0], "code");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_simple_print() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "code": "print('hello from python')" }))
            .await;
        assert!(
            result.content.contains("hello from python"),
            "expected Python output, got: {}",
            result.content
        );
        assert!(!result.is_error);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_missing_code_argument() {
        let tool = make_tool();
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("Missing"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_syntax_error_returns_traceback() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "code": "def" }))
            .await;
        assert!(
            result.content.contains("SyntaxError"),
            "expected SyntaxError, got: {}",
            result.content
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_runtime_error_returns_traceback() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "code": "1/0" }))
            .await;
        assert!(
            result.content.contains("ZeroDivisionError"),
            "expected ZeroDivisionError, got: {}",
            result.content
        );
    }
}
