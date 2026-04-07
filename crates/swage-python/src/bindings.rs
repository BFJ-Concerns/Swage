//! Python `swage` namespace module — tool functions callable from agent code.
//!
//! Creates a PyO3 module named `swage` that is injected into the Python
//! execution environment (available without import). Each function dispatches
//! to the thread-local `ToolDispatcher` installed by `execute_code`.

use pyo3::prelude::*;

use crate::execute::get_dispatcher;

// ---------------------------------------------------------------------------
// Helper: dispatch a tool call and format the result
// ---------------------------------------------------------------------------

/// Retrieve the dispatcher and call a tool, returning the result as a string.
///
/// On success (`is_error = false`): returns the content directly.
/// On failure (`is_error = true`): returns `"ERROR: {content}"`.
/// If no dispatcher is set (called outside `execute_code`): raises RuntimeError.
fn dispatch_tool(tool_name: &str, arguments: serde_json::Value) -> PyResult<String> {
    let dispatcher = get_dispatcher().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err(
            "swage functions are only available during python_execute",
        )
    })?;

    let result = dispatcher.dispatch(tool_name, arguments);
    if result.is_error {
        Ok(format!("ERROR: {}", result.content))
    } else {
        Ok(result.content)
    }
}

// ---------------------------------------------------------------------------
// PyO3 module: swage
// ---------------------------------------------------------------------------

