use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    GateConfig, GateDecision, NewSchedule, NewScheduleRun, Schedule, ScheduleCreator, ScheduleRun,
    ScheduleRunState, ScheduleRunUpdate, SessionPolicy, UpdateSchedule,
};
use crate::sqlite::SqliteStorage;
use crate::store::ScheduleStore;

const TERMINAL_RUN_STATES_SQL: &str = "'completed', 'completed_empty', 'failed', 'skipped'";

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

fn parse_enum<T: std::str::FromStr<Err = String>>(
    column: usize,
    value: &str,
) -> rusqlite::Result<T> {
    value.parse().map_err(|error: String| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })
}

/// Reassemble the session policy from its flattened columns.
fn parse_session_policy(
    policy: &str,
    space_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
) -> rusqlite::Result<SessionPolicy> {
    let invalid = |message: &str| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message.to_string(),
            )),
        )
    };
    match policy {
        "fresh" => Ok(SessionPolicy::Fresh {
            space_id: parse_uuid(
                &space_id.ok_or_else(|| invalid("fresh policy row is missing space_id"))?,
            )?,
            agent_id: agent_id.ok_or_else(|| invalid("fresh policy row is missing agent_id"))?,
        }),
        "continuation" => Ok(SessionPolicy::Continuation {
            session_id: parse_uuid(
                &session_id
                    .ok_or_else(|| invalid("continuation policy row is missing session_id"))?,
            )?,
        }),
        other => Err(invalid(&format!("unknown session policy: {other}"))),
    }
}

/// Flatten the session policy into its column values.
fn session_policy_columns(
    policy: &SessionPolicy,
) -> (&'static str, Option<String>, Option<String>, Option<String>) {
    match policy {
        SessionPolicy::Fresh { space_id, agent_id } => (
            "fresh",
            Some(space_id.to_string()),
            Some(agent_id.clone()),
            None,
        ),
        SessionPolicy::Continuation { session_id } => {
            ("continuation", None, None, Some(session_id.to_string()))
        }
    }
}

/// Reassemble schedule provenance from its flattened columns.
fn parse_creator(kind: &str, agent_id: Option<String>) -> rusqlite::Result<ScheduleCreator> {
    let invalid = |message: String| {
        rusqlite::Error::FromSqlConversionFailure(
            16,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
        )
    };
    match kind {
        "user" => Ok(ScheduleCreator::User),
        "agent" => Ok(ScheduleCreator::Agent {
            agent_id: agent_id
                .ok_or_else(|| invalid("agent creator row is missing creator_agent_id".into()))?,
        }),
        other => Err(invalid(format!("unknown schedule creator: {other}"))),
    }
}

/// Flatten schedule provenance into its column values.
fn creator_columns(creator: &ScheduleCreator) -> (&'static str, Option<String>) {
    match creator {
        ScheduleCreator::User => ("user", None),
        ScheduleCreator::Agent { agent_id } => ("agent", Some(agent_id.clone())),
    }
}

fn gate_from_columns(model: Option<String>, criteria: Option<String>) -> Option<GateConfig> {
    model.map(|model| GateConfig { model, criteria })
}

/// A gate decision is stored as three columns that are always written
/// together; a row with a verdict but no model (or vice versa) cannot be
/// produced through this module.
fn gate_decision_from_columns(
    column: usize,
    model: Option<String>,
    verdict: Option<String>,
    reasoning: Option<String>,
) -> rusqlite::Result<Option<GateDecision>> {
    match (model, verdict) {
        (Some(model), Some(verdict)) => Ok(Some(GateDecision {
            model,
            verdict: parse_enum(column, &verdict)?,
            reasoning: reasoning.unwrap_or_default(),
        })),
        _ => Ok(None),
    }
}

const SCHEDULE_COLUMNS: &str = "id, message, status, kind, expression, session_policy, space_id, \
     agent_id, session_id, delivery_json, gate_model, gate_criteria, next_run_at, last_run_at, \
     created_at, updated_at, creator_kind, creator_agent_id";

