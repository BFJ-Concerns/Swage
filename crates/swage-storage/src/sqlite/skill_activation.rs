//! SQLite implementation of skill activation persistence.
//!
//! Stores active skill state per conversation (session), surviving client
//! disconnections and gateway restarts.

use async_trait::async_trait;
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::ActiveSkillRecord;
use crate::sqlite::SqliteStorage;
use crate::store::SkillActivationStore;

// ---------------------------------------------------------------------------
// SkillActivationStore
// ---------------------------------------------------------------------------

#[async_trait]
impl SkillActivationStore for SqliteStorage {
    async fn activate_skill(&self, session_id: Uuid, skill_name: &str) -> Result<(), StorageError> {
        let skill_name = skill_name.to_owned();
        let now = Utc::now().to_rfc3339();

        self.with_conn(move |conn| {
            // INSERT OR IGNORE: a second activation of the same skill is a no-op.
            // The original activated_at timestamp is preserved so ordering stays stable.
            conn.execute(
                "INSERT OR IGNORE INTO active_skills (session_id, skill_name, activated_at)
                 VALUES (?1, ?2, ?3)",
                params![session_id.to_string(), &skill_name, &now],
            )?;
            Ok(())
        })
        .await
    }

    async fn deactivate_skill(
        &self,
        session_id: Uuid,
        skill_name: &str,
    ) -> Result<bool, StorageError> {
        let skill_name = skill_name.to_owned();

        self.with_conn(move |conn| {
            let rows = conn.execute(
                "DELETE FROM active_skills WHERE session_id = ?1 AND skill_name = ?2",
                params![session_id.to_string(), &skill_name],
            )?;
            // rows_affected > 0 means the skill was present and has been removed.
            Ok(rows > 0)
        })
        .await
    }

    async fn list_active_skills(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ActiveSkillRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT session_id, skill_name, activated_at
                 FROM active_skills
                 WHERE session_id = ?1
                 ORDER BY activated_at ASC",
            )?;

            let records = stmt.query_map(params![session_id.to_string()], |row| {
                let session_str: String = row.get(0)?;
                let skill_name: String = row.get(1)?;
                let activated_str: String = row.get(2)?;

                // Parse the UUID and timestamp back from their stored string forms.
                let session_id = session_str
                    .parse::<Uuid>()
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    ))?;
                let activated_at = activated_str
                    .parse::<chrono::DateTime<Utc>>()
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    ))?;

                Ok(ActiveSkillRecord { session_id, skill_name, activated_at })
            })?
            .collect::<Result<Vec<_>, _>>()?;

            Ok(records)
        })
        .await
    }

    async fn clear_active_skills(&self, session_id: Uuid) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM active_skills WHERE session_id = ?1",
                params![session_id.to_string()],
            )?;
            Ok(())
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::model::{NewProject, NewSession};
    use crate::sqlite::SqliteStorage;
    use crate::store::{ProjectStore, SessionStore, SkillActivationStore};

    async fn make_session() -> (SqliteStorage, uuid::Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject { name: "p".into(), slug: "p".into(), workspace_path: None })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession { project_id: project.id, agent_id: "agent".to_string() })
            .await
            .unwrap();
        (storage, session.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn activate_stores_record() {
        let (storage, session_id) = make_session().await;

        storage.activate_skill(session_id, "forge").await.unwrap();

        let skills = storage.list_active_skills(session_id).await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].skill_name, "forge");
        assert_eq!(skills[0].session_id, session_id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn activate_is_idempotent() {
        // INSERT OR IGNORE: activating the same skill twice should leave one record.
        let (storage, session_id) = make_session().await;

        storage.activate_skill(session_id, "forge").await.unwrap();
        storage.activate_skill(session_id, "forge").await.unwrap();

        let skills = storage.list_active_skills(session_id).await.unwrap();
        assert_eq!(skills.len(), 1, "duplicate activation should not insert a second row");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_returns_in_activation_order() {
        // Skills must come back in the order they were activated (activated_at ASC).
        let (storage, session_id) = make_session().await;

        storage.activate_skill(session_id, "alpha").await.unwrap();
        // Brief pause so activated_at timestamps differ (stored as RFC3339 strings).
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        storage.activate_skill(session_id, "beta").await.unwrap();

        let skills = storage.list_active_skills(session_id).await.unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].skill_name, "alpha");
        assert_eq!(skills[1].skill_name, "beta");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deactivate_removes_and_returns_true() {
        let (storage, session_id) = make_session().await;
        storage.activate_skill(session_id, "forge").await.unwrap();

        let removed = storage.deactivate_skill(session_id, "forge").await.unwrap();
        assert!(removed, "deactivate_skill should return true when skill was active");

        let skills = storage.list_active_skills(session_id).await.unwrap();
        assert!(skills.is_empty(), "skill should be gone after deactivation");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deactivate_returns_false_when_not_active() {
        let (storage, session_id) = make_session().await;

        let removed = storage.deactivate_skill(session_id, "nonexistent").await.unwrap();
        assert!(!removed, "deactivate_skill should return false when skill was not active");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clear_removes_all_for_session() {
        let (storage, session_id) = make_session().await;

        storage.activate_skill(session_id, "alpha").await.unwrap();
        storage.activate_skill(session_id, "beta").await.unwrap();

        storage.clear_active_skills(session_id).await.unwrap();

        let skills = storage.list_active_skills(session_id).await.unwrap();
        assert!(skills.is_empty(), "clear should remove all active skills for the session");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clear_does_not_affect_other_sessions() {
        let (storage, session_a) = make_session().await;

        // Create a second session in the same storage instance.
        let project = storage
            .create(NewProject { name: "q".into(), slug: "q".into(), workspace_path: None })
            .await
            .unwrap();
        let session_b = storage
            .create_session(NewSession { project_id: project.id, agent_id: "agent2".to_string() })
            .await
            .unwrap()
            .id;

        storage.activate_skill(session_a, "shared").await.unwrap();
        storage.activate_skill(session_b, "shared").await.unwrap();

        storage.clear_active_skills(session_a).await.unwrap();

        let skills_a = storage.list_active_skills(session_a).await.unwrap();
        let skills_b = storage.list_active_skills(session_b).await.unwrap();
        assert!(skills_a.is_empty(), "session A should have no skills after clear");
        assert_eq!(skills_b.len(), 1, "session B skills should be unaffected");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cascade_delete_removes_skills_with_session() {
        // Deleting a session should cascade to active_skills via the FK constraint.
        let (storage, session_id) = make_session().await;
        storage.activate_skill(session_id, "forge").await.unwrap();

        // Delete the session directly; foreign keys are ON so the cascade fires.
        storage
            .with_conn(move |conn| {
                conn.execute(
                    "DELETE FROM sessions WHERE id = ?1",
                    rusqlite::params![session_id.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let count: i64 = storage
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM active_skills", [], |r| r.get(0))
            })
            .await
            .unwrap();
        assert_eq!(count, 0, "cascade delete should remove active_skills when session is deleted");
    }
}
