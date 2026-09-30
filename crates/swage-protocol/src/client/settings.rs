//! Gateway settings, provider discovery, and restart HTTP contracts.

use std::collections::HashMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Reasoning effort as the user means it: one provider-neutral intent,
/// ordered lowest to highest, resolved per model against that model's
/// declared [`EffortCapability`] before anything reaches a provider.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// No reasoning at all — a true disable where the model has one.
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Effort; 7] = [
        Effort::Off,
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    /// The wire spelling, which is also the vocabulary users see.
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a model's provider actually accepts as an effort control, declared
/// per model in the catalogue or supplied by the provider adapter for the
/// models it serves. The declaration is bound to the provider's documented
/// control: it must admit every level the control accepts and must not
/// collapse levels the control distinguishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "control", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum EffortCapability {
    /// The control takes a named level from a fixed set. `can_disable`
    /// says whether the model can switch reasoning off entirely.
    Levels {
        /// The levels the control accepts, in any order; never `off`,
        /// which is expressed through `can_disable`.
        levels: Vec<Effort>,
        can_disable: bool,
    },
    /// The control takes only a numeric thinking budget, in tokens.
    Budget {
        min_tokens: u32,
        max_tokens: u32,
        can_disable: bool,
    },
    /// The model has no effort control; every intent is omitted.
    None,
}

