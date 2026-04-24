//! Bench context for per-turn tool access.
//!
//! `BenchContext` is set as a tokio task-local before each tool execution
//! during a turn, giving tools (e.g., MemoryTool) access to the current
//! bench without requiring it to be threaded through every function call.

use std::path::PathBuf;

use uuid::Uuid;

/// Per-turn bench context, available to all tools during execution.
#[derive(Debug, Clone)]
pub struct BenchContext {
    /// The bench this turn belongs to.
    pub bench_id: Uuid,
    /// Slug of the bench (used for config resolution and filesystem paths).
    pub bench_slug: String,
    /// Absolute path to the bench's memory directory.
    ///
    /// Typically: `~/.swage/benches/{slug}/memory/`
    pub memory_path: PathBuf,
}

tokio::task_local! {
    /// Per-turn bench context, accessible to all tool executions.
    ///
    /// Set by `process_turn` in `turn.rs` before each tool call. Tools like
    /// MemoryTool read this to determine which bench's memory directory to use.
    ///
    /// Uses `try_with` rather than `with` to avoid panicking if called outside
    /// a turn (e.g., in unit tests that don't run through `process_turn`).
    pub static BENCH_CONTEXT: BenchContext;
}
