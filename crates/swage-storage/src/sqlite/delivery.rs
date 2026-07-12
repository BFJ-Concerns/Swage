use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{DeliveryRecord, NewDeliveryRecord};
use crate::sqlite::SqliteStorage;
use crate::store::DeliveryStore;

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

fn row_to_delivery(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryRecord> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let target_json: String = row.get(2)?;
    let delivered_at: String = row.get(6)?;

    Ok(DeliveryRecord {
        id: parse_uuid(&id)?,
        session_id: parse_uuid(&session_id)?,
        target: serde_json::from_str(&target_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        content: row.get(3)?,
        status: row.get(4)?,
        error: row.get(5)?,
        delivered_at: parse_dt(&delivered_at)?,
    })
}

#[async_trait]
impl DeliveryStore for SqliteStorage {
    async fn create_delivery_record(
        &self,
        delivery: NewDeliveryRecord,
    ) -> Result<DeliveryRecord, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let delivered_at = Utc::now();
            let target_json = serde_json::to_string(&delivery.target)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;

            conn.execute(
                "INSERT INTO delivery_records (id, session_id, target_json, content, status, error, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id.to_string(),
                    delivery.session_id.to_string(),
                    target_json,
                    delivery.content,
                    delivery.status,
                    delivery.error,
                    delivered_at.to_rfc3339(),
                ],
            )?;

            Ok(DeliveryRecord {
                id,
                session_id: delivery.session_id,
                target: delivery.target,
                content: delivery.content,
                status: delivery.status,
                error: delivery.error,
                delivered_at,
            })
        })
        .await
    }

    async fn list_delivery_records(&self) -> Result<Vec<DeliveryRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, target_json, content, status, error, delivered_at
                 FROM delivery_records
                 ORDER BY delivered_at ASC, rowid ASC",
            )?;
            let rows = stmt.query_map([], row_to_delivery)?;
            rows.collect()
        })
        .await
    }

    async fn list_delivery_records_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<DeliveryRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, target_json, content, status, error, delivered_at
                 FROM delivery_records
                 WHERE session_id = ?1
                 ORDER BY delivered_at ASC, rowid ASC",
            )?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_delivery)?;
            rows.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::sqlite::SqliteStorage;
    use crate::store::{DeliveryStore, SessionStore, SpaceStore};
    use crate::{NewDeliveryRecord, NewSession};

    #[tokio::test(flavor = "multi_thread")]
    async fn delivery_record_round_trips_and_lists_by_session() {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "gateway".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        let created = storage
            .create_delivery_record(NewDeliveryRecord {
                session_id: session.id,
                target: json!({"kind": "local_storage"}),
                content: "delivered".into(),
                status: "sent".into(),
                error: None,
            })
            .await
            .unwrap();

        let session_records = storage
            .list_delivery_records_by_session(session.id)
            .await
            .unwrap();
        assert_eq!(session_records.len(), 1);
        assert_eq!(session_records[0].id, created.id);

        let all = storage.list_delivery_records().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content, "delivered");
    }
}
