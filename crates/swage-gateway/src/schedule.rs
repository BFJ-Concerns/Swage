//! The unified schedule domain: durable definitions of recurring or
//! one-off agent work, and the recorded runs they produce.
//!
//! A schedule carries when to fire (cron, interval, or one-shot), what to
//! ask (message), which agent runs it and in what kind of session (session
//! policy), where output goes (delivery routing), and optionally a
//! cheap-model gate that can decide "no work needed" before the main model
//! wakes.
//!
//! Every due occurrence produces a recorded run — executed, gated out, or
//! skipped for overlap — so the run history is the complete account of
//! what the scheduler did and why. Gate verdicts land on the run row *and*
//! in `audit_events`; state transitions are audited by SQL trigger.

use chrono::{DateTime, Duration, Utc};
use cron::Schedule as CronSchedule;
use serde::Deserialize;
use std::str::FromStr;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_core::{ChatMessage, CompletionRequest, GatewayError, Role};
use swage_protocol::client::schedule as wire;
use swage_storage::{
    GateConfig, GateDecision, GateVerdict, NewAuditEvent, NewSchedule, NewScheduleRun, Schedule,
    ScheduleKind, ScheduleRun, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate,
    ScheduleStatus, SessionPolicy, SkipReason, UpdateSchedule,
};

use crate::channel_gateway::{
    DeliveryRoutingRequest, deliver_to_targets, resolve_delivery_targets,
};
use crate::service::ConversationService;
use crate::state::AppState;

const SCHEDULE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const RUN_HISTORY_RETENTION_INTERVAL: Duration = Duration::hours(1);

/// Cap on the gate model's reply — a verdict and a sentence or two of
/// reasoning, not an essay.
const GATE_MAX_TOKENS: u32 = 500;

// ---------------------------------------------------------------------------
// Spec parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum ParsedSpec {
    OneShot {
        at: DateTime<Utc>,
    },
    Interval {
        every: Duration,
        expression: String,
    },
    Cron {
        schedule: Box<CronSchedule>,
        expression: String,
    },
}

impl ParsedSpec {
    fn parse(spec: &wire::ScheduleSpec) -> Result<Self, GatewayError> {
        match spec {
            wire::ScheduleSpec::OneShot { at } => Ok(Self::OneShot {
                at: DateTime::parse_from_rfc3339(at)
                    .map(|value| value.with_timezone(&Utc))
                    .map_err(|error| {
                        GatewayError::ValidationError(format!(
                            "invalid one-shot timestamp: {error}"
                        ))
                    })?,
            }),
            wire::ScheduleSpec::Interval { every } => Ok(Self::Interval {
                every: parse_interval(every)?,
                expression: every.clone(),
            }),
            wire::ScheduleSpec::Cron { cron } => Ok(Self::Cron {
                schedule: Box::new(CronSchedule::from_str(cron).map_err(|error| {
                    GatewayError::ValidationError(format!("invalid cron expression: {error}"))
                })?),
                expression: cron.clone(),
            }),
        }
    }

    fn parse_stored(kind: ScheduleKind, expression: &str) -> Result<Self, GatewayError> {
        Self::parse(&match kind {
            ScheduleKind::OneShot => wire::ScheduleSpec::OneShot {
                at: expression.to_string(),
            },
            ScheduleKind::Interval => wire::ScheduleSpec::Interval {
                every: expression.to_string(),
            },
            ScheduleKind::Cron => wire::ScheduleSpec::Cron {
                cron: expression.to_string(),
            },
        })
    }

    fn kind(&self) -> ScheduleKind {
        match self {
            ParsedSpec::OneShot { .. } => ScheduleKind::OneShot,
            ParsedSpec::Interval { .. } => ScheduleKind::Interval,
            ParsedSpec::Cron { .. } => ScheduleKind::Cron,
        }
    }

    fn expression(&self) -> String {
        match self {
            ParsedSpec::OneShot { at } => at.to_rfc3339(),
            ParsedSpec::Interval { expression, .. } => expression.clone(),
            ParsedSpec::Cron { expression, .. } => expression.clone(),
        }
    }

