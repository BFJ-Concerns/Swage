//! The governed secret system: every credential the gateway uses resolves
//! through one manager, by reference, with an audit event per access and
//! the resolved value registered for redaction.
//!
//! A secret's `backend_kind` names where its value lives, and a resolution
//! reads that backend **every time** — nothing is copied into a second
//! store at creation, so material that changes underneath the record (a
//! harness refreshing its own OAuth tokens) is current at the next access.
//! Three backends:
//!
//! - `managed` — the value is encrypted at rest in the gateway's own store.
//! - `env` — `external_reference` names an environment variable.
//! - `file` — `external_reference` names a file on the gateway host;
//!   `metadata.json_pointer` optionally selects one field of a JSON
//!   document (RFC 6901), which is how a harness-owned auth store such as
//!   the Codex CLI's is brought under governance without moving it.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::{GatewayError, StorageError};
use swage_storage::{
    ManagedSecretEncryptionDiagnostics, NewAuditEvent, NewSecretRecord, SecretRecord,
    managed_secret_encryption_diagnostics, store::Storage,
};

/// Backend identifiers as stored in `secrets.backend_kind`.
pub const MANAGED_BACKEND: &str = "managed";
pub const ENV_BACKEND: &str = "env";
pub const FILE_BACKEND: &str = "file";

/// The `metadata` key under which a `file` secret names the JSON field to
/// read. Absent, the whole file (trimmed) is the value.
pub const FILE_JSON_POINTER_KEY: &str = "json_pointer";

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
    pub managed_backend: ManagedSecretEncryptionDiagnostics,
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
    /// When the backend last changed the material, where the backend can
    /// say — a `file` secret's modification time. `None` for backends that
    /// hold no such fact.
    pub observed_at: Option<DateTime<Utc>>,
}

