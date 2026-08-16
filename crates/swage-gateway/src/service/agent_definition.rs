//! Agent-definition mutations: validate, apply through the store, record.
//!
//! The reference shape for a configuration mutation surface (see
//! `config_change.rs`): validation runs before any store touch, the apply
//! goes through the resource's own store, and the recorded change carries
//! the whole prior and new state of the definition.

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::NewAgentDefinition;

use crate::config_change::{record_config_change, ConfigChange};
use crate::state::AppState;

/// The outcome a mutation surface reports: which definition changed and a
/// human-readable account of what happened. The full record is the audit
/// trail's.
pub struct AgentDefinitionChange {
    pub agent_id: String,
    pub space_id: Option<Uuid>,
    pub summary: String,
}

/// Create or replace a durable agent definition, recorded.
pub async fn upsert_agent_definition(
    state: &AppState,
    definition: NewAgentDefinition,
    source: &str,
    session_id: Option<Uuid>,
) -> Result<AgentDefinitionChange, GatewayError> {
    validate_definition(&definition)?;

    let prior = snapshot_definition(state, &definition.id, definition.space_id).await?;
    let action_word = if prior.is_some() { "updated" } else { "created" };
    let scope = definition
        .space_id
        .map_or_else(|| "globally".to_string(), |id| format!("in space {id}"));
    let summary = format!(
        "{action_word} agent definition '{}' {scope}",
        definition.id
    );

    let agent_id = definition.id.clone();
    let space_id = definition.space_id;
    state
        .storage
        .upsert_agent_definition(definition)
        .await
        .map_err(GatewayError::from)?;
    let new_state = snapshot_definition(state, &agent_id, space_id).await?;

    record_config_change(
        state,
        ConfigChange {
            entity_kind: "agent_definition",
            entity_id: &scoped_entity_id(&agent_id, space_id),
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
        space_id,
        summary,
    })
}

/// Remove a durable agent definition, recorded with the state it removed.
pub async fn delete_agent_definition(
    state: &AppState,
    agent_id: &str,
    space_id: Option<Uuid>,
    source: &str,
    session_id: Option<Uuid>,
) -> Result<AgentDefinitionChange, GatewayError> {
    if agent_id.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "deleting a definition needs its agent id".into(),
        ));
    }

    let prior = snapshot_definition(state, agent_id, space_id).await?;
    let removed = state
        .storage
        .delete_agent_definition(agent_id, space_id)
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
            entity_id: &scoped_entity_id(agent_id, space_id),
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
        space_id,
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
    if !definition
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(GatewayError::ValidationError(
            "agent ids are lowercase ASCII letters, digits, and hyphens".into(),
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
    if definition.skills.iter().any(|skill| skill.trim().is_empty()) {
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
        jsonschema::validator_for(schema).map_err(|error| {
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
    space_id: Option<Uuid>,
) -> Result<Option<serde_json::Value>, GatewayError> {
    let definitions = state
        .storage
        .list_agent_definitions(space_id)
        .await
        .map_err(GatewayError::from)?;
    Ok(definitions
        .into_iter()
        .find(|definition| definition.id == agent_id)
        .map(|definition| serde_json::to_value(definition).unwrap_or(serde_json::Value::Null)))
}

/// Agent ids are unique only within a scope, so the recorded identity
/// carries the scope — a global `assistant` and a space-scoped `assistant`
/// must never share an audit identity.
fn scoped_entity_id(id: &str, space_id: Option<Uuid>) -> String {
    match space_id {
        Some(space) => format!("{id}@{space}"),
        None => format!("{id}@global"),
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn definition(id: &str) -> NewAgentDefinition {
        NewAgentDefinition {
            id: id.into(),
            space_id: None,
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
}
