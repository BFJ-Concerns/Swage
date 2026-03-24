use std::collections::HashMap;
use serde::{Deserialize, Serialize};
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
    /// Tools that require explicit human approval before execution.
    ///
    /// Each entry is either a plain tool name (uses global HITL defaults) or
    /// an object with per-tool timeout and policy overrides. Defaults to empty
    /// — no approval gates unless explicitly configured.
    #[serde(default)]
    pub require_approval: Vec<RequireApprovalEntry>,
}

impl Default for ToolPermissionConfig {
    fn default() -> Self {
        Self {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![],
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


/// Context transfer policy for a single target agent.
///
/// Governs what context crosses agent boundaries during delegation.
/// Evaluated at send time — the Delegate message is self-contained after creation.
// TODO CommunicationConfig and context transfer policy model
// tags: forge:inter-agent-communication
// Define ContextTransferMode enum (None, Explicit, Summary) with serde support,
// Default = None.
// Define ContextTransferTargetConfig { mode: ContextTransferMode,
//   allowed_fields: Vec<String>, summary_budget: Option<u32> }.
// Define ContextTransferConfig { mode: ContextTransferMode,
//   allowed_fields: Vec<String>, summary_budget: Option<u32>,
//   targets: HashMap<String, ContextTransferTargetConfig> }.
// Define CommunicationConfig { context_transfer: ContextTransferConfig }.
// Add `communication: CommunicationConfig` to ResolvedConfig with Default.
// Derive Debug, Clone, PartialEq, Default, Serialize, Deserialize on all.
// Add unit tests: defaults, TOML round-trips, per-target overrides.
// Success: types compile, default mode is None, serialisation works.
// ---

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
    /// Output schema validation settings — JSON Schema and retry limits.
    /// Defaults to no schema (validation disabled) with 3 retries.
    pub output: OutputConfig,
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

    // --- RequireApprovalEntry tests [cqn0] ---

    #[test]
    fn require_approval_entry_from_plain_string() {
        // TOML requires top-level tables, so use a wrapper for array entries.
        #[derive(Debug, Deserialize)]
        struct W { items: Vec<RequireApprovalEntry> }
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
        if let RequireApprovalEntry::WithOptions { timeout, timeout_policy, .. } = &entry {
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
        if let RequireApprovalEntry::WithOptions { timeout, timeout_policy, .. } = &entry {
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
    fn tool_permission_config_default_is_allow_all() {
        let cfg = ToolPermissionConfig::default();
        assert_eq!(cfg.allow, vec!["*".to_string()]);
        assert!(cfg.deny.is_empty());
        assert!(cfg.bundles.is_empty());
        // No approval gates by default — preserves backward compatibility.
        assert!(cfg.require_approval.is_empty());
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
            max_output_bytes: 50_000,
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
            schema: Some(serde_json::json!({"type": "object", "properties": {"answer": {"type": "string"}}})),
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
    fn tool_permission_config_require_approval_defaults_to_empty() {
        let cfg = ToolPermissionConfig::default();
        assert!(cfg.require_approval.is_empty());
    }

    #[test]
    fn tool_permission_config_require_approval_simple_toml_round_trip() {
        let original = ToolPermissionConfig {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![
                RequireApprovalEntry::Simple("write_file".to_string()),
            ],
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
            require_approval: vec![
                RequireApprovalEntry::WithOptions {
                    tool: "execute_command".to_string(),
                    timeout: Some(600),
                    timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
                },
            ],
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
        if let RequireApprovalEntry::WithOptions { timeout, timeout_policy, .. } = &entry {
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
        if let RequireApprovalEntry::WithOptions { timeout, timeout_policy, .. } = entry {
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
        assert_eq!(cfg.hitl.timeout, 300, "default HITL timeout should be 300 seconds");
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
