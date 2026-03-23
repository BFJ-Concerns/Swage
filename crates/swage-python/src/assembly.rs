//! Assembly Python execution — reads and runs `.py` assembly configs.
//!
//! Entry point for programmable context assembly. Separate from PTC's
//! `execute_code`: this reads a `.py` file from disk, injects the
//! `swage_context` namespace (NOT the `swage` PTC namespace), creates an
//! `AssemblyContext` with pre-computed element data, calls the Python
//! `assemble(ctx)` function, and extracts results via `take_output()`.
//!
//! Timeout enforcement uses the same `mpsc` + `PyErr_SetInterrupt` mechanism
//! as PTC — a dedicated thread holds the GIL while Python runs, and the
//! calling thread interrupts via CPython's async signal mechanism if the
//! wall-clock deadline expires.

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use tracing::{debug, warn};

use swage_context::compaction::PendingCompaction;
use swage_context::report::ElementReport;
use swage_core::ChatMessage;

use crate::assembly_bindings::{AssemblyContext, AssemblyData};

// ─────────────────────────────────────────────────────────────────────────────
// Result and error types
// ─────────────────────────────────────────────────────────────────────────────

/// Successful output from running a Python assembly config.
#[derive(Debug)]
pub struct AssemblyExecutionResult {
    /// Accumulated messages in the order `ctx.add()` was called.
    pub messages: Vec<ChatMessage>,

    /// Per-element reports for each `ctx.add()` call.
    pub element_reports: Vec<ElementReport>,

    /// Deferred compaction intents (e.g. from `history_older` with a strategy).
    pub pending_compactions: Vec<PendingCompaction>,

    /// Non-fatal warnings accumulated during assembly.
    pub warnings: Vec<String>,
}

/// Error category for assembly execution failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyErrorKind {
    /// Invalid Python syntax in the assembly file.
    Syntax,
    /// Runtime error during execution (missing function, exception, etc.).
    Runtime,
    /// Execution exceeded the wall-clock timeout.
    Timeout,
    /// Sandbox violation (reserved for future use).
    Sandbox,
}

impl std::fmt::Display for AssemblyErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax => write!(f, "syntax"),
            Self::Runtime => write!(f, "runtime"),
            Self::Timeout => write!(f, "timeout"),
            Self::Sandbox => write!(f, "sandbox"),
        }
    }
}

/// Error from assembly execution.
#[derive(Debug)]
pub struct AssemblyExecutionError {
    /// Error category.
    pub kind: AssemblyErrorKind,
    /// Human-readable error message.
    pub message: String,
    /// Python traceback when the error originates from a Python exception.
    pub traceback: Option<String>,
}

impl std::fmt::Display for AssemblyExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for AssemblyExecutionError {}

// ─────────────────────────────────────────────────────────────────────────────
// execute_assembly
// ─────────────────────────────────────────────────────────────────────────────