    fn initial_next_run_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            ParsedSpec::OneShot { at } => Some(*at),
            ParsedSpec::Interval { every, .. } => Some(now + *every),
            ParsedSpec::Cron { schedule, .. } => schedule.after(&now).next(),
        }
    }

    fn next_run_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            ParsedSpec::OneShot { .. } => None,
            ParsedSpec::Interval { every, .. } => Some(now + *every),
            ParsedSpec::Cron { schedule, .. } => schedule.after(&now).next(),
        }
    }

    /// Compute the armed lifecycle state for this spec as of `now`: the
    /// status the schedule should carry and its next fire time.
    ///
    /// This is the one place that decides whether a spec is live or spent,
    /// and it is deliberately shared by resume and update so both honour the
    /// same one-shot contract:
    ///
    /// - A future occurrence arms the schedule `Active` to fire then. For a
    ///   one-shot that means a moment still ahead; for an interval or cron,
    ///   the next computed occurrence (always ahead).
    /// - No occurrence, or a one-shot whose moment already lies in the past,
    ///   completes the schedule *without firing*. This is the pre-unification
    ///   one-shot semantics: a moment missed while paused is completed
    ///   deliberately and shown in status, never fired late as a surprise.
    ///
    /// Because a one-shot only reaches the past by having fired or by having
    /// its moment lapse, "future moment" and "never fired" coincide — so a
    /// fired one-shot never re-arms here, and only a *new* future moment
    /// (via update) can revive it.
    fn arm(&self, now: DateTime<Utc>) -> (ScheduleStatus, Option<DateTime<Utc>>) {
        match self.initial_next_run_at(now) {
            Some(next) if next > now => (ScheduleStatus::Active, Some(next)),
            _ => (ScheduleStatus::Completed, None),
        }
    }

    fn into_spec(self) -> wire::ScheduleSpec {
        match self {
            ParsedSpec::OneShot { at } => wire::ScheduleSpec::OneShot {
                at: at.to_rfc3339(),
            },
            ParsedSpec::Interval { expression, .. } => {
                wire::ScheduleSpec::Interval { every: expression }
            }
            ParsedSpec::Cron { expression, .. } => wire::ScheduleSpec::Cron { cron: expression },
        }
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

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

pub async fn create_schedule(
    state: &AppState,
    request: wire::CreateScheduleRequest,
) -> Result<wire::Schedule, GatewayError> {
    if request.message.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "schedule message must not be empty".into(),
        ));
    }
    let parsed = ParsedSpec::parse(&request.spec)?;
    validate_session_policy(state, &request.session).await?;
    if let Some(gate) = &request.gate {
        validate_gate(state, gate)?;
    }

    let routing = request
        .routing
        .unwrap_or_else(DeliveryRoutingRequest::local_only);
    // Schedules have no channel origin; reject routing that would need one.
    let _ = resolve_delivery_targets(None, routing.clone())?;

    let now = Utc::now();
    let schedule = state
        .storage
        .create_schedule(NewSchedule {
            message: request.message,
            status: ScheduleStatus::Active,
            kind: parsed.kind(),
            expression: parsed.expression(),
            session_policy: request.session,
            delivery_routing: routing_to_value(&routing)?,
            gate: request.gate,
            next_run_at: parsed.initial_next_run_at(now),
            last_run_at: None,
        })
        .await?;

    map_schedule(schedule)
}

