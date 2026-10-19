//! External communication channel gateway and canonical delivery routing.
//!
//! Canonical model:
//! - `origin` identifies where an inbound message came from.
//! - `home_target` is an optional configured delivery target distinct from the
//!   origin; callers can use it to fan responses out to a default destination.
//! - `explicit_targets` are one-off additional destinations for a single run.
//! - `local_storage` records the final response in the gateway's delivery log.
//! - duplicate targets are removed before dispatch so overlapping rules do not
//!   emit the same user-facing delivery twice.
//!
//! The initial integrations are intentionally narrow dev adapters:
//! - `dev_dm` models direct-message routing keyed by `(account, user_id)`.
//! - `dev_thread` models thread-or-topic routing keyed by
//!   `(account, channel_id/thread_id)`.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use swage_core::{GatewayError, Role};
use swage_storage::{
    NewDeliveryRecord, PendingChannelOrigin,
    model::{ChannelInboundMessage, NewSession},
};

use crate::state::AppState;
use crate::subscription::SubscriptionState;
use crate::thread_selection::resolve_thread_for_session;

const WEBHOOK_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Webhook delivery is a frequent path, so retain its connection pool instead
/// of constructing a fresh client for every POST.
static WEBHOOK_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    webhook_client(WEBHOOK_REQUEST_TIMEOUT).expect("webhook HTTP client should build")
});

fn webhook_client(timeout: Duration) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().timeout(timeout).build()
}

// The delivery wire types live in the protocol crate (the schedule contract
// embeds them); re-exported here so gateway call sites keep their paths.
pub use swage_protocol::client::delivery::{
    DeliveryRoutingRequest, DeliveryTarget, ExternalChannelIntegration,
};
use swage_protocol::client::surface::{ModuleDelivery, WriteSurfaceModuleRequest};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelOrigin {
    pub integration: ExternalChannelIntegration,
    pub account: String,
    pub conversation: String,
}

impl ChannelOrigin {
    pub fn home_target(&self) -> DeliveryTarget {
        DeliveryTarget::Channel {
            integration: self.integration.clone(),
            account: self.account.clone(),
            conversation: self.conversation.clone(),
        }
    }
}

pub use swage_protocol::channel::{
    ChannelInboundResponse, ChannelStatusListResponse, ChannelStatusResponse, DeliveryLogQuery,
    DeliveryLogResponse, DevDmInboundRequest, DevThreadInboundRequest, RecordedDelivery,
};

pub async fn handle_dev_dm_inbound(
    state: &AppState,
    request: DevDmInboundRequest,
) -> Result<ChannelInboundResponse, GatewayError> {
    handle_inbound(
        state,
        ChannelOrigin {
            integration: ExternalChannelIntegration::DevDm,
            account: request.account,
            conversation: request.user_id,
        },
        request.text,
        request.routing,
    )
    .await
}

pub async fn handle_dev_thread_inbound(
    state: &AppState,
    request: DevThreadInboundRequest,
) -> Result<ChannelInboundResponse, GatewayError> {
    handle_inbound(
        state,
        ChannelOrigin {
            integration: ExternalChannelIntegration::DevThread,
            account: request.account,
            conversation: format!("{}/{}", request.channel_id, request.thread_id),
        },
        request.text,
        request.routing,
    )
    .await
}

pub async fn read_delivery_log(state: &AppState, query: DeliveryLogQuery) -> DeliveryLogResponse {
    DeliveryLogResponse {
        deliveries: state
            .storage
            .list_delivery_records_by_session(query.session_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|record| {
                let session_id = record.session_id?;
                parse_delivery_target(record.target).map(|target| RecordedDelivery {
                    session_id,
                    run_id: record.run_id,
                    turn_id: record.turn_id,
                    target,
                    content: record.content,
                    delivered_at: record.delivered_at,
                })
            })
            .collect(),
    }
}

pub async fn list_channel_statuses(state: &AppState) -> ChannelStatusListResponse {
    let routes = state.channel_routes.lock().await;
    let deliveries = state
        .storage
        .list_delivery_records()
        .await
        .unwrap_or_default();

    let channels = [
        ExternalChannelIntegration::DevDm,
        ExternalChannelIntegration::DevThread,
        ExternalChannelIntegration::Telegram,
    ]
    .into_iter()
    .map(|integration| {
        let route_count = routes
            .keys()
            .filter(|origin| origin.integration == integration)
            .count();
        let matching_deliveries: Vec<&swage_storage::DeliveryRecord> = deliveries
            .iter()
            .filter(|delivery| {
                delivery.status == "sent"
                    && matches!(
                        parse_delivery_target(delivery.target.clone()),
                        Some(DeliveryTarget::Channel {
                            integration: target_integration,
                            ..
                        }) if target_integration == integration
                    )
            })
            .collect();
        let delivery_count = matching_deliveries.len();
        let last_delivery_at = matching_deliveries
            .iter()
            .map(|delivery| delivery.delivered_at)
            .max();
        let status =
            if integration == ExternalChannelIntegration::Telegram && state.telegram.is_some() {
                "polling"
            } else if route_count > 0 || delivery_count > 0 {
                "active"
            } else {
                "idle"
            };

        ChannelStatusResponse {
            integration: integration.as_metadata_value().to_string(),
            status: status.to_string(),
            route_count,
            delivery_count,
            last_delivery_at,
        }
    })
    .collect();

    ChannelStatusListResponse { channels }
}

