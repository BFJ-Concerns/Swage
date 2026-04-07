//! Tool system — re-exports from swage-tool and built-in tool implementations.
//!
//! The Tool trait and ToolRegistry are now defined in swage-tool. This module
//! re-exports them so the rest of the gateway can import from `crate::tool`
//! without updating every import site.

pub mod file_list;
pub mod file_read;
pub mod file_search;
pub mod file_write;
pub mod memory;
pub mod plugin;
pub mod python_dispatch;
pub mod python_execute;
pub mod shell;
pub mod url_fetch;
pub mod web_search;
pub mod workspace;

// Re-export the canonical Tool trait, ToolRegistry, and ToolDispatcher from swage-tool.
// These replace the local definitions that previously lived in this file.
pub use swage_tool::dispatch::ToolDispatcher;
pub use swage_tool::registry::ToolRegistry;
pub use swage_tool::tool::Tool;
