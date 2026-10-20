//! Granted authoring surface for durable agent definitions — the agents a
//! person can pick when starting a session.
//!
//! The tool is a conversational front on `service::agent_definition`: every
//! write goes through that module's audited mutations, so the trail carries
//! the whole definition before and after. The tool adds what a conversation
//! needs and a whole-definition service does not: `create` refuses to
//! overwrite, `update` merges the named fields over the stored definition so
//! an omitted field is never silently cleared, and a definition's scope is
//! read from the calling session rather than taken from the model.
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use swage_core::{GatewayError, ToolError};
use swage_storage::{AgentDefinition, NewAgentDefinition};
use swage_tool::context::ToolContext;
use swage_tool::tool::ToolDisplayHints;
use uuid::Uuid;

use crate::service;
use crate::state::AppState;
use crate::tool::Tool;

pub struct AgentDefinitionTool;

pub const NAME: &str = "agent_definition";
const ACTIONS: &[&str] = &["create", "update", "delete", "list"];
const SCOPES: &[&str] = &["global", "topic"];
/// The surface named in each recorded change.
const SOURCE: &str = "agent_tool";

fn state(ctx: &dyn ToolContext) -> Result<&AppState, ToolError> {
    super::require_gateway_state(
        ctx,
        NAME,
        "agent-definition configuration runs only in a gateway host turn",
    )
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: NAME.into(),
        message: message.into(),
    }
}

fn failed(error: impl ToString) -> ToolError {
    ToolError::ExecutionFailed {
        name: NAME.into(),
        message: error.to_string(),
    }
}

fn map(error: GatewayError) -> ToolError {
    match error {
        GatewayError::ValidationError(message) => invalid(message),
        other => failed(other),
    }
}

/// How a call spoke of one field: left it out, set it to null, or gave a
/// value. `update` needs all three — an omitted field keeps its stored
/// value, a null clears an optional one — where `create` reads only the
/// last.
enum Given<T> {
    Absent,
    Cleared,
    Value(T),
}

impl<T> Given<T> {
    fn into_option(self) -> Option<T> {
        match self {
            Given::Value(value) => Some(value),
            Given::Absent | Given::Cleared => None,
        }
    }
}

fn given_string(params: &Value, key: &str) -> Result<Given<String>, ToolError> {
    match params.get(key) {
        None => Ok(Given::Absent),
        Some(Value::Null) => Ok(Given::Cleared),
        Some(Value::String(text)) => Ok(Given::Value(text.clone())),
        Some(_) => Err(invalid(format!("'{key}' must be a string"))),
    }
}

fn given_object(params: &Value, key: &str) -> Result<Given<Value>, ToolError> {
    match params.get(key) {
        None => Ok(Given::Absent),
        Some(Value::Null) => Ok(Given::Cleared),
        Some(value @ Value::Object(_)) => Ok(Given::Value(value.clone())),
        Some(_) => Err(invalid(format!("'{key}' must be an object"))),
    }
}

fn given_strings(params: &Value, key: &str) -> Result<Given<Vec<String>>, ToolError> {
    match params.get(key) {
        None => Ok(Given::Absent),
        Some(Value::Null) => Ok(Given::Cleared),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(format!("'{key}' must be an array of strings")))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Given::Value),
        Some(_) => Err(invalid(format!("'{key}' must be an array of strings"))),
    }
}

fn given_u32(params: &Value, key: &str) -> Result<Given<u32>, ToolError> {
    match params.get(key) {
        None => Ok(Given::Absent),
        Some(Value::Null) => Ok(Given::Cleared),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Given::Value)
            .ok_or_else(|| {
                invalid(format!(
                    "'{key}' must be an integer between 0 and {}",
                    u32::MAX
                ))
            }),
    }
}

/// A field `create` cannot do without.
fn required(given: Given<String>, key: &str) -> Result<String, ToolError> {
    given
        .into_option()
        .ok_or_else(|| invalid(format!("'{key}' is required to create an agent definition")))
}

/// A field `update` may replace but never clear: the stored text stands
/// unless new text is given.
fn replaced(given: Given<String>, current: String, key: &str) -> Result<String, ToolError> {
    match given {
        Given::Value(value) => Ok(value),
        Given::Absent => Ok(current),
        Given::Cleared => Err(invalid(format!(
            "'{key}' cannot be cleared; give the new text or leave the field out to keep it"
        ))),
    }
}

