use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    Heartbeat, HeartbeatRun, HeartbeatRunState, NewHeartbeat, NewHeartbeatRun, UpdateHeartbeat,
};
use crate::sqlite::SqliteStorage;
use crate::store::HeartbeatStore;

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

fn parse_optional_dt(s: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    s.map(|value| parse_dt(&value)).transpose()
}

fn serialise_optional_dt(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.to_rfc3339())
}

fn row_to_heartbeat(row: &rusqlite::Row<'_>) -> rusqlite::Result<Heartbeat> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let message: String = row.get(3)?;
    let interval_seconds: i64 = row.get(4)?;
    let status: String = row.get(5)?;
    let last_beat_at: Option<String> = row.get(6)?;
    let next_beat_at: Option<String> = row.get(7)?;
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;

    Ok(Heartbeat {
        id: parse_uuid(&id)?,
        session_id: parse_uuid(&session_id)?,
        agent_id,
        message,
        interval_seconds,
        status: status.parse().map_err(|error: String| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        })?,
        last_beat_at: parse_optional_dt(last_beat_at)?,
        next_beat_at: parse_optional_dt(next_beat_at)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

fn row_to_heartbeat_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<HeartbeatRun> {
    let id: String = row.get(0)?;
    let heartbeat_id: String = row.get(1)?;
    let state: String = row.get(2)?;
    let scheduled_for: Option<String> = row.get(3)?;
    let turn_id: Option<String> = row.get(4)?;
    let output_text: Option<String> = row.get(5)?;
    let error_message: Option<String> = row.get(6)?;
    let created_at: String = row.get(7)?;
    let started_at: Option<String> = row.get(8)?;
    let completed_at: Option<String> = row.get(9)?;

    Ok(HeartbeatRun {
        id: parse_uuid(&id)?,
        heartbeat_id: parse_uuid(&heartbeat_id)?,
        state: state.parse().map_err(|error: String| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        })?,
        scheduled_for: parse_optional_dt(scheduled_for)?,
        turn_id: turn_id.as_deref().map(parse_uuid).transpose()?,
        output_text,
        error_message,
        created_at: parse_dt(&created_at)?,
        started_at: parse_optional_dt(started_at)?,
        completed_at: parse_optional_dt(completed_at)?,
    })
}

#[async_trait]
impl HeartbeatStore for SqliteStorage {
    async fn create_heartbeat(&self, heartbeat: NewHeartbeat) -> Result<Heartbeat, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let last_beat_at = serialise_optional_dt(heartbeat.last_beat_at);
            let next_beat_at = serialise_optional_dt(heartbeat.next_beat_at);

            conn.execute(
                "INSERT INTO heartbeats
                 (id, session_id, agent_id, message, interval_seconds, status, last_beat_at, next_beat_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    id.to_string(),
                    heartbeat.session_id.to_string(),
                    &heartbeat.agent_id,
                    &heartbeat.message,
                    heartbeat.interval_seconds,
                    heartbeat.status.to_string(),
                    &last_beat_at,
                    &next_beat_at,
                    &now_str,
                ],
            )?;

            Ok(Heartbeat {
                id,
                session_id: heartbeat.session_id,
                agent_id: heartbeat.agent_id,
                message: heartbeat.message,
                interval_seconds: heartbeat.interval_seconds,
                status: heartbeat.status,
                last_beat_at: heartbeat.last_beat_at,
                next_beat_at: heartbeat.next_beat_at,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_heartbeat(&self, id: Uuid) -> Result<Option<Heartbeat>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, session_id, agent_id, message, interval_seconds, status, last_beat_at, next_beat_at, created_at, updated_at
                 FROM heartbeats WHERE id = ?1",
                params![id.to_string()],
                row_to_heartbeat,
            )
            .optional()
        })
        .await
    }

    async fn list_heartbeats(&self) -> Result<Vec<Heartbeat>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, message, interval_seconds, status, last_beat_at, next_beat_at, created_at, updated_at
                 FROM heartbeats ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], row_to_heartbeat)?;
            rows.collect()
        })
        .await
    }

    async fn list_due_heartbeats(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<Heartbeat>, StorageError> {
        let now = now.to_rfc3339();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, message, interval_seconds, status, last_beat_at, next_beat_at, created_at, updated_at
                 FROM heartbeats
                 WHERE status = 'enabled' AND next_beat_at IS NOT NULL AND next_beat_at <= ?1
                 ORDER BY next_beat_at ASC, created_at ASC",
            )?;
            let rows = stmt.query_map(params![&now], row_to_heartbeat)?;
            rows.collect()
        })
        .await
    }

    async fn update_heartbeat(
        &self,
        id: Uuid,
        update: UpdateHeartbeat,
    ) -> Result<Heartbeat, StorageError> {
        let existing = self
            .get_heartbeat(id)
            .await?
            .ok_or(StorageError::NotFound {
                entity: "Heartbeat".into(),
                id: id.to_string(),
            })?;

        let updated = Heartbeat {
            id: existing.id,
            session_id: existing.session_id,
            agent_id: existing.agent_id,
            message: update.message.unwrap_or(existing.message),
            interval_seconds: update.interval_seconds.unwrap_or(existing.interval_seconds),
            status: update.status.unwrap_or(existing.status),
            last_beat_at: update.last_beat_at.unwrap_or(existing.last_beat_at),
            next_beat_at: update.next_beat_at.unwrap_or(existing.next_beat_at),
            created_at: existing.created_at,
            updated_at: Utc::now(),
        };

        let last_beat_at = serialise_optional_dt(updated.last_beat_at);
        let next_beat_at = serialise_optional_dt(updated.next_beat_at);
        let updated_at = updated.updated_at.to_rfc3339();

        self.with_conn(|conn| {
            conn.execute(
                "UPDATE heartbeats
                 SET message = ?1,
                     interval_seconds = ?2,
                     status = ?3,
                     last_beat_at = ?4,
                     next_beat_at = ?5,
                     updated_at = ?6
                 WHERE id = ?7",
                params![
                    &updated.message,
                    updated.interval_seconds,
                    updated.status.to_string(),
                    &last_beat_at,
                    &next_beat_at,
                    &updated_at,
                    id.to_string(),
                ],
            )?;
            Ok(updated)
        })
        .await
    }

    async fn delete_heartbeat(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(|conn| {
            let affected = conn.execute(
                "DELETE FROM heartbeats WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    async fn create_heartbeat_run(
        &self,
        run: NewHeartbeatRun,
    ) -> Result<HeartbeatRun, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let created_at = now.to_rfc3339();
            let scheduled_for = serialise_optional_dt(run.scheduled_for);
            let turn_id = run.turn_id.map(|value| value.to_string());
            let started_at = serialise_optional_dt(run.started_at);
            let completed_at = serialise_optional_dt(run.completed_at);

            conn.execute(
                "INSERT INTO heartbeat_runs
                 (id, heartbeat_id, state, scheduled_for, turn_id, output_text, error_message, created_at, started_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id.to_string(),
                    run.heartbeat_id.to_string(),
                    run.state.to_string(),
                    &scheduled_for,
                    &turn_id,
                    &run.output_text,
                    &run.error_message,
                    &created_at,
                    &started_at,
                    &completed_at,
                ],
            )?;

            Ok(HeartbeatRun {
                id,
                heartbeat_id: run.heartbeat_id,
                state: run.state,
                scheduled_for: run.scheduled_for,
                turn_id: run.turn_id,
                output_text: run.output_text,
                error_message: run.error_message,
                created_at: now,
                started_at: run.started_at,
                completed_at: run.completed_at,
            })
        })
        .await
    }

    async fn get_heartbeat_run(&self, id: Uuid) -> Result<Option<HeartbeatRun>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, heartbeat_id, state, scheduled_for, turn_id, output_text, error_message, created_at, started_at, completed_at
                 FROM heartbeat_runs WHERE id = ?1",
                params![id.to_string()],
                row_to_heartbeat_run,
            )
            .optional()
        })
        .await
    }

    async fn list_heartbeat_runs_by_heartbeat(
        &self,
        heartbeat_id: Uuid,
    ) -> Result<Vec<HeartbeatRun>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, heartbeat_id, state, scheduled_for, turn_id, output_text, error_message, created_at, started_at, completed_at
                 FROM heartbeat_runs WHERE heartbeat_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![heartbeat_id.to_string()], row_to_heartbeat_run)?;
            rows.collect()
        })
        .await
    }

    async fn update_heartbeat_run_state(
        &self,
        id: Uuid,
        state: HeartbeatRunState,
        turn_id: Option<Uuid>,
        output_text: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        self.with_conn(|conn| {
            let now = Utc::now().to_rfc3339();
            let turn_id = turn_id.map(|value| value.to_string());
            let started_at = if matches!(state, HeartbeatRunState::Running) {
                Some(now.clone())
            } else {
                None
            };
            let completed_at = if matches!(
                state,
                HeartbeatRunState::Completed | HeartbeatRunState::Failed
            ) {
                Some(now)
            } else {
                None
            };

            conn.execute(
                "UPDATE heartbeat_runs
                 SET state = ?1,
                     turn_id = COALESCE(?2, turn_id),
                     output_text = COALESCE(?3, output_text),
                     error_message = COALESCE(?4, error_message),
                     started_at = COALESCE(?5, started_at),
                     completed_at = COALESCE(?6, completed_at)
                 WHERE id = ?7",
                params![
                    state.to_string(),
                    &turn_id,
                    &output_text,
                    &error_message,
                    &started_at,
                    &completed_at,
                    id.to_string(),
                ],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HeartbeatRunState, HeartbeatStatus, NewBench};
    use crate::store::{BenchStore, SessionStore};

    async fn make_storage_with_session() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let bench = storage
            .create(NewBench {
                name: "heartbeat".into(),
                slug: "heartbeat".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(crate::model::NewSession {
                bench_id: bench.id,
                agent_id: "gateway".into(),
                metadata: None,
            })
            .await
            .unwrap();
        (storage, session.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heartbeat_round_trips_and_due_query_filters_enabled_rows() {
        let (storage, session_id) = make_storage_with_session().await;
        let now = Utc::now();
        let due = storage
            .create_heartbeat(NewHeartbeat {
                session_id,
                agent_id: "gateway".into(),
                message: "check in".into(),
                interval_seconds: 30,
                status: HeartbeatStatus::Enabled,
                last_beat_at: None,
                next_beat_at: Some(now),
            })
            .await
            .unwrap();
        storage
            .create_heartbeat(NewHeartbeat {
                session_id,
                agent_id: "gateway".into(),
                message: "paused".into(),
                interval_seconds: 30,
                status: HeartbeatStatus::Disabled,
                last_beat_at: None,
                next_beat_at: Some(now),
            })
            .await
            .unwrap();

        let due_heartbeats = storage.list_due_heartbeats(now).await.unwrap();
        assert_eq!(due_heartbeats.len(), 1);
        assert_eq!(due_heartbeats[0].id, due.id);

        let fetched = storage.get_heartbeat(due.id).await.unwrap().unwrap();
        assert_eq!(fetched.message, "check in");
        assert_eq!(fetched.interval_seconds, 30);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heartbeat_run_round_trips_and_updates_state() {
        let (storage, session_id) = make_storage_with_session().await;
        let heartbeat = storage
            .create_heartbeat(NewHeartbeat {
                session_id,
                agent_id: "gateway".into(),
                message: "beat".into(),
                interval_seconds: 60,
                status: HeartbeatStatus::Enabled,
                last_beat_at: None,
                next_beat_at: Some(Utc::now()),
            })
            .await
            .unwrap();
        let run = storage
            .create_heartbeat_run(NewHeartbeatRun {
                heartbeat_id: heartbeat.id,
                state: HeartbeatRunState::Pending,
                scheduled_for: None,
                turn_id: None,
                output_text: None,
                error_message: None,
                started_at: None,
                completed_at: None,
            })
            .await
            .unwrap();

        storage
            .update_heartbeat_run_state(
                run.id,
                HeartbeatRunState::Completed,
                Some(Uuid::new_v4()),
                Some("done"),
                None,
            )
            .await
            .unwrap();

        let fetched = storage.get_heartbeat_run(run.id).await.unwrap().unwrap();
        assert_eq!(fetched.state, HeartbeatRunState::Completed);
        assert_eq!(fetched.output_text.as_deref(), Some("done"));
        assert!(fetched.completed_at.is_some());
    }
}
