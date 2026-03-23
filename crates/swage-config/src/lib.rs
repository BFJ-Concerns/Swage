// Hierarchical TOML configuration with cascade merging, instruction assembly,
// and validation for the Swage platform.

pub mod model;
pub mod error;
pub mod merge;
pub mod validation;
pub mod instructions;
pub mod discovery;
pub mod resolve;
pub mod prompt_rule;
pub mod skill;

// ---------------------------------------------------------------------------
// Re-exports — public surface of the crate
// ---------------------------------------------------------------------------

// Resolved config types
pub use model::{
    ContextConfig,
    HierarchyLevel,
    LifecycleConfig,
    LogLevel,
    LoggingConfig,
    Protocol,
    ProviderConfig,
    ResolvedConfig,
    SandboxConfig,
    SearchProviderConfig,
    ServerConfig,
    ToolConfig,
    WorkspaceConfig,
};

// Error types
pub use error::{ConfigError, ConfigProblem, ConfigProblemKind};

// Discovery
pub use discovery::{DiscoveredProject, discover, resolve_root, has_default_lens};

// Resolution pipeline — primary public API
pub use resolve::{load_resolved_config, load_resolved_config_from, validate_files};
