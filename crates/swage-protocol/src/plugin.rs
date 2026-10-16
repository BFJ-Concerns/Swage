//! The contract a brokered plugin process speaks with the gateway.
//!
//! A plugin whose manifest declares a `[process]` runs as a process of its
//! own: the gateway starts it on a session's first call to one of the
//! plugin's tools, keeps one per session, and stops it when the session
//! closes, after the idle period the manifest declares, or when the gateway
//! stops. The gateway connects the process over the Unix socket named by
//! [`PLUGIN_SOCKET_ENV`], and the two sides exchange one JSON object per
//! line. The process answers the tool calls the gateway sends and reaches
//! the platform only by asking the gateway: a host call carries the same
//! name and payload the plugin isolate's door takes, and is answered under
//! the same grants.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// The environment variable that names the socket the process connects to.
pub const PLUGIN_SOCKET_ENV: &str = "SWAGE_PLUGIN_SOCKET";

/// Where a tool call comes from, as the plugin's handler sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields = nullable)]
pub struct PluginCallContext {
    pub session_id: String,
    pub agent_id: String,
    pub call_id: Option<String>,
    pub working_directory: String,
}

/// Why the gateway is stopping the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginProcessStopReason {
    /// The session the process served has closed.
    SessionClosed,
    /// Nothing called the process for the manifest's idle period.
    Idle,
    /// The gateway is stopping.
    GatewayStopping,
    /// A newer revision of the plugin was installed.
    Superseded,
}

/// A host call's refusal or failure, by name and message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PluginHostCallError {
    pub name: String,
    pub message: String,
}

/// The gateway's answer to a host call: a value, or an error by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PluginHostReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginHostCallError>,
}

/// What the gateway sends the process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginProcessRequest {
    /// Run a tool the plugin provides; the process answers with a `result`
    /// or a `failure` carrying the same `id`.
    Invoke {
        id: String,
        tool: String,
        #[ts(type = "unknown")]
        params: Value,
        context: PluginCallContext,
    },
    /// The answer to a host call the process made, by the call's `id`.
    Reply { id: String, reply: PluginHostReply },
    /// The process is being stopped and has the gateway's grace period to
    /// exit on its own; after it the process is signalled, and killed a
    /// moment later if it ignores that.
    Stop { reason: PluginProcessStopReason },
}

/// What the process sends the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginProcessMessage {
    /// The process's first message: the tools it serves. A tool the
    /// manifest declares that is missing here fails the start.
    Ready { tools: Vec<String> },
    /// The invocation `id` succeeded with `value`.
    Result {
        id: String,
        #[ts(type = "unknown")]
        value: Value,
    },
    /// The invocation `id` failed.
    Failure {
        id: String,
        name: String,
        message: String,
    },
    /// A host call made while serving `invocation`: `name` and `payload`
    /// as the plugin isolate's door takes them (`callTool` with
    /// `{ "name", "arguments" }`, `log` with a string). The gateway
    /// answers with a `reply` carrying the same `id`.
    Call {
        id: String,
        invocation: String,
        name: String,
        #[ts(type = "unknown")]
        payload: Value,
    },
}

#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct PluginProcessSurface {
    request: PluginProcessRequest,
    message: PluginProcessMessage,
}
