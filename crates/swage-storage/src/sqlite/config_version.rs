use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{ConfigurationVersion, NewConfigurationVersion};
use crate::sqlite::SqliteStorage;
use crate::store::ConfigurationVersionStore;

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_optional_uuid(value: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    value.as_deref().map(parse_uuid).transpose()
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

fn row_to_configuration_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConfigurationVersion> {
    let project_id: Option<String> = row.get(1)?;
    let session_id: Option<String> = row.get(2)?;
    let snapshot_json: String = row.get(9)?;
    let diff_json: String = row.get(10)?;
    let previous_version_id: Option<String> = row.get(11)?;
    let rollback_to_version_id: Option<String> = row.get(12)?;
    let created_at: String = row.get(13)?;

    Ok(ConfigurationVersion {
        id: parse_uuid(&row.get::<_, String>(0)?)?,
        project_id: parse_optional_uuid(project_id)?,
        session_id: parse_optional_uuid(session_id)?,
        entity_kind: row.get(3)?,
        entity_id: row.get(4)?,
        version_number: row.get(5)?,
        operation_kind: row.get(6)?,
        source: row.get(7)?,
        summary: row.get(8)?,
        snapshot: serde_json::from_str(&snapshot_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        diff: serde_json::from_str(&diff_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        previous_version_id: parse_optional_uuid(previous_version_id)?,
        rollback_to_version_id: parse_optional_uuid(rollback_to_version_id)?,
        created_at: parse_dt(&created_at)?,
    })
}

#[async_trait]
impl ConfigurationVersionStore for SqliteStorage {
    async fn create_configuration_version(
        &self,
        version: NewConfigurationVersion,
    ) -> Result<ConfigurationVersion, StorageError> {
        self.with_conn(move |conn| {
            let created_at = Utc::now();
            let created_at_str = created_at.to_rfc3339();
            let snapshot_json = serde_json::to_string(&version.snapshot)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let diff_json = serde_json::to_string(&version.diff)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            conn.execute(
                "INSERT INTO configuration_versions (
                    id, project_id, session_id, entity_kind, entity_id, version_number,
                    operation_kind, source, summary, snapshot_json, diff_json,
                    previous_version_id, rollback_to_version_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    version.id.to_string(),
                    version.project_id.map(|id| id.to_string()),
                    version.session_id.map(|id| id.to_string()),
                    version.entity_kind,
                    version.entity_id,
                    version.version_number,
                    version.operation_kind,
                    version.source,
                    version.summary,
                    snapshot_json,
                    diff_json,
                    version.previous_version_id.map(|id| id.to_string()),
                    version.rollback_to_version_id.map(|id| id.to_string()),
                    created_at_str,
                ],
            )?;
            Ok(ConfigurationVersion {
                id: version.id,
                project_id: version.project_id,
                session_id: version.session_id,
                entity_kind: version.entity_kind,
                entity_id: version.entity_id,
                version_number: version.version_number,
                operation_kind: version.operation_kind,
                source: version.source,
                summary: version.summary,
                snapshot: version.snapshot,
                diff: version.diff,
                previous_version_id: version.previous_version_id,
                rollback_to_version_id: version.rollback_to_version_id,
                created_at,
            })
        })
        .await
    }

    async fn get_configuration_version(
        &self,
        id: Uuid,
    ) -> Result<Option<ConfigurationVersion>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project_id, session_id, entity_kind, entity_id, version_number,
                        operation_kind, source, summary, snapshot_json, diff_json,
                        previous_version_id, rollback_to_version_id, created_at
                 FROM configuration_versions
                 WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id.to_string()])?;
            rows.next()?.map(row_to_configuration_version).transpose()
        })
        .await
    }

    async fn list_configuration_versions(
        &self,
        project_id: Option<Uuid>,
    ) -> Result<Vec<ConfigurationVersion>, StorageError> {
        self.with_conn(move |conn| {
            let sql = if project_id.is_some() {
                "SELECT id, project_id, session_id, entity_kind, entity_id, version_number,
                        operation_kind, source, summary, snapshot_json, diff_json,
                        previous_version_id, rollback_to_version_id, created_at
                 FROM configuration_versions
                 WHERE project_id = ?1
                 ORDER BY created_at DESC"
            } else {
                "SELECT id, project_id, session_id, entity_kind, entity_id, version_number,
                        operation_kind, source, summary, snapshot_json, diff_json,
                        previous_version_id, rollback_to_version_id, created_at
                 FROM configuration_versions
                 ORDER BY created_at DESC"
            };
            let mut stmt = conn.prepare(sql)?;
            let rows = if let Some(project_id) = project_id {
                stmt.query_map(
                    params![project_id.to_string()],
                    row_to_configuration_version,
                )?
            } else {
                stmt.query_map([], row_to_configuration_version)?
            };
            rows.collect()
        })
        .await
    }

    async fn list_configuration_versions_for_entity(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Vec<ConfigurationVersion>, StorageError> {
        let entity_kind = entity_kind.to_string();
        let entity_id = entity_id.to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project_id, session_id, entity_kind, entity_id, version_number,
                        operation_kind, source, summary, snapshot_json, diff_json,
                        previous_version_id, rollback_to_version_id, created_at
                 FROM configuration_versions
                 WHERE entity_kind = ?1 AND entity_id = ?2
                 ORDER BY version_number DESC",
            )?;
            let rows = stmt.query_map(
                params![entity_kind, entity_id],
                row_to_configuration_version,
            )?;
            rows.collect()
        })
        .await
    }

    async fn get_latest_configuration_version(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Option<ConfigurationVersion>, StorageError> {
        let entity_kind = entity_kind.to_string();
        let entity_id = entity_id.to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project_id, session_id, entity_kind, entity_id, version_number,
                        operation_kind, source, summary, snapshot_json, diff_json,
                        previous_version_id, rollback_to_version_id, created_at
                 FROM configuration_versions
                 WHERE entity_kind = ?1 AND entity_id = ?2
                 ORDER BY version_number DESC
                 LIMIT 1",
            )?;
            let mut rows = stmt.query(params![entity_kind, entity_id])?;
            rows.next()?.map(row_to_configuration_version).transpose()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use crate::sqlite::SqliteStorage;
    use crate::store::ConfigurationVersionStore;
    use crate::{NewConfigurationVersion, ProjectStore, SessionStore};

    #[tokio::test(flavor = "multi_thread")]
    async fn configuration_version_round_trips_and_lists_by_entity() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(crate::NewSession {
                project_id: project.id,
                agent_id: "builder".into(),
                metadata: None,
            })
            .await
            .unwrap();
        let first = storage
            .create_configuration_version(NewConfigurationVersion {
                id: Uuid::new_v4(),
                project_id: Some(project.id),
                session_id: Some(session.id),
                entity_kind: "team".into(),
                entity_id: "team-1".into(),
                version_number: 1,
                operation_kind: "create_team".into(),
                source: "test".into(),
                summary: "created team".into(),
                snapshot: json!({"name": "team-1"}),
                diff: json!([{"path": "/name", "before": null, "after": "team-1"}]),
                previous_version_id: None,
                rollback_to_version_id: None,
            })
            .await
            .unwrap();
        let second = storage
            .create_configuration_version(NewConfigurationVersion {
                id: Uuid::new_v4(),
                project_id: Some(project.id),
                session_id: Some(session.id),
                entity_kind: "team".into(),
                entity_id: "team-1".into(),
                version_number: 2,
                operation_kind: "update_team".into(),
                source: "test".into(),
                summary: "updated team".into(),
                snapshot: json!({"name": "team-1", "description": "Updated"}),
                diff: json!([{"path": "/description", "before": null, "after": "Updated"}]),
                previous_version_id: Some(first.id),
                rollback_to_version_id: None,
            })
            .await
            .unwrap();

        let listed = storage
            .list_configuration_versions(Some(project.id))
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second.id);

        let entity_versions = storage
            .list_configuration_versions_for_entity("team", "team-1")
            .await
            .unwrap();
        assert_eq!(entity_versions.len(), 2);
        assert_eq!(entity_versions[0].version_number, 2);

        let latest = storage
            .get_latest_configuration_version("team", "team-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, second.id);

        let fetched = storage
            .get_configuration_version(first.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.snapshot["name"], "team-1");
    }
}
