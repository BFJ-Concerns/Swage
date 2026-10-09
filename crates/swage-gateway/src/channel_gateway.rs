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
use swage_protocol::client::surface::WriteSurfaceModuleRequest;

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
    let response = run_channel_turn(state, session_id, text).await?;
    let targets = deliver_channel_response(
        state,
        session_id,
        DeliveryAttribution::default(),
        &origin,
        routing,
        &response,
    )
    .await?;

    Ok(ChannelInboundResponse {
        session_id,
        delivered_to: targets,
        response,
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
    let origins = match state.storage.list_pending_channel_origins().await {
        Ok(origins) => origins,
        Err(error) => {
            return Err(GatewayError::Storage(error));
        }
    };

    let mut first_error = None;
    for pending in origins {
        if let Err(error) = drain_channel_origin(state, pending).await
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

async fn drain_channel_origin(
    state: &AppState,
    pending: PendingChannelOrigin,
) -> Result<(), GatewayError> {
    let origin = channel_origin(&pending)?;
    let rows = state.storage.list_pending_by_origin(pending).await?;
    if rows.is_empty() {
        return Ok(());
    }
    let row_ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();

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
                .discard_channel_messages(row_ids, "unlinked")
                .await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    let busy = state.active_turns.lock().await.contains_key(&session_id);
    let session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("linked session {session_id} no longer exists"))
        })?;
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    let input = compose_channel_messages(&rows);
    if busy || crate::turn_queue::has_queued_input(state, session_id).await? {
        return queue_channel_rows(state, session_id, thread.id, rows).await;
    }

    let turn = match state
        .storage
        .begin_channel_turn(thread.id, &session.agent_id, &input, row_ids)
        .await
    {
        Ok(turn) => turn,
        Err(swage_core::StorageError::ConstraintViolation(_)) => {
            return queue_channel_rows(state, session_id, thread.id, rows).await;
        }
        Err(error) => return Err(GatewayError::Storage(error)),
    };

    let response =
        crate::automated_turn::run_placeholder_turn(state, session_id, input.as_str(), turn.id)
            .await?;
    deliver_channel_response(
        state,
        session_id,
        DeliveryAttribution {
            run_id: None,
            turn_id: Some(turn.id),
        },
        &origin,
        DeliveryRoutingRequest::default(),
        &response,
    )
    .await?;
    Ok(())
}

async fn queue_channel_rows(
    state: &AppState,
    session_id: Uuid,
    thread_id: Uuid,
    rows: Vec<ChannelInboundMessage>,
) -> Result<(), GatewayError> {
    for row in rows {
        match crate::turn_queue::queue_input_with_attribution(
            state,
            session_id,
            thread_id,
            crate::turn::TurnInput::text(&row.content),
            None,
            vec![row.id],
            crate::turn::TurnAttendance::Unattended,
        )
        .await
        {
            Ok(_) => {}
            // Another inbox reader already handed this input to the queue.
            Err(
                error @ GatewayError::Storage(swage_core::StorageError::ConstraintViolation(_)),
            ) => {
                let queued = state.storage.list_queued_turn_inputs(session_id).await?;
                if !queued
                    .iter()
                    .any(|input| input.channel_message_ids.contains(&row.id))
                {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Deliver the shared turn's reply to each originating chat represented in it.
pub(crate) async fn deliver_queued_response(
    state: &AppState,
    session_id: Uuid,
    turn: &swage_storage::Turn,
    pending: PendingChannelOrigin,
) -> Result<(), GatewayError> {
    let origin = channel_origin(&pending)?;
    deliver_channel_response(
        state,
        session_id,
        DeliveryAttribution {
            run_id: None,
            turn_id: Some(turn.id),
        },
        &origin,
        DeliveryRoutingRequest::default(),
        turn.assistant_response.as_deref().unwrap_or_default(),
    )
    .await?;
    Ok(())
}

fn compose_channel_messages(rows: &[ChannelInboundMessage]) -> String {
    rows.iter()
        .map(|row| row.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

async fn run_channel_turn(
    state: &AppState,
    session_id: Uuid,
    text: String,
) -> Result<String, GatewayError> {
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
        .map(|message| message.content)
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

fn push_unique_target(targets: &mut Vec<DeliveryTarget>, target: DeliveryTarget) {
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
            match deliver_to_target(state, session_id, target, content).await {
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
                run_id = ?attribution.run_id,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct DeliveryAttribution {
    pub run_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
}

enum DeliveryOutcome {
    Sent,
    NotSent,
}

async fn deliver_to_target(
    state: &AppState,
    session_id: Uuid,
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
            run_id: attribution.run_id,
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
        AuditStore, ChannelLinkStore, SessionStore, SurfaceStore, ThreadStore, TopicStore,
    };
    use tokio::net::TcpListener;

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
        let state = AppState::new(
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
        let state = AppState::new(
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

    #[test]
    fn compose_channel_messages_separates_queued_updates() {
        let received_at = chrono::Utc::now();
        let rows = vec![
            ChannelInboundMessage {
                id: Uuid::new_v4(),
                integration: "telegram".into(),
                account: "bot-1".into(),
                conversation: "chat-1".into(),
                external_id: "1".into(),
                sender: "chat-1".into(),
                content: "first".into(),
                state: swage_storage::model::ChannelInboundMessageState::Pending,
                discard_reason: None,
                turn_id: None,
                received_at,
                presented_at: None,
            },
            ChannelInboundMessage {
                id: Uuid::new_v4(),
                integration: "telegram".into(),
                account: "bot-1".into(),
                conversation: "chat-1".into(),
                external_id: "2".into(),
                sender: "chat-1".into(),
                content: "second".into(),
                state: swage_storage::model::ChannelInboundMessageState::Pending,
                discard_reason: None,
                turn_id: None,
                received_at,
                presented_at: None,
            },
        ];

        assert_eq!(compose_channel_messages(&rows[..1]), "first");
        assert_eq!(compose_channel_messages(&rows), "first\n\nsecond");
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

        let response = run_channel_turn(&state, session_id, "Channel hello".into())
            .await
            .expect("channel turn should complete");

        assert_eq!(response, "Channel answer");
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
