use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewWorkspaceAttachment, WorkspaceAttachment};
use crate::sqlite::SqliteStorage;
use crate::store::WorkspaceAttachmentStore;

fn parse_uuid(column: usize, value: String) -> rusqlite::Result<Uuid> {
    value.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn parse_dt(column: usize, value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|date| date.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn parse_permissions(value: String) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn serialise_permissions(value: &[String]) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| {
        StorageError::ConstraintViolation(format!(
            "failed to serialise workspace permissions: {error}"
        ))
    })
}

fn row_to_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceAttachment> {
    let id: String = row.get(0)?;
    let bench_id: String = row.get(1)?;
    let permissions_json: String = row.get(4)?;
    let created_at: String = row.get(9)?;
    let updated_at: String = row.get(10)?;

    Ok(WorkspaceAttachment {
        id: parse_uuid(0, id)?,
        bench_id: parse_uuid(1, bench_id)?,
        label: row.get(2)?,
        path: row.get(3)?,
        canonical_path: row.get(6)?,
        is_missing: row.get::<_, i64>(7)? != 0,
        is_default: row.get::<_, i64>(8)? != 0,
        permissions: parse_permissions(permissions_json)?,
        is_trusted: row.get::<_, i64>(5)? != 0,
        created_at: parse_dt(9, created_at)?,
        updated_at: parse_dt(10, updated_at)?,
    })
}

#[async_trait]
impl WorkspaceAttachmentStore for SqliteStorage {
    async fn create_workspace_attachment(
        &self,
        workspace: NewWorkspaceAttachment,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let permissions_json = serialise_permissions(&workspace.permissions)?;
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO workspace_attachments (
                    id, bench_id, label, path, permissions_json, is_trusted, canonical_path, is_missing, is_default, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?4, 0, 0, ?7, ?7)",
                params![
                    id.to_string(),
                    workspace.bench_id.to_string(),
                    &workspace.label,
                    &workspace.path,
                    &permissions_json,
                    if workspace.is_trusted { 1 } else { 0 },
                    now_str
                ],
            )?;

            Ok(WorkspaceAttachment {
                id,
                bench_id: workspace.bench_id,
                label: workspace.label,
                path: workspace.path.clone(),
                canonical_path: workspace.path,
                is_missing: false,
                is_default: false,
                permissions: workspace.permissions,
                is_trusted: workspace.is_trusted,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_workspace_attachment(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkspaceAttachment>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, bench_id, label, path, permissions_json, is_trusted, canonical_path, is_missing, is_default, created_at, updated_at
                 FROM workspace_attachments
                 WHERE id = ?1",
                params![id.to_string()],
                row_to_workspace,
            )
            .optional()
        })
        .await
    }

    async fn list_workspace_attachments_by_bench(
        &self,
        bench_id: Uuid,
    ) -> Result<Vec<WorkspaceAttachment>, StorageError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, bench_id, label, path, permissions_json, is_trusted, canonical_path, is_missing, is_default, created_at, updated_at
                 FROM workspace_attachments
                 WHERE bench_id = ?1
                 ORDER BY created_at ASC, label ASC",
            )?;
            let rows = statement.query_map(params![bench_id.to_string()], row_to_workspace)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn set_workspace_trust(
        &self,
        id: Uuid,
        is_trusted: bool,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let affected = self
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE workspace_attachments
                     SET is_trusted = ?2, updated_at = ?3
                     WHERE id = ?1",
                    params![
                        id.to_string(),
                        if is_trusted { 1 } else { 0 },
                        Utc::now().to_rfc3339()
                    ],
                )
            })
            .await?;
        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "workspace_attachment".to_string(),
                id: id.to_string(),
            });
        }
        self.get_workspace_attachment(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "workspace_attachment".to_string(),
                id: id.to_string(),
            })
    }

    async fn set_workspace_missing_state(
        &self,
        id: Uuid,
        is_missing: bool,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let affected = self
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE workspace_attachments
                     SET is_missing = ?2, updated_at = ?3
                     WHERE id = ?1",
                    params![
                        id.to_string(),
                        if is_missing { 1 } else { 0 },
                        Utc::now().to_rfc3339()
                    ],
                )
            })
            .await?;
        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "workspace_attachment".to_string(),
                id: id.to_string(),
            });
        }
        self.get_workspace_attachment(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "workspace_attachment".to_string(),
                id: id.to_string(),
            })
    }

    async fn set_bench_default_workspace(
        &self,
        bench_id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<(), StorageError> {
        let workspace_id = workspace_id.map(|id| id.to_string());
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            if let Some(ref workspace_id) = workspace_id {
                let found_bench: Option<String> = tx
                    .query_row(
                        "SELECT bench_id FROM workspace_attachments WHERE id = ?1",
                        params![workspace_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                match found_bench {
                    Some(found_bench) if found_bench == bench_id.to_string() => {}
                    Some(_) => {
                        return Err(rusqlite::Error::InvalidParameterName(
                            "workspace does not belong to bench".to_string(),
                        ));
                    }
                    None => return Err(rusqlite::Error::QueryReturnedNoRows),
                }
            }

            tx.execute(
                "UPDATE workspace_attachments
                 SET is_default = 0, updated_at = ?2
                 WHERE bench_id = ?1",
                params![bench_id.to_string(), Utc::now().to_rfc3339()],
            )?;
            if let Some(workspace_id) = workspace_id {
                tx.execute(
                    "UPDATE workspace_attachments
                     SET is_default = 1, updated_at = ?2
                     WHERE id = ?1",
                    params![workspace_id, Utc::now().to_rfc3339()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn delete_workspace_attachment(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM workspace_attachments WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}
