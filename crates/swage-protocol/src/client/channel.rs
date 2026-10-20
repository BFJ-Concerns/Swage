//! Operator-facing channel onboarding types.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::delivery::{DeliveryTarget, ExternalChannelIntegration};

/// Request a short-lived code that links a channel conversation to a topic.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct CreateChannelLinkCodeRequest {
    pub integration: ExternalChannelIntegration,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub topic_id: Option<Uuid>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub agent_id: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub home_target: Option<DeliveryTarget>,
}

/// A short-lived, single-use channel link code.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ChannelLinkCodeResponse {
    pub integration: String,
    pub link_code: String,
    pub expires_at: DateTime<Utc>,
}

/// A channel conversation linked to a topic and agent.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ChannelLinkResponse {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub topic_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    #[ts(type = "unknown")]
    pub home_target: Option<serde_json::Value>,
    pub linked_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// All linked channel conversations.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ChannelLinkListResponse {
    pub links: Vec<ChannelLinkResponse>,
}

/// The readable, unredeemed codes, represented on the wire as an array.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ChannelLinkCodeListResponse(pub Vec<ChannelLinkCodeResponse>);
