//! Agent-definition mutations: validate, apply through the store, record.
//!
//! The reference shape for a configuration mutation surface (see
//! `config_change.rs`): validation runs before any store touch, the apply
//! goes through the resource's own store, and the recorded change carries
//! the whole prior and new state of the definition.

use std::collections::BTreeMap;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::NewAgentDefinition;

use crate::config_change::{ConfigChange, record_config_change};
use crate::state::AppState;
use swage_storage::Storage;

/// The outcome a mutation surface reports: which definition changed and a
/// human-readable account of what happened. The full record is the audit
/// trail's.
pub struct AgentDefinitionChange {
    pub agent_id: String,
    pub topic_id: Option<Uuid>,
    pub summary: String,
}

/// A definition that can actually be selected for a new session in a topic.
pub struct EffectiveAgentDefinition {
    pub definition: NewAgentDefinition,
    pub is_builtin: bool,
}

fn gateway_definition() -> NewAgentDefinition {
    NewAgentDefinition {
        id: "gateway".into(),
        topic_id: None,
        display_name: "Swage default".into(),
        description: "The shipped default agent".into(),
        instructions: String::new(),
        model_id: None,
        tool_permissions: None,
        skills: Vec::new(),
        output_schema: None,
        output_max_retries: None,
    }
}

/// Resolve the single definition every runtime consumer should use.
///
/// Authored topic and global rows retain storage's established precedence.
/// The shipped `gateway` default is the only definition that exists without
/// a row: nothing else ships as a global definition (a shipped workflow's
/// roles live inside its script as bare dispatches).
pub async fn resolve_effective_agent_definition(
    storage: &dyn Storage,
    agent_id: &str,
    topic_id: Uuid,
) -> Result<Option<EffectiveAgentDefinition>, GatewayError> {
    if agent_id == "gateway" {
        return Ok(Some(EffectiveAgentDefinition {
            definition: gateway_definition(),
            is_builtin: true,
        }));
    }
    Ok(storage
        .resolve_agent_definition(agent_id, topic_id)
        .await
        .map_err(GatewayError::from)?
        .map(stored_definition))
}

/// Read the effective registry for a topic. Topic definitions shadow global
/// definitions with the same id; definitions from every other topic remain
/// invisible. The shipped gateway agent is the only entry present without a
/// row.
pub async fn effective_agent_definitions(
    storage: &dyn Storage,
    topic_id: Uuid,
) -> Result<Vec<EffectiveAgentDefinition>, GatewayError> {
    let mut effective = BTreeMap::new();
    for definition in storage
        .list_agent_definitions(None)
        .await
        .map_err(GatewayError::from)?
    {
        effective.insert(definition.id.clone(), stored_definition(definition));
    }
    for definition in storage
        .list_agent_definitions(Some(topic_id))
        .await
        .map_err(GatewayError::from)?
    {
        effective.insert(definition.id.clone(), stored_definition(definition));
    }

    // `gateway` is a reserved creation selector whose behaviour is supplied
    // by shipped configuration, never by an authored registry row.
    effective.insert(
        "gateway".into(),
        EffectiveAgentDefinition {
            definition: gateway_definition(),
            is_builtin: true,
        },
    );
    Ok(effective.into_values().collect())
}

fn stored_definition(definition: swage_storage::AgentDefinition) -> EffectiveAgentDefinition {
    EffectiveAgentDefinition {
        definition: NewAgentDefinition {
            id: definition.id,
            topic_id: definition.topic_id,
            display_name: definition.display_name,
            description: definition.description,
            instructions: definition.instructions,
            model_id: definition.model_id,
            tool_permissions: definition.tool_permissions,
            skills: definition.skills,
            output_schema: definition.output_schema,
            output_max_retries: definition.output_max_retries,
        },
        is_builtin: false,
    }
}

/// Create or replace a durable agent definition, recorded.
pub async fn upsert_agent_definition(
    state: &AppState,
    definition: NewAgentDefinition,
    source: &str,
    session_id: Option<Uuid>,
) -> Result<AgentDefinitionChange, GatewayError> {
    validate_definition(&definition)?;

    let prior = snapshot_definition(state, &definition.id, definition.topic_id).await?;
    let action_word = if prior.is_some() {
        "updated"
    } else {
        "created"
    };
    let scope = definition
        .topic_id
        .map_or_else(|| "globally".to_string(), |id| format!("in topic {id}"));
    let summary = format!("{action_word} agent definition '{}' {scope}", definition.id);

    let agent_id = definition.id.clone();
    let topic_id = definition.topic_id;
    state
        .storage
        .upsert_agent_definition(definition)
        .await
        .map_err(GatewayError::from)?;
    let new_state = snapshot_definition(state, &agent_id, topic_id).await?;

    record_config_change(
        state,
        ConfigChange {
            entity_kind: "agent_definition",
            entity_id: &scoped_entity_id(&agent_id, topic_id),
            action: "upsert_agent_definition",
            source,
            session_id,
            summary: &summary,
            prior_state: prior,
            new_state,
        },
    )
    .await?;

    Ok(AgentDefinitionChange {
        agent_id,
        topic_id,
        summary,
    })
}

