use std::collections::HashMap;
use serde::{Deserialize, Serialize};

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

/// LLM provider settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub protocol: Protocol,
    pub model: String,
    pub api_key_env: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            protocol: Protocol::Anthropic,
            model: "claude-sonnet-4-20250514".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
        }
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

/// Agent lifecycle settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleConfig {
    /// Seconds of inactivity before the agent session is considered idle.
    pub idle_timeout: u64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self { idle_timeout: 300 }
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
/// When no permissions are configured at any level, the default is allow-all
/// to preserve backward compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionConfig {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub bundles: HashMap<String, Vec<String>>,
}

impl Default for ToolPermissionConfig {
    fn default() -> Self {
        Self {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
        }
    }
}

/// Tool execution settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolConfig {
    /// Maximum seconds a shell command may run before being killed.
    pub shell_timeout_secs: u64,
    /// Output truncation threshold (bytes) applied to all tool results.
    pub max_output_bytes: usize,
    /// Web search provider configuration. None means web search is disabled.
    pub search: Option<SearchProviderConfig>,
    /// Tool permission rules — controls which tools an agent may invoke.
    /// Defaults to allow-all when unconfigured (backward compatibility).
    #[serde(default)]
    pub permissions: ToolPermissionConfig,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            shell_timeout_secs: 30,
            max_output_bytes: 100_000,
            search: None,
            permissions: ToolPermissionConfig::default(),
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
        let input_cost  = (input_tokens  as i64) * pricing.input_price_per_mtok  / 1_000_000;
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// When true, all observability events are delivered to every connected
    /// client regardless of their subscription state.
    pub force_on: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self { force_on: false }
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
    pub provider: ProviderConfig,
    pub workspace: WorkspaceConfig,
    pub sandbox: SandboxConfig,
    pub lifecycle: LifecycleConfig,
    pub logging: LoggingConfig,
    pub tools: ToolConfig,
    pub context: ContextConfig,
    /// Per-model pricing data for cost derivation. Empty by default — cost
    /// computation returns None for any model absent from the map.
    pub pricing: PricingConfig,
    /// Observability settings — controls channel delivery behaviour.
    pub observability: ObservabilityConfig,
    /// Assembled instruction text from all hierarchy levels, in order.
    pub instructions: String,
}

/// Which level of the hierarchy a value or instruction came from.
///
/// Used by instruction assembly to attribute sections to their source level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HierarchyLevel {
    Global,
    Lens,
    Project,
    Team,
    Agent,
}

impl std::fmt::Display for HierarchyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Global  => "Global",
            Self::Lens    => "Lens",
            Self::Project => "Project",
            Self::Team    => "Team",
            Self::Agent   => "Agent",
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
    fn provider_config_defaults() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.protocol, Protocol::Anthropic);
        assert_eq!(cfg.model, "claude-sonnet-4-20250514");
        assert_eq!(cfg.api_key_env, "ANTHROPIC_API_KEY");
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
    fn lifecycle_config_defaults() {
        let cfg = LifecycleConfig::default();
        assert_eq!(cfg.idle_timeout, 300);
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
        assert_eq!(cfg.provider.protocol, Protocol::Anthropic);
        assert_eq!(cfg.provider.model, "claude-sonnet-4-20250514");
        assert_eq!(cfg.provider.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(cfg.workspace.path, ".");
        assert!(cfg.sandbox.enabled);
        assert_eq!(cfg.lifecycle.idle_timeout, 300);
        assert_eq!(cfg.logging.level, LogLevel::Info);
        assert_eq!(cfg.instructions, "");
    }

    #[test]
    fn tool_config_defaults() {
        let cfg = ToolConfig::default();
        assert_eq!(cfg.shell_timeout_secs, 30);
        assert_eq!(cfg.max_output_bytes, 100_000);
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
        assert_eq!(cfg.compute_cost("test-model", 1_000_000, 500_000), Some(1050));
    }

    #[test]
    fn compute_cost_zero_tokens_is_zero() {
        let mut cfg = PricingConfig::default();
        cfg.models.insert(
            "m".to_string(),
            ModelPricing { input_price_per_mtok: 300, output_price_per_mtok: 1500 },
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
        let restored: ObservabilityConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert!(restored.force_on);
    }

    // --- ToolPermissionConfig tests ---

    #[test]
    fn tool_permission_config_default_is_allow_all() {
        let cfg = ToolPermissionConfig::default();
        assert_eq!(cfg.allow, vec!["*".to_string()]);
        assert!(cfg.deny.is_empty());
        assert!(cfg.bundles.is_empty());
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
            max_output_bytes: 50_000,
            search: None,
            permissions: ToolPermissionConfig {
                allow: vec!["echo".into()],
                deny: vec!["shell_exec".into()],
                bundles,
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

    #[test]
    fn pricing_config_toml_round_trip() {
        let mut models = std::collections::HashMap::new();
        models.insert(
            "my-model".to_string(),
            ModelPricing { input_price_per_mtok: 100, output_price_per_mtok: 200 },
        );
        let original = PricingConfig { models };
        let serialised = toml::to_string(&original).expect("serialise failed");
        let restored: PricingConfig = toml::from_str(&serialised).expect("deserialise failed");
        assert_eq!(
            restored.models["my-model"].input_price_per_mtok,
            100,
        );
        assert_eq!(
            restored.models["my-model"].output_price_per_mtok,
            200,
        );
    }
}
