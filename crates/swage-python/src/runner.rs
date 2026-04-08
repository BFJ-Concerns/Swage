//! Concrete Python assembly runner for the `swage-context` trait boundary.
//!
//! `ConcreteAssemblyRunner` implements `PythonAssemblyRunner` from
//! `swage-context`. It converts the `ContextElement` slices into an
//! serialisable worker payload and delegates to the child-worker runtime.

use std::path::{Path, PathBuf};
use std::time::Duration;

use swage_config::HierarchyLevel;
use swage_config::model::PythonExecuteToolConfig;
use swage_context::element::ContextElement;
use swage_context::python_runner::{
    PythonAssemblyError, PythonAssemblyErrorKind, PythonAssemblyResult, PythonAssemblyRunner,
};
use swage_context::tokeniser::Tokeniser;

use crate::blocking::run_worker_future;
use crate::worker_host::PythonWorkerRuntime;
use crate::worker_protocol::{AssemblyExecutionPayload, AssemblySearchLevel, AssemblyWorkerData};

// ─────────────────────────────────────────────────────────────────────────────
// ConcreteAssemblyRunner
// ─────────────────────────────────────────────────────────────────────────────

/// Concrete implementation of [`PythonAssemblyRunner`] that invokes the
/// child-worker Python runtime via `swage_python::worker_host`.
pub struct ConcreteAssemblyRunner;

/// Configured assembly runner that resolves Python through the shared worker backend.
pub struct ConfiguredAssemblyRunner {
    python_config: PythonExecuteToolConfig,
}

impl ConfiguredAssemblyRunner {
    pub fn new(python_config: PythonExecuteToolConfig) -> Self {
        Self { python_config }
    }
}

impl PythonAssemblyRunner for ConcreteAssemblyRunner {
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
    ) -> Result<PythonAssemblyResult, PythonAssemblyError> {
        ConfiguredAssemblyRunner::new(PythonExecuteToolConfig::default()).run(
            path,
            timeout,
            budget,
            model,
            agent_id,
            elements,
            tokeniser,
            search_levels,
        )
    }
}

impl PythonAssemblyRunner for ConfiguredAssemblyRunner {
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
    ) -> Result<PythonAssemblyResult, PythonAssemblyError> {
        let data = build_assembly_data(elements, tokeniser, search_levels);
        let code = std::fs::read_to_string(path).map_err(|error| PythonAssemblyError {
            kind: PythonAssemblyErrorKind::Runtime,
            message: format!("failed to read assembly file {}: {error}", path.display()),
        })?;
        let runtime = PythonWorkerRuntime::new(self.python_config.clone()).map_err(|error| {
            PythonAssemblyError {
                kind: PythonAssemblyErrorKind::Runtime,
                message: error.to_string(),
            }
        })?;

        let payload = AssemblyExecutionPayload {
            code,
            path: path.display().to_string(),
            budget,
            model: model.to_string(),
            agent_id: agent_id.to_string(),
            data,
        };

        let result = run_worker_future(
            move || async move { runtime.execute_assembly(payload, timeout).await },
            "assembly runner",
        );

        result
            .map(|result| PythonAssemblyResult {
                messages: result.messages,
                element_reports: result.element_reports,
                pending_compactions: result.pending_compactions,
                warnings: result.warnings,
            })
            .map_err(|error| {
                let message = error.to_string();
                let kind = if message.contains("SyntaxError") {
                    PythonAssemblyErrorKind::Syntax
                } else if message.contains("timed out") || message.contains("timeout") {
                    PythonAssemblyErrorKind::Timeout
                } else {
                    PythonAssemblyErrorKind::Runtime
                };
                PythonAssemblyError { kind, message }
            })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AssemblyData construction
// ─────────────────────────────────────────────────────────────────────────────

/// Build an `AssemblyData` struct from the provided context elements.
///
/// Each standard element type is identified by name and its messages / token
/// counts are extracted via the element trait. History gets per-message token
/// counts for the `history_older` / `history_recent` split logic in Python.
/// Unknown element types are silently ignored — the Python API only exposes
/// the four standard accessors (`prompts`, `skills`, `memory`, `history`).
fn build_assembly_data(
    elements: &[Box<dyn ContextElement>],
    tokeniser: &dyn Tokeniser,
    search_levels: Vec<(HierarchyLevel, PathBuf)>,
) -> AssemblyWorkerData {
    // Accumulate each element type. Fields default to empty if the element
    // is absent from the provided slice.
    let mut prompts_messages = vec![];
    let mut prompts_token_count = 0;
    let mut prompts_source = String::new();

    let mut skills_messages = vec![];
    let mut skills_token_count = 0;
    let mut skills_source = String::new();

    let mut memory_messages = vec![];
    let mut memory_token_count = 0;
    let mut memory_source = String::new();

    let mut history_messages = vec![];
    let mut history_token_count = 0;
    let mut history_source = String::new();
    let mut history_per_message_tokens = vec![];

    for element in elements {
        match element.name() {
            "prompts" => {
                prompts_messages = element.content();
                prompts_token_count = element.token_count(tokeniser);
                prompts_source = element.source();
            }
            "skills" => {
                skills_messages = element.content();
                skills_token_count = element.token_count(tokeniser);
                skills_source = element.source();
            }
            "memory" => {
                memory_messages = element.content();
                memory_token_count = element.token_count(tokeniser);
                memory_source = element.source();
            }
            "history" => {
                history_messages = element.content();
                history_token_count = element.token_count(tokeniser);
                history_source = element.source();
                // Count each message individually so Python can split history
                // into "older" and "recent" sub-ranges without a tokeniser.
                history_per_message_tokens = history_messages
                    .iter()
                    .map(|m| tokeniser.count_messages(std::slice::from_ref(m)))
                    .collect();
            }
            // Non-standard elements are not exposed to the Python API.
            _ => {}
        }
    }

    AssemblyWorkerData {
        prompts_messages,
        prompts_token_count,
        prompts_source,
        skills_messages,
        skills_token_count,
        skills_source,
        memory_messages,
        memory_token_count,
        memory_source,
        history_messages,
        history_token_count,
        history_source,
        history_per_message_tokens,
        search_levels: search_levels
            .into_iter()
            .map(|(level, path)| AssemblySearchLevel {
                level,
                path: path.display().to_string(),
            })
            .collect(),
    }
}
