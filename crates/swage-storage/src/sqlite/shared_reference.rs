use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;

use swage_core::StorageError;

use crate::model::{NewSharedReferenceEvent, SharedReferenceEvent, SharedReferenceKind};
use crate::sqlite::SqliteStorage;
use crate::store::SharedReferenceStore;

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<SharedReferenceEvent> {
    Ok(SharedReferenceEvent {
        id: row.get::<_, String>(0)?.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        object_kind: SharedReferenceKind::from_str(&row.get::<_, String>(1)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        })?,
        object_id: row.get(2)?,
        event_kind: row.get(3)?,
        payload: serde_json::from_str(&row.get::<_, String>(4)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        created_at: parse_dt(&row.get::<_, String>(5)?)?,
    })
}

#[async_trait]
impl SharedReferenceStore for SqliteStorage {
    async fn create_shared_reference_event(
        &self,
        event: NewSharedReferenceEvent,
    ) -> Result<SharedReferenceEvent, StorageError> {
        self.with_conn(move |conn| {
            let created_at = Utc::now();
            let created_at_text = created_at.to_rfc3339();
            let payload_json = serde_json::to_string(&event.payload)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;

            conn.execute(
                "INSERT INTO shared_reference_events (
                    id, object_kind, object_id, event_kind, payload_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    event.id.to_string(),
                    event.object_kind.to_string(),
                    event.object_id,
                    event.event_kind,
                    payload_json,
                    created_at_text,
                ],
            )?;

            Ok(SharedReferenceEvent {
                id: event.id,
                object_kind: event.object_kind,
                object_id: event.object_id,
                event_kind: event.event_kind,
                payload: event.payload,
                created_at,
            })
        })
        .await
    }

    async fn list_shared_reference_events(
        &self,
        object_kind: SharedReferenceKind,
        object_id: &str,
    ) -> Result<Vec<SharedReferenceEvent>, StorageError> {
        let object_id = object_id.to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, object_kind, object_id, event_kind, payload_json, created_at
                 FROM shared_reference_events
                 WHERE object_kind = ?1 AND object_id = ?2
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![object_kind.to_string(), object_id], row_to_event)?;
            rows.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::model::{NewSharedReferenceEvent, SharedReferenceKind};
    use crate::store::SharedReferenceStore;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn shared_reference_events_round_trip_for_object() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");
        let created = storage
            .create_shared_reference_event(NewSharedReferenceEvent {
                id: Uuid::new_v4(),
                object_kind: SharedReferenceKind::TeamTopology,
                object_id: "team-1".into(),
                event_kind: "selected".into(),
                payload: serde_json::json!({ "node_id": "default" }),
            })
            .await
            .expect("create event");

        let listed = storage
            .list_shared_reference_events(SharedReferenceKind::TeamTopology, "team-1")
            .await
            .expect("list events");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], created);
    }
}
