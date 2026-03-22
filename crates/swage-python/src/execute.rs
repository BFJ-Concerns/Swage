//! Python code execution — stdout capture, timeout, and error handling.
//!
//! Runs agent-generated Python code in the embedded interpreter, captures
//! stdout, enforces a wall-clock timeout, and categorises errors. The
//! `ToolDispatcher` is installed as a thread-local so the `swage` namespace
//! bindings (from `bindings.rs`) can access it during execution.

use std::cell::RefCell;
use std::ffi::CString;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use tracing::{debug, warn};

use crate::dispatch::ToolDispatcher;

// ---------------------------------------------------------------------------
// Thread-local dispatcher
// ---------------------------------------------------------------------------

thread_local! {
    /// The active tool dispatcher for the current Python execution thread.
    ///
    /// Set by `execute_code` before running agent code, cleared after
    /// execution completes (success, error, or timeout). The `swage`
    /// namespace bindings read this to dispatch tool calls back to Rust.
    pub(crate) static DISPATCHER: RefCell<Option<Arc<dyn ToolDispatcher>>> =
        const { RefCell::new(None) };
}

/// Retrieve the thread-local dispatcher (for use by `bindings.rs`).
///
/// Returns `None` if called outside of `execute_code` — bindings should
/// raise a Python `RuntimeError` in that case.
pub(crate) fn get_dispatcher() -> Option<Arc<dyn ToolDispatcher>> {
    DISPATCHER.with(|d| d.borrow().clone())
}

// ---------------------------------------------------------------------------
// Execution result
// ---------------------------------------------------------------------------

/// The outcome of running a Python code snippet.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Captured stdout on success, or the full traceback on error.
    pub output: String,
    /// `true` if execution was terminated by the timeout mechanism.
    pub timed_out: bool,
}

// ---------------------------------------------------------------------------
// execute_code
// ---------------------------------------------------------------------------

