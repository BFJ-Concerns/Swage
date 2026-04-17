use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use swage_core::{GatewayError, StorageError};
use swage_storage::{
    HeartbeatRunState, NodeExecutionState, ScheduledRunState, TopologyRunState, TurnState,
};

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
    pub project_id: Option<Uuid>,
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
    runs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
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
        "scheduled_run" => get_scheduled_run_detail(state, id).await,
        "heartbeat_run" => get_heartbeat_run_detail(state, id).await,
        "topology_run" => get_topology_run_detail(state, id).await,
        "node_execution" => get_node_execution_detail(state, id).await,
        other => Err(GatewayError::ValidationError(format!(
            "unknown run kind: {other}"
        ))),
    }
}

async fn gather_runs(state: &AppState) -> Result<Vec<RunSummary>, GatewayError> {
    let now = Utc::now();
    let mut project_names = HashMap::new();
    let mut session_projects = HashMap::new();
    let mut runs = Vec::new();

    for project in state.project_store.list_all().await? {
        project_names.insert(project.id, project.name.clone());
        for session in state.storage.list_by_project(project.id).await? {
            session_projects.insert(session.id, project.id);

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
                            .or(turn.assistant_response.clone())
                            .unwrap_or_else(|| "turn".to_string()),
                        session_id: Some(session.id),
                        project_id: Some(project.id),
                        started_at: turn.created_at,
                        updated_at: turn.completed_at.unwrap_or(turn.created_at),
                        completed_at: turn.completed_at,
                    });
                }
            }
        }
    }

    let scheduled_jobs = state.storage.list_scheduled_jobs().await?;
    for job in &scheduled_jobs {
        for run in state.storage.list_scheduled_runs_by_job(job.id).await? {
            runs.push(RunSummary {
                kind: "scheduled_run".to_string(),
                id: run.id,
                status: classify_scheduled_run_status(run.state.clone(), run.created_at, now),
                title: job.message.clone(),
                session_id: Some(job.session_id),
                project_id: session_projects.get(&job.session_id).copied(),
                started_at: run.created_at,
                updated_at: run
                    .completed_at
                    .or(run.started_at)
                    .unwrap_or(run.created_at),
                completed_at: run.completed_at,
            });
        }
    }

    let heartbeats = state.storage.list_heartbeats().await?;
    for heartbeat in &heartbeats {
        for run in state
            .storage
            .list_heartbeat_runs_by_heartbeat(heartbeat.id)
            .await?
        {
            runs.push(RunSummary {
                kind: "heartbeat_run".to_string(),
                id: run.id,
                status: classify_heartbeat_run_status(run.state.clone(), run.created_at, now),
                title: heartbeat.message.clone(),
                session_id: Some(heartbeat.session_id),
                project_id: session_projects.get(&heartbeat.session_id).copied(),
                started_at: run.created_at,
                updated_at: run
                    .completed_at
                    .or(run.started_at)
                    .unwrap_or(run.created_at),
                completed_at: run.completed_at,
            });
        }
    }

    for session_id in session_projects.keys().copied() {
        for run in state
            .storage
            .list_topology_runs_by_session(session_id)
            .await?
        {
            runs.push(RunSummary {
                kind: "topology_run".to_string(),
                id: run.id,
                status: classify_topology_run_status(run.state.clone(), run.created_at, now),
                title: project_names
                    .get(&session_projects[&session_id])
                    .map(|name| format!("topology for {name}"))
                    .unwrap_or_else(|| "topology run".to_string()),
                session_id: Some(run.session_id),
                project_id: session_projects.get(&run.session_id).copied(),
                started_at: run.created_at,
                updated_at: run.completed_at.unwrap_or(run.created_at),
                completed_at: run.completed_at,
            });

            for node in state.storage.list_node_executions_by_run(run.id).await? {
                runs.push(RunSummary {
                    kind: "node_execution".to_string(),
                    id: node.id,
                    status: classify_node_execution_status(
                        node.state.clone(),
                        node.created_at,
                        now,
                    ),
                    title: node
                        .task_name
                        .clone()
                        .unwrap_or_else(|| node.node_id.clone()),
                    session_id: Some(run.session_id),
                    project_id: session_projects.get(&run.session_id).copied(),
                    started_at: node.created_at,
                    updated_at: node.completed_at.unwrap_or(node.created_at),
                    completed_at: node.completed_at,
                });
            }
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
            .or(turn.assistant_response.clone())
            .unwrap_or_else(|| "turn".to_string()),
        session_id: Some(session.id),
        project_id: Some(session.project_id),
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

async fn get_scheduled_run_detail(
    state: &AppState,
    id: Uuid,
) -> Result<RunDetailResponse, GatewayError> {
    let run = state
        .storage
        .get_scheduled_run(id)
        .await?
        .ok_or_else(|| not_found("scheduled_run", id))?;
    let job = state
        .storage
        .get_scheduled_job(run.job_id)
        .await?
        .ok_or_else(|| not_found("scheduled_job", run.job_id))?;
    let summary = RunSummary {
        kind: "scheduled_run".to_string(),
        id: run.id,
        status: classify_scheduled_run_status(run.state.clone(), run.created_at, Utc::now()),
        title: job.message.clone(),
        session_id: Some(job.session_id),
        project_id: Some(
            state
                .storage
                .get_session(job.session_id)
                .await?
                .ok_or_else(|| not_found("session", job.session_id))?
                .project_id,
        ),
        started_at: run.created_at,
        updated_at: run
            .completed_at
            .or(run.started_at)
            .unwrap_or(run.created_at),
        completed_at: run.completed_at,
    };
    let detail = json!({
        "job_id": run.job_id,
        "trigger": run.trigger.to_string(),
        "scheduled_for": run.scheduled_for,
        "turn_id": run.turn_id,
        "output_text": run.output_text,
        "error_message": run.error_message,
        "schedule_expression": job.schedule_expression,
    });
    Ok(RunDetailResponse {
        run: summary,
        detail,
    })
}

async fn get_heartbeat_run_detail(
    state: &AppState,
    id: Uuid,
) -> Result<RunDetailResponse, GatewayError> {
    let run = state
        .storage
        .get_heartbeat_run(id)
        .await?
        .ok_or_else(|| not_found("heartbeat_run", id))?;
    let heartbeat = state
        .storage
        .get_heartbeat(run.heartbeat_id)
        .await?
        .ok_or_else(|| not_found("heartbeat", run.heartbeat_id))?;
    let summary = RunSummary {
        kind: "heartbeat_run".to_string(),
        id: run.id,
        status: classify_heartbeat_run_status(run.state.clone(), run.created_at, Utc::now()),
        title: heartbeat.message.clone(),
        session_id: Some(heartbeat.session_id),
        project_id: Some(
            state
                .storage
                .get_session(heartbeat.session_id)
                .await?
                .ok_or_else(|| not_found("session", heartbeat.session_id))?
                .project_id,
        ),
        started_at: run.created_at,
        updated_at: run
            .completed_at
            .or(run.started_at)
            .unwrap_or(run.created_at),
        completed_at: run.completed_at,
    };
    let detail = json!({
        "heartbeat_id": run.heartbeat_id,
        "scheduled_for": run.scheduled_for,
        "turn_id": run.turn_id,
        "output_text": run.output_text,
        "error_message": run.error_message,
        "interval_seconds": heartbeat.interval_seconds,
    });
    Ok(RunDetailResponse {
        run: summary,
        detail,
    })
}

async fn get_topology_run_detail(
    state: &AppState,
    id: Uuid,
) -> Result<RunDetailResponse, GatewayError> {
    let run = state
        .storage
        .get_topology_run(id)
        .await?
        .ok_or_else(|| not_found("topology_run", id))?;
    let node_count = state
        .storage
        .list_node_executions_by_run(run.id)
        .await?
        .len();
    let summary = RunSummary {
        kind: "topology_run".to_string(),
        id: run.id,
        status: classify_topology_run_status(run.state.clone(), run.created_at, Utc::now()),
        title: "topology run".to_string(),
        session_id: Some(run.session_id),
        project_id: Some(
            state
                .storage
                .get_session(run.session_id)
                .await?
                .ok_or_else(|| not_found("session", run.session_id))?
                .project_id,
        ),
        started_at: run.created_at,
        updated_at: run.completed_at.unwrap_or(run.created_at),
        completed_at: run.completed_at,
    };
    let detail = json!({
        "team_id": run.team_id,
        "session_id": run.session_id,
        "source_turn_id": run.source_turn_id,
        "final_output": run.final_output,
        "error_message": run.error_message,
        "node_count": node_count,
    });
    Ok(RunDetailResponse {
        run: summary,
        detail,
    })
}

async fn get_node_execution_detail(
    state: &AppState,
    id: Uuid,
) -> Result<RunDetailResponse, GatewayError> {
    let node = state
        .storage
        .get_node_execution(id)
        .await?
        .ok_or_else(|| not_found("node_execution", id))?;
    let run = state
        .storage
        .get_topology_run(node.topology_run_id)
        .await?
        .ok_or_else(|| not_found("topology_run", node.topology_run_id))?;
    let retries = state.storage.list_node_retries(node.id).await?;
    let summary = RunSummary {
        kind: "node_execution".to_string(),
        id: node.id,
        status: classify_node_execution_status(node.state.clone(), node.created_at, Utc::now()),
        title: node
            .task_name
            .clone()
            .unwrap_or_else(|| node.node_id.clone()),
        session_id: Some(run.session_id),
        project_id: Some(
            state
                .storage
                .get_session(run.session_id)
                .await?
                .ok_or_else(|| not_found("session", run.session_id))?
                .project_id,
        ),
        started_at: node.created_at,
        updated_at: node.completed_at.unwrap_or(node.created_at),
        completed_at: node.completed_at,
    };
    let detail = json!({
        "topology_run_id": node.topology_run_id,
        "node_id": node.node_id,
        "node_kind": node.node_kind.to_string(),
        "agent_id": node.agent_id,
        "task_name": node.task_name,
        "input_json": node.input_json,
        "output_json": node.output_json,
        "error_message": node.error_message,
        "retry_count": retries.len(),
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

fn classify_scheduled_run_status(
    state: ScheduledRunState,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> String {
    match state {
        ScheduledRunState::Pending | ScheduledRunState::Running => {
            if now - created_at > STUCK_AFTER {
                "stuck".to_string()
            } else {
                "running".to_string()
            }
        }
        ScheduledRunState::Completed => "completed".to_string(),
        ScheduledRunState::Failed => "failed".to_string(),
    }
}

fn classify_heartbeat_run_status(
    state: HeartbeatRunState,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> String {
    match state {
        HeartbeatRunState::Pending | HeartbeatRunState::Running => {
            if now - created_at > STUCK_AFTER {
                "stuck".to_string()
            } else {
                "running".to_string()
            }
        }
        HeartbeatRunState::Completed => "completed".to_string(),
        HeartbeatRunState::Failed => "failed".to_string(),
    }
}

fn classify_topology_run_status(
    state: TopologyRunState,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> String {
    match state {
        TopologyRunState::Pending | TopologyRunState::Running => {
            if now - created_at > STUCK_AFTER {
                "stuck".to_string()
            } else {
                "running".to_string()
            }
        }
        TopologyRunState::Completed => "completed".to_string(),
        TopologyRunState::Failed => "failed".to_string(),
        TopologyRunState::Interrupted => "interrupted".to_string(),
    }
}

fn classify_node_execution_status(
    state: NodeExecutionState,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> String {
    match state {
        NodeExecutionState::Pending | NodeExecutionState::Running => {
            if now - created_at > STUCK_AFTER {
                "stuck".to_string()
            } else {
                "running".to_string()
            }
        }
        NodeExecutionState::Completed | NodeExecutionState::Skipped => "completed".to_string(),
        NodeExecutionState::Failed => "failed".to_string(),
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
