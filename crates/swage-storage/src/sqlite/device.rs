use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;

use swage_core::StorageError;

use crate::model::{DeviceRegistration, NewDeviceRegistration};
use crate::sqlite::SqliteStorage;
use crate::store::DeviceStore;

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

fn parse_workspace_roots(json: &str) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn row_to_device(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRegistration> {
    let registered_at: String = row.get(8)?;
    let capabilities_json: String = row.get(7)?;
    let workspace_roots_json: String = row.get(6)?;
    Ok(DeviceRegistration {
        device_id: row.get(0)?,
        label: row.get(1)?,
        kind: row.get(2)?,
        base_url: row.get(3)?,
        shared_token: row.get(4)?,
        workspace_root: row.get(5)?,
        workspace_roots: parse_workspace_roots(&workspace_roots_json)?,
        capabilities: parse_capabilities(&capabilities_json)?,
        registered_at: parse_dt(&registered_at)?,
    })
}

#[async_trait]
impl DeviceStore for SqliteStorage {
    async fn upsert_device(
        &self,
        device: NewDeviceRegistration,
    ) -> Result<DeviceRegistration, StorageError> {
        self.with_conn(|conn| {
            let registered_at = Utc::now();
            let registered_at_str = registered_at.to_rfc3339();
            let capabilities_json = serde_json::to_string(&device.capabilities)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let workspace_roots_json = serde_json::to_string(&device.workspace_roots)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;

            conn.execute(
                "INSERT INTO devices
                 (device_id, label, kind, base_url, shared_token, workspace_root, workspace_roots_json, capabilities_json, registered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(device_id) DO UPDATE SET
                     label = excluded.label,
                     kind = excluded.kind,
                     base_url = excluded.base_url,
                     shared_token = excluded.shared_token,
                     workspace_root = excluded.workspace_root,
                     workspace_roots_json = excluded.workspace_roots_json,
                     capabilities_json = excluded.capabilities_json,
                     registered_at = excluded.registered_at",
                params![
                    &device.device_id,
                    &device.label,
                    &device.kind,
                    &device.base_url,
                    &device.shared_token,
                    &device.workspace_root,
                    &workspace_roots_json,
                    &capabilities_json,
                    &registered_at_str,
                ],
            )?;

            Ok(DeviceRegistration {
                device_id: device.device_id,
                label: device.label,
                kind: device.kind,
                base_url: device.base_url,
                shared_token: device.shared_token,
                workspace_root: device.workspace_root,
                workspace_roots: device.workspace_roots,
                capabilities: device.capabilities,
                registered_at,
            })
        })
        .await
    }

    async fn list_devices(&self) -> Result<Vec<DeviceRegistration>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT device_id, label, kind, base_url, shared_token, workspace_root, workspace_roots_json, capabilities_json, registered_at
                 FROM devices ORDER BY registered_at ASC, device_id ASC",
            )?;
            let rows = stmt.query_map([], row_to_device)?;
            rows.collect()
        })
        .await
    }

    async fn delete_device(&self, device_id: &str) -> Result<bool, StorageError> {
        let device_id = device_id.to_string();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM devices WHERE device_id = ?1",
                params![device_id],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewDeviceRegistration, NewSession, NewTopic};
    use crate::store::{DeviceStore, SessionStore, TopicStore};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn device_round_trips_and_replaces_registration() {
        let storage = SqliteStorage::in_memory().unwrap();

        let first = storage
            .upsert_device(NewDeviceRegistration {
                device_id: "device-a".into(),
                label: Some("Device A".into()),
                kind: "attached_device".into(),
                base_url: "http://127.0.0.1:4000".into(),
                shared_token: "secret-a".into(),
                workspace_root: "/tmp/a".into(),
                workspace_roots: vec!["/tmp/a".into(), "/mnt/shared".into()],
                capabilities: vec!["shell".into()],
            })
            .await
            .unwrap();
        assert_eq!(first.device_id, "device-a");

        let replaced = storage
            .upsert_device(NewDeviceRegistration {
                device_id: "device-a".into(),
                label: Some("Device A2".into()),
                kind: "docker".into(),
                base_url: "http://127.0.0.1:5000".into(),
                shared_token: "secret-b".into(),
                workspace_root: "/tmp/b".into(),
                workspace_roots: vec!["/tmp/b".into()],
                capabilities: vec!["device_identity".into()],
            })
            .await
            .unwrap();
        assert_eq!(replaced.kind, "docker");

        let devices = storage.list_devices().await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].label.as_deref(), Some("Device A2"));
        assert_eq!(devices[0].workspace_root, "/tmp/b");
        assert_eq!(devices[0].workspace_roots, vec!["/tmp/b"]);
        assert_eq!(devices[0].capabilities, vec!["device_identity"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_session_by_channel_origin_returns_matching_session() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "test".into(),
                slug: "test".into(),
            })
            .await
            .unwrap();

        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "gateway".into(),
                origin: crate::model::SessionOrigin::Direct,
                metadata: Some(serde_json::json!({
                    "channel_origin": {
                        "integration": "dev_dm",
                        "account": "workspace-a",
                        "conversation": "alice"
                    }
                })),
                active_workspace_attachment_id: None,
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
