//! Python `swage_topology` module — helper functions for topology definitions.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// Convert a Python value into JSON so topology definitions can return normal
/// Python dicts and lists that deserialize into Rust topology types.
pub fn py_any_to_json(value: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if value.is_none() {
        return Ok(serde_json::Value::Null);
    }
    if let Ok(boolean) = value.extract::<bool>() {
        return Ok(serde_json::Value::Bool(boolean));
    }
    if let Ok(integer) = value.extract::<i64>() {
        return Ok(serde_json::Value::Number(integer.into()));
    }
    if let Ok(float) = value.extract::<f64>() {
        let number = serde_json::Number::from_f64(float).ok_or_else(|| {
            pyo3::exceptions::PyTypeError::new_err("floating-point value is not valid JSON")
        })?;
        return Ok(serde_json::Value::Number(number));
    }
    if let Ok(text) = value.extract::<String>() {
        return Ok(serde_json::Value::String(text));
    }
    if let Ok(list) = value.downcast::<PyList>() {
        let mut items = Vec::with_capacity(list.len());
        for item in list.iter() {
            items.push(py_any_to_json(&item)?);
        }
        return Ok(serde_json::Value::Array(items));
    }
    if let Ok(dict) = value.downcast::<PyDict>() {
        let mut object = serde_json::Map::with_capacity(dict.len());
        for (key, item) in dict.iter() {
            let key = key.extract::<String>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("topology dictionaries must use string keys")
            })?;
            object.insert(key, py_any_to_json(&item)?);
        }
        return Ok(serde_json::Value::Object(object));
    }

    Err(pyo3::exceptions::PyTypeError::new_err(
        "unsupported Python value in topology definition",
    ))
}

#[pymodule]
pub fn swage_topology(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(graph, module)?)?;
    module.add_function(wrap_pyfunction!(llm_node, module)?)?;
    module.add_function(wrap_pyfunction!(deterministic_node, module)?)?;
    module.add_function(wrap_pyfunction!(continuation_node, module)?)?;
    module.add_function(wrap_pyfunction!(unconditional_edge, module)?)?;
    module.add_function(wrap_pyfunction!(conditional_edge, module)?)?;
    module.add_function(wrap_pyfunction!(fan_out_edge, module)?)?;
    module.add_function(wrap_pyfunction!(fork_edge, module)?)?;
    Ok(())
}

#[pyfunction]
#[pyo3(signature = (entry_node_id, nodes, edges, max_fan_out=None, max_concurrency=None, max_total_tokens=None))]
fn graph(
    py: Python<'_>,
    entry_node_id: &str,
    nodes: &Bound<'_, PyAny>,
    edges: &Bound<'_, PyAny>,
    max_fan_out: Option<u32>,
    max_concurrency: Option<u32>,
    max_total_tokens: Option<u64>,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("entry_node_id", entry_node_id)?;

    let budget = PyDict::new(py);
    budget.set_item("max_fan_out", max_fan_out)?;
    budget.set_item("max_concurrency", max_concurrency)?;
    budget.set_item("max_total_tokens", max_total_tokens)?;

    dict.set_item("budget", budget)?;
    dict.set_item("nodes", nodes)?;
    dict.set_item("edges", edges)?;
    Ok(dict.into_any().unbind())
}

#[pyfunction]
#[pyo3(signature = (node_id, agent_id, task_name, output_schema=None, iteration_limit=None))]
fn llm_node(
    py: Python<'_>,
    node_id: &str,
    agent_id: &str,
    task_name: &str,
    output_schema: Option<&Bound<'_, PyAny>>,
    iteration_limit: Option<u32>,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("id", node_id)?;
    dict.set_item("kind", "llm")?;
    dict.set_item("agent_id", agent_id)?;
    dict.set_item("task_name", task_name)?;
    dict.set_item("handler_name", py.None())?;
    if let Some(output_schema) = output_schema {
        dict.set_item("output_schema", output_schema)?;
    } else {
        dict.set_item("output_schema", py.None())?;
    }
    dict.set_item("iteration_limit", iteration_limit)?;
    Ok(dict.into_any().unbind())
}

