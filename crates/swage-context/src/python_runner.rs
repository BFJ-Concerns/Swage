//! Trait for running Python assembly scripts from the context assembly engine.
//!
//! Defined here (not in `swage-python`) to avoid a circular dependency:
//! `swage-python` already depends on `swage-context`. The concrete
//! implementation lives in `swage-python::runner::ConcreteAssemblyRunner`.
//!
//! The trait accepts pre-boxed `ContextElement` slices and a `Tokeniser` so
//! the runner can construct the per-element data structures expected by the
//! Python runtime (e.g. `AssemblyData` in `swage-python::assembly_bindings`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use swage_config::HierarchyLevel;
use swage_core::ChatMessage;

use crate::compaction::PendingCompaction;
use crate::element::ContextElement;
use crate::report::ElementReport;
use crate::tokeniser::Tokeniser;

// ─────────────────────────────────────────────────────────────────────────────
// Result and error types
// ─────────────────────────────────────────────────────────────────────────────

/// Successful output from running a Python assembly script.
pub struct PythonAssemblyResult {
    /// Assembled messages in the order `ctx.add()` was called.
    pub messages: Vec<ChatMessage>,

    /// Per-element reports for each `ctx.add()` call.
    pub element_reports: Vec<ElementReport>,

    /// Deferred compaction intents recorded by `history_older()` calls
    /// that specify a compaction strategy.
    pub pending_compactions: Vec<PendingCompaction>,

    /// Non-fatal warnings accumulated during assembly (e.g. budget exceeded).
    pub warnings: Vec<String>,
}

/// Error category for Python assembly execution failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonAssemblyErrorKind {
    /// Invalid Python syntax in the assembly file.
    Syntax,
    /// Runtime error during execution (missing function, unhandled exception).
    Runtime,
    /// Execution exceeded the wall-clock timeout.
    Timeout,
}

impl std::fmt::Display for PythonAssemblyErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax => write!(f, "syntax"),
            Self::Runtime => write!(f, "runtime"),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

/// Error from Python assembly execution.
pub struct PythonAssemblyError {
    /// Error category.
    pub kind: PythonAssemblyErrorKind,
    /// Human-readable error message. May contain a Python traceback.
    pub message: String,
}

impl std::fmt::Display for PythonAssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Runner trait
// ─────────────────────────────────────────────────────────────────────────────

/// Runs a Python assembly script with pre-computed element data.
///
/// Implementors convert the provided `ContextElement` slices into the element
/// data format expected by the Python runtime, then delegate to the underlying
/// Python interpreter (e.g. `execute_assembly` in `swage-python::assembly`).
///
/// The trait is `Send + Sync` so a shared reference can be passed across the
/// async assembly call chain without additional wrapping.
pub trait PythonAssemblyRunner: Send + Sync {
    /// Run the Python assembly at `path`.
    ///
    /// `elements` and `tokeniser` are used to build per-element accessor data
    /// for the Python `AssemblyContext`. `search_levels` is forwarded to the
    /// runtime so `ctx.include()` can resolve named sub-assemblies.
    fn run(
        &self,
        path: &Path,
        timeout: Duration,
        budget: usize,
        model: &str,
        agent_id: &str,
        elements: &[Box<dyn ContextElement>],
        tokeniser: &dyn Tokeniser,
        search_levels: Vec<(HierarchyLevel, PathBuf)>,
    ) -> Result<PythonAssemblyResult, PythonAssemblyError>;
}