/// A value read from a secret's backend, before material validation.
struct BackendValue {
    value: String,
    observed_at: Option<DateTime<Utc>>,
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
            .ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "secret".into(),
                    id: id.to_string(),
                })
            })?;
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
        Ok(SecretListResponse {
            secrets,
            managed_backend: managed_secret_encryption_diagnostics(),
        })
    }

    pub async fn get_secret(&self, id: Uuid) -> Result<SecretResponse, GatewayError> {
        let record = self
            .storage
            .get_secret(id)
            .await
            .map_err(GatewayError::Storage)?
            .ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "secret".into(),
                    id: id.to_string(),
                })
            })?;
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
                    .or_else(|| existing.external_reference.clone()),
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
        self.resolve_for_session(reference, None).await
    }

    /// Resolve a reference on a session's behalf — a tool call carrying a
    /// `secret://` parameter — so the access event names the session that
    /// used the secret.
    pub async fn resolve_for_session(
        &self,
        reference: &str,
        session_id: Option<Uuid>,
    ) -> Result<ResolvedSecret, GatewayError> {
        let id = parse_secret_reference(reference)?;
        let record = self.required_secret(id).await?;
        if record.status != "active" {
            self.audit_secret_event_for_session(
                &record,
                session_id,
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

        let BackendValue { value, observed_at } = self.read_backend_value(&record).await?;
        validate_resolved_material(&record.material_kind, &value)?;
        self.audit_secret_event_for_session(
            &record,
            session_id,
            "accessed",
            "secret accessed",
            json!({"observed_at": observed_at}),
        )
        .await?;
        Ok(ResolvedSecret {
            record,
            value,
            observed_at,
        })
    }

    /// Every active secret's current value, for redacting captured provider
    /// traffic. Read from the backends at call time — the same reads
    /// `resolve` makes — so a value that changed since the last turn is
    /// redacted under its new form. A backend that cannot currently be read
    /// (an env variable unset, a file not yet written) contributes nothing
    /// rather than failing the capture.
    pub async fn known_values(&self) -> Result<Vec<String>, GatewayError> {
        let records = self
            .storage
            .list_secrets()
            .await
            .map_err(GatewayError::Storage)?;
        let mut values = Vec::new();
        for record in records
            .into_iter()
            .filter(|record| record.status == "active")
        {
            match self.read_backend_value(&record).await {
                Ok(BackendValue { value, .. }) if !value.is_empty() => values.push(value),
                Ok(_) => {}
                // A managed value that cannot be read is corruption; an
                // external backend that is merely absent is not.
                Err(error) if record.backend_kind == MANAGED_BACKEND => return Err(error),
                Err(_) => {}
            }
        }
        Ok(values)
    }

    /// Bring a harness-owned credential store under governance: the Codex
    /// CLI keeps its ChatGPT OAuth tokens in `auth.json` under its own
    /// state directory and refreshes them itself. Two `file` records — the
    /// access token and the refresh token — point into that document, so
    /// resolution, audit, and redaction all see the tokens the harness is
    /// currently using. Idempotent: an existing active record for either
    /// token is re-pointed at the current path rather than duplicated.
    /// Registration precedes sign-in, so the file need not exist yet.
    pub async fn register_codex_auth_store(
        &self,
        state_dir: &Path,
    ) -> Result<Vec<SecretResponse>, GatewayError> {
        let auth_store = state_dir.join("auth.json");
        let mut registered = Vec::with_capacity(CODEX_AUTH_TOKENS.len());
        for (name, material_kind, pointer) in CODEX_AUTH_TOKENS {
            registered.push(
                self.ensure_file_secret(FileSecretSpec {
                    name,
                    material_kind,
                    owner_kind: "harness",
                    scope_kind: "provider",
                    scope_id: Some("codex"),
                    path: &auth_store,
                    json_pointer: Some(pointer),
                })
                .await?,
            );
        }
        Ok(registered)
    }

    /// Create a `file` secret, or re-point the active one of the same name
    /// and scope when the path or field has moved.
    async fn ensure_file_secret(
        &self,
        spec: FileSecretSpec<'_>,
    ) -> Result<SecretResponse, GatewayError> {
        let metadata = match spec.json_pointer {
            Some(pointer) => json!({ FILE_JSON_POINTER_KEY: pointer }),
            None => json!({}),
        };
        let path = spec.path.to_string_lossy().into_owned();
        let existing = self
            .storage
            .list_secrets()
            .await
            .map_err(GatewayError::Storage)?
            .into_iter()
            .find(|record| {
                record.status == "active"
                    && record.name == spec.name
                    && record.scope_kind == spec.scope_kind
                    && record.scope_id.as_deref() == spec.scope_id
                    && record.backend_kind == FILE_BACKEND
            });
        match existing {
            Some(record)
                if record.external_reference.as_deref() == Some(path.as_str())
                    && record.metadata == metadata =>
            {
                Ok(secret_response(record, None))
            }
            Some(record) => {
                self.rotate_secret(
                    record.id,
                    RotateSecretRequest {
                        value: None,
                        external_reference: Some(path),
                        metadata: Some(metadata),
                    },
                )
                .await
            }
            None => {
                self.create_secret(CreateSecretRequest {
                    name: spec.name.to_string(),
                    material_kind: spec.material_kind.to_string(),
                    owner_kind: spec.owner_kind.to_string(),
                    backend_kind: FILE_BACKEND.to_string(),
                    scope_kind: spec.scope_kind.to_string(),
                    scope_id: spec.scope_id.map(str::to_string),
                    metadata: Some(metadata),
                    value: None,
                    external_reference: Some(path),
                })
                .await
            }
        }
    }

    /// Read a record's current material from its backend. The one place a
    /// backend is dereferenced: resolution and redaction both come here.
    async fn read_backend_value(
        &self,
        record: &SecretRecord,
    ) -> Result<BackendValue, GatewayError> {
        match record.backend_kind.as_str() {
            MANAGED_BACKEND => {
                let value = self
                    .storage
                    .get_managed_secret_value(record.id)
                    .await
                    .map_err(GatewayError::Storage)?
                    .ok_or_else(|| {
                        GatewayError::ValidationError(format!(
                            "managed secret '{}' has no stored value",
                            record.name
                        ))
                    })?;
                Ok(BackendValue {
                    value,
                    observed_at: None,
                })
            }
            ENV_BACKEND => {
                let variable = record.external_reference.as_deref().ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "external secret '{}' is missing its reference",
                        record.name
                    ))
                })?;
                let value = env::var(variable).map_err(|_| {
                    GatewayError::ValidationError(format!(
                        "environment reference '{}' is not set for secret '{}'",
                        variable, record.name
                    ))
                })?;
                Ok(BackendValue {
                    value,
                    observed_at: None,
                })
            }
            FILE_BACKEND => read_file_backend(record).await,
            other => Err(GatewayError::ValidationError(format!(
                "unsupported secret backend '{other}'"
            ))),
        }
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
        self.audit_secret_event_for_session(record, None, event_kind, summary, metadata)
            .await
    }

    /// A secret event with the session it happened in, where one did: the
    /// operator's management of secrets has none; a tool's use of one does.
    async fn audit_secret_event_for_session(
        &self,
        record: &SecretRecord,
        session_id: Option<Uuid>,
        event_kind: &str,
        summary: &str,
        metadata: Value,
    ) -> Result<(), GatewayError> {
        self.storage
            .create_audit_event(NewAuditEvent {
                id: format!("secret-{}-{}", event_kind, Uuid::new_v4()),
                session_id,
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

/// The two tokens the Codex CLI keeps in its auth store, as governed
/// records: (name, material_kind, JSON pointer into `auth.json`).
const CODEX_AUTH_TOKENS: [(&str, &str, &str); 2] = [
    (
        "codex.access_token",
        "oauth_access_token",
        "/tokens/access_token",
    ),
    (
        "codex.refresh_token",
        "oauth_refresh_token",
        "/tokens/refresh_token",
    ),
];

struct FileSecretSpec<'a> {
    name: &'a str,
    material_kind: &'a str,
    owner_kind: &'a str,
    scope_kind: &'a str,
    scope_id: Option<&'a str>,
    path: &'a Path,
    json_pointer: Option<&'a str>,
}

/// Read a `file` secret: the file at `external_reference`, re-read on every
/// call so a backend that refreshes its own material is never stale here,
/// narrowed to `metadata.json_pointer` when one is set. The file's
/// modification time rides along as the observation stamp.
async fn read_file_backend(record: &SecretRecord) -> Result<BackendValue, GatewayError> {
    let path = file_backend_path(record)?;
    let pointer = file_backend_pointer(&record.metadata)?;
    let contents = tokio::fs::read_to_string(&path).await.map_err(|error| {
        GatewayError::ValidationError(format!(
            "file reference '{}' for secret '{}' could not be read: {error}",
            path.display(),
            record.name
        ))
    })?;
    let observed_at = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from);
    let value = match pointer {
        None => contents.trim().to_string(),
        Some(pointer) => {
            let document: Value = serde_json::from_str(&contents).map_err(|error| {
                GatewayError::ValidationError(format!(
                    "file reference '{}' for secret '{}' is not a JSON document: {error}",
                    path.display(),
                    record.name
                ))
            })?;
            match document.pointer(&pointer) {
                Some(Value::String(value)) => value.clone(),
                Some(other) => other.to_string(),
                None => {
                    return Err(GatewayError::ValidationError(format!(
                        "file reference '{}' for secret '{}' has no field at '{pointer}'",
                        path.display(),
                        record.name
                    )));
                }
            }
        }
    };
    Ok(BackendValue { value, observed_at })
}

fn file_backend_path(record: &SecretRecord) -> Result<PathBuf, GatewayError> {
    record
        .external_reference
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            GatewayError::ValidationError(format!(
                "file secret '{}' is missing its path",
                record.name
            ))
        })
}

