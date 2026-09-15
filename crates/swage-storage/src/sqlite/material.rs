use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    AttachmentScopeKind, Material, MaterialSourceKind, MaterialStatus, NewMaterial,
};
use crate::sqlite::SqliteStorage;
use crate::store::MaterialStore;

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

fn row_to_material(row: &rusqlite::Row<'_>) -> rusqlite::Result<Material> {
    let id: String = row.get(0)?;
    let session_id: Option<String> = row.get(2)?;
    let topic_id: Option<String> = row.get(3)?;
    let source_attachment_id: Option<String> = row.get(5)?;
    let created_at: String = row.get(10)?;
    let updated_at: String = row.get(11)?;
    let archived_at: Option<String> = row.get(12)?;
    Ok(Material {
        id: parse_uuid(&id)?,
        scope_kind: row
            .get::<_, String>(1)?
            .parse::<AttachmentScopeKind>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        session_id: session_id.as_deref().map(parse_uuid).transpose()?,
        topic_id: topic_id.as_deref().map(parse_uuid).transpose()?,
        source_kind: row
            .get::<_, String>(4)?
            .parse::<MaterialSourceKind>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        source_attachment_id: source_attachment_id
            .as_deref()
            .map(parse_uuid)
            .transpose()?,
        status: row
            .get::<_, String>(6)?
            .parse::<MaterialStatus>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        title: row.get(7)?,
        relative_path: row.get(8)?,
        mime_type: row.get(9)?,
        content_text: row.get(13)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
        archived_at: archived_at.as_deref().map(parse_dt).transpose()?,
    })
}

const MATERIAL_SELECT: &str = "SELECT
        id,
        scope_kind,
        session_id,
        topic_id,
        source_kind,
        source_attachment_id,
        status,
        title,
        relative_path,
        mime_type,
        created_at,
        updated_at,
        archived_at,
        content_text
    FROM materials";

