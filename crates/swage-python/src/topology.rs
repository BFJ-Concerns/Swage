//! Python topology definition execution.

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use swage_topology::{
    TopologyEntityCatalog, TopologyGraph, TopologyValidationProblem, validate_graph,
};
use tracing::debug;

use crate::topology_bindings::{inject_topology_module, py_any_to_json};

#[derive(Debug)]
pub enum TopologyDefinitionError {
    ReadFailed(String),
    Syntax(String),
    Runtime(String),
    Timeout(String),
    Deserialisation(String),
    Validation(Vec<TopologyValidationProblem>),
}

impl std::fmt::Display for TopologyDefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFailed(message)
            | Self::Syntax(message)
            | Self::Runtime(message)
            | Self::Timeout(message)
            | Self::Deserialisation(message) => write!(f, "{message}"),
            Self::Validation(problems) => {
                let summary = problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                write!(f, "topology validation failed: {summary}")
            }
        }
    }
}

impl std::error::Error for TopologyDefinitionError {}

const TOPOLOGY_WRAPPER: &str = concat!(
    "import traceback as _tb\n",
    "_topology_err = None\n",
    "_topology_err_kind = None\n",
    "_topology_result = None\n",
    "_topology_ns = {}\n",
    "try:\n",
    "    ",
    "exec",
    "(compile(_topology_code, _topology_path, '",
    "exec",
    "'), _topology_ns)\n",
    "except SyntaxError:\n",
    "    _topology_err_kind = 'syntax'\n",
    "    _topology_err = _tb.format_exc()\n",
    "except BaseException:\n",
    "    _topology_err_kind = 'runtime'\n",
    "    _topology_err = _tb.format_exc()\n",
    "if _topology_err is None:\n",
    "    if 'build_topology' not in _topology_ns:\n",
    "        _topology_err_kind = 'runtime'\n",
    "        _topology_err = 'topology file does not define a build_topology() function'\n",
    "    else:\n",
    "        try:\n",
    "            _topology_result = _topology_ns['build_topology']()\n",
    "        except KeyboardInterrupt:\n",
    "            _topology_err_kind = 'timeout'\n",
    "            _topology_err = 'topology execution interrupted by timeout'\n",
    "        except BaseException:\n",
    "            _topology_err_kind = 'runtime'\n",
    "            _topology_err = _tb.format_exc()\n",
);

pub fn execute_topology_definition(
    path: &Path,
    timeout: Duration,
    catalog: TopologyEntityCatalog,
) -> Result<TopologyGraph, TopologyDefinitionError> {
    let code = fs::read_to_string(path).map_err(|e| {
        TopologyDefinitionError::ReadFailed(format!(
            "failed to read topology definition {}: {e}",
            path.display()
        ))
    })?;

    execute_topology_source(&code, &path.display().to_string(), timeout, catalog)
}

pub fn execute_topology_source(
    code: &str,
    source_name: &str,
    timeout: Duration,
    catalog: TopologyEntityCatalog,
) -> Result<TopologyGraph, TopologyDefinitionError> {
    let code = code.to_string();
    let path_str = source_name.to_string();
    let (result_tx, result_rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let result = Python::with_gil(|py| run_topology(py, &code, &path_str, catalog));
        result_tx.send(result).ok();
    });

    match result_rx.recv_timeout(timeout) {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            debug!(
                timeout_secs = timeout.as_secs(),
                "topology definition timed out, interrupting"
            );
            unsafe { pyo3::ffi::PyErr_SetInterrupt() };
            let _ = result_rx.recv_timeout(Duration::from_secs(2));
            Err(TopologyDefinitionError::Timeout(format!(
                "topology definition exceeded {}s timeout",
                timeout.as_secs_f64()
            )))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(TopologyDefinitionError::Runtime(
            "topology definition terminated: internal error (thread lost)".into(),
        )),
    }
}

