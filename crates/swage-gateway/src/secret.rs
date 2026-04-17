use std::env;
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::{GatewayError, StorageError};
use swage_storage::{NewAuditEvent, NewSecretRecord, SecretRecord, store::Storage};

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSecretRequest {
    pub name: String,
    pub material_kind: String,
    pub owner_kind: String,
    pub backend_kind: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub metadata: Option<Value>,
    pub value: Option<String>,
    pub external_reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RotateSecretRequest {
    pub value: Option<String>,
    pub external_reference: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateSecretRequest {
    pub name: String,
    pub material_kind: String,
    pub owner_kind: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub length: Option<usize>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretResponse {
    pub id: Uuid,
    pub name: String,
    pub material_kind: String,
    pub owner_kind: String,
    pub backend_kind: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub reference: String,
    pub metadata: Value,
    pub status: String,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub revoked_at: Option<chrono::DateTime<Utc>>,
    pub revealed_value: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretListResponse {
    pub secrets: Vec<SecretResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretValidationResponse {
    pub id: Uuid,
    pub valid: bool,
    pub detail: String,
}

#[derive(Clone)]
pub struct SecretManager {
    storage: Arc<dyn Storage>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSecret {
    pub record: SecretRecord,
    pub value: String,
}

impl SecretManager {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
    }

    pub async fn create_secret(
        &self,
        request: CreateSecretRequest,
    ) -> Result<SecretResponse, GatewayError> {
        validate_secret_request(&request)?;
        let id = Uuid::new_v4();
        let record = self
            .storage
            .create_secret(NewSecretRecord {
                id,
                name: request.name.trim().to_string(),
                material_kind: request.material_kind.trim().to_string(),
                owner_kind: request.owner_kind.trim().to_string(),
                backend_kind: request.backend_kind.trim().to_string(),
                scope_kind: request.scope_kind.trim().to_string(),
                scope_id: request.scope_id.clone(),
                external_reference: request.external_reference.clone(),
                metadata: request.metadata.unwrap_or_else(|| json!({})),
                value: request.value.clone(),
            })
            .await
            .map_err(GatewayError::Storage)?;
        self.audit_secret_event(&record, "created", "secret created", json!({}))
            .await?;
        Ok(secret_response(record, request.value))
    }

    pub async fn generate_secret(
        &self,
        request: GenerateSecretRequest,
    ) -> Result<SecretResponse, GatewayError> {
        let generated_value = generate_material(&request.material_kind, request.length)?;
        let response = self
            .create_secret(CreateSecretRequest {
                name: request.name,
                material_kind: request.material_kind,
                owner_kind: request.owner_kind,
                backend_kind: "managed".to_string(),
                scope_kind: request.scope_kind,
                scope_id: request.scope_id,
                metadata: request.metadata,
                value: Some(generated_value.clone()),
                external_reference: None,
            })
            .await?;
        let id = response.id;
        let record = self
            .storage
            .get_secret(id)
            .await
            .map_err(GatewayError::Storage)?
            .ok_or_else(|| GatewayError::Storage(StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            }))?;
        self.audit_secret_event(&record, "generated", "secret generated", json!({}))
            .await?;
        Ok(SecretResponse {
            revealed_value: Some(generated_value),
            ..response
        })
    }

    pub async fn list_secrets(&self) -> Result<SecretListResponse, GatewayError> {
        let secrets = self
            .storage
            .list_secrets()
            .await
            .map_err(GatewayError::Storage)?
            .into_iter()
            .map(|record| secret_response(record, None))
            .collect();
        Ok(SecretListResponse { secrets })
    }

    pub async fn get_secret(&self, id: Uuid) -> Result<SecretResponse, GatewayError> {
        let record = self
            .storage
            .get_secret(id)
            .await
            .map_err(GatewayError::Storage)?
            .ok_or_else(|| GatewayError::Storage(StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            }))?;
        Ok(secret_response(record, None))
    }

    pub async fn rotate_secret(
        &self,
        id: Uuid,
        request: RotateSecretRequest,
    ) -> Result<SecretResponse, GatewayError> {
        let existing = self.required_secret(id).await?;
        let metadata = request
            .metadata
            .unwrap_or_else(|| existing.metadata.clone());
        let rotated = self
            .storage
            .update_secret_value(
                id,
                request.value.clone(),
                request
                    .external_reference
                    .clone()
                    .or(existing.external_reference.clone()),
                metadata,
            )
            .await
            .map_err(GatewayError::Storage)?;
        self.audit_secret_event(&rotated, "rotated", "secret rotated", json!({}))
            .await?;
        Ok(secret_response(rotated, request.value))
    }

    pub async fn revoke_secret(&self, id: Uuid) -> Result<SecretResponse, GatewayError> {
        let record = self
            .storage
            .revoke_secret(id)
            .await
            .map_err(GatewayError::Storage)?;
        self.audit_secret_event(&record, "revoked", "secret revoked", json!({}))
            .await?;
        Ok(secret_response(record, None))
    }

    pub async fn restore_secret(&self, id: Uuid) -> Result<SecretResponse, GatewayError> {
        let record = self
            .storage
            .restore_secret(id)
            .await
            .map_err(GatewayError::Storage)?;
        self.audit_secret_event(&record, "restored", "secret restored", json!({}))
            .await?;
        Ok(secret_response(record, None))
    }

    pub async fn validate_secret(
        &self,
        id: Uuid,
    ) -> Result<SecretValidationResponse, GatewayError> {
        let record = self.required_secret(id).await?;
        let resolution = self.resolve(&record.reference).await;
        match resolution {
            Ok(resolved) => {
                validate_resolved_material(&record.material_kind, &resolved.value)?;
                Ok(SecretValidationResponse {
                    id,
                    valid: true,
                    detail: "secret resolved and validated".into(),
                })
            }
            Err(error) => Ok(SecretValidationResponse {
                id,
                valid: false,
                detail: error.to_string(),
            }),
        }
    }

    pub async fn resolve(&self, reference: &str) -> Result<ResolvedSecret, GatewayError> {
        let id = parse_secret_reference(reference)?;
        let record = self.required_secret(id).await?;
        if record.status != "active" {
            self.audit_secret_event(
                &record,
                "access_failed",
                "secret access failed",
                json!({"reason": "revoked_or_inactive"}),
            )
            .await?;
            return Err(GatewayError::ValidationError(format!(
                "secret '{}' is not active",
                record.name
            )));
        }

        let value = match record.backend_kind.as_str() {
            "managed" => self
                .storage
                .get_managed_secret_value(record.id)
                .await
                .map_err(GatewayError::Storage)?
                .ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "managed secret '{}' has no stored value",
                        record.name
                    ))
                })?,
            "env" => {
                let variable = record.external_reference.as_deref().ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "external secret '{}' is missing its reference",
                        record.name
                    ))
                })?;
                env::var(variable).map_err(|_| {
                    GatewayError::ValidationError(format!(
                        "environment reference '{}' is not set for secret '{}'",
                        variable, record.name
                    ))
                })?
            }
            other => {
                return Err(GatewayError::ValidationError(format!(
                    "unsupported secret backend '{other}'"
                )));
            }
        };

        validate_resolved_material(&record.material_kind, &value)?;
        self.audit_secret_event(&record, "accessed", "secret accessed", json!({}))
            .await?;
        Ok(ResolvedSecret { record, value })
    }

    async fn required_secret(&self, id: Uuid) -> Result<SecretRecord, GatewayError> {
        self.storage
            .get_secret(id)
            .await
            .map_err(GatewayError::Storage)?
            .ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "secret".into(),
                    id: id.to_string(),
                })
            })
    }

    async fn audit_secret_event(
        &self,
        record: &SecretRecord,
        event_kind: &str,
        summary: &str,
        metadata: Value,
    ) -> Result<(), GatewayError> {
        self.storage
            .create_audit_event(NewAuditEvent {
                id: format!("secret-{}-{}", event_kind, Uuid::new_v4()),
                session_id: None,
                entity_kind: "secret".into(),
                entity_id: record.id.to_string(),
                event_kind: event_kind.into(),
                summary: summary.into(),
                metadata: json!({
                    "reference": record.reference,
                    "material_kind": record.material_kind,
                    "owner_kind": record.owner_kind,
                    "scope_kind": record.scope_kind,
                    "scope_id": record.scope_id,
                    "backend_kind": record.backend_kind,
                    "detail": metadata
                }),
            })
            .await
            .map_err(GatewayError::Storage)?;
        Ok(())
    }
}

