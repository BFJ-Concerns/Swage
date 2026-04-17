//! Python topology definition execution via child workers.

use std::fs;
use std::path::Path;
use std::time::Duration;

use swage_config::model::PythonExecuteToolConfig;
use swage_topology::{
    TopologyEntityCatalog, TopologyGraph, TopologyValidationProblem, validate_graph,
};

use crate::blocking::run_worker_future;
use crate::worker_host::PythonWorkerRuntime;
use crate::worker_protocol::TopologyExecutionPayload;

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

pub fn execute_topology_definition(
    path: &Path,
    timeout: Duration,
    catalog: TopologyEntityCatalog,
) -> Result<TopologyGraph, TopologyDefinitionError> {
    execute_topology_definition_with_config(
        &PythonExecuteToolConfig::default(),
        path,
        timeout,
        catalog,
    )
}

pub fn execute_topology_definition_with_config(
    python_config: &PythonExecuteToolConfig,
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

    execute_topology_source_with_config(
        python_config,
        &code,
        &path.display().to_string(),
        timeout,
        catalog,
    )
}

pub fn execute_topology_source(
    code: &str,
    source_name: &str,
    timeout: Duration,
    catalog: TopologyEntityCatalog,
) -> Result<TopologyGraph, TopologyDefinitionError> {
    execute_topology_source_with_config(
        &PythonExecuteToolConfig::default(),
        code,
        source_name,
        timeout,
        catalog,
    )
}

pub fn execute_topology_source_with_config(
    python_config: &PythonExecuteToolConfig,
    code: &str,
    source_name: &str,
    timeout: Duration,
    catalog: TopologyEntityCatalog,
) -> Result<TopologyGraph, TopologyDefinitionError> {
    let runtime = PythonWorkerRuntime::new(python_config.clone())
        .map_err(|error| TopologyDefinitionError::Runtime(error.to_string()))?;
    let payload = TopologyExecutionPayload {
        code: code.to_string(),
        path: source_name.to_string(),
        catalog: catalog.clone(),
    };
    let execution = run_worker_future(
        move || async move { runtime.execute_topology(payload, timeout).await },
        "topology execution",
    )
    .map_err(|error| classify_error(error.to_string()))?;

    if let Some(traceback) = execution.traceback {
        return Err(match execution.exception_type.as_deref() {
            Some("SyntaxError") => TopologyDefinitionError::Syntax(traceback),
            Some("TimeoutError") => TopologyDefinitionError::Timeout(traceback),
            _ => TopologyDefinitionError::Runtime(traceback),
        });
    }

    let graph: TopologyGraph = serde_json::from_value(execution.result.ok_or_else(|| {
        TopologyDefinitionError::Runtime("build_topology() returned no topology result".to_string())
    })?)
    .map_err(|error| {
        TopologyDefinitionError::Deserialisation(format!(
            "failed to deserialize topology definition: {error}"
        ))
    })?;

    let effective_catalog = TopologyEntityCatalog {
        known_agents: catalog.known_agents,
        // Task names are authored inline in topology definitions today, so the
        // validation boundary is "present and non-empty", not membership in a
        // separate registry that does not exist yet. Seed the validation set
        // from the decoded graph so execution still rejects malformed nodes
        // without spuriously rejecting valid authored task names.
        known_tasks: catalog
            .known_tasks
            .into_iter()
            .chain(
                graph
                    .nodes
                    .iter()
                    .filter_map(|node| node.task_name.as_ref().cloned()),
            )
            .collect(),
        known_handlers: catalog.known_handlers,
    };
    validate_graph(&graph, &effective_catalog).map_err(TopologyDefinitionError::Validation)?;
    Ok(graph)
}

fn classify_error(message: String) -> TopologyDefinitionError {
    if message.contains("timed out") || message.contains("timeout") {
        TopologyDefinitionError::Timeout(message)
    } else {
        TopologyDefinitionError::Runtime(message)
    }
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
            topo.llm_node("plan", "planner", "draft-plan"),
            topo.deterministic_node("collect", "collect-results"),
        ],
        [
            topo.unconditional_edge("edge_1", "plan", "collect"),
        ],
    )
"#,
        );

        let graph = execute_topology_definition(file.path(), Duration::from_secs(5), catalog())
            .expect("topology definition should succeed");

        assert_eq!(graph.entry_node_id, "plan");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn execute_topology_definition_allows_standard_library_imports() {
        let file = write_topology_file(
            r#"
import json
import swage_topology as topo

def build_topology():
    agent_id = json.loads('"planner"')
    task_name = json.loads('"draft-plan"')
    return topo.graph(
        "plan",
        [
            topo.llm_node("plan", agent_id, task_name),
        ],
        [],
    )
"#,
        );

        let graph = execute_topology_definition(file.path(), Duration::from_secs(5), catalog())
            .expect("topology definition should allow stdlib imports");

        assert_eq!(graph.entry_node_id, "plan");
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn execute_topology_definition_rejects_unknown_agent_ids() {
        let file = write_topology_file(
            r#"
import swage_topology as topo

def build_topology():
    return topo.graph(
        "plan",
        [
            topo.llm_node("plan", "unknown-agent", "draft-plan"),
        ],
        [],
    )
"#,
        );

        let error = execute_topology_definition(file.path(), Duration::from_secs(5), catalog())
            .expect_err("unknown agent should fail validation");

        match error {
            TopologyDefinitionError::Validation(problems) => {
                let rendered = problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                assert!(
                    rendered.contains("agent_id"),
                    "expected agent validation problem, got: {rendered}"
                );
                assert!(
                    rendered.contains("unknown-agent"),
                    "expected the unknown agent name in the problem, got: {rendered}"
                );
            }
            other => panic!("expected validation error, got: {other}"),
        }
    }

    #[test]
    fn execute_topology_definition_rejects_unknown_task_names() {
        let file = write_topology_file(
            r#"
import swage_topology as topo

def build_topology():
    return topo.graph(
        "plan",
        [
            topo.llm_node("plan", "planner", "unknown-task"),
        ],
        [],
    )
"#,
        );

        let error = execute_topology_definition(file.path(), Duration::from_secs(5), catalog())
            .expect_err("unknown task should fail validation");

        match error {
            TopologyDefinitionError::Validation(problems) => {
                let rendered = problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                assert!(
                    rendered.contains("task_name"),
                    "expected task validation problem, got: {rendered}"
                );
                assert!(
                    rendered.contains("unknown-task"),
                    "expected the unknown task name in the problem, got: {rendered}"
                );
            }
            other => panic!("expected validation error, got: {other}"),
        }
    }
}
