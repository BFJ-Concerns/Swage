//! PythonExecuteTool — the `python_execute` tool backed by sandboxed child workers.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use swage_core::ToolError;
use swage_python::{PythonWorkerRuntime, WorkerPool};
use swage_storage::store::Storage;
use swage_tool::context::{ToolContext, serialise_tool_context};

use super::{Tool, ToolDispatcher};
use crate::tool::nested_tool_runtime::NestedToolRuntime;

pub struct PythonExecuteTool {
    runtime: Arc<PythonWorkerRuntime>,
    pool: Arc<WorkerPool>,
    sub_dispatcher: Arc<ToolDispatcher>,
    storage: Arc<dyn Storage>,
}

impl PythonExecuteTool {
    pub fn new(
        runtime: Arc<PythonWorkerRuntime>,
        pool: Arc<WorkerPool>,
        sub_dispatcher: Arc<ToolDispatcher>,
        storage: Arc<dyn Storage>,
    ) -> Self {
        Self {
            runtime,
            pool,
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
        "Execute Python code with access to Swage tool functions. Use structured tool results, for example swage.shell(\"echo hi\")[\"output\"]."
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

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let code = params
            .get("code")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: 'code'".to_owned(),
            })?
            .to_string();

        let turn_id = ctx.turn_id().ok_or_else(|| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message: "python_execute requires a turn ID".to_string(),
        })?;
        let parent_call_id = ctx.call_id().ok_or_else(|| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message: "python_execute requires a tool call ID".to_string(),
        })?;
        let gateway_ctx = super::require_gateway_context(
            ctx,
            self.name(),
            "python_execute requires gateway execution context",
        )?;

        let nested_runtime = NestedToolRuntime::new(
            Arc::clone(&self.sub_dispatcher),
            gateway_ctx.ws_tx().clone(),
            Arc::clone(&self.storage),
            turn_id,
            parent_call_id.to_string(),
            serialise_tool_context(ctx),
        );

        let mut lease = self
            .pool
            .acquire()
            .await
            .map_err(|error| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: error.to_string(),
            })?;

        let result = self
            .runtime
            .execute_python(
                lease.worker_mut(),
                &code,
                ctx.working_directory(),
                &nested_runtime,
            )
            .await;
        lease.mark_spent().await;

        let result = result.map_err(|error| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message: error.to_string(),
        })?;

        if let Some(traceback) = result.traceback {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: traceback,
            });
        }

        Ok(serde_json::json!({ "output": result.stdout }))
    }
}