#[pyfunction]
#[pyo3(signature = (node_id, handler_name, output_schema=None, iteration_limit=None))]
fn deterministic_node(
    py: Python<'_>,
    node_id: &str,
    handler_name: &str,
    output_schema: Option<&Bound<'_, PyAny>>,
    iteration_limit: Option<u32>,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("id", node_id)?;
    dict.set_item("kind", "deterministic")?;
    dict.set_item("agent_id", py.None())?;
    dict.set_item("task_name", py.None())?;
    dict.set_item("handler_name", handler_name)?;
    if let Some(output_schema) = output_schema {
        dict.set_item("output_schema", output_schema)?;
    } else {
        dict.set_item("output_schema", py.None())?;
    }
    dict.set_item("iteration_limit", iteration_limit)?;
    Ok(dict.into_any().unbind())
}

#[pyfunction]
#[pyo3(signature = (node_id, task_name, output_schema=None, iteration_limit=None, agent_id=None))]
fn continuation_node(
    py: Python<'_>,
    node_id: &str,
    task_name: &str,
    output_schema: Option<&Bound<'_, PyAny>>,
    iteration_limit: Option<u32>,
    agent_id: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("id", node_id)?;
    dict.set_item("kind", "continuation")?;
    dict.set_item("agent_id", agent_id)?;
    dict.set_item("task_name", task_name)?;
    dict.set_item("handler_name", py.None())?;
    if let Some(output_schema) = output_schema {
        dict.set_item("output_schema", output_schema)?;
    } else {
        dict.set_item("output_schema", py.None())?;
    }
    dict.set_item("iteration_limit", iteration_limit)?;
    Ok(dict.into_any().unbind())
}

#[pyfunction]
fn unconditional_edge(
    py: Python<'_>,
    edge_id: &str,
    source_node_id: &str,
    target_node_id: &str,
) -> PyResult<Py<PyAny>> {
    edge_with_routing(py, edge_id, source_node_id, target_node_id, |routing| {
        routing.set_item("type", "unconditional")
    })
}

#[pyfunction]
fn conditional_edge(
    py: Python<'_>,
    edge_id: &str,
    source_node_id: &str,
    target_node_id: &str,
    field_path: &str,
    equals: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    edge_with_routing(py, edge_id, source_node_id, target_node_id, |routing| {
        routing.set_item("type", "condition")?;
        routing.set_item("field_path", field_path)?;
        routing.set_item("equals", equals)
    })
}

#[pyfunction]
fn fan_out_edge(
    py: Python<'_>,
    edge_id: &str,
    source_node_id: &str,
    target_node_id: &str,
    items_field: &str,
) -> PyResult<Py<PyAny>> {
    edge_with_routing(py, edge_id, source_node_id, target_node_id, |routing| {
        routing.set_item("type", "fan_out")?;
        routing.set_item("items_field", items_field)
    })
}

#[pyfunction]
fn fork_edge(
    py: Python<'_>,
    edge_id: &str,
    source_node_id: &str,
    target_node_id: &str,
) -> PyResult<Py<PyAny>> {
    edge_with_routing(py, edge_id, source_node_id, target_node_id, |routing| {
        routing.set_item("type", "fork")
    })
}

fn edge_with_routing(
    py: Python<'_>,
    edge_id: &str,
    source_node_id: &str,
    target_node_id: &str,
    fill_routing: impl FnOnce(&Bound<'_, PyDict>) -> PyResult<()>,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    let routing = PyDict::new(py);
    fill_routing(&routing)?;
    dict.set_item("id", edge_id)?;
    dict.set_item("source_node_id", source_node_id)?;
    dict.set_item("target_node_id", target_node_id)?;
    dict.set_item("routing", routing)?;
    Ok(dict.into_any().unbind())
}

pub fn inject_topology_module(py: Python<'_>) -> PyResult<()> {
    let module = PyModule::new(py, "swage_topology")?;
    swage_topology(&module)?;

    let sys = py.import("sys")?;
    let modules = sys.getattr("modules")?;
    modules.set_item("swage_topology", &module)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_any_to_json_converts_nested_python_values() {
        Python::with_gil(|py| {
            let dict = PyDict::new(py);
            dict.set_item("name", "plan").unwrap();
            dict.set_item("numbers", vec![1, 2, 3]).unwrap();
            dict.set_item("flag", true).unwrap();

            let json = py_any_to_json(&dict.into_any()).expect("conversion should succeed");
            assert_eq!(json["name"], "plan");
            assert_eq!(json["numbers"], serde_json::json!([1, 2, 3]));
            assert_eq!(json["flag"], serde_json::json!(true));
        });
    }
}
