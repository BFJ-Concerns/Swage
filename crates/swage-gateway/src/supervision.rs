use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use swage_core::{GatewayError, StorageError};
use swage_storage::{ScheduleRunState, SessionPolicy, TurnState};

use crate::state::AppState;

const STUCK_AFTER: Duration = Duration::minutes(5);

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RunListQuery {
    pub active_only: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunListResponse {
    pub runs: Vec<RunSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub kind: String,
    pub id: Uuid,
    pub status: String,
    pub title: String,
    pub session_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunDetailResponse {
    pub run: RunSummary,
    pub detail: serde_json::Value,
}

pub async fn list_runs(
    state: &AppState,
    query: RunListQuery,
) -> Result<RunListResponse, GatewayError> {
    let mut runs = gather_runs(state).await?;
    if query.active_only.unwrap_or(false) {
        runs.retain(|run| is_active_status(&run.status));
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.updated_at));
    if let Some(limit) = query.limit {
        runs.truncate(limit);
    }
    Ok(RunListResponse { runs })
}

pub async fn get_run_detail(
    state: &AppState,
    kind: &str,
    id: Uuid,
) -> Result<RunDetailResponse, GatewayError> {
    match kind {
        "user_turn" | "delegated_turn" => get_turn_detail(state, id).await,
        "schedule_run" => get_schedule_run_detail(state, id).await,
        other => Err(GatewayError::ValidationError(format!(
            "unknown run kind: {other}"
        ))),
    }
}

async fn gather_runs(state: &AppState) -> Result<Vec<RunSummary>, GatewayError> {
    let now = Utc::now();
    let mut topic_names = HashMap::new();
    let mut session_topics = HashMap::new();
    let mut runs = Vec::new();

    for topic in state.topic_store.list_scopes().await? {
        topic_names.insert(topic.id, topic.name.clone());
        for session in state.storage.list_by_topic(topic.id).await? {
            session_topics.insert(session.id, topic.id);

            for thread in state.storage.list_by_session(session.id).await? {
                for turn in state.storage.list_turns_by_thread(thread.id).await? {
                    let status = classify_turn_status(turn.state.clone(), turn.created_at, now);
                    runs.push(RunSummary {
                        kind: if turn.parent_turn_id.is_some() {
                            "delegated_turn".to_string()
                        } else {
                            "user_turn".to_string()
                        },
                        id: turn.id,
                        status,
                        title: turn
                            .user_input
                            .clone()
                            .or_else(|| turn.assistant_response.clone())
                            .unwrap_or_else(|| "turn".to_string()),
                        session_id: Some(session.id),
                        topic_id: Some(topic.id),
                        started_at: turn.created_at,
                        updated_at: turn.completed_at.unwrap_or(turn.created_at),
                        completed_at: turn.completed_at,
                    });
                }
            }
        }
    }

    let schedules = state.storage.list_schedules().await?;
    for schedule in &schedules {
        for run in state.storage.list_schedule_runs(schedule.id).await? {
            // Fresh-policy runs record their own session; continuation runs
            // fall back to the schedule's target session before they start.
            let session_id = run.session_id.or(match &schedule.session_policy {
                SessionPolicy::Continuation { session_id } => Some(*session_id),
                SessionPolicy::Fresh { .. } => None,
            });
            let topic_id = match &schedule.session_policy {
                SessionPolicy::Fresh { topic_id, .. } => match topic_id {
                    Some(id) => state
                        .topic_store
                        .get(*id)
                        .await?
                        .and_then(|scope| scope.client_topic_id()),
                    None => None,
                },
                SessionPolicy::Continuation { .. } => {
                    session_id.and_then(|id| session_topics.get(&id).copied())
                }
            };
            runs.push(RunSummary {
                kind: "schedule_run".to_string(),
                id: run.id,
                status: classify_schedule_run_status(run.state, run.created_at, now),
                title: schedule.message.clone(),
                session_id,
                topic_id,
                started_at: run.created_at,
                updated_at: run
                    .completed_at
                    .or(run.started_at)
                    .unwrap_or(run.created_at),
                completed_at: run.completed_at,
            });
        }
    }

    Ok(runs)
}

async fn get_turn_detail(state: &AppState, id: Uuid) -> Result<RunDetailResponse, GatewayError> {
    let turn = state
        .storage
        .get_turn(id)
        .await?
        .ok_or_else(|| not_found("turn", id))?;
    let thread = state
        .storage
        .get(turn.thread_id)
        .await?
        .ok_or_else(|| not_found("thread", turn.thread_id))?;
    let session = state
        .storage
        .get_session(thread.session_id)
        .await?
        .ok_or_else(|| not_found("session", thread.session_id))?;
    let summary = RunSummary {
        kind: if turn.parent_turn_id.is_some() {
            "delegated_turn".to_string()
        } else {
            "user_turn".to_string()
        },
        id: turn.id,
        status: classify_turn_status(turn.state.clone(), turn.created_at, Utc::now()),
        title: turn
            .user_input
            .clone()
            .or_else(|| turn.assistant_response.clone())
            .unwrap_or_else(|| "turn".to_string()),
        session_id: Some(session.id),
        topic_id: Some(session.topic_id),
        started_at: turn.created_at,
        updated_at: turn.completed_at.unwrap_or(turn.created_at),
        completed_at: turn.completed_at,
    };
    let detail = json!({
        "thread_id": turn.thread_id,
        "agent_id": turn.agent_id,
        "user_input": turn.user_input,
        "assistant_response": turn.assistant_response,
        "parent_turn_id": turn.parent_turn_id,
        "input_tokens": turn.input_tokens,
        "output_tokens": turn.output_tokens,
        "cost_microcents": turn.cost_microcents,
        "structured_output": turn.structured_output,
    });
    Ok(RunDetailResponse {
        run: summary,
        detail,
    })
}

async fn get_schedule_run_detail(
    state: &AppState,
    id: Uuid,
) -> Result<RunDetailResponse, GatewayError> {
    let run = state
        .storage
        .get_schedule_run(id)
        .await?
        .ok_or_else(|| not_found("schedule_run", id))?;
    let schedule = state
        .storage
        .get_schedule(run.schedule_id)
        .await?
        .ok_or_else(|| not_found("schedule", run.schedule_id))?;
    let session_id = run.session_id.or(match &schedule.session_policy {
        SessionPolicy::Continuation { session_id } => Some(*session_id),
        SessionPolicy::Fresh { .. } => None,
    });
    let topic_id = match &schedule.session_policy {
        SessionPolicy::Fresh { topic_id, .. } => match topic_id {
            Some(id) => state
                .topic_store
                .get(*id)
                .await?
                .and_then(|scope| scope.client_topic_id()),
            None => None,
        },
        SessionPolicy::Continuation { session_id } => Some(
            state
                .storage
                .get_session(*session_id)
                .await?
                .ok_or_else(|| not_found("session", *session_id))?
                .topic_id,
        ),
    };
    let summary = RunSummary {
        kind: "schedule_run".to_string(),
        id: run.id,
        status: classify_schedule_run_status(run.state, run.created_at, Utc::now()),
        title: schedule.message.clone(),
        session_id,
        topic_id,
        started_at: run.created_at,
        updated_at: run
            .completed_at
            .or(run.started_at)
            .unwrap_or(run.created_at),
        completed_at: run.completed_at,
    };
    let detail = json!({
        "schedule_id": run.schedule_id,
        "trigger": run.trigger.to_string(),
        "scheduled_for": run.scheduled_for,
        "session_id": run.session_id,
        "turn_id": run.turn_id,
        "skip_reason": run.skip_reason.map(|reason| reason.to_string()),
        "gate": run.gate,
        "output_text": run.output_text,
        "error_message": run.error_message,
        "expression": schedule.expression,
        "session_policy": schedule.session_policy,
    });
    Ok(RunDetailResponse {
        run: summary,
        detail,
    })
}

fn classify_turn_status(state: TurnState, created_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match state {
        TurnState::Pending | TurnState::Processing => {
            if now - created_at > STUCK_AFTER {
                "stuck".to_string()
            } else {
                "running".to_string()
            }
        }
        TurnState::AwaitingApproval => "awaiting_approval".to_string(),
        TurnState::Delegating => "delegating".to_string(),
        TurnState::Completed => "completed".to_string(),
        TurnState::Failed => "failed".to_string(),
        TurnState::Interrupted => "interrupted".to_string(),
    }
}

fn classify_schedule_run_status(
    state: ScheduleRunState,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> String {
    match state {
        ScheduleRunState::Pending | ScheduleRunState::Running => {
            if now - created_at > STUCK_AFTER {
                "stuck".to_string()
            } else {
                "running".to_string()
            }
        }
        ScheduleRunState::Completed | ScheduleRunState::CompletedEmpty => "completed".to_string(),
        ScheduleRunState::Failed => "failed".to_string(),
        ScheduleRunState::Skipped => "skipped".to_string(),
    }
}

fn is_active_status(status: &str) -> bool {
    matches!(
        status,
        "running" | "awaiting_approval" | "delegating" | "stuck"
    )
}

fn not_found(entity: &str, id: Uuid) -> GatewayError {
    GatewayError::Storage(StorageError::NotFound {
        entity: entity.to_string(),
        id: id.to_string(),
    })
}
