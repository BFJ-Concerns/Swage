use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_protocol::client::workflow::WorkflowBudget;
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{
    tool::Tool,
    tool_executor::GatewayToolContext,
    workflow::{GatewayWorkflowLauncher, WorkflowLauncher},
};

pub struct WorkflowRunTool;

#[async_trait]
impl Tool for WorkflowRunTool {
    fn name(&self) -> &str {
        "workflow_run"
    }
    fn description(&self) -> &str {
        "Start an available workflow by name. Workflows enforce their own agent and delivery capabilities; use wait=true only when this turn needs the final result."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The available workflow's name in this space."
                },
                "args": {
                    "description": "Arguments handed to the script as `args`; any JSON value the workflow expects."
                },
                "budget": {
                    "type": "object",
                    "description": "Optional run limits.",
                    "properties": {
                        "max_agent_calls": {"type": "integer", "description": "Most agent calls the run may make."},
                        "max_total_tokens": {"type": "integer", "description": "Token ceiling across all the run's agent calls."},
                        "reserved_closing_tokens": {"type": "integer", "description": "Tokens held back from ordinary work and released when the run enters wind-down."},
                        "max_concurrent_agent_calls": {"type": "integer", "description": "Most agent calls in flight at once."}
                    }
                },
                "wait": {
                    "type": "boolean",
                    "default": false,
                    "description": "Wait for the run to finish and return its final output. Leave false to start it detached and get the run reference."
                }
            },
            "required": ["name"]
        })
    }
    fn requires_device_execution(&self) -> bool {
        true
    }
    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidParameters {
                name: self.name().into(),
                message: "missing required field: name".into(),
            }
        })?;
        let args = params.get("args").cloned().unwrap_or(Value::Null);
        let budget = params
            .get("budget")
            .cloned()
            .map(serde_json::from_value::<WorkflowBudget>)
            .transpose()
            .map_err(|error| ToolError::InvalidParameters {
                name: self.name().into(),
                message: error.to_string(),
            })?;
        let wait = params.get("wait").and_then(Value::as_bool).unwrap_or(false);
        let session_id =
            Uuid::parse_str(ctx.session_id()).map_err(|error| ToolError::InvalidParameters {
                name: self.name().into(),
                message: error.to_string(),
            })?;
        let gateway = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: "workflow_run requires gateway execution".into(),
            })?;
        let state = gateway
            .app_state()
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: "workflow gateway state is unavailable".into(),
            })?;
        GatewayWorkflowLauncher::new(state.clone())
            .start_by_name(session_id, name, args, budget, wait)
            .await
            .map_err(|message| ToolError::ExecutionFailed {
                name: self.name().into(),
                message,
            })
    }
}