/// Remove a durable agent definition, recorded with the state it removed.
pub async fn delete_agent_definition(
    state: &AppState,
    agent_id: &str,
    topic_id: Option<Uuid>,
    source: &str,
    session_id: Option<Uuid>,
) -> Result<AgentDefinitionChange, GatewayError> {
    if agent_id.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "deleting a definition needs its agent id".into(),
        ));
    }

    let prior = snapshot_definition(state, agent_id, topic_id).await?;
    let removed = state
        .storage
        .delete_agent_definition(agent_id, topic_id)
        .await
        .map_err(GatewayError::from)?;
    if !removed {
        return Err(GatewayError::ValidationError(format!(
            "no agent definition '{agent_id}' exists at that scope"
        )));
    }
    let summary = format!("deleted agent definition '{agent_id}'");

    record_config_change(
        state,
        ConfigChange {
            entity_kind: "agent_definition",
            entity_id: &scoped_entity_id(agent_id, topic_id),
            action: "delete_agent_definition",
            source,
            session_id,
            summary: &summary,
            prior_state: prior,
            new_state: None,
        },
    )
    .await?;

    Ok(AgentDefinitionChange {
        agent_id: agent_id.to_owned(),
        topic_id,
        summary,
    })
}

/// Validate a definition before anything is written.
pub fn validate_definition(definition: &NewAgentDefinition) -> Result<(), GatewayError> {
    if definition.id.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "an agent definition needs an id".into(),
        ));
    }
    if !super::is_lowercase_identifier(&definition.id) {
        return Err(GatewayError::ValidationError(
            "agent ids are lowercase ASCII letters, digits, and hyphens".into(),
        ));
    }
    if definition.id == "gateway" {
        return Err(GatewayError::ValidationError(
            "the gateway agent id is reserved".into(),
        ));
    }
    if definition.display_name.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "an agent definition needs a display name".into(),
        ));
    }
    if let Some(overlay) = &definition.tool_permissions {
        serde_json::from_value::<swage_config::model::ToolPermissionConfig>(overlay.clone())
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "the permission overlay is not a permission configuration: {error}"
                ))
            })?;
    }
    if definition
        .skills
        .iter()
        .any(|skill| skill.trim().is_empty())
    {
        return Err(GatewayError::ValidationError(
            "declared skill names must not be empty".into(),
        ));
    }
    let mut unique_skills = std::collections::HashSet::new();
    if definition
        .skills
        .iter()
        .any(|skill| !unique_skills.insert(skill))
    {
        return Err(GatewayError::ValidationError(
            "declared skill names must be unique".into(),
        ));
    }
    if let Some(schema) = &definition.output_schema {
        swage_core::json_schema_validator(schema).map_err(|error| {
            GatewayError::ValidationError(format!(
                "the output contract is not a valid JSON Schema: {error}"
            ))
        })?;
    }
    Ok(())
}

/// The definition's current stored state, `None` when absent.
async fn snapshot_definition(
    state: &AppState,
    agent_id: &str,
    topic_id: Option<Uuid>,
) -> Result<Option<serde_json::Value>, GatewayError> {
    let definitions = state
        .storage
        .list_agent_definitions(topic_id)
        .await
        .map_err(GatewayError::from)?;
    Ok(definitions
        .into_iter()
        .find(|definition| definition.id == agent_id)
        .map(|definition| serde_json::to_value(definition).unwrap_or(serde_json::Value::Null)))
}