pub async fn list_schedules(state: &AppState) -> Result<wire::ScheduleListResponse, GatewayError> {
    let schedules = state.storage.list_schedules().await?;
    Ok(wire::ScheduleListResponse {
        schedules: schedules
            .into_iter()
            .map(map_schedule)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub async fn update_schedule(
    state: &AppState,
    schedule_id: Uuid,
    request: wire::UpdateScheduleRequest,
) -> Result<wire::Schedule, GatewayError> {
    let existing = get_schedule(state, schedule_id).await?;

    let now = Utc::now();
    let parsed = request.spec.as_ref().map(ParsedSpec::parse).transpose()?;
    if let Some(routing) = &request.routing {
        let _ = resolve_delivery_targets(None, routing.clone())?;
    }
    if let Some(wire::ScheduleGateUpdate::Set(gate)) = &request.gate {
        validate_gate(state, gate)?;
    }
    // Fold the wire tri-state into the storage tri-state: absent leaves the
    // gate unchanged, `Set` installs it, `Clear` removes it.
    let gate = request.gate.map(|update| match update {
        wire::ScheduleGateUpdate::Set(gate) => Some(gate),
        wire::ScheduleGateUpdate::Clear => None,
    });

    // Re-arm from the new spec. Recomputing status alongside next_run_at is
    // what lets an update revive a `completed` schedule (a one-shot given a
    // future moment fires again); see `ParsedSpec::arm`. A paused schedule
    // stays paused through an edit — resume is what re-arms it — so its
    // lifecycle is left untouched here.
    let armed = match existing.status {
        ScheduleStatus::Paused => None,
        _ => parsed.as_ref().map(|spec| spec.arm(now)),
    };

    let updated = state
        .storage
        .update_schedule(
            schedule_id,
            UpdateSchedule {
                message: request.message,
                status: armed.map(|(status, _)| status),
                kind: parsed.as_ref().map(ParsedSpec::kind),
                expression: parsed.as_ref().map(ParsedSpec::expression),
                delivery_routing: request.routing.as_ref().map(routing_to_value).transpose()?,
                gate,
                next_run_at: armed.map(|(_, next_run_at)| next_run_at),
                ..Default::default()
            },
        )
        .await?;

    map_schedule(updated)
}

pub async fn pause_schedule(
    state: &AppState,
    schedule_id: Uuid,
) -> Result<wire::Schedule, GatewayError> {
    let updated = state
        .storage
        .update_schedule(
            schedule_id,
            UpdateSchedule {
                status: Some(ScheduleStatus::Paused),
                ..Default::default()
            },
        )
        .await?;
    map_schedule(updated)
}

/// Resume a paused schedule, re-arming it from its stored spec.
///
/// The one-shot contract lives in [`ParsedSpec::arm`]: a paused one-shot
/// whose moment is still ahead resumes `Active` and fires at that moment; a
/// one-shot whose moment passed while paused (or that had already fired)
/// resumes `Completed` without firing. Recurring schedules resume to their
/// next occurrence.
pub async fn resume_schedule(
    state: &AppState,
    schedule_id: Uuid,
) -> Result<wire::Schedule, GatewayError> {
    let existing = get_schedule(state, schedule_id).await?;
    let parsed = ParsedSpec::parse_stored(existing.kind, &existing.expression)?;
    let (status, next_run_at) = parsed.arm(Utc::now());
    let updated = state
        .storage
        .update_schedule(
            schedule_id,
            UpdateSchedule {
                status: Some(status),
                next_run_at: Some(next_run_at),
                ..Default::default()
            },
        )
        .await?;
    map_schedule(updated)
}

pub async fn delete_schedule(state: &AppState, schedule_id: Uuid) -> Result<bool, GatewayError> {
    state
        .storage
        .delete_schedule(schedule_id)
        .await
        .map_err(Into::into)
}

pub async fn list_schedule_runs(
    state: &AppState,
    schedule_id: Uuid,
) -> Result<wire::ScheduleRunListResponse, GatewayError> {
    let runs = state.storage.list_schedule_runs(schedule_id).await?;
    Ok(wire::ScheduleRunListResponse {
        runs: runs.into_iter().map(map_run).collect(),
    })
}

/// Fire a schedule immediately, regardless of its next occurrence.
///
/// Returns the recorded run in whatever state it ended — completed, failed,
/// gated out, or skipped for overlap. The run record is the outcome; a
/// failed run is not an HTTP error.
pub async fn trigger_schedule(
    state: &AppState,
    schedule_id: Uuid,
) -> Result<wire::TriggerScheduleResponse, GatewayError> {
    let schedule = get_schedule(state, schedule_id).await?;
    let run = execute_schedule(state, &schedule, ScheduleRunTrigger::Manual, None).await?;
    Ok(wire::TriggerScheduleResponse { run: map_run(run) })
}

async fn get_schedule(state: &AppState, schedule_id: Uuid) -> Result<Schedule, GatewayError> {
    state
        .storage
        .get_schedule(schedule_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError(format!("schedule {schedule_id} not found")))
}

async fn validate_session_policy(
    state: &AppState,
    policy: &SessionPolicy,
) -> Result<(), GatewayError> {
    match policy {
        SessionPolicy::Fresh { space_id, agent_id } => {
            if agent_id.trim().is_empty() {
                return Err(GatewayError::ValidationError(
                    "fresh session policy needs a target agent".into(),
                ));
            }
            state
                .space_store
                .get(*space_id)
                .await?
                .ok_or(GatewayError::SpaceNotFound { id: *space_id })?;
        }
        SessionPolicy::Continuation { session_id } => {
            state
                .storage
                .get_session(*session_id)
                .await?
                .ok_or(GatewayError::ConversationNotFound { id: *session_id })?;
        }
    }
    Ok(())
}

fn validate_gate(state: &AppState, gate: &GateConfig) -> Result<(), GatewayError> {
    let config = state.active_config();
    resolve_gate_model(&config, &gate.model)
        .map(|_| ())
        .ok_or_else(|| {
            GatewayError::ValidationError(format!(
                "gate model '{}' is not in the configured model catalogue",
                gate.model
            ))
        })
}

/// Resolve a catalogue entry id or alias to the provider-facing model id.
fn resolve_gate_model(config: &swage_config::ResolvedConfig, name: &str) -> Option<String> {
    if let Some(entry) = config.models.entries.get(name) {
        return Some(entry.model_id.clone());
    }
    config
        .models
        .entries
        .values()
        .find(|entry| entry.aliases.iter().any(|alias| alias == name))
        .map(|entry| entry.model_id.clone())
}

fn routing_to_value(routing: &DeliveryRoutingRequest) -> Result<serde_json::Value, GatewayError> {
    serde_json::to_value(routing)
        .map_err(|error| GatewayError::ValidationError(format!("invalid routing payload: {error}")))
}

// ---------------------------------------------------------------------------
// Ticking
// ---------------------------------------------------------------------------

pub async fn tick_once(state: &AppState, now: DateTime<Utc>) -> Result<usize, GatewayError> {
    let schedules = state.storage.list_due_schedules(now).await?;
    let mut executed = 0;
    for schedule in schedules {
        let parsed = ParsedSpec::parse_stored(schedule.kind, &schedule.expression)?;
        let next_status = match schedule.kind {
            ScheduleKind::OneShot => ScheduleStatus::Completed,
            _ => ScheduleStatus::Active,
        };
        state
            .storage
            .update_schedule(
                schedule.id,
                UpdateSchedule {
                    status: Some(next_status),
                    last_run_at: Some(Some(now)),
                    next_run_at: Some(parsed.next_run_after(now)),
                    ..Default::default()
                },
            )
            .await?;
        // One schedule's bad day must not stall the rest of the tick: the
        // failure is already on the run record, so log and move on.
        if let Err(error) =
            execute_schedule(state, &schedule, ScheduleRunTrigger::Tick, Some(now)).await
        {
            tracing::warn!(schedule_id = %schedule.id, %error, "schedule run failed");
        }
        executed += 1;
    }
    Ok(executed)
}

async fn prune_schedule_run_history(
    state: &AppState,
    now: DateTime<Utc>,
) -> Result<u64, GatewayError> {
    let retention = state.active_config().scheduling;
    let max_age = std::time::Duration::from_secs(
        retention
            .run_history_max_age_days
            .saturating_mul(24 * 60 * 60),
    );
    state
        .storage
        .prune_schedule_runs(now, max_age, retention.run_history_max_per_schedule)
        .await
        .map_err(Into::into)
}

pub fn spawn_schedule_loop(state: AppState) -> JoinHandle<()> {
    spawn_schedule_loop_until_cancelled(state, CancellationToken::new())
}

pub fn spawn_schedule_loop_until_cancelled(
    state: AppState,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    let last_retention_sweep = std::sync::Arc::new(std::sync::Mutex::new(None::<DateTime<Utc>>));
    crate::background_loop::spawn_periodic_loop(
        SCHEDULE_POLL_INTERVAL,
        shutdown,
        "schedule tick failed",
        move || {
            let state = state.clone();
            let last_retention_sweep = last_retention_sweep.clone();
            async move {
                let now = Utc::now();
                tick_once(&state, now).await?;

                // The scheduler tick is deliberately frequent; retention is
                // not. Reuse the existing loop without turning every clock
                // check into a history-table DELETE.
                let should_prune = {
                    let mut last = last_retention_sweep
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let due = last
                        .map(|previous| now - previous >= RUN_HISTORY_RETENTION_INTERVAL)
                        .unwrap_or(true);
                    if due {
                        *last = Some(now);
                    }
                    due
                };
                if should_prune {
                    let removed = prune_schedule_run_history(&state, now).await?;
                    if removed > 0 {
                        tracing::info!(removed, "pruned schedule run history");
                    }
                }
                Ok(())
            }
        },
    )
}

// ---------------------------------------------------------------------------
// Run execution
// ---------------------------------------------------------------------------

/// The terminal shape a run's body reaches: either it executed and
/// produced output, or it deliberately did not run.
enum RunOutcome {
    Completed {
        output: String,
    },
    /// Gated out, or the target session was busy. `message` records a clean,
    /// human-readable cause on the run row (gate skips leave it to the
    /// recorded gate decision instead).
    Skipped {
        reason: SkipReason,
        gate: Option<GateDecision>,
        message: Option<String>,
    },
}

/// A run-body failure carried to the single catch point: the message to
/// record, plus any output the turn produced before a later step (delivery)
/// failed — that output stays on the failed run row.
struct RunFailure {
    message: String,
    output: Option<String>,
}

impl From<GatewayError> for RunFailure {
    fn from(error: GatewayError) -> Self {
        Self {
            message: error.to_string(),
            output: None,
        }
    }
}

impl From<swage_core::StorageError> for RunFailure {
    fn from(error: swage_core::StorageError) -> Self {
        Self {
            message: error.to_string(),
            output: None,
        }
    }
}

/// Drive one run of a schedule from creation to a terminal state.
///
/// The pending-run insert is the overlap check (a partial unique index
/// admits one non-terminal run per schedule); losing that race records a
/// terminal overlap skip. Past that point a run row exists, and a single
/// catch guarantees it reaches a terminal state on every exit — completed,
/// skipped, or failed with any output it managed to produce. The returned
/// run *is* the outcome: a failure lives on the row, not in an `Err`, so a
/// manual trigger returns the failed run and the tick loop simply moves on.
async fn execute_schedule(
    state: &AppState,
    schedule: &Schedule,
    trigger: ScheduleRunTrigger,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<ScheduleRun, GatewayError> {
    let run = match state
        .storage
        .create_schedule_run(NewScheduleRun::pending(schedule.id, trigger, scheduled_for))
        .await
    {
        Ok(run) => run,
        // The one-in-flight index rejected the insert: a predecessor is
        // still running (a manual trigger racing the tick, or another manual
        // trigger). Record the skip terminally with a clean message — never
        // the raw constraint error — rather than executing over the top.
        Err(swage_core::StorageError::ConstraintViolation(_)) => {
            return record_overlap_skip(state, schedule, trigger, scheduled_for).await;
        }
        Err(error) => return Err(error.into()),
    };

    match run_body(state, schedule, &run).await {
        Ok(RunOutcome::Completed { output }) => complete_run(state, run.id, output).await,
        Ok(RunOutcome::Skipped {
            reason,
            gate,
            message,
        }) => skip_run(state, schedule, run.id, reason, gate, message).await,
        Err(RunFailure { message, output }) => {
            fail_run(state, schedule, run.id, message, output).await
        }
    }
}

/// Everything between a fresh pending run and its terminal outcome: gate
/// check, session resolution, turn creation, turn execution, delivery. Every
/// fallible step propagates its error as a [`RunFailure`] to the single
/// catch in [`execute_schedule`]; the one non-failure exit is a busy
/// session, which is a deliberate overlap skip.
async fn run_body(
    state: &AppState,
    schedule: &Schedule,
    run: &ScheduleRun,
) -> Result<RunOutcome, RunFailure> {
    // The previous outcome feeds both the gate and fresh-session context.
    let previous_outcome = previous_run_outcome(state, schedule.id).await?;

    if let Some(gate) = &schedule.gate {
        let decision = run_gate_check(state, schedule, gate, previous_outcome.as_deref())
            .await
            .map_err(|error| RunFailure {
                // Fail closed: an unreachable gate fails the run visibly
                // rather than silently waking the main model. The next
                // occurrence tries again.
                message: format!("gate check failed: {error}"),
                output: None,
            })?;

        record_gate_decision(state, schedule, run.id, &decision).await?;

        if decision.verdict == GateVerdict::Skip {
            return Ok(RunOutcome::Skipped {
                reason: SkipReason::Gate,
                gate: Some(decision),
                message: None,
            });
        }
        state
            .storage
            .update_schedule_run(
                run.id,
                ScheduleRunUpdate {
                    gate: Some(decision),
                    ..Default::default()
                },
            )
            .await?;
    }

    // Resolve the session per policy: fresh creates one for the target
    // agent, continuation picks up the named session's agent as it stands
    // right now.
    let (session_id, agent_id, input) = match &schedule.session_policy {
        SessionPolicy::Fresh { space_id, agent_id } => {
            let service =
                ConversationService::new(state.storage.clone(), state.space_store.clone());
            let conversation = service
                .create_session_in_space_for_agent(*space_id, agent_id, None)
                .await?;
            let input = compose_fresh_opening(&schedule.message, previous_outcome.as_deref());
            (conversation.id, agent_id.clone(), input)
        }
        SessionPolicy::Continuation { session_id } => {
            let session = state
                .storage
                .get_session(*session_id)
                .await?
                .ok_or(GatewayError::ConversationNotFound { id: *session_id })?;
            (session.id, session.agent_id, schedule.message.clone())
        }
    };
    state
        .storage
        .update_schedule_run(
            run.id,
            ScheduleRunUpdate {
                session_id: Some(session_id),
                ..Default::default()
            },
        )
        .await?;

    let turn_id =
        match crate::automated_turn::create_placeholder_turn(state, session_id, &agent_id, &input)
            .await
        {
            Ok(turn_id) => turn_id,
            // The target session is busy with another turn — a user's, or
            // another automation that won the atomic turn guard on a shared
            // continuation session. Overlap in fact if not in name: skip
            // deliberately instead of stumbling into a failure.
            Err(GatewayError::TurnInProgress { .. }) => {
                return Ok(RunOutcome::Skipped {
                    reason: SkipReason::Overlap,
                    gate: None,
                    message: Some("the target session was busy with another turn".into()),
                });
            }
            Err(error) => return Err(error.into()),
        };
    state
        .storage
        .update_schedule_run(
            run.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Running),
                turn_id: Some(turn_id),
                ..Default::default()
            },
        )
        .await?;

    let output =
        crate::automated_turn::run_placeholder_turn(state, session_id, &input, turn_id).await?;

    // Delivery runs after a successful turn: a failure here still fails the
    // run, but the output the turn produced is preserved on the row.
    deliver_run_output(state, schedule, session_id, &output)
        .await
        .map_err(|error| RunFailure {
            message: format!("delivery failed: {error}"),
            output: Some(output.clone()),
        })?;

    Ok(RunOutcome::Completed { output })
}

/// Record a run that never executes because a predecessor still holds the
/// schedule's one in-flight slot. Inserted straight into its terminal
/// skipped state with a clean message — never a raw constraint error — plus
/// an explicit audit event, since no state transition fired the audit
/// trigger for a row that was born terminal.
async fn record_overlap_skip(
    state: &AppState,
    schedule: &Schedule,
    trigger: ScheduleRunTrigger,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<ScheduleRun, GatewayError> {
    const MESSAGE: &str = "a previous run of this schedule was still in flight";
    let run = state
        .storage
        .create_schedule_run(NewScheduleRun::skipped_overlap(
            schedule.id,
            trigger,
            scheduled_for,
            MESSAGE,
        ))
        .await?;
    record_overlap_audit(state, schedule, run.id, MESSAGE).await?;
    tracing::info!(
        schedule_id = %schedule.id,
        run_id = %run.id,
        reason = %SkipReason::Overlap,
        "schedule run skipped"
    );
    fetch_run(state, run.id).await
}

/// Mark a completed run and return the recorded row.
async fn complete_run(
    state: &AppState,
    run_id: Uuid,
    output: String,
) -> Result<ScheduleRun, GatewayError> {
    let state_value = if output.trim().is_empty() {
        ScheduleRunState::CompletedEmpty
    } else {
        ScheduleRunState::Completed
    };
    state
        .storage
        .update_schedule_run(
            run_id,
            ScheduleRunUpdate {
                state: Some(state_value),
                output_text: Some(output),
                ..Default::default()
            },
        )
        .await?;
    fetch_run(state, run_id).await
}

/// Land a run terminally failed — preserving any output produced before the
/// failure — and return the recorded row. The failure lives on the run, not
/// in an `Err`: a manual trigger returns the failed run, and the tick loop
/// moves on to the next schedule.
async fn fail_run(
    state: &AppState,
    schedule: &Schedule,
    run_id: Uuid,
    message: String,
    output: Option<String>,
) -> Result<ScheduleRun, GatewayError> {
    state
        .storage
        .update_schedule_run(
            run_id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Failed),
                error_message: Some(message),
                output_text: output,
                ..Default::default()
            },
        )
        .await?;
    tracing::warn!(
        schedule_id = %schedule.id,
        run_id = %run_id,
        "schedule run failed"
    );
    fetch_run(state, run_id).await
}

async fn deliver_run_output(
    state: &AppState,
    schedule: &Schedule,
    session_id: Uuid,
    output_text: &str,
) -> Result<(), GatewayError> {
    let routing: DeliveryRoutingRequest = serde_json::from_value(schedule.delivery_routing.clone())
        .map_err(|error| {
            GatewayError::ValidationError(format!("invalid stored delivery routing: {error}"))
        })?;
    let targets = resolve_delivery_targets(None, routing)?;
    deliver_to_targets(state, session_id, &targets, output_text).await
}

async fn skip_run(
    state: &AppState,
    schedule: &Schedule,
    run_id: Uuid,
    reason: SkipReason,
    gate: Option<GateDecision>,
    message: Option<String>,
) -> Result<ScheduleRun, GatewayError> {
    state
        .storage
        .update_schedule_run(
            run_id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Skipped),
                skip_reason: Some(reason),
                gate,
                error_message: message,
                ..Default::default()
            },
        )
        .await?;
    tracing::info!(
        schedule_id = %schedule.id,
        run_id = %run_id,
        reason = %reason,
        "schedule run skipped"
    );
    fetch_run(state, run_id).await
}