fn run_topology(
    py: Python<'_>,
    code: &str,
    path: &str,
    catalog: TopologyEntityCatalog,
) -> Result<TopologyGraph, TopologyDefinitionError> {
    inject_topology_module(py).map_err(|e| {
        TopologyDefinitionError::Runtime(format!("failed to inject swage_topology module: {e}"))
    })?;

    let locals = PyDict::new(py);
    locals.set_item("_topology_code", code).unwrap();
    locals.set_item("_topology_path", path).unwrap();

    let wrapper = CString::new(TOPOLOGY_WRAPPER)
        .expect("topology wrapper template should not contain NUL bytes");
    let wrapper_cstr = std::ffi::CStr::from_bytes_with_nul(wrapper.as_bytes_with_nul())
        .expect("CString should produce valid CStr");

    if let Err(err) = py.run(wrapper_cstr, None, Some(&locals)) {
        return Err(TopologyDefinitionError::Runtime(format!(
            "internal topology wrapper error: {err}"
        )));
    }

    let error_kind = locals
        .get_item("_topology_err_kind")
        .map_err(|err| TopologyDefinitionError::Runtime(err.to_string()))?
        .and_then(|value| value.extract::<String>().ok());
    let error_message = locals
        .get_item("_topology_err")
        .map_err(|err| TopologyDefinitionError::Runtime(err.to_string()))?
        .and_then(|value| value.extract::<String>().ok());

    if let Some(kind) = error_kind {
        let message = error_message.unwrap_or_else(|| "unknown topology error".into());
        return Err(match kind.as_str() {
            "syntax" => TopologyDefinitionError::Syntax(message),
            "timeout" => TopologyDefinitionError::Timeout(message),
            _ => TopologyDefinitionError::Runtime(message),
        });
    }

    let topology_value = locals
        .get_item("_topology_result")
        .map_err(|err| TopologyDefinitionError::Runtime(err.to_string()))?
        .ok_or_else(|| {
            TopologyDefinitionError::Runtime("build_topology() returned no topology result".into())
        })?;

    let json = py_any_to_json(&topology_value).map_err(|err| {
        TopologyDefinitionError::Deserialisation(format!(
            "failed to convert Python topology to JSON: {err}"
        ))
    })?;
    let graph: TopologyGraph = serde_json::from_value(json).map_err(|err| {
        TopologyDefinitionError::Deserialisation(format!(
            "failed to deserialize topology definition: {err}"
        ))
    })?;

    let mut catalog = catalog;
    for node in &graph.nodes {
        if let Some(task_name) = &node.task_name {
            catalog.known_tasks.insert(task_name.clone());
        }
    }

    validate_graph(&graph, &catalog).map_err(TopologyDefinitionError::Validation)?;
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn catalog() -> TopologyEntityCatalog {
        TopologyEntityCatalog {
            known_agents: ["planner", "reviewer"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            known_tasks: ["draft-plan", "review-plan"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            known_handlers: ["collect-results"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        }
    }

    fn write_topology_file(source: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("temp file");
        file.write_all(source.as_bytes())
            .expect("write topology source");
        file
    }

    #[test]
    fn execute_topology_definition_returns_validated_graph() {
        let file = write_topology_file(
            r#"
import swage_topology as topo

def build_topology():
    return topo.graph(
        "plan",
        [
            topo.llm_node(
                "plan",
                "planner",
                "draft-plan",
                output_schema={
                    "type": "object",
                    "properties": {
                        "decision": {"type": "string"}
                    }
                }
            ),
            topo.llm_node(
                "review",
                "reviewer",
                "review-plan",
                output_schema={"type": "object", "properties": {}}
            ),
        ],
        [
            topo.conditional_edge(
                "plan-to-review",
                "plan",
                "review",
                "decision",
                "continue",
            )
        ],
        max_fan_out=4,
        max_concurrency=2,
        max_total_tokens=1000,
    )
"#,
        );

        let graph = execute_topology_definition(file.path(), Duration::from_secs(5), catalog())
            .expect("topology should load");

        assert_eq!(graph.entry_node_id, "plan");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn execute_topology_definition_surfaces_validation_errors() {
        let file = write_topology_file(
            r#"
import swage_topology as topo

def build_topology():
    return topo.graph(
        "plan",
        [
            topo.llm_node(
                "plan",
                "ghost",
                "draft-plan",
                output_schema={"type": "object", "properties": {}}
            )
        ],
        [],
    )
"#,
        );

        let err = execute_topology_definition(file.path(), Duration::from_secs(5), catalog())
            .expect_err("topology should fail validation");

        match err {
            TopologyDefinitionError::Validation(problems) => {
                assert!(
                    problems
                        .iter()
                        .any(|problem| problem.message.contains("agent_id 'ghost'"))
                );
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }
}
