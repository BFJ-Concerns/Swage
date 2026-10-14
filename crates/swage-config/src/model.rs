use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::path::PathBuf;
use swage_core::ApprovalTimeoutPolicy;

// ---------------------------------------------------------------------------
// Resolved config model — fully merged output of the hierarchy
// ---------------------------------------------------------------------------

/// Network server settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3000,
        }
    }
}

/// LLM provider protocol variant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    OpenAi,
    /// The OpenAI Responses API (`POST /v1/responses`) — the envelope
    /// GPT-family models are addressed with by their native harness. `OpenAi`
    /// remains Chat Completions for generic OpenAI-compatible endpoints.
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[default]
    Anthropic,
    Google,
    OpenRouter,
    Codex,
}

/// Connection settings for one configured LLM provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEndpointConfig {
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<PathBuf>,
    /// How long a held worker's session may sit idle on this route before a
    /// continued turn stops being the cheaper move. Routes differ — a proxy
    /// holding a prompt cache for minutes and a metered key holding none do
    /// not share a number — so this is declared per route and never defaulted
    /// to a global constant. `None` means this route makes no claim: a
    /// staleness reading over it reports the idle gap and says the verdict is
    /// unknown rather than guessing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_staleness_threshold_ms: Option<u64>,
}

impl Default for ProviderEndpointConfig {
    fn default() -> Self {
        Self {
            protocol: Protocol::OpenAi,
            api_key_secret_ref: None,
            api_key_env: None,
            base_url: None,
            command: None,
            state_dir: None,
            worker_staleness_threshold_ms: None,
        }
    }
}

/// Configured LLM providers keyed by provider id.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProviderCatalogConfig {
    pub default: String,
    #[serde(default)]
    pub entries: HashMap<String, ProviderEndpointConfig>,
}

impl ProviderCatalogConfig {
    pub fn default_provider(&self) -> Option<&ProviderEndpointConfig> {
        self.entries.get(&self.default)
    }
}

/// Model identity and metadata separate from provider connection settings.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModelEndpointConfig {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<swage_protocol::client::settings::Effort>,
    #[serde(default, skip_serializing_if = "ModelCapabilitiesConfig::is_empty")]
    pub capabilities: ModelCapabilitiesConfig,
}

pub use swage_protocol::client::settings::ModelCapabilitiesConfig;

/// Configured LLM models keyed by app-level model id.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModelCatalogConfig {
    pub default: String,
    #[serde(default)]
    pub entries: HashMap<String, ModelEndpointConfig>,
}

impl ModelCatalogConfig {
    pub fn default_model(&self) -> Option<&ModelEndpointConfig> {
        self.entries.get(&self.default)
    }
}

/// The provider route a model draws on: the configured allowance and endpoint
/// its calls actually touch, named by its provider id.
///
/// A "route" means this and only this wherever the term is used — held-worker
/// staleness thresholds and per-route token accounting both resolve through
/// [`resolve_provider_route`], so the two never disagree about which
/// allowance a model belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderRoute<'a> {
    /// The catalogue key selected before alias resolution.
    pub model_key: &'a str,
    pub provider_id: &'a str,
    pub endpoint: &'a ProviderEndpointConfig,
    pub model: &'a ModelEndpointConfig,
}

/// Resolve the provider route a model id draws on.
///
/// `model` is matched against the catalogue's keys first and its declared
/// aliases second; `None` resolves the catalogue default. A model named but
/// absent from the catalogue has no route — the caller must say so rather
/// than substitute the default, since a mistyped model is not the default
/// model's allowance.
pub fn resolve_provider_route<'a>(
    models: &'a ModelCatalogConfig,
    providers: &'a ProviderCatalogConfig,
    model: Option<&str>,
) -> Option<ProviderRoute<'a>> {
    let (model_key, model) = match model {
        Some(name) => models.entries.get_key_value(name).or_else(|| {
            models
                .entries
                .iter()
                .find(|(_, entry)| entry.aliases.iter().any(|alias| alias == name))
        })?,
        None => models.entries.get_key_value(&models.default)?,
    };
    let (provider_id, endpoint) = providers.entries.get_key_value(&model.provider_id)?;
    Some(ProviderRoute {
        model_key,
        provider_id,
        endpoint,
        model,
    })
}

/// Sandbox isolation settings.
pub use swage_protocol::client::settings::SandboxConfig;

/// Log verbosity levels.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Logging settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: LogLevel,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
        }
    }
}

/// Web search configuration for the supported SearxNG backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchProviderConfig {
    /// Which search backend to use ("searxng").
    pub provider_type: String,
    /// The search provider's API endpoint (e.g. "http://localhost:8080").
    pub base_url: String,
}

/// Tool permission configuration — controls which tools an agent may invoke.
///
/// Follows the same allow/deny/bundle model as `swage_tool::PermissionConfig`.
/// When no permissions are configured at any level, tools remain broadly
/// available, but high-risk tools are gated by human approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionConfig {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub bundles: HashMap<String, Vec<String>>,
    /// Tools that require explicit human approval before execution.
    ///
    /// Each entry is either a plain tool name (uses global HITL defaults) or
    /// an object with per-tool timeout and policy overrides. Defaults to the
    /// high-risk tools; an operator removes an entry to stop prompting for it.
    #[serde(default)]
    pub require_approval: Vec<RequireApprovalEntry>,
}