#[async_trait]
impl MaterialStore for SqliteStorage {
    async fn create_material(&self, material: NewMaterial) -> Result<Material, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO materials (
                    id, scope_kind, session_id, topic_id, source_kind, source_attachment_id,
                    status, title, relative_path, mime_type, content_text, created_at, updated_at, archived_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9, ?10, ?11, ?11, NULL)",
                params![
                    material.id.to_string(),
                    material.scope_kind.to_string(),
                    material.session_id.map(|id| id.to_string()),
                    material.topic_id.map(|id| id.to_string()),
                    material.source_kind.to_string(),
                    material.source_attachment_id.map(|id| id.to_string()),
                    material.title,
                    material.relative_path,
                    material.mime_type,
                    material.content_text,
                    now,
                ],
            )?;
            conn.query_row(
                &format!("{MATERIAL_SELECT} WHERE id = ?1"),
                params![material.id.to_string()],
                row_to_material,
            )
        })
        .await
    }

    async fn get_material(&self, id: Uuid) -> Result<Option<Material>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!("{MATERIAL_SELECT} WHERE id = ?1"))?;
            let mut rows = stmt.query(params![id.to_string()])?;
            match rows.next()? {
                Some(row) => row_to_material(row).map(Some),
                None => Ok(None),
            }
        })
        .await
    }

    async fn get_material_by_source_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Option<Material>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{MATERIAL_SELECT} WHERE source_attachment_id = ?1"
            ))?;
            let mut rows = stmt.query(params![attachment_id.to_string()])?;
            match rows.next()? {
                Some(row) => row_to_material(row).map(Some),
                None => Ok(None),
            }
        })
        .await
    }

    async fn list_materials_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Material>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{MATERIAL_SELECT} WHERE session_id = ?1 ORDER BY created_at ASC, id ASC"
            ))?;
            stmt.query_map(params![session_id.to_string()], row_to_material)?
                .collect()
        })
        .await
    }

    async fn list_materials_by_topic(&self, topic_id: Uuid) -> Result<Vec<Material>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{MATERIAL_SELECT} WHERE topic_id = ?1 AND scope_kind = 'topic' ORDER BY created_at ASC, id ASC"
            ))?;
            stmt.query_map(params![topic_id.to_string()], row_to_material)?
                .collect()
        })
        .await
    }

    async fn update_material_content(
        &self,
        id: Uuid,
        content_text: &str,
        mime_type: &str,
    ) -> Result<Material, StorageError> {
        let content_text = content_text.to_string();
        let mime_type = mime_type.to_string();
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE materials
                 SET content_text = ?2, mime_type = ?3, status = 'active', archived_at = NULL, updated_at = ?4
                 WHERE id = ?1",
                params![id.to_string(), content_text, mime_type, now],
            )?;
            conn.query_row(
                &format!("{MATERIAL_SELECT} WHERE id = ?1"),
                params![id.to_string()],
                row_to_material,
            )
        })
        .await
    }

    async fn archive_material(&self, id: Uuid) -> Result<Material, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE materials
                 SET status = 'archived', archived_at = ?2, updated_at = ?2
                 WHERE id = ?1",
                params![id.to_string(), now],
            )?;
            conn.query_row(
                &format!("{MATERIAL_SELECT} WHERE id = ?1"),
                params![id.to_string()],
                row_to_material,
            )
        })
        .await
    }

    async fn restore_material(&self, id: Uuid) -> Result<Material, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE materials
                 SET status = 'active', archived_at = NULL, updated_at = ?2
                 WHERE id = ?1",
                params![id.to_string(), now],
            )?;
            conn.query_row(
                &format!("{MATERIAL_SELECT} WHERE id = ?1"),
                params![id.to_string()],
                row_to_material,
            )
        })
        .await
    }

    async fn promote_material(
        &self,
        id: Uuid,
        scope_kind: AttachmentScopeKind,
        session_id: Option<Uuid>,
        topic_id: Option<Uuid>,
        relative_path: &str,
    ) -> Result<Material, StorageError> {
        let relative_path = relative_path.to_string();
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE materials
                 SET scope_kind = ?2,
                     session_id = ?3,
                     topic_id = ?4,
                     relative_path = ?5,
                     status = 'active',
                     archived_at = NULL,
                     updated_at = ?6
                 WHERE id = ?1",
                params![
                    id.to_string(),
                    scope_kind.to_string(),
                    session_id.map(|id| id.to_string()),
                    topic_id.map(|id| id.to_string()),
                    relative_path,
                    now,
                ],
            )?;
            conn.query_row(
                &format!("{MATERIAL_SELECT} WHERE id = ?1"),
                params![id.to_string()],
                row_to_material,
            )
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SessionStore, TopicStore};

    #[tokio::test(flavor = "multi_thread")]
    async fn material_round_trips_archive_restore_and_promote() {
        let storage = SqliteStorage::in_memory().unwrap();
        let material_id = Uuid::new_v4();
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(crate::model::NewSession {
                topic_id: topic.id,
                agent_id: "gateway".into(),
                origin: crate::model::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let session_id = session.id;
        let topic_id = topic.id;

        let created = storage
            .create_material(NewMaterial {
                id: material_id,
                scope_kind: AttachmentScopeKind::Session,
                session_id: Some(session_id),
                topic_id: Some(topic_id),
                source_kind: MaterialSourceKind::Manual,
                source_attachment_id: None,
                title: "Draft".into(),
                relative_path: "notes/draft.md".into(),
                mime_type: "text/markdown".into(),
                content_text: "hello".into(),
            })
            .await
            .unwrap();
        assert_eq!(created.status, MaterialStatus::Active);

        let archived = storage.archive_material(material_id).await.unwrap();
        assert_eq!(archived.status, MaterialStatus::Archived);
        assert!(archived.archived_at.is_some());

        let restored = storage.restore_material(material_id).await.unwrap();
        assert_eq!(restored.status, MaterialStatus::Active);
        assert!(restored.archived_at.is_none());

        let promoted = storage
            .promote_material(
                material_id,
                AttachmentScopeKind::Topic,
                None,
                Some(topic_id),
                "shared/draft.md",
            )
            .await
            .unwrap();
        assert_eq!(promoted.scope_kind, AttachmentScopeKind::Topic);
        assert_eq!(promoted.session_id, None);
        assert_eq!(promoted.topic_id, Some(topic_id));
        assert_eq!(promoted.relative_path, "shared/draft.md");

        let updated = storage
            .update_material_content(material_id, "updated", "text/plain")
            .await
            .unwrap();
        assert_eq!(updated.content_text, "updated");
        assert_eq!(updated.mime_type, "text/plain");
    }
}
