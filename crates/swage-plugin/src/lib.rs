//! The plugin kernel: what a plugin is, how it is read from disk, and how it
//! runs.
//!
//! A plugin is a package — a directory holding a `plugin.toml` manifest and
//! the script or process command it names. The manifest declares what the
//! plugin provides (its tools), the host grants it needs, the secrets it
//! holds and the function it is a variant of; [`manifest`] parses and
//! validates it. [`package`] reads a package from a directory and copies it
//! into an install root under its name and revision. A plugin runs one of
//! two ways, and the host API is the only door for both: [`runtime`] runs a
//! script in one caged V8 isolate of its own, on a thread of its own;
//! [`process`] brokers a process per session — started on first use,
//! stopped when the session closes, after an idle period or at shutdown —
//! that speaks the same host API over a socket. Every effect a plugin asks
//! for is answered by a [`PluginHost`] the embedding process supplies, where
//! the process's own grants, approvals and record apply.
//!
//! The kernel knows nothing of the gateway: no storage, no settings, no tool
//! trait. The gateway owns the install records, the enablement, the tool
//! wrapper and the settings surface, and feeds this crate a package and a
//! host.

pub mod manifest;
pub mod package;
pub mod process;
pub mod runtime;

pub use manifest::{
    HookDeclaration, HostGrant, IsolateLimitsDeclaration, MANIFEST_FILE, ManifestError,
    PluginManifest, ProcessDeclaration, Profile, Provides, SecretDeclaration, SlotDeclaration,
    ToolDeclaration,
};
pub use package::{PackageError, PluginPackage, install_dir, install_package, read_package};
pub use process::{
    Held, PlainLauncher, ProcessKeeper, ProcessLauncher, ProcessObserver, ProcessPlugin,
    ProcessSpec, ServingProcess, SilentObserver,
};
pub use runtime::{
    HostCallFailure, InvocationContext, PluginError, PluginHost, PluginRuntime, PluginRuntimeState,
};