/// Overlap skips inserted straight into a terminal state never trip the
/// audit trigger (which fires on state *transitions*), so they get an
/// explicit audit event — the same pattern the gate decision uses.
async fn record_overlap_audit(
    state: &AppState,
    schedule: &Schedule,
    run_id: Uuid,
    cause: &str,
) -> Result<(), GatewayError> {
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "schedule_run".into(),
            entity_id: run_id.to_string(),
            event_kind: "overlap_skip".into(),
            summary: "schedule run skipped: a previous run was still in flight".into(),
            metadata: serde_json::json!({
                "schedule_id": schedule.id,
                "cause": cause,
            }),
        })
        .await?;
    Ok(())
}

async fn fetch_run(state: &AppState, run_id: Uuid) -> Result<ScheduleRun, GatewayError> {
    state
        .storage
        .get_schedule_run(run_id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError("schedule run disappeared".into()))
}

/// The most recent finished run's outcome, for the gate and for fresh
/// session context. Skipped runs are non-events and don't count.
async fn previous_run_outcome(
    state: &AppState,
    schedule_id: Uuid,
) -> Result<Option<String>, GatewayError> {
    let runs = state.storage.list_schedule_runs(schedule_id).await?;
    Ok(runs.into_iter().rev().find_map(|run| match run.state {
        ScheduleRunState::Completed => run.output_text,
        ScheduleRunState::CompletedEmpty => {
            Some("The previous run completed without assistant output.".into())
        }
        ScheduleRunState::Failed => Some(format!(
            "The previous run failed: {}",
            run.error_message.unwrap_or_else(|| "unknown error".into())
        )),
        _ => None,
    }))
}

