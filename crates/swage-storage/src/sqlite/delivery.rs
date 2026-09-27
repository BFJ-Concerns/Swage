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
    let session_id: Option<String> = row.get(1)?;
    let run_id: Option<String> = row.get(2)?;
    let turn_id: Option<String> = row.get(3)?;
    let target_json: String = row.get(4)?;
    let delivered_at: String = row.get(8)?;

    Ok(DeliveryRecord {
        id: parse_uuid(&id)?,
        session_id: session_id.as_deref().map(parse_uuid).transpose()?,
        run_id: run_id.as_deref().map(parse_uuid).transpose()?,
        turn_id: turn_id.as_deref().map(parse_uuid).transpose()?,
        target: serde_json::from_str(&target_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        content: row.get(5)?,
        status: row.get(6)?,
        error: row.get(7)?,
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
                "INSERT INTO delivery_records (id, session_id, run_id, turn_id, target_json, content, status, error, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.to_string(),
                    delivery.session_id.map(|id| id.to_string()),
                    delivery.run_id.map(|id| id.to_string()),
                    delivery.turn_id.map(|id| id.to_string()),
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
                run_id: delivery.run_id,
                turn_id: delivery.turn_id,
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
                "SELECT id, session_id, run_id, turn_id, target_json, content, status, error, delivered_at
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
                "SELECT id, session_id, run_id, turn_id, target_json, content, status, error, delivered_at
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
    use chrono::Utc;
    use uuid::Uuid;

    use crate::model::{
        NewSchedule, NewScheduleRun, NewThread, NewTurn, ScheduleCreator, ScheduleKind,
        ScheduleRunTrigger, ScheduleStatus, SessionPolicy, TurnState,
    };
    use crate::store::{
        DeliveryStore, ScheduleStore, SessionStore, ThreadStore, TopicStore, TurnStore,
    };
    use crate::{NewDeliveryRecord, NewSession};

    #[tokio::test(flavor = "multi_thread")]
    async fn delivery_record_round_trips_and_lists_by_session() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                ..crate::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();

        let created = storage
            .create_delivery_record(NewDeliveryRecord {
                session_id: Some(session.id),
                run_id: None,
                turn_id: None,
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
        assert_eq!(session_records[0].run_id, None);
        assert_eq!(session_records[0].turn_id, None);

        let all = storage.list_delivery_records().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content, "delivered");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delivery_record_round_trips_schedule_run_and_turn_attribution() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                ..crate::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &storage,
            NewThread {
                session_id: session.id,
                agent_id: "gateway".into(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".into(),
                state: TurnState::Completed,
                user_input: Some("deliver".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let schedule = storage
            .create_schedule(NewSchedule {
                id: Uuid::new_v4(),
                message: "deliver".into(),
                status: ScheduleStatus::Active,
                kind: ScheduleKind::Interval,
                expression: "every 5 seconds".into(),
                session_policy: SessionPolicy::Continuation {
                    session_id: session.id,
                },
                creator: ScheduleCreator::User,
                delivery_routing: json!({"store_local_output": true}),
                gate: None,
                next_run_at: Some(Utc::now()),
                last_run_at: None,
            })
            .await
            .unwrap();
        let run = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Manual,
                None,
            ))
            .await
            .unwrap();

        storage
            .create_delivery_record(NewDeliveryRecord {
                session_id: Some(session.id),
                run_id: Some(run.id),
                turn_id: Some(turn.id),
                target: json!({"kind": "local_storage"}),
                content: "delivered".into(),
                status: "sent".into(),
                error: None,
            })
            .await
            .unwrap();

        let records = storage
            .list_delivery_records_by_session(session.id)
            .await
            .unwrap();
        assert_eq!(records[0].run_id, Some(run.id));
        assert_eq!(records[0].turn_id, Some(turn.id));
    }
}
