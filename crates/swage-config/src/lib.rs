// Hierarchical TOML configuration with cascade merging, instruction assembly,
// and validation for the Swage platform.

pub mod agent_guidance;
pub mod discovery;
pub mod error;
pub mod instructions;
mod masked_string;
pub mod merge;
pub mod model;
pub mod prompt_rule;
pub mod resolve;
pub mod skill;
pub mod validation;

/// Reserved identity used for bare workflow workers.
///
/// It must never resolve an authored agent directory or appear in discovery.
pub const RESERVED_WORKFLOW_AGENT_ID: &str = "__workflow_ephemeral__";

// ---------------------------------------------------------------------------
// Re-exports — public surface of the crate
// ---------------------------------------------------------------------------

// Resolved config types
pub use masked_string::MaskedString;
pub use model::{
    AttachmentsConfig, BrowserScreenshotPolicy, BrowserToolConfig, ChannelsConfig,
    CommunicationConfig, ContextConfig, ContextTransferConfig, ContextTransferField,
    ContextTransferMode, ContextTransferTargetConfig, GenerationConfig, GenerationPresetConfig,
    HierarchyLevel, HitlConfig, LogLevel, LoggingConfig, MemoryConfig, ModelCapabilitiesConfig,
    ModelCatalogConfig, ModelEndpointConfig, ModelPricing, ObservabilityConfig, OutputConfig,
    PricingConfig, Protocol, ProviderCatalogConfig, ProviderEndpointConfig, ProviderRoute,
    PythonExecuteToolConfig, PythonRuntimePolicy, RequireApprovalEntry, ResolvedConfig,
    SandboxConfig, SearchProviderConfig, ServerConfig, SessionsConfig, TelegramChannelConfig,
    ToolConfig, ToolPermissionConfig, VoiceConfig, resolve_provider_route,
};

// Error types
pub use error::{ConfigError, ConfigProblem, ConfigProblemKind};

// Discovery
pub use discovery::{DiscoveredTopic, discover, effective_root_path, resolve_root};

// Merge types — PartialConfig for programmatic overlays (task definitions)
pub use merge::PartialConfig;

// Resolution pipeline — primary public API
pub use resolve::{
    apply_overlay, configuration_hierarchy, load_resolved_config, load_resolved_config_from,
    validate_files,
};
