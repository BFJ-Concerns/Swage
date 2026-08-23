use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_protocol::{
    client::context::AssemblyReport,
    hosting::{
        AssemblyHistoryEntry, ConfigurationHistoryEvent, OperatorHistoryResponse,
        VariantSelectionHistoryEvent,
    },
};
use swage_storage::AgentMemoryRecord;

use crate::state::AppState;

const OPERATOR_HISTORY_SNAPSHOTS_PER_SPACE: usize = 100;

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
    pub space_id: Option<Uuid>,
    pub title: String,
    pub excerpt: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OperatorUsageQuery {
    pub session_id: Option<Uuid>,
    pub schedule_id: Option<Uuid>,
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
    pub schedule: Option<UsageAggregate>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentMemoryListQuery {
    pub space_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub agent_id: Option<String>,
    pub q: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentMemoryListResponse {
    pub memories: Vec<AgentMemoryRecord>,
}

pub async fn operator_history(state: &AppState) -> Result<OperatorHistoryResponse, GatewayError> {
    let events = state.storage.list_audit_events().await?;
    let mut configuration_changes = Vec::new();
    let mut variant_selections = Vec::new();

    for event in events {
        match event.event_kind.as_str() {
            "configuration_change" => {
                configuration_changes.push(ConfigurationHistoryEvent {
                    id: event.id,
                    resource_kind: event.entity_kind,
                    resource_id: event.entity_id,
                    action: metadata_string(&event.metadata, "action")?,
                    source: metadata_string(&event.metadata, "source")?,
                    session_id: event.session_id,
                    prior_state: metadata_value(&event.metadata, "prior_state")?,
                    new_state: metadata_value(&event.metadata, "new_state")?,
                    recorded_at: event.created_at,
                });
            }
            "variant_selected" => {
                let turn_id = event
                    .metadata
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .map(Uuid::parse_str)
                    .transpose()
                    .map_err(|error| {
                        GatewayError::ConfigError(format!(
                            "audit event {} has an invalid turn_id: {error}",
                            event.id
                        ))
                    })?;
                variant_selections.push(VariantSelectionHistoryEvent {
                    id: event.id,
                    location_id: event.entity_id,
                    variant: metadata_string(&event.metadata, "variant")?,
                    reason: metadata_string(&event.metadata, "reason")?,
                    turn_id,
                    session_id: event.session_id,
                    recorded_at: event.created_at,
                });
            }
            _ => {}
        }
    }

    configuration_changes.sort_by(|left, right| right.recorded_at.cmp(&left.recorded_at));
    variant_selections.sort_by(|left, right| right.recorded_at.cmp(&left.recorded_at));

    let mut assembly_reports = Vec::new();
    for space in state.space_store.list_all().await? {
        for snapshot in state
            .storage
            .list_snapshots_by_space(space.id, OPERATOR_HISTORY_SNAPSHOTS_PER_SPACE)
            .await?
        {
            let report: AssemblyReport =
                serde_json::from_str(&snapshot.report_json).map_err(|error| {
                    GatewayError::ConfigError(format!(
                        "context snapshot for turn {} is invalid: {error}",
                        snapshot.turn_id
                    ))
                })?;
            assembly_reports.push(AssemblyHistoryEntry {
                turn_id: snapshot.turn_id,
                session_id: snapshot.session_id,
                space_id: snapshot.space_id,
                recorded_at: snapshot.created_at,
                assembly_name: report.assembly_name,
            });
        }
    }
    assembly_reports.sort_by(|left, right| right.recorded_at.cmp(&left.recorded_at));

    Ok(OperatorHistoryResponse {
        configuration_changes,
        variant_selections,
        assembly_reports,
    })
}

fn metadata_string(metadata: &serde_json::Value, field: &str) -> Result<String, GatewayError> {
    metadata
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| GatewayError::ConfigError(format!("audit event is missing {field}")))
}

fn metadata_value(
    metadata: &serde_json::Value,
    field: &str,
) -> Result<serde_json::Value, GatewayError> {
    metadata
        .get(field)
        .cloned()
        .ok_or_else(|| GatewayError::ConfigError(format!("audit event is missing {field}")))
}

pub async fn operator_search(
    state: &AppState,
    query: OperatorSearchQuery,
) -> Result<OperatorSearchResponse, GatewayError> {
    let search_text = query.q.as_ref().map(|value| value.to_lowercase());
    let kind_filter = query.kind.as_deref();
    let session_filter = query.session_id;
    let limit = query.limit.unwrap_or(50);

    let spaces = state.space_store.list_all().await?;
    let space_names: HashMap<Uuid, String> = spaces
        .iter()
        .map(|space| (space.id, space.name.clone()))
        .collect();
    let mut sessions = Vec::new();
    for space in &spaces {
        sessions.extend(state.storage.list_by_space(space.id).await?);
    }
    let session_spaces: HashMap<Uuid, Uuid> = sessions
        .iter()
        .map(|session| (session.id, session.space_id))
        .collect();

    let mut results = Vec::new();

    if matches_kind(kind_filter, "session") {
        for session in &sessions {
            if !matches_session(session_filter, session.id) {
                continue;
            }
            let title = space_names
                .get(&session.space_id)
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
                    space_id: Some(session.space_id),
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
                            space_id: Some(session.space_id),
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
            let (session_id, space_id) =
                session_and_space_for_turn(state, approval.turn_id).await?;
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
                    space_id,
                    title,
                    excerpt,
                    occurred_at: approval.decided_at.unwrap_or(approval.created_at),
                });
            }
        }
    }

    if matches_kind(kind_filter, "schedule_run") {
        for schedule in state.storage.list_schedules().await? {
            for run in state.storage.list_schedule_runs(schedule.id).await? {
                // Fresh-policy runs record their own session; continuation
                // runs fall back to the schedule's target session.
                let run_session = run.session_id.or(match &schedule.session_policy {
                    swage_storage::SessionPolicy::Continuation { session_id } => Some(*session_id),
                    swage_storage::SessionPolicy::Fresh { .. } => None,
                });
                if let Some(filter) = session_filter
                    && run_session != Some(filter)
                {
                    continue;
                }
                let excerpt = run
                    .error_message
                    .clone()
                    .or_else(|| run.output_text.clone())
                    .or_else(|| Some(run.state.to_string()));
                if matches_query(
                    &search_text,
                    &[
                        &run.id.to_string(),
                        &run.state.to_string(),
                        &schedule.expression,
                    ],
                    Some(&schedule.message),
                ) || matches_query(&search_text, &[], excerpt.as_deref())
                {
                    results.push(OperatorSearchResult {
                        kind: "schedule_run".into(),
                        id: run.id.to_string(),
                        session_id: run_session,
                        space_id: run_session
                            .and_then(|session_id| session_spaces.get(&session_id).copied()),
                        title: schedule.message.clone(),
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
                    space_id: session_spaces.get(&delivery.session_id).copied(),
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
                    space_id: event
                        .session_id
                        .and_then(|id| session_spaces.get(&id).copied()),
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
                            session.space_id,
                            None,
                            Some(session.id),
                            search_query,
                            limit,
                        )
                        .await?,
                );
            }
        } else {
            for space in &spaces {
                memories.extend(
                    state
                        .storage
                        .list_agent_memories(space.id, None, None, search_query, limit)
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
                    space_id: Some(memory.space_id),
                    title: memory.title,
                    excerpt: Some(memory.source_summary),
                    occurred_at: memory.updated_at,
                });
            }
        }
    }

    results.sort_by_key(|result| std::cmp::Reverse(result.occurred_at));
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

    if let Some(space_id) = query.space_id {
        memories.extend(
            state
                .storage
                .list_agent_memories(
                    space_id,
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
                        session.space_id,
                        query.agent_id.as_deref(),
                        Some(session_id),
                        search,
                        limit,
                    )
                    .await?,
            );
        }
    } else {
        for space in state.space_store.list_all().await? {
            memories.extend(
                state
                    .storage
                    .list_agent_memories(space.id, query.agent_id.as_deref(), None, search, limit)
                    .await?,
            );
        }
        memories.sort_by_key(|memory| std::cmp::Reverse(memory.updated_at));
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
                kind: query
                    .run_kind
                    .expect("run_kind was matched as present above"),
                id: run_id.to_string(),
                session_id: usage.0,
                input_tokens: usage.1,
                output_tokens: usage.2,
                cost_microcents: usage.3,
            })
        }
        (Some("schedule_run"), Some(run_id)) => {
            if let Some(run) = state.storage.get_schedule_run(run_id).await? {
                match run.turn_id {
                    Some(turn_id) => turn_usage(state, turn_id).await?.map(|usage| RunUsage {
                        kind: "schedule_run".into(),
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

    let schedule = match query.schedule_id {
        Some(schedule_id) => Some(schedule_usage(state, schedule_id).await?),
        None => None,
    };

    Ok(OperatorUsageResponse {
        run,
        session,
        schedule,
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

async fn schedule_usage(
    state: &AppState,
    schedule_id: Uuid,
) -> Result<UsageAggregate, GatewayError> {
    let mut aggregate = UsageAggregate {
        scope: "schedule".into(),
        id: schedule_id.to_string(),
        run_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_microcents: 0,
    };

    for run in state.storage.list_schedule_runs(schedule_id).await? {
        if let Some(turn_id) = run.turn_id
            && let Some((_session_id, input_tokens, output_tokens, cost_microcents)) =
                turn_usage(state, turn_id).await?
        {
            aggregate.run_count += 1;
            aggregate.input_tokens += input_tokens;
            aggregate.output_tokens += output_tokens;
            aggregate.cost_microcents += cost_microcents;
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

async fn session_and_space_for_turn(
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
    let space_id = state
        .storage
        .get_session(thread.session_id)
        .await?
        .map(|session| session.space_id);
    Ok((session_id, space_id))
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