/// Agent ids are unique only within a scope, so the recorded identity
/// carries the scope — a global `assistant` and a topic-scoped `assistant`
/// must never share an audit identity.
fn scoped_entity_id(id: &str, topic_id: Option<Uuid>) -> String {
    match topic_id {
        Some(topic) => format!("{id}@{topic}"),
        None => format!("{id}@global"),
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use swage_storage::{AgentDefinitionStore, NewTopic, SqliteStorage, TopicStore};

    fn definition(id: &str) -> NewAgentDefinition {
        NewAgentDefinition {
            id: id.into(),
            topic_id: None,
            display_name: "Test agent".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: None,
            skills: Vec::new(),
            output_schema: None,
            output_max_retries: None,
        }
    }

    #[test]
    fn rejects_an_invalid_agent_id() {
        let result = validate_definition(&definition("Not Valid"));
        assert!(matches!(
            result,
            Err(GatewayError::ValidationError(message))
                if message.contains("lowercase")
        ));
    }

    #[test]
    fn rejects_the_reserved_gateway_agent_id() {
        let result = validate_definition(&definition("gateway"));
        assert!(matches!(
            result,
            Err(GatewayError::ValidationError(message))
                if message.contains("reserved")
        ));
    }

    #[test]
    fn rejects_an_empty_display_name() {
        let mut bad = definition("ok-id");
        bad.display_name = "  ".into();
        assert!(validate_definition(&bad).is_err());
    }

    #[test]
    fn rejects_a_malformed_permission_overlay() {
        let mut bad = definition("ok-id");
        bad.tool_permissions = Some(serde_json::json!({ "allow": "not-a-list" }));
        assert!(matches!(
            validate_definition(&bad),
            Err(GatewayError::ValidationError(message))
                if message.contains("permission configuration")
        ));
    }

    #[test]
    fn rejects_blank_and_duplicate_skills() {
        let mut blank = definition("ok-id");
        blank.skills = vec!["real".into(), "  ".into()];
        assert!(validate_definition(&blank).is_err());

        let mut duplicate = definition("ok-id");
        duplicate.skills = vec!["twice".into(), "twice".into()];
        assert!(validate_definition(&duplicate).is_err());
    }

    #[test]
    fn rejects_an_invalid_output_schema() {
        let mut bad = definition("ok-id");
        bad.output_schema = Some(serde_json::json!({ "type": 42 }));
        assert!(matches!(
            validate_definition(&bad),
            Err(GatewayError::ValidationError(message))
                if message.contains("JSON Schema")
        ));
    }

    #[test]
    fn accepts_a_complete_valid_definition() {
        let mut good = definition("research-lead");
        good.skills = vec!["search".into(), "cite".into()];
        good.output_schema = Some(serde_json::json!({ "type": "object" }));
        assert!(validate_definition(&good).is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn effective_registry_merges_scopes_around_the_shipped_default() {
        let storage = SqliteStorage::in_memory().unwrap();
        let current = storage
            .create(NewTopic {
                name: "Current".into(),
                slug: "current".into(),
            })
            .await
            .unwrap();
        let other = storage
            .create(NewTopic {
                name: "Other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();

        let mut global = definition("shared");
        global.instructions = "global".into();
        storage.upsert_agent_definition(global).await.unwrap();
        let mut local = definition("shared");
        local.topic_id = Some(current.id);
        local.instructions = "local".into();
        storage.upsert_agent_definition(local).await.unwrap();
        let mut global_only = definition("global-only");
        global_only.instructions = "visible globally".into();
        storage.upsert_agent_definition(global_only).await.unwrap();
        let mut foreign = definition("foreign");
        foreign.topic_id = Some(other.id);
        storage.upsert_agent_definition(foreign).await.unwrap();

        let effective = effective_agent_definitions(&storage, current.id)
            .await
            .unwrap();
        let ids = effective
            .iter()
            .map(|entry| entry.definition.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["gateway", "global-only", "shared"]);
        assert!(effective[0].is_builtin);
        assert_eq!(effective[0].definition.display_name, "Swage default");
        assert!(!effective[1].is_builtin);
        assert_eq!(effective[2].definition.instructions, "local");
    }

    /// Nothing ships as a global definition: a fresh store resolves only the
    /// `gateway` default, and a shipped workflow's role names are not
    /// definitions a session can select.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_fresh_store_holds_only_the_shipped_default() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage.get_or_create_default().await.unwrap();

        let effective = effective_agent_definitions(&storage, topic.id)
            .await
            .unwrap();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].definition.id, "gateway");
        assert!(effective[0].is_builtin);

        for role in ["researcher", "organiser", "synthesist"] {
            assert!(
                resolve_effective_agent_definition(&storage, role, topic.id)
                    .await
                    .unwrap()
                    .is_none(),
                "{role} must not resolve without an authored row"
            );
        }
    }
}
