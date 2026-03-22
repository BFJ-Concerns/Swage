//! Hook system — re-exported from `swage-hook`.
//!
//! Lifecycle hooks intercept agent execution events and can modify
//! or block the flow. Register hooks via [`HookRegistry`].

pub use swage_hook::*;
