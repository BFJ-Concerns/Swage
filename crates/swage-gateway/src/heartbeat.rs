use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    Heartbeat, HeartbeatRun, HeartbeatRunState, HeartbeatStatus, NewHeartbeat, NewHeartbeatRun,
    UpdateHeartbeat,
};

use crate::state::AppState;

const HEARTBEAT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Deserialize)]
pub struct CreateHeartbeatRequest {
    pub session_id: Uuid,
    pub message: String,
    pub interval_seconds: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateHeartbeatRequest {
    pub message: Option<String>,
    pub interval_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatResponse {
    pub id: Uuid,
    pub session_id: Uuid,
    pub message: String,
    pub interval_seconds: i64,
    pub status: String,
    pub last_beat_at: Option<DateTime<Utc>>,
    pub next_beat_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatRunResponse {
    pub id: Uuid,
    pub heartbeat_id: Uuid,
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
pub struct HeartbeatListResponse {
    pub heartbeats: Vec<HeartbeatResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatRunListResponse {
    pub runs: Vec<HeartbeatRunResponse>,
}

fn validate_interval_seconds(interval_seconds: i64) -> Result<(), GatewayError> {
    if interval_seconds <= 0 {
        return Err(GatewayError::ValidationError(
            "heartbeat interval_seconds must be positive".into(),
        ));
    }
    Ok(())
}

fn next_beat_after(now: DateTime<Utc>, interval_seconds: i64) -> DateTime<Utc> {
    now + Duration::seconds(interval_seconds)
}

pub async fn create_heartbeat(
    state: &AppState,
    request: CreateHeartbeatRequest,
) -> Result<HeartbeatResponse, GatewayError> {
    validate_interval_seconds(request.interval_seconds)?;
    let session = state.storage.get_session(request.session_id).await?.ok_or(
        GatewayError::ConversationNotFound {
            id: request.session_id,
        },
    )?;
    let now = Utc::now();
    let heartbeat = state
        .storage
        .create_heartbeat(NewHeartbeat {
            session_id: request.session_id,
            agent_id: session.agent_id,
            message: request.message,
            interval_seconds: request.interval_seconds,
            status: HeartbeatStatus::Enabled,
            last_beat_at: None,
            next_beat_at: Some(next_beat_after(now, request.interval_seconds)),
        })
        .await?;
    Ok(map_heartbeat_response(heartbeat))
}

pub async fn list_heartbeats(state: &AppState) -> Result<HeartbeatListResponse, GatewayError> {
    let heartbeats = state.storage.list_heartbeats().await?;
    Ok(HeartbeatListResponse {
        heartbeats: heartbeats.into_iter().map(map_heartbeat_response).collect(),
    })
}

pub async fn update_heartbeat(
    state: &AppState,
    heartbeat_id: Uuid,
    request: UpdateHeartbeatRequest,
) -> Result<HeartbeatResponse, GatewayError> {
    if let Some(interval_seconds) = request.interval_seconds {
        validate_interval_seconds(interval_seconds)?;
    }
    let existing = state
        .storage
        .get_heartbeat(heartbeat_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("heartbeat {heartbeat_id} not found"))
        })?;
    let interval_seconds = request
        .interval_seconds
        .unwrap_or(existing.interval_seconds);
    let now = Utc::now();
    let next_beat_at = if existing.status == HeartbeatStatus::Enabled {
        Some(Some(next_beat_after(now, interval_seconds)))
    } else {
        None
    };
    let updated = state
        .storage
        .update_heartbeat(
            heartbeat_id,
            UpdateHeartbeat {
                message: request.message,
                interval_seconds: Some(interval_seconds),
                next_beat_at,
                ..Default::default()
            },
        )
        .await?;
    Ok(map_heartbeat_response(updated))
}

pub async fn enable_heartbeat(
    state: &AppState,
    heartbeat_id: Uuid,
) -> Result<HeartbeatResponse, GatewayError> {
    let existing = state
        .storage
        .get_heartbeat(heartbeat_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("heartbeat {heartbeat_id} not found"))
        })?;
    let now = Utc::now();
    let updated = state
        .storage
        .update_heartbeat(
            heartbeat_id,
            UpdateHeartbeat {
                status: Some(HeartbeatStatus::Enabled),
                next_beat_at: Some(Some(next_beat_after(now, existing.interval_seconds))),
                ..Default::default()
            },
        )
        .await?;
    Ok(map_heartbeat_response(updated))
}

pub async fn disable_heartbeat(
    state: &AppState,
    heartbeat_id: Uuid,
) -> Result<HeartbeatResponse, GatewayError> {
    let updated = state
        .storage
        .update_heartbeat(
            heartbeat_id,
            UpdateHeartbeat {
                status: Some(HeartbeatStatus::Disabled),
                next_beat_at: Some(None),
                ..Default::default()
            },
        )
        .await?;
    Ok(map_heartbeat_response(updated))
}

pub async fn delete_heartbeat(state: &AppState, heartbeat_id: Uuid) -> Result<bool, GatewayError> {
    state
        .storage
        .delete_heartbeat(heartbeat_id)
        .await
        .map_err(Into::into)
}

pub async fn list_heartbeat_runs(
    state: &AppState,
    heartbeat_id: Uuid,
) -> Result<HeartbeatRunListResponse, GatewayError> {
    let runs = state
        .storage
        .list_heartbeat_runs_by_heartbeat(heartbeat_id)
        .await?;
    Ok(HeartbeatRunListResponse {
        runs: runs.into_iter().map(map_heartbeat_run_response).collect(),
    })
}

pub async fn tick_once(state: &AppState, now: DateTime<Utc>) -> Result<usize, GatewayError> {
    let heartbeats = state.storage.list_due_heartbeats(now).await?;
    let mut executed = 0;
    for heartbeat in heartbeats {
        state
            .storage
            .update_heartbeat(
                heartbeat.id,
                UpdateHeartbeat {
                    last_beat_at: Some(Some(now)),
                    next_beat_at: Some(Some(next_beat_after(now, heartbeat.interval_seconds))),
                    ..Default::default()
                },
            )
            .await?;
        let _ = execute_heartbeat(state, &heartbeat, Some(now)).await?;
        executed += 1;
    }
    Ok(executed)
}

pub fn spawn_heartbeat_loop(state: AppState) -> JoinHandle<()> {
    spawn_heartbeat_loop_until_cancelled(state, CancellationToken::new())
}

pub fn spawn_heartbeat_loop_until_cancelled(
    state: AppState,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    crate::background_loop::spawn_periodic_loop(
        HEARTBEAT_POLL_INTERVAL,
        shutdown,
        "heartbeat tick failed",
        move || {
            let state = state.clone();
            async move {
                tick_once(&state, Utc::now()).await?;
                Ok(())
            }
        },
    )
}

async fn execute_heartbeat(
    state: &AppState,
    heartbeat: &Heartbeat,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<HeartbeatRun, GatewayError> {
    let run = state
        .storage
        .create_heartbeat_run(NewHeartbeatRun {
            heartbeat_id: heartbeat.id,
            state: HeartbeatRunState::Pending,
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
        heartbeat.session_id,
        &heartbeat.agent_id,
        &heartbeat.message,
    )
    .await?;
    state
        .storage
        .update_heartbeat_run_state(
            run.id,
            HeartbeatRunState::Running,
            Some(turn_id),
            None,
            None,
        )
        .await?;

    match crate::automated_turn::run_placeholder_turn(
        state,
        heartbeat.session_id,
        &heartbeat.message,
        turn_id,
    )
    .await
    {
        Ok(output_text) => {
            state
                .storage
                .update_heartbeat_run_state(
                    run.id,
                    HeartbeatRunState::Completed,
                    Some(turn_id),
                    Some(&output_text),
                    None,
                )
                .await?;
        }
        Err(error) => {
            state
                .storage
                .update_heartbeat_run_state(
                    run.id,
                    HeartbeatRunState::Failed,
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
        .get_heartbeat_run(run.id)
        .await?
        .ok_or_else(|| GatewayError::ValidationError("heartbeat run disappeared".into()))
}

fn map_heartbeat_response(heartbeat: Heartbeat) -> HeartbeatResponse {
    HeartbeatResponse {
        id: heartbeat.id,
        session_id: heartbeat.session_id,
        message: heartbeat.message,
        interval_seconds: heartbeat.interval_seconds,
        status: heartbeat.status.to_string(),
        last_beat_at: heartbeat.last_beat_at,
        next_beat_at: heartbeat.next_beat_at,
    }
}

fn map_heartbeat_run_response(run: HeartbeatRun) -> HeartbeatRunResponse {
    HeartbeatRunResponse {
        id: run.id,
        heartbeat_id: run.heartbeat_id,
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
