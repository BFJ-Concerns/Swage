use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::ContextSnapshotRecord;
use crate::sqlite::SqliteStorage;
use crate::store::ContextSnapshotStore;

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

// ---------------------------------------------------------------------------
// ContextSnapshotStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ContextSnapshotStore for SqliteStorage {
    async fn store_snapshot(&self, turn_id: Uuid, report_json: &str) -> Result<(), StorageError> {
        let report_json = report_json.to_owned();
        let now = Utc::now().to_rfc3339();

        self.with_conn(move |conn| {
            // INSERT OR REPLACE: one snapshot per turn; newer report wins.
            conn.execute(
                "INSERT OR REPLACE INTO context_snapshots (turn_id, report_json, created_at)
                 VALUES (?1, ?2, ?3)",
                params![turn_id.to_string(), &report_json, &now],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_snapshot(&self, turn_id: Uuid) -> Result<Option<String>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT report_json FROM context_snapshots WHERE turn_id = ?1",
                params![turn_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
        })
        .await
    }

    async fn list_snapshots_by_topic(
        &self,
        topic_id: Uuid,
        limit: usize,
    ) -> Result<Vec<ContextSnapshotRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT cs.turn_id, th.session_id, s.topic_id, cs.created_at, cs.report_json
                 FROM context_snapshots cs
                 INNER JOIN turns t ON t.id = cs.turn_id
                 INNER JOIN threads th ON th.id = t.thread_id
                 INNER JOIN sessions s ON s.id = th.session_id
                 WHERE s.topic_id = ?1
                 ORDER BY cs.created_at DESC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![topic_id.to_string(), limit as i64], |row| {
                Ok(ContextSnapshotRecord {
                    turn_id: row.get::<_, String>(0)?.parse().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    session_id: row.get::<_, String>(1)?.parse().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    topic_id: row.get::<_, String>(2)?.parse().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    created_at: parse_dt(&row.get::<_, String>(3)?)?,
                    report_json: row.get(4)?,
                })
            })?;
            rows.collect()
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::model::{NewSession, NewTopic, NewTurn, TurnState};
    use crate::sqlite::SqliteStorage;
    use crate::store::{ContextSnapshotStore, SessionStore, TopicStore, TurnStore};

    /// Create a topic → session → thread → turn chain, returning storage + turn_id.
    async fn make_turn() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "p".into(),
                slug: "p".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        // Insert thread via raw SQL (same pattern used in turn tests).
        let thread_id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
                     VALUES (?1, ?2, 'agent', ?3, ?3)",
                    rusqlite::params![thread_id.to_string(), session.id.to_string(), &now],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let turn = storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "agent".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        (storage, turn.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn store_and_retrieve_snapshot() {
        let (storage, turn_id) = make_turn().await;
        let report = r#"{"total_tokens":500,"elements":[]}"#;

        storage.store_snapshot(turn_id, report).await.unwrap();

        let retrieved = storage.get_snapshot(turn_id).await.unwrap();
        assert_eq!(retrieved.as_deref(), Some(report));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_snapshot_returns_none_for_unknown_turn() {
        let (storage, _) = make_turn().await;
        let unknown_id = Uuid::new_v4();

        let result = storage.get_snapshot(unknown_id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn store_snapshot_overwrites_existing() {
        let (storage, turn_id) = make_turn().await;

        storage.store_snapshot(turn_id, r#"{"v":1}"#).await.unwrap();
        storage.store_snapshot(turn_id, r#"{"v":2}"#).await.unwrap();

        let retrieved = storage.get_snapshot(turn_id).await.unwrap().unwrap();
        assert_eq!(
            retrieved, r#"{"v":2}"#,
            "newer snapshot should replace the old one"
        );
    }
}
