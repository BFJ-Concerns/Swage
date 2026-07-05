//! Delivery routing: where a run's output goes.
//!
//! These types predate the protocol crate — they moved here from the
//! gateway because the schedule contract embeds them. The gateway
//! re-exports them, so existing call sites are unaffected.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// An external channel integration that can receive delivered output.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExternalChannelIntegration {
    DevDm,
    DevThread,
    Telegram,
}

impl ExternalChannelIntegration {
    /// The stable string form used in session metadata and channel links.
    pub fn as_metadata_value(&self) -> &'static str {
        match self {
            Self::DevDm => "dev_dm",
            Self::DevThread => "dev_thread",
            Self::Telegram => "telegram",
        }
    }
}

impl std::str::FromStr for ExternalChannelIntegration {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dev_dm" => Ok(Self::DevDm),
            "dev_thread" => Ok(Self::DevThread),
            "telegram" => Ok(Self::Telegram),
            other => Err(format!("unknown channel integration: {other}")),
        }
    }
}

/// A destination for a run's output.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS, JsonSchema)]
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

/// Where output should be routed once a run completes.
///
/// `deliver_to_origin` defaults to true for channel-initiated turns, where
/// an origin always exists. Callers without an origin (schedules, manual
/// API calls) must set it false or target resolution errors.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
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

impl DeliveryRoutingRequest {
    /// Routing for runs with no origin: keep the output as a local record.
    pub fn local_only() -> Self {
        Self {
            deliver_to_origin: false,
            home_target: None,
            explicit_targets: Vec::new(),
            store_local_output: true,
        }
    }
}

fn default_true() -> bool {
    true
}