fn secret_response(record: SecretRecord, revealed_value: Option<String>) -> SecretResponse {
    SecretResponse {
        id: record.id,
        name: record.name,
        material_kind: record.material_kind,
        owner_kind: record.owner_kind,
        backend_kind: record.backend_kind,
        scope_kind: record.scope_kind,
        scope_id: record.scope_id,
        reference: record.reference,
        metadata: record.metadata,
        status: record.status,
        created_at: record.created_at,
        updated_at: record.updated_at,
        revoked_at: record.revoked_at,
        revealed_value,
    }
}

fn validate_secret_request(request: &CreateSecretRequest) -> Result<(), GatewayError> {
    if request.name.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "secret name must not be empty".into(),
        ));
    }
    if request.material_kind.trim().is_empty()
        || request.owner_kind.trim().is_empty()
        || request.backend_kind.trim().is_empty()
        || request.scope_kind.trim().is_empty()
    {
        return Err(GatewayError::ValidationError(
            "secret kind, owner, backend, and scope must not be empty".into(),
        ));
    }
    match request.backend_kind.as_str() {
        "managed" => {
            let value = request.value.as_deref().ok_or_else(|| {
                GatewayError::ValidationError("managed secrets require a value".into())
            })?;
            validate_resolved_material(&request.material_kind, value)?;
        }
        "env" => {
            let external_reference = request.external_reference.as_deref().ok_or_else(|| {
                GatewayError::ValidationError(
                    "environment-backed secrets require an external_reference".into(),
                )
            })?;
            if external_reference.trim().is_empty() {
                return Err(GatewayError::ValidationError(
                    "environment secret reference must not be empty".into(),
                ));
            }
        }
        other => {
            return Err(GatewayError::ValidationError(format!(
                "unsupported secret backend '{other}'"
            )));
        }
    }
    Ok(())
}

