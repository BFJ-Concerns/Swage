use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewSecretRecord, SecretRecord};
use crate::sqlite::SqliteStorage;
use crate::store::SecretStore;

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

#[async_trait]
impl SecretStore for SqliteStorage {
    async fn create_secret(&self, secret: NewSecretRecord) -> Result<SecretRecord, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let metadata_json = serde_json::to_string(&secret.metadata)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let reference = format!("secret://{}/{}", secret.backend_kind, secret.id);

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

            if let Some(value) = secret.value {
                conn.execute(
                    "INSERT INTO managed_secret_values (secret_id, value, updated_at)
                     VALUES (?1, ?2, ?3)",
                    params![secret.id.to_string(), value, now_str],
                )?;
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
            conn.execute(
                "UPDATE secrets
                 SET external_reference = ?2, metadata_json = ?3, updated_at = ?4
                 WHERE id = ?1",
                params![id.to_string(), external_reference, metadata_json, now_str],
            )?;
            if conn.changes() == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }

            if let Some(value) = value {
                conn.execute(
                    "INSERT INTO managed_secret_values (secret_id, value, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(secret_id) DO UPDATE SET
                        value = excluded.value,
                        updated_at = excluded.updated_at",
                    params![id.to_string(), value, now_str],
                )?;
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
            let mut stmt = conn.prepare(
                "SELECT value
                 FROM managed_secret_values
                 WHERE secret_id = ?1",
            )?;
            let mut rows = stmt.query(params![id.to_string()])?;
            match rows.next()? {
                Some(row) => Ok(Some(row.get(0)?)),
                None => Ok(None),
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::store::SecretStore;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn secret_round_trips_rotate_and_revoke() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create_secret(NewSecretRecord {
                id: Uuid::new_v4(),
                name: "Node token".into(),
                material_kind: "node_credential".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                external_reference: None,
                metadata: json!({"backend": "attached_node"}),
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
                json!({"backend": "attached_node", "rotated": true}),
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
}