/// Run a Python assembly config file.
///
/// Reads the `.py` file at `path`, spawns a dedicated thread for execution,
/// injects the `swage_context` namespace module, creates an `AssemblyContext`
/// with the provided element data, calls `assemble(ctx)`, and extracts
/// results. Enforces a wall-clock `timeout` via `PyErr_SetInterrupt`.
///
/// The assembly bindings module provides element accessors and budget queries
/// but does NOT expose tool dispatch functions.
pub fn execute_assembly(
    path: &Path,
    timeout: Duration,
    budget: usize,
    model: String,
    agent_id: String,
    data: AssemblyData,
) -> Result<AssemblyExecutionResult, AssemblyExecutionError> {
    // Read the .py file from disk.
    let code = fs::read_to_string(path).map_err(|e| AssemblyExecutionError {
        kind: AssemblyErrorKind::Runtime,
        message: format!("failed to read assembly file {}: {e}", path.display()),
        traceback: None,
    })?;

    let path_str = path.display().to_string();
    let (result_tx, result_rx) = mpsc::channel();

    // Spawn a dedicated thread — same pattern as PTC's execute_code.
    let handle = thread::spawn(move || {
        let result = Python::with_gil(|py| {
            run_assembly(py, &code, &path_str, budget, model, agent_id, data)
        });
        result_tx.send(result).ok();
    });

    // Wait for the result with timeout.
    match result_rx.recv_timeout(timeout) {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            debug!(
                timeout_secs = timeout.as_secs(),
                "assembly execution timed out, interrupting"
            );
            // Interrupt the Python thread — GIL-safe, same mechanism as PTC.
            unsafe { pyo3::ffi::PyErr_SetInterrupt(); }
            // Wait briefly for the interrupted thread to finish cleanup.
            let _ = result_rx.recv_timeout(Duration::from_secs(2));

            Err(AssemblyExecutionError {
                kind: AssemblyErrorKind::Timeout,
                message: format!(
                    "assembly execution exceeded {}s timeout",
                    timeout.as_secs_f64()
                ),
                traceback: None,
            })
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            warn!("assembly execution thread disconnected unexpectedly");
            let _ = handle.join();
            Err(AssemblyExecutionError {
                kind: AssemblyErrorKind::Runtime,
                message: "assembly execution terminated: internal error (thread lost)".into(),
                traceback: None,
            })
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Python wrapper template
// ─────────────────────────────────────────────────────────────────────────────

/// Python wrapper that compiles and runs the assembly file, then calls
/// `assemble(ctx)`. Errors are captured into `_asm_err` / `_asm_err_kind`
/// rather than propagating — the Rust side inspects these after execution.
///
/// Injected locals (set by Rust before running):
/// - `_asm_code`: assembly file source code (str)
/// - `_asm_path`: file path for tracebacks (str)
/// - `_asm_ctx`: the `AssemblyContext` instance
///
/// Output locals (read by Rust after running):
/// - `_asm_err`: error message (None on success)
/// - `_asm_err_kind`: "syntax" | "runtime" | "timeout" (None on success)
const ASSEMBLY_WRAPPER: &str = concat!(
    "import traceback as _tb\n",
    "_asm_err = None\n",
    "_asm_err_kind = None\n",
    "_asm_ns = {}\n",
    "\n",
    // Compile and exec the assembly file into a dedicated namespace.
    "try:\n",
    "    ", "exec", "(compile(_asm_code, _asm_path, '", "exec", "'), _asm_ns)\n",
    "except SyntaxError:\n",
    "    _asm_err_kind = 'syntax'\n",
    "    _asm_err = _tb.format_exc()\n",
    "except BaseException:\n",
    "    _asm_err_kind = 'runtime'\n",
    "    _asm_err = _tb.format_exc()\n",
    "\n",
    // If compilation succeeded, check for assemble() and call it.
    "if _asm_err is None:\n",
    "    if 'assemble' not in _asm_ns:\n",
    "        _asm_err_kind = 'runtime'\n",
    "        _asm_err = 'assembly file does not define an assemble(ctx) function'\n",
    "    else:\n",
    "        try:\n",
    "            _asm_ns['assemble'](_asm_ctx)\n",
    "        except KeyboardInterrupt:\n",
    "            _asm_err_kind = 'timeout'\n",
    "            _asm_err = 'assembly execution interrupted by timeout'\n",
    "        except BaseException:\n",
    "            _asm_err_kind = 'runtime'\n",
    "            _asm_err = _tb.format_exc()\n",
);

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Run the assembly inside the GIL — injects module, creates context,
/// executes the wrapper, and extracts results.
fn run_assembly(
    py: Python<'_>,
    code: &str,
    path: &str,
    budget: usize,
    model: String,
    agent_id: String,
    data: AssemblyData,
) -> Result<AssemblyExecutionResult, AssemblyExecutionError> {
    // Inject the swage_context module (assembly bindings, NOT PTC tool bindings).
    inject_assembly_module(py).map_err(|e| AssemblyExecutionError {
        kind: AssemblyErrorKind::Runtime,
        message: format!("failed to inject swage_context module: {e}"),
        traceback: None,
    })?;

    // Create the AssemblyContext with pre-computed element data.
    let context = AssemblyContext::with_data(budget, model, agent_id, data);
    let ctx_py = Py::new(py, context).map_err(|e| AssemblyExecutionError {
        kind: AssemblyErrorKind::Runtime,
        message: format!("failed to create AssemblyContext: {e}"),
        traceback: None,
    })?;

    // Set up locals for the wrapper: assembly code, file path, and context.
    let locals = PyDict::new(py);
    locals.set_item("_asm_code", code).unwrap();
    locals.set_item("_asm_path", path).unwrap();
    locals.set_item("_asm_ctx", &ctx_py).unwrap();

    // Run the wrapper script.
    let wrapper = CString::new(ASSEMBLY_WRAPPER)
        .expect("assembly wrapper template should not contain NUL bytes");
    let wrapper_cstr = std::ffi::CStr::from_bytes_with_nul(wrapper.as_bytes_with_nul())
        .expect("CString should produce valid CStr");

    if let Err(e) = py.run(wrapper_cstr, None, Some(&locals)) {
        return Err(AssemblyExecutionError {
            kind: AssemblyErrorKind::Runtime,
            message: format!("internal wrapper error: {e}"),
            traceback: None,
        });
    }

    // Check whether the wrapper recorded an error.
    let err_kind: Option<String> = locals
        .get_item("_asm_err_kind")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok());

    if let Some(kind_str) = err_kind {
        let message = locals
            .get_item("_asm_err")
            .ok()
            .flatten()
            .and_then(|v| v.extract::<String>().ok())
            .unwrap_or_else(|| "unknown error".to_string());

        let kind = match kind_str.as_str() {
            "syntax" => AssemblyErrorKind::Syntax,
            "timeout" => AssemblyErrorKind::Timeout,
            _ => AssemblyErrorKind::Runtime,
        };

        // Python tracebacks start with "Traceback (most recent call last)".
        // When present, the message IS the traceback.
        let traceback = if message.starts_with("Traceback") {
            Some(message.clone())
        } else {
            None
        };

        return Err(AssemblyExecutionError {
            kind,
            message,
            traceback,
        });
    }

    // Success — extract accumulated results from the context.
    let mut ctx_ref = ctx_py.borrow_mut(py);
    let (messages, element_reports, pending_compactions, warnings) = ctx_ref.take_output();

    Ok(AssemblyExecutionResult {
        messages,
        element_reports,
        pending_compactions,
        warnings,
    })
}

/// Inject the `swage_context` module into the Python interpreter.
///
/// Creates a module with `AssemblyContext` and `PyElement` classes, registered
/// in `sys.modules` and builtins. This is separate from the `swage` PTC
/// module — assembly code gets element accessors, NOT tool dispatch functions.
fn inject_assembly_module(py: Python<'_>) -> PyResult<()> {
    let module = PyModule::new(py, "swage_context")?;
    crate::assembly_bindings::register(&module)?;

    // Register in sys.modules so `import swage_context` works.
    let sys = py.import("sys")?;
    let modules = sys.getattr("modules")?;
    modules.set_item("swage_context", &module)?;

    // Inject into builtins so it's accessible without import.
    let builtins = py.import("builtins")?;
    builtins.setattr("swage_context", &module)?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly_bindings::AssemblyData;
    use swage_core::{ChatMessage, Role};
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Minimal element data for tests — one prompt and a short history.
    fn simple_assembly_data() -> AssemblyData {
        AssemblyData {
            prompts_messages: vec![ChatMessage::new(Role::System, "You are helpful.")],
            prompts_token_count: 10,
            prompts_source: "prompt rules".to_string(),
            skills_messages: vec![],
            skills_token_count: 0,
            skills_source: "none".to_string(),
            memory_messages: vec![],
            memory_token_count: 0,
            memory_source: "none".to_string(),
            history_messages: vec![
                ChatMessage::new(Role::User, "Hello"),
                ChatMessage::new(Role::Assistant, "Hi there"),
            ],
            history_token_count: 10,
            history_source: "thread-1".to_string(),
            history_per_message_tokens: vec![5, 5],
            search_levels: Vec::new(),
        }
    }

    /// Write Python code to a temporary file.
    fn write_temp_py(code: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".py").unwrap();
        f.write_all(code.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    // ── Success cases ────────────────────────────────────────────────────────

    #[test]
    fn valid_assembly_returns_messages_in_add_order() {
        let py_code = r#"
def assemble(ctx):
    prompts = ctx.prompts()
    ctx.add(prompts)
    history = ctx.history_recent()
    ctx.add(history)
"#;
        let file = write_temp_py(py_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap();

        // 1 prompt message + 2 history messages.
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.element_reports.len(), 2);
        assert_eq!(result.element_reports[0].name, "prompts");
        assert_eq!(result.element_reports[1].name, "history");
    }

    #[test]
    fn empty_assembly_returns_empty_results() {
        let py_code = r#"
def assemble(ctx):
    pass
"#;
        let file = write_temp_py(py_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap();

        assert!(result.messages.is_empty());
        assert!(result.element_reports.is_empty());
        assert!(result.pending_compactions.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn budget_decremented_through_python() {
        let py_code = r#"
def assemble(ctx):
    prompts = ctx.prompts()
    ctx.add(prompts)
    # Verify budget was decremented within Python.
    assert ctx.remaining_budget < ctx.total_budget, \
        f"budget should have decreased: {ctx.remaining_budget} < {ctx.total_budget}"
"#;
        let file = write_temp_py(py_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap();

        assert_eq!(result.messages.len(), 1);
    }

    // ── Error cases ──────────────────────────────────────────────────────────

    #[test]
    fn syntax_error_returns_syntax_kind() {
        // Missing colon — invalid Python syntax.
        let py_code = "def assemble(ctx)\n";
        let file = write_temp_py(py_code);
        let err = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap_err();

        assert_eq!(err.kind, AssemblyErrorKind::Syntax);
        assert!(
            err.message.contains("SyntaxError"),
            "expected SyntaxError in message, got: {}",
            err.message
        );
        assert!(err.traceback.is_some(), "syntax errors should include a traceback");
    }

    #[test]
    fn missing_file_returns_error() {
        let err = execute_assembly(
            Path::new("/nonexistent/assembly.py"),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap_err();

        assert_eq!(err.kind, AssemblyErrorKind::Runtime);
        assert!(
            err.message.contains("failed to read"),
            "expected file-read error, got: {}",
            err.message
        );
        assert!(err.traceback.is_none());
    }

    #[test]
    fn missing_assemble_function_returns_runtime_error() {
        // Valid Python but no assemble() function.
        let py_code = "x = 42\n";
        let file = write_temp_py(py_code);
        let err = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap_err();

        assert_eq!(err.kind, AssemblyErrorKind::Runtime);
        assert!(
            err.message.contains("assemble"),
            "expected mention of assemble function, got: {}",
            err.message
        );
    }

    #[test]
    fn runtime_error_in_assemble_returns_traceback() {
        let py_code = r#"
def assemble(ctx):
    raise ValueError("something went wrong")
"#;
        let file = write_temp_py(py_code);
        let err = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap_err();

        assert_eq!(err.kind, AssemblyErrorKind::Runtime);
        assert!(
            err.message.contains("ValueError"),
            "expected ValueError in message, got: {}",
            err.message
        );
        assert!(
            err.message.contains("something went wrong"),
            "expected error detail, got: {}",
            err.message
        );
        assert!(err.traceback.is_some(), "runtime exceptions should include a traceback");
    }

    // ── Warnings and compactions ─────────────────────────────────────────────

    #[test]
    fn warnings_collected_when_budget_exceeded() {
        let py_code = r#"
def assemble(ctx):
    prompts = ctx.prompts()
    ctx.add(prompts)
"#;
        // Budget of 5 tokens — prompts element has 10, triggering a warning.
        let file = write_temp_py(py_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            5,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap();

        assert!(
            !result.warnings.is_empty(),
            "expected budget-exceeded warning"
        );
        assert!(result.warnings[0].contains("prompts"));
    }

    #[test]
    fn pending_compactions_collected() {
        let py_code = r#"
def assemble(ctx):
    prompts = ctx.prompts()
    ctx.add(prompts)
    older = ctx.history_older(budget=500, strategy="structured_extract")
    ctx.add(older)
"#;
        let file = write_temp_py(py_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap();

        assert_eq!(result.pending_compactions.len(), 1);
        assert_eq!(result.pending_compactions[0].strategy, "structured_extract");
        assert_eq!(result.pending_compactions[0].budget, 500);
    }

    // ── Timeout ──────────────────────────────────────────────────────────────

    #[test]
    fn timeout_returns_timeout_error() {
        let py_code = r#"
def assemble(ctx):
    while True:
        pass
"#;
        let file = write_temp_py(py_code);
        let err = execute_assembly(
            file.path(),
            Duration::from_millis(500),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(),
        )
        .unwrap_err();

        assert_eq!(err.kind, AssemblyErrorKind::Timeout);
        assert!(
            err.message.contains("timeout"),
            "expected timeout in message, got: {}",
            err.message
        );
    }

    // ── Named assembly include (integration) ─────────────────────────────────

    /// Create a temp dir with assemblies/ subdirectory and write named .py
    /// files into it. Returns the TempDir (must be kept alive for the test).
    fn setup_assembly_hierarchy(
        assemblies: &[(&str, &str)],
    ) -> (tempfile::TempDir, Vec<(swage_config::HierarchyLevel, std::path::PathBuf)>) {
        let dir = tempfile::TempDir::new().unwrap();
        let assemblies_dir = dir.path().join("assemblies");
        std::fs::create_dir_all(&assemblies_dir).unwrap();
        for (name, code) in assemblies {
            let path = assemblies_dir.join(format!("{name}.py"));
            std::fs::write(&path, code).unwrap();
        }
        let levels = vec![(
            swage_config::HierarchyLevel::Project,
            dir.path().to_path_buf(),
        )];
        (dir, levels)
    }

    fn data_with_levels(
        levels: Vec<(swage_config::HierarchyLevel, std::path::PathBuf)>,
    ) -> AssemblyData {
        AssemblyData {
            search_levels: levels,
            ..simple_assembly_data()
        }
    }

    #[test]
    fn include_executes_named_assembly() {
        let (_dir, levels) = setup_assembly_hierarchy(&[(
            "helper",
            "def assemble(ctx):\n    prompts = ctx.prompts()\n    ctx.add(prompts)\n",
        )]);

        // Main assembly includes the helper.
        let main_code = r#"
def assemble(ctx):
    ctx.include("helper")
"#;
        let file = write_temp_py(main_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            data_with_levels(levels),
        )
        .unwrap();

        // The helper added 1 prompt message.
        assert_eq!(result.messages.len(), 1);
        // The element report should be a single named-assembly wrapper.
        assert_eq!(result.element_reports.len(), 1);
        assert_eq!(result.element_reports[0].name, "assembly:helper");
        let named = result.element_reports[0].named_assembly.as_ref().unwrap();
        assert_eq!(named.assembly_name, "helper");
        assert_eq!(named.resolved_level, "project");
        assert_eq!(named.child_elements.len(), 1);
        assert_eq!(named.child_elements[0].name, "prompts");
    }

    #[test]
    fn include_shares_budget_with_caller() {
        let (_dir, levels) = setup_assembly_hierarchy(&[(
            "budget_check",
            r#"
def assemble(ctx):
    prompts = ctx.prompts()
    ctx.add(prompts)
    # Budget should have decreased from the caller's perspective too.
"#,
        )]);

        let main_code = r#"
def assemble(ctx):
    initial = ctx.remaining_budget
    ctx.include("budget_check")
    # After include, budget should be lower.
    assert ctx.remaining_budget < initial, \
        f"budget should decrease after include: {ctx.remaining_budget} < {initial}"
"#;
        let file = write_temp_py(main_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            data_with_levels(levels),
        )
        .unwrap();

        // Verify the include succeeded (prompts were added).
        assert_eq!(result.messages.len(), 1);
    }

    #[test]
    fn include_cycle_is_detected_and_warned() {
        let (_dir, levels) = setup_assembly_hierarchy(&[
            (
                "alpha",
                "def assemble(ctx):\n    ctx.include(\"beta\")\n",
            ),
            (
                "beta",
                "def assemble(ctx):\n    ctx.include(\"alpha\")\n",
            ),
        ]);

        // Start by including alpha, which includes beta, which tries to
        // include alpha again — the cycle should be detected.
        let main_code = r#"
def assemble(ctx):
    ctx.include("alpha")
"#;
        let file = write_temp_py(main_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            data_with_levels(levels),
        )
        .unwrap();

        // Should have a circular reference warning.
        assert!(
            result.warnings.iter().any(|w| w.contains("circular reference")),
            "expected circular reference warning, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn include_missing_assembly_warns() {
        let main_code = r#"
def assemble(ctx):
    ctx.include("nonexistent")
"#;
        let file = write_temp_py(main_code);
        let result = execute_assembly(
            file.path(),
            Duration::from_secs(10),
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            simple_assembly_data(), // empty search_levels
        )
        .unwrap();

        assert!(
            result.warnings.iter().any(|w| w.contains("not found")),
            "expected not-found warning, got: {:?}",
            result.warnings
        );
    }
}
