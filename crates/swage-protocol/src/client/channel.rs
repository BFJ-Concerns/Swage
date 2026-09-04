//! Operator-facing channel onboarding types.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::delivery::{DeliveryTarget, ExternalChannelIntegration};

/// Request a short-lived code that links a channel conversation to a space.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct CreateChannelLinkCodeRequest {
    pub integration: ExternalChannelIntegration,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub space_id: Option<Uuid>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub agent_id: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub home_target: Option<DeliveryTarget>,
}

/// A short-lived, single-use channel link code.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ChannelLinkCodeResponse {
    pub link_code: String,
    pub expires_at: DateTime<Utc>,
}
