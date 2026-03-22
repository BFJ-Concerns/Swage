//! Tool framework — re-exported from `swage-tool`.
//!
//! The [`tool::Tool`] trait defines the tool interface. [`registry::ToolRegistry`]
//! holds registered tools, [`dispatch::ToolDispatcher`] runs the full
//! execution pipeline (permission check → rate limit → schema validation →
//! execute → leak detection).

pub use swage_tool::*;
