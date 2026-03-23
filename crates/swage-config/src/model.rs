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

/// Tool execution settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolConfig {
    /// Maximum seconds a shell command may run before being killed.
    pub shell_timeout_secs: u64,
    /// Output truncation threshold (bytes) applied to all tool results.
    pub max_output_bytes: usize,
    /// Web search provider configuration. None means web search is disabled.
    pub search: Option<SearchProviderConfig>,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            shell_timeout_secs: 30,
            max_output_bytes: 100_000,
            search: None,
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
}