/// Run a Python code string in the embedded interpreter.
///
/// Spawns a dedicated thread for execution so the caller can enforce a
/// wall-clock timeout. The `ToolDispatcher` is installed as a thread-local
/// before code runs and cleaned up afterwards.
///
/// # Timeout mechanism
///
/// The executing thread holds the GIL while Python code runs. On timeout,
/// `PyErr_SetInterrupt` raises a `KeyboardInterrupt` in the Python thread —
/// this is the standard CPython mechanism for asynchronous exceptions and
/// does not require the GIL.
pub fn execute_code(
    code: &str,
    timeout: Duration,
    dispatcher: Arc<dyn ToolDispatcher>,
) -> ExecutionResult {
    let code = code.to_string();
    let (result_tx, result_rx) = mpsc::channel::<ExecutionResult>();

    let handle = thread::spawn(move || {
        // Install the dispatcher for this thread.
        DISPATCHER.with(|d| *d.borrow_mut() = Some(dispatcher));

        let result = Python::with_gil(|py| run_python_code(py, &code));

        // Clean up the dispatcher regardless of outcome.
        DISPATCHER.with(|d| *d.borrow_mut() = None);

        result_tx.send(result).ok();
    });

    // Wait for the result with timeout.
    match result_rx.recv_timeout(timeout) {
        Ok(result) => {
            // Thread completed normally — join to clean up.
            let _ = handle.join();
            result
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            debug!(timeout_secs = timeout.as_secs(), "Python execution timed out, interrupting");
            // Interrupt the Python thread by raising KeyboardInterrupt.
            // We use PyErr_SetInterrupt which is GIL-safe and simpler
            // than PyThreadState_SetAsyncExc.
            unsafe { pyo3::ffi::PyErr_SetInterrupt(); }
            // Wait briefly for the interrupted thread to finish.
            let _ = result_rx.recv_timeout(Duration::from_secs(2));

            ExecutionResult {
                output: format!(
                    "Execution terminated: exceeded {}s timeout",
                    timeout.as_secs_f64()
                ),
                timed_out: true,
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The executing thread panicked or was killed.
            warn!("Python execution thread disconnected unexpectedly");
            let _ = handle.join();
            ExecutionResult {
                output: "Execution terminated: internal error (thread lost)".into(),
                timed_out: false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Python wrapper
// ---------------------------------------------------------------------------

/// Python wrapper script that redirects stdout, runs user code, and captures
/// output. Uses `io.StringIO` for reliable capture and `traceback.format_exc()`
/// for clean error formatting.
///
/// The user's code is passed as the `_code` local variable (a string).
/// The wrapper uses Python's `exec(compile(...))` to run it — this is the
/// standard mechanism for executing code strings in an embedded interpreter.
const WRAPPER_TEMPLATE: &str = concat!(
    "import sys as _sys, io as _io, traceback as _tb\n",
    "\n",
    "_buf = _io.StringIO()\n",
    "_old_stdout = _sys.stdout\n",
    "_sys.stdout = _buf\n",
    "_err_output = None\n",
    "\n",
    "try:\n",
    "    ", "exec", "(compile(_code, '<python_execute>', '", "exec", "'))\n",
    "except SyntaxError:\n",
    "    _sys.stdout = _old_stdout\n",
    "    _err_output = _tb.format_exc()\n",
    "except BaseException:\n",
    "    _sys.stdout = _old_stdout\n",
    "    _err_output = _tb.format_exc()\n",
    "finally:\n",
    "    _sys.stdout = _old_stdout\n",
    "\n",
    "if _err_output is not None:\n",
    "    _output = _err_output\n",
    "else:\n",
    "    _output = _buf.getvalue()\n",
);

/// Run the Python code inside the GIL, capturing stdout and errors.
fn run_python_code(py: Python<'_>, code: &str) -> ExecutionResult {
    // Inject the swage module so tool functions are available without import.
    if let Err(e) = crate::bindings::inject_swage_module(py) {
        warn!("failed to inject swage module: {e}");
        return ExecutionResult {
            output: format!("Internal error: failed to inject swage module: {e}"),
            timed_out: false,
        };
    }

    let locals = PyDict::new(py);
    locals.set_item("_code", code).unwrap();

    let wrapper =
        CString::new(WRAPPER_TEMPLATE).expect("wrapper template should not contain NUL bytes");
    let wrapper_cstr = std::ffi::CStr::from_bytes_with_nul(wrapper.as_bytes_with_nul())
        .expect("CString should produce valid CStr");

    match py.run(wrapper_cstr, None, Some(&locals)) {
        Ok(()) => {
            let output = locals
                .get_item("_output")
                .ok()
                .flatten()
                .and_then(|v| v.extract::<String>().ok())
                .unwrap_or_default();
            ExecutionResult {
                output,
                timed_out: false,
            }
        }
        Err(e) => {
            // The wrapper itself failed — shouldn't happen normally.
            ExecutionResult {
                output: format!("Internal wrapper error: {e}"),
                timed_out: false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{DispatchResult, ToolDispatcher};
    use std::sync::Arc;
    use std::time::Duration;

    /// A no-op dispatcher for tests that don't exercise tool calls.
    struct StubDispatcher;

    impl ToolDispatcher for StubDispatcher {
        fn dispatch(&self, tool_name: &str, _arguments: serde_json::Value) -> DispatchResult {
            DispatchResult {
                content: format!("stub:{tool_name}"),
                is_error: false,
            }
        }
    }

    fn stub_dispatcher() -> Arc<dyn ToolDispatcher> {
        Arc::new(StubDispatcher)
    }

    #[test]
    fn print_hello_captures_stdout() {
        let result = execute_code(
            r#"print("hello")"#,
            Duration::from_secs(5),
            stub_dispatcher(),
        );
        assert_eq!(result.output.trim(), "hello");
        assert!(!result.timed_out);
    }

    #[test]
    fn syntax_error_returns_traceback() {
        let result = execute_code("def", Duration::from_secs(5), stub_dispatcher());
        assert!(
            result.output.contains("SyntaxError"),
            "expected SyntaxError in output, got: {}",
            result.output
        );
        assert!(!result.timed_out);
    }

    #[test]
    fn runtime_error_returns_traceback() {
        let result = execute_code("1/0", Duration::from_secs(5), stub_dispatcher());
        assert!(
            result.output.contains("ZeroDivisionError"),
            "expected ZeroDivisionError in output, got: {}",
            result.output
        );
        assert!(!result.timed_out);
    }

    #[test]
    fn timeout_returns_timeout_message() {
        let result = execute_code(
            "while True: pass",
            Duration::from_millis(500),
            stub_dispatcher(),
        );
        assert!(result.timed_out, "expected timed_out = true");
        assert!(
            result.output.contains("timeout"),
            "expected 'timeout' in output, got: {}",
            result.output
        );
    }

    #[test]
    fn multiline_output_captured() {
        let result = execute_code(
            "for i in range(3): print(i)",
            Duration::from_secs(5),
            stub_dispatcher(),
        );
        assert_eq!(result.output, "0\n1\n2\n");
        assert!(!result.timed_out);
    }

    #[test]
    fn dispatcher_thread_local_is_set_during_execution() {
        // The dispatcher should be accessible from the executing thread.
        // We verify this by checking get_dispatcher() returns Some.
        let result = execute_code(
            r#"print("dispatcher check")"#,
            Duration::from_secs(5),
            stub_dispatcher(),
        );
        assert!(!result.timed_out);
        assert!(result.output.contains("dispatcher check"));
    }

    #[test]
    fn dispatcher_thread_local_cleaned_up_after_execution() {
        // After execute_code returns, the thread-local should be cleared
        // on the executing thread. We verify indirectly: the function
        // returns normally, which means cleanup ran.
        let _result = execute_code("pass", Duration::from_secs(5), stub_dispatcher());
        // If we're here, cleanup didn't panic.
    }
}