/// The `swage` Python module — tool functions for agent code.
///
/// Injected into the execution namespace so agent code can call
/// `swage.shell("ls")` without explicit imports.
#[pymodule]
pub fn swage(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(shell, module)?)?;
    module.add_function(wrap_pyfunction!(read_file, module)?)?;
    module.add_function(wrap_pyfunction!(write_file, module)?)?;
    module.add_function(wrap_pyfunction!(list_files, module)?)?;
    module.add_function(wrap_pyfunction!(search_files, module)?)?;
    module.add_function(wrap_pyfunction!(web_search, module)?)?;
    module.add_function(wrap_pyfunction!(fetch_url, module)?)?;
    module.add_function(wrap_pyfunction!(memory_read, module)?)?;
    module.add_function(wrap_pyfunction!(memory_write, module)?)?;
    module.add_function(wrap_pyfunction!(memory_list, module)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tool functions
// ---------------------------------------------------------------------------

/// Run a shell command and return its output.
#[pyfunction]
fn shell(command: &str) -> PyResult<String> {
    dispatch_tool("shell", serde_json::json!({ "command": command }))
}

/// Read the contents of a file at the given path.
#[pyfunction]
fn read_file(path: &str) -> PyResult<String> {
    dispatch_tool("file_read", serde_json::json!({ "path": path }))
}

/// Write content to a file at the given path.
#[pyfunction]
fn write_file(path: &str, content: &str) -> PyResult<String> {
    dispatch_tool(
        "file_write",
        serde_json::json!({ "path": path, "content": content }),
    )
}

/// List files in a directory.
#[pyfunction]
fn list_files(path: &str) -> PyResult<String> {
    dispatch_tool("file_list", serde_json::json!({ "path": path }))
}

/// Search for files matching a pattern.
#[pyfunction]
#[pyo3(signature = (pattern, path = ""))]
fn search_files(pattern: &str, path: &str) -> PyResult<String> {
    dispatch_tool(
        "file_search",
        serde_json::json!({ "pattern": pattern, "path": path }),
    )
}

/// Search the web for a query.
#[pyfunction]
fn web_search(query: &str) -> PyResult<String> {
    dispatch_tool("web_search", serde_json::json!({ "query": query }))
}

/// Fetch the contents of a URL.
#[pyfunction]
fn fetch_url(url: &str) -> PyResult<String> {
    dispatch_tool("url_fetch", serde_json::json!({ "url": url }))
}

/// Read one or all memory files for the current project.
///
/// If `file` is given, returns the contents of that specific markdown file.
/// If omitted, returns all memory files concatenated with separators.
#[pyfunction]
#[pyo3(signature = (file = None))]
fn memory_read(file: Option<&str>) -> PyResult<String> {
    let mut args = serde_json::json!({ "operation": "read" });
    if let Some(f) = file {
        args["file"] = f.into();
    }
    dispatch_tool("memory", args)
}

/// Write content to a named memory file for the current project.
#[pyfunction]
fn memory_write(file: &str, content: &str) -> PyResult<String> {
    dispatch_tool(
        "memory",
        serde_json::json!({ "operation": "write", "file": file, "content": content }),
    )
}

/// List all memory file names for the current project.
#[pyfunction]
fn memory_list() -> PyResult<String> {
    dispatch_tool("memory", serde_json::json!({ "operation": "list" }))
}

// ---------------------------------------------------------------------------
// Module injection helper
// ---------------------------------------------------------------------------

/// Inject the `swage` module into a Python interpreter so it's available
/// without `import`. Call this before running agent code.
pub fn inject_swage_module(py: Python<'_>) -> PyResult<()> {
    // Create the module and add it to sys.modules so `import swage` works,
    // then also inject it into builtins so it's available without import.
    let module = PyModule::new(py, "swage")?;
    swage(&module)?;

    // Register in sys.modules.
    let sys = py.import("sys")?;
    let modules = sys.getattr("modules")?;
    modules.set_item("swage", &module)?;

    // Inject into builtins so `swage.shell(...)` works without import.
    let builtins = py.import("builtins")?;
    builtins.setattr("swage", &module)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{DispatchResult, ToolDispatcher};
    use crate::execute::{DISPATCHER, execute_code};
    use std::sync::Arc;
    use std::time::Duration;

    /// Dispatcher that records calls and returns predictable output.
    struct RecordingDispatcher;

    impl ToolDispatcher for RecordingDispatcher {
        fn dispatch(&self, tool_name: &str, arguments: serde_json::Value) -> DispatchResult {
            DispatchResult {
                content: format!("ok:{tool_name}:{arguments}"),
                is_error: false,
            }
        }
    }

    /// Dispatcher that always returns an error.
    struct ErrorDispatcher;

    impl ToolDispatcher for ErrorDispatcher {
        fn dispatch(&self, tool_name: &str, _arguments: serde_json::Value) -> DispatchResult {
            DispatchResult {
                content: format!("{tool_name} failed"),
                is_error: true,
            }
        }
    }

    #[test]
    fn shell_dispatches_correctly() {
        let result = execute_code(
            r#"result = swage.shell("ls -la")
print(result)"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(
            result.output.contains("ok:shell:"),
            "expected shell dispatch, got: {}",
            result.output
        );
        assert!(result.output.contains("ls -la"));
    }

    #[test]
    fn read_file_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.read_file("/tmp/test.txt"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(result.output.contains("ok:file_read:"));
        assert!(result.output.contains("/tmp/test.txt"));
    }

    #[test]
    fn write_file_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.write_file("/tmp/out.txt", "hello"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(result.output.contains("ok:file_write:"));
        assert!(result.output.contains("hello"));
    }

    #[test]
    fn list_files_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.list_files("/tmp"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(result.output.contains("ok:file_list:"));
    }

    #[test]
    fn search_files_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.search_files("*.rs", "/src"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(result.output.contains("ok:file_search:"));
        assert!(result.output.contains("*.rs"));
    }

    #[test]
    fn search_files_default_path() {
        let result = execute_code(
            r#"print(swage.search_files("*.py"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(result.output.contains("ok:file_search:"));
    }

    #[test]
    fn web_search_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.web_search("rust pyo3"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(result.output.contains("ok:web_search:"));
    }

    #[test]
    fn fetch_url_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.fetch_url("https://example.com"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(result.output.contains("ok:url_fetch:"));
    }

    #[test]
    fn memory_read_all_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.memory_read())"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        // Should dispatch to "memory" with operation "read" and no file key.
        assert!(
            result.output.contains("ok:memory:"),
            "got: {}",
            result.output
        );
        assert!(
            result.output.contains("\"operation\":\"read\""),
            "got: {}",
            result.output
        );
    }

    #[test]
    fn memory_read_file_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.memory_read("notes.md"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(
            result.output.contains("ok:memory:"),
            "got: {}",
            result.output
        );
        assert!(result.output.contains("notes.md"), "got: {}", result.output);
    }

    #[test]
    fn memory_write_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.memory_write("notes.md", "hello world"))"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(
            result.output.contains("ok:memory:"),
            "got: {}",
            result.output
        );
        assert!(
            result.output.contains("\"operation\":\"write\""),
            "got: {}",
            result.output
        );
        assert!(result.output.contains("notes.md"), "got: {}", result.output);
        assert!(
            result.output.contains("hello world"),
            "got: {}",
            result.output
        );
    }

    #[test]
    fn memory_list_dispatches_correctly() {
        let result = execute_code(
            r#"print(swage.memory_list())"#,
            Duration::from_secs(5),
            Arc::new(RecordingDispatcher),
        );
        assert!(
            result.output.contains("ok:memory:"),
            "got: {}",
            result.output
        );
        assert!(
            result.output.contains("\"operation\":\"list\""),
            "got: {}",
            result.output
        );
    }

    #[test]
    fn error_returns_error_prefix() {
        let result = execute_code(
            r#"result = swage.shell("bad")
print(result)"#,
            Duration::from_secs(5),
            Arc::new(ErrorDispatcher),
        );
        assert!(
            result.output.contains("ERROR: shell failed"),
            "expected ERROR: prefix, got: {}",
            result.output
        );
    }

    #[test]
    fn calling_outside_execute_raises_runtime_error() {
        // Ensure the thread-local dispatcher is NOT set.
        DISPATCHER.with(|d| *d.borrow_mut() = None);

        // Calling dispatch_tool directly should fail.
        let err = dispatch_tool("shell", serde_json::json!({"command": "ls"}));
        assert!(err.is_err());
        Python::with_gil(|py| {
            let e = err.unwrap_err();
            assert!(e.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
        });
    }
}
