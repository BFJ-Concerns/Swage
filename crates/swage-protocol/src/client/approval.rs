//! Human approval values shared by HTTP, WebSocket, and agent consumers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;
use ts_rs::TS;

/// The outcome and scope of a human approval decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ApprovalDecision {
    pub decision: ApprovalDecisionValue,
    pub scope: ApprovalDecisionScope,
    /// Optional human-readable rationale from the reviewer.
    pub reason: Option<String>,
}

impl ApprovalDecision {
    pub fn is_approved(&self) -> bool {
        self.decision == ApprovalDecisionValue::Approved
    }
}

/// How widely an approved decision should apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionScope {
    #[default]
    Once,
    Session,
    Permanent,
    PermanentGlobal,
}

impl ApprovalDecisionScope {
    pub fn is_once(&self) -> bool {
        *self == Self::Once
    }
}

/// The human verdict on a pending approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionValue {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown approval decision: {value}")]
pub struct ApprovalDecisionParseError {
    value: String,
}

impl ApprovalDecisionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

impl std::fmt::Display for ApprovalDecisionValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ApprovalDecisionValue {
    type Err = ApprovalDecisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            other => Err(ApprovalDecisionParseError {
                value: other.to_string(),
            }),
        }
    }
}
