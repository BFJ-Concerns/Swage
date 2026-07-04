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
    if let Some(gate) = &request.gate {
        validate_gate(state, gate)?;
    }
    let gate = if request.remove_gate {
        Some(None)
    } else {
        request.gate.map(Some)
    };

    let updated = state
        .storage
        .update_schedule(
            schedule_id,
            UpdateSchedule {
                message: request.message,
                kind: parsed.as_ref().map(ParsedSpec::kind),
                expression: parsed.as_ref().map(ParsedSpec::expression),
                delivery_routing: request.routing.as_ref().map(routing_to_value).transpose()?,
                gate,
                next_run_at: parsed.as_ref().map(|spec| match existing.status {
                    ScheduleStatus::Completed => None,
                    _ => spec.initial_next_run_at(now),
                }),
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

pub async fn resume_schedule(
    state: &AppState,
    schedule_id: Uuid,
) -> Result<wire::Schedule, GatewayError> {
    let existing = get_schedule(state, schedule_id).await?;
    let parsed = ParsedSpec::parse_stored(existing.kind, &existing.expression)?;
    let next_run_at = parsed.next_run_after(Utc::now());
    let status = if next_run_at.is_some() {
        ScheduleStatus::Active
    } else {
        ScheduleStatus::Completed
    };
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
        SessionPolicy::Fresh { bench_id, agent_id } => {
            if agent_id.trim().is_empty() {
                return Err(GatewayError::ValidationError(
                    "fresh session policy needs a target agent".into(),
                ));
            }
            state
                .bench_store
                .get(*bench_id)
                .await?
                .ok_or(GatewayError::BenchNotFound { id: *bench_id })?;
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

pub fn spawn_schedule_loop(state: AppState) -> JoinHandle<()> {
    spawn_schedule_loop_until_cancelled(state, CancellationToken::new())
}

pub fn spawn_schedule_loop_until_cancelled(
    state: AppState,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    crate::background_loop::spawn_periodic_loop(
        SCHEDULE_POLL_INTERVAL,
        shutdown,
        "schedule tick failed",
        move || {
            let state = state.clone();
            async move {
                tick_once(&state, Utc::now()).await?;
                Ok(())
            }
        },
    )
}

// ---------------------------------------------------------------------------
// Run execution
// ---------------------------------------------------------------------------

/// Drive one run of a schedule from creation to a terminal state.
///
/// The run row is created first and every subsequent decision — overlap
/// skip, gate verdict, session, turn, output, failure — is written onto
/// it, so a crash at any point leaves a reconstructable record (startup
/// recovery fails whatever was left pending/running).
async fn execute_schedule(
    state: &AppState,
    schedule: &Schedule,
    trigger: ScheduleRunTrigger,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<ScheduleRun, GatewayError> {
    // Overlap check happens before this run's own row exists, so it only
    // sees predecessors (a manual trigger racing the tick loop, or a run
    // that outlived its interval).
    let overlapped = state
        .storage
        .has_unfinished_schedule_run(schedule.id)
        .await?;

    let run = state
        .storage
        .create_schedule_run(NewScheduleRun {
            schedule_id: schedule.id,
            trigger,
            scheduled_for,
        })
        .await?;

    if overlapped {
        return skip_run(state, schedule, run.id, SkipReason::Overlap, None).await;
    }

    // The previous outcome feeds both the gate and fresh-session context.
    let previous_outcome = previous_run_outcome(state, schedule.id).await?;

    if let Some(gate) = &schedule.gate {
        let decision =
            match run_gate_check(state, schedule, gate, previous_outcome.as_deref()).await {
                Ok(decision) => decision,
                Err(error) => {
                    // Fail closed: an unreachable gate fails the run visibly
                    // rather than silently waking the main model. The next
                    // occurrence tries again.
                    let message = format!("gate check failed: {error}");
                    state
                        .storage
                        .update_schedule_run(
                            run.id,
                            ScheduleRunUpdate {
                                state: Some(ScheduleRunState::Failed),
                                error_message: Some(message.clone()),
                                ..Default::default()
                            },
                        )
                        .await?;
                    return Err(GatewayError::ValidationError(message));
                }
            };

        record_gate_decision(state, schedule, run.id, &decision).await?;

        if decision.verdict == GateVerdict::Skip {
            return skip_run(state, schedule, run.id, SkipReason::Gate, Some(decision)).await;
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
        SessionPolicy::Fresh { bench_id, agent_id } => {
            let service =
                ConversationService::new(state.storage.clone(), state.bench_store.clone());
            let conversation = service
                .create_session_in_bench_for_agent(*bench_id, agent_id, None)
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
            // The session is busy with another turn (a user's, or another
            // automation's). That is an overlap in fact if not in name:
            // skip deliberately instead of stumbling into a failure.
            Err(GatewayError::TurnInProgress { .. }) => {
                return skip_run(state, schedule, run.id, SkipReason::Overlap, None).await;
            }
            Err(error) => {
                state
                    .storage
                    .update_schedule_run(
                        run.id,
                        ScheduleRunUpdate {
                            state: Some(ScheduleRunState::Failed),
                            error_message: Some(error.to_string()),
                            ..Default::default()
                        },
                    )
                    .await?;
                return Err(error);
            }
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

    match crate::automated_turn::run_placeholder_turn(state, session_id, &input, turn_id).await {
        Ok(output_text) => {
            // A delivery failure must still land the run in a terminal
            // state — the output is preserved on the run either way.
            match deliver_run_output(state, schedule, session_id, &output_text).await {
                Ok(()) => {
                    state
                        .storage
                        .update_schedule_run(
                            run.id,
                            ScheduleRunUpdate {
                                state: Some(ScheduleRunState::Completed),
                                output_text: Some(output_text),
                                ..Default::default()
                            },
                        )
                        .await?;
                }
                Err(error) => {
                    state
                        .storage
                        .update_schedule_run(
                            run.id,
                            ScheduleRunUpdate {
                                state: Some(ScheduleRunState::Failed),
                                output_text: Some(output_text),
                                error_message: Some(format!("delivery failed: {error}")),
                                ..Default::default()
                            },
                        )
                        .await?;
                    return Err(error);
                }
            }
        }
        Err(error) => {
            state
                .storage
                .update_schedule_run(
                    run.id,
                    ScheduleRunUpdate {
                        state: Some(ScheduleRunState::Failed),
                        error_message: Some(error.to_string()),
                        ..Default::default()
                    },
                )
                .await?;
            return Err(error);
        }
    }

    fetch_run(state, run.id).await
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
) -> Result<ScheduleRun, GatewayError> {
    state
        .storage
        .update_schedule_run(
            run_id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Skipped),
                skip_reason: Some(reason),
                gate,
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