/// A field `update` may replace or clear.
fn revised<T>(given: Given<T>, current: Option<T>) -> Option<T> {
    match given {
        Given::Value(value) => Some(value),
        Given::Cleared => None,
        Given::Absent => current,
    }
}

fn agent_id(params: &Value) -> Result<String, ToolError> {
    params
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid("'id' is required"))
}

/// The scope a call names, resolved to a stored row's key: `None` for a
/// global definition, the calling session's topic for `topic`. The topic is
/// the session's own — a call never names another topic.
async fn scope(
    params: &Value,
    state: &AppState,
    ctx: &dyn ToolContext,
) -> Result<Option<Uuid>, ToolError> {
    match params.get("scope").and_then(Value::as_str) {
        None | Some("global") => Ok(None),
        Some("topic") => {
            let (topic_id, _, _) = super::knowledge::session_topic(NAME, state, ctx).await?;
            Ok(Some(topic_id))
        }
        Some(other) => Err(invalid(format!(
            "unknown scope '{other}'; use one of {}",
            SCOPES.join(", ")
        ))),
    }
}

fn scope_word(topic_id: Option<Uuid>) -> &'static str {
    if topic_id.is_some() {
        "topic"
    } else {
        "global"
    }
}

fn scope_phrase(topic_id: Option<Uuid>) -> &'static str {
    if topic_id.is_some() {
        "in this session's topic"
    } else {
        "globally"
    }
}

/// The definition stored at exactly this scope, if any — no shadowing: a
/// topic row and a global row with one id are two definitions.
async fn stored(
    state: &AppState,
    id: &str,
    topic_id: Option<Uuid>,
) -> Result<Option<AgentDefinition>, ToolError> {
    Ok(state
        .storage
        .list_agent_definitions(topic_id)
        .await
        .map_err(failed)?
        .into_iter()
        .find(|definition| definition.id == id))
}

fn session_id(ctx: &dyn ToolContext) -> Option<Uuid> {
    ctx.session_id().parse().ok()
}

/// A stored definition as the tool reports it: its fields with `scope` in
/// place of the row's topic id.
fn definition_json(definition: &AgentDefinition) -> Value {
    let mut fields = match serde_json::to_value(definition) {
        Ok(Value::Object(fields)) => fields,
        _ => Map::new(),
    };
    fields.remove("topic_id");
    fields.insert("scope".into(), json!(scope_word(definition.topic_id)));
    Value::Object(fields)
}

/// The change the service reports, with the definition as it now stands.
async fn changed(
    state: &AppState,
    change: service::AgentDefinitionChange,
) -> Result<Value, ToolError> {
    let definition = stored(state, &change.agent_id, change.topic_id)
        .await?
        .map(|definition| definition_json(&definition));
    Ok(json!({
        "id": change.agent_id,
        "scope": scope_word(change.topic_id),
        "summary": change.summary,
        "definition": definition,
    }))
}

