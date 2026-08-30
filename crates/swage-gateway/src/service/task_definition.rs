//! Audited whole-state mutations for durable task definitions.

use swage_core::GatewayError;
use swage_storage::{NewTaskDefinition, Storage, TaskDefinition};

use crate::{
    config_change::{ConfigChange, record_config_change},
    service::is_lowercase_identifier,
    state::AppState,
};

pub struct TaskDefinitionChange {
    pub id: String,
    pub summary: String,
}

pub fn validate_task_definition(definition: &NewTaskDefinition) -> Result<(), GatewayError> {
    if !is_lowercase_identifier(&definition.id) {
        return Err(GatewayError::ValidationError(
            "task ids are non-empty lowercase ASCII letters, digits, and hyphens".into(),
        ));
    }
    if definition.instructions.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "a task definition needs instructions".into(),
        ));
    }
    if let Some(value) = &definition.tool_permissions {
        serde_json::from_value::<swage_config::model::ToolPermissionConfig>(value.clone())
            .map_err(|e| {
                GatewayError::ValidationError(format!(
                    "the permission overlay is not a permission configuration: {e}"
                ))
            })?;
    }
    if let Some(schema) = &definition.output_schema {
        jsonschema::validator_for(schema).map_err(|e| {
            GatewayError::ValidationError(format!(
                "the output contract is not a valid JSON Schema: {e}"
            ))
        })?;
    }
    Ok(())
}

async fn snapshot(
    storage: &dyn Storage,
    id: &str,
) -> Result<Option<serde_json::Value>, GatewayError> {
    storage
        .get_task_definition(id)
        .await
        .map_err(GatewayError::from)?
        .map(|v| serde_json::to_value(v).map_err(|e| GatewayError::ConfigError(e.to_string())))
        .transpose()
}

pub async fn upsert_task_definition(
    state: &AppState,
    definition: NewTaskDefinition,
    source: &str,
    session_id: Option<uuid::Uuid>,
) -> Result<TaskDefinitionChange, GatewayError> {
    validate_task_definition(&definition)?;
    let id = definition.id.clone();
    let prior = snapshot(state.storage.as_ref(), &id).await?;
    state
        .storage
        .upsert_task_definition(definition)
        .await
        .map_err(GatewayError::from)?;
    let new_state = snapshot(state.storage.as_ref(), &id).await?;
    let summary = format!(
        "{} task definition '{id}'",
        if prior.is_some() {
            "updated"
        } else {
            "created"
        }
    );
    record_config_change(
        state,
        ConfigChange {
            entity_kind: "task_definition",
            entity_id: &id,
            action: "upsert_task_definition",
            source,
            session_id,
            summary: &summary,
            prior_state: prior,
            new_state,
        },
    )
    .await?;
    Ok(TaskDefinitionChange { id, summary })
}

pub async fn delete_task_definition(
    state: &AppState,
    id: &str,
    source: &str,
    session_id: Option<uuid::Uuid>,
) -> Result<TaskDefinitionChange, GatewayError> {
    if id.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "deleting a task definition needs its id".into(),
        ));
    }
    let prior = snapshot(state.storage.as_ref(), id).await?;
    if !state
        .storage
        .delete_task_definition(id)
        .await
        .map_err(GatewayError::from)?
    {
        return Err(GatewayError::ValidationError(format!(
            "no task definition '{id}' exists"
        )));
    }
    let summary = format!("deleted task definition '{id}'");
    record_config_change(
        state,
        ConfigChange {
            entity_kind: "task_definition",
            entity_id: id,
            action: "delete_task_definition",
            source,
            session_id,
            summary: &summary,
            prior_state: prior,
            new_state: None,
        },
    )
    .await?;
    Ok(TaskDefinitionChange {
        id: id.into(),
        summary,
    })
}

pub async fn resolve_task_definition(
    storage: &dyn Storage,
    id: &str,
) -> Result<TaskDefinition, GatewayError> {
    storage
        .get_task_definition(id)
        .await
        .map_err(GatewayError::from)?
        .ok_or_else(|| GatewayError::ValidationError(format!("no task definition '{id}' exists")))
}
