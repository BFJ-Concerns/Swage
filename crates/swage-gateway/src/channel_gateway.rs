//! External communication channel gateway.
//!
//! Canonical model:
//! - `origin` identifies where an inbound message came from.
//! - Each origin has a `home target`, which is where replies go by default.
//! - Callers may supply an explicit `delivery_target` override to send the
//!   final response somewhere else without changing the session routing key.
//!
//! The initial integrations are intentionally narrow dev adapters:
//! - `dev_dm` models direct-message routing keyed by `(account, user_id)`.
//! - `dev_thread` models thread-or-topic routing keyed by
//!   `(account, channel_id/thread_id)`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use swage_core::{GatewayError, Role};
use swage_storage::model::NewSession;

use crate::state::AppState;
use crate::subscription::SubscriptionState;
use crate::thread_selection::resolve_thread_for_session;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalChannelIntegration {
    DevDm,
    DevThread,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeliveryTarget {
    pub integration: ExternalChannelIntegration,
    pub account: String,
    pub conversation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelOrigin {
    pub integration: ExternalChannelIntegration,
    pub account: String,
    pub conversation: String,
}

impl ChannelOrigin {
    pub fn home_target(&self) -> DeliveryTarget {
        DeliveryTarget {
            integration: self.integration.clone(),
            account: self.account.clone(),
            conversation: self.conversation.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveredChannelMessage {
    pub session_id: Uuid,
    pub target: DeliveryTarget,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelInboundResponse {
    pub session_id: Uuid,
    pub delivered_to: DeliveryTarget,
    pub response: String,
}

#[derive(Debug, Deserialize)]
pub struct DevDmInboundRequest {
    pub account: String,
    pub user_id: String,
    pub text: String,
    #[serde(default)]
    pub delivery_target: Option<DeliveryTarget>,
}

#[derive(Debug, Deserialize)]
pub struct DevThreadInboundRequest {
    pub account: String,
    pub channel_id: String,
    pub thread_id: String,
    pub text: String,
    #[serde(default)]
    pub delivery_target: Option<DeliveryTarget>,
}

#[derive(Debug, Deserialize)]
pub struct OutboxQuery {
    pub integration: ExternalChannelIntegration,
    pub account: String,
    pub conversation: String,
}

#[derive(Debug, Serialize)]
pub struct OutboxResponse {
    pub deliveries: Vec<DeliveredChannelMessage>,
}

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
        request.delivery_target,
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
        request.delivery_target,
    )
    .await
}

pub async fn read_outbox(state: &AppState, query: OutboxQuery) -> OutboxResponse {
    let deliveries = state.external_channel_outbox.lock().await;
    OutboxResponse {
        deliveries: deliveries
            .iter()
            .filter(|delivery| {
                delivery.target.integration == query.integration
                    && delivery.target.account == query.account
                    && delivery.target.conversation == query.conversation
            })
            .cloned()
            .collect(),
    }
}

async fn handle_inbound(
    state: &AppState,
    origin: ChannelOrigin,
    text: String,
    delivery_target: Option<DeliveryTarget>,
) -> Result<ChannelInboundResponse, GatewayError> {
    let session_id = ensure_session_for_origin(state, &origin).await?;
    let response = run_channel_turn(state, session_id, text).await?;
    let target = delivery_target.unwrap_or_else(|| origin.home_target());
    record_delivery(state, session_id, target.clone(), response.clone()).await;

    Ok(ChannelInboundResponse {
        session_id,
        delivered_to: target,
        response,
    })
}

async fn ensure_session_for_origin(
    state: &AppState,
    origin: &ChannelOrigin,
) -> Result<Uuid, GatewayError> {
    if let Some(existing) = state.channel_routes.lock().await.get(origin).copied() {
        if state.storage.get_session(existing).await?.is_some() {
            return Ok(existing);
        }
    }

    let project = state.project_store.get_or_create_default().await?;
    let session = state
        .storage
        .create_session(NewSession {
            project_id: project.id,
            agent_id: "gateway".to_string(),
            metadata: Some(serde_json::json!({
                "channel_origin": {
                    "integration": origin.integration,
                    "account": origin.account,
                    "conversation": origin.conversation
                }
            })),
        })
        .await?;
    state
        .storage
        .create(swage_storage::model::NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        })
        .await?;

    state
        .channel_routes
        .lock()
        .await
        .insert(origin.clone(), session.id);

    Ok(session.id)
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

    crate::turn::process_turn_with_existing_turn(
        state,
        session_id,
        Some(thread.id),
        text,
        ws_tx,
        subscription,
        None,
        None,
    )
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

async fn record_delivery(
    state: &AppState,
    session_id: Uuid,
    target: DeliveryTarget,
    content: String,
) {
    state
        .external_channel_outbox
        .lock()
        .await
        .push(DeliveredChannelMessage {
            session_id,
            target,
            content,
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_target_defaults_to_origin() {
        let origin = ChannelOrigin {
            integration: ExternalChannelIntegration::DevThread,
            account: "ops".into(),
            conversation: "room/thread".into(),
        };

        assert_eq!(
            origin.home_target(),
            DeliveryTarget {
                integration: ExternalChannelIntegration::DevThread,
                account: "ops".into(),
                conversation: "room/thread".into(),
            }
        );
    }
}
