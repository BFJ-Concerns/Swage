//! Sandbox execution — re-exported from `swage-sandbox`.
//!
//! The [`Sandbox`] trait abstracts platform isolation. [`BubblewrapSandbox`]
//! provides namespace + Landlock isolation on Linux; [`FallbackSandbox`]
//! runs in-process on unsupported platforms. Tool dispatch and LLM requests
//! are proxied via IPC so credentials never enter the agent process.

pub use swage_sandbox::*;