impl Default for ToolPermissionConfig {
    fn default() -> Self {
        let mut bundles = HashMap::new();
        bundles.insert(
            "browsing".to_string(),
            vec![
                "browser_navigate".to_string(),
                "browser_snapshot".to_string(),
            ],
        );
        bundles.insert(
            "browser_actions".to_string(),
            vec![
                "browser_click".to_string(),
                "browser_type".to_string(),
                "browser_select".to_string(),
                "browser_scroll".to_string(),
            ],
        );
        // The schedule tools as one grantable unit: a user can allow (or deny)
        // an agent's standing-schedule powers without touching the rest of its
        // permissions.
        bundles.insert(
            "scheduling".to_string(),
            vec!["schedule".to_string(), "wake_me_later".to_string()],
        );
        bundles.insert(
            "session-authoring".to_string(),
            vec!["session_values".to_string()],
        );
        // Knowledge-base access at registry grain: reading the topic's
        // gathered evidence and writing captures into it are separate powers,
        // so gathering workers can hold write without every reader gaining it.
        // Surfaces at the same grain: reading the session's work surfaces,
        // writing on them, and reaching the shared library are separate
        // powers.
        bundles.insert(
            "surfaces-read".to_string(),
            vec!["surface_read".to_string(), "surface_library".to_string()],
        );
        bundles.insert(
            "surfaces-write".to_string(),
            vec!["surface_write".to_string()],
        );
        bundles.insert(
            "knowledge-read".to_string(),
            vec![
                "knowledge_search".to_string(),
                "knowledge_orient".to_string(),
            ],
        );
        bundles.insert(
            "knowledge-write".to_string(),
            vec!["knowledge_record".to_string()],
        );
        bundles.insert(
            "knowledge-curation".to_string(),
            vec!["knowledge_curate".to_string()],
        );
        bundles.insert(
            "knowledge-citation".to_string(),
            vec!["knowledge_cite".to_string()],
        );
        bundles.insert(
            "knowledge-orientation-curation".to_string(),
            vec!["knowledge_curate_orientation".to_string()],
        );
        bundles.insert(
            "materials-read".to_string(),
            vec!["material_read".to_string()],
        );
        Self {
            allow: vec!["*".into()],
            deny: vec![],
            bundles,
            require_approval: default_high_risk_tool_approvals(),
        }
    }
}

/// Tools that act outside the conversation — running commands or code,
/// writing files, operating a page — and so prompt before each call in an
/// attended session. Unattended sessions run on their grants alone.
fn default_high_risk_tool_approvals() -> Vec<RequireApprovalEntry> {
    [
        "shell",
        "file_edit",
        "file_write",
        "python_execute",
        "browser_click",
        "browser_type",
        "browser_select",
        "browser_scroll",
    ]
    .into_iter()
    .map(|tool| RequireApprovalEntry::Simple(tool.to_string()))
    .collect()
}

/// Settings for the gateway-hosted browser tools.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserScreenshotPolicy {
    Off,
    #[default]
    Actions,
    All,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserToolConfig {
    pub binary_path: Option<String>,
    pub page_idle_secs: u64,
    pub process_idle_secs: u64,
    pub max_snapshot_bytes: usize,
    pub screenshots: BrowserScreenshotPolicy,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

impl Default for BrowserToolConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            page_idle_secs: 300,
            process_idle_secs: 600,
            max_snapshot_bytes: 40_000,
            screenshots: BrowserScreenshotPolicy::Actions,
            allow_hosts: Vec::new(),
        }
    }
}

/// Tool execution settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolConfig {
    /// Maximum seconds a shell command may run before being killed.
    pub shell_timeout_secs: u64,
    /// Python execution settings shared by python-backed features.
    #[serde(default)]
    pub python_execute: PythonExecuteToolConfig,
    /// Output truncation threshold (bytes) applied to all tool results.
    pub max_output_bytes: usize,
    /// Optional plugin directory scanned at gateway startup.
    pub plugin_directory: Option<String>,
    /// Web search provider configuration. None means web search is disabled.
    pub search: Option<SearchProviderConfig>,
    /// Gateway-hosted browser settings.
    #[serde(default)]
    pub browser: BrowserToolConfig,
    /// Tool permission rules — controls which tools an agent may invoke.
    /// Defaults to broad availability with approval gates for high-risk tools.
    #[serde(default)]
    pub permissions: ToolPermissionConfig,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            shell_timeout_secs: 30,
            python_execute: PythonExecuteToolConfig::default(),
            max_output_bytes: 100_000,
            plugin_directory: None,
            search: None,
            browser: BrowserToolConfig::default(),
            permissions: ToolPermissionConfig::default(),
        }
    }
}

/// Runtime discovery policy for Python-backed features.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PythonRuntimePolicy {
    #[default]
    BundledThenSystem,
}

/// Settings for child-process Python execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PythonExecuteToolConfig {
    pub enabled: bool,
    pub timeout_secs: u64,
    pub startup_timeout_secs: u64,
    pub idle_ttl_secs: u64,
    pub warm_target: usize,
    pub max_workers: usize,
    pub runtime_policy: PythonRuntimePolicy,
    pub runtime_path: Option<String>,
    pub require_sandbox: bool,
}

fn default_python_max_workers() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get().min(8))
        .unwrap_or(1)
        .max(1)
}

impl Default for PythonExecuteToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: 30,
            startup_timeout_secs: 10,
            idle_ttl_secs: 60,
            warm_target: 1,
            max_workers: default_python_max_workers(),
            runtime_policy: PythonRuntimePolicy::BundledThenSystem,
            runtime_path: None,
            require_sandbox: true,
        }
    }
}

/// Context assembly settings — controls how the agent's context window is built
/// before each inference call.
pub use swage_protocol::client::settings::ContextConfig;

/// Per-model token pricing in microcents per million tokens.
///
/// Prices are stored as integers to avoid floating-point rounding when
/// computing costs across large token counts.
pub use swage_protocol::client::settings::ModelPricing;

/// Model pricing configuration — optional per-model cost data for observability.
///
/// When a model is absent from the map, `compute_cost` returns `None`
/// rather than zero so callers can distinguish "free/unknown" from "configured zero".
pub use swage_protocol::client::settings::PricingConfig;

/// Observability settings for the gateway.
///
/// When `force_on` is true, all WebSocket connections are treated as
/// subscribed to every observability channel — no explicit subscribe
/// message is required from the client. Useful for development and
/// single-user deployments.
pub use swage_protocol::client::settings::ObservabilityConfig;

