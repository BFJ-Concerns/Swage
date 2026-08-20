use std::collections::BTreeMap;

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::Storage;

use crate::config_change::{ConfigChange, record_config_change};
use crate::state::AppState;

pub const VARIABLE_NAME_RULE: &str =
    "variable names are lowercase ASCII letters, digits, and hyphens";

pub fn validate_variable_name(name: &str) -> Result<(), GatewayError> {
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
    {
        return Err(GatewayError::ValidationError(VARIABLE_NAME_RULE.into()));
    }
    Ok(())
}

pub async fn snapshot_session_variables(
    storage: &dyn Storage,
    session_id: Uuid,
) -> Result<serde_json::Value, GatewayError> {
    let variables = storage
        .list_session_variables(session_id)
        .await
        .map_err(GatewayError::from)?;
    let state: BTreeMap<_, _> = variables
        .into_iter()
        .map(|variable| (variable.name, variable.value))
        .collect();
    serde_json::to_value(state).map_err(|error| GatewayError::ConfigError(error.to_string()))
}

pub async fn set_session_variable(
    state: &AppState,
    session_id: Uuid,
    name: &str,
    value: &str,
    source: &str,
) -> Result<String, GatewayError> {
    validate_variable_name(name)?;
    let prior = snapshot_session_variables(state.storage.as_ref(), session_id).await?;
    state
        .storage
        .set_session_variable(session_id, name, value)
        .await
        .map_err(GatewayError::from)?;
    let new = snapshot_session_variables(state.storage.as_ref(), session_id).await?;
    let summary = format!("set session value '{name}'");
    record_config_change(
        state,
        ConfigChange {
            entity_kind: "session_values",
            entity_id: &session_id.to_string(),
            action: "set_session_value",
            source,
            session_id: Some(session_id),
            summary: &summary,
            prior_state: Some(prior),
            new_state: Some(new),
        },
    )
    .await?;
    Ok(summary)
}

pub async fn delete_session_variable(
    state: &AppState,
    session_id: Uuid,
    name: &str,
    source: &str,
) -> Result<String, GatewayError> {
    validate_variable_name(name)?;
    let prior = snapshot_session_variables(state.storage.as_ref(), session_id).await?;
    if !state
        .storage
        .delete_session_variable(session_id, name)
        .await
        .map_err(GatewayError::from)?
    {
        return Err(GatewayError::ValidationError(format!(
            "no session value '{name}' exists"
        )));
    }
    let new = snapshot_session_variables(state.storage.as_ref(), session_id).await?;
    let summary = format!("removed session value '{name}'");
    record_config_change(
        state,
        ConfigChange {
            entity_kind: "session_values",
            entity_id: &session_id.to_string(),
            action: "remove_session_value",
            source,
            session_id: Some(session_id),
            summary: &summary,
            prior_state: Some(prior),
            new_state: Some(new),
        },
    )
    .await?;
    Ok(summary)
}
