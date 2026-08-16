//! Gateway settings, provider discovery, and restart HTTP contracts.

use std::collections::HashMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProtocol {
    OpenAi,
    Anthropic,
    Google,
    OpenRouter,
    Codex,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct ProviderSettings {
    pub protocol: ProviderProtocol,
    pub base_url: Option<String>,
    pub api_key_secret_ref: Option<String>,
    pub api_key_env: Option<String>,
    pub has_credential: bool,
    pub command: Option<String>,
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct SaveProviderSettings {
    pub protocol: ProviderProtocol,
    pub base_url: Option<String>,
    pub api_key_secret_ref: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub api_key: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilitiesConfig {
    #[serde(default)]
    pub reasoning_replay: bool,
}
impl ModelCapabilitiesConfig {
    pub fn is_empty(&self) -> bool {
        !self.reasoning_replay
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ModelSettings {
    pub provider_id: String,
    pub model_id: String,
    pub aliases: Vec<String>,
    pub context_window: Option<u32>,
    pub generation_preset: Option<String>,
    #[serde(default)]
    pub capabilities: ModelCapabilitiesConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GenerationPresetConfig {
    #[ts(optional)]
    pub max_tokens: Option<u32>,
    #[ts(optional)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    #[ts(optional, type = "unknown")]
    pub metadata: Option<serde_json::Value>,
}
impl GenerationPresetConfig {
    pub fn is_empty(&self) -> bool {
        self.max_tokens.is_none()
            && self.temperature.is_none()
            && self.stop_sequences.is_empty()
            && self.metadata.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum PythonRuntimePolicy {
    BundledThenSystem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct PythonExecuteToolConfig {
    pub enabled: bool,
    #[ts(type = "number")]
    pub timeout_secs: u64,
    #[ts(type = "number")]
    pub startup_timeout_secs: u64,
    #[ts(type = "number")]
    pub idle_ttl_secs: u64,
    #[ts(type = "number")]
    pub warm_target: usize,
    #[ts(type = "number")]
    pub max_workers: usize,
    pub runtime_policy: PythonRuntimePolicy,
    pub runtime_path: Option<String>,
    pub require_sandbox: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SearchProviderConfig {
    pub provider_type: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct ToolRuntimeSettings {
    #[ts(type = "number")]
    pub shell_timeout_secs: u64,
    pub python_execute: PythonExecuteToolConfig,
    #[ts(type = "number")]
    pub max_output_bytes: usize,
    pub plugin_directory: Option<String>,
    pub search: Option<SearchProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalTimeoutPolicy {
    Reject,
    Interrupt,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(untagged)]
pub enum RequireApprovalEntry {
    Simple(String),
    WithOptions {
        tool: String,
        #[ts(type = "number | null")]
        timeout: Option<u64>,
        timeout_policy: Option<ApprovalTimeoutPolicy>,
    },
}
impl RequireApprovalEntry {
    pub fn tool_name(&self) -> &str {
        match self {
            Self::Simple(name) => name,
            Self::WithOptions { tool, .. } => tool,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolPermissionConfig {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub bundles: HashMap<String, Vec<String>>,
    pub require_approval: Vec<RequireApprovalEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct HitlConfig {
    #[ts(type = "number")]
    pub timeout: u64,
    pub timeout_policy: ApprovalTimeoutPolicy,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub allow_degraded_execution: bool,
}
impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_degraded_execution: false,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    pub assembly: String,
    #[ts(type = "number")]
    pub timeout: u64,
    #[ts(type = "number | null")]
    pub budget: Option<usize>,
}
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            assembly: "default".to_string(),
            timeout: 5,
            budget: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ModelPricing {
    #[ts(type = "number")]
    pub input_price_per_mtok: i64,
    #[ts(type = "number")]
    pub output_price_per_mtok: i64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct PricingConfig {
    pub models: HashMap<String, ModelPricing>,
}
impl PricingConfig {
    pub fn compute_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> Option<i64> {
        let pricing = self.models.get(model)?;
        Some(
            (input_tokens as i64) * pricing.input_price_per_mtok / 1_000_000
                + (output_tokens as i64) * pricing.output_price_per_mtok / 1_000_000,
        )
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityConfig {
    pub force_on: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContextTransferMode {
    None,
    Explicit,
    Summary,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContextTransferField {
    System,
    User,
    Assistant,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct ContextTransferTargetConfig {
    pub mode: ContextTransferMode,
    pub allowed_fields: Vec<ContextTransferField>,
    pub summary_budget: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct ContextTransferConfig {
    pub mode: ContextTransferMode,
    pub allowed_fields: Vec<ContextTransferField>,
    pub summary_budget: Option<u32>,
    pub targets: HashMap<String, ContextTransferTargetConfig>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CommunicationConfig {
    pub context_transfer: ContextTransferConfig,
    #[ts(type = "number")]
    pub message_ttl_secs: u64,
    #[ts(type = "number")]
    pub expiry_interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct SettingsResponse {
    pub providers: HashMap<String, ProviderSettings>,
    pub default_provider: String,
    pub models: HashMap<String, ModelSettings>,
    pub default_model: String,
    pub generation_presets: HashMap<String, GenerationPresetConfig>,
    pub tool_runtime: ToolRuntimeSettings,
    pub tool_permissions: ToolPermissionConfig,
    pub hitl: HitlConfig,
    pub sandbox: SandboxConfig,
    pub context: ContextConfig,
    pub pricing: PricingConfig,
    pub observability: ObservabilityConfig,
    pub communication: CommunicationConfig,
    pub restart_required: bool,
    pub can_restart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct SaveSettingsRequest {
    pub default_provider: String,
    pub providers: HashMap<String, SaveProviderSettings>,
    pub default_model: String,
    pub models: HashMap<String, ModelSettings>,
    #[serde(default)]
    pub generation_presets: HashMap<String, GenerationPresetConfig>,
    #[serde(default)]
    pub tool_runtime: Option<ToolRuntimeSettings>,
    #[serde(default)]
    pub tool_permissions: Option<ToolPermissionConfig>,
    #[serde(default)]
    pub hitl: Option<HitlConfig>,
    #[serde(default)]
    pub sandbox: Option<SandboxConfig>,
    #[serde(default)]
    pub context: Option<ContextConfig>,
    #[serde(default)]
    pub pricing: Option<PricingConfig>,
    #[serde(default)]
    pub observability: Option<ObservabilityConfig>,
    #[serde(default)]
    pub communication: Option<CommunicationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: String,
    pub context_window: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct ModelDiscoveryResponse {
    pub provider_id: String,
    pub models: Vec<DiscoveredModel>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct RestartStatusResponse {
    pub restart_required: bool,
    pub can_restart: bool,
    pub guidance: String,
}
