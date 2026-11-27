//! The turn graph's revisions in SQLite: one row per revision, at most one
//! active, activation swapped in one transaction.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Row, params};
use swage_core::StorageError;

use super::SqliteStorage;
use crate::model::{NewTurnGraphRevision, TurnGraphRevisionRecord};
use crate::store::TurnGraphStore;

fn row_to_revision(row: &Row<'_>) -> rusqlite::Result<TurnGraphRevisionRecord> {
    let graph: String = row.get("graph")?;
    let created_at: String = row.get("created_at")?;
    Ok(TurnGraphRevisionRecord {
        revision: row.get::<_, i64>("revision")? as u32,
        graph: serde_json::from_str(&graph).unwrap_or(serde_json::Value::Null),
        activation: row.get("activation")?,
        failure: row.get("failure")?,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    })
}

#[async_trait]
impl TurnGraphStore for SqliteStorage {
    async fn record_turn_graph_revision(
        &self,
        revision: NewTurnGraphRevision,
    ) -> Result<TurnGraphRevisionRecord, StorageError> {
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            if revision.activation == "active" {
                transaction.execute(
                    "UPDATE turn_graph_revisions SET activation = 'superseded' WHERE activation = 'active'",
                    [],
                )?;
            }
            let created_at = Utc::now().to_rfc3339();
            transaction.execute(
                "INSERT INTO turn_graph_revisions (revision, graph, activation, failure, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    revision.revision as i64,
                    revision.graph.to_string(),
                    revision.activation,
                    revision.failure,
                    created_at,
                ],
            )?;
            let record = transaction.query_row(
                "SELECT revision, graph, activation, failure, created_at FROM turn_graph_revisions WHERE revision = ?1",
                params![revision.revision as i64],
                row_to_revision,
            )?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    async fn list_turn_graph_revisions(
        &self,
    ) -> Result<Vec<TurnGraphRevisionRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT revision, graph, activation, failure, created_at FROM turn_graph_revisions
                 ORDER BY revision",
            )?;
            statement
                .query_map([], row_to_revision)?
                .collect::<Result<Vec<_>, _>>()
        })
        .await
    }

    async fn activate_turn_graph_revision(&self, revision: u32) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "UPDATE turn_graph_revisions SET activation = 'superseded' WHERE activation = 'active'",
                [],
            )?;
            let changed = transaction.execute(
                "UPDATE turn_graph_revisions SET activation = 'active', failure = NULL WHERE revision = ?1",
                params![revision as i64],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(n: u32, activation: &str) -> NewTurnGraphRevision {
        NewTurnGraphRevision {
            revision: n,
            graph: serde_json::json!({ "steps": [{ "id": format!("step-{n}") }] }),
            activation: activation.into(),
            failure: (activation == "failed").then(|| "invalid".to_owned()),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recording_an_active_revision_supersedes_the_one_before() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .record_turn_graph_revision(revision(0, "active"))
            .await
            .unwrap();
        let second = storage
            .record_turn_graph_revision(revision(1, "active"))
            .await
            .unwrap();
        assert_eq!(second.activation, "active");
        let listed = storage.list_turn_graph_revisions().await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].activation, "superseded");
        assert_eq!(listed[1].graph["steps"][0]["id"], "step-1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_revision_is_recorded_without_touching_the_active_one() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .record_turn_graph_revision(revision(0, "active"))
            .await
            .unwrap();
        let failed = storage
            .record_turn_graph_revision(revision(1, "failed"))
            .await
            .unwrap();
        assert_eq!(failed.failure.as_deref(), Some("invalid"));
        let listed = storage.list_turn_graph_revisions().await.unwrap();
        assert_eq!(listed[0].activation, "active");
        assert_eq!(listed[1].activation, "failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn activating_an_earlier_revision_swaps_the_active_one_in_place() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .record_turn_graph_revision(revision(0, "active"))
            .await
            .unwrap();
        storage
            .record_turn_graph_revision(revision(1, "active"))
            .await
            .unwrap();
        storage.activate_turn_graph_revision(0).await.unwrap();
        let listed = storage.list_turn_graph_revisions().await.unwrap();
        assert_eq!(listed[0].activation, "active");
        assert_eq!(listed[1].activation, "superseded");
        assert!(
            storage.activate_turn_graph_revision(9).await.is_err(),
            "unknown revision"
        );
    }
}