impl EffortCapability {
    /// A declaration that could not be honoured as written, named so the
    /// catalogue can refuse it at load rather than resolve nonsense.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            EffortCapability::Levels { levels, .. } => {
                if levels.is_empty() {
                    return Err("an effort capability with named levels needs at least one".into());
                }
                if levels.contains(&Effort::Off) {
                    return Err(
                        "`off` is not a level; declare it through can_disable instead".into(),
                    );
                }
                let mut seen = std::collections::BTreeSet::new();
                for level in levels {
                    if !seen.insert(*level) {
                        return Err(format!("effort level `{level}` is declared twice"));
                    }
                }
                Ok(())
            }
            EffortCapability::Budget {
                min_tokens,
                max_tokens,
                ..
            } => {
                if min_tokens > max_tokens {
                    return Err(format!(
                        "effort budget range is inverted: min {min_tokens} exceeds max {max_tokens}"
                    ));
                }
                Ok(())
            }
            EffortCapability::None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProtocol {
    OpenAi,
    /// The OpenAI Responses API — the envelope GPT-family models are
    /// addressed with by their native harness.
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    Anthropic,
    Google,
    OpenRouter,
    Codex,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct ProviderSettings {
    pub protocol: ProviderProtocol,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub worker_staleness_threshold_ms: Option<u64>,
    pub base_url: Option<String>,
    pub api_key_secret_ref: Option<String>,
    pub api_key_env: Option<String>,
    pub has_credential: bool,
    pub command: Option<String>,
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct SaveProviderSettings {
    pub protocol: ProviderProtocol,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub worker_staleness_threshold_ms: Option<u64>,
    pub base_url: Option<String>,
    pub api_key_secret_ref: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub api_key: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub command: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct ModelCapabilitiesConfig {
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub reasoning_replay: bool,
    /// The model's effort control, when the catalogue declares one; absent,
    /// the provider adapter's declaration for the models it serves applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub effort: Option<EffortCapability>,
}
impl ModelCapabilitiesConfig {
    pub fn is_empty(&self) -> bool {
        !self.reasoning_replay && self.effort.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct ModelSettings {
    pub provider_id: String,
    pub model_id: String,
    pub aliases: Vec<String>,
    pub context_window: Option<u32>,
    pub generation_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub default_effort: Option<Effort>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub capabilities: ModelCapabilitiesConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct GenerationPresetConfig {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
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
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct SearchProviderConfig {
    pub provider_type: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
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
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct RequireApprovalOptions {
    pub tool: String,
    #[ts(optional, type = "number | null")]
    pub timeout: Option<u64>,
    pub timeout_policy: Option<ApprovalTimeoutPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(untagged)]
#[ts(optional_fields = nullable)]
pub enum RequireApprovalEntry {
    Simple(String),
    WithOptions(#[ts(inline)] RequireApprovalOptions),
}
impl RequireApprovalEntry {
    pub fn tool_name(&self) -> &str {
        match self {
            Self::Simple(name) => name,
            Self::WithOptions(options) => &options.tool,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct ToolPermissionConfig {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub bundles: HashMap<String, Vec<String>>,
    pub require_approval: Vec<RequireApprovalEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct HitlConfig {
    #[ts(type = "number")]
    pub timeout: u64,
    pub timeout_policy: ApprovalTimeoutPolicy,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
pub struct ContextConfig {
    pub assembly: String,
    /// Named context assemblies registered as hidden alternatives to the
    /// configured default. Selection records the chosen name locally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub alternatives: Vec<String>,
    #[ts(type = "number")]
    pub timeout: u64,
    #[ts(optional, type = "number | null")]
    pub budget: Option<usize>,
    /// When a turn's context is too large and the turn continues on a
    /// successor: a message count or a token count. Unset, a turn compacts
    /// once its thread holds more than 100 messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub compaction_threshold: Option<CompactionThresholdConfig>,
    /// The model entry (a catalogue name or alias) that reads the history
    /// for where things went wrong when a session keeps hitting walls.
    /// Unset, the session's own model reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diagnosis_reader_model: Option<String>,
    /// The model entry (a catalogue name or alias) that writes the diagnosis
    /// a stalled session continues from, out of the readers' reports. Unset,
    /// the session's own model writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diagnosis_synthesis_model: Option<String>,
}
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            assembly: "default".to_string(),
            alternatives: Vec::new(),
            timeout: 5,
            budget: None,
            compaction_threshold: None,
            diagnosis_reader_model: None,
            diagnosis_synthesis_model: None,
        }
    }
}
/// The size past which a turn's context is compacted, counted in messages or
/// in tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CompactionThresholdConfig {
    Messages(u32),
    Tokens(#[ts(type = "number")] u64),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct ModelPricing {
    #[ts(type = "number")]
    pub input_price_per_mtok: i64,
    #[ts(type = "number")]
    pub output_price_per_mtok: i64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
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
#[ts(optional_fields = nullable)]
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
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct ContextTransferTargetConfig {
    pub mode: ContextTransferMode,
    pub allowed_fields: Vec<ContextTransferField>,
    pub summary_budget: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct ContextTransferConfig {
    pub mode: ContextTransferMode,
    pub allowed_fields: Vec<ContextTransferField>,
    pub summary_budget: Option<u32>,
    pub targets: HashMap<String, ContextTransferTargetConfig>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct CommunicationConfig {
    pub context_transfer: ContextTransferConfig,
    #[ts(type = "number")]
    pub message_ttl_secs: u64,
    #[ts(type = "number")]
    pub expiry_interval_secs: u64,
}

/// Session list housekeeping.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct SessionsConfig {
    /// Days without a user or assistant message after which a session leaves
    /// the recent list. Only a session with a turn executing is exempt, and
    /// a new turn on an archived session brings it back.
    #[ts(type = "number")]
    pub archive_after_days: u64,
}

/// Model routes for voice, served by the default provider's endpoint. An
/// unset id leaves that capability unavailable.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct VoiceConfig {
    /// Model id sent with speech-to-text requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transcription_model: Option<String>,
    /// Model id sent with text-to-speech requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub speech_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
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
    pub sessions: SessionsConfig,
    pub voice: VoiceConfig,
    pub restart_required: bool,
    pub can_restart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct SaveSettingsRequest {
    pub default_provider: String,
    pub providers: HashMap<String, SaveProviderSettings>,
    pub default_model: String,
    pub models: HashMap<String, ModelSettings>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub generation_presets: HashMap<String, GenerationPresetConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub tool_runtime: Option<ToolRuntimeSettings>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub tool_permissions: Option<ToolPermissionConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub hitl: Option<HitlConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub sandbox: Option<SandboxConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub context: Option<ContextConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub pricing: Option<PricingConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub observability: Option<ObservabilityConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub communication: Option<CommunicationConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub sessions: Option<SessionsConfig>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub voice: Option<VoiceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: String,
    pub context_window: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct ModelDiscoveryResponse {
    pub provider_id: String,
    pub models: Vec<DiscoveredModel>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct RestartStatusResponse {
    pub restart_required: bool,
    pub can_restart: bool,
    pub guidance: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_levels_are_ordered_lowest_first_and_spelled_on_the_wire() {
        assert!(Effort::Off < Effort::Minimal && Effort::Xhigh < Effort::Max);
        let spelled: Vec<&str> = Effort::ALL.iter().map(|level| level.as_str()).collect();
        assert_eq!(
            spelled,
            ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
        );
        for level in Effort::ALL {
            let wire = serde_json::to_value(level).unwrap();
            assert_eq!(wire, serde_json::json!(level.as_str()));
        }
    }

    #[test]
    fn a_capability_declaration_is_tagged_by_its_control() {
        let levels = EffortCapability::Levels {
            levels: vec![Effort::Low, Effort::High],
            can_disable: true,
        };
        assert_eq!(
            serde_json::to_value(&levels).unwrap(),
            serde_json::json!({"control": "levels", "levels": ["low", "high"], "can_disable": true})
        );
        let budget: EffortCapability = serde_json::from_value(serde_json::json!({
            "control": "budget", "min_tokens": 1024, "max_tokens": 32000, "can_disable": false
        }))
        .unwrap();
        assert!(matches!(
            budget,
            EffortCapability::Budget {
                min_tokens: 1024,
                ..
            }
        ));
    }

    #[test]
    fn a_declaration_that_cannot_be_honoured_is_refused() {
        let off_as_level = EffortCapability::Levels {
            levels: vec![Effort::Off],
            can_disable: false,
        };
        assert!(off_as_level.validate().unwrap_err().contains("can_disable"));
        let empty = EffortCapability::Levels {
            levels: vec![],
            can_disable: true,
        };
        assert!(empty.validate().is_err());
        let inverted = EffortCapability::Budget {
            min_tokens: 10,
            max_tokens: 5,
            can_disable: false,
        };
        assert!(inverted.validate().unwrap_err().contains("inverted"));
        assert!(EffortCapability::None.validate().is_ok());
    }
}
