use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::{GatewayError, ToolError};
use swage_tool::context::ToolContext;

use crate::state::AppState;
use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;

const TOOL: &str = "session_values";

pub struct SessionVariableTool;

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: TOOL.into(),
        message: message.into(),
    }
}
fn state(ctx: &dyn ToolContext) -> Result<&AppState, ToolError> {
    super::require_gateway_state(
        ctx,
        TOOL,
        "session values can be changed only inside a gateway host turn",
    )
}
fn service_error(error: GatewayError) -> ToolError {
    match error {
        GatewayError::ValidationError(message) => invalid(message),
        other => ToolError::ExecutionFailed {
            name: TOOL.into(),
            message: other.to_string(),
        },
    }
}

#[async_trait]
impl Tool for SessionVariableTool {
    fn name(&self) -> &str {
        TOOL
    }
    fn description(&self) -> &str {
        "Save, replace, or remove named values for this session. Saved values remain available across turns and can fill {{name}} slots when context elements are rendered. Use set to create or replace a value, or remove to delete one."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["set","remove"]},
            "name":{"type":"string","description":"A lowercase name containing letters, digits, or hyphens."},
            "value":{"type":"string","description":"Opaque text to save. Required for set; ignored for remove."}
        },"required":["action","name"]})
    }
    fn requires_device_execution(&self) -> bool {
        true
    }
    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let state = state(ctx)?;
        let session_id = Uuid::parse_str(ctx.session_id())
            .map_err(|_| invalid("the authoring session id is invalid"))?;
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("'action' is required"))?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("'name' is required"))?;
        let summary = match action {
            "set" => {
                let value = params
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("'value' is required for set"))?;
                crate::service::set_session_variable(state, session_id, name, value, TOOL).await
            }
            "remove" => {
                crate::service::delete_session_variable(state, session_id, name, TOOL).await
            }
            other => {
                return Err(invalid(format!(
                    "unknown action '{other}': use set or remove"
                )));
            }
        }
        .map_err(service_error)?;
        Ok(json!({"summary": summary}))
    }
}
