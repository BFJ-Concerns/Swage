use aes_gcm_siv::{
    Aes256GcmSiv, Nonce,
    aead::{Aead, KeyInit},
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewSecretRecord, SecretRecord};
use crate::sqlite::SqliteStorage;
use crate::store::SecretStore;

pub const MANAGED_SECRET_KEY_ENV: &str = "SWAGE_MANAGED_SECRET_KEY";
const MANAGED_SECRET_ALGORITHM: &str = "aes256-gcm-siv-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedSecretEncryptionDiagnostics {
    pub status: String,
    pub detail: String,
    pub key_source: Option<String>,
}

#[derive(Clone)]
struct ManagedSecretEncryptor {
    cipher: Aes256GcmSiv,
    key_source: String,
}

#[derive(Debug, Clone)]
struct EncryptedManagedSecretValue {
    algorithm: String,
    key_source: String,
    nonce_b64: String,
    ciphertext_b64: String,
}

pub fn managed_secret_encryption_diagnostics() -> ManagedSecretEncryptionDiagnostics {
    match ManagedSecretEncryptor::from_env() {
        Ok(encryptor) => ManagedSecretEncryptionDiagnostics {
            status: "ready".into(),
            detail: format!(
                "managed secrets are encrypted at rest with {MANAGED_SECRET_ALGORITHM}"
            ),
            key_source: Some(encryptor.key_source),
        },
        Err(StorageError::QueryFailed(message)) if message.contains("is not configured") => {
            ManagedSecretEncryptionDiagnostics {
                status: "missing".into(),
                detail: message,
                key_source: None,
            }
        }
        Err(StorageError::QueryFailed(message)) => ManagedSecretEncryptionDiagnostics {
            status: "invalid".into(),
            detail: message,
            key_source: None,
        },
        Err(error) => ManagedSecretEncryptionDiagnostics {
            status: "invalid".into(),
            detail: error.to_string(),
            key_source: None,
        },
    }
}

impl ManagedSecretEncryptor {
    fn from_env() -> Result<Self, StorageError> {
        let encoded = std::env::var(MANAGED_SECRET_KEY_ENV).map_err(|_| {
            StorageError::QueryFailed(format!(
                "managed secret encryption key is not configured; set {MANAGED_SECRET_KEY_ENV} to a base64-encoded 32-byte key"
            ))
        })?;
        let bytes = BASE64.decode(encoded.trim()).map_err(|error| {
            StorageError::QueryFailed(format!(
                "managed secret encryption key in {MANAGED_SECRET_KEY_ENV} is invalid base64: {error}"
            ))
        })?;
        if bytes.len() != 32 {
            return Err(StorageError::QueryFailed(format!(
                "managed secret encryption key in {MANAGED_SECRET_KEY_ENV} must decode to exactly 32 bytes"
            )));
        }
        let cipher = Aes256GcmSiv::new_from_slice(&bytes).map_err(|error| {
            StorageError::QueryFailed(format!(
                "managed secret encryption key in {MANAGED_SECRET_KEY_ENV} is invalid: {error}"
            ))
        })?;
        Ok(Self {
            cipher,
            key_source: format!("env:{MANAGED_SECRET_KEY_ENV}:{}", key_fingerprint(&bytes)),
        })
    }

    fn encrypt_value(&self, plaintext: &str) -> Result<EncryptedManagedSecretValue, StorageError> {
        let mut nonce_bytes = [0_u8; 12];
        rand::fill(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|error| {
                StorageError::QueryFailed(format!(
                    "managed secret encryption failed with {MANAGED_SECRET_ALGORITHM}: {error}"
                ))
            })?;
        Ok(EncryptedManagedSecretValue {
            algorithm: MANAGED_SECRET_ALGORITHM.into(),
            key_source: self.key_source.clone(),
            nonce_b64: BASE64.encode(nonce_bytes),
            ciphertext_b64: BASE64.encode(ciphertext),
        })
    }

