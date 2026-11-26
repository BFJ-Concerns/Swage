//! Granted authoring surface for durable task definitions.
use crate::{state::AppState, tool::Tool};

#[cfg(test)]
use crate::tool_executor::GatewayToolContext;
use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::{GatewayError, ToolError};
use swage_tool::context::ToolContext;

pub struct TaskDefinitionTool;
const NAME: &str = "task_definition";
fn state(ctx: &dyn ToolContext) -> Result<&AppState, ToolError> {
    super::require_gateway_state(
        ctx,
        NAME,
        "task-definition configuration runs only in a gateway host turn",
    )
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
        "Create, list, update, or delete durable task definitions: a named work contract — \
         instructions, a preferred model, narrowed tool grants and an output contract — that a \
         delegated child turn runs under when a delegation names it. Set `action` to `upsert` \
         (create a definition, or replace one whole: an upsert stores exactly the fields given, \
         so a field left out is cleared — restate every field that should remain), `list` (every \
         stored definition), or `delete`.\n\n\
         Reach for this when the person wants a repeatable contract for work handed to a child \
         turn, or wants one changed. A task definition has no entry in the new-session picker and \
         no name people choose; for an agent a person can pick when starting a session — with a \
         display name, a description and standing instructions of its own — use \
         `agent_definition` instead.\n\n\
         Every upsert and delete is written to the audit trail with the definition's whole state \
         before and after. Deleting is permanent (the definition can be recreated); a delegation \
         that names a deleted definition is refused. `tool_permissions` can only narrow what the \
         delegating context grants, never widen it."
    }
    fn parameters(&self) -> Value {
        let string_array = || json!({"type":"array","items":{"type":"string"}});
        json!({
            "type":"object",
            "properties":{
                "action":{
                    "type":"string",
                    "enum":["upsert","list","delete"],
                    "description":"What to do: `upsert` stores the definition exactly as given, creating or replacing it whole; `list` returns every stored definition; `delete` removes one."
                },
                "id":{
                    "type":"string",
                    "description":"The definition's stable identifier — lowercase ASCII letters, digits and hyphens — that a delegation names to run under it. Required for upsert and delete."
                },
                "instructions":{
                    "type":"string",
                    "description":"The complete contract the child turn works to, as it will read it. Required on upsert; it replaces any stored text."
                },
                "model_id":{
                    "type":["string","null"],
                    "description":"The id of a model in the gateway's configured catalogue the child turn uses. Omit or null to keep the delegating context's model."
                },
                "tool_permissions":{
                    "type":["object","null"],
                    "description":"Narrows the tools the child turn may call; the result intersects with what the delegating context grants and can only remove tools. Omit or null to leave the grants unchanged.",
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
                "output_schema":{
                    "type":["object","null"],
                    "description":"A JSON Schema the child's final reply must satisfy; a reply that does not is retried. Omit or null for a free-form reply."
                },
                "output_max_retries":{
                    "type":["integer","null"],
                    "minimum":0,
                    "maximum":u32::MAX,
                    "description":"How many times a reply failing `output_schema` is retried before the delegation fails. Omit or null for the configured default."
                }
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
                let output_max_retries = params
                    .get("output_max_retries")
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        value
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .ok_or_else(|| {
                                invalid(format!(
                                    "'output_max_retries' must be an integer between 0 and {}",
                                    u32::MAX
                                ))
                            })
                    })
                    .transpose()?;
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
                    output_max_retries,
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
    use swage_storage::{AuditStore, SqliteStorage, TaskDefinitionStore, TopicStore};
    use swage_tool::context::MockToolContext;
    use tokio::sync::mpsc;

    use super::*;
    use crate::turn::tests::MockProvider;

    fn context() -> (GatewayToolContext, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::for_test(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
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

        let invalid = tool
            .execute(
                json!({
                    "action":"upsert",
                    "id":"review",
                    "instructions":"Contract.",
                    "output_max_retries":u64::from(u32::MAX) + 1
                }),
                &context,
            )
            .await
            .expect_err("retry counts above the durable contract must be rejected");
        assert!(matches!(invalid, ToolError::InvalidParameters { .. }));
        assert!(storage.list_task_definitions().await.unwrap().is_empty());
        assert!(storage.list_audit_events().await.unwrap().is_empty());

        tool.execute(
            json!({
                "action":"upsert",
                "id":"review",
                "instructions":"First contract.",
                "model_id":"gpt-5.6",
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
                "instructions":"Edited contract."
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
        assert_eq!(edited.metadata["prior_state"]["model_id"], "gpt-5.6");
        assert_eq!(
            edited.metadata["prior_state"]["tool_permissions"],
            json!({
                "allow":["file_read"],
                "deny":[],
                "bundles":{},
                "require_approval":[]
            })
        );
        assert_eq!(
            edited.metadata["prior_state"]["output_schema"],
            json!({"type":"object"})
        );
        assert_eq!(edited.metadata["prior_state"]["output_max_retries"], 2);
        assert_eq!(edited.metadata["new_state"]["model_id"], Value::Null);
        assert_eq!(
            edited.metadata["new_state"]["tool_permissions"],
            Value::Null
        );
        assert_eq!(edited.metadata["new_state"]["output_schema"], Value::Null);
        assert_eq!(
            edited.metadata["new_state"]["output_max_retries"],
            Value::Null
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
        assert_eq!(
            deleted.metadata["prior_state"]["tool_permissions"],
            Value::Null
        );
        assert_eq!(
            deleted.metadata["prior_state"]["output_schema"],
            Value::Null
        );
        assert_eq!(
            deleted.metadata["prior_state"]["output_max_retries"],
            Value::Null
        );
        assert!(deleted.metadata["new_state"].is_null());
    }
}