fn row_to_schedule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    let id: String = row.get(0)?;
    let message: String = row.get(1)?;
    let status: String = row.get(2)?;
    let kind: String = row.get(3)?;
    let expression: String = row.get(4)?;
    let session_policy: String = row.get(5)?;
    let space_id: Option<String> = row.get(6)?;
    let agent_id: Option<String> = row.get(7)?;
    let session_id: Option<String> = row.get(8)?;
    let delivery_json: String = row.get(9)?;
    let gate_model: Option<String> = row.get(10)?;
    let gate_criteria: Option<String> = row.get(11)?;
    let next_run_at: Option<String> = row.get(12)?;
    let last_run_at: Option<String> = row.get(13)?;
    let created_at: String = row.get(14)?;
    let updated_at: String = row.get(15)?;
    let creator_kind: String = row.get(16)?;
    let creator_agent_id: Option<String> = row.get(17)?;

    Ok(Schedule {
        id: parse_uuid(&id)?,
        message,
        status: parse_enum(2, &status)?,
        kind: parse_enum(3, &kind)?,
        expression,
        session_policy: parse_session_policy(&session_policy, space_id, agent_id, session_id)?,
        creator: parse_creator(&creator_kind, creator_agent_id)?,
        delivery_routing: serde_json::from_str(&delivery_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        gate: gate_from_columns(gate_model, gate_criteria),
        next_run_at: parse_optional_dt(next_run_at)?,
        last_run_at: parse_optional_dt(last_run_at)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

const SCHEDULE_RUN_COLUMNS: &str = "id, schedule_id, trigger_kind, state, skip_reason, \
     scheduled_for, session_id, turn_id, gate_model, gate_verdict, gate_reasoning, output_text, \
     error_message, created_at, started_at, completed_at";

fn row_to_schedule_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRun> {
    let id: String = row.get(0)?;
    let schedule_id: String = row.get(1)?;
    let trigger_kind: String = row.get(2)?;
    let state: String = row.get(3)?;
    let skip_reason: Option<String> = row.get(4)?;
    let scheduled_for: Option<String> = row.get(5)?;
    let session_id: Option<String> = row.get(6)?;
    let turn_id: Option<String> = row.get(7)?;
    let gate_model: Option<String> = row.get(8)?;
    let gate_verdict: Option<String> = row.get(9)?;
    let gate_reasoning: Option<String> = row.get(10)?;
    let output_text: Option<String> = row.get(11)?;
    let error_message: Option<String> = row.get(12)?;
    let created_at: String = row.get(13)?;
    let started_at: Option<String> = row.get(14)?;
    let completed_at: Option<String> = row.get(15)?;

    Ok(ScheduleRun {
        id: parse_uuid(&id)?,
        schedule_id: parse_uuid(&schedule_id)?,
        trigger: parse_enum(2, &trigger_kind)?,
        state: parse_enum(3, &state)?,
        skip_reason: skip_reason
            .as_deref()
            .map(|value| parse_enum(4, value))
            .transpose()?,
        scheduled_for: parse_optional_dt(scheduled_for)?,
        session_id: session_id.as_deref().map(parse_uuid).transpose()?,
        turn_id: turn_id.as_deref().map(parse_uuid).transpose()?,
        gate: gate_decision_from_columns(9, gate_model, gate_verdict, gate_reasoning)?,
        output_text,
        error_message,
        created_at: parse_dt(&created_at)?,
        started_at: parse_optional_dt(started_at)?,
        completed_at: parse_optional_dt(completed_at)?,
    })
}

fn serialise_json(value: &serde_json::Value) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::QueryFailed(error.to_string()))
}

fn serialise_optional_dt(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.to_rfc3339())
}