    fn decrypt_value(
        &self,
        algorithm: &str,
        key_source: &str,
        nonce_b64: &str,
        ciphertext_b64: &str,
    ) -> Result<String, StorageError> {
        if algorithm != MANAGED_SECRET_ALGORITHM {
            return Err(StorageError::QueryFailed(format!(
                "managed secret encryption algorithm '{algorithm}' is not supported"
            )));
        }
        if key_source != self.key_source {
            return Err(StorageError::QueryFailed(format!(
                "managed secret encryption key mismatch: row expects '{key_source}' but current key is '{}'",
                self.key_source
            )));
        }
        let nonce = BASE64.decode(nonce_b64).map_err(|error| {
            StorageError::QueryFailed(format!("managed secret nonce is invalid base64: {error}"))
        })?;
        let nonce = <&Nonce>::try_from(nonce.as_slice()).map_err(|_| {
            StorageError::QueryFailed("managed secret nonce must decode to exactly 12 bytes".into())
        })?;
        let ciphertext = BASE64.decode(ciphertext_b64).map_err(|error| {
            StorageError::QueryFailed(format!(
                "managed secret ciphertext is invalid base64: {error}"
            ))
        })?;
        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|error| {
                StorageError::QueryFailed(format!(
                    "managed secret decryption failed with {MANAGED_SECRET_ALGORITHM}: {error}"
                ))
            })?;
        String::from_utf8(plaintext).map_err(|error| {
            StorageError::QueryFailed(format!(
                "managed secret plaintext is not valid UTF-8: {error}"
            ))
        })
    }
}

fn key_fingerprint(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest[..8].iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn managed_secret_encryptor() -> Result<ManagedSecretEncryptor, StorageError> {
    ManagedSecretEncryptor::from_env()
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn row_to_secret(row: &rusqlite::Row<'_>) -> rusqlite::Result<SecretRecord> {
    let id: String = row.get(0)?;
    let metadata_json: String = row.get(9)?;
    let created_at: String = row.get(11)?;
    let updated_at: String = row.get(12)?;
    let revoked_at: Option<String> = row.get(13)?;
    Ok(SecretRecord {
        id: parse_uuid(&id)?,
        name: row.get(1)?,
        material_kind: row.get(2)?,
        owner_kind: row.get(3)?,
        backend_kind: row.get(4)?,
        scope_kind: row.get(5)?,
        scope_id: row.get(6)?,
        reference: row.get(7)?,
        external_reference: row.get(8)?,
        metadata: serde_json::from_str(&metadata_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        status: row.get(10)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
        revoked_at: revoked_at.as_deref().map(parse_dt).transpose()?,
    })
}

fn store_managed_secret_value(
    conn: &Connection,
    id: Uuid,
    plaintext: &str,
    now_str: &str,
) -> Result<(), rusqlite::Error> {
    let encrypted = managed_secret_encryptor()
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
        .encrypt_value(plaintext)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    conn.execute(
        "INSERT INTO managed_secret_values (
            secret_id, algorithm, key_source, nonce_b64, ciphertext_b64, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(secret_id) DO UPDATE SET
            algorithm = excluded.algorithm,
            key_source = excluded.key_source,
            nonce_b64 = excluded.nonce_b64,
            ciphertext_b64 = excluded.ciphertext_b64,
            updated_at = excluded.updated_at",
        params![
            id.to_string(),
            encrypted.algorithm,
            encrypted.key_source,
            encrypted.nonce_b64,
            encrypted.ciphertext_b64,
            now_str,
        ],
    )?;
    Ok(())
}

pub(crate) fn migrate_managed_secret_storage(conn: &Connection) -> Result<(), StorageError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(managed_secret_values)")
        .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| StorageError::MigrationFailed(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
    if columns.is_empty() || columns.iter().any(|column| column == "ciphertext_b64") {
        return Ok(());
    }
    if !columns.iter().any(|column| column == "value") {
        return Err(StorageError::MigrationFailed(
            "managed_secret_values has an unrecognised schema".into(),
        ));
    }

    let row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM managed_secret_values", [], |row| {
            row.get(0)
        })
        .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
    let encryptor = if row_count > 0 {
        Some(managed_secret_encryptor().map_err(|error| {
            StorageError::MigrationFailed(format!(
                "cannot migrate plaintext managed secrets to encrypted storage: {error}"
            ))
        })?)
    } else {
        None
    };

    conn.execute_batch(
        "BEGIN;
         ALTER TABLE managed_secret_values RENAME TO managed_secret_values_legacy;
         CREATE TABLE managed_secret_values (
             secret_id       TEXT NOT NULL PRIMARY KEY REFERENCES secrets(id) ON DELETE CASCADE,
             algorithm       TEXT NOT NULL,
             key_source      TEXT NOT NULL,
             nonce_b64       TEXT NOT NULL,
             ciphertext_b64  TEXT NOT NULL,
             updated_at      TEXT NOT NULL
         );",
    )
    .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;

    let migration_result = (|| -> Result<(), StorageError> {
        let mut stmt = conn
            .prepare(
                "SELECT secret_id, value, updated_at
                 FROM managed_secret_values_legacy",
            )
            .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
        for row in rows {
            let (secret_id, value, updated_at) =
                row.map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
            let encrypted = encryptor
                .as_ref()
                .expect("encryptor must exist when plaintext rows exist")
                .encrypt_value(&value)
                .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
            conn.execute(
                "INSERT INTO managed_secret_values (
                    secret_id, algorithm, key_source, nonce_b64, ciphertext_b64, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    secret_id,
                    encrypted.algorithm,
                    encrypted.key_source,
                    encrypted.nonce_b64,
                    encrypted.ciphertext_b64,
                    updated_at,
                ],
            )
            .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
        }
        conn.execute_batch("DROP TABLE managed_secret_values_legacy; COMMIT;")
            .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
        Ok(())
    })();

    if let Err(error) = migration_result {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(error);
    }

    Ok(())
}

