//! The terminal account of a workflow run, for the agent that started it.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_protocol::client::workflow::WorkflowRunTerminalAccount;
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{state::AppState, tool::Tool, tool_executor::GatewayToolContext};

pub const WORKFLOW_RUN_ACCOUNT_TOOL: &str = "workflow_run_account";

/// Read one run's account, scoped to the caller's own space.
///
/// A run in another space reads as absent rather than forbidden: the caller has
/// no way to know it exists, so naming it would be the disclosure.
pub async fn account_for_run(
    state: &AppState,
    session_id: Uuid,
    run_id: Uuid,
) -> Result<Value, String> {
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("session {session_id} was not found"))?;
    let record = state
        .storage
        .get_workflow_run(run_id)
        .await
        .map_err(|error| error.to_string())?
        .filter(|record| record.space_id == session.space_id)
        .ok_or_else(|| format!("workflow run {run_id} was not found"))?;

    let account = record
        .terminal_account
        .map(serde_json::from_value::<WorkflowRunTerminalAccount>)
        .transpose()
        .map_err(|error| {
            format!("workflow run {run_id} terminal account does not match the recorded contract: {error}")
        })?;

    let Some(account) = account else {
        return Ok(json!({
            "run_id": run_id,
            "script_name": record.script_name,
            "state": record.state,
            "ended": false,
            "note": "This run has not ended, so it has no account yet.",
        }));
    };

    Ok(json!({
        "run_id": run_id,
        "script_name": record.script_name,
        "state": record.state,
        "ended": true,
        "account": serde_json::to_value(&account).map_err(|error| error.to_string())?,
    }))
}

pub struct WorkflowRunAccountTool;

#[async_trait]
impl Tool for WorkflowRunAccountTool {
    fn name(&self) -> &str {
        WORKFLOW_RUN_ACCOUNT_TOOL
    }

    fn description(&self) -> &str {
        "Read how a workflow run ended and where its workers stood when it did. The account names the ending (returned, threw, cancelled, wound down, or interrupted without resume) and partitions every worker the run dispatched into done, failed, cut short, and unsettled, each with the directory its work was left in. A run that ended without returning also carries the completed workers' output as partial results — recoverable work, never the run's result. Use this after a run ends, especially one that ended without a result, to find out what survived and where."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The id of the run to account for.",
                },
            },
            "required": ["run_id"],
            "additionalProperties": false,
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let invalid = |message: String| ToolError::InvalidParameters {
            name: WORKFLOW_RUN_ACCOUNT_TOOL.into(),
            message,
        };
        let run_id = params
            .get("run_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("run_id is required".into()))?;
        let run_id = Uuid::parse_str(run_id).map_err(|error| invalid(error.to_string()))?;
        let session_id =
            Uuid::parse_str(ctx.session_id()).map_err(|error| invalid(error.to_string()))?;
        let state = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .and_then(GatewayToolContext::app_state)
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: WORKFLOW_RUN_ACCOUNT_TOOL.into(),
                message: format!("{WORKFLOW_RUN_ACCOUNT_TOOL} requires gateway execution"),
            })?;
        account_for_run(state, session_id, run_id)
            .await
            .map_err(|message| ToolError::ExecutionFailed {
                name: WORKFLOW_RUN_ACCOUNT_TOOL.into(),
                message,
            })
    }
}