/// Experiment switches for hidden runtime variants.
///
/// Variant ids are an operator/runtime concern. User-facing and model-facing
/// surfaces should continue to name the function being performed, while this
/// section decides whether the runtime may choose among alternative
/// implementations behind that stable surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentationConfig {
    /// Master switch for every experimental variant decision.
    ///
    /// When false, variant selection helpers must return the default
    /// implementation and record, at most, that experimentation was disabled.
    #[serde(default = "default_experimentation_enabled")]
    pub enabled: bool,
    /// Per-location controls keyed by stable runtime location id.
    ///
    /// Location ids are intentionally application-defined strings so callers
    /// can address broad areas such as `context.assembly` or precise work such
    /// as `task.important-import`. Entries merge across config layers by key.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub locations: HashMap<String, ExperimentLocationConfig>,
}

fn default_experimentation_enabled() -> bool {
    true
}

impl Default for ExperimentationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            locations: HashMap::new(),
        }
    }
}

impl ExperimentationConfig {
    pub fn is_enabled_for(&self, location_id: &str) -> bool {
        self.enabled
            && self
                .locations
                .get(location_id)
                .is_none_or(|location| location.enabled)
    }

    pub fn forced_variant_for(&self, location_id: &str) -> Option<&str> {
        self.locations
            .get(location_id)
            .filter(|location| self.enabled && location.enabled)
            .and_then(|location| location.forced_variant.as_deref())
    }
}

/// Per-location experiment controls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentLocationConfig {
    /// Whether this exact location may participate in experimentation.
    #[serde(default = "default_experimentation_enabled")]
    pub enabled: bool,
    /// Optional operator pin for deterministic dogfooding.
    ///
    /// This is useful for forcing a specific variant during a session or task
    /// without changing the public function name that the user and model see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_variant: Option<String>,
}

impl Default for ExperimentLocationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            forced_variant: None,
        }
    }
}

/// Output schema validation settings — controls structured-output enforcement
/// for the agent's final response.
///
/// When `schema` is `Some`, the agent's response is validated against the
/// provided JSON Schema. On failure, the agent retries up to `max_retries`
/// times before giving up and surfacing an error event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputConfig {
    /// JSON Schema the agent response must conform to. `None` disables
    /// schema validation entirely.
    pub schema: Option<serde_json::Value>,
    /// Maximum number of validation-retry attempts before the turn is
    /// considered failed. Defaults to 3.
    pub max_retries: u32,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            schema: None,
            max_retries: 3,
        }
    }
}

/// Human-in-the-loop approval settings.
///
/// Controls how long the gateway waits for a human decision on a pending
/// tool call and what action to take when that window expires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HitlConfig {
    /// Maximum seconds to wait for a human decision before applying
    /// `timeout_policy`. Defaults to 300 (five minutes).
    pub timeout: u64,
    /// What to do if the timeout elapses without a decision.
    pub timeout_policy: ApprovalTimeoutPolicy,
}

impl Default for HitlConfig {
    fn default() -> Self {
        Self {
            timeout: 300,
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        }
    }
}

/// A single entry in the `require_approval` list.
///
/// Deserialises from either a plain tool name string or an object that
/// includes per-tool timeout overrides:
///
/// ```toml
/// require_approval = ["write_file", { tool = "execute_command", timeout = 600 }]
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequireApprovalEntry {
    /// Plain tool name — uses global HITL defaults for timeout and policy.
    Simple(String),
    /// Tool name with per-tool timeout and/or policy overrides.
    WithOptions {
        tool: String,
        /// Per-tool timeout override in seconds. Falls back to HitlConfig.timeout.
        timeout: Option<u64>,
        /// Per-tool timeout policy override. Falls back to HitlConfig.timeout_policy.
        timeout_policy: Option<ApprovalTimeoutPolicy>,
    },
}

impl RequireApprovalEntry {
    /// Returns the tool name regardless of which variant this is.
    pub fn tool_name(&self) -> &str {
        match self {
            RequireApprovalEntry::Simple(name) => name,
            RequireApprovalEntry::WithOptions { tool, .. } => tool,
        }
    }
}

// Re-exported from swage-core — single definition for both config and wire format.
// The core type uses `serde(rename_all = "snake_case")` which is equivalent to
// `lowercase` for all current single-word variants (None, Explicit, Summary).
pub use swage_core::ContextTransferMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextTransferField {
    System,
    User,
    Assistant,
}

/// Context transfer policy for a single target agent.
///
/// Overrides the defaults in [`ContextTransferConfig`] for a specific
/// sender → target pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextTransferTargetConfig {
    /// Transfer mode for this specific target.
    pub mode: ContextTransferMode,
    /// For `Explicit` mode: which context element types to include.
    #[serde(default)]
    pub allowed_fields: Vec<ContextTransferField>,
    /// For `Summary` mode: maximum token budget for the compacted summary.
    pub summary_budget: Option<u32>,
}

impl Default for ContextTransferTargetConfig {
    fn default() -> Self {
        Self {
            mode: ContextTransferMode::None,
            allowed_fields: vec![],
            summary_budget: None,
        }
    }
}

/// Context transfer configuration — default policy and per-target overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextTransferConfig {
    /// Default transfer mode for all targets not in `targets`.
    pub mode: ContextTransferMode,
    /// For `Explicit` mode: default allowed fields.
    #[serde(default)]
    pub allowed_fields: Vec<ContextTransferField>,
    /// For `Summary` mode: default token budget.
    pub summary_budget: Option<u32>,
    /// Per-target overrides keyed by recipient agent_id.
    #[serde(default)]
    pub targets: HashMap<String, ContextTransferTargetConfig>,
}

impl Default for ContextTransferConfig {
    fn default() -> Self {
        Self {
            mode: ContextTransferMode::None,
            allowed_fields: vec![],
            summary_budget: None,
            targets: HashMap::new(),
        }
    }
}

/// Inter-agent communication settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunicationConfig {
    /// Context transfer policy (default + per-target overrides).
    #[serde(default)]
    pub context_transfer: ContextTransferConfig,
    /// How long a pending message survives before expiry (seconds).
    /// Default: 86400 (24 hours).
    #[serde(default = "default_message_ttl_secs")]
    pub message_ttl_secs: u64,
    /// How often the expiry task checks for stale messages (seconds).
    /// Default: 60.
    #[serde(default = "default_expiry_interval_secs")]
    pub expiry_interval_secs: u64,
}

