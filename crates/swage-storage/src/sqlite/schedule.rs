use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    NewScheduledJob, NewScheduledRun, ScheduledJob, ScheduledRun, ScheduledRunState,
    UpdateScheduledJob,
};
use crate::sqlite::SqliteStorage;
use crate::store::ScheduleStore;

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

fn row_to_scheduled_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledJob> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let message: String = row.get(3)?;
    let status: String = row.get(4)?;
    let kind: String = row.get(5)?;
    let schedule_expression: String = row.get(6)?;
    let delivery_json: String = row.get(7)?;
    let next_run_at: Option<String> = row.get(8)?;
    let last_run_at: Option<String> = row.get(9)?;
    let created_at: String = row.get(10)?;
    let updated_at: String = row.get(11)?;

    Ok(ScheduledJob {
        id: parse_uuid(&id)?,
        session_id: parse_uuid(&session_id)?,
        agent_id,
        message,
        status: status.parse().map_err(|error: String| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        })?,
        kind: kind.parse().map_err(|error: String| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        })?,
        schedule_expression,
        delivery_routing: serde_json::from_str(&delivery_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        next_run_at: parse_optional_dt(next_run_at)?,
        last_run_at: parse_optional_dt(last_run_at)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

fn row_to_scheduled_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledRun> {
    let id: String = row.get(0)?;
    let job_id: String = row.get(1)?;
    let trigger_kind: String = row.get(2)?;
    let state: String = row.get(3)?;
    let scheduled_for: Option<String> = row.get(4)?;
    let turn_id: Option<String> = row.get(5)?;
    let output_text: Option<String> = row.get(6)?;
    let error_message: Option<String> = row.get(7)?;
    let created_at: String = row.get(8)?;
    let started_at: Option<String> = row.get(9)?;
    let completed_at: Option<String> = row.get(10)?;

    Ok(ScheduledRun {
        id: parse_uuid(&id)?,
        job_id: parse_uuid(&job_id)?,
        trigger: trigger_kind.parse().map_err(|error: String| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        })?,
        state: state.parse().map_err(|error: String| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
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

fn serialise_json(value: &serde_json::Value) -> Result<String, rusqlite::Error> {
    serde_json::to_string(value)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn serialise_optional_dt(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.to_rfc3339())
}

#[async_trait]
impl ScheduleStore for SqliteStorage {
    async fn create_scheduled_job(
        &self,
        job: NewScheduledJob,
    ) -> Result<ScheduledJob, StorageError> {
        let delivery_json = serialise_json(&job.delivery_routing)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let next_run_at = serialise_optional_dt(job.next_run_at);
            let last_run_at = serialise_optional_dt(job.last_run_at);

            conn.execute(
                "INSERT INTO scheduled_jobs
                 (id, session_id, agent_id, message, status, kind, schedule_expression, delivery_json, next_run_at, last_run_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    id.to_string(),
                    job.session_id.to_string(),
                    &job.agent_id,
                    &job.message,
                    job.status.to_string(),
                    job.kind.to_string(),
                    &job.schedule_expression,
                    &delivery_json,
                    &next_run_at,
                    &last_run_at,
                    &now_str
                ],
            )?;

            Ok(ScheduledJob {
                id,
                session_id: job.session_id,
                agent_id: job.agent_id,
                message: job.message,
                status: job.status,
                kind: job.kind,
                schedule_expression: job.schedule_expression,
                delivery_routing: job.delivery_routing,
                next_run_at: job.next_run_at,
                last_run_at: job.last_run_at,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_scheduled_job(&self, id: Uuid) -> Result<Option<ScheduledJob>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, session_id, agent_id, message, status, kind, schedule_expression,
                        delivery_json, next_run_at, last_run_at, created_at, updated_at
                 FROM scheduled_jobs WHERE id = ?1",
                params![id.to_string()],
                row_to_scheduled_job,
            )
            .optional()
        })
        .await
    }

    async fn list_scheduled_jobs(&self) -> Result<Vec<ScheduledJob>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, message, status, kind, schedule_expression,
                        delivery_json, next_run_at, last_run_at, created_at, updated_at
                 FROM scheduled_jobs ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], row_to_scheduled_job)?;
            rows.collect()
        })
        .await
    }

    async fn list_due_scheduled_jobs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScheduledJob>, StorageError> {
        let now = now.to_rfc3339();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, message, status, kind, schedule_expression,
                        delivery_json, next_run_at, last_run_at, created_at, updated_at
                 FROM scheduled_jobs
                 WHERE status = 'active' AND next_run_at IS NOT NULL AND next_run_at <= ?1
                 ORDER BY next_run_at ASC, created_at ASC",
            )?;
            let rows = stmt.query_map(params![&now], row_to_scheduled_job)?;
            rows.collect()
        })
        .await
    }

    async fn update_scheduled_job(
        &self,
        id: Uuid,
        update: UpdateScheduledJob,
    ) -> Result<ScheduledJob, StorageError> {
        let existing = self
            .get_scheduled_job(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "ScheduledJob".into(),
                id: id.to_string(),
            })?;

        let updated = ScheduledJob {
            id: existing.id,
            session_id: existing.session_id,
            agent_id: existing.agent_id,
            message: update.message.unwrap_or(existing.message),
            status: update.status.unwrap_or(existing.status),
            kind: update.kind.unwrap_or(existing.kind),
            schedule_expression: update
                .schedule_expression
                .unwrap_or(existing.schedule_expression),
            delivery_routing: update.delivery_routing.unwrap_or(existing.delivery_routing),
            next_run_at: update.next_run_at.unwrap_or(existing.next_run_at),
            last_run_at: update.last_run_at.unwrap_or(existing.last_run_at),
            created_at: existing.created_at,
            updated_at: Utc::now(),
        };

        let delivery_json = serialise_json(&updated.delivery_routing)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        let next_run_at = serialise_optional_dt(updated.next_run_at);
        let last_run_at = serialise_optional_dt(updated.last_run_at);
        let updated_at = updated.updated_at.to_rfc3339();

        self.with_conn(|conn| {
            conn.execute(
                "UPDATE scheduled_jobs
                 SET message = ?1,
                     status = ?2,
                     kind = ?3,
                     schedule_expression = ?4,
                     delivery_json = ?5,
                     next_run_at = ?6,
                     last_run_at = ?7,
                     updated_at = ?8
                 WHERE id = ?9",
                params![
                    &updated.message,
                    updated.status.to_string(),
                    updated.kind.to_string(),
                    &updated.schedule_expression,
                    &delivery_json,
                    &next_run_at,
                    &last_run_at,
                    &updated_at,
                    id.to_string(),
                ],
            )?;
            Ok(updated)
        })
        .await
    }

    async fn delete_scheduled_job(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(|conn| {
            let affected = conn.execute(
                "DELETE FROM scheduled_jobs WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    async fn create_scheduled_run(
        &self,
        run: NewScheduledRun,
    ) -> Result<ScheduledRun, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let created_at = now.to_rfc3339();
            let scheduled_for = serialise_optional_dt(run.scheduled_for);
            let turn_id = run.turn_id.map(|value| value.to_string());
            let started_at = serialise_optional_dt(run.started_at);
            let completed_at = serialise_optional_dt(run.completed_at);

            conn.execute(
                "INSERT INTO scheduled_runs
                 (id, job_id, trigger_kind, state, scheduled_for, turn_id, output_text, error_message, created_at, started_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id.to_string(),
                    run.job_id.to_string(),
                    run.trigger.to_string(),
                    run.state.to_string(),
                    &scheduled_for,
                    &turn_id,
                    &run.output_text,
                    &run.error_message,
                    &created_at,
                    &started_at,
                    &completed_at
                ],
            )?;

            Ok(ScheduledRun {
                id,
                job_id: run.job_id,
                trigger: run.trigger,
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

    async fn get_scheduled_run(&self, id: Uuid) -> Result<Option<ScheduledRun>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, job_id, trigger_kind, state, scheduled_for, turn_id, output_text, error_message, created_at, started_at, completed_at
                 FROM scheduled_runs WHERE id = ?1",
                params![id.to_string()],
                row_to_scheduled_run,
            )
            .optional()
        })
        .await
    }

    async fn list_scheduled_runs_by_job(
        &self,
        job_id: Uuid,
    ) -> Result<Vec<ScheduledRun>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, job_id, trigger_kind, state, scheduled_for, turn_id, output_text, error_message, created_at, started_at, completed_at
                 FROM scheduled_runs WHERE job_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![job_id.to_string()], row_to_scheduled_run)?;
            rows.collect()
        })
        .await
    }

    async fn update_scheduled_run_state(
        &self,
        id: Uuid,
        state: ScheduledRunState,
        turn_id: Option<Uuid>,
        output_text: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        self.with_conn(|conn| {
            let now = Utc::now().to_rfc3339();
            let turn_id = turn_id.map(|value| value.to_string());
            let started_at = if matches!(state, ScheduledRunState::Running) {
                Some(now.clone())
            } else {
                None
            };
            let completed_at = if matches!(
                state,
                ScheduledRunState::Completed | ScheduledRunState::Failed
            ) {
                Some(now)
            } else {
                None
            };

            conn.execute(
                "UPDATE scheduled_runs
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
                    id.to_string()
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn recover_incomplete_scheduled_runs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError> {
        self.with_conn(|conn| {
            let now = Utc::now().to_rfc3339();
            let changed = conn.execute(
                "UPDATE scheduled_runs
                 SET state = 'failed',
                     error_message = ?1,
                     completed_at = ?2
                 WHERE state IN ('pending', 'running')",
                params![error_message, now],
            )?;
            Ok(changed as u64)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{
        NewBench, ScheduledJobKind, ScheduledJobStatus, ScheduledRunState, ScheduledRunTrigger,
    };
    use crate::store::{BenchStore, SessionStore};

    use super::*;

    async fn make_storage_with_session() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let bench = storage
            .create(NewBench {
                name: "sched".into(),
                slug: "sched".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(crate::model::NewSession {
                bench_id: bench.id,
                agent_id: "gateway".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        (storage, session.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn scheduled_job_round_trips_and_due_query_filters_active_rows() {
        let (storage, session_id) = make_storage_with_session().await;
        let now = Utc::now();
        let due = storage
            .create_scheduled_job(NewScheduledJob {
                session_id,
                agent_id: "gateway".into(),
                message: "run now".into(),
                status: ScheduledJobStatus::Active,
                kind: ScheduledJobKind::Interval,
                schedule_expression: "every 5 seconds".into(),
                delivery_routing: serde_json::json!({"store_local_output": true}),
                next_run_at: Some(now),
                last_run_at: None,
            })
            .await
            .unwrap();
        storage
            .create_scheduled_job(NewScheduledJob {
                session_id,
                agent_id: "gateway".into(),
                message: "paused".into(),
                status: ScheduledJobStatus::Paused,
                kind: ScheduledJobKind::Interval,
                schedule_expression: "every 5 seconds".into(),
                delivery_routing: serde_json::json!({"store_local_output": true}),
                next_run_at: Some(now),
                last_run_at: None,
            })
            .await
            .unwrap();

        let due_jobs = storage.list_due_scheduled_jobs(now).await.unwrap();
        assert_eq!(due_jobs.len(), 1);
        assert_eq!(due_jobs[0].id, due.id);

        let fetched = storage.get_scheduled_job(due.id).await.unwrap().unwrap();
        assert_eq!(fetched.message, "run now");
        assert_eq!(fetched.delivery_routing["store_local_output"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn scheduled_run_round_trips_and_updates_state() {
        let (storage, session_id) = make_storage_with_session().await;
        let job = storage
            .create_scheduled_job(NewScheduledJob {
                session_id,
                agent_id: "gateway".into(),
                message: "run".into(),
                status: ScheduledJobStatus::Active,
                kind: ScheduledJobKind::OneShot,
                schedule_expression: "2026-04-17T12:00:00Z".into(),
                delivery_routing: serde_json::json!({"store_local_output": true}),
                next_run_at: None,
                last_run_at: None,
            })
            .await
            .unwrap();
        let run = storage
            .create_scheduled_run(NewScheduledRun {
                job_id: job.id,
                trigger: ScheduledRunTrigger::Manual,
                state: ScheduledRunState::Pending,
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
            .update_scheduled_run_state(
                run.id,
                ScheduledRunState::Completed,
                Some(Uuid::new_v4()),
                Some("done"),
                None,
            )
            .await
            .unwrap();

        let fetched = storage.get_scheduled_run(run.id).await.unwrap().unwrap();
        assert_eq!(fetched.state, ScheduledRunState::Completed);
        assert_eq!(fetched.output_text.as_deref(), Some("done"));
        assert!(fetched.completed_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recovery_fails_incomplete_scheduled_runs() {
        let (storage, session_id) = make_storage_with_session().await;
        let job = storage
            .create_scheduled_job(NewScheduledJob {
                session_id,
                agent_id: "gateway".into(),
                message: "run".into(),
                status: ScheduledJobStatus::Active,
                kind: ScheduledJobKind::OneShot,
                schedule_expression: "2026-04-17T12:00:00Z".into(),
                delivery_routing: serde_json::json!({"store_local_output": true}),
                next_run_at: None,
                last_run_at: None,
            })
            .await
            .unwrap();
        let running = storage
            .create_scheduled_run(NewScheduledRun {
                job_id: job.id,
                trigger: ScheduledRunTrigger::Scheduled,
                state: ScheduledRunState::Running,
                scheduled_for: Some(Utc::now()),
                turn_id: Some(Uuid::new_v4()),
                output_text: None,
                error_message: None,
                started_at: Some(Utc::now()),
                completed_at: None,
            })
            .await
            .unwrap();
        let pending = storage
            .create_scheduled_run(NewScheduledRun {
                job_id: job.id,
                trigger: ScheduledRunTrigger::Scheduled,
                state: ScheduledRunState::Pending,
                scheduled_for: Some(Utc::now()),
                turn_id: None,
                output_text: None,
                error_message: None,
                started_at: None,
                completed_at: None,
            })
            .await
            .unwrap();
        let completed = storage
            .create_scheduled_run(NewScheduledRun {
                job_id: job.id,
                trigger: ScheduledRunTrigger::Manual,
                state: ScheduledRunState::Completed,
                scheduled_for: None,
                turn_id: Some(Uuid::new_v4()),
                output_text: Some("done".into()),
                error_message: None,
                started_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
            })
            .await
            .unwrap();

        let count = storage
            .recover_incomplete_scheduled_runs("gateway stopped")
            .await
            .unwrap();
        assert_eq!(count, 2);

        let recovered = storage
            .get_scheduled_run(running.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, ScheduledRunState::Failed);
        assert_eq!(recovered.error_message.as_deref(), Some("gateway stopped"));
        assert!(recovered.completed_at.is_some());

        let recovered = storage
            .get_scheduled_run(pending.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, ScheduledRunState::Failed);
        assert_eq!(recovered.error_message.as_deref(), Some("gateway stopped"));
        assert!(recovered.completed_at.is_some());

        let untouched = storage
            .get_scheduled_run(completed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(untouched.state, ScheduledRunState::Completed);
        assert_eq!(untouched.output_text.as_deref(), Some("done"));
    }
}
