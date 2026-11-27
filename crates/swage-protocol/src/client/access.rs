//! Pairing a client with a gateway bound beyond loopback (C41, C69).
//!
//! A gateway listening on a private network address admits a request only
//! with its token. A client gets the token once, by redeeming a one-time
//! pairing code the operator issued from the gateway's own machine, and
//! remembers it: the web UI as a cookie the gateway sets on pairing, the
//! CLI and desktop app as a stored token they send on every request.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How the gateway gates requests from beyond loopback.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum AccessGateMode {
    /// A request without the token is refused. The shipped default.
    #[default]
    Token,
    /// A request without the token is admitted and logged.
    Warn,
    /// Requests are admitted without a token.
    Off,
}

/// A one-time pairing code, issued from the gateway's own machine.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct PairingCodeResponse {
    pub pairing_code: String,
    pub expires_at: DateTime<Utc>,
}

/// Redeem a pairing code for the gateway's token.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct PairClientRequest {
    pub pairing_code: String,
}

/// The paired client's token. The gateway also sets it as the `swage_token`
/// cookie, so a browser's later requests and WebSocket connections carry it
/// without the page keeping it anywhere.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct PairClientResponse {
    pub token: String,
}

/// Whether the connection the request arrived on is admitted, and why: the
/// web UI reads it to decide whether to ask for a pairing code.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct AccessStatusResponse {
    /// Whether the gate applies to this gateway's binding at all.
    pub gated: bool,
    pub mode: AccessGateMode,
    /// Whether this request carried the token, or came from loopback.
    pub admitted: bool,
}
