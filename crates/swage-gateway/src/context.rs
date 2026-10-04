//! Topic context for per-turn tool access.
//!
//! `TopicContext` is set as a tokio task-local before each tool execution
//! during a turn, giving tools (e.g., the memory tools) access to the current
//! topic without requiring it to be threaded through every function call.

use std::path::PathBuf;

use uuid::Uuid;

/// Per-turn topic context, available to all tools during execution.
#[derive(Debug, Clone)]
pub struct TopicContext {
    /// The topic this turn belongs to.
    pub topic_id: Uuid,
    /// Slug of the topic (used for config resolution and filesystem paths).
    pub scope: String,
    /// Absolute path to the topic's memory directory.
    ///
    /// Typically: `{swage_root}/topics/{slug}/memory/`
    pub memory_path: PathBuf,
}

tokio::task_local! {
    /// Per-turn topic context, accessible to all tool executions.
    ///
    /// Set by `process_turn` in `turn.rs` before each tool call. Tools like
    /// The memory tools read this to determine which topic's memory directory to use.
    ///
    /// Uses `try_with` rather than `with` to avoid panicking if called outside
    /// a turn (e.g., in unit tests that don't run through `process_turn`).
    pub static TOPIC_CONTEXT: TopicContext;
}
