//! Project context for per-turn tool access.
//!
//! `ProjectContext` is set as a tokio task-local before each tool execution
//! during a turn, giving tools (e.g., MemoryTool) access to the current
//! project without requiring it to be threaded through every function call.

use std::path::PathBuf;

use uuid::Uuid;

/// Per-turn project context, available to all tools during execution.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// The project this turn belongs to.
    pub project_id: Uuid,
    /// Slug of the project (used for config resolution and filesystem paths).
    pub project_slug: String,
    /// Absolute path to the project's memory directory.
    ///
    /// Typically: `~/.swage/projects/{slug}/memory/`
    pub memory_path: PathBuf,
}

tokio::task_local! {
    /// Per-turn project context, accessible to all tool executions.
    ///
    /// Set by `process_turn` in `turn.rs` before each tool call. Tools like
    /// MemoryTool read this to determine which project's memory directory to use.
    ///
    /// Uses `try_with` rather than `with` to avoid panicking if called outside
    /// a turn (e.g., in unit tests that don't run through `process_turn`).
    pub static PROJECT_CONTEXT: ProjectContext;
}
