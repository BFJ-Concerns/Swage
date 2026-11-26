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

fn field<T: serde::de::DeserializeOwned>(
    params: &Value,
    key: &str,
    current: Option<T>,
) -> Result<Option<T>, ToolError> {
    match params.get(key) {
        None => Ok(current),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| invalid(format!("'{key}': {error}"))),
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
         (store the definition `id` names: with nothing stored under that id the call creates \
         it, and `instructions` is required; with one stored it changes only the fields the call \
         gives — a field you leave out keeps its stored value, a field you give replaces it, and \
         setting `model_id`, `tool_permissions`, `output_schema` or `output_max_retries` to null \
         clears that field), `list` (every stored definition, with all its fields), or `delete`.\n\n\
         Reach for this when the person wants a repeatable contract for work handed to a child \
         turn, or wants one changed. A task definition has no entry in the new-session picker and \
         no name people choose; for an agent a person can pick when starting a session — with a \
         display name, a description and standing instructions of its own — use \
         `agent_definition` instead.\n\n\
         An upsert's reply says whether it created the definition or updated it. Every upsert and \
         delete is written to the audit trail with the definition's whole state before and \
         after. Deleting is permanent (the definition can be recreated); a delegation that names \
         a deleted definition is refused. `tool_permissions` can only narrow what the delegating \
         context grants, never widen it. A `model_id` must be one the gateway's model catalogue \
         configures; a refusal names the configured ids."
    }
    fn parameters(&self) -> Value {
        let string_array = || json!({"type":"array","items":{"type":"string"}});
        json!({
            "type":"object",
            "properties":{
                "action":{
                    "type":"string",
                    "enum":["upsert","list","delete"],
                    "description":"What to do: `upsert` creates the definition `id` names or, when one is already stored, changes the fields given and leaves the rest as stored (to change part of a text, `list` first and give the whole revised text, since a text given replaces all of the stored one); `list` returns every stored definition; `delete` removes one."
                },
                "id":{
                    "type":"string",
                    "description":"The definition's stable identifier — lowercase ASCII letters, digits and hyphens — that a delegation names to run under it. Required for upsert and delete. An upsert under an id with nothing stored creates a new definition, so to change an existing one give its id exactly as `list` shows it."
                },
                "instructions":{
                    "type":"string",
                    "description":"The complete contract the child turn works to, as it will read it. Required when the upsert creates the definition. On a change, omit it to keep the stored text; a text given replaces the whole stored text, so include everything that should remain. It cannot be cleared: a definition always has instructions, and null or an empty text is refused."
                },
                "model_id":{
                    "type":["string","null"],
                    "description":"The id of a model in the gateway's configured catalogue the child turn uses in place of the delegating context's model. Omit on create to keep the delegating context's model; on a change, null clears the preference and omitting keeps it. An id the catalogue does not configure is refused."
                },
                "tool_permissions":{
                    "type":["object","null"],
                    "description":"Narrows the tools the child turn may call; the result intersects with what the delegating context grants and can only remove tools. Omit on create to leave the grants unchanged; on a change, null removes the narrowing, omitting keeps it, and an object given replaces the stored one whole, so give every member.",
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
                    "description":"A JSON Schema the child's final reply must satisfy; a reply that does not is retried. Omit on create for a free-form reply; on a change, null removes the contract and omitting keeps it."
                },
                "output_max_retries":{
                    "type":["integer","null"],
                    "minimum":0,
                    "maximum":u32::MAX,
                    "description":"How many times a reply failing `output_schema` is retried before the delegation fails. Omit on create for the configured default; on a change, null restores that default and omitting keeps the stored value."
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
                let _write = gateway.definition_writes.lock().await;
                let current = gateway
                    .storage
                    .get_task_definition(&id)
                    .await
                    .map_err(GatewayError::from)
                    .map_err(map)?;
                let (instructions, model_id, tool_permissions, output_schema, output_max_retries) =
                    current.map_or(Default::default(), |current| {
                        (
                            Some(current.instructions),
                            current.model_id,
                            current.tool_permissions,
                            current.output_schema,
                            current.output_max_retries,
                        )
                    });
                let definition = swage_storage::NewTaskDefinition {
                    id,
                    instructions: field(&params, "instructions", instructions)?.ok_or_else(
                        || invalid("'instructions' is required and cannot be cleared"),
                    )?,
                    model_id: field(&params, "model_id", model_id)?,
                    tool_permissions: field(&params, "tool_permissions", tool_permissions)?,
                    output_schema: field(&params, "output_schema", output_schema)?,
                    output_max_retries: field(&params, "output_max_retries", output_max_retries)?,
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
                let _write = gateway.definition_writes.lock().await;
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
        let mut config = swage_config::ResolvedConfig::default();
        config.models.entries.insert(
            "gpt-5.6".into(),
            swage_config::model::ModelEndpointConfig {
                provider_id: "test".into(),
                model_id: "gpt-5.6".into(),
                aliases: vec![],
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: Default::default(),
            },
        );
        let state = AppState::for_test(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            config,
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
    async fn tool_validates_and_audits_partial_definition_mutations() {
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
        assert_eq!(edited.metadata["new_state"]["model_id"], "gpt-5.6");
        assert_eq!(
            edited.metadata["new_state"]["tool_permissions"],
            edited.metadata["prior_state"]["tool_permissions"]
        );
        assert_eq!(
            edited.metadata["new_state"]["output_schema"],
            edited.metadata["prior_state"]["output_schema"]
        );
        assert_eq!(
            edited.metadata["new_state"]["output_max_retries"],
            edited.metadata["prior_state"]["output_max_retries"]
        );

        let invalid_model = tool
            .execute(
                json!({"action":"upsert","id":"review","model_id":"not-configured"}),
                &context,
            )
            .await
            .expect_err("an unknown model is refused");
        assert!(
            matches!(&invalid_model, ToolError::InvalidParameters { message, .. }
                if message.contains("not-configured") && message.contains("gpt-5.6"))
        );

        let invalid_instructions = tool
            .execute(
                json!({"action":"upsert","id":"review","instructions":null}),
                &context,
            )
            .await
            .expect_err("instructions cannot be cleared");
        assert!(matches!(
            invalid_instructions,
            ToolError::InvalidParameters { .. }
        ));
        assert_eq!(
            storage
                .get_task_definition("review")
                .await
                .unwrap()
                .unwrap()
                .instructions,
            "Edited contract."
        );

        tool.execute(
            json!({
                "action":"upsert", "id":"review", "model_id":null,
                "tool_permissions":null, "output_schema":null, "output_max_retries":null
            }),
            &context,
        )
        .await
        .expect("clear optional fields without restating instructions");

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