fn default_message_ttl_secs() -> u64 {
    86400
}

fn default_expiry_interval_secs() -> u64 {
    60
}

impl Default for CommunicationConfig {
    fn default() -> Self {
        Self {
            context_transfer: ContextTransferConfig::default(),
            message_ttl_secs: default_message_ttl_secs(),
            expiry_interval_secs: default_expiry_interval_secs(),
        }
    }
}

/// Scheduler settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulingConfig {
    /// Maximum age for terminal schedule-run rows before retention may reap
    /// them (days). Audit events remain the durable reconstruction record.
    /// Default: 90 days.
    #[serde(default = "default_run_history_max_age_days")]
    pub run_history_max_age_days: u64,
    /// Maximum terminal schedule-run rows kept per schedule. Active rows are
    /// never counted against or removed by this cap.
    /// Default: 500 rows.
    #[serde(default = "default_run_history_max_per_schedule")]
    pub run_history_max_per_schedule: u32,
}

fn default_run_history_max_age_days() -> u64 {
    90
}

fn default_run_history_max_per_schedule() -> u32 {
    500
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            run_history_max_age_days: default_run_history_max_age_days(),
            run_history_max_per_schedule: default_run_history_max_per_schedule(),
        }
    }
}

// Declared once in swage-protocol: the wire type and the resolved config
// type are one definition, with the partial, schema and semantics beside it.
pub use swage_protocol::client::settings::{MemoryConfig, SessionsConfig};

/// Model routes for voice. Voice speaks only to the default provider's
/// endpoint; each id is the model that endpoint serves for that audio API.
/// An unset id leaves that capability unavailable — voice never falls back
/// to the default chat model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// Model id sent with speech-to-text requests (`/audio/transcriptions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription_model: Option<String>,
    /// Model id sent with text-to-speech requests (`/audio/speech`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_model: Option<String>,
}

/// Named generation parameters that can be applied to provider requests.
///
/// Empty presets are invalid at the settings API boundary. The config type is
/// permissive so older or hand-written files can still be loaded and repaired
/// through settings rather than making the whole gateway unusable.
pub use swage_protocol::client::settings::GenerationPresetConfig;

/// Global generation preset catalogue. `None` is intentionally not represented
/// here; it is a built-in UI/runtime selection meaning provider defaults.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GenerationConfig {
    #[serde(default)]
    pub presets: HashMap<String, GenerationPresetConfig>,
}

/// Configured external communication channels.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram: Option<TelegramChannelConfig>,
}

/// Telegram bot API polling settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_token_secret_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_token_env: Option<String>,
    #[serde(default = "default_telegram_api_base_url")]
    pub api_base_url: String,
    #[serde(default = "default_telegram_poll_timeout_secs")]
    pub poll_timeout_secs: u64,
}

impl Default for TelegramChannelConfig {
    fn default() -> Self {
        Self {
            bot_token_secret_ref: None,
            bot_token_env: None,
            api_base_url: default_telegram_api_base_url(),
            poll_timeout_secs: default_telegram_poll_timeout_secs(),
        }
    }
}

fn default_telegram_api_base_url() -> String {
    "https://api.telegram.org".to_string()
}

fn default_telegram_poll_timeout_secs() -> u64 {
    25
}

/// Fully resolved configuration — the merged output of all hierarchy levels.
///
/// This is what consumers receive after discovery, loading, validation, and
/// cascade merging have all completed.
// ResolvedConfig derives Default — all section types implement Default,
// and String::default() produces the empty string for instructions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ResolvedConfig {
    pub server: ServerConfig,
    /// Human-in-the-loop approval settings: timeout duration and expiry policy.
    pub hitl: HitlConfig,
    /// Canonical provider catalogue. Prefer this over the legacy singular
    /// `provider` field when selecting runtime backends.
    pub providers: ProviderCatalogConfig,
    /// Canonical model catalogue. Model entries point at provider catalogue ids.
    pub models: ModelCatalogConfig,
    pub sandbox: SandboxConfig,
    pub logging: LoggingConfig,
    pub tools: ToolConfig,
    pub context: ContextConfig,
    /// Per-model pricing data for cost derivation. Empty by default — cost
    /// computation returns None for any model absent from the map.
    pub pricing: PricingConfig,
    /// Observability settings — controls channel delivery behaviour.
    pub observability: ObservabilityConfig,
    /// Experimentation settings — controls hidden runtime variants.
    pub experimentation: ExperimentationConfig,
    /// Output schema validation settings — JSON Schema and retry limits.
    /// Defaults to no schema (validation disabled) with 3 retries.
    pub output: OutputConfig,
    /// Skills declared by the resolved durable agent definition.
    ///
    /// This runtime-only overlay is empty for file configuration and is
    /// populated after durable-definition resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_skills: Vec<String>,
    /// Inter-agent communication settings — context transfer policy.
    /// Defaults to no communication (context transfer mode = none).
    pub communication: CommunicationConfig,
    /// Scheduler settings, including local run-history retention.
    pub scheduling: SchedulingConfig,
    /// Session list housekeeping: the inactivity archive.
    pub sessions: SessionsConfig,
    /// `[memory]` — when the curator passes over an attended conversation.
    pub memory: MemoryConfig,
    /// Model routes for transcription and speech synthesis.
    pub voice: VoiceConfig,
    /// Named generation parameter presets available to model entries.
    pub generation: GenerationConfig,
    /// External communication channel configuration.
    pub channels: ChannelsConfig,
    /// Assembled instruction text from all hierarchy levels, in order.
    pub instructions: String,
    /// When true, the entire permission and approval system is bypassed:
    /// tool allow/deny lists are ignored, `require_approval` gates do not
    /// fire, and out-of-workspace location prompts are auto-approved. The
    /// framework code stays in place; flipping this back to `false` (or
    /// removing it from the config) restores normal enforcement. Mirrors
    /// the spirit of Claude Code's `--dangerously-skip-permissions`.
    #[serde(default)]
    pub dangerously_skip_permissions: bool,
}

