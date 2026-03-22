//! PythonExecuteTool — the `python_execute` tool for Programmatic Tool Calling.
//!
//! Bridges the gateway's `Tool` trait to `swage_python::execute::execute_code`.
//! When the model invokes `python_execute`, agent-generated Python code runs
//! in an embedded interpreter with access to all registered tools via the
//! `swage` namespace.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

use swage_core::ToolError;
use swage_python::runtime::PythonRuntime;
use swage_storage::store::Storage;
use swage_tool::context::ToolContext;

use super::{Tool, ToolDispatcher};
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
/// Registered in `build_tool_dispatcher` only when a Python runtime is
/// available. If the runtime isn't found at startup, this tool is simply
/// absent from the model's tool list (graceful degradation).
pub struct PythonExecuteTool {
    /// The embedded Python interpreter.
    runtime: Arc<PythonRuntime>,
    /// Wall-clock timeout for Python execution.
    timeout: Duration,
    /// Shared dispatch pipeline — the GatewayToolDispatcher uses this to
    /// route tool calls from Python code (e.g. `swage.shell()`) through
    /// the full permission → rate-limit → validate → execute → leak-detect
    /// pipeline. Intentionally excludes `python_execute` itself to prevent
    /// infinite recursion.
    sub_dispatcher: Arc<ToolDispatcher>,
    /// Shared storage for persisting sub-tool call records.
    storage: Arc<dyn Storage>,
}

impl PythonExecuteTool {
    pub fn new(
        runtime: Arc<PythonRuntime>,
        timeout: Duration,
        sub_dispatcher: Arc<ToolDispatcher>,
        storage: Arc<dyn Storage>,
    ) -> Self {
        Self {
            runtime,
            timeout,
            sub_dispatcher,
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

    fn parameters(&self) -> Value {
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

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Extract the code argument.
        let code = params
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: 'code'".to_owned(),
            })?
            .to_string();

        // Check runtime availability.
        if !self.runtime.is_available() {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: "Python runtime not available".to_owned(),
            });
        }

        // Snapshot the working directory for the dispatcher before the
        // PYTHON_CONTEXT closure moves ownership.
        let working_dir: PathBuf = ctx.working_directory().to_path_buf();

        // Read the per-invocation context from the task-local.
        // If it's not set (shouldn't happen in normal flow), fall back to
        // a no-op dispatcher that can't emit WebSocket events or persist.
        let dispatcher: Arc<dyn swage_python::dispatch::ToolDispatcher> =
            match PYTHON_CONTEXT.try_with(|py_ctx| {
                Arc::new(GatewayToolDispatcher::new(
                    Arc::clone(&self.sub_dispatcher),
                    tokio::runtime::Handle::current(),
                    py_ctx.ws_tx.clone(),
                    Arc::clone(&self.storage),
                    py_ctx.turn_id,
                    py_ctx.call_id.clone(),
                    working_dir.clone(),
                )) as Arc<dyn swage_python::dispatch::ToolDispatcher>
            }) {
                Ok(d) => d,
                Err(_) => {
                    warn!("python_execute invoked without PYTHON_CONTEXT — sub-tool calls will not emit events");
                    // Fall back to a stub that still dispatches tools but
                    // without WebSocket events or storage persistence.
                    Arc::new(FallbackDispatcher {
                        sub_dispatcher: Arc::clone(&self.sub_dispatcher),
                        working_directory: working_dir,
                    }) as Arc<dyn swage_python::dispatch::ToolDispatcher>
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

        if result.timed_out {
            Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: result.output,
            })
        } else {
            Ok(serde_json::json!({"output": result.output}))
        }
    }
}

// ---------------------------------------------------------------------------
// Fallback dispatcher (no WebSocket / storage)
// ---------------------------------------------------------------------------

/// Minimal dispatcher used when the task-local context is missing.
///
/// Routes tool calls through the sub-dispatcher but cannot emit WebSocket
/// events or persist call records. Only used as a safety fallback.
struct FallbackDispatcher {
    sub_dispatcher: Arc<ToolDispatcher>,
    /// Working directory forwarded to tools that derive paths from context.
    working_directory: PathBuf,
}

impl swage_python::dispatch::ToolDispatcher for FallbackDispatcher {
    fn dispatch(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> swage_python::dispatch::DispatchResult {
        let working_directory = self.working_directory.clone();
        let sub_dispatcher = Arc::clone(&self.sub_dispatcher);
        let tool_name_owned = tool_name.to_string();

        // Bridge async → sync. Use handle.spawn() since this runs on a
        // plain OS thread (from execute_code) without a tokio context.
        let handle = tokio::runtime::Handle::current();
        let exec = handle
            .block_on(handle.spawn(async move {
                let ctx = swage_tool::context::MockToolContext {
                    agent_id: "python-fallback".to_string(),
                    session_id: String::new(),
                    working_directory,
                    allowed_paths: vec![],
                    user_info: None,
                };
                sub_dispatcher
                    .dispatch(&tool_name_owned, arguments, &ctx, "python-fallback")
                    .await
            }))
            .expect("fallback dispatch should not panic");

        match exec {
            Ok(tool_output) => swage_python::dispatch::DispatchResult {
                content: tool_output.result["output"].as_str().unwrap_or("").to_string(),
                is_error: false,
            },
            Err(e) => swage_python::dispatch::DispatchResult {
                content: e.to_string(),
                is_error: true,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use swage_python::runtime::PythonRuntime;
    use swage_storage::SqliteStorage;
    use swage_tool::context::MockToolContext;
    use swage_tool::dispatch::ToolDispatcher as SwageToolDispatcher;

    fn make_tool() -> PythonExecuteTool {
        let runtime = Arc::new(
            PythonRuntime::new(None).expect("system Python should be available in dev"),
        );
        // Empty sub-dispatcher — tests that execute Python don't call gateway tools.
        let sub_dispatcher = Arc::new(SwageToolDispatcher::allow_all(vec![]));
        let storage =
            Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;

        PythonExecuteTool::new(runtime, Duration::from_secs(5), sub_dispatcher, storage)
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
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
            .execute(serde_json::json!({ "code": "print('hello from python')" }), &ctx())
            .await
            .unwrap();
        assert!(
            result["output"].as_str().unwrap().contains("hello from python"),
            "expected Python output, got: {}",
            result["output"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_missing_code_argument() {
        let tool = make_tool();
        let err = tool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("Missing"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_syntax_error_returns_traceback() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "code": "def" }), &ctx())
            .await
            .unwrap();
        assert!(
            result["output"].as_str().unwrap().contains("SyntaxError"),
            "expected SyntaxError, got: {}",
            result["output"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_runtime_error_returns_traceback() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "code": "1/0" }), &ctx())
            .await
            .unwrap();
        assert!(
            result["output"].as_str().unwrap().contains("ZeroDivisionError"),
            "expected ZeroDivisionError, got: {}",
            result["output"]
        );
    }
}
