//! Granted authoring surface for durable task definitions.
use crate::{state::AppState, tool::Tool, tool_executor::GatewayToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::{GatewayError, ToolError};
use swage_tool::context::ToolContext;

pub struct TaskDefinitionTool;
const NAME: &str = "task_definition";
fn state<'a>(ctx: &'a dyn ToolContext) -> Result<&'a AppState, ToolError> {
    ctx.as_any()
        .downcast_ref::<GatewayToolContext>()
        .and_then(GatewayToolContext::app_state)
        .ok_or_else(|| ToolError::ExecutionFailed {
            name: NAME.into(),
            message: "task-definition configuration runs only in a gateway host turn".into(),
        })
}
fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: NAME.into(),
        message: message.into(),
    }
}
fn map(error: GatewayError) -> ToolError {
    match error {
        GatewayError::ValidationError(message) => invalid(message),
        other => ToolError::ExecutionFailed {
            name: NAME.into(),
            message: other.to_string(),
        },
    }
}

#[async_trait]
impl Tool for TaskDefinitionTool {
    fn name(&self) -> &str {
        NAME
    }
    fn description(&self) -> &str {
        "Create, list, update, or delete durable named task definitions. Changes are whole-definition configuration mutations and are audited."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["upsert","list","delete"]},"id":{"type":"string"},"instructions":{"type":"string"},"model_id":{"type":["string","null"]},"tool_permissions":{"type":["object","null"]},"output_schema":{"type":["object","null"]},"output_max_retries":{"type":["integer","null"],"minimum":0}},"required":["action"]})
    }
    fn requires_device_execution(&self) -> bool {
        true
    }
    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let gateway = state(ctx)?;
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("'action' is required"))?;
        match action {
            "list" => gateway
                .storage
                .list_task_definitions()
                .await
                .map(|definitions| json!({"definitions":definitions}))
                .map_err(|e| ToolError::ExecutionFailed {
                    name: NAME.into(),
                    message: e.to_string(),
                }),
            "upsert" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("'id' is required"))?
                    .to_owned();
                let instructions = params
                    .get("instructions")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("'instructions' is required"))?
                    .to_owned();
                let definition = swage_storage::NewTaskDefinition {
                    id,
                    instructions,
                    model_id: params
                        .get("model_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    tool_permissions: params
                        .get("tool_permissions")
                        .filter(|v| !v.is_null())
                        .cloned(),
                    output_schema: params
                        .get("output_schema")
                        .filter(|v| !v.is_null())
                        .cloned(),
                    output_max_retries: params
                        .get("output_max_retries")
                        .and_then(Value::as_u64)
                        .map(|v| v as u32),
                };
                crate::service::upsert_task_definition(
                    gateway,
                    definition,
                    "agent_tool",
                    ctx.session_id().parse().ok(),
                )
                .await
                .map(|v| json!({"id":v.id,"summary":v.summary}))
                .map_err(map)
            }
            "delete" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("'id' is required"))?;
                crate::service::delete_task_definition(
                    gateway,
                    id,
                    "agent_tool",
                    ctx.session_id().parse().ok(),
                )
                .await
                .map(|v| json!({"id":v.id,"summary":v.summary}))
                .map_err(map)
            }
            other => Err(invalid(format!("unknown action '{other}'"))),
        }
    }
}
