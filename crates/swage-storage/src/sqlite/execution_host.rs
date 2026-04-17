use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;

use swage_core::StorageError;

use crate::model::{ExecutionHostRegistration, NewExecutionHostRegistration};
use crate::sqlite::SqliteStorage;
use crate::store::ExecutionHostStore;

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn parse_capabilities(json: &str) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn row_to_execution_host(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionHostRegistration> {
    let registered_at: String = row.get(7)?;
    let capabilities_json: String = row.get(6)?;
    Ok(ExecutionHostRegistration {
        host_id: row.get(0)?,
        label: row.get(1)?,
        kind: row.get(2)?,
        base_url: row.get(3)?,
        shared_token: row.get(4)?,
        workspace_root: row.get(5)?,
        capabilities: parse_capabilities(&capabilities_json)?,
        registered_at: parse_dt(&registered_at)?,
    })
}

#[async_trait]
impl ExecutionHostStore for SqliteStorage {
    async fn upsert_execution_host(
        &self,
        host: NewExecutionHostRegistration,
    ) -> Result<ExecutionHostRegistration, StorageError> {
        self.with_conn(|conn| {
            let registered_at = Utc::now();
            let registered_at_str = registered_at.to_rfc3339();
            let capabilities_json = serde_json::to_string(&host.capabilities)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;

            conn.execute(
                "INSERT INTO execution_hosts
                 (host_id, label, kind, base_url, shared_token, workspace_root, capabilities_json, registered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(host_id) DO UPDATE SET
                     label = excluded.label,
                     kind = excluded.kind,
                     base_url = excluded.base_url,
                     shared_token = excluded.shared_token,
                     workspace_root = excluded.workspace_root,
                     capabilities_json = excluded.capabilities_json,
                     registered_at = excluded.registered_at",
                params![
                    &host.host_id,
                    &host.label,
                    &host.kind,
                    &host.base_url,
                    &host.shared_token,
                    &host.workspace_root,
                    &capabilities_json,
                    &registered_at_str,
                ],
            )?;

            Ok(ExecutionHostRegistration {
                host_id: host.host_id,
                label: host.label,
                kind: host.kind,
                base_url: host.base_url,
                shared_token: host.shared_token,
                workspace_root: host.workspace_root,
                capabilities: host.capabilities,
                registered_at,
            })
        })
        .await
    }

    async fn list_execution_hosts(&self) -> Result<Vec<ExecutionHostRegistration>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT host_id, label, kind, base_url, shared_token, workspace_root, capabilities_json, registered_at
                 FROM execution_hosts ORDER BY registered_at ASC, host_id ASC",
            )?;
            let rows = stmt.query_map([], row_to_execution_host)?;
            rows.collect()
        })
        .await
    }

    async fn delete_execution_host(&self, host_id: &str) -> Result<bool, StorageError> {
        let host_id = host_id.to_string();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM execution_hosts WHERE host_id = ?1",
                params![host_id],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewExecutionHostRegistration, NewProject, NewSession};
    use crate::store::{ExecutionHostStore, ProjectStore, SessionStore};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_host_round_trips_and_replaces_registration() {
        let storage = SqliteStorage::in_memory().unwrap();

        let first = storage
            .upsert_execution_host(NewExecutionHostRegistration {
                host_id: "node-a".into(),
                label: Some("Node A".into()),
                kind: "attached_node".into(),
                base_url: "http://127.0.0.1:4000".into(),
                shared_token: "secret-a".into(),
                workspace_root: "/tmp/a".into(),
                capabilities: vec!["shell".into()],
            })
            .await
            .unwrap();
        assert_eq!(first.host_id, "node-a");

        let replaced = storage
            .upsert_execution_host(NewExecutionHostRegistration {
                host_id: "node-a".into(),
                label: Some("Node A2".into()),
                kind: "docker".into(),
                base_url: "http://127.0.0.1:5000".into(),
                shared_token: "secret-b".into(),
                workspace_root: "/tmp/b".into(),
                capabilities: vec!["host_identity".into()],
            })
            .await
            .unwrap();
        assert_eq!(replaced.kind, "docker");

        let hosts = storage.list_execution_hosts().await.unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].label.as_deref(), Some("Node A2"));
        assert_eq!(hosts[0].workspace_root, "/tmp/b");
        assert_eq!(hosts[0].capabilities, vec!["host_identity"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_session_by_channel_origin_returns_matching_session() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject {
                name: "test".into(),
                slug: "test".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".into(),
                metadata: Some(serde_json::json!({
                    "channel_origin": {
                        "integration": "dev_dm",
                        "account": "workspace-a",
                        "conversation": "alice"
                    }
                })),
            })
            .await
            .unwrap();

        let found = storage
            .find_session_by_channel_origin("dev_dm", "workspace-a", "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, session.id);
    }
}
