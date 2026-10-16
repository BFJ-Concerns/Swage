//! Installed plugins as the settings surface lists and manages them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// A tool a plugin provides, as the model sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProvidedTool {
    pub name: String,
    pub description: String,
}

/// What a plugin provides, by kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct PluginProvides {
    pub tools: Vec<ProvidedTool>,
}

/// A secret a plugin declares it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct PluginSecret {
    pub name: String,
    pub description: String,
}

/// One installed revision of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct PluginRevision {
    pub revision: u32,
    pub version: String,
    /// Where the package was installed from.
    pub installed_from: String,
    pub installed_at: String,
    /// Whether this is the revision serving now.
    pub active: bool,
}

/// Whether the plugin's isolate is up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PluginServiceState {
    /// The isolate serves calls.
    Serving,
    /// The last call ended the isolate at a limit; the next call rebuilds it.
    Terminated { reason: String },
    /// The script could not be loaded; nothing serves until a revision that
    /// loads is installed.
    Failed { reason: String },
}

/// An installed plugin with everything the settings surface shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PluginSummary {
    pub name: String,
    pub description: String,
    /// The active revision's version.
    pub version: String,
    pub revision: u32,
    pub provides: PluginProvides,
    /// The host grants the plugin asks for.
    pub grants: Vec<String>,
    pub secrets: Vec<PluginSecret>,
    /// The function this plugin is a variant of, when it declares one.
    pub variant_of: Option<String>,
    /// Every recorded revision, oldest first.
    pub revisions: Vec<PluginRevision>,
    /// Whether the install enables it everywhere.
    pub enabled_for_install: bool,
    /// Topics that enable it on their own.
    pub enabled_topics: Vec<Uuid>,
    /// Topics that disable it on their own, over the install's answer.
    pub disabled_topics: Vec<Uuid>,
    pub service: PluginServiceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct PluginListResponse {
    pub plugins: Vec<PluginSummary>,
}

/// Install a plugin package from a directory on the gateway's host. A
/// package already installed under its name becomes a new revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct InstallPluginRequest {
    /// The package directory: the one holding `plugin.toml`.
    pub source_path: String,
}

/// Enable or disable a plugin for the install, or for one topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct PluginEnablementRequest {
    /// The topic whose answer this is; absent means the install's.
    pub topic_id: Option<Uuid>,
    pub enabled: bool,
}
