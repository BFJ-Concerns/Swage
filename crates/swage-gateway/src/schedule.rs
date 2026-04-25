use chrono::{DateTime, Duration, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tokio::task::JoinHandle;
use tracing::warn;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    NewScheduledJob, NewScheduledRun, ScheduledJob, ScheduledJobKind, ScheduledJobStatus,
    ScheduledRun, ScheduledRunState, ScheduledRunTrigger, UpdateScheduledJob,
};

use crate::channel_gateway::{
    DeliveryRoutingRequest, deliver_to_targets, resolve_delivery_targets,
};
use crate::state::AppState;

const SCHEDULER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpecRequest {
    OneShot { at: String },
    Interval { every: String },
    Cron { cron: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateScheduledJobRequest {
    pub session_id: Uuid,
    pub message: String,
    pub schedule: ScheduleSpecRequest,
    #[serde(default = "default_local_routing")]
    pub routing: DeliveryRoutingRequest,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateScheduledJobRequest {
    pub message: Option<String>,
    pub schedule: Option<ScheduleSpecRequest>,
    pub routing: Option<DeliveryRoutingRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledJobResponse {
    pub id: Uuid,
    pub session_id: Uuid,
    pub message: String,
    pub status: String,
    pub schedule: ScheduleSpecRequest,
    pub routing: serde_json::Value,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledRunResponse {
    pub id: Uuid,
    pub job_id: Uuid,
    pub trigger: String,
    pub state: String,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub turn_id: Option<Uuid>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledJobListResponse {
    pub jobs: Vec<ScheduledJobResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledRunListResponse {
    pub runs: Vec<ScheduledRunResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriggerScheduledJobResponse {
    pub run: ScheduledRunResponse,
}

#[derive(Debug, Clone)]
enum ParsedSchedule {
    OneShot {
        at: DateTime<Utc>,
    },
    Interval {
        every: Duration,
        expression: String,
    },
    Cron {
        schedule: Box<Schedule>,
        expression: String,
    },
}

impl ParsedSchedule {
    fn parse(spec: &ScheduleSpecRequest) -> Result<Self, GatewayError> {
        match spec {
            ScheduleSpecRequest::OneShot { at } => Ok(Self::OneShot {
                at: DateTime::parse_from_rfc3339(at)
                    .map(|value| value.with_timezone(&Utc))
                    .map_err(|error| {
                        GatewayError::ValidationError(format!(
                            "invalid one-shot timestamp: {error}"
                        ))
                    })?,
            }),
            ScheduleSpecRequest::Interval { every } => Ok(Self::Interval {
                every: parse_interval(every)?,
                expression: every.clone(),
            }),
            ScheduleSpecRequest::Cron { cron } => Ok(Self::Cron {
                schedule: Box::new(Schedule::from_str(cron).map_err(|error| {
                    GatewayError::ValidationError(format!("invalid cron expression: {error}"))
                })?),
                expression: cron.clone(),
            }),
        }
    }

    fn kind(&self) -> ScheduledJobKind {
        match self {
            ParsedSchedule::OneShot { .. } => ScheduledJobKind::OneShot,
            ParsedSchedule::Interval { .. } => ScheduledJobKind::Interval,
            ParsedSchedule::Cron { .. } => ScheduledJobKind::Cron,
        }
    }

    fn expression(&self) -> String {
        match self {
            ParsedSchedule::OneShot { at } => at.to_rfc3339(),
            ParsedSchedule::Interval { expression, .. } => expression.clone(),
            ParsedSchedule::Cron { expression, .. } => expression.clone(),
        }
    }

    fn initial_next_run_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            ParsedSchedule::OneShot { at } => Some(*at),
            ParsedSchedule::Interval { every, .. } => Some(now + *every),
            ParsedSchedule::Cron { schedule, .. } => schedule.after(&now).next(),
        }
    }

    fn next_run_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            ParsedSchedule::OneShot { .. } => None,
            ParsedSchedule::Interval { every, .. } => Some(now + *every),
            ParsedSchedule::Cron { schedule, .. } => schedule.after(&now).next(),
        }
    }

    fn into_request(self) -> ScheduleSpecRequest {
        match self {
            ParsedSchedule::OneShot { at } => ScheduleSpecRequest::OneShot {
                at: at.to_rfc3339(),
            },
            ParsedSchedule::Interval { expression, .. } => {
                ScheduleSpecRequest::Interval { every: expression }
            }
            ParsedSchedule::Cron { expression, .. } => {
                ScheduleSpecRequest::Cron { cron: expression }
            }
        }
    }
}

fn parse_stored_schedule(
    kind: ScheduledJobKind,
    expression: &str,
) -> Result<ParsedSchedule, GatewayError> {
    match kind {
        ScheduledJobKind::OneShot => ParsedSchedule::parse(&ScheduleSpecRequest::OneShot {
            at: expression.to_string(),
        }),
        ScheduledJobKind::Interval => ParsedSchedule::parse(&ScheduleSpecRequest::Interval {
            every: expression.to_string(),
        }),
        ScheduledJobKind::Cron => ParsedSchedule::parse(&ScheduleSpecRequest::Cron {
            cron: expression.to_string(),
        }),
    }
}

fn parse_interval(text: &str) -> Result<Duration, GatewayError> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() < 3 || parts[0] != "every" {
        return Err(GatewayError::ValidationError(
            "intervals must use the form 'every N seconds|minutes|hours|days'".into(),
        ));
    }

    let quantity: i64 = parts[1].parse().map_err(|error| {
        GatewayError::ValidationError(format!("invalid interval quantity: {error}"))
    })?;
    if quantity <= 0 {
        return Err(GatewayError::ValidationError(
            "interval quantity must be positive".into(),
        ));
    }

    let seconds = match parts[2] {
        "second" | "seconds" => quantity,
        "minute" | "minutes" => quantity * 60,
        "hour" | "hours" => quantity * 60 * 60,
        "day" | "days" => quantity * 60 * 60 * 24,
        other => {
            return Err(GatewayError::ValidationError(format!(
                "unsupported interval unit: {other}"
            )));
        }
    };

    Ok(Duration::seconds(seconds))
}

pub async fn create_scheduled_job(
    state: &AppState,
    request: CreateScheduledJobRequest,
) -> Result<ScheduledJobResponse, GatewayError> {
    let session = state.storage.get_session(request.session_id).await?.ok_or(
        GatewayError::ConversationNotFound {
            id: request.session_id,
        },
    )?;

    let parsed = ParsedSchedule::parse(&request.schedule)?;
    let _ = resolve_delivery_targets(None, request.routing.clone())?;
    let now = Utc::now();
    let job = state
        .storage
        .create_scheduled_job(NewScheduledJob {
            session_id: request.session_id,
            agent_id: session.agent_id,
            message: request.message,
            status: ScheduledJobStatus::Active,
            kind: parsed.kind(),
            schedule_expression: parsed.expression(),
            delivery_routing: serde_json::to_value(&request.routing).map_err(|error| {
                GatewayError::ValidationError(format!("invalid routing payload: {error}"))
            })?,
            next_run_at: parsed.initial_next_run_at(now),
            last_run_at: None,
        })
        .await?;

    map_job_response(job)
}

pub async fn list_scheduled_jobs(
    state: &AppState,
) -> Result<ScheduledJobListResponse, GatewayError> {
    let jobs = state.storage.list_scheduled_jobs().await?;
    Ok(ScheduledJobListResponse {
        jobs: jobs
            .into_iter()
            .map(map_job_response)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub async fn update_scheduled_job(
    state: &AppState,
    job_id: Uuid,
    request: UpdateScheduledJobRequest,
) -> Result<ScheduledJobResponse, GatewayError> {
    let existing = state
        .storage
        .get_scheduled_job(job_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("scheduled job {job_id} not found"))
        })?;

    let now = Utc::now();
    let parsed = if let Some(schedule) = &request.schedule {
        Some(ParsedSchedule::parse(schedule)?)
    } else {
        None
    };
    if let Some(routing) = &request.routing {
        let _ = resolve_delivery_targets(None, routing.clone())?;
    }

    let updated = state
        .storage
        .update_scheduled_job(
            job_id,
            UpdateScheduledJob {
                message: request.message,
                kind: parsed.as_ref().map(ParsedSchedule::kind),
                schedule_expression: parsed.as_ref().map(ParsedSchedule::expression),
                delivery_routing: request
                    .routing
                    .map(|routing| serde_json::to_value(&routing).expect("routing serialises")),
                next_run_at: parsed.as_ref().map(|schedule| match existing.status {
                    ScheduledJobStatus::Completed => None,
                    _ => schedule.initial_next_run_at(now),
                }),
                ..Default::default()
            },
        )
        .await?;

    map_job_response(updated)
}

pub async fn pause_scheduled_job(
    state: &AppState,
    job_id: Uuid,
) -> Result<ScheduledJobResponse, GatewayError> {
    let updated = state
        .storage
        .update_scheduled_job(
            job_id,
            UpdateScheduledJob {
                status: Some(ScheduledJobStatus::Paused),
                ..Default::default()
            },
        )
        .await?;
    map_job_response(updated)
}

pub async fn resume_scheduled_job(
    state: &AppState,
    job_id: Uuid,
) -> Result<ScheduledJobResponse, GatewayError> {
    let existing = state
        .storage
        .get_scheduled_job(job_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("scheduled job {job_id} not found"))
        })?;
    let parsed = parse_stored_schedule(existing.kind.clone(), &existing.schedule_expression)?;
    let next_run_at = parsed.next_run_after(Utc::now());
    let status = if next_run_at.is_some() {
        ScheduledJobStatus::Active
    } else {
        ScheduledJobStatus::Completed
    };
    let updated = state
        .storage
        .update_scheduled_job(
            job_id,
            UpdateScheduledJob {
                status: Some(status),
                next_run_at: Some(next_run_at),
                ..Default::default()
            },
        )
        .await?;
    map_job_response(updated)
}

pub async fn delete_scheduled_job(state: &AppState, job_id: Uuid) -> Result<bool, GatewayError> {
    state
        .storage
        .delete_scheduled_job(job_id)
        .await
        .map_err(Into::into)
}

pub async fn list_scheduled_runs(
    state: &AppState,
    job_id: Uuid,
) -> Result<ScheduledRunListResponse, GatewayError> {
    let runs = state.storage.list_scheduled_runs_by_job(job_id).await?;
    Ok(ScheduledRunListResponse {
        runs: runs.into_iter().map(map_run_response).collect(),
    })
}

pub async fn trigger_scheduled_job(
    state: &AppState,
    job_id: Uuid,
) -> Result<TriggerScheduledJobResponse, GatewayError> {
    let job = state
        .storage
        .get_scheduled_job(job_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("scheduled job {job_id} not found"))
        })?;
    let run = execute_job(state, &job, ScheduledRunTrigger::Manual, None).await?;
    Ok(TriggerScheduledJobResponse {
        run: map_run_response(run),
    })
}

