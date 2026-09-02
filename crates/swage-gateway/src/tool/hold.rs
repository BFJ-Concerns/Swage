use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_protocol::client::workflow::AnswerWorkflowRunHoldRequest;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;
use crate::workflow::{RunEventSource, answer_run_hold, hold_projection};

pub struct WorkflowHoldTool;

const TOOL: &str = "workflow_hold";

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: TOOL.into(),
        message: message.into(),
    }
}

fn failed(message: impl Into<String>) -> ToolError {
    ToolError::ExecutionFailed {
        name: TOOL.into(),
        message: message.into(),
    }
}

#[async_trait]
impl Tool for WorkflowHoldTool {
    fn name(&self) -> &str {
        TOOL
    }

    fn description(&self) -> &str {
        "Read and answer the decision holds workflow runs are waiting on. Use action=list to see \
         every open hold with its question and the verdicts it accepts, and action=answer with a \
         hold and one of that hold's verdicts to release the waiting run. An answer outside the \
         hold's accepted set is refused with feedback and the hold keeps waiting."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "answer"]},
                "hold_id": {"type": "string", "description": "The hold to answer (action=answer)."},
                "verdict": {
                    "type": "string",
                    "description": "One of the hold's accepted verdicts (action=answer)."
                }
            },
            "required": ["action"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("'action' is required"))?;
        let state = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .and_then(GatewayToolContext::app_state)
            .ok_or_else(|| failed("this tool runs only inside a gateway host turn"))?;
        let session_id = Uuid::parse_str(ctx.session_id())
            .map_err(|_| failed("the calling session id is invalid"))?;

        match action {
            "list" => {
                let session = state
                    .storage
                    .get_session(session_id)
                    .await
                    .map_err(|error| failed(error.to_string()))?
                    .ok_or_else(|| failed("the calling session no longer exists"))?;
                let holds = state
                    .storage
                    .list_open_workflow_run_holds(session.space_id)
                    .await
                    .map_err(|error| failed(error.to_string()))?;
                let mut projected = Vec::with_capacity(holds.len());
                for hold in holds {
                    projected.push(
                        hold_projection(state, hold)
                            .await
                            .map_err(|error| failed(error.message().to_owned()))?,
                    );
                }
                serde_json::to_value(json!({"holds": projected}))
                    .map_err(|error| failed(error.to_string()))
            }
            "answer" => {
                let hold_id = params
                    .get("hold_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("'hold_id' is required"))?;
                let hold_id =
                    Uuid::parse_str(hold_id).map_err(|_| invalid("'hold_id' must be a UUID"))?;
                let verdict = params
                    .get("verdict")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("'verdict' is required"))?
                    .to_owned();
                let response = answer_run_hold(
                    state,
                    AnswerWorkflowRunHoldRequest { hold_id, verdict },
                    RunEventSource {
                        surface: "agent",
                        agent_id: Some(ctx.agent_id().to_owned()),
                        session_id: Some(session_id),
                        turn_id: ctx.turn_id(),
                    },
                )
                .await
                .map_err(|error| failed(error.message().to_owned()))?;
                serde_json::to_value(response).map_err(|error| failed(error.to_string()))
            }
            other => Err(invalid(format!(
                "'{other}' is not an action; use 'list' or 'answer'"
            ))),
        }
    }
}