fn compose_fresh_opening(message: &str, previous_outcome: Option<&str>) -> String {
    let previous = previous_outcome.unwrap_or("This is the first run of this schedule.");
    format!("{message}\n\n## Previous run outcome\n\n{previous}")
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GateReply {
    decision: String,
    #[serde(default)]
    reasoning: String,
}

async fn run_gate_check(
    state: &AppState,
    schedule: &Schedule,
    gate: &GateConfig,
    previous_outcome: Option<&str>,
) -> Result<GateDecision, GatewayError> {
    let config = state.active_config();
    let model_id = resolve_gate_model(&config, &gate.model).ok_or_else(|| {
        GatewayError::ValidationError(format!(
            "gate model '{}' is not in the configured model catalogue",
            gate.model
        ))
    })?;

    let prompt = build_gate_prompt(schedule, gate, previous_outcome);
    let response = state
        .active_provider()
        .complete(CompletionRequest {
            messages: vec![ChatMessage::new(Role::User, prompt)],
            model: Some(model_id),
            max_tokens: Some(GATE_MAX_TOKENS),
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: Vec::new(),
        })
        .await
        .map_err(|error| {
            GatewayError::ValidationError(format!("gate model call failed: {error}"))
        })?;

    Ok(parse_gate_reply(&gate.model, &response.content))
}

fn build_gate_prompt(
    schedule: &Schedule,
    gate: &GateConfig,
    previous_outcome: Option<&str>,
) -> String {
    let mut prompt = format!(
        "You are the pre-flight check for a scheduled agent task. Decide whether the task \
         actually needs to run right now, or whether nothing has changed and waking the main \
         model would be wasted work.\n\n## Scheduled task\n\n{}\n",
        schedule.message
    );
    if let Some(criteria) = &gate.criteria {
        prompt.push_str(&format!("\n## When the task should run\n\n{criteria}\n"));
    }
    prompt.push_str(&format!(
        "\n## Previous run outcome\n\n{}\n",
        previous_outcome.unwrap_or("No previous run has finished yet.")
    ));
    prompt.push_str(
        "\nAnswer with a single JSON object and nothing else: \
         {\"decision\": \"run\" | \"skip\", \"reasoning\": \"one or two sentences\"}. \
         Answer \"skip\" only when you are confident no work is needed.",
    );
    prompt
}

/// Parse the gate model's reply, failing open: an unparseable verdict runs
/// the task (the gate is an economy measure, not a gatekeeper of last
/// resort) and preserves the raw reply as the recorded reasoning.
fn parse_gate_reply(gate_model: &str, content: &str) -> GateDecision {
    let json_slice = content
        .find('{')
        .and_then(|start| content.rfind('}').map(|end| &content[start..=end]));
    if let Some(reply) = json_slice.and_then(|slice| serde_json::from_str::<GateReply>(slice).ok())
    {
        let verdict = match reply.decision.trim().to_lowercase().as_str() {
            "skip" => GateVerdict::Skip,
            _ => GateVerdict::Run,
        };
        GateDecision {
            model: gate_model.to_string(),
            verdict,
            reasoning: reply.reasoning,
        }
    } else {
        GateDecision {
            model: gate_model.to_string(),
            verdict: GateVerdict::Run,
            reasoning: format!("gate reply was not parseable, running anyway: {content}"),
        }
    }
}

/// Gate verdicts are decisions no SQL trigger can see, so they get an
/// explicit audit event alongside the fields on the run row.
async fn record_gate_decision(
    state: &AppState,
    schedule: &Schedule,
    run_id: Uuid,
    decision: &GateDecision,
) -> Result<(), GatewayError> {
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "schedule_run".into(),
            entity_id: run_id.to_string(),
            event_kind: "gate_decision".into(),
            summary: format!("gate verdict: {}", decision.verdict),
            metadata: serde_json::json!({
                "schedule_id": schedule.id,
                "model": decision.model,
                "verdict": decision.verdict.to_string(),
                "reasoning": decision.reasoning,
            }),
        })
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Wire mapping
// ---------------------------------------------------------------------------

