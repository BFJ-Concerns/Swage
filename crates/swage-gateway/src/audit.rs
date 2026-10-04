use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_protocol::{
    client::context::AssemblyReport,
    hosting::{
        AssemblyHistoryEntry, AuditEventListResponse, AuditEventView, ConfigurationHistoryEvent,
        OperatorHistoryResponse, VariantSelectionHistoryEvent,
    },
};

use crate::state::AppState;

const OPERATOR_HISTORY_SNAPSHOTS_PER_TOPIC: usize = 100;

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

/// The audit surface's query: filters by session, entity and kind, a
/// `before` cursor from the previous page, and a page size.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditEventQuery {
    pub session_id: Option<Uuid>,
    pub entity_kind: Option<String>,
    pub entity_id: Option<String>,
    pub event_kind: Option<String>,
    pub before: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

/// The largest page the audit surface serves.
pub const AUDIT_PAGE_LIMIT: usize = 500;
const AUDIT_PAGE_DEFAULT: usize = 100;

/// One page of the audit record under the query's filters, newest first,
/// with the cursor for the next page when the record continues.
pub async fn list_audit_events(
    state: &AppState,
    query: AuditEventQuery,
) -> Result<AuditEventListResponse, GatewayError> {
    let limit = query.limit.unwrap_or(AUDIT_PAGE_DEFAULT);
    if limit == 0 || limit > AUDIT_PAGE_LIMIT {
        return Err(GatewayError::ValidationError(format!(
            "limit must be between 1 and {AUDIT_PAGE_LIMIT}"
        )));
    }
    let blank_is_none = |value: Option<String>| {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let events = state
        .storage
        .list_audit_events_filtered(swage_storage::AuditEventFilter {
            session_id: query.session_id,
            entity_kind: blank_is_none(query.entity_kind),
            entity_id: blank_is_none(query.entity_id),
            event_kind: blank_is_none(query.event_kind),
            before: query.before,
            limit,
        })
        .await?;
    let next_before = (events.len() == limit)
        .then(|| events.last().map(|event| event.created_at))
        .flatten();
    Ok(AuditEventListResponse {
        events: events.into_iter().map(audit_event_view).collect(),
        next_before,
    })
}

fn audit_event_view(event: swage_storage::AuditEvent) -> AuditEventView {
    AuditEventView {
        id: event.id,
        session_id: event.session_id,
        entity_kind: event.entity_kind,
        entity_id: event.entity_id,
        event_kind: event.event_kind,
        summary: event.summary,
        metadata: event.metadata,
        created_at: event.created_at,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorSearchResult {
    pub kind: String,
    pub id: String,
    pub session_id: Option<Uuid>,
    pub topic_id: Option<Uuid>,
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

    configuration_changes.sort_by_key(|entry| std::cmp::Reverse(entry.recorded_at));
    variant_selections.sort_by_key(|entry| std::cmp::Reverse(entry.recorded_at));

    let mut assembly_reports = Vec::new();
    for topic in state.topic_store.list_scopes().await? {
        for snapshot in state
            .storage
            .list_snapshots_by_topic(topic.id, OPERATOR_HISTORY_SNAPSHOTS_PER_TOPIC)
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
                topic_id: snapshot.topic_id,
                recorded_at: snapshot.created_at,
                assembly_name: report.assembly_name,
            });
        }
    }
    assembly_reports.sort_by_key(|entry| std::cmp::Reverse(entry.recorded_at));

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

    let topics = state.topic_store.list_scopes().await?;
    let general_scope_id = state.topic_store.general_scope().await?.id;
    let topic_names: HashMap<Uuid, String> = topics
        .iter()
        .map(|topic| (topic.id, topic.name.clone()))
        .collect();
    let mut sessions = Vec::new();
    for topic in &topics {
        sessions.extend(state.storage.list_by_topic(topic.id).await?);
    }
    let session_topics: HashMap<Uuid, Uuid> = sessions
        .iter()
        .map(|session| (session.id, session.topic_id))
        .collect();

    let mut results = Vec::new();

    if matches_kind(kind_filter, "session") {
        for session in &sessions {
            if !matches_session(session_filter, session.id) {
                continue;
            }
            let title = topic_names
                .get(&session.topic_id)
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
                    topic_id: Some(session.topic_id),
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
                            topic_id: Some(session.topic_id),
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
            let (session_id, topic_id) =
                session_and_topic_for_turn(state, approval.turn_id).await?;
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
                    topic_id,
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
                        topic_id: run_session
                            .and_then(|session_id| session_topics.get(&session_id).copied())
                            .filter(|id| *id != general_scope_id),
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
            if !matches_optional_session(session_filter, delivery.session_id) {
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
                    session_id: delivery.session_id,
                    topic_id: delivery
                        .session_id
                        .and_then(|session_id| session_topics.get(&session_id).copied()),
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
                    topic_id: event
                        .session_id
                        .and_then(|id| session_topics.get(&id).copied()),
                    title: event.summary,
                    excerpt: Some(event.metadata.to_string()),
                    occurred_at: event.created_at,
                });
            }
        }
    }

    results.sort_by_key(|result| std::cmp::Reverse(result.occurred_at));
    results.truncate(limit);
    Ok(OperatorSearchResponse { results })
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

async fn session_and_topic_for_turn(
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
    let topic_id = state
        .storage
        .get_session(thread.session_id)
        .await?
        .map(|session| session.topic_id);
    Ok((session_id, topic_id))
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
