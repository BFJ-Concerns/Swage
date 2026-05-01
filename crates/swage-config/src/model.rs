use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    #[default]
    Anthropic,
    Google,
    OpenRouter,
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
}

impl Default for ProviderEndpointConfig {
    fn default() -> Self {
        Self {
            protocol: Protocol::Anthropic,
            api_key_secret_ref: None,
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            base_url: None,
        }
    }
}

/// Configured LLM providers keyed by provider id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCatalogConfig {
    pub default: String,
    #[serde(default)]
    pub entries: HashMap<String, ProviderEndpointConfig>,
}

impl Default for ProviderCatalogConfig {
    fn default() -> Self {
        Self {
            default: "anthropic".to_string(),
            entries: HashMap::from([("anthropic".to_string(), ProviderEndpointConfig::default())]),
        }
    }
}

impl ProviderCatalogConfig {
    pub fn default_provider(&self) -> Option<&ProviderEndpointConfig> {
        self.entries.get(&self.default)
    }
}

/// Model identity and metadata separate from provider connection settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEndpointConfig {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

impl Default for ModelEndpointConfig {
    fn default() -> Self {
        Self {
            provider_id: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
            aliases: Vec::new(),
            context_window: None,
        }
    }
}

/// Configured LLM models keyed by app-level model id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogConfig {
    pub default: String,
    #[serde(default)]
    pub entries: HashMap<String, ModelEndpointConfig>,
}

impl Default for ModelCatalogConfig {
    fn default() -> Self {
        Self {
            default: "sonnet".to_string(),
            entries: HashMap::from([("sonnet".to_string(), ModelEndpointConfig::default())]),
        }
    }
}

impl ModelCatalogConfig {
    pub fn default_model(&self) -> Option<&ModelEndpointConfig> {
        self.entries.get(&self.default)
    }
}

/// Working directory for the agent workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub path: String,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            path: ".".to_string(),
        }
    }
}

/// Sandbox isolation settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub enabled: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

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

/// Web search provider configuration. Supports self-hosted instances (e.g. SearxNG)
/// and any provider that exposes a compatible search API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchProviderConfig {
    /// Which search backend to use (e.g. "searxng").
    pub provider_type: String,
    /// The search provider's API endpoint (e.g. "http://localhost:8080").
    pub base_url: String,
    /// Optional API key — required by some hosted providers, unused for self-hosted.
    pub api_key: Option<String>,
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
    /// an object with per-tool timeout and policy overrides. Defaults to empty
    /// — high-risk defaults apply unless explicitly overridden.
    #[serde(default)]
    pub require_approval: Vec<RequireApprovalEntry>,
}

impl Default for ToolPermissionConfig {
    fn default() -> Self {
        Self {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: default_high_risk_tool_approvals(),
        }
    }
}

fn default_high_risk_tool_approvals() -> Vec<RequireApprovalEntry> {
    ["shell", "file_write", "python_execute"]
        .into_iter()
        .map(|tool| RequireApprovalEntry::Simple(tool.to_string()))
        .collect()
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Assembly strategy name to use (must match a registered assembly).
    pub assembly: String,
    /// Seconds allowed for assembly before fail-open triggers.
    /// Clamped to the range 1–60 at validation time.
    pub timeout: u64,
    /// Token budget for the assembled context. `None` means the model's
    /// context window limit is used as the upper bound.
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

/// Per-model token pricing in microcents per million tokens.
///
/// Prices are stored as integers to avoid floating-point rounding when
/// computing costs across large token counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Input (prompt) token price in microcents per million tokens.
    pub input_price_per_mtok: i64,
    /// Output (completion) token price in microcents per million tokens.
    pub output_price_per_mtok: i64,
}

/// Model pricing configuration — optional per-model cost data for observability.
///
/// When a model is absent from the map, `compute_cost` returns `None`
/// rather than zero so callers can distinguish "free/unknown" from "configured zero".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PricingConfig {
    /// Map of model identifier → pricing data.
    pub models: HashMap<String, ModelPricing>,
}

