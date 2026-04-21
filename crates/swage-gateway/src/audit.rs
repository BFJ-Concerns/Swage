use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::AgentMemoryRecord;

use crate::state::AppState;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OperatorSearchQuery {
    pub q: Option<String>,
    pub kind: Option<String>,
    pub session_id: Option<Uuid>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorSearchResponse {
    pub results: Vec<OperatorSearchResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorSearchResult {
    pub kind: String,
    pub id: String,
    pub session_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub title: String,
    pub excerpt: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OperatorUsageQuery {
    pub session_id: Option<Uuid>,
    pub scheduled_job_id: Option<Uuid>,
    pub run_kind: Option<String>,
    pub run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageAggregate {
    pub scope: String,
    pub id: String,
    pub run_count: usize,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_microcents: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunUsage {
    pub kind: String,
    pub id: String,
    pub session_id: Option<Uuid>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_microcents: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorUsageResponse {
    pub run: Option<RunUsage>,
    pub session: Option<UsageAggregate>,
    pub scheduled_job: Option<UsageAggregate>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentMemoryListQuery {
    pub project_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub agent_id: Option<String>,
    pub q: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentMemoryListResponse {
    pub memories: Vec<AgentMemoryRecord>,
}

pub async fn operator_search(
    state: &AppState,
    query: OperatorSearchQuery,
) -> Result<OperatorSearchResponse, GatewayError> {
    let search_text = query.q.as_ref().map(|value| value.to_lowercase());
    let kind_filter = query.kind.as_deref();
    let session_filter = query.session_id;
    let limit = query.limit.unwrap_or(50);

    let projects = state.project_store.list_all().await?;
    let project_names: HashMap<Uuid, String> = projects
        .iter()
        .map(|project| (project.id, project.name.clone()))
        .collect();
    let mut sessions = Vec::new();
    for project in &projects {
        sessions.extend(state.storage.list_by_project(project.id).await?);
    }
    let session_projects: HashMap<Uuid, Uuid> = sessions
        .iter()
        .map(|session| (session.id, session.project_id))
        .collect();

    let mut results = Vec::new();

    if matches_kind(kind_filter, "session") {
        for session in &sessions {
            if !matches_session(session_filter, session.id) {
                continue;
            }
            let title = project_names
                .get(&session.project_id)
                .map(|name| format!("session in {name}"))
                .unwrap_or_else(|| "session".to_string());
            let excerpt = session.metadata.as_ref().map(|value| value.to_string());
            if matches_query(
                &search_text,
                &[&session.id.to_string(), &session.agent_id, &title],
                excerpt.as_deref(),
            ) {
                results.push(OperatorSearchResult {
                    kind: "session".into(),
                    id: session.id.to_string(),
                    session_id: Some(session.id),
                    project_id: Some(session.project_id),
                    title,
                    excerpt,
                    occurred_at: session.updated_at,
                });
            }
        }
    }

    if matches_kind(kind_filter, "message") {
        for session in &sessions {
            if !matches_session(session_filter, session.id) {
                continue;
            }
            for thread in state.storage.list_by_session(session.id).await? {
                for message in state.storage.list_messages_by_thread(thread.id).await? {
                    if matches_query(
                        &search_text,
                        &[
                            &message.id.to_string(),
                            &message.role.to_string(),
                            &message.agent_id,
                        ],
                        Some(&message.content),
                    ) {
                        results.push(OperatorSearchResult {
                            kind: "message".into(),
                            id: message.id.to_string(),
                            session_id: Some(session.id),
                            project_id: Some(session.project_id),
                            title: format!("{} message", message.role),
                            excerpt: Some(message.content),
                            occurred_at: message.created_at,
                        });
                    }
                }
            }
        }
    }

    if matches_kind(kind_filter, "approval") {
        for approval in state.storage.list_approvals().await? {
            let (session_id, project_id) =
                session_and_project_for_turn(state, approval.turn_id).await?;
            if !matches_optional_session(session_filter, session_id) {
                continue;
            }
            let title = approval.prompt.clone();
            let excerpt = Some(format!(
                "{} {}",
                approval.state,
                approval
                    .tool_name
                    .unwrap_or_else(|| approval.kind.to_string())
            ));
            if matches_query(
                &search_text,
                &[
                    &approval.id.to_string(),
                    &approval.agent_id,
                    &approval.state.to_string(),
                ],
                Some(&title),
            ) || matches_query(&search_text, &[], excerpt.as_deref())
            {
                results.push(OperatorSearchResult {
                    kind: "approval".into(),
                    id: approval.id.to_string(),
                    session_id,
                    project_id,
                    title,
                    excerpt,
                    occurred_at: approval.decided_at.unwrap_or(approval.created_at),
                });
            }
        }
    }

    if matches_kind(kind_filter, "scheduled_run") {
        for job in state.storage.list_scheduled_jobs().await? {
            if !matches_session(session_filter, job.session_id) {
                continue;
            }
            for run in state.storage.list_scheduled_runs_by_job(job.id).await? {
                let excerpt = run
                    .error_message
                    .clone()
                    .or(run.output_text.clone())
                    .or_else(|| Some(run.state.to_string()));
                if matches_query(
                    &search_text,
                    &[
                        &run.id.to_string(),
                        &run.state.to_string(),
                        &job.schedule_expression,
                    ],
                    Some(&job.message),
                ) || matches_query(&search_text, &[], excerpt.as_deref())
                {
                    results.push(OperatorSearchResult {
                        kind: "scheduled_run".into(),
                        id: run.id.to_string(),
                        session_id: Some(job.session_id),
                        project_id: session_projects.get(&job.session_id).copied(),
                        title: job.message.clone(),
                        excerpt,
                        occurred_at: run
                            .completed_at
                            .or(run.started_at)
                            .unwrap_or(run.created_at),
                    });
                }
            }
        }
    }

    if matches_kind(kind_filter, "delivery") {
        for delivery in state.storage.list_delivery_records().await? {
            if !matches_session(session_filter, delivery.session_id) {
                continue;
            }
            let excerpt = Some(delivery.content.clone());
            let target = delivery.target.to_string();
            if matches_query(
                &search_text,
                &[&delivery.id.to_string(), &target],
                excerpt.as_deref(),
            ) {
                results.push(OperatorSearchResult {
                    kind: "delivery".into(),
                    id: delivery.id.to_string(),
                    session_id: Some(delivery.session_id),
                    project_id: session_projects.get(&delivery.session_id).copied(),
                    title: "delivery record".into(),
                    excerpt,
                    occurred_at: delivery.delivered_at,
                });
            }
        }
    }

    if matches_kind(kind_filter, "audit_event") {
        let events = match session_filter {
            Some(session_id) => {
                state
                    .storage
                    .list_audit_events_by_session(session_id)
                    .await?
            }
            None => state.storage.list_audit_events().await?,
        };
        for event in events {
            if matches_query(
                &search_text,
                &[&event.id, &event.entity_kind, &event.event_kind],
                Some(&format!("{} {}", event.summary, event.metadata)),
            ) {
                results.push(OperatorSearchResult {
                    kind: "audit_event".into(),
                    id: event.id,
                    session_id: event.session_id,
                    project_id: event
                        .session_id
                        .and_then(|id| session_projects.get(&id).copied()),
                    title: event.summary,
                    excerpt: Some(event.metadata.to_string()),
                    occurred_at: event.created_at,
                });
            }
        }
    }

    if matches_kind(kind_filter, "agent_memory") {
        let search_query = search_text.as_deref();
        let mut memories = Vec::new();
        if let Some(session_id) = session_filter {
            if let Some(session) = state.storage.get_session(session_id).await? {
                memories.extend(
                    state
                        .storage
                        .list_agent_memories(
                            session.project_id,
                            None,
                            Some(session.id),
                            search_query,
                            limit,
                        )
                        .await?,
                );
            }
        } else {
            for project in &projects {
                memories.extend(
                    state
                        .storage
                        .list_agent_memories(project.id, None, None, search_query, limit)
                        .await?,
                );
            }
        }

        for memory in memories {
            if matches_query(
                &search_text,
                &[
                    &memory.id.to_string(),
                    &memory.agent_id,
                    &memory.memory_kind,
                ],
                Some(&format!("{} {}", memory.title, memory.content)),
            ) || matches_query(&search_text, &[], Some(&memory.source_summary))
            {
                results.push(OperatorSearchResult {
                    kind: "agent_memory".into(),
                    id: memory.id.to_string(),
                    session_id: Some(memory.session_id),
                    project_id: Some(memory.project_id),
                    title: memory.title,
                    excerpt: Some(memory.source_summary),
                    occurred_at: memory.updated_at,
                });
            }
        }
    }

    results.sort_by(|left, right| right.occurred_at.cmp(&left.occurred_at));
    results.truncate(limit);
    Ok(OperatorSearchResponse { results })
}

pub async fn list_agent_memories(
    state: &AppState,
    query: AgentMemoryListQuery,
) -> Result<AgentMemoryListResponse, GatewayError> {
    let limit = query.limit.unwrap_or(50).min(200);
    let search = query.q.as_deref();
    let mut memories = Vec::new();

    if let Some(project_id) = query.project_id {
        memories.extend(
            state
                .storage
                .list_agent_memories(
                    project_id,
                    query.agent_id.as_deref(),
                    query.session_id,
                    search,
                    limit,
                )
                .await?,
        );
    } else if let Some(session_id) = query.session_id {
        if let Some(session) = state.storage.get_session(session_id).await? {
            memories.extend(
                state
                    .storage
                    .list_agent_memories(
                        session.project_id,
                        query.agent_id.as_deref(),
                        Some(session_id),
                        search,
                        limit,
                    )
                    .await?,
            );
        }
    } else {
        for project in state.project_store.list_all().await? {
            memories.extend(
                state
                    .storage
                    .list_agent_memories(project.id, query.agent_id.as_deref(), None, search, limit)
                    .await?,
            );
        }
        memories.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        memories.truncate(limit);
    }

    Ok(AgentMemoryListResponse { memories })
}

pub async fn get_agent_memory(
    state: &AppState,
    id: Uuid,
) -> Result<Option<AgentMemoryRecord>, GatewayError> {
    Ok(state.storage.get_agent_memory(id).await?)
}

pub async fn operator_usage(
    state: &AppState,
    query: OperatorUsageQuery,
) -> Result<OperatorUsageResponse, GatewayError> {
    let run = match (query.run_kind.as_deref(), query.run_id) {
        (Some("user_turn" | "delegated_turn"), Some(run_id)) => {
            turn_usage(state, run_id).await?.map(|usage| RunUsage {
                kind: query.run_kind.unwrap(),
                id: run_id.to_string(),
                session_id: usage.0,
                input_tokens: usage.1,
                output_tokens: usage.2,
                cost_microcents: usage.3,
            })
        }
        (Some("scheduled_run"), Some(run_id)) => {
            if let Some(run) = state.storage.get_scheduled_run(run_id).await? {
                match run.turn_id {
                    Some(turn_id) => turn_usage(state, turn_id).await?.map(|usage| RunUsage {
                        kind: "scheduled_run".into(),
                        id: run_id.to_string(),
                        session_id: usage.0,
                        input_tokens: usage.1,
                        output_tokens: usage.2,
                        cost_microcents: usage.3,
                    }),
                    None => None,
                }
            } else {
                None
            }
        }
        _ => None,
    };

    let session = match query.session_id {
        Some(session_id) => Some(session_usage(state, session_id).await?),
        None => None,
    };

    let scheduled_job = match query.scheduled_job_id {
        Some(job_id) => Some(scheduled_job_usage(state, job_id).await?),
        None => None,
    };

    Ok(OperatorUsageResponse {
        run,
        session,
        scheduled_job,
    })
}

async fn session_usage(state: &AppState, session_id: Uuid) -> Result<UsageAggregate, GatewayError> {
    let mut aggregate = UsageAggregate {
        scope: "session".into(),
        id: session_id.to_string(),
        run_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_microcents: 0,
    };

    for thread in state.storage.list_by_session(session_id).await? {
        for turn in state.storage.list_turns_by_thread(thread.id).await? {
            if turn.input_tokens.is_some()
                || turn.output_tokens.is_some()
                || turn.cost_microcents.is_some()
            {
                aggregate.run_count += 1;
            }
            aggregate.input_tokens += turn.input_tokens.unwrap_or(0);
            aggregate.output_tokens += turn.output_tokens.unwrap_or(0);
            aggregate.cost_microcents += turn.cost_microcents.unwrap_or(0);
        }
    }

    Ok(aggregate)
}

async fn scheduled_job_usage(
    state: &AppState,
    job_id: Uuid,
) -> Result<UsageAggregate, GatewayError> {
    let mut aggregate = UsageAggregate {
        scope: "scheduled_job".into(),
        id: job_id.to_string(),
        run_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_microcents: 0,
    };

    for run in state.storage.list_scheduled_runs_by_job(job_id).await? {
        if let Some(turn_id) = run.turn_id {
            if let Some((_session_id, input_tokens, output_tokens, cost_microcents)) =
                turn_usage(state, turn_id).await?
            {
                aggregate.run_count += 1;
                aggregate.input_tokens += input_tokens;
                aggregate.output_tokens += output_tokens;
                aggregate.cost_microcents += cost_microcents;
            }
        }
    }

    Ok(aggregate)
}

async fn turn_usage(
    state: &AppState,
    turn_id: Uuid,
) -> Result<Option<(Option<Uuid>, i64, i64, i64)>, GatewayError> {
    let Some(turn) = state.storage.get_turn(turn_id).await? else {
        return Ok(None);
    };
    let session_id = state
        .storage
        .get(turn.thread_id)
        .await?
        .map(|thread| thread.session_id);
    Ok(Some((
        session_id,
        turn.input_tokens.unwrap_or(0),
        turn.output_tokens.unwrap_or(0),
        turn.cost_microcents.unwrap_or(0),
    )))
}

async fn session_and_project_for_turn(
    state: &AppState,
    turn_id: Uuid,
) -> Result<(Option<Uuid>, Option<Uuid>), GatewayError> {
    let Some(turn) = state.storage.get_turn(turn_id).await? else {
        return Ok((None, None));
    };
    let Some(thread) = state.storage.get(turn.thread_id).await? else {
        return Ok((None, None));
    };
    let session_id = Some(thread.session_id);
    let project_id = state
        .storage
        .get_session(thread.session_id)
        .await?
        .map(|session| session.project_id);
    Ok((session_id, project_id))
}

fn matches_kind(filter: Option<&str>, kind: &str) -> bool {
    match filter {
        None => true,
        Some(value) => value == kind,
    }
}

fn matches_session(filter: Option<Uuid>, session_id: Uuid) -> bool {
    filter.is_none() || filter == Some(session_id)
}

fn matches_optional_session(filter: Option<Uuid>, session_id: Option<Uuid>) -> bool {
    match filter {
        None => true,
        Some(value) => session_id == Some(value),
    }
}

fn matches_query(search_text: &Option<String>, fields: &[&str], excerpt: Option<&str>) -> bool {
    match search_text {
        None => true,
        Some(query) => {
            fields
                .iter()
                .any(|field| field.to_lowercase().contains(query))
                || excerpt
                    .map(|value| value.to_lowercase().contains(query))
                    .unwrap_or(false)
        }
    }
}
