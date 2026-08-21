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
        let string_array = || json!({"type":"array","items":{"type":"string"}});
        json!({
            "type":"object",
            "properties":{
                "action":{"type":"string","enum":["upsert","list","delete"]},
                "id":{"type":"string"},
                "instructions":{"type":"string"},
                "model_id":{"type":["string","null"]},
                "tool_permissions":{
                    "type":["object","null"],
                    "properties":{
                        "allow":string_array(),
                        "deny":string_array(),
                        "bundles":{
                            "type":"object",
                            "additionalProperties":{"type":"array","items":{"type":"string"}}
                        },
                        "require_approval":{
                            "type":"array",
                            "items":{
                                "oneOf":[
                                    {"type":"string"},
                                    {
                                        "type":"object",
                                        "properties":{
                                            "tool":{"type":"string"},
                                            "timeout":{"type":["integer","null"],"minimum":0},
                                            "timeout_policy":{"type":["string","null"],"enum":["deny","allow",null]}
                                        },
                                        "required":["tool"],
                                        "additionalProperties":false
                                    }
                                ]
                            }
                        }
                    },
                    "required":["allow","deny","bundles","require_approval"],
                    "additionalProperties":false
                },
                "output_schema":{"type":["object","null"]},
                "output_max_retries":{"type":["integer","null"],"minimum":0}
            },
            "required":["action"],
            "additionalProperties":false
        })
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_provider::LlmProvider;
    use swage_storage::{AuditStore, SpaceStore, SqliteStorage, TaskDefinitionStore};
    use swage_tool::context::MockToolContext;
    use tokio::sync::mpsc;

    use super::*;
    use crate::turn::tests::MockProvider;

    fn context() -> (GatewayToolContext, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let base = MockToolContext::new("author", "not-a-session");
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let context = GatewayToolContext::new(
            &base,
            ws_tx,
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
        )
        .with_app_state(Some(state));
        (context, storage)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_validates_and_audits_whole_definition_mutations() {
        let (context, storage) = context();
        let tool = TaskDefinitionTool;

        let invalid = tool
            .execute(
                json!({"action":"upsert","id":"Bad Id","instructions":"Contract."}),
                &context,
            )
            .await
            .expect_err("invalid identifiers must be rejected");
        assert!(matches!(invalid, ToolError::InvalidParameters { .. }));
        assert!(storage.list_task_definitions().await.unwrap().is_empty());

        tool.execute(
            json!({
                "action":"upsert",
                "id":"review",
                "instructions":"First contract.",
                "tool_permissions":{
                    "allow":["file_read"],
                    "deny":[],
                    "bundles":{},
                    "require_approval":[]
                },
                "output_schema":{"type":"object"},
                "output_max_retries":2
            }),
            &context,
        )
        .await
        .expect("create");
        tool.execute(
            json!({
                "action":"upsert",
                "id":"review",
                "instructions":"Edited contract.",
                "output_schema":{"type":"string"},
                "output_max_retries":1
            }),
            &context,
        )
        .await
        .expect("update");

        let listed = tool
            .execute(json!({"action":"list"}), &context)
            .await
            .expect("list");
        assert_eq!(listed["definitions"][0]["instructions"], "Edited contract.");
        let changes: Vec<_> = storage
            .list_audit_events()
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.entity_kind == "task_definition")
            .collect();
        assert_eq!(changes.len(), 2);
        let edited = changes
            .iter()
            .find(|event| event.metadata["prior_state"]["instructions"] == "First contract.")
            .expect("update audit");
        assert_eq!(
            edited.metadata["new_state"]["instructions"],
            "Edited contract."
        );

        tool.execute(json!({"action":"delete","id":"review"}), &context)
            .await
            .expect("delete");
        assert!(
            storage
                .get_task_definition("review")
                .await
                .unwrap()
                .is_none()
        );
        let deleted = storage
            .list_audit_events()
            .await
            .unwrap()
            .into_iter()
            .find(|event| event.metadata["action"] == "delete_task_definition")
            .expect("delete audit");
        assert_eq!(
            deleted.metadata["prior_state"]["instructions"],
            "Edited contract."
        );
        assert!(deleted.metadata["new_state"].is_null());
    }
}
