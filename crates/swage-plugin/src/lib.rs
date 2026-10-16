//! The plugin kernel: what a plugin is, how it is read from disk, and how its
//! script runs.
//!
//! A plugin is a package — a directory holding a `plugin.toml` manifest and
//! the script it names. The manifest declares what the plugin provides (its
//! tools), the host grants it needs, the secrets it holds and the function
//! it is a variant of; [`manifest`] parses and validates it. [`package`]
//! reads a package from a directory and copies it into an install root under
//! its name and revision. [`runtime`] runs the script in one caged V8
//! isolate of its own, on a thread of its own, with the host API as the only
//! door: every effect the script asks for is answered by a [`PluginHost`]
//! the embedding process supplies, where the process's own grants, approvals
//! and record apply.
//!
//! The kernel knows nothing of the gateway: no storage, no settings, no tool
//! trait. The gateway owns the install records, the enablement, the tool
//! wrapper and the settings surface, and feeds this crate a package and a
//! host.

pub mod manifest;
pub mod package;
pub mod runtime;

pub use manifest::{
    HostGrant, IsolateLimitsDeclaration, MANIFEST_FILE, ManifestError, PluginManifest, Provides,
    SecretDeclaration, ToolDeclaration,
};
pub use package::{PackageError, PluginPackage, install_dir, install_package, read_package};
pub use runtime::{
    HostCallFailure, InvocationContext, PluginError, PluginHost, PluginRuntime, PluginRuntimeState,
};