#[async_trait]
impl SecretStore for SqliteStorage {
    async fn create_secret(&self, secret: NewSecretRecord) -> Result<SecretRecord, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let metadata_json = serde_json::to_string(&secret.metadata)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let reference = format!("secret://{}/{}", secret.backend_kind, secret.id);

            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| -> Result<SecretRecord, rusqlite::Error> {
                conn.execute(
                    "INSERT INTO secrets (
                        id, name, material_kind, owner_kind, backend_kind, scope_kind, scope_id,
                        reference, external_reference, metadata_json, status, created_at, updated_at, revoked_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', ?11, ?12, NULL)",
                    params![
                        secret.id.to_string(),
                        secret.name,
                        secret.material_kind,
                        secret.owner_kind,
                        secret.backend_kind,
                        secret.scope_kind,
                        secret.scope_id,
                        reference,
                        secret.external_reference,
                        metadata_json,
                        now_str,
                        now_str,
                    ],
                )?;

                if let Some(value) = secret.value.as_deref() {
                    store_managed_secret_value(conn, secret.id, value, &now_str)?;
                }

                Ok(SecretRecord {
                    id: secret.id,
                    name: secret.name,
                    material_kind: secret.material_kind,
                    owner_kind: secret.owner_kind,
                    backend_kind: secret.backend_kind.clone(),
                    scope_kind: secret.scope_kind,
                    scope_id: secret.scope_id,
                    reference,
                    external_reference: secret.external_reference,
                    metadata: secret.metadata,
                    status: "active".to_string(),
                    created_at: now,
                    updated_at: now,
                    revoked_at: None,
                })
            })();
            match result {
                Ok(record) => {
                    conn.execute_batch("COMMIT")?;
                    Ok(record)
                }
                Err(error) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        })
        .await
    }

    async fn list_secrets(&self) -> Result<Vec<SecretRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, material_kind, owner_kind, backend_kind, scope_kind, scope_id,
                        reference, external_reference, metadata_json, status, created_at, updated_at, revoked_at
                 FROM secrets
                 ORDER BY created_at DESC, id DESC",
            )?;
            let rows = stmt.query_map([], row_to_secret)?;
            rows.collect()
        })
        .await
    }

    async fn get_secret(&self, id: Uuid) -> Result<Option<SecretRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, material_kind, owner_kind, backend_kind, scope_kind, scope_id,
                        reference, external_reference, metadata_json, status, created_at, updated_at, revoked_at
                 FROM secrets
                 WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id.to_string()])?;
            match rows.next()? {
                Some(row) => row_to_secret(row).map(Some),
                None => Ok(None),
            }
        })
        .await
    }

    async fn update_secret_value(
        &self,
        id: Uuid,
        value: Option<String>,
        external_reference: Option<String>,
        metadata: serde_json::Value,
    ) -> Result<SecretRecord, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let metadata_json = serde_json::to_string(&metadata)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| -> Result<SecretRecord, rusqlite::Error> {
                conn.execute(
                    "UPDATE secrets
                     SET external_reference = ?2, metadata_json = ?3, updated_at = ?4
                     WHERE id = ?1",
                    params![id.to_string(), external_reference, metadata_json, now_str],
                )?;
                if conn.changes() == 0 {
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }

                if let Some(value) = value.as_deref() {
                    store_managed_secret_value(conn, id, value, &now_str)?;
                }

                let mut stmt = conn.prepare(
                    "SELECT id, name, material_kind, owner_kind, backend_kind, scope_kind, scope_id,
                            reference, external_reference, metadata_json, status, created_at, updated_at, revoked_at
                     FROM secrets
                     WHERE id = ?1",
                )?;
                stmt.query_row(params![id.to_string()], row_to_secret)
            })();
            match result {
                Ok(record) => {
                    conn.execute_batch("COMMIT")?;
                    Ok(record)
                }
                Err(error) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        })
        .await
    }

    async fn revoke_secret(&self, id: Uuid) -> Result<SecretRecord, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "UPDATE secrets
                 SET status = 'revoked', updated_at = ?2, revoked_at = ?2
                 WHERE id = ?1",
                params![id.to_string(), now_str],
            )?;
            if conn.changes() == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            let mut stmt = conn.prepare(
                "SELECT id, name, material_kind, owner_kind, backend_kind, scope_kind, scope_id,
                        reference, external_reference, metadata_json, status, created_at, updated_at, revoked_at
                 FROM secrets
                 WHERE id = ?1",
            )?;
            stmt.query_row(params![id.to_string()], row_to_secret)
        })
        .await
    }

    async fn restore_secret(&self, id: Uuid) -> Result<SecretRecord, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "UPDATE secrets
                 SET status = 'active', updated_at = ?2, revoked_at = NULL
                 WHERE id = ?1",
                params![id.to_string(), now_str],
            )?;
            if conn.changes() == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            let mut stmt = conn.prepare(
                "SELECT id, name, material_kind, owner_kind, backend_kind, scope_kind, scope_id,
                        reference, external_reference, metadata_json, status, created_at, updated_at, revoked_at
                 FROM secrets
                 WHERE id = ?1",
            )?;
            stmt.query_row(params![id.to_string()], row_to_secret)
        })
        .await
    }

    async fn get_managed_secret_value(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        self.with_conn(move |conn| {
            let encryptor = managed_secret_encryptor()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let mut stmt = conn.prepare(
                "SELECT algorithm, key_source, nonce_b64, ciphertext_b64
                 FROM managed_secret_values
                 WHERE secret_id = ?1",
            )?;
            let mut rows = stmt.query(params![id.to_string()])?;
            match rows.next()? {
                Some(row) => {
                    let algorithm: String = row.get(0)?;
                    let key_source: String = row.get(1)?;
                    let nonce_b64: String = row.get(2)?;
                    let ciphertext_b64: String = row.get(3)?;
                    let value = encryptor
                        .decrypt_value(&algorithm, &key_source, &nonce_b64, &ciphertext_b64)
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    Ok(Some(value))
                }
                None => Ok(None),
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::store::SecretStore;
    use serde_json::json;
    use std::sync::{Mutex, MutexGuard};

    // These tests mutate process-wide environment variables. The guard is
    // intentionally held across awaits so concurrent secret tests cannot race
    // each other while resolving the managed encryption key.
    static MANAGED_SECRET_KEY_LOCK: Mutex<()> = Mutex::new(());

    fn with_managed_secret_key() -> MutexGuard<'static, ()> {
        let guard = MANAGED_SECRET_KEY_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(MANAGED_SECRET_KEY_ENV, BASE64.encode([7_u8; 32]));
        }
        guard
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn secret_round_trips_rotate_and_revoke() {
        let _guard = with_managed_secret_key();
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create_secret(NewSecretRecord {
                id: Uuid::new_v4(),
                name: "Device token".into(),
                material_kind: "device_credential".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                external_reference: None,
                metadata: json!({"backend": "attached_device"}),
                value: Some("secret-a".into()),
            })
            .await
            .unwrap();
        assert_eq!(created.status, "active");
        assert_eq!(
            storage.get_managed_secret_value(created.id).await.unwrap(),
            Some("secret-a".into())
        );

        let rotated = storage
            .update_secret_value(
                created.id,
                Some("secret-b".into()),
                None,
                json!({"backend": "attached_device", "rotated": true}),
            )
            .await
            .unwrap();
        assert_eq!(rotated.metadata["rotated"], true);
        assert_eq!(
            storage.get_managed_secret_value(created.id).await.unwrap(),
            Some("secret-b".into())
        );

        let revoked = storage.revoke_secret(created.id).await.unwrap();
        assert_eq!(revoked.status, "revoked");

        let restored = storage.restore_secret(created.id).await.unwrap();
        assert_eq!(restored.status, "active");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn managed_secret_values_are_encrypted_at_rest() {
        let _guard = with_managed_secret_key();
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create_secret(NewSecretRecord {
                id: Uuid::new_v4(),
                name: "Encrypted value".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                external_reference: None,
                metadata: json!({}),
                value: Some("super-secret-value".into()),
            })
            .await
            .unwrap();

        let stored = storage
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT algorithm, key_source, nonce_b64, ciphertext_b64
                     FROM managed_secret_values
                     WHERE secret_id = ?1",
                    params![created.id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
            })
            .await
            .unwrap();
        assert_eq!(stored.0, MANAGED_SECRET_ALGORITHM);
        assert!(stored.1.starts_with("env:SWAGE_MANAGED_SECRET_KEY:"));
        assert!(!stored.3.contains("super-secret-value"));
        assert_eq!(
            storage.get_managed_secret_value(created.id).await.unwrap(),
            Some("super-secret-value".into())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_managed_secret_key_reports_clear_diagnostic() {
        let _guard = MANAGED_SECRET_KEY_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var(MANAGED_SECRET_KEY_ENV);
        }

        let storage = SqliteStorage::in_memory().unwrap();
        let error = storage
            .create_secret(NewSecretRecord {
                id: Uuid::new_v4(),
                name: "Missing key".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                external_reference: None,
                metadata: json!({}),
                value: Some("secret-value".into()),
            })
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(MANAGED_SECRET_KEY_ENV) && message.contains("not configured"),
            "unexpected diagnostic: {message}"
        );
        assert!(storage.list_secrets().await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_plaintext_rows_are_migrated_without_losing_secret_ids() {
        let _guard = with_managed_secret_key();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE secrets (
                id                 TEXT NOT NULL PRIMARY KEY,
                name               TEXT NOT NULL,
                material_kind      TEXT NOT NULL,
                owner_kind         TEXT NOT NULL,
                backend_kind       TEXT NOT NULL,
                scope_kind         TEXT NOT NULL,
                scope_id           TEXT,
                reference          TEXT NOT NULL UNIQUE,
                external_reference TEXT,
                metadata_json      TEXT NOT NULL,
                status             TEXT NOT NULL,
                created_at         TEXT NOT NULL,
                updated_at         TEXT NOT NULL,
                revoked_at         TEXT
            );
            CREATE TABLE managed_secret_values (
                secret_id  TEXT NOT NULL PRIMARY KEY REFERENCES secrets(id) ON DELETE CASCADE,
                value      TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .unwrap();
        let secret_id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO secrets (
                id, name, material_kind, owner_kind, backend_kind, scope_kind, scope_id,
                reference, external_reference, metadata_json, status, created_at, updated_at, revoked_at
             ) VALUES (?1, 'Legacy token', 'provider_key', 'service', 'managed', 'gateway', NULL,
                       ?2, NULL, '{}', 'active', ?3, ?3, NULL)",
            params![
                secret_id.to_string(),
                format!("secret://managed/{secret_id}"),
                now,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO managed_secret_values (secret_id, value, updated_at)
             VALUES (?1, 'legacy-secret', ?2)",
            params![secret_id.to_string(), now],
        )
        .unwrap();

        migrate_managed_secret_storage(&conn).unwrap();

        let columns = conn
            .prepare("PRAGMA table_info(managed_secret_values)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "ciphertext_b64"));
        assert!(!columns.iter().any(|column| column == "value"));

        let record = conn
            .query_row(
                "SELECT secret_id, key_source, ciphertext_b64 FROM managed_secret_values",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(record.0, secret_id.to_string());
        assert!(record.1.starts_with("env:SWAGE_MANAGED_SECRET_KEY:"));
        assert!(!record.2.contains("legacy-secret"));

        let storage = SqliteStorage {
            conn: tokio::sync::Mutex::new(conn),
        };
        let restored = storage.get_managed_secret_value(secret_id).await.unwrap();
        assert_eq!(restored, Some("legacy-secret".into()));
    }
}