fn map_schedule(schedule: Schedule) -> Result<wire::Schedule, GatewayError> {
    Ok(wire::Schedule {
        id: schedule.id,
        message: schedule.message,
        status: schedule.status,
        spec: ParsedSpec::parse_stored(schedule.kind, &schedule.expression)?.into_spec(),
        session: schedule.session_policy,
        routing: serde_json::from_value(schedule.delivery_routing).map_err(|error| {
            GatewayError::ValidationError(format!("invalid stored delivery routing: {error}"))
        })?,
        gate: schedule.gate,
        next_run_at: schedule.next_run_at,
        last_run_at: schedule.last_run_at,
        created_at: schedule.created_at,
        updated_at: schedule.updated_at,
    })
}

fn map_run(run: ScheduleRun) -> wire::ScheduleRun {
    wire::ScheduleRun {
        id: run.id,
        schedule_id: run.schedule_id,
        trigger: run.trigger,
        state: run.state,
        skip_reason: run.skip_reason,
        scheduled_for: run.scheduled_for,
        session_id: run.session_id,
        turn_id: run.turn_id,
        gate: run.gate,
        output_text: run.output_text,
        error_message: run.error_message,
        created_at: run.created_at,
        started_at: run.started_at,
        completed_at: run.completed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_natural_language_interval() {
        let parsed = ParsedSpec::parse(&wire::ScheduleSpec::Interval {
            every: "every 5 minutes".into(),
        })
        .expect("interval");

        match parsed {
            ParsedSpec::Interval { every, .. } => assert_eq!(every, Duration::minutes(5)),
            _ => panic!("expected interval"),
        }
    }

    #[test]
    fn cron_schedule_computes_next_occurrence() {
        let parsed = ParsedSpec::parse(&wire::ScheduleSpec::Cron {
            cron: "0/5 * * * * * *".into(),
        })
        .expect("cron");
        let next = parsed.initial_next_run_at(Utc::now());
        assert!(next.is_some());
    }

    #[test]
    fn cron_schedule_respects_supplied_reference_time() {
        let parsed = ParsedSpec::parse(&wire::ScheduleSpec::Cron {
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

    #[test]
    fn gate_reply_parses_json_verdicts() {
        let decision = parse_gate_reply(
            "small",
            r#"{"decision": "skip", "reasoning": "queue empty"}"#,
        );
        assert_eq!(decision.verdict, GateVerdict::Skip);
        assert_eq!(decision.reasoning, "queue empty");

        let decision = parse_gate_reply(
            "small",
            "Sure! Here is my answer:\n{\"decision\": \"run\", \"reasoning\": \"new items\"}",
        );
        assert_eq!(decision.verdict, GateVerdict::Run);
    }

    #[test]
    fn unparseable_gate_reply_fails_open_to_run() {
        let decision = parse_gate_reply("small", "I cannot decide.");
        assert_eq!(decision.verdict, GateVerdict::Run);
        assert!(decision.reasoning.contains("not parseable"));
    }

    #[test]
    fn fresh_opening_carries_previous_outcome() {
        let opening = compose_fresh_opening("Check the queue.", Some("Queue was empty."));
        assert!(opening.starts_with("Check the queue."));
        assert!(opening.contains("## Previous run outcome"));
        assert!(opening.contains("Queue was empty."));

        let first = compose_fresh_opening("Check the queue.", None);
        assert!(first.contains("first run of this schedule"));
    }
}