#[async_trait]
impl ScheduleStore for SqliteStorage {
    async fn create_schedule(&self, schedule: NewSchedule) -> Result<Schedule, StorageError> {
        let delivery_json = serialise_json(&schedule.delivery_routing)?;
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let (policy, space_id, agent_id, session_id) =
                session_policy_columns(&schedule.session_policy);
            let (creator_kind, creator_agent_id) = creator_columns(&schedule.creator);
            let next_run_at = serialise_optional_dt(schedule.next_run_at);
            let last_run_at = serialise_optional_dt(schedule.last_run_at);
            let (gate_model, gate_criteria) = match &schedule.gate {
                Some(gate) => (Some(gate.model.clone()), gate.criteria.clone()),
                None => (None, None),
            };

            conn.execute(
                "INSERT INTO schedules
                 (id, message, status, kind, expression, session_policy, space_id, agent_id,
                  session_id, creator_kind, creator_agent_id, delivery_json, gate_model,
                  gate_criteria, next_run_at, last_run_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                  ?17, ?17)",
                params![
                    id.to_string(),
                    &schedule.message,
                    schedule.status.to_string(),
                    schedule.kind.to_string(),
                    &schedule.expression,
                    policy,
                    &space_id,
                    &agent_id,
                    &session_id,
                    creator_kind,
                    &creator_agent_id,
                    &delivery_json,
                    &gate_model,
                    &gate_criteria,
                    &next_run_at,
                    &last_run_at,
                    &now_str
                ],
            )?;

            Ok(Schedule {
                id,
                message: schedule.message,
                status: schedule.status,
                kind: schedule.kind,
                expression: schedule.expression,
                session_policy: schedule.session_policy,
                creator: schedule.creator,
                delivery_routing: schedule.delivery_routing,
                gate: schedule.gate,
                next_run_at: schedule.next_run_at,
                last_run_at: schedule.last_run_at,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_schedule(&self, id: Uuid) -> Result<Option<Schedule>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {SCHEDULE_COLUMNS} FROM schedules WHERE id = ?1"),
                params![id.to_string()],
                row_to_schedule,
            )
            .optional()
        })
        .await
    }

    async fn list_schedules(&self) -> Result<Vec<Schedule>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SCHEDULE_COLUMNS} FROM schedules ORDER BY created_at ASC"
            ))?;
            let rows = stmt.query_map([], row_to_schedule)?;
            rows.collect()
        })
        .await
    }

    async fn list_due_schedules(&self, now: DateTime<Utc>) -> Result<Vec<Schedule>, StorageError> {
        let now = now.to_rfc3339();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SCHEDULE_COLUMNS} FROM schedules
                 WHERE status = 'active' AND next_run_at IS NOT NULL AND next_run_at <= ?1
                 ORDER BY next_run_at ASC, created_at ASC"
            ))?;
            let rows = stmt.query_map(params![&now], row_to_schedule)?;
            rows.collect()
        })
        .await
    }

    async fn update_schedule(
        &self,
        id: Uuid,
        update: UpdateSchedule,
    ) -> Result<Schedule, StorageError> {
        let existing = self
            .get_schedule(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "Schedule".into(),
                id: id.to_string(),
            })?;

        let updated = Schedule {
            id: existing.id,
            message: update.message.unwrap_or(existing.message),
            status: update.status.unwrap_or(existing.status),
            kind: update.kind.unwrap_or(existing.kind),
            expression: update.expression.unwrap_or(existing.expression),
            session_policy: existing.session_policy,
            creator: existing.creator,
            delivery_routing: update.delivery_routing.unwrap_or(existing.delivery_routing),
            gate: update.gate.unwrap_or(existing.gate),
            next_run_at: update.next_run_at.unwrap_or(existing.next_run_at),
            last_run_at: update.last_run_at.unwrap_or(existing.last_run_at),
            created_at: existing.created_at,
            updated_at: Utc::now(),
        };

        let delivery_json = serialise_json(&updated.delivery_routing)?;
        let next_run_at = serialise_optional_dt(updated.next_run_at);
        let last_run_at = serialise_optional_dt(updated.last_run_at);
        let updated_at = updated.updated_at.to_rfc3339();
        let (gate_model, gate_criteria) = match &updated.gate {
            Some(gate) => (Some(gate.model.clone()), gate.criteria.clone()),
            None => (None, None),
        };

        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE schedules
                 SET message = ?1,
                     status = ?2,
                     kind = ?3,
                     expression = ?4,
                     delivery_json = ?5,
                     gate_model = ?6,
                     gate_criteria = ?7,
                     next_run_at = ?8,
                     last_run_at = ?9,
                     updated_at = ?10
                 WHERE id = ?11",
                params![
                    &updated.message,
                    updated.status.to_string(),
                    updated.kind.to_string(),
                    &updated.expression,
                    &delivery_json,
                    &gate_model,
                    &gate_criteria,
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

    async fn delete_schedule(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM schedules WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    async fn create_schedule_run(&self, run: NewScheduleRun) -> Result<ScheduleRun, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let created_at = now.to_rfc3339();
            let scheduled_for = serialise_optional_dt(run.scheduled_for);
            let state = run.state.to_string();
            let skip_reason = run.skip_reason.map(|reason| reason.to_string());
            // A run born terminal (an overlap skip) is complete the moment it
            // is recorded; a pending run has not finished.
            let completed_at = matches!(
                run.state,
                ScheduleRunState::Completed
                    | ScheduleRunState::CompletedEmpty
                    | ScheduleRunState::Failed
                    | ScheduleRunState::Skipped
            )
            .then(|| now.to_rfc3339());

            conn.execute(
                "INSERT INTO schedule_runs
                 (id, schedule_id, trigger_kind, state, skip_reason, scheduled_for,
                  error_message, created_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.to_string(),
                    run.schedule_id.to_string(),
                    run.trigger.to_string(),
                    &state,
                    &skip_reason,
                    &scheduled_for,
                    &run.error_message,
                    &created_at,
                    &completed_at,
                ],
            )?;

            Ok(ScheduleRun {
                id,
                schedule_id: run.schedule_id,
                trigger: run.trigger,
                state: run.state,
                skip_reason: run.skip_reason,
                scheduled_for: run.scheduled_for,
                session_id: None,
                turn_id: None,
                gate: None,
                output_text: None,
                error_message: run.error_message,
                created_at: now,
                started_at: None,
                completed_at: completed_at.as_deref().map(parse_dt).transpose()?,
            })
        })
        .await
    }

    async fn get_schedule_run(&self, id: Uuid) -> Result<Option<ScheduleRun>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {SCHEDULE_RUN_COLUMNS} FROM schedule_runs WHERE id = ?1"),
                params![id.to_string()],
                row_to_schedule_run,
            )
            .optional()
        })
        .await
    }

    async fn list_schedule_runs(
        &self,
        schedule_id: Uuid,
    ) -> Result<Vec<ScheduleRun>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SCHEDULE_RUN_COLUMNS} FROM schedule_runs
                 WHERE schedule_id = ?1 ORDER BY created_at ASC"
            ))?;
            let rows = stmt.query_map(params![schedule_id.to_string()], row_to_schedule_run)?;
            rows.collect()
        })
        .await
    }

    async fn update_schedule_run(
        &self,
        id: Uuid,
        update: ScheduleRunUpdate,
    ) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            let state = update.state.map(|state| state.to_string());
            let skip_reason = update.skip_reason.map(|reason| reason.to_string());
            let session_id = update.session_id.map(|value| value.to_string());
            let turn_id = update.turn_id.map(|value| value.to_string());
            let (gate_model, gate_verdict, gate_reasoning) = match update.gate {
                Some(gate) => (
                    Some(gate.model),
                    Some(gate.verdict.to_string()),
                    Some(gate.reasoning),
                ),
                None => (None, None, None),
            };
            // Timestamps follow the state transition: entering Running
            // stamps started_at, any terminal state stamps completed_at.
            let started_at =
                matches!(update.state, Some(ScheduleRunState::Running)).then(|| now.clone());
            let completed_at = matches!(
                update.state,
                Some(
                    ScheduleRunState::Completed
                        | ScheduleRunState::CompletedEmpty
                        | ScheduleRunState::Failed
                        | ScheduleRunState::Skipped
                )
            )
            .then_some(now);

            conn.execute(
                "UPDATE schedule_runs
                 SET state = COALESCE(?1, state),
                     skip_reason = COALESCE(?2, skip_reason),
                     session_id = COALESCE(?3, session_id),
                     turn_id = COALESCE(?4, turn_id),
                     gate_model = COALESCE(?5, gate_model),
                     gate_verdict = COALESCE(?6, gate_verdict),
                     gate_reasoning = COALESCE(?7, gate_reasoning),
                     output_text = COALESCE(?8, output_text),
                     error_message = COALESCE(?9, error_message),
                     started_at = COALESCE(?10, started_at),
                     completed_at = COALESCE(?11, completed_at)
                 WHERE id = ?12",
                params![
                    &state,
                    &skip_reason,
                    &session_id,
                    &turn_id,
                    &gate_model,
                    &gate_verdict,
                    &gate_reasoning,
                    &update.output_text,
                    &update.error_message,
                    &started_at,
                    &completed_at,
                    id.to_string()
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn prune_schedule_runs(
        &self,
        now: DateTime<Utc>,
        max_age: std::time::Duration,
        max_per_schedule: u32,
    ) -> Result<u64, StorageError> {
        let cutoff = chrono::Duration::from_std(max_age)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))
            .map(|duration| now - duration)?
            .to_rfc3339();
        let max_per_schedule = i64::from(max_per_schedule);

        self.with_conn(move |conn| {
            let sql = format!(
                "DELETE FROM schedule_runs
                 WHERE state IN ({terminal_states})
                   AND (
                       created_at < ?1
                       OR id IN (
                           SELECT id
                           FROM (
                               SELECT id,
                                      ROW_NUMBER() OVER (
                                          PARTITION BY schedule_id
                                          ORDER BY created_at DESC, id DESC
                                      ) AS rn
                               FROM schedule_runs
                               WHERE state IN ({terminal_states})
                           )
                           WHERE rn > ?2
                       )
                   )",
                terminal_states = TERMINAL_RUN_STATES_SQL,
            );
            let changed = conn.execute(&sql, params![&cutoff, max_per_schedule])?;
            Ok(changed as u64)
        })
        .await
    }

    async fn recover_incomplete_schedule_runs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError> {
        let error_message = error_message.to_string();
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            let changed = conn.execute(
                "UPDATE schedule_runs
                 SET state = 'failed',
                     error_message = ?1,
                     completed_at = ?2
                 WHERE state IN ('pending', 'running')",
                params![&error_message, now],
            )?;
            Ok(changed as u64)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{
        GateVerdict, NewSpace, ScheduleKind, ScheduleRunTrigger, ScheduleStatus, SkipReason,
    };
    use crate::store::{SessionStore, SpaceStore};

    use super::*;

    async fn make_storage_with_session() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage
            .create(NewSpace {
                name: "sched".into(),
                slug: "sched".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(crate::model::NewSession {
                space_id: space.id,
                agent_id: "gateway".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        (storage, space.id, session.id)
    }

    fn fresh_schedule(space_id: Uuid) -> NewSchedule {
        NewSchedule {
            message: "run now".into(),
            status: ScheduleStatus::Active,
            kind: ScheduleKind::Interval,
            expression: "every 5 seconds".into(),
            session_policy: SessionPolicy::Fresh {
                space_id,
                agent_id: "gateway".into(),
            },
            creator: ScheduleCreator::User,
            delivery_routing: serde_json::json!({"store_local_output": true}),
            gate: None,
            next_run_at: Some(Utc::now()),
            last_run_at: None,
        }
    }

    async fn force_run_timestamp(storage: &SqliteStorage, run_id: Uuid, timestamp: DateTime<Utc>) {
        let value = timestamp.to_rfc3339();
        storage
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE schedule_runs
                     SET created_at = ?1, completed_at = COALESCE(completed_at, ?1)
                     WHERE id = ?2",
                    params![&value, run_id.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn schedule_round_trips_both_session_policies_and_gate() {
        let (storage, space_id, session_id) = make_storage_with_session().await;

        let fresh = storage
            .create_schedule(NewSchedule {
                gate: Some(GateConfig {
                    model: "small".into(),
                    criteria: Some("only when the queue is non-empty".into()),
                }),
                ..fresh_schedule(space_id)
            })
            .await
            .unwrap();
        let continuation = storage
            .create_schedule(NewSchedule {
                message: "check in".into(),
                session_policy: SessionPolicy::Continuation { session_id },
                ..fresh_schedule(space_id)
            })
            .await
            .unwrap();

        let fetched = storage.get_schedule(fresh.id).await.unwrap().unwrap();
        assert_eq!(
            fetched.session_policy,
            SessionPolicy::Fresh {
                space_id,
                agent_id: "gateway".into()
            }
        );
        let gate = fetched.gate.expect("gate persisted");
        assert_eq!(gate.model, "small");
        assert_eq!(
            gate.criteria.as_deref(),
            Some("only when the queue is non-empty")
        );

        let fetched = storage
            .get_schedule(continuation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fetched.session_policy,
            SessionPolicy::Continuation { session_id }
        );
        assert!(fetched.gate.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn creator_provenance_round_trips() {
        let (storage, space_id, _) = make_storage_with_session().await;

        // fresh_schedule stamps User; an agent-created schedule carries the
        // agent id through storage unchanged.
        let user = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();
        let agent = storage
            .create_schedule(NewSchedule {
                creator: ScheduleCreator::Agent {
                    agent_id: "researcher".into(),
                },
                ..fresh_schedule(space_id)
            })
            .await
            .unwrap();

        let fetched_user = storage.get_schedule(user.id).await.unwrap().unwrap();
        assert_eq!(fetched_user.creator, ScheduleCreator::User);

        let fetched_agent = storage.get_schedule(agent.id).await.unwrap().unwrap();
        assert_eq!(
            fetched_agent.creator,
            ScheduleCreator::Agent {
                agent_id: "researcher".into()
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn due_query_only_returns_active_rows() {
        let (storage, space_id, _) = make_storage_with_session().await;
        let due = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();
        storage
            .create_schedule(NewSchedule {
                status: ScheduleStatus::Paused,
                ..fresh_schedule(space_id)
            })
            .await
            .unwrap();

        let due_schedules = storage.list_due_schedules(Utc::now()).await.unwrap();
        assert_eq!(due_schedules.len(), 1);
        assert_eq!(due_schedules[0].id, due.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_can_remove_the_gate() {
        let (storage, space_id, _) = make_storage_with_session().await;
        let schedule = storage
            .create_schedule(NewSchedule {
                gate: Some(GateConfig {
                    model: "small".into(),
                    criteria: None,
                }),
                ..fresh_schedule(space_id)
            })
            .await
            .unwrap();

        let updated = storage
            .update_schedule(
                schedule.id,
                UpdateSchedule {
                    gate: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updated.gate.is_none());
        let fetched = storage.get_schedule(schedule.id).await.unwrap().unwrap();
        assert!(fetched.gate.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_accumulates_gate_decision_skip_reason_and_state() {
        let (storage, space_id, session_id) = make_storage_with_session().await;
        let schedule = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();
        let run = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Tick,
                Some(Utc::now()),
            ))
            .await
            .unwrap();
        assert_eq!(run.state, ScheduleRunState::Pending);

        storage
            .update_schedule_run(
                run.id,
                ScheduleRunUpdate {
                    state: Some(ScheduleRunState::Skipped),
                    skip_reason: Some(SkipReason::Gate),
                    gate: Some(GateDecision {
                        model: "small".into(),
                        verdict: GateVerdict::Skip,
                        reasoning: "nothing has changed since the last run".into(),
                    }),
                    session_id: Some(session_id),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let fetched = storage.get_schedule_run(run.id).await.unwrap().unwrap();
        assert_eq!(fetched.state, ScheduleRunState::Skipped);
        assert_eq!(fetched.skip_reason, Some(SkipReason::Gate));
        assert_eq!(fetched.session_id, Some(session_id));
        let gate = fetched.gate.expect("gate decision persisted");
        assert_eq!(gate.verdict, GateVerdict::Skip);
        assert_eq!(gate.reasoning, "nothing has changed since the last run");
        assert!(
            fetched.completed_at.is_some(),
            "skipping is a terminal state and stamps completed_at"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_in_flight_run_per_schedule_is_enforced_by_the_index() {
        let (storage, space_id, _) = make_storage_with_session().await;
        let schedule = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();

        // The first pending run claims the schedule's one in-flight slot.
        let first = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Tick,
                None,
            ))
            .await
            .unwrap();

        // A second in-flight run for the same schedule is refused by the
        // partial unique index — the insert *is* the overlap check.
        let clash = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Manual,
                None,
            ))
            .await;
        assert!(
            matches!(clash, Err(StorageError::ConstraintViolation(_))),
            "a second in-flight run must trip the guard, got {clash:?}"
        );

        // A run born terminal (an overlap skip) sits outside the predicate,
        // so it coexists with the live run freely.
        storage
            .create_schedule_run(NewScheduleRun::skipped_overlap(
                schedule.id,
                ScheduleRunTrigger::Manual,
                None,
                "a previous run of this schedule was still in flight",
            ))
            .await
            .unwrap();

        // Once the first run finishes, the slot frees and a fresh run inserts.
        storage
            .update_schedule_run(
                first.id,
                ScheduleRunUpdate {
                    state: Some(ScheduleRunState::Completed),
                    output_text: Some("done".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Tick,
                None,
            ))
            .await
            .expect("a new run inserts once the slot is free");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recovery_fails_incomplete_runs_and_leaves_finished_ones() {
        let (storage, space_id, _) = make_storage_with_session().await;
        // Incomplete runs land on separate schedules: the one-in-flight
        // guard forbids two non-terminal runs on a single schedule, and
        // recovery sweeps them across the whole table regardless.
        let schedule_a = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();
        let schedule_b = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();

        // A finished run first, so it frees schedule A's slot for the
        // still-running run that follows.
        let completed = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule_a.id,
                ScheduleRunTrigger::Manual,
                None,
            ))
            .await
            .unwrap();
        storage
            .update_schedule_run(
                completed.id,
                ScheduleRunUpdate {
                    state: Some(ScheduleRunState::Completed),
                    output_text: Some("done".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let running = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule_a.id,
                ScheduleRunTrigger::Tick,
                Some(Utc::now()),
            ))
            .await
            .unwrap();
        storage
            .update_schedule_run(
                running.id,
                ScheduleRunUpdate {
                    state: Some(ScheduleRunState::Running),
                    turn_id: Some(Uuid::new_v4()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let pending = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule_b.id,
                ScheduleRunTrigger::Tick,
                Some(Utc::now()),
            ))
            .await
            .unwrap();

        let count = storage
            .recover_incomplete_schedule_runs("gateway stopped")
            .await
            .unwrap();
        assert_eq!(count, 2);

        for id in [running.id, pending.id] {
            let recovered = storage.get_schedule_run(id).await.unwrap().unwrap();
            assert_eq!(recovered.state, ScheduleRunState::Failed);
            assert_eq!(recovered.error_message.as_deref(), Some("gateway stopped"));
            assert!(recovered.completed_at.is_some());
        }

        let untouched = storage
            .get_schedule_run(completed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(untouched.state, ScheduleRunState::Completed);
        assert_eq!(untouched.output_text.as_deref(), Some("done"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retention_reaps_terminal_runs_older_than_the_age_cap() {
        let (storage, space_id, _) = make_storage_with_session().await;
        let schedule = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-07-06T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let old = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Tick,
                None,
            ))
            .await
            .unwrap();
        storage
            .update_schedule_run(
                old.id,
                ScheduleRunUpdate {
                    state: Some(ScheduleRunState::Completed),
                    output_text: Some("old".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        force_run_timestamp(&storage, old.id, now - chrono::Duration::days(91)).await;

        let recent = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Manual,
                None,
            ))
            .await
            .unwrap();
        storage
            .update_schedule_run(
                recent.id,
                ScheduleRunUpdate {
                    state: Some(ScheduleRunState::CompletedEmpty),
                    output_text: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        force_run_timestamp(&storage, recent.id, now - chrono::Duration::days(10)).await;

        let removed = storage
            .prune_schedule_runs(now, std::time::Duration::from_secs(90 * 24 * 60 * 60), 500)
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let remaining = storage.list_schedule_runs(schedule.id).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, recent.id);
        assert_eq!(remaining[0].state, ScheduleRunState::CompletedEmpty);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retention_reaps_beyond_count_cap_but_spares_active_runs() {
        let (storage, space_id, _) = make_storage_with_session().await;
        let schedule = storage
            .create_schedule(fresh_schedule(space_id))
            .await
            .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-07-06T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut terminal_ids = Vec::new();
        for offset in 0..3 {
            let run = storage
                .create_schedule_run(NewScheduleRun::pending(
                    schedule.id,
                    ScheduleRunTrigger::Tick,
                    None,
                ))
                .await
                .unwrap();
            storage
                .update_schedule_run(
                    run.id,
                    ScheduleRunUpdate {
                        state: Some(ScheduleRunState::Completed),
                        output_text: Some(format!("run {offset}")),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            force_run_timestamp(&storage, run.id, now - chrono::Duration::hours(3 - offset)).await;
            terminal_ids.push(run.id);
        }

        let active = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Manual,
                None,
            ))
            .await
            .unwrap();
        force_run_timestamp(&storage, active.id, now - chrono::Duration::hours(10)).await;

        let removed = storage
            .prune_schedule_runs(now, std::time::Duration::from_secs(365 * 24 * 60 * 60), 2)
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let remaining = storage.list_schedule_runs(schedule.id).await.unwrap();
        let remaining_ids: std::collections::HashSet<_> =
            remaining.iter().map(|run| run.id).collect();
        assert!(!remaining_ids.contains(&terminal_ids[0]));
        assert!(remaining_ids.contains(&terminal_ids[1]));
        assert!(remaining_ids.contains(&terminal_ids[2]));
        assert!(remaining_ids.contains(&active.id));
        assert_eq!(
            remaining
                .iter()
                .find(|run| run.id == active.id)
                .unwrap()
                .state,
            ScheduleRunState::Pending
        );
    }
}