impl PricingConfig {
    /// Compute the cost of a call in microcents, given model and token counts.
    ///
    /// Returns `None` if no pricing is configured for `model`.
    pub fn compute_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> Option<i64> {
        let pricing = self.models.get(model)?;
        let input_cost = (input_tokens as i64) * pricing.input_price_per_mtok / 1_000_000;
        let output_cost = (output_tokens as i64) * pricing.output_price_per_mtok / 1_000_000;
        Some(input_cost + output_cost)
    }
}

/// Observability settings for the gateway.
///
/// When `force_on` is true, all WebSocket connections are treated as
/// subscribed to every observability channel — no explicit subscribe
/// message is required from the client. Useful for development and
/// single-user deployments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// When true, all observability events are delivered to every connected
    /// client regardless of their subscription state.
    pub force_on: bool,
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
    pub workspace: WorkspaceConfig,
    pub sandbox: SandboxConfig,
    pub logging: LoggingConfig,
    pub tools: ToolConfig,
    pub context: ContextConfig,
    /// Per-model pricing data for cost derivation. Empty by default — cost
    /// computation returns None for any model absent from the map.
    pub pricing: PricingConfig,
    /// Observability settings — controls channel delivery behaviour.
    pub observability: ObservabilityConfig,
    /// Output schema validation settings — JSON Schema and retry limits.
    /// Defaults to no schema (validation disabled) with 3 retries.
    pub output: OutputConfig,
    /// Inter-agent communication settings — context transfer policy.
    /// Defaults to no communication (context transfer mode = none).
    pub communication: CommunicationConfig,
    /// Assembled instruction text from all hierarchy levels, in order.
    pub instructions: String,
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
            .unwrap_or_default()
    }

    pub fn set_default_model_id(&mut self, model_id: String) {
        if let Some(model) = self.models.entries.get_mut(&self.models.default) {
            model.model_id = model_id;
        }
    }
}

/// Which level of the hierarchy a value or instruction came from.
///
/// Used by instruction assembly to attribute sections to their source level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HierarchyLevel {
    Global,
    Bench,
    Team,
    Agent,
}

impl std::fmt::Display for HierarchyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Global => "Global",
            Self::Bench => "Bench",
            Self::Team => "Team",
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

    #[test]
    fn server_config_defaults() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 3000);
    }

    #[test]
    fn workspace_config_defaults() {
        let cfg = WorkspaceConfig::default();
        assert_eq!(cfg.path, ".");
    }

    #[test]
    fn sandbox_config_defaults() {
        let cfg = SandboxConfig::default();
        assert!(cfg.enabled);
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
        assert_eq!(cfg.providers.default, "anthropic");
        assert_eq!(cfg.models.default, "sonnet");
        assert_eq!(cfg.default_model_id(), "claude-sonnet-4-20250514");
        assert_eq!(cfg.workspace.path, ".");
        assert!(cfg.sandbox.enabled);
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
        let cfg = SearchProviderConfig {
            provider_type: "searxng".to_string(),
            base_url: "http://localhost:8080".to_string(),
            api_key: None,
        };
        assert_eq!(cfg.provider_type, "searxng");
        assert_eq!(cfg.base_url, "http://localhost:8080");
        assert_eq!(cfg.api_key, None);
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
        assert_eq!(cfg.timeout, 5);
        assert_eq!(cfg.budget, None);
    }

    #[test]
    fn context_config_serialise_round_trip() {
        let original = ContextConfig {
            assembly: "custom".to_string(),
            timeout: 10,
            budget: Some(4096),
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

    // --- RequireApprovalEntry tests [cqn0] ---

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
        assert!(cfg.bundles.is_empty());
        assert_eq!(
            cfg.require_approval
                .iter()
                .map(RequireApprovalEntry::tool_name)
                .collect::<Vec<_>>(),
            vec!["shell", "file_write", "python_execute"]
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
        assert!(cfg.permissions.bundles.is_empty());
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
    fn tool_permission_config_require_approval_defaults_gate_high_risk_tools() {
        let cfg = ToolPermissionConfig::default();
        assert_eq!(
            cfg.require_approval
                .iter()
                .map(RequireApprovalEntry::tool_name)
                .collect::<Vec<_>>(),
            vec!["shell", "file_write", "python_execute"]
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