#[async_trait]
impl Tool for AgentDefinitionTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Create, update, list, or delete the durable agent definitions a person can pick when \
         starting a session: a named agent with its own instructions, preferred model, tool \
         grants, skills and output contract. Set `action` to `create` (a new definition — `id`, \
         `display_name`, `description` and `instructions` are required; `model_id`, \
         `tool_permissions`, `skills`, `output_schema` and `output_max_retries` are optional), \
         `update` (change named fields of an existing definition — a field you leave out keeps \
         its stored value, and setting an optional field to null clears it), `delete`, or `list` \
         (every definition this session's topic can see, as the new-session picker shows them, \
         the shipped ones included).\n\n\
         Reach for this when the person asks for a new or changed agent, assistant, persona or \
         role they can choose for future sessions, or asks what agents exist. It does not change \
         the session you are in: a definition governs sessions started under it, and a change \
         reaches a session already running under that definition from its next turn. For a \
         reusable work contract a delegated child turn runs under — instructions and limits with \
         no entry in the picker — use `task_definition` instead.\n\n\
         A definition is stored for every topic unless `scope` is `topic`, which stores it for \
         this session's topic alone, where it hides a global definition with the same id. Every \
         create, update and delete is written to the audit trail with the definition's whole \
         state before and after, and shows at once in the agent settings and the picker. \
         Deleting is permanent (the definition can be recreated); a session already running under \
         a deleted definition falls back to the configuration's defaults. The shipped default \
         agent and the memory curator cannot be changed or deleted. `tool_permissions` can only \
         narrow what the configuration already grants, never widen it, and approval requirements \
         are set in configuration, not here. A `model_id` must be one the gateway's model \
         catalogue configures; a refusal names the configured ids."
    }

    fn display_hints(&self) -> ToolDisplayHints {
        ToolDisplayHints {
            summary_param: Some("id"),
        }
    }

    fn parameters(&self) -> Value {
        let string_array = |description: &str| json!({"type": "array", "items": {"type": "string"}, "description": description});
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ACTIONS,
                    "description": "What to do: `create` a definition that does not yet exist at the scope (refused if one does), `update` fields of one that does (refused if none does; to change part of a text, `list` first and give the whole revised text, since a text given replaces all of the stored one), `delete` one, or `list` the definitions this session's topic sees."
                },
                "id": {
                    "type": "string",
                    "description": "The definition's stable identifier — lowercase ASCII letters, digits and hyphens — used when a session is started under it and in the audit trail. Required for create, update and delete. People read `display_name`, not this; when the person names an agent and you do not know its id, `list` shows each id beside its display name."
                },
                "scope": {
                    "type": "string",
                    "enum": SCOPES,
                    "description": "Where the definition lives. `global` (the default when omitted): every topic sees it. `topic`: only this session's topic sees it, and it hides a global definition with the same id there. On update and delete, names which of the two rows to act on."
                },
                "display_name": {
                    "type": "string",
                    "description": "The name people see in the picker and the agent settings. Required on create; on update, replaces the stored name."
                },
                "description": {
                    "type": "string",
                    "description": "One or two sentences on what the agent is for, shown beside its name in the picker. Required on create; on update, replaces the stored text."
                },
                "instructions": {
                    "type": "string",
                    "description": "The agent's standing instructions, given to it after the operator's own guidance at the start of every session run under it. Write the complete text as the agent will read it. Required on create; on update, replaces the whole stored text, so include everything that should remain."
                },
                "model_id": {
                    "type": ["string", "null"],
                    "description": "The id of a model in the gateway's configured catalogue that sessions under this agent use unless a person picks another at session start. Omit on create for the configured default; on update, null clears the preference and omitting keeps it. An id the catalogue does not configure is refused."
                },
                "tool_permissions": {
                    "type": ["object", "null"],
                    "description": "Narrows the tools sessions under this agent may call. The result intersects with what the configuration grants: it can only remove tools, never add them. Omit on create to leave the configuration's grants unchanged; on update, null removes the narrowing and omitting keeps it. Approval requirements cannot be set here.",
                    "properties": {
                        "allow": string_array("Tool names, bundle names, or `*` for every tool the configuration grants."),
                        "deny": string_array("Tool names withheld even when allowed."),
                        "bundles": {
                            "type": "object",
                            "additionalProperties": {"type": "array", "items": {"type": "string"}},
                            "description": "Bundle name to the tool names it stands for; an empty object when none are defined here."
                        }
                    },
                    "required": ["allow", "deny", "bundles"],
                    "additionalProperties": false
                },
                "skills": {
                    "type": ["array", "null"],
                    "items": {"type": "string"},
                    "description": "Names of configured skills the agent activates in every session, as the skill catalogue lists them. Omit on create for none; on update, null removes them all and omitting keeps them."
                },
                "output_schema": {
                    "type": ["object", "null"],
                    "description": "A JSON Schema the agent's final reply must satisfy; a reply that does not is retried. Omit on create for free-form replies; on update, null removes the contract and omitting keeps it."
                },
                "output_max_retries": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "maximum": u32::MAX,
                    "description": "How many times a reply failing `output_schema` is retried before the turn fails. Omit on create for the configured default; on update, null restores that default and omitting keeps the stored value."
                }
            },
            "required": ["action"],
            "additionalProperties": false
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
            "list" => {
                let (topic_id, _, _) = super::knowledge::session_topic(NAME, gateway, ctx).await?;
                let definitions =
                    service::effective_agent_definitions(gateway.storage.as_ref(), topic_id)
                        .await
                        .map_err(map)?
                        .into_iter()
                        .map(|effective| {
                            let scope = if effective.is_builtin {
                                "built-in"
                            } else {
                                scope_word(effective.definition.topic_id)
                            };
                            let mut fields = match serde_json::to_value(&effective.definition) {
                                Ok(Value::Object(fields)) => fields,
                                _ => Map::new(),
                            };
                            fields.remove("topic_id");
                            fields.insert("scope".into(), json!(scope));
                            fields.insert("selectable".into(), json!(effective.selectable));
                            Value::Object(fields)
                        })
                        .collect::<Vec<_>>();
                Ok(json!({"definitions": definitions}))
            }
            "create" => {
                let id = agent_id(&params)?;
                let topic_id = scope(&params, gateway, ctx).await?;
                if stored(gateway, &id, topic_id).await?.is_some() {
                    return Err(invalid(format!(
                        "an agent definition '{id}' already exists {}; use action 'update' to change it",
                        scope_phrase(topic_id)
                    )));
                }
                let definition = NewAgentDefinition {
                    id,
                    topic_id,
                    display_name: required(given_string(&params, "display_name")?, "display_name")?,
                    description: required(given_string(&params, "description")?, "description")?,
                    instructions: required(given_string(&params, "instructions")?, "instructions")?,
                    model_id: given_string(&params, "model_id")?.into_option(),
                    tool_permissions: given_object(&params, "tool_permissions")?.into_option(),
                    skills: given_strings(&params, "skills")?
                        .into_option()
                        .unwrap_or_default(),
                    output_schema: given_object(&params, "output_schema")?.into_option(),
                    output_max_retries: given_u32(&params, "output_max_retries")?.into_option(),
                };
                let change =
                    service::upsert_agent_definition(gateway, definition, SOURCE, session_id(ctx))
                        .await
                        .map_err(map)?;
                changed(gateway, change).await
            }
            "update" => {
                let id = agent_id(&params)?;
                let topic_id = scope(&params, gateway, ctx).await?;
                let Some(current) = stored(gateway, &id, topic_id).await? else {
                    return Err(invalid(format!(
                        "no agent definition '{id}' exists {}; use action 'create' to add one",
                        scope_phrase(topic_id)
                    )));
                };
                let definition = NewAgentDefinition {
                    id,
                    topic_id,
                    display_name: replaced(
                        given_string(&params, "display_name")?,
                        current.display_name,
                        "display_name",
                    )?,
                    description: replaced(
                        given_string(&params, "description")?,
                        current.description,
                        "description",
                    )?,
                    instructions: replaced(
                        given_string(&params, "instructions")?,
                        current.instructions,
                        "instructions",
                    )?,
                    model_id: revised(given_string(&params, "model_id")?, current.model_id),
                    tool_permissions: revised(
                        given_object(&params, "tool_permissions")?,
                        current.tool_permissions,
                    ),
                    skills: revised(given_strings(&params, "skills")?, Some(current.skills))
                        .unwrap_or_default(),
                    output_schema: revised(
                        given_object(&params, "output_schema")?,
                        current.output_schema,
                    ),
                    output_max_retries: revised(
                        given_u32(&params, "output_max_retries")?,
                        current.output_max_retries,
                    ),
                };
                let change =
                    service::upsert_agent_definition(gateway, definition, SOURCE, session_id(ctx))
                        .await
                        .map_err(map)?;
                changed(gateway, change).await
            }
            "delete" => {
                let id = agent_id(&params)?;
                let topic_id = scope(&params, gateway, ctx).await?;
                let change = service::delete_agent_definition(
                    gateway,
                    &id,
                    topic_id,
                    SOURCE,
                    session_id(ctx),
                )
                .await
                .map_err(map)?;
                Ok(json!({
                    "id": change.agent_id,
                    "scope": scope_word(change.topic_id),
                    "summary": change.summary,
                }))
            }
            other => Err(invalid(format!(
                "unknown action '{other}'; use one of {}",
                ACTIONS.join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_config::model::{ModelCapabilitiesConfig, ModelEndpointConfig};
    use swage_provider::LlmProvider;
    use swage_storage::{
        AgentDefinitionStore, AuditStore, NewSession, NewTopic, SessionOrigin, SessionStore,
        SqliteStorage, Storage, TopicStore,
    };
    use swage_tool::context::MockToolContext;
    use tokio::sync::mpsc;

    use super::*;
    use crate::tool_executor::GatewayToolContext;
    use crate::turn::tests::MockProvider;

    const CONFIGURED_MODEL: &str = "local-small";

    /// A session in a topic, on a gateway whose catalogue configures one
    /// model, with the tool context that session's turn would carry.
    async fn session_ctx() -> (GatewayToolContext, AppState, Arc<SqliteStorage>, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Planning".into(),
                slug: "planning".into(),
            },
        )
        .await
        .unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "gateway".into(),
                origin: SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let mut config = swage_config::ResolvedConfig::default();
        config.models.entries.insert(
            CONFIGURED_MODEL.into(),
            ModelEndpointConfig {
                provider_id: "local".into(),
                model_id: "small".into(),
                aliases: vec![],
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: ModelCapabilitiesConfig::default(),
            },
        );
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::for_test(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let base = MockToolContext {
            agent_id: "gateway".into(),
            session_id: session.id.to_string(),
            working_directory: std::env::temp_dir(),
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(&base, ws_tx, Arc::clone(&storage) as Arc<dyn Storage>)
            .with_app_state(Some(state.clone()));
        (ctx, state, storage, topic.id)
    }

    fn heron(scope: Option<&str>) -> Value {
        let mut call = json!({
            "action": "create",
            "id": "heron",
            "display_name": "Heron",
            "description": "Watches the water and strikes once.",
            "instructions": "Answer in one sentence.",
            "model_id": CONFIGURED_MODEL,
            "skills": ["source-checking"],
        });
        if let Some(scope) = scope {
            call["scope"] = json!(scope);
        }
        call
    }

    async fn definition_changes(storage: &SqliteStorage) -> Vec<swage_storage::AuditEvent> {
        storage
            .list_audit_events()
            .await
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.event_kind == "configuration_change"
                    && event.entity_kind == "agent_definition"
            })
            .collect()
    }

    /// The agent creates a definition, changes one field of it without
    /// losing the rest, clears an optional field with null, and deletes it;
    /// each step leaves one record carrying the whole state either side.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_update_and_delete_record_the_whole_definition_either_side() {
        let (ctx, _state, storage, _) = session_ctx().await;
        let tool = AgentDefinitionTool;

        let created = tool.execute(heron(None), &ctx).await.expect("create");
        assert_eq!(created["id"], "heron");
        assert_eq!(created["scope"], "global");
        assert_eq!(created["definition"]["model_id"], CONFIGURED_MODEL);
        assert_eq!(created["definition"]["skills"], json!(["source-checking"]));
        assert!(created["definition"].get("topic_id").is_none());

        let renamed = tool
            .execute(
                json!({"action": "update", "id": "heron", "display_name": "Grey heron"}),
                &ctx,
            )
            .await
            .expect("update keeps what it is not given");
        assert_eq!(renamed["definition"]["display_name"], "Grey heron");
        assert_eq!(
            renamed["definition"]["instructions"],
            "Answer in one sentence."
        );
        assert_eq!(renamed["definition"]["model_id"], CONFIGURED_MODEL);
        assert_eq!(renamed["definition"]["skills"], json!(["source-checking"]));

        let cleared = tool
            .execute(
                json!({"action": "update", "id": "heron", "model_id": null, "skills": null}),
                &ctx,
            )
            .await
            .expect("null clears an optional field");
        assert!(cleared["definition"]["model_id"].is_null());
        assert_eq!(cleared["definition"]["skills"], json!([]));
        assert_eq!(cleared["definition"]["display_name"], "Grey heron");

        let kept_text = tool
            .execute(
                json!({"action": "update", "id": "heron", "instructions": null}),
                &ctx,
            )
            .await
            .expect_err("the instructions are never cleared by null");
        assert!(
            matches!(&kept_text, ToolError::InvalidParameters { message, .. } if message.contains("instructions")),
            "{kept_text}"
        );

        let changes = definition_changes(&storage).await;
        assert_eq!(
            changes.len(),
            3,
            "create and two updates each leave one record"
        );
        assert!(changes.iter().all(|event| {
            event.metadata["action"] == "upsert_agent_definition"
                && event.metadata["source"] == SOURCE
                && event.entity_id == "heron@global"
        }));
        let creation = changes
            .iter()
            .find(|event| event.metadata["prior_state"].is_null())
            .expect("the creation records no prior state");
        assert_eq!(creation.metadata["new_state"]["display_name"], "Heron");
        let rename = changes
            .iter()
            .find(|event| event.metadata["prior_state"]["display_name"] == "Heron")
            .expect("the rename records the name it replaced");
        assert_eq!(rename.metadata["new_state"]["display_name"], "Grey heron");
        assert_eq!(rename.metadata["new_state"]["model_id"], CONFIGURED_MODEL);
        let clearing = changes
            .iter()
            .find(|event| event.metadata["prior_state"]["model_id"] == CONFIGURED_MODEL)
            .expect("the clearing records the model it dropped");
        assert!(clearing.metadata["new_state"]["model_id"].is_null());

        let deleted = tool
            .execute(json!({"action": "delete", "id": "heron"}), &ctx)
            .await
            .expect("delete");
        assert_eq!(deleted["id"], "heron");
        assert!(
            storage
                .list_agent_definitions(None)
                .await
                .unwrap()
                .is_empty()
        );
        let deletion = definition_changes(&storage)
            .await
            .into_iter()
            .find(|event| event.metadata["action"] == "delete_agent_definition")
            .expect("the deletion leaves a record");
        assert_eq!(
            deletion.metadata["prior_state"]["display_name"],
            "Grey heron"
        );
        assert!(deletion.metadata["new_state"].is_null());
    }

    /// `create` never overwrites and `update` never creates: each says which
    /// action the agent wanted, and a refused call writes and records nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_refuses_an_existing_id_and_update_refuses_a_missing_one() {
        let (ctx, _state, storage, _) = session_ctx().await;
        let tool = AgentDefinitionTool;

        let missing = tool
            .execute(
                json!({"action": "update", "id": "heron", "display_name": "Heron"}),
                &ctx,
            )
            .await
            .expect_err("nothing to update yet");
        assert!(
            matches!(&missing, ToolError::InvalidParameters { message, .. } if message.contains("'create'")),
            "{missing}"
        );

        tool.execute(heron(None), &ctx).await.expect("create");
        let mut again = heron(None);
        again["instructions"] = json!("Something else entirely.");
        let duplicate = tool
            .execute(again, &ctx)
            .await
            .expect_err("a second create must not overwrite");
        assert!(
            matches!(&duplicate, ToolError::InvalidParameters { message, .. } if message.contains("'update'")),
            "{duplicate}"
        );

        let stored = storage.list_agent_definitions(None).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].instructions, "Answer in one sentence.");
        assert_eq!(definition_changes(&storage).await.len(), 1);
    }

    /// A topic-scoped definition belongs to the calling session's topic, which
    /// the call never names; `list` shows the picker's view — the topic row
    /// hiding the global one, the shipped default offered and the curator
    /// listed but not offered.
    #[tokio::test(flavor = "multi_thread")]
    async fn topic_scope_comes_from_the_session_and_list_shows_the_pickers_view() {
        let (ctx, _state, storage, topic_id) = session_ctx().await;
        let tool = AgentDefinitionTool;

        tool.execute(heron(None), &ctx).await.expect("global heron");
        let mut local = heron(Some("topic"));
        local["display_name"] = json!("Heron (planning)");
        let created = tool.execute(local, &ctx).await.expect("topic heron");
        assert_eq!(created["scope"], "topic");
        let rows = storage
            .list_agent_definitions(Some(topic_id))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].topic_id, Some(topic_id));
        let changes = definition_changes(&storage).await;
        assert!(
            changes
                .iter()
                .any(|event| event.entity_id == format!("heron@{topic_id}")),
            "the topic row has its own audit identity"
        );

        let listed = tool
            .execute(json!({"action": "list"}), &ctx)
            .await
            .expect("list");
        let definitions = listed["definitions"].as_array().unwrap();
        let by_id = |id: &str| {
            definitions
                .iter()
                .find(|definition| definition["id"] == id)
                .unwrap_or_else(|| panic!("{id} listed"))
        };
        assert_eq!(by_id("heron")["display_name"], "Heron (planning)");
        assert_eq!(by_id("heron")["scope"], "topic");
        assert_eq!(by_id("heron")["selectable"], true);
        assert_eq!(by_id("gateway")["scope"], "built-in");
        assert_eq!(by_id("gateway")["selectable"], true);
        let curator = by_id(crate::memory::MEMORY_CURATOR_AGENT_ID);
        assert_eq!(curator["scope"], "built-in");
        assert_eq!(curator["selectable"], false);
        assert!(
            definitions
                .iter()
                .all(|definition| definition.get("topic_id").is_none())
        );

        tool.execute(
            json!({"action": "delete", "id": "heron", "scope": "topic"}),
            &ctx,
        )
        .await
        .expect("delete the topic row");
        let listed = tool
            .execute(json!({"action": "list"}), &ctx)
            .await
            .expect("list");
        let heron = listed["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|definition| definition["id"] == "heron")
            .expect("the global heron shows once the topic row is gone");
        assert_eq!(heron["display_name"], "Heron");
        assert_eq!(heron["scope"], "global");

        let unknown = tool
            .execute(
                json!({"action": "delete", "id": "heron", "scope": "everywhere"}),
                &ctx,
            )
            .await
            .expect_err("a scope outside the two is refused");
        assert!(matches!(unknown, ToolError::InvalidParameters { .. }));
    }

    /// The service's refusals reach the agent as parameter errors it can act
    /// on, and nothing is written or recorded for a refused definition.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_definition_is_a_parameter_error_and_writes_nothing() {
        let (ctx, _state, storage, _) = session_ctx().await;
        let tool = AgentDefinitionTool;

        let mut unknown_model = heron(None);
        unknown_model["model_id"] = json!("nowhere");
        let error = tool
            .execute(unknown_model, &ctx)
            .await
            .expect_err("a model outside the catalogue is refused");
        assert!(
            matches!(&error, ToolError::InvalidParameters { message, .. } if message.contains(CONFIGURED_MODEL)),
            "{error}"
        );

        let mut reserved = heron(None);
        reserved["id"] = json!("gateway");
        let error = tool
            .execute(reserved, &ctx)
            .await
            .expect_err("the shipped default cannot be redefined");
        assert!(matches!(error, ToolError::InvalidParameters { .. }));

        let mut approvals = heron(None);
        approvals["tool_permissions"] =
            json!({"allow": ["*"], "deny": [], "bundles": {}, "require_approval": []});
        let error = tool
            .execute(approvals, &ctx)
            .await
            .expect_err("approval requirements are configuration's");
        assert!(
            matches!(&error, ToolError::InvalidParameters { message, .. } if message.contains("require_approval")),
            "{error}"
        );

        assert!(
            storage
                .list_agent_definitions(None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(definition_changes(&storage).await.is_empty());
    }

    fn names_as_a_word(text: &str, word: &str) -> bool {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|token| token == word)
    }

    /// The model picks and fills the tool from its text alone, so the
    /// description names every action and parameter, and every parameter
    /// says what it means.
    #[test]
    fn the_description_names_every_action_and_parameter_the_schema_offers() {
        let tool = AgentDefinitionTool;
        let description = tool.description();
        let schema = tool.parameters();
        for action in ACTIONS {
            assert!(
                names_as_a_word(description, action),
                "the description names the action {action}"
            );
        }
        for (parameter, spec) in schema["properties"].as_object().expect("properties") {
            assert!(
                names_as_a_word(description, parameter),
                "the description names the parameter {parameter}"
            );
            assert!(
                spec["description"]
                    .as_str()
                    .is_some_and(|text| text.split_whitespace().count() >= 8),
                "the parameter {parameter} says what it means"
            );
        }
        assert_eq!(schema["properties"]["action"]["enum"], json!(ACTIONS));
        assert_eq!(schema["properties"]["scope"]["enum"], json!(SCOPES));
        assert!(
            names_as_a_word(description, "task_definition"),
            "the description points the model to its sibling"
        );
        assert_eq!(tool.display_hints().summary_param, Some("id"));
    }
}
