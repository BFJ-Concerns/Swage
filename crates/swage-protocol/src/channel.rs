//! Contract exposed to external channel integrations.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::client::delivery::{DeliveryRoutingRequest, DeliveryTarget};

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct RecordedDelivery {
    pub session_id: Uuid,
    pub run_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub target: DeliveryTarget,
    pub content: String,
    pub delivered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ChannelInboundResponse {
    pub session_id: Uuid,
    pub delivered_to: Vec<DeliveryTarget>,
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DevDmInboundRequest {
    pub account: String,
    pub user_id: String,
    pub text: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub routing: DeliveryRoutingRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DevThreadInboundRequest {
    pub account: String,
    pub channel_id: String,
    pub thread_id: String,
    pub text: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub routing: DeliveryRoutingRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DeliveryLogQuery {
    pub session_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DeliveryLogResponse {
    pub deliveries: Vec<RecordedDelivery>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ChannelStatusResponse {
    pub integration: String,
    pub status: String,
    pub route_count: usize,
    pub delivery_count: usize,
    pub last_delivery_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ChannelStatusListResponse {
    pub channels: Vec<ChannelStatusResponse>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct ChannelSurface {
    dm_inbound: DevDmInboundRequest,
    thread_inbound: DevThreadInboundRequest,
    inbound_response: ChannelInboundResponse,
    delivery_log_query: DeliveryLogQuery,
    delivery_log: DeliveryLogResponse,
    status: ChannelStatusListResponse,
}
