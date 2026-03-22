//! Lifecycle hook infrastructure for the Swage framework.
//!
//! Provides a priority-ordered dispatch engine for lifecycle events with
//! configurable timeout enforcement and failure handling. Hooks intercept
//! events and return outcomes that control whether execution continues,
//! is modified, or is blocked.

pub mod registry;
pub mod types;

pub use registry::{HookRegistration, HookRegistry};
pub use types::{FailureMode, Hook, HookEvent, HookOutcome};
