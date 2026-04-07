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

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let id: String = row.get(0)?;
    let project_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let created: String = row.get(3)?;
    let updated: String = row.get(4)?;

    Ok(Session {
        id: parse_uuid(&id)?,
        project_id: parse_uuid(&project_id)?,
        agent_id,
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

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for SqliteStorage {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO sessions (id, project_id, agent_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![
                    id.to_string(),
                    session.project_id.to_string(),
                    &session.agent_id,
                    &now_str
                ],
            )?;

            Ok(Session {
                id,
                project_id: session.project_id,
                agent_id: session.agent_id,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, project_id, agent_id, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id.to_string()],
                row_to_session,
            )
            .optional()
        })
        .await
    }

    async fn list_by_project(&self, project_id: Uuid) -> Result<Vec<Session>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project_id, agent_id, created_at, updated_at
                 FROM sessions WHERE project_id = ?1
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map(params![project_id.to_string()], row_to_session)?;
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
                "SELECT id, project_id, agent_id, created_at, updated_at
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
}

#[cfg(test)]
mod tests {
    use crate::model::NewProject;
    use crate::store::ProjectStore;

    use super::*;

    async fn make_storage_with_project() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject {
                name: "test".into(),
                slug: "test".into(),
                workspace_path: None,
            })
            .await
            .unwrap();
        (storage, project.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_session() {
        let (storage, project_id) = make_storage_with_project().await;
        let session = storage
            .create_session(NewSession {
                project_id,
                agent_id: "test-agent".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(session.project_id, project_id);

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, session.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_sessions_ordered_by_updated_at_desc() {
        let (storage, project_id) = make_storage_with_project().await;

        let s1 = storage
            .create_session(NewSession {
                project_id,
                agent_id: "test-agent".to_string(),
            })
            .await
            .unwrap();
        // Bump s1 so it has a later updated_at.
        storage.update_activity(s1.id).await.unwrap();
        let s2 = storage
            .create_session(NewSession {
                project_id,
                agent_id: "test-agent".to_string(),
            })
            .await
            .unwrap();
        storage.update_activity(s2.id).await.unwrap();

        let sessions = storage.list_by_project(project_id).await.unwrap();
        assert_eq!(sessions.len(), 2);
        // Most-recently-updated appears first.
        assert!(sessions[0].updated_at >= sessions[1].updated_at);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_session_returns_true_then_false() {
        let (storage, project_id) = make_storage_with_project().await;
        let session = storage
            .create_session(NewSession {
                project_id,
                agent_id: "test-agent".to_string(),
            })
            .await
            .unwrap();

        assert!(storage.delete_session(session.id).await.unwrap());
        assert!(!storage.delete_session(session.id).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_stores_agent_id() {
        let (storage, project_id) = make_storage_with_project().await;
        let session = storage
            .create_session(NewSession {
                project_id,
                agent_id: "my-agent".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(session.agent_id, "my-agent");

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.agent_id, "my-agent");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_agent_id_filters_and_orders() {
        let (storage, project_id) = make_storage_with_project().await;

        // Create sessions for two different agents.
        let s1 = storage
            .create_session(NewSession {
                project_id,
                agent_id: "alpha".to_string(),
            })
            .await
            .unwrap();
        let _s2 = storage
            .create_session(NewSession {
                project_id,
                agent_id: "beta".to_string(),
            })
            .await
            .unwrap();
        let s3 = storage
            .create_session(NewSession {
                project_id,
                agent_id: "alpha".to_string(),
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
        let (storage, project_id) = make_storage_with_project().await;
        let session = storage
            .create_session(NewSession {
                project_id,
                agent_id: "test-agent".to_string(),
            })
            .await
            .unwrap();
        storage.delete_session(session.id).await.unwrap();

        assert!(storage.get_session(session.id).await.unwrap().is_none());
    }
}
