// Hierarchical TOML configuration with cascade merging, instruction assembly,
// and validation for the Swage platform.

pub mod discovery;
pub mod error;
pub mod instructions;
pub mod merge;
pub mod model;
pub mod prompt_rule;
pub mod resolve;
pub mod skill;
pub mod task;
pub mod validation;

// ---------------------------------------------------------------------------
// Re-exports — public surface of the crate
// ---------------------------------------------------------------------------

// Resolved config types
pub use model::{
    CommunicationConfig, ContextConfig, ContextTransferConfig, ContextTransferMode,
    ContextTransferTargetConfig, HierarchyLevel, HitlConfig, LifecycleConfig, LogLevel,
    LoggingConfig, ObservabilityConfig, OutputConfig, PricingConfig, Protocol, ProviderConfig,
    RequireApprovalEntry, ResolvedConfig, SandboxConfig, SearchProviderConfig, ServerConfig,
    ToolConfig, ToolPermissionConfig, WorkspaceConfig,
};

// Error types
pub use error::{ConfigError, ConfigProblem, ConfigProblemKind};

// Discovery
pub use discovery::{
    AssemblyFile, DiscoveredProject, ResolvedAssembly, discover, discover_assemblies,
    discover_teams, has_default_lens, resolve_assembly, resolve_root,
};

// Merge types — PartialConfig for programmatic overlays (task definitions)
pub use merge::PartialConfig;

// Task definitions
pub use task::TaskDefinition;

// Resolution pipeline — primary public API
pub use resolve::{
    apply_overlay, load_resolved_config, load_resolved_config_for_team, load_resolved_config_from,
    load_resolved_config_from_with_team, validate_files,
};