fn file_backend_pointer(metadata: &Value) -> Result<Option<String>, GatewayError> {
    match metadata.get(FILE_JSON_POINTER_KEY) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(pointer)) if pointer.starts_with('/') => Ok(Some(pointer.clone())),
        Some(other) => Err(GatewayError::ValidationError(format!(
            "file secret {FILE_JSON_POINTER_KEY} must be a JSON pointer starting with '/', got {other}"
        ))),
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
        MANAGED_BACKEND => {
            let value = request.value.as_deref().ok_or_else(|| {
                GatewayError::ValidationError("managed secrets require a value".into())
            })?;
            validate_resolved_material(&request.material_kind, value)?;
        }
        ENV_BACKEND => {
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
        FILE_BACKEND => {
            let path = request.external_reference.as_deref().ok_or_else(|| {
                GatewayError::ValidationError(
                    "file-backed secrets require an external_reference naming the file".into(),
                )
            })?;
            if path.trim().is_empty() {
                return Err(GatewayError::ValidationError(
                    "file secret path must not be empty".into(),
                ));
            }
            if let Some(metadata) = request.metadata.as_ref() {
                file_backend_pointer(metadata)?;
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
                    "totp_secret must be a base32-style secret with at least 16 characters".into(),
                ));
            }
        }
        "ssh_key" if (!value.contains("BEGIN") || !value.contains("PRIVATE KEY")) => {
            return Err(GatewayError::ValidationError(
                "ssh_key must contain a private-key PEM block".into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn generate_material(
    material_kind: &str,
    requested_length: Option<usize>,
) -> Result<String, GatewayError> {
    let length = requested_length.unwrap_or(32).max(16);
    match material_kind {
        "token" | "provider_key" | "channel_token" | "device_credential" | "plugin_secret" => {
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

    use std::sync::{Mutex, MutexGuard};

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use swage_storage::{AuditStore, MANAGED_SECRET_KEY_ENV, SqliteStorage, store::SecretStore};

    use super::*;

    // Secret resolution reads process-wide env vars. Hold this guard across
    // awaits so concurrent secret tests cannot race the managed key value.
    static SECRET_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_secret_env() -> MutexGuard<'static, ()> {
        let guard = SECRET_ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var(MANAGED_SECRET_KEY_ENV, BASE64.encode([9_u8; 32]));
        }
        guard
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn managed_and_env_backends_resolve_through_same_model() {
        let _guard = with_secret_env();
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

    fn file_secret(name: &str, path: &Path, pointer: Option<&str>) -> CreateSecretRequest {
        CreateSecretRequest {
            name: name.into(),
            material_kind: "oauth_access_token".into(),
            owner_kind: "harness".into(),
            backend_kind: FILE_BACKEND.into(),
            scope_kind: "provider".into(),
            scope_id: Some("codex".into()),
            metadata: pointer.map(|pointer| json!({ FILE_JSON_POINTER_KEY: pointer })),
            value: None,
            external_reference: Some(path.to_string_lossy().into_owned()),
        }
    }

    /// The refresh property: a file-backed secret is read from its file at
    /// every resolution, so material the owning harness rewrites is current
    /// at the next access rather than a copy taken at registration.
    #[tokio::test(flavor = "multi_thread")]
    async fn file_backed_secret_is_re_read_on_every_resolution() {
        let _guard = with_secret_env();
        let dir = tempfile::tempdir().unwrap();
        let auth_store = dir.path().join("auth.json");
        std::fs::write(
            &auth_store,
            r#"{"tokens":{"access_token":"first-token","refresh_token":"r1"}}"#,
        )
        .unwrap();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let manager = SecretManager::new(storage.clone());
        let secret = manager
            .create_secret(file_secret(
                "codex.access_token",
                &auth_store,
                Some("/tokens/access_token"),
            ))
            .await
            .unwrap();

        let first = manager.resolve(&secret.reference).await.unwrap();
        assert_eq!(first.value, "first-token");
        assert!(
            first.observed_at.is_some(),
            "a file backend reports when it changed"
        );

        std::fs::write(
            &auth_store,
            r#"{"tokens":{"access_token":"refreshed-token","refresh_token":"r2"}}"#,
        )
        .unwrap();
        let second = manager.resolve(&secret.reference).await.unwrap();
        assert_eq!(second.value, "refreshed-token");

        let audits = storage.list_audit_events().await.unwrap();
        let access_events: Vec<_> = audits
            .iter()
            .filter(|event| event.event_kind == "accessed")
            .collect();
        assert_eq!(
            access_events.len(),
            2,
            "each file-backed resolution must be recorded: {audits:?}"
        );
        assert!(
            access_events.iter().all(|event| {
                let rendered = event.metadata.to_string();
                !rendered.contains("first-token") && !rendered.contains("refreshed-token")
            }),
            "audit trail must not leak file-backed values: {audits:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn file_backed_secret_without_a_pointer_is_the_whole_file() {
        let _guard = with_secret_env();
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, "bare-token\n").unwrap();
        let manager = SecretManager::new(Arc::new(SqliteStorage::in_memory().unwrap()));
        let secret = manager
            .create_secret(file_secret("bare", &token_file, None))
            .await
            .unwrap();
        assert_eq!(
            manager.resolve(&secret.reference).await.unwrap().value,
            "bare-token"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn file_backed_secret_names_a_missing_file_or_field() {
        let _guard = with_secret_env();
        let dir = tempfile::tempdir().unwrap();
        let auth_store = dir.path().join("auth.json");
        let manager = SecretManager::new(Arc::new(SqliteStorage::in_memory().unwrap()));
        let secret = manager
            .create_secret(file_secret(
                "missing",
                &auth_store,
                Some("/tokens/access_token"),
            ))
            .await
            .expect("registration precedes sign-in, so the file need not exist yet");

        let absent = manager.resolve(&secret.reference).await.unwrap_err();
        assert!(absent.to_string().contains("could not be read"), "{absent}");

        std::fs::write(&auth_store, r#"{"tokens":{}}"#).unwrap();
        let no_field = manager.resolve(&secret.reference).await.unwrap_err();
        assert!(
            no_field.to_string().contains("/tokens/access_token"),
            "{no_field}"
        );

        let bad_pointer = manager
            .create_secret(file_secret("bad", &auth_store, Some("tokens.access_token")))
            .await
            .unwrap_err();
        assert!(
            bad_pointer.to_string().contains("JSON pointer"),
            "{bad_pointer}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn codex_auth_store_registration_is_idempotent_and_repoints() {
        let _guard = with_secret_env();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let manager = SecretManager::new(storage.clone());
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();

        let first = manager
            .register_codex_auth_store(first_dir.path())
            .await
            .unwrap();
        let again = manager
            .register_codex_auth_store(first_dir.path())
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(
            first.iter().map(|secret| secret.id).collect::<Vec<_>>(),
            again.iter().map(|secret| secret.id).collect::<Vec<_>>(),
            "re-registration reuses the records"
        );
        assert_eq!(storage.list_secrets().await.unwrap().len(), 2);

        let moved = manager
            .register_codex_auth_store(second_dir.path())
            .await
            .unwrap();
        assert_eq!(
            moved.iter().map(|secret| secret.id).collect::<Vec<_>>(),
            first.iter().map(|secret| secret.id).collect::<Vec<_>>(),
            "a moved state dir re-points the same records"
        );
        let access = storage
            .get_secret(moved[0].id)
            .await
            .unwrap()
            .expect("access token record");
        assert_eq!(access.name, "codex.access_token");
        assert_eq!(access.backend_kind, FILE_BACKEND);
        assert_eq!(access.material_kind, "oauth_access_token");
        assert_eq!(
            access.external_reference.as_deref(),
            Some(
                second_dir
                    .path()
                    .join("auth.json")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(
            access.metadata[FILE_JSON_POINTER_KEY],
            "/tokens/access_token"
        );

        std::fs::write(
            second_dir.path().join("auth.json"),
            r#"{"tokens":{"access_token":"live-access","refresh_token":"live-refresh"}}"#,
        )
        .unwrap();
        let values = manager.known_values().await.unwrap();
        assert!(values.contains(&"live-access".to_string()));
        assert!(values.contains(&"live-refresh".to_string()));
    }

    /// Redaction reads every backend the same way resolution does, and an
    /// external backend that cannot be read right now contributes nothing
    /// rather than failing the capture.
    #[tokio::test(flavor = "multi_thread")]
    async fn known_values_cover_every_readable_backend() {
        let _guard = with_secret_env();
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let manager = SecretManager::new(storage.clone());
        manager
            .create_secret(CreateSecretRequest {
                name: "managed".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: MANAGED_BACKEND.into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                metadata: None,
                value: Some("managed-value".into()),
                external_reference: None,
            })
            .await
            .unwrap();
        manager
            .create_secret(file_secret("unwritten", &dir.path().join("absent"), None))
            .await
            .unwrap();
        let present = dir.path().join("present");
        std::fs::write(&present, "file-value").unwrap();
        manager
            .create_secret(file_secret("written", &present, None))
            .await
            .unwrap();

        let mut values = manager.known_values().await.unwrap();
        values.sort();
        assert_eq!(
            values,
            vec!["file-value".to_string(), "managed-value".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_tools_secret_access_is_attributed_to_its_session() {
        use swage_storage::store::{SessionStore, TopicStore};
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .unwrap();
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

        manager
            .resolve_for_session(&managed.reference, Some(session.id))
            .await
            .unwrap();
        manager.resolve(&managed.reference).await.unwrap();

        let accesses: Vec<_> = storage
            .list_audit_events()
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.event_kind == "accessed")
            .collect();
        assert_eq!(accesses.len(), 2);
        assert_eq!(
            accesses
                .iter()
                .filter(|event| event.session_id == Some(session.id))
                .count(),
            1,
            "the tool's access names its session; the operator's names none: {accesses:?}"
        );
        assert!(
            accesses
                .iter()
                .all(|event| event.session_id.is_none() || event.session_id == Some(session.id))
        );
    }
}
