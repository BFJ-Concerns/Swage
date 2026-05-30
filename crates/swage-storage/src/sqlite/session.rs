use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewSession, Session};
use crate::sqlite::SqliteStorage;
use crate::store::SessionStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

pub(crate) fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let id: String = row.get(0)?;
    let bench_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let metadata_json: Option<String> = row.get(3)?;
    let active_workspace_attachment_id: Option<String> = row.get(4)?;
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;

    Ok(Session {
        id: parse_uuid(&id)?,
        bench_id: parse_uuid(&bench_id)?,
        agent_id,
        metadata: parse_metadata(metadata_json.as_deref())?,
        active_workspace_attachment_id: active_workspace_attachment_id
            .as_deref()
            .map(parse_uuid)
            .transpose()?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn parse_metadata(metadata_json: Option<&str>) -> rusqlite::Result<Option<serde_json::Value>> {
    metadata_json
        .map(|json| {
            serde_json::from_str(json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn serialise_metadata(
    metadata: Option<&serde_json::Value>,
) -> Result<Option<String>, rusqlite::Error> {
    metadata
        .map(|value| {
            serde_json::to_string(value)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
        })
        .transpose()
}

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for SqliteStorage {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError> {
        let metadata_json = serialise_metadata(session.metadata.as_ref())
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO sessions (id, bench_id, agent_id, metadata_json, active_workspace_attachment_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    id.to_string(),
                    session.bench_id.to_string(),
                    &session.agent_id,
                    &metadata_json,
                    session
                        .active_workspace_attachment_id
                        .map(|id| id.to_string()),
                    &now_str
                ],
            )?;

            Ok(Session {
                id,
                bench_id: session.bench_id,
                agent_id: session.agent_id,
                metadata: session.metadata,
                active_workspace_attachment_id: session.active_workspace_attachment_id,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, bench_id, agent_id, metadata_json, active_workspace_attachment_id, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id.to_string()],
                row_to_session,
            )
            .optional()
        })
        .await
    }

    async fn list_by_bench(&self, bench_id: Uuid) -> Result<Vec<Session>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, bench_id, agent_id, metadata_json, active_workspace_attachment_id, created_at, updated_at
                 FROM sessions WHERE bench_id = ?1
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map(params![bench_id.to_string()], row_to_session)?;
            rows.collect()
        })
        .await
    }

    async fn list_sessions_by_agent_id(
        &self,
        agent_id: &str,
    ) -> Result<Vec<Session>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, bench_id, agent_id, metadata_json, active_workspace_attachment_id, created_at, updated_at
                 FROM sessions WHERE agent_id = ?1
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map(params![&agent_id], row_to_session)?;
            rows.collect()
        })
        .await
    }

    async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(|conn| {
            let affected = conn.execute(
                "DELETE FROM sessions WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    async fn update_activity(&self, id: Uuid) -> Result<(), StorageError> {
        self.with_conn(|conn| {
            let now_str = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![&now_str, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn set_session_active_workspace(
        &self,
        id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<Session, StorageError> {
        let affected = self
            .with_conn(move |conn| {
                let now_str = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE sessions
                     SET active_workspace_attachment_id = ?2, updated_at = ?3
                     WHERE id = ?1",
                    params![
                        id.to_string(),
                        workspace_id.map(|workspace_id| workspace_id.to_string()),
                        now_str
                    ],
                )
            })
            .await?;
        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            });
        }
        self.get_session(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })
    }

    async fn find_session_by_channel_origin(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<Session>, StorageError> {
        let integration = integration.to_string();
        let account = account.to_string();
        let conversation = conversation.to_string();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, bench_id, agent_id, metadata_json, active_workspace_attachment_id, created_at, updated_at
                 FROM sessions
                 WHERE json_extract(metadata_json, '$.channel_origin.integration') = ?1
                   AND json_extract(metadata_json, '$.channel_origin.account') = ?2
                   AND json_extract(metadata_json, '$.channel_origin.conversation') = ?3
                 ORDER BY updated_at DESC
                 LIMIT 1",
                params![integration, account, conversation],
                row_to_session,
            )
            .optional()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewBench, NewWorkspaceAttachment};
    use crate::store::{BenchStore, WorkspaceAttachmentStore};

    use super::*;

    async fn make_storage_with_bench() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let bench = storage
            .create(NewBench {
                name: "test".into(),
                slug: "test".into(),
            })
            .await
            .unwrap();
        (storage, bench.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_session() {
        let (storage, bench_id) = make_storage_with_bench().await;
        let session = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "test-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        assert_eq!(session.bench_id, bench_id);

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, session.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_sessions_ordered_by_updated_at_desc() {
        let (storage, bench_id) = make_storage_with_bench().await;

        let s1 = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "test-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        // Bump s1 so it has a later updated_at.
        storage.update_activity(s1.id).await.unwrap();
        let s2 = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "test-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage.update_activity(s2.id).await.unwrap();

        let sessions = storage.list_by_bench(bench_id).await.unwrap();
        assert_eq!(sessions.len(), 2);
        // Most-recently-updated appears first.
        assert!(sessions[0].updated_at >= sessions[1].updated_at);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_session_returns_true_then_false() {
        let (storage, bench_id) = make_storage_with_bench().await;
        let session = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "test-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        assert!(storage.delete_session(session.id).await.unwrap());
        assert!(!storage.delete_session(session.id).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_stores_agent_id() {
        let (storage, bench_id) = make_storage_with_bench().await;
        let session = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "my-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        assert_eq!(session.agent_id, "my-agent");

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.agent_id, "my-agent");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_round_trips_metadata() {
        let (storage, bench_id) = make_storage_with_bench().await;
        let metadata = serde_json::json!({
            "host": "embedder",
            "labels": ["demo", "cli"]
        });

        let session = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "my-agent".to_string(),
                metadata: Some(metadata.clone()),
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        assert_eq!(session.metadata, Some(metadata.clone()));

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.metadata, Some(metadata));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_active_workspace_round_trips_and_updates() {
        let (storage, bench_id) = make_storage_with_bench().await;
        let workspace = storage
            .create_workspace_attachment(NewWorkspaceAttachment {
                bench_id,
                label: "repo".to_string(),
                path: "/tmp/swage-test-repo".to_string(),
                permissions: vec!["read".to_string(), "write".to_string()],
                is_trusted: true,
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "my-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: Some(workspace.id),
            })
            .await
            .unwrap();

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.active_workspace_attachment_id, Some(workspace.id));

        let updated = storage
            .set_session_active_workspace(session.id, None)
            .await
            .unwrap();
        assert_eq!(updated.active_workspace_attachment_id, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_agent_id_filters_and_orders() {
        let (storage, bench_id) = make_storage_with_bench().await;

        // Create sessions for two different agents.
        let s1 = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "alpha".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let _s2 = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "beta".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let s3 = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "alpha".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        // Bump s1 so it's more recently updated than s3.
        storage.update_activity(s1.id).await.unwrap();

        let alpha_sessions = storage.list_sessions_by_agent_id("alpha").await.unwrap();
        assert_eq!(
            alpha_sessions.len(),
            2,
            "should only return alpha's sessions"
        );
        // Most recently updated first.
        assert_eq!(alpha_sessions[0].id, s1.id);
        assert_eq!(alpha_sessions[1].id, s3.id);

        let beta_sessions = storage.list_sessions_by_agent_id("beta").await.unwrap();
        assert_eq!(beta_sessions.len(), 1);

        let none = storage.list_sessions_by_agent_id("gamma").await.unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cascade_delete_removes_sessions() {
        // Deleting a session should also remove its turns via ON DELETE CASCADE.
        let (storage, bench_id) = make_storage_with_bench().await;
        let session = storage
            .create_session(NewSession {
                bench_id,
                agent_id: "test-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage.delete_session(session.id).await.unwrap();

        assert!(storage.get_session(session.id).await.unwrap().is_none());
    }
}