async fn handle_inbound(
    state: &AppState,
    origin: ChannelOrigin,
    text: String,
    routing: DeliveryRoutingRequest,
) -> Result<ChannelInboundResponse, GatewayError> {
    let session_id =
        ensure_session_for_origin(state, &origin, SessionResolutionPolicy::AllowDefaultTopic)
            .await?;
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    let targets = resolve_delivery_targets(
        Some(&origin),
        DeliveryRoutingRequest {
            home_target: routing
                .home_target
                .clone()
                .or(configured_home_target(state, &origin).await?),
            ..routing.clone()
        },
    )?;
    let result = if state.active_turns.lock().await.contains_key(&session_id)
        || crate::turn_queue::has_queued_input(state, session_id).await?
    {
        Err(GatewayError::TurnInProgress { session_id })
    } else {
        run_channel_turn(state, session_id, text.clone()).await
    };
    let answered = match result {
        Ok(answered) => answered,
        Err(GatewayError::TurnInProgress { .. }) => {
            let queued = crate::turn_queue::queue_input_with_attribution(
                state,
                session_id,
                thread.id,
                crate::turn::TurnInput::text(&text),
                None,
                Vec::new(),
                crate::turn::TurnAttendance::Unattended,
                targets,
            )
            .await?;
            return Ok(ChannelInboundResponse {
                session_id,
                queued: Some(queued),
                delivered_to: Vec::new(),
                response: String::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let targets = deliver_channel_response(
        state,
        session_id,
        DeliveryAttribution {
            turn_id: answered.turn_id,
            ..DeliveryAttribution::default()
        },
        &origin,
        routing,
        &answered.response,
    )
    .await?;

    Ok(ChannelInboundResponse {
        queued: None,
        session_id,
        delivered_to: targets,
        response: answered.response,
    })
}

#[derive(Debug, Clone, Copy)]
enum SessionResolutionPolicy {
    AllowDefaultTopic,
    LinkedOnly,
}

async fn ensure_session_for_origin(
    state: &AppState,
    origin: &ChannelOrigin,
    policy: SessionResolutionPolicy,
) -> Result<Uuid, GatewayError> {
    if let Some(existing) = state.channel_routes.lock().await.get(origin).copied()
        && state.storage.get_session(existing).await?.is_some()
    {
        return Ok(existing);
    }

    if let Some(existing) = state
        .storage
        .find_session_by_channel_origin(
            origin.integration.as_metadata_value(),
            &origin.account,
            &origin.conversation,
        )
        .await?
    {
        state
            .channel_routes
            .lock()
            .await
            .insert(origin.clone(), existing.id);
        return Ok(existing.id);
    }

    if let Some(link) = state
        .storage
        .get_channel_link(
            origin.integration.as_metadata_value(),
            &origin.account,
            &origin.conversation,
        )
        .await?
    {
        if let Some(session_id) = link.session_id
            && state.storage.get_session(session_id).await?.is_some()
        {
            state
                .channel_routes
                .lock()
                .await
                .insert(origin.clone(), session_id);
            return Ok(session_id);
        }

        let session_id =
            create_channel_session(state, origin, link.topic_id, &link.agent_id).await?;
        state
            .storage
            .upsert_channel_link(swage_storage::NewChannelLink {
                integration: origin.integration.as_metadata_value().to_string(),
                account: origin.account.clone(),
                conversation: origin.conversation.clone(),
                topic_id: link.topic_id,
                agent_id: link.agent_id,
                session_id: Some(session_id),
                home_target: link.home_target,
            })
            .await?;
        state
            .channel_routes
            .lock()
            .await
            .insert(origin.clone(), session_id);
        return Ok(session_id);
    }

    if matches!(policy, SessionResolutionPolicy::LinkedOnly) {
        return Err(GatewayError::ValidationError(format!(
            "channel origin {}:{}:{} is not linked",
            origin.integration.as_metadata_value(),
            origin.account,
            origin.conversation
        )));
    }

    let topic = state.topic_store.general_scope().await?;
    let session_id = create_channel_session(state, origin, topic.id, "gateway").await?;

    state
        .channel_routes
        .lock()
        .await
        .insert(origin.clone(), session_id);

    Ok(session_id)
}

/// Both channel opening routes establish the root thread before applying a
/// topic's default surfaces. The general scope has no defaults, but shares the
/// same ordering when a session is opened there.
async fn create_channel_session(
    state: &AppState,
    origin: &ChannelOrigin,
    topic_id: Uuid,
    agent_id: &str,
) -> Result<Uuid, GatewayError> {
    let session = state
        .storage
        .create_session(NewSession {
            topic_id,
            agent_id: agent_id.to_string(),
            origin: swage_storage::SessionOrigin::Direct,
            metadata: Some(serde_json::json!({
                "channel_origin": {
                    "integration": origin.integration,
                    "account": origin.account,
                    "conversation": origin.conversation
                }
            })),
            active_workspace_attachment_id: None,
        })
        .await?;
    state
        .storage
        .create(swage_storage::model::NewThread {
            session_id: session.id,
            agent_id: agent_id.to_string(),
        })
        .await?;
    crate::surface::open_default_surfaces(state.storage.as_ref(), &state.broadcast, session.id)
        .await?;
    Ok(session.id)
}

async fn configured_home_target(
    state: &AppState,
    origin: &ChannelOrigin,
) -> Result<Option<DeliveryTarget>, GatewayError> {
    state
        .storage
        .get_channel_link(
            origin.integration.as_metadata_value(),
            &origin.account,
            &origin.conversation,
        )
        .await?
        .and_then(|link| link.home_target)
        .map(serde_json::from_value::<DeliveryTarget>)
        .transpose()
        .map_err(|error| {
            GatewayError::ValidationError(format!(
                "invalid stored channel home_target for {}:{}:{}: {error}",
                origin.integration.as_metadata_value(),
                origin.account,
                origin.conversation
            ))
        })
}

async fn deliver_channel_response(
    state: &AppState,
    session_id: Uuid,
    attribution: DeliveryAttribution,
    origin: &ChannelOrigin,
    routing: DeliveryRoutingRequest,
    response: &str,
) -> Result<Vec<DeliveryTarget>, GatewayError> {
    let configured_home_target = configured_home_target(state, origin).await?;
    let targets = resolve_delivery_targets(
        Some(origin),
        DeliveryRoutingRequest {
            home_target: routing.home_target.or(configured_home_target),
            ..routing
        },
    )?;
    deliver_to_targets(state, session_id, attribution, &targets, response).await?;
    Ok(targets)
}

pub async fn drain_channel_queue(state: AppState) {
    if let Err(error) = drain_channel_queue_checked(&state).await {
        tracing::warn!("failed to drain channel queue origin: {error}");
    }
}

pub async fn drain_channel_queue_checked(state: &AppState) -> Result<(), GatewayError> {
    let rows = state.storage.list_pending_channel_messages().await?;
    // Allocate each session's queue sequence in inbox arrival order, including
    // messages from different chats linked to that same session.
    let mut batches: Vec<(Uuid, Uuid, Vec<ChannelInboundMessage>)> = Vec::new();
    let mut first_error = None;
    for row in rows {
        match resolve_channel_destination(state, &row).await {
            Ok(Some((session_id, thread_id))) => {
                if let Some((_, _, batch)) = batches.iter_mut().find(|(id, _, _)| *id == session_id)
                {
                    batch.push(row);
                } else {
                    batches.push((session_id, thread_id, vec![row]));
                }
            }
            Ok(None) => {}
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    for (session_id, thread_id, rows) in batches {
        if let Err(error) = queue_channel_rows(state, session_id, thread_id, rows).await
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok(())
    }
}

fn channel_origin(pending: &PendingChannelOrigin) -> Result<ChannelOrigin, GatewayError> {
    let integration = pending
        .integration
        .parse::<ExternalChannelIntegration>()
        .map_err(GatewayError::ValidationError)?;
    Ok(ChannelOrigin {
        integration,
        account: pending.account.clone(),
        conversation: pending.conversation.clone(),
    })
}

async fn resolve_channel_destination(
    state: &AppState,
    row: &ChannelInboundMessage,
) -> Result<Option<(Uuid, Uuid)>, GatewayError> {
    let origin = channel_origin(&PendingChannelOrigin {
        integration: row.integration.clone(),
        account: row.account.clone(),
        conversation: row.conversation.clone(),
    })?;
    let session_id = match ensure_session_for_origin(
        state,
        &origin,
        SessionResolutionPolicy::LinkedOnly,
    )
    .await
    {
        Ok(session_id) => session_id,
        Err(GatewayError::ValidationError(_))
            if origin.integration == ExternalChannelIntegration::Telegram =>
        {
            state
                .storage
                .discard_channel_messages(vec![row.id], "unlinked")
                .await?;
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    Ok(Some((session_id, thread.id)))
}

async fn queue_channel_rows(
    state: &AppState,
    session_id: Uuid,
    thread_id: Uuid,
    rows: Vec<ChannelInboundMessage>,
) -> Result<(), GatewayError> {
    let inputs = rows
        .iter()
        .map(|row| swage_storage::NewQueuedTurnInput {
            delivery_targets: Vec::new(),
            attended: false,
            client_id: None,
            session_id,
            thread_id,
            origin: swage_storage::MessageOrigin::User,
            content: row.content.clone(),
            attachments: Vec::new(),
            schedule_run_id: None,
            channel_message_ids: vec![row.id],
        })
        .collect();
    match state.storage.enqueue_turn_inputs(inputs).await {
        Ok(queued) => {
            crate::turn_queue::announce_queued_inputs(state, session_id, &queued).await?;
        }
        Err(error @ swage_core::StorageError::ConstraintViolation(_)) => {
            let queued = state.storage.list_queued_turn_inputs(session_id).await?;
            if !rows.iter().all(|row| {
                queued
                    .iter()
                    .any(|input| input.channel_message_ids.contains(&row.id))
            }) {
                return Err(error.into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Resolve the shared turn's destinations for an originating chat.
pub(crate) async fn queued_response_targets(
    state: &AppState,
    pending: PendingChannelOrigin,
) -> Result<Vec<DeliveryTarget>, GatewayError> {
    let origin = channel_origin(&pending)?;
    resolve_delivery_targets(
        Some(&origin),
        DeliveryRoutingRequest {
            home_target: configured_home_target(state, &origin).await?,
            ..Default::default()
        },
    )
}

/// What a channel's immediate turn answered, with the turn that answered
/// it: a delivery of the response records that turn as its origin.
struct ChannelTurnAnswer {
    turn_id: Option<Uuid>,
    response: String,
}

async fn run_channel_turn(
    state: &AppState,
    session_id: Uuid,
    text: String,
) -> Result<ChannelTurnAnswer, GatewayError> {
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
    let subscription = Arc::new(Mutex::new(SubscriptionState::new(false)));

    tokio::spawn(async move { while ws_rx.recv().await.is_some() {} });

    Box::pin(crate::turn::process_turn_with_existing_turn(
        state,
        session_id,
        Some(thread.id),
        text,
        ws_tx,
        subscription,
        None,
        None,
        None,
        None,
        None,
        crate::turn::TurnAttendance::Unattended,
    ))
    .await?;

    let messages = state.storage.list_messages_by_thread(thread.id).await?;
    messages
        .into_iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(|message| ChannelTurnAnswer {
            turn_id: message.turn_id,
            response: message.content,
        })
        .ok_or_else(|| {
            GatewayError::ValidationError(
                "channel turn completed without assistant response".into(),
            )
        })
}

pub fn resolve_delivery_targets(
    origin: Option<&ChannelOrigin>,
    routing: DeliveryRoutingRequest,
) -> Result<Vec<DeliveryTarget>, GatewayError> {
    let mut targets = Vec::new();

    if routing.deliver_to_origin {
        let origin = origin.ok_or_else(|| {
            GatewayError::ValidationError(
                "delivery routing requested origin delivery without an origin".into(),
            )
        })?;
        push_unique_target(&mut targets, origin.home_target());
    }

    if let Some(home_target) = routing.home_target {
        push_unique_target(&mut targets, home_target);
    }

    for target in routing.explicit_targets {
        push_unique_target(&mut targets, target);
    }

    if routing.store_local_output {
        push_unique_target(&mut targets, DeliveryTarget::LocalStorage);
    }

    if targets.is_empty() {
        return Err(GatewayError::ValidationError(
            "delivery routing resolved no targets".into(),
        ));
    }

    Ok(targets)
}

pub(crate) fn push_unique_target(targets: &mut Vec<DeliveryTarget>, target: DeliveryTarget) {
    if !targets.contains(&target) {
        targets.push(target);
    }
}

pub async fn deliver_to_targets(
    state: &AppState,
    session_id: Uuid,
    attribution: DeliveryAttribution,
    targets: &[DeliveryTarget],
    content: &str,
) -> Result<(), GatewayError> {
    let mut delivery_failure: Option<GatewayError> = None;
    let mut recording_failure: Option<GatewayError> = None;
    for target in targets {
        let (status, delivery_error) =
            match deliver_to_target(state, session_id, &attribution, target, content).await {
                Ok(DeliveryOutcome::Sent) => ("sent", None),
                Ok(DeliveryOutcome::NotSent) => (
                    "not_sent",
                    Some("development channel target is a deliberate no-op".to_string()),
                ),
                Err(error) => {
                    let error_text = error.to_string();
                    if delivery_failure.is_none() {
                        delivery_failure = Some(error);
                    }
                    ("failed", Some(error_text))
                }
            };

        if let Err(record_error) = record_delivery(
            state,
            session_id,
            attribution,
            target.clone(),
            content.to_string(),
            status,
            delivery_error,
        )
        .await
        {
            tracing::warn!(
                %session_id,
                run_id = ?attribution.run_id(),
                turn_id = ?attribution.turn_id,
                ?target,
                error = %record_error,
                "failed to record delivery attempt"
            );
            if recording_failure.is_none() {
                recording_failure = Some(record_error);
            }
        }
    }

    // A recording failure is returned rather than silently losing audit data.
    // When both operations fail, the delivery failure wins because it is the
    // operational cause; the recording failure is still emitted above.
    match (delivery_failure, recording_failure) {
        (Some(error), _) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}

/// What produced the output being delivered: a scheduled run (with its
/// schedule) and the turn that ran it, a turn alone, or neither.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeliveryAttribution {
    /// The scheduled run behind the output, with its schedule.
    pub schedule_run: Option<ScheduleRunAttribution>,
    pub turn_id: Option<Uuid>,
}

/// A scheduled run and the schedule it belongs to — a run is never named
/// without its schedule.
#[derive(Debug, Clone, Copy)]
pub struct ScheduleRunAttribution {
    pub run_id: Uuid,
    pub schedule_id: Uuid,
}

impl DeliveryAttribution {
    fn run_id(&self) -> Option<Uuid> {
        self.schedule_run.map(|run| run.run_id)
    }

    /// The delivery a module written under this attribution records: the
    /// schedule run's where one is behind the output, else the turn's, else
    /// none — every shape answered, so a new origin that sets neither is
    /// visible here rather than swallowed.
    pub fn module_delivery(
        &self,
        delivered_at: chrono::DateTime<chrono::Utc>,
    ) -> Option<ModuleDelivery> {
        match (self.schedule_run, self.turn_id) {
            (Some(run), _) => Some(ModuleDelivery::ScheduleRun {
                run_id: run.run_id,
                schedule_id: run.schedule_id,
                delivered_at,
            }),
            (None, Some(turn_id)) => Some(ModuleDelivery::Turn {
                turn_id,
                delivered_at,
            }),
            (None, None) => None,
        }
    }
}

enum DeliveryOutcome {
    Sent,
    NotSent,
}

async fn deliver_to_target(
    state: &AppState,
    session_id: Uuid,
    attribution: &DeliveryAttribution,
    target: &DeliveryTarget,
    content: &str,
) -> Result<DeliveryOutcome, GatewayError> {
    match target {
        DeliveryTarget::Channel {
            integration: ExternalChannelIntegration::DevDm | ExternalChannelIntegration::DevThread,
            ..
        } => Ok(DeliveryOutcome::NotSent),
        DeliveryTarget::LocalStorage => Ok(DeliveryOutcome::Sent),
        DeliveryTarget::Surface { surface_id } => {
            let session = state
                .storage
                .get_session(session_id)
                .await?
                .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
            let surface = state.storage.get_page(*surface_id).await?.ok_or_else(|| {
                GatewayError::ValidationError(format!("no surface with id {surface_id}"))
            })?;
            if surface.session_id != session.id {
                return Err(GatewayError::ValidationError(format!(
                    "surface delivery target {surface_id} does not belong to the session"
                )));
            }
            crate::surface::write_surface_module(
                state,
                *surface_id,
                WriteSurfaceModuleRequest {
                    module_id: None,
                    kind: Some("document".into()),
                    source: content.to_string(),
                    expected_revision: None,
                },
                "delivery",
                attribution.module_delivery(chrono::Utc::now()),
            )
            .await?;
            Ok(DeliveryOutcome::Sent)
        }
        DeliveryTarget::Channel {
            integration: ExternalChannelIntegration::Telegram,
            conversation,
            ..
        } => {
            let telegram = state.telegram.as_ref().ok_or_else(|| {
                GatewayError::ValidationError("telegram channel is not configured".into())
            })?;
            telegram
                .send_message(conversation, content)
                .await
                .map(|()| DeliveryOutcome::Sent)
        }
        DeliveryTarget::Webhook { url } => {
            let parsed = reqwest::Url::parse(url).map_err(|error| {
                GatewayError::ValidationError(format!("invalid webhook target URL: {error}"))
            })?;
            let response = WEBHOOK_CLIENT
                .post(parsed)
                .json(&serde_json::json!({
                    "session_id": session_id,
                    "content": content,
                }))
                .send()
                .await
                .map_err(|error| {
                    GatewayError::ValidationError(format!("webhook delivery failed: {error}"))
                })?;

            if !response.status().is_success() {
                return Err(GatewayError::ValidationError(format!(
                    "webhook delivery failed with status {}",
                    response.status()
                )));
            }

            Ok(DeliveryOutcome::Sent)
        }
    }
}

async fn record_delivery(
    state: &AppState,
    session_id: Uuid,
    attribution: DeliveryAttribution,
    target: DeliveryTarget,
    content: String,
    status: &str,
    error: Option<String>,
) -> Result<(), GatewayError> {
    let target_json = serde_json::to_value(&target).map_err(|error| {
        GatewayError::ValidationError(format!("failed to serialise delivery target: {error}"))
    })?;
    state
        .storage
        .create_delivery_record(NewDeliveryRecord {
            session_id: Some(session_id),
            run_id: attribution.run_id(),
            turn_id: attribution.turn_id,
            target: target_json,
            content,
            status: status.to_string(),
            error,
        })
        .await?;
    Ok(())
}

/// Record why a schedule produced no output delivery before it reached a
/// target. The schedule run is the durable owner when no session exists.
pub async fn record_no_delivery(
    state: &AppState,
    run_id: Uuid,
    reason: impl Into<String>,
) -> Result<(), GatewayError> {
    state
        .storage
        .create_delivery_record(NewDeliveryRecord {
            session_id: None,
            run_id: Some(run_id),
            turn_id: None,
            target: serde_json::json!({ "kind": "no_delivery" }),
            content: String::new(),
            status: "not_delivered".into(),
            error: Some(reason.into()),
        })
        .await?;
    Ok(())
}

fn parse_delivery_target(target: serde_json::Value) -> Option<DeliveryTarget> {
    serde_json::from_value(target).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::tests::make_state_with_session;
    use swage_core::{FinishReason, StreamChunk};
    use swage_storage::{
        AuditStore, ChannelLinkStore, ScheduleStore, SessionStore, SurfaceStore, ThreadStore,
        TopicStore,
    };
    use tokio::net::TcpListener;

    #[tokio::test(flavor = "multi_thread")]
    async fn developer_routes_accept_busy_session_inputs_in_the_shared_queue() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("channels.db");
        let storage = Arc::new(swage_storage::SqliteStorage::open(&database).unwrap());
        let state = AppState::new(
            storage.clone(),
            storage.clone(),
            Arc::new(crate::turn::tests::MockProvider::new(vec![
                swage_core::StreamChunk::Delta {
                    content: "shared reply".into(),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ])),
            Default::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let linked = crate::onboarding::create_channel_link(
            &state,
            crate::onboarding::CreateChannelLinkRequest {
                integration: "dev_dm".into(),
                account: "account".into(),
                conversation: "person".into(),
                topic_id: None,
                agent_id: None,
                home_target: None,
            },
        )
        .await
        .unwrap();
        let session_id = linked.session_id.unwrap();
        let active =
            crate::automated_turn::create_placeholder_turn(&state, session_id, "gateway", "active")
                .await
                .unwrap();
        for (integration, conversation) in [
            (ExternalChannelIntegration::DevDm, "person"),
            (ExternalChannelIntegration::DevThread, "room/thread"),
        ] {
            state.channel_routes.lock().await.insert(
                ChannelOrigin {
                    integration,
                    account: "account".into(),
                    conversation: conversation.into(),
                },
                session_id,
            );
        }
        state
            .active_turns
            .lock()
            .await
            .insert(session_id, tokio_util::sync::CancellationToken::new());
        let dm = handle_dev_dm_inbound(
            &state,
            DevDmInboundRequest {
                account: "account".into(),
                user_id: "person".into(),
                text: "first".into(),
                routing: DeliveryRoutingRequest {
                    store_local_output: true,
                    ..Default::default()
                },
            },
        )
        .await;
        let thread = handle_dev_thread_inbound(
            &state,
            DevThreadInboundRequest {
                account: "account".into(),
                channel_id: "room".into(),
                thread_id: "thread".into(),
                text: "second".into(),
                routing: DeliveryRoutingRequest {
                    store_local_output: true,
                    ..Default::default()
                },
            },
        )
        .await;
        assert!(
            dm.is_ok() && thread.is_ok(),
            "busy developer routes must accept input: dm={dm:?}, thread={thread:?}"
        );
        let queued = state
            .storage
            .list_queued_turn_inputs(session_id)
            .await
            .unwrap();
        assert_eq!(
            queued
                .iter()
                .map(|row| (row.content.as_str(), row.origin))
                .collect::<Vec<_>>(),
            vec![
                ("first", swage_storage::MessageOrigin::User),
                ("second", swage_storage::MessageOrigin::User)
            ]
        );
        state
            .storage
            .update_turn_state(active, swage_storage::TurnState::Completed)
            .await
            .unwrap();
        drop(state);
        drop(storage);
        let reopened = Arc::new(swage_storage::SqliteStorage::open(&database).unwrap());
        let rebuilt = AppState::new(
            reopened.clone(),
            reopened,
            Arc::new(crate::turn::tests::MockProvider::new(vec![
                swage_core::StreamChunk::Delta {
                    content: "shared reply".into(),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ])),
            Default::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        crate::turn_queue::drain_queue(&rebuilt, session_id)
            .await
            .unwrap();
        let deliveries = rebuilt.storage.list_delivery_records().await.unwrap();
        assert_eq!(
            deliveries
                .iter()
                .map(
                    |record| serde_json::from_value::<DeliveryTarget>(record.target.clone())
                        .unwrap()
                )
                .collect::<std::collections::HashSet<_>>(),
            [
                DeliveryTarget::Channel {
                    integration: ExternalChannelIntegration::DevDm,
                    account: "account".into(),
                    conversation: "person".into()
                },
                DeliveryTarget::Channel {
                    integration: ExternalChannelIntegration::DevThread,
                    account: "account".into(),
                    conversation: "room/thread".into()
                },
                DeliveryTarget::LocalStorage,
            ]
            .into_iter()
            .collect(),
            "each originating channel and the shared local target must receive the reply"
        );
        assert_eq!(
            deliveries.len(),
            3,
            "two origins share one local output, with one delivery per target"
        );
        assert!(
            deliveries
                .iter()
                .all(|delivery| delivery.content == "shared reply")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shared_channel_targets_receive_one_delivery_per_drained_turn() {
        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        let state = AppState::new(
            storage.clone(),
            storage.clone(),
            Arc::new(crate::turn::tests::MockProvider::new(vec![
                StreamChunk::Delta {
                    content: "shared reply".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ])),
            Default::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let linked = crate::onboarding::create_channel_link(
            &state,
            crate::onboarding::CreateChannelLinkRequest {
                integration: "dev_dm".into(),
                account: "account".into(),
                conversation: "one".into(),
                topic_id: None,
                agent_id: None,
                home_target: None,
            },
        )
        .await
        .unwrap();
        let session = linked.session_id.unwrap();
        let thread = resolve_thread_for_session(state.storage.as_ref(), session, None)
            .await
            .unwrap();
        state
            .active_turns
            .lock()
            .await
            .insert(session, tokio_util::sync::CancellationToken::new());
        for conversation in ["one", "two"] {
            state
                .storage
                .upsert_channel_link(swage_storage::NewChannelLink {
                    integration: "dev_dm".into(),
                    account: "account".into(),
                    conversation: conversation.into(),
                    topic_id: linked.topic_id,
                    agent_id: "gateway".into(),
                    session_id: Some(session),
                    home_target: Some(serde_json::to_value(DeliveryTarget::LocalStorage).unwrap()),
                })
                .await
                .unwrap();
            state
                .storage
                .ingest_channel_inbound(
                    vec![swage_storage::NewChannelInboundMessage {
                        integration: "dev_dm".into(),
                        account: "account".into(),
                        conversation: conversation.into(),
                        external_id: conversation.into(),
                        sender: "person".into(),
                        content: conversation.into(),
                    }],
                    "dev_dm",
                    "account",
                    "1",
                )
                .await
                .unwrap();
            let rows = state
                .storage
                .list_pending_by_origin(PendingChannelOrigin {
                    integration: "dev_dm".into(),
                    account: "account".into(),
                    conversation: conversation.into(),
                })
                .await
                .unwrap();
            queue_channel_rows(&state, session, thread.id, rows)
                .await
                .unwrap();
        }
        state.active_turns.lock().await.remove(&session);
        crate::turn_queue::drain_queue(&state, session)
            .await
            .unwrap();
        let deliveries = state.storage.list_delivery_records().await.unwrap();
        assert_eq!(
            deliveries
                .iter()
                .map(
                    |record| serde_json::from_value::<DeliveryTarget>(record.target.clone())
                        .unwrap()
                )
                .collect::<std::collections::HashSet<_>>(),
            [
                DeliveryTarget::Channel {
                    integration: ExternalChannelIntegration::DevDm,
                    account: "account".into(),
                    conversation: "one".into()
                },
                DeliveryTarget::Channel {
                    integration: ExternalChannelIntegration::DevDm,
                    account: "account".into(),
                    conversation: "two".into()
                },
                DeliveryTarget::LocalStorage,
            ]
            .into_iter()
            .collect(),
            "each originating channel and the shared local target must receive the reply"
        );
        assert_eq!(
            deliveries.len(),
            3,
            "two origins with one shared target must deliver to three distinct targets"
        );
        assert!(
            deliveries
                .iter()
                .all(|record| record.content == "shared reply")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn interleaved_channel_origins_keep_session_arrival_order() {
        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        let state = AppState::new(
            storage.clone(),
            storage.clone(),
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            Default::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let linked = crate::onboarding::create_channel_link(
            &state,
            crate::onboarding::CreateChannelLinkRequest {
                integration: "dev_dm".into(),
                account: "account".into(),
                conversation: "a".into(),
                topic_id: None,
                agent_id: None,
                home_target: None,
            },
        )
        .await
        .unwrap();
        let session = linked.session_id.unwrap();
        state
            .storage
            .upsert_channel_link(swage_storage::NewChannelLink {
                integration: "dev_dm".into(),
                account: "account".into(),
                conversation: "b".into(),
                topic_id: linked.topic_id,
                agent_id: "gateway".into(),
                session_id: Some(session),
                home_target: None,
            })
            .await
            .unwrap();
        state
            .active_turns
            .lock()
            .await
            .insert(session, tokio_util::sync::CancellationToken::new());
        state
            .storage
            .ingest_channel_inbound(
                [("a", "A1"), ("b", "B1"), ("a", "A2")]
                    .into_iter()
                    .map(
                        |(conversation, content)| swage_storage::NewChannelInboundMessage {
                            integration: "dev_dm".into(),
                            account: "account".into(),
                            conversation: conversation.into(),
                            external_id: content.into(),
                            sender: "person".into(),
                            content: content.into(),
                        },
                    )
                    .collect(),
                "dev_dm",
                "account",
                "1",
            )
            .await
            .unwrap();
        drain_channel_queue_checked(&state).await.unwrap();
        let queued = state
            .storage
            .list_queued_turn_inputs(session)
            .await
            .unwrap();
        assert_eq!(
            queued
                .iter()
                .map(|input| input.content.as_str())
                .collect::<Vec<_>>(),
            ["A1", "B1", "A2"],
            "shared session inputs must keep cross-origin arrival order"
        );
        assert!(
            queued
                .iter()
                .all(|input| input.channel_message_ids.len() == 1)
        );
        drain_channel_queue_checked(&state).await.unwrap();
        assert_eq!(
            state
                .storage
                .list_queued_turn_inputs(session)
                .await
                .unwrap()
                .len(),
            3,
            "transferred inbox messages must not be queued twice"
        );
    }

    #[test]
    fn home_target_defaults_to_origin() {
        let origin = ChannelOrigin {
            integration: ExternalChannelIntegration::DevThread,
            account: "ops".into(),
            conversation: "room/thread".into(),
        };

        assert_eq!(
            origin.home_target(),
            DeliveryTarget::Channel {
                integration: ExternalChannelIntegration::DevThread,
                account: "ops".into(),
                conversation: "room/thread".into(),
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_channel_default_is_recorded_after_its_root_thread_exists() {
        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::create(
            storage.as_ref(),
            swage_storage::NewTopic {
                name: "Channel defaults".into(),
                slug: "channel-defaults".into(),
            },
        )
        .await
        .unwrap();
        let state = AppState::for_test(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let owner = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "gateway".into(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let surface = crate::surface::create_surface(
            &state,
            owner.id,
            swage_protocol::client::surface::CreateSurfaceRequest {
                title: "Channel board".into(),
                library_entry_id: None,
            },
        )
        .await
        .unwrap();
        let entry = crate::surface::save_surface_to_library(
            &state,
            surface.id,
            swage_protocol::client::surface::SaveSurfaceToLibraryRequest {
                name: "Channel board".into(),
                description: "A board opened for channel sessions".into(),
                tags: vec![],
            },
            "user",
        )
        .await
        .unwrap();
        storage
            .add_topic_default_entry(topic.id, entry.id)
            .await
            .unwrap();
        storage
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER require_root_for_seed_audit BEFORE INSERT ON audit_events
             WHEN NEW.event_kind = 'surface_created' AND NOT EXISTS
               (SELECT 1 FROM threads WHERE session_id = NEW.session_id)
             BEGIN SELECT RAISE(ABORT, 'root thread missing at surface audit'); END;",
                )
            })
            .await
            .unwrap();
        let origin = ChannelOrigin {
            integration: ExternalChannelIntegration::DevDm,
            account: "workspace".into(),
            conversation: "channel-board".into(),
        };
        storage
            .upsert_channel_link(swage_storage::NewChannelLink {
                integration: origin.integration.as_metadata_value().into(),
                account: origin.account.clone(),
                conversation: origin.conversation.clone(),
                topic_id: topic.id,
                agent_id: "gateway".into(),
                session_id: None,
                home_target: None,
            })
            .await
            .unwrap();

        let session_id =
            ensure_session_for_origin(&state, &origin, SessionResolutionPolicy::LinkedOnly)
                .await
                .unwrap();
        assert_eq!(storage.list_by_session(session_id).await.unwrap().len(), 1);
        let surfaces = storage.list_surfaces_by_session(session_id).await.unwrap();
        assert_eq!(surfaces.len(), 1);
        let events = storage
            .list_audit_events_by_session(session_id)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.event_kind == "surface_created"
                    && event.entity_id == surfaces[0].id.to_string()),
            "the channel seed has a creation event after its root thread"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlinked_channel_opens_with_a_root_thread_in_the_general_scope() {
        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        let general = storage.general_scope().await.unwrap();
        let state = AppState::for_test(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let origin = ChannelOrigin {
            integration: ExternalChannelIntegration::DevDm,
            account: "workspace".into(),
            conversation: "unlinked".into(),
        };

        let session_id =
            ensure_session_for_origin(&state, &origin, SessionResolutionPolicy::AllowDefaultTopic)
                .await
                .unwrap();
        let session = storage.get_session(session_id).await.unwrap().unwrap();
        assert_eq!(session.topic_id, general.id);
        assert_eq!(
            storage.list_by_session(session_id).await.unwrap().len(),
            1,
            "the unlinked branch uses the channel opening sequence with a root thread"
        );
        assert!(
            storage
                .list_surfaces_by_session(session_id)
                .await
                .unwrap()
                .is_empty(),
            "the general scope has no default surfaces"
        );
    }

    #[test]
    fn resolve_delivery_targets_deduplicates_overlapping_rules() {
        let origin = ChannelOrigin {
            integration: ExternalChannelIntegration::DevDm,
            account: "workspace".into(),
            conversation: "alice".into(),
        };

        let targets = resolve_delivery_targets(
            Some(&origin),
            DeliveryRoutingRequest {
                deliver_to_origin: true,
                home_target: Some(origin.home_target()),
                explicit_targets: vec![origin.home_target()],
                store_local_output: true,
            },
        )
        .expect("targets");

        assert_eq!(
            targets,
            vec![origin.home_target(), DeliveryTarget::LocalStorage]
        );
    }

    #[test]
    fn resolve_delivery_targets_rejects_empty_resolution() {
        let origin = ChannelOrigin {
            integration: ExternalChannelIntegration::DevDm,
            account: "workspace".into(),
            conversation: "alice".into(),
        };

        let error = resolve_delivery_targets(
            Some(&origin),
            DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: Vec::new(),
                store_local_output: false,
            },
        )
        .expect_err("empty routing should fail");

        assert!(error.to_string().contains("resolved no targets"));
    }

    #[test]
    fn resolve_delivery_targets_rejects_origin_delivery_without_origin_context() {
        let error = resolve_delivery_targets(
            None,
            DeliveryRoutingRequest {
                deliver_to_origin: true,
                home_target: None,
                explicit_targets: Vec::new(),
                store_local_output: false,
            },
        )
        .expect_err("missing origin should fail");

        assert!(error.to_string().contains("without an origin"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn channel_status_counts_only_sent_deliveries() {
        let (mut state, _) = make_state_with_session(vec![]).await;
        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("general scope");
        let session_id = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .expect("session")
            .id;
        state.storage = storage.clone();
        state.topic_store = storage;
        let target = DeliveryTarget::Channel {
            integration: ExternalChannelIntegration::DevDm,
            account: "workspace".into(),
            conversation: "alice".into(),
        };
        for (status, error) in [
            ("sent", None),
            ("not_sent", Some("deliberate no-op".to_string())),
            ("failed", Some("delivery failed".to_string())),
        ] {
            record_delivery(
                &state,
                session_id,
                DeliveryAttribution::default(),
                target.clone(),
                format!("{status} payload"),
                status,
                error,
            )
            .await
            .expect("record delivery status");
        }
        let sent_at = state
            .storage
            .list_delivery_records()
            .await
            .expect("list delivery records")
            .into_iter()
            .find(|record| record.status == "sent")
            .expect("sent delivery record")
            .delivered_at;

        let statuses = list_channel_statuses(&state).await;
        let dev_dm = statuses
            .channels
            .iter()
            .find(|channel| channel.integration == "dev_dm")
            .expect("dev_dm status");

        assert_eq!(dev_dm.delivery_count, 1);
        assert_eq!(dev_dm.last_delivery_at, Some(sent_at));
    }

    /// The attribution answers every shape: a schedule run's delivery where
    /// a run is behind the output, a turn's where only a turn is, none where
    /// neither — nothing falls through.
    #[test]
    fn a_delivery_attribution_names_the_schedule_run_or_the_turn_behind_it() {
        let at = chrono::Utc::now();
        let run = ScheduleRunAttribution {
            run_id: Uuid::new_v4(),
            schedule_id: Uuid::new_v4(),
        };
        let turn_id = Uuid::new_v4();
        assert_eq!(
            DeliveryAttribution {
                schedule_run: Some(run),
                turn_id: Some(turn_id),
            }
            .module_delivery(at),
            Some(ModuleDelivery::ScheduleRun {
                run_id: run.run_id,
                schedule_id: run.schedule_id,
                delivered_at: at,
            })
        );
        assert_eq!(
            DeliveryAttribution {
                schedule_run: None,
                turn_id: Some(turn_id),
            }
            .module_delivery(at),
            Some(ModuleDelivery::Turn {
                turn_id,
                delivered_at: at,
            })
        );
        assert_eq!(DeliveryAttribution::default().module_delivery(at), None);
    }

    /// A surface target reached through the shared delivery path records
    /// the schedule run, or the turn, that produced the output.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_surface_delivery_records_the_schedule_run_or_the_turn_behind_it() {
        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        let state = AppState::new(
            storage.clone(),
            storage.clone(),
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            Default::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let topic = storage.general_scope().await.unwrap();
        let session_id = storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .unwrap()
            .id;
        let surface = crate::surface::create_surface(
            &state,
            session_id,
            swage_protocol::client::surface::CreateSurfaceRequest {
                title: "Deliveries".into(),
                library_entry_id: None,
            },
        )
        .await
        .unwrap();
        let targets = [DeliveryTarget::Surface {
            surface_id: surface.id,
        }];
        // The delivery record names real rows: a schedule's run and the
        // turn that ran it.
        let schedule = storage
            .create_schedule(swage_storage::model::NewSchedule {
                id: Uuid::new_v4(),
                message: "Deliver the digest".into(),
                status: swage_storage::model::ScheduleStatus::Active,
                kind: swage_storage::model::ScheduleKind::Interval,
                expression: "every 5 minutes".into(),
                session_policy: swage_storage::model::SessionPolicy::Continuation { session_id },
                creator: swage_storage::model::ScheduleCreator::User,
                delivery_routing: serde_json::json!({}),
                gate: None,
                next_run_at: None,
                last_run_at: None,
            })
            .await
            .unwrap();
        let run = ScheduleRunAttribution {
            run_id: storage
                .create_schedule_run(swage_storage::model::NewScheduleRun::pending(
                    schedule.id,
                    swage_storage::model::ScheduleRunTrigger::Tick,
                    None,
                ))
                .await
                .unwrap()
                .id,
            schedule_id: schedule.id,
        };
        ThreadStore::create(
            &*storage,
            swage_storage::model::NewThread {
                session_id,
                agent_id: "gateway".into(),
            },
        )
        .await
        .unwrap();
        let turn_id =
            crate::automated_turn::create_placeholder_turn(&state, session_id, "gateway", "run")
                .await
                .unwrap();
        deliver_to_targets(
            &state,
            session_id,
            DeliveryAttribution {
                schedule_run: Some(run),
                turn_id: Some(turn_id),
            },
            &targets,
            "# The run's output",
        )
        .await
        .unwrap();
        deliver_to_targets(
            &state,
            session_id,
            DeliveryAttribution {
                schedule_run: None,
                turn_id: Some(turn_id),
            },
            &targets,
            "# A routed response",
        )
        .await
        .unwrap();

        let modules = storage.list_surface_modules(surface.id).await.unwrap();
        assert_eq!(modules.len(), 2);
        assert!(
            matches!(
                modules[0].delivery,
                Some(ModuleDelivery::ScheduleRun { run_id, schedule_id, .. })
                    if run_id == run.run_id && schedule_id == run.schedule_id
            ),
            "{:?}",
            modules[0].delivery
        );
        assert!(
            matches!(
                modules[1].delivery,
                Some(ModuleDelivery::Turn { turn_id: by, .. }) if by == turn_id
            ),
            "{:?}",
            modules[1].delivery
        );
    }

    #[tokio::test]
    async fn webhook_client_enforces_its_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // Accept the request but never produce an HTTP response. This
            // isolates the client's request deadline from connection errors.
            let (_connection, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let client = webhook_client(Duration::from_millis(25)).unwrap();

        let error = client
            .post(format!("http://{address}"))
            .body("payload")
            .send()
            .await
            .expect_err("a stalled webhook must time out");

        assert!(error.is_timeout(), "unexpected request error: {error}");
        server.abort();
    }

    #[tokio::test]
    async fn channel_turn_receives_provider_visible_snapshot() {
        let (state, session_id) = make_state_with_session(vec![
            StreamChunk::Delta {
                content: "Channel answer".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ])
        .await;

        let answered = run_channel_turn(&state, session_id, "Channel hello".into())
            .await
            .expect("channel turn should complete");

        assert_eq!(answered.response, "Channel answer");
        assert!(
            answered.turn_id.is_some(),
            "the answer names the turn that produced it"
        );
        let thread = state
            .storage
            .list_by_session(session_id)
            .await
            .expect("list session threads")
            .into_iter()
            .next()
            .expect("session should have a thread");
        let turn = state
            .storage
            .list_turns_by_thread(thread.id)
            .await
            .expect("list turns")
            .into_iter()
            .next()
            .expect("channel turn should be persisted");
        let snapshot = state
            .storage
            .get_snapshot(turn.id)
            .await
            .expect("get snapshot")
            .expect("channel turn should receive context snapshot");
        let parsed: serde_json::Value =
            serde_json::from_str(&snapshot).expect("snapshot should be JSON");
        let provider_request = &parsed["provider_visible"]["requests"][0];
        assert_eq!(provider_request["round"], serde_json::json!(1));
        assert!(
            provider_request["request"]["body"]["messages"].is_array(),
            "channel turn snapshot should include provider-visible messages"
        );
    }
}