pub async fn tick_once(state: &AppState, now: DateTime<Utc>) -> Result<usize, GatewayError> {
    let jobs = state.storage.list_due_scheduled_jobs(now).await?;
    let mut executed = 0;
    for job in jobs {
        let parsed = parse_stored_schedule(job.kind.clone(), &job.schedule_expression)?;
        let next_status = match job.kind {
            ScheduledJobKind::OneShot => ScheduledJobStatus::Completed,
            _ => ScheduledJobStatus::Active,
        };
        state
            .storage
            .update_scheduled_job(
                job.id,
                UpdateScheduledJob {
                    status: Some(next_status),
                    last_run_at: Some(Some(now)),
                    next_run_at: Some(parsed.next_run_after(now)),
                    ..Default::default()
                },
            )
            .await?;
        let _ = execute_job(state, &job, ScheduledRunTrigger::Scheduled, Some(now)).await?;
        executed += 1;
    }
    Ok(executed)
}

pub fn spawn_scheduler_loop(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = tick_once(&state, Utc::now()).await {
                warn!(%error, "scheduled job tick failed");
            }
            tokio::time::sleep(SCHEDULER_POLL_INTERVAL).await;
        }
    })
}

async fn execute_job(
    state: &AppState,
    job: &ScheduledJob,
    trigger: ScheduledRunTrigger,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<ScheduledRun, GatewayError> {
    let run = state
        .storage
        .create_scheduled_run(NewScheduledRun {
            job_id: job.id,
            trigger,
            state: ScheduledRunState::Pending,
            scheduled_for,
            turn_id: None,
            output_text: None,
            error_message: None,
            started_at: None,
            completed_at: None,
        })
        .await?;

    let turn_id = crate::automated_turn::create_placeholder_turn(
        state,
        job.session_id,
        &job.agent_id,
        &job.message,
    )
    .await?;
    state
        .storage
        .update_scheduled_run_state(
            run.id,
            ScheduledRunState::Running,
            Some(turn_id),
            None,
            None,
        )
        .await?;

    match crate::automated_turn::run_placeholder_turn(state, job.session_id, &job.message, turn_id)
        .await
    {
        Ok(output_text) => {
            let routing: DeliveryRoutingRequest =
                serde_json::from_value(job.delivery_routing.clone()).map_err(|error| {
                    GatewayError::ValidationError(format!(
                        "invalid stored delivery routing: {error}"
                    ))
                })?;
            let targets = resolve_delivery_targets(None, routing)?;
            deliver_to_targets(state, job.session_id, &targets, &output_text).await?;
            state
                .storage
                .update_scheduled_run_state(
                    run.id,
                    ScheduledRunState::Completed,
                    Some(turn_id),
                    Some(&output_text),
                    None,
                )
                .await?;
        }
        Err(error) => {
            state
                .storage
                .update_scheduled_run_state(
                    run.id,
                    ScheduledRunState::Failed,
                    Some(turn_id),
                    None,
                    Some(&error.to_string()),
                )
                .await?;
            return Err(error);
        }
    }

    state
        .storage
        .get_scheduled_run(run.id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError("scheduled run disappeared".into()))
}

fn map_job_response(job: ScheduledJob) -> Result<ScheduledJobResponse, GatewayError> {
    Ok(ScheduledJobResponse {
        id: job.id,
        session_id: job.session_id,
        message: job.message,
        status: job.status.to_string(),
        schedule: parse_stored_schedule(job.kind, &job.schedule_expression)?.into_request(),
        routing: job.delivery_routing,
        next_run_at: job.next_run_at,
        last_run_at: job.last_run_at,
    })
}

fn map_run_response(run: ScheduledRun) -> ScheduledRunResponse {
    ScheduledRunResponse {
        id: run.id,
        job_id: run.job_id,
        trigger: run.trigger.to_string(),
        state: run.state.to_string(),
        scheduled_for: run.scheduled_for,
        turn_id: run.turn_id,
        output_text: run.output_text,
        error_message: run.error_message,
        created_at: run.created_at,
        started_at: run.started_at,
        completed_at: run.completed_at,
    }
}

fn default_local_routing() -> DeliveryRoutingRequest {
    DeliveryRoutingRequest {
        deliver_to_origin: false,
        home_target: None,
        explicit_targets: Vec::new(),
        store_local_output: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_natural_language_interval() {
        let parsed = ParsedSchedule::parse(&ScheduleSpecRequest::Interval {
            every: "every 5 minutes".into(),
        })
        .expect("interval");

        match parsed {
            ParsedSchedule::Interval { every, .. } => assert_eq!(every, Duration::minutes(5)),
            _ => panic!("expected interval"),
        }
    }

    #[test]
    fn cron_schedule_computes_next_occurrence() {
        let parsed = ParsedSchedule::parse(&ScheduleSpecRequest::Cron {
            cron: "0/5 * * * * * *".into(),
        })
        .expect("cron");
        let next = parsed.initial_next_run_at(Utc::now());
        assert!(next.is_some());
    }

    #[test]
    fn cron_schedule_respects_supplied_reference_time() {
        let parsed = ParsedSchedule::parse(&ScheduleSpecRequest::Cron {
            cron: "15 * * * * * *".into(),
        })
        .expect("cron");
        let reference = DateTime::parse_from_rfc3339("2026-04-17T12:34:10Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = parsed
            .initial_next_run_at(reference)
            .expect("next occurrence");
        assert_eq!(next.to_rfc3339(), "2026-04-17T12:34:15+00:00");
    }
}
