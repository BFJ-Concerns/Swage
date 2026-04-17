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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryTarget {
    Channel {
        integration: ExternalChannelIntegration,
        account: String,
        conversation: String,
    },
    LocalStorage,
    Webhook {
        url: String,
    },
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedDelivery {
    pub session_id: Uuid,
    pub target: DeliveryTarget,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelInboundResponse {
    pub session_id: Uuid,
    pub delivered_to: Vec<DeliveryTarget>,
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRoutingRequest {
    #[serde(default = "default_true")]
    pub deliver_to_origin: bool,
    #[serde(default)]
    pub home_target: Option<DeliveryTarget>,
    #[serde(default)]
    pub explicit_targets: Vec<DeliveryTarget>,
    #[serde(default)]
    pub store_local_output: bool,
}

impl Default for DeliveryRoutingRequest {
    fn default() -> Self {
        Self {
            deliver_to_origin: true,
            home_target: None,
            explicit_targets: Vec::new(),
            store_local_output: false,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DevDmInboundRequest {
    pub account: String,
    pub user_id: String,
    pub text: String,
    #[serde(default)]
    pub routing: DeliveryRoutingRequest,
}

#[derive(Debug, Deserialize)]
pub struct DevThreadInboundRequest {
    pub account: String,
    pub channel_id: String,
    pub thread_id: String,
    pub text: String,
    #[serde(default)]
    pub routing: DeliveryRoutingRequest,
}

#[derive(Debug, Deserialize)]
pub struct DeliveryLogQuery {
    pub session_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct DeliveryLogResponse {
    pub deliveries: Vec<RecordedDelivery>,
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
    let deliveries = state.delivery_log.lock().await;
    DeliveryLogResponse {
        deliveries: deliveries
            .iter()
            .filter(|delivery| delivery.session_id == query.session_id)
            .cloned()
            .collect(),
    }
}

async fn handle_inbound(
    state: &AppState,
    origin: ChannelOrigin,
    text: String,
    routing: DeliveryRoutingRequest,
) -> Result<ChannelInboundResponse, GatewayError> {
    let session_id = ensure_session_for_origin(state, &origin).await?;
    let response = run_channel_turn(state, session_id, text).await?;
    let targets = resolve_delivery_targets(Some(&origin), routing)?;
    deliver_to_targets(state, session_id, &targets, &response).await?;

    Ok(ChannelInboundResponse {
        session_id,
        delivered_to: targets,
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
    targets: &[DeliveryTarget],
    content: &str,
) -> Result<(), GatewayError> {
    for target in targets {
        deliver_to_target(session_id, target, content).await?;
        record_delivery(state, session_id, target.clone(), content.to_string()).await;
    }

    Ok(())
}

async fn deliver_to_target(
    session_id: Uuid,
    target: &DeliveryTarget,
    content: &str,
) -> Result<(), GatewayError> {
    match target {
        DeliveryTarget::Channel { .. } | DeliveryTarget::LocalStorage => Ok(()),
        DeliveryTarget::Webhook { url } => {
            let parsed = reqwest::Url::parse(url).map_err(|error| {
                GatewayError::ValidationError(format!("invalid webhook target URL: {error}"))
            })?;
            let response = reqwest::Client::new()
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

            Ok(())
        }
    }
}

async fn record_delivery(
    state: &AppState,
    session_id: Uuid,
    target: DeliveryTarget,
    content: String,
) {
    state.delivery_log.lock().await.push(RecordedDelivery {
        session_id,
        target,
        content,
    });
}

fn default_true() -> bool {
    true
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
            DeliveryTarget::Channel {
                integration: ExternalChannelIntegration::DevThread,
                account: "ops".into(),
                conversation: "room/thread".into(),
            }
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
}
