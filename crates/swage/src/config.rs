//! Hierarchical configuration — re-exported from `swage-config`.
//!
//! [`ResolvedConfig`] is the fully-merged view after the global → project →
//! agent cascade. Use [`load_resolved_config`] to load and validate from disk.

pub use swage_config::*;