impl ResolvedConfig {
    pub fn default_model_id(&self) -> &str {
        self.models
            .default_model()
            .map(|model| model.model_id.as_str())
            .unwrap_or("")
    }

    pub fn default_provider_endpoint(&self) -> Option<&ProviderEndpointConfig> {
        let model = self.models.default_model()?;
        self.providers.entries.get(&model.provider_id)
    }

    pub fn default_provider_protocol(&self) -> Protocol {
        self.default_provider_endpoint()
            .map(|provider| provider.protocol.clone())
            .unwrap_or(Protocol::OpenAi)
    }

    pub fn set_default_model_id(&mut self, model_id: String) {
        if self.models.default.is_empty() {
            self.models.default = "default".to_string();
        }
        let model = self
            .models
            .entries
            .entry(self.models.default.clone())
            .or_insert_with(|| ModelEndpointConfig {
                provider_id: String::new(),
                model_id: String::new(),
                aliases: Vec::new(),
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: ModelCapabilitiesConfig::default(),
            });
        model.model_id = model_id;
    }
}

/// Which level of the hierarchy a value or instruction came from.
///
/// Used by instruction assembly to attribute sections to their source level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HierarchyLevel {
    Global,
    Topic,
    Agent,
}

impl std::fmt::Display for HierarchyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Global => "Global",
            Self::Topic => "Topic",
            Self::Agent => "Agent",
        };
        write!(f, "{name}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn route_catalogues() -> (ModelCatalogConfig, ProviderCatalogConfig) {
        let mut models = ModelCatalogConfig {
            default: "house".into(),
            entries: HashMap::new(),
        };
        models.entries.insert(
            "house".into(),
            ModelEndpointConfig {
                provider_id: "proxy".into(),
                model_id: "house-1".into(),
                aliases: vec!["cheap".into()],
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: ModelCapabilitiesConfig::default(),
            },
        );
        models.entries.insert(
            "metered".into(),
            ModelEndpointConfig {
                provider_id: "vendor".into(),
                model_id: "vendor-1".into(),
                aliases: Vec::new(),
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: ModelCapabilitiesConfig::default(),
            },
        );

        let mut providers = ProviderCatalogConfig {
            default: "proxy".into(),
            entries: HashMap::new(),
        };
        providers.entries.insert(
            "proxy".into(),
            ProviderEndpointConfig {
                protocol: Protocol::Anthropic,
                base_url: Some("http://proxy.invalid".into()),
                worker_staleness_threshold_ms: Some(600_000),
                ..Default::default()
            },
        );
        providers.entries.insert(
            "vendor".into(),
            ProviderEndpointConfig {
                protocol: Protocol::Anthropic,
                base_url: Some("http://vendor.invalid".into()),
                ..Default::default()
            },
        );
        (models, providers)
    }

    #[test]
    fn provider_route_resolves_by_id_alias_and_default() {
        let (models, providers) = route_catalogues();

        let by_id = resolve_provider_route(&models, &providers, Some("house")).expect("by id");
        assert_eq!(by_id.provider_id, "proxy");
        assert_eq!(by_id.model.model_id, "house-1");
        assert_eq!(by_id.endpoint.worker_staleness_threshold_ms, Some(600_000));

        let by_alias = resolve_provider_route(&models, &providers, Some("cheap")).expect("alias");
        assert_eq!(by_alias.provider_id, "proxy");
        assert_eq!(by_alias.model.model_id, "house-1");

        let by_default = resolve_provider_route(&models, &providers, None).expect("default");
        assert_eq!(by_default.provider_id, "proxy");
    }

    #[test]
    fn provider_route_is_per_route_and_absent_for_an_unknown_model() {
        let (models, providers) = route_catalogues();

        let metered = resolve_provider_route(&models, &providers, Some("metered")).expect("route");
        assert_eq!(metered.provider_id, "vendor");
        assert_eq!(
            metered.endpoint.worker_staleness_threshold_ms, None,
            "a route that declares no threshold does not inherit another route's"
        );

        assert!(resolve_provider_route(&models, &providers, Some("absent")).is_none());
    }

    #[test]
    fn server_config_defaults() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 3000);
    }

    #[test]
    fn sandbox_config_defaults() {
        let cfg = SandboxConfig::default();
        assert!(cfg.enabled);
        assert!(!cfg.allow_degraded_execution);
    }

    #[test]
    fn logging_config_defaults() {
        let cfg = LoggingConfig::default();
        assert_eq!(cfg.level, LogLevel::Info);
    }

    #[test]
    fn resolved_config_defaults() {
        let cfg = ResolvedConfig::default();
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, 3000);
        assert_eq!(cfg.providers.default, "");
        assert!(cfg.providers.entries.is_empty());
        assert_eq!(cfg.models.default, "");
        assert!(cfg.models.entries.is_empty());
        assert_eq!(cfg.default_model_id(), "");
        assert!(cfg.sandbox.enabled);
        assert!(!cfg.sandbox.allow_degraded_execution);
        assert_eq!(cfg.logging.level, LogLevel::Info);
        assert_eq!(cfg.instructions, "");
    }

    #[test]
    fn tool_config_defaults() {
        let cfg = ToolConfig::default();
        assert_eq!(cfg.shell_timeout_secs, 30);
        assert_eq!(cfg.max_output_bytes, 100_000);
        assert_eq!(cfg.plugin_directory, None);
        assert_eq!(cfg.search, None);
    }

    #[test]
    fn search_provider_config_fields() {
        let cfg: SearchProviderConfig = serde_json::from_value(serde_json::json!({
            "provider_type": "searxng", "base_url": "http://localhost:8080"
        }))
        .unwrap();
        let encoded = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "provider_type": "searxng", "base_url": "http://localhost:8080"
            })
        );
    }

    #[test]
    fn resolved_config_includes_tool_config() {
        let cfg = ResolvedConfig::default();
        assert_eq!(cfg.tools.shell_timeout_secs, 30);
        assert_eq!(cfg.tools.max_output_bytes, 100_000);
        assert_eq!(cfg.tools.plugin_directory, None);
        assert_eq!(cfg.tools.search, None);
    }

    #[test]
    fn context_config_defaults() {
        let cfg = ContextConfig::default();
        assert_eq!(cfg.assembly, "default");
        assert!(cfg.alternatives.is_empty());
        assert_eq!(cfg.timeout, 5);
        assert_eq!(cfg.budget, None);
    }

    #[test]
    fn context_config_serialise_round_trip() {
        let original = ContextConfig {
            assembly: "custom".to_string(),
            alternatives: vec!["alternate".to_string()],
            timeout: 10,
            budget: Some(4096),
            compaction_threshold: Some(
                swage_protocol::client::settings::CompactionThresholdConfig::Messages(12),
            ),
            diagnosis_reader_model: Some("reader".to_string()),
            diagnosis_synthesis_model: Some("synthesis".to_string()),
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ContextConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn context_config_default_round_trip() {
        let original = ContextConfig::default();
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ContextConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn resolved_config_includes_context() {
        let cfg = ResolvedConfig::default();
        assert_eq!(cfg.context.assembly, "default");
        assert_eq!(cfg.context.timeout, 5);
        assert_eq!(cfg.context.budget, None);
    }

    // --- PricingConfig tests ---

    #[test]
    fn pricing_config_default_is_empty() {
        let cfg = PricingConfig::default();
        assert!(cfg.models.is_empty());
    }

    #[test]
    fn compute_cost_returns_none_for_unknown_model() {
        let cfg = PricingConfig::default();
        assert_eq!(cfg.compute_cost("unknown-model", 1000, 500), None);
    }

    #[test]
    fn compute_cost_returns_correct_value() {
        let mut cfg = PricingConfig::default();
        cfg.models.insert(
            "test-model".to_string(),
            ModelPricing {
                input_price_per_mtok: 300,
                output_price_per_mtok: 1500,
            },
        );
        // input:  1_000_000 * 300  / 1_000_000 = 300
        // output:   500_000 * 1500 / 1_000_000 = 750
        // total: 1050
        assert_eq!(
            cfg.compute_cost("test-model", 1_000_000, 500_000),
            Some(1050)
        );
    }

    #[test]
    fn compute_cost_zero_tokens_is_zero() {
        let mut cfg = PricingConfig::default();
        cfg.models.insert(
            "m".to_string(),
            ModelPricing {
                input_price_per_mtok: 300,
                output_price_per_mtok: 1500,
            },
        );
        assert_eq!(cfg.compute_cost("m", 0, 0), Some(0));
    }

    #[test]
    fn resolved_config_includes_empty_pricing() {
        let cfg = ResolvedConfig::default();
        assert!(cfg.pricing.models.is_empty());
    }

    // --- ObservabilityConfig tests ---

    #[test]
    fn observability_config_default_force_on_is_false() {
        let cfg = ObservabilityConfig::default();
        assert!(!cfg.force_on);
    }

    #[test]
    fn resolved_config_includes_observability_default() {
        let cfg = ResolvedConfig::default();
        assert!(!cfg.observability.force_on);
    }

    #[test]
    fn observability_config_toml_round_trip() {
        let original = ObservabilityConfig { force_on: true };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ObservabilityConfig =
            toml::from_str(&serialised).expect("deserialise failed");
        assert!(restored.force_on);
    }

    // --- RequireApprovalEntry tests ---

    #[test]
    fn require_approval_entry_from_plain_string() {
        // TOML requires top-level tables, so use a wrapper for array entries.
        #[derive(Debug, Deserialize)]
        struct W {
            items: Vec<RequireApprovalEntry>,
        }
        let w: W = toml::from_str(r#"items = ["write_file"]"#).unwrap();
        assert_eq!(w.items.len(), 1);
        assert_eq!(w.items[0].tool_name(), "write_file");
    }

    #[test]
    fn require_approval_entry_from_object_minimal() {
        let toml_str = r#"tool = "execute_command""#;
        let entry: RequireApprovalEntry = toml::from_str(toml_str).unwrap();
        assert_eq!(entry.tool_name(), "execute_command");
    }

    #[test]
    fn require_approval_entry_from_object_full() {
        let toml_str = r#"
tool = "execute_command"
timeout = 600
timeout_policy = "interrupt"
"#;
        let entry: RequireApprovalEntry = toml::from_str(toml_str).unwrap();
        assert_eq!(entry.tool_name(), "execute_command");
        if let RequireApprovalEntry::WithOptions {
            timeout,
            timeout_policy,
            ..
        } = &entry
        {
            assert_eq!(*timeout, Some(600));
            assert_eq!(*timeout_policy, Some(ApprovalTimeoutPolicy::Interrupt));
        } else {
            panic!("expected WithOverrides variant");
        }
    }

    #[test]
    fn require_approval_entry_object_reject_policy() {
        let toml_str = r#"
tool = "deploy"
timeout_policy = "reject"
"#;
        let entry: RequireApprovalEntry = toml::from_str(toml_str).unwrap();
        assert_eq!(entry.tool_name(), "deploy");
        if let RequireApprovalEntry::WithOptions {
            timeout,
            timeout_policy,
            ..
        } = &entry
        {
            assert!(timeout.is_none());
            assert_eq!(*timeout_policy, Some(ApprovalTimeoutPolicy::Reject));
        } else {
            panic!("expected WithOverrides variant");
        }
    }

    #[test]
    fn require_approval_entry_tool_name_both_variants() {
        let simple = RequireApprovalEntry::Simple("bash".to_string());
        assert_eq!(simple.tool_name(), "bash");

        let overrides = RequireApprovalEntry::WithOptions {
            tool: "deploy".to_string(),
            timeout: None,
            timeout_policy: None,
        };
        assert_eq!(overrides.tool_name(), "deploy");
    }

    #[test]
    fn require_approval_entry_toml_round_trip_string() {
        // String entries round-trip through a wrapping table since TOML
        // requires a top-level table for serialisation.
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Wrapper {
            entries: Vec<RequireApprovalEntry>,
        }
        let original = Wrapper {
            entries: vec![RequireApprovalEntry::Simple("write_file".to_string())],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: Wrapper = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn require_approval_entry_toml_round_trip_object() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Wrapper {
            entries: Vec<RequireApprovalEntry>,
        }
        let original = Wrapper {
            entries: vec![RequireApprovalEntry::WithOptions {
                tool: "execute_command".to_string(),
                timeout: Some(600),
                timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
            }],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: Wrapper = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn require_approval_entry_toml_round_trip_mixed() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Wrapper {
            entries: Vec<RequireApprovalEntry>,
        }
        let original = Wrapper {
            entries: vec![
                RequireApprovalEntry::Simple("write_file".to_string()),
                RequireApprovalEntry::WithOptions {
                    tool: "execute_command".to_string(),
                    timeout: Some(600),
                    timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
                },
            ],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: Wrapper = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    // --- ToolPermissionConfig tests ---

    #[test]
    fn tool_permission_config_default_gates_high_risk_tools() {
        let cfg = ToolPermissionConfig::default();
        assert_eq!(cfg.allow, vec!["*".to_string()]);
        assert!(cfg.deny.is_empty());
        assert_eq!(
            cfg.bundles.get("browsing").expect("browsing bundle"),
            &vec![
                "browser_navigate".to_string(),
                "browser_snapshot".to_string()
            ]
        );
        assert_eq!(
            cfg.bundles
                .get("browser_actions")
                .expect("browser action bundle"),
            &vec![
                "browser_click".to_string(),
                "browser_type".to_string(),
                "browser_select".to_string(),
                "browser_scroll".to_string()
            ]
        );
        // The schedule tools are grantable as one unit.
        assert_eq!(
            cfg.bundles.get("scheduling").expect("scheduling bundle"),
            &vec!["schedule".to_string(), "wake_me_later".to_string()]
        );
        assert_eq!(
            cfg.bundles
                .get("knowledge-curation")
                .expect("knowledge curation bundle"),
            &vec!["knowledge_curate".to_string()]
        );
        assert_eq!(
            cfg.bundles
                .get("knowledge-citation")
                .expect("knowledge citation bundle"),
            &vec!["knowledge_cite".to_string()]
        );
        assert_eq!(
            cfg.bundles
                .get("knowledge-read")
                .expect("knowledge read bundle"),
            &vec![
                "knowledge_search".to_string(),
                "knowledge_orient".to_string()
            ]
        );
        assert_eq!(
            cfg.bundles
                .get("knowledge-orientation-curation")
                .expect("knowledge orientation curation bundle"),
            &vec!["knowledge_curate_orientation".to_string()]
        );
        assert_eq!(
            cfg.require_approval
                .iter()
                .map(RequireApprovalEntry::tool_name)
                .collect::<Vec<_>>(),
            vec![
                "shell",
                "file_edit",
                "file_write",
                "python_execute",
                "browser_click",
                "browser_type",
                "browser_select",
                "browser_scroll"
            ]
        );
    }

    #[test]
    fn tool_permission_config_require_approval_simple_entry_toml_round_trip() {
        let original = ToolPermissionConfig {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry::Simple("write_file".into())],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ToolPermissionConfig =
            toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
        assert_eq!(restored.require_approval[0].tool_name(), "write_file");
    }

    #[test]
    fn tool_permission_config_require_approval_with_options_toml_round_trip() {
        use swage_core::ApprovalTimeoutPolicy;
        let original = ToolPermissionConfig {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry::WithOptions {
                tool: "execute_command".into(),
                timeout: Some(600),
                timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
            }],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ToolPermissionConfig =
            toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
        assert_eq!(restored.require_approval[0].tool_name(), "execute_command");
    }

    #[test]
    fn tool_permission_config_default_round_trips_toml() {
        let original = ToolPermissionConfig::default();
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ToolPermissionConfig =
            toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn tool_permission_config_custom_deny_round_trips_toml() {
        let original = ToolPermissionConfig {
            allow: vec!["*".into()],
            deny: vec!["shell_exec".into(), "file_write".into()],
            bundles: HashMap::new(),
            require_approval: vec![],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ToolPermissionConfig =
            toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn tool_permission_config_custom_allow_and_bundles_round_trips_toml() {
        let mut bundles = HashMap::new();
        bundles.insert(
            "read_only".to_string(),
            vec!["file_read".into(), "web_search".into()],
        );
        bundles.insert("write".to_string(), vec!["file_write".into()]);

        let original = ToolPermissionConfig {
            allow: vec!["file_read".into(), "web_search".into()],
            deny: vec![],
            bundles,
            require_approval: vec![],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ToolPermissionConfig =
            toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn tool_config_with_permissions_serialises_correctly() {
        let mut bundles = HashMap::new();
        bundles.insert("safe".to_string(), vec!["echo".into()]);

        let original = ToolConfig {
            shell_timeout_secs: 60,
            python_execute: PythonExecuteToolConfig::default(),
            max_output_bytes: 50_000,
            plugin_directory: Some("plugins".into()),
            search: None,
            browser: BrowserToolConfig::default(),
            permissions: ToolPermissionConfig {
                allow: vec!["echo".into()],
                deny: vec!["shell_exec".into()],
                bundles,
                require_approval: vec![],
            },
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ToolConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn tool_config_defaults_include_permissions() {
        let cfg = ToolConfig::default();
        assert_eq!(cfg.permissions.allow, vec!["*".to_string()]);
        assert!(cfg.permissions.deny.is_empty());
        assert!(cfg.permissions.bundles.contains_key("browsing"));
        assert!(cfg.permissions.bundles.contains_key("browser_actions"));
    }

    // --- OutputConfig tests ---

    #[test]
    fn output_config_default_schema_is_none() {
        let cfg = OutputConfig::default();
        assert!(cfg.schema.is_none());
    }

    #[test]
    fn output_config_default_max_retries_is_three() {
        let cfg = OutputConfig::default();
        assert_eq!(cfg.max_retries, 3);
    }

    #[test]
    fn output_config_toml_round_trip_defaults() {
        let original = OutputConfig::default();
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: OutputConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn output_config_toml_round_trip_with_schema() {
        let original = OutputConfig {
            schema: Some(
                serde_json::json!({"type": "object", "properties": {"answer": {"type": "string"}}}),
            ),
            max_retries: 5,
        };
        // OutputConfig uses JSON for schema — round-trip through JSON (TOML can't
        // represent arbitrary JSON natively, so we verify JSON serialisation).
        let serialised = serde_json::to_string(&original).expect("serialise failed");
        let restored: OutputConfig = serde_json::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn resolved_config_output_defaults() {
        let cfg = ResolvedConfig::default();
        assert_eq!(cfg.output.max_retries, 3);
        assert!(cfg.output.schema.is_none());
    }

    // --- require_approval field in ToolPermissionConfig tests ---

    #[test]
    fn tool_permission_config_require_approval_defaults_to_simple_entries() {
        let cfg = ToolPermissionConfig::default();
        assert!(!cfg.require_approval.is_empty());
        assert!(
            cfg.require_approval
                .iter()
                .all(|entry| matches!(entry, RequireApprovalEntry::Simple(_)))
        );
    }

    #[test]
    fn tool_permission_config_require_approval_simple_toml_round_trip() {
        let original = ToolPermissionConfig {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry::Simple("write_file".to_string())],
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: ToolPermissionConfig =
            toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
        assert_eq!(restored.require_approval[0].tool_name(), "write_file");
    }

    #[test]
    fn tool_permission_config_require_approval_with_options_json_round_trip() {
        // Use JSON since untagged object variant is awkward in TOML inline tables
        let original = ToolPermissionConfig {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry::WithOptions {
                tool: "execute_command".to_string(),
                timeout: Some(600),
                timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
            }],
        };
        let json = serde_json::to_string(&original).expect("serialise failed");
        let restored: ToolPermissionConfig =
            serde_json::from_str(&json).expect("deserialise failed");
        assert_eq!(original, restored);
        assert_eq!(restored.require_approval[0].tool_name(), "execute_command");
    }

    // --- RequireApprovalEntry tests ---

    #[test]
    fn require_approval_entry_simple_tool_name() {
        let entry = RequireApprovalEntry::Simple("write_file".to_string());
        assert_eq!(entry.tool_name(), "write_file");
    }

    #[test]
    fn require_approval_entry_with_options_tool_name() {
        let entry = RequireApprovalEntry::WithOptions {
            tool: "execute_command".to_string(),
            timeout: Some(600),
            timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
        };
        assert_eq!(entry.tool_name(), "execute_command");
    }

    #[test]
    fn require_approval_entry_simple_deserialises_from_string() {
        let json = r#""write_file""#;
        let entry: RequireApprovalEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.tool_name(), "write_file");
    }

    #[test]
    fn require_approval_entry_with_options_deserialises_from_object() {
        let json = r#"{"tool":"execute_command","timeout":600,"timeout_policy":"interrupt"}"#;
        let entry: RequireApprovalEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.tool_name(), "execute_command");
        if let RequireApprovalEntry::WithOptions {
            timeout,
            timeout_policy,
            ..
        } = &entry
        {
            assert_eq!(*timeout, Some(600));
            assert_eq!(*timeout_policy, Some(ApprovalTimeoutPolicy::Interrupt));
        } else {
            panic!("expected WithOptions variant");
        }
    }

    #[test]
    fn require_approval_entry_with_options_partial_fields() {
        // timeout and timeout_policy are optional — object with only tool must work
        let json = r#"{"tool":"shell"}"#;
        let entry: RequireApprovalEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.tool_name(), "shell");
        if let RequireApprovalEntry::WithOptions {
            timeout,
            timeout_policy,
            ..
        } = entry
        {
            assert!(timeout.is_none());
            assert!(timeout_policy.is_none());
        } else {
            panic!("expected WithOptions variant");
        }
    }

    #[test]
    fn require_approval_entry_json_round_trip_simple() {
        let original = RequireApprovalEntry::Simple("write_file".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let restored: RequireApprovalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn require_approval_entry_json_round_trip_with_options() {
        let original = RequireApprovalEntry::WithOptions {
            tool: "execute_command".to_string(),
            timeout: Some(600),
            timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: RequireApprovalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    // --- HitlConfig tests ---

    #[test]
    fn hitl_config_default_timeout_is_300() {
        let cfg = HitlConfig::default();
        assert_eq!(cfg.timeout, 300);
    }

    #[test]
    fn hitl_config_default_policy_is_reject() {
        use swage_core::ApprovalTimeoutPolicy;
        let cfg = HitlConfig::default();
        assert_eq!(cfg.timeout_policy, ApprovalTimeoutPolicy::Reject);
    }

    #[test]
    fn hitl_config_toml_round_trip_defaults() {
        let original = HitlConfig::default();
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: HitlConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn hitl_config_toml_round_trip_with_interrupt_policy() {
        use swage_core::ApprovalTimeoutPolicy;
        let original = HitlConfig {
            timeout: 60,
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: HitlConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(original, restored);
    }

    #[test]
    fn resolved_config_default_includes_hitl_defaults() {
        let cfg = ResolvedConfig::default();
        assert_eq!(
            cfg.hitl.timeout, 300,
            "default HITL timeout should be 300 seconds"
        );
        assert_eq!(
            cfg.hitl.timeout_policy,
            ApprovalTimeoutPolicy::Reject,
            "default HITL timeout policy should be Reject",
        );
    }

    #[test]
    fn pricing_config_toml_round_trip() {
        let mut models = std::collections::HashMap::new();
        models.insert(
            "my-model".to_string(),
            ModelPricing {
                input_price_per_mtok: 100,
                output_price_per_mtok: 200,
            },
        );
        let original = PricingConfig { models };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: PricingConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(restored.models["my-model"].input_price_per_mtok, 100,);
        assert_eq!(restored.models["my-model"].output_price_per_mtok, 200,);
    }
}