fn parse_secret_reference(reference: &str) -> Result<Uuid, GatewayError> {
    let (scheme, tail) = reference.split_once("://").ok_or_else(|| {
        GatewayError::ValidationError(format!("invalid secret reference '{reference}'"))
    })?;
    if scheme != "secret" {
        return Err(GatewayError::ValidationError(format!(
            "invalid secret reference scheme '{scheme}'"
        )));
    }
    let (_, id) = tail.rsplit_once('/').ok_or_else(|| {
        GatewayError::ValidationError(format!("invalid secret reference '{reference}'"))
    })?;
    id.parse::<Uuid>().map_err(|error| {
        GatewayError::ValidationError(format!("invalid secret reference '{reference}': {error}"))
    })
}

fn validate_resolved_material(material_kind: &str, value: &str) -> Result<(), GatewayError> {
    if value.trim().is_empty() {
        return Err(GatewayError::ValidationError(format!(
            "{material_kind} value must not be empty"
        )));
    }
    match material_kind {
        "totp_secret" => {
            let uppercase = value.to_ascii_uppercase();
            if uppercase.len() < 16
                || !uppercase
                    .chars()
                    .all(|character| matches!(character, 'A'..='Z' | '2'..='7'))
            {
                return Err(GatewayError::ValidationError(
                    "totp_secret must be a base32-style secret with at least 16 characters"
                        .into(),
                ));
            }
        }
        "ssh_key" => {
            if !value.contains("BEGIN") || !value.contains("PRIVATE KEY") {
                return Err(GatewayError::ValidationError(
                    "ssh_key must contain a private-key PEM block".into(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn generate_material(material_kind: &str, requested_length: Option<usize>) -> Result<String, GatewayError> {
    let length = requested_length.unwrap_or(32).max(16);
    match material_kind {
        "token" | "provider_key" | "channel_token" | "node_credential" | "plugin_secret" => {
            Ok(make_alphabetic_secret("swg", length))
        }
        "password" => Ok(make_password(length)),
        "totp_secret" => Ok(make_base32_secret(length.max(20))),
        other => Err(GatewayError::ValidationError(format!(
            "secret generation is not supported for '{other}'"
        ))),
    }
}

fn make_alphabetic_secret(prefix: &str, minimum_length: usize) -> String {
    let mut value = format!("{prefix}-{}", Uuid::new_v4().simple());
    while value.len() < minimum_length {
        value.push_str(&Uuid::new_v4().simple().to_string());
    }
    value.truncate(minimum_length);
    value
}

fn make_password(minimum_length: usize) -> String {
    let mut value = format!("Pw!{}-{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    value.truncate(minimum_length);
    value
}

fn make_base32_secret(minimum_length: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut value = String::new();
    while value.len() < minimum_length {
        let bytes = *Uuid::new_v4().as_bytes();
        for byte in bytes {
            value.push(ALPHABET[(byte as usize) % ALPHABET.len()] as char);
            if value.len() == minimum_length {
                break;
            }
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use swage_storage::{AuditStore, SqliteStorage};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn managed_and_env_backends_resolve_through_same_model() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let manager = SecretManager::new(storage.clone());
        let managed = manager
            .create_secret(CreateSecretRequest {
                name: "Managed provider key".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                metadata: None,
                value: Some("managed-key".into()),
                external_reference: None,
            })
            .await
            .unwrap();

        unsafe {
            env::set_var("SWAGE_ENV_SECRET_TEST", "env-key");
        }
        let env_secret = manager
            .create_secret(CreateSecretRequest {
                name: "Env provider key".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: "env".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                metadata: None,
                value: None,
                external_reference: Some("SWAGE_ENV_SECRET_TEST".into()),
            })
            .await
            .unwrap();

        assert_eq!(
            manager.resolve(&managed.reference).await.unwrap().value,
            "managed-key"
        );
        assert_eq!(
            manager.resolve(&env_secret.reference).await.unwrap().value,
            "env-key"
        );

        let audits = storage.list_audit_events().await.unwrap();
        assert!(
            audits.iter().all(|event| {
                !event.metadata.to_string().contains("managed-key")
                    && !event.metadata.to_string().contains("env-key")
            }),
            "audit trail must not leak secret values: {audits:?}"
        );

        unsafe {
            env::remove_var("SWAGE_ENV_SECRET_TEST");
        }
    }
}
