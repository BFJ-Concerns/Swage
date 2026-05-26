use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyEntityCatalog {
    pub known_agents: HashSet<String>,
    pub known_tasks: HashSet<String>,
    pub known_handlers: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyGraph {
    pub entry_node_id: String,
    pub budget: TopologyBudget,
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TopologyBudget {
    pub max_fan_out: Option<u32>,
    pub max_concurrency: Option<u32>,
    pub max_total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyNode {
    pub id: String,
    #[serde(flatten)]
    pub kind: TopologyNodeKind,
    pub output_schema: Option<serde_json::Value>,
    pub iteration_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TopologyNodeKind {
    Llm {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_name: Option<String>,
    },
    Deterministic {
        handler_name: String,
    },
    Continuation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_name: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyNodeKindTag {
    Llm,
    Deterministic,
    Continuation,
}

impl TopologyNode {
    pub fn kind_tag(&self) -> TopologyNodeKindTag {
        match &self.kind {
            TopologyNodeKind::Llm { .. } => TopologyNodeKindTag::Llm,
            TopologyNodeKind::Deterministic { .. } => TopologyNodeKindTag::Deterministic,
            TopologyNodeKind::Continuation { .. } => TopologyNodeKindTag::Continuation,
        }
    }

    pub fn agent_id(&self) -> Option<&str> {
        match &self.kind {
            TopologyNodeKind::Llm { agent_id, .. } => Some(agent_id),
            TopologyNodeKind::Continuation { agent_id, .. } => agent_id.as_deref(),
            TopologyNodeKind::Deterministic { .. } => None,
        }
    }

    pub fn task_name(&self) -> Option<&str> {
        match &self.kind {
            TopologyNodeKind::Llm { task_name, .. }
            | TopologyNodeKind::Continuation { task_name, .. } => task_name.as_deref(),
            TopologyNodeKind::Deterministic { .. } => None,
        }
    }

    pub fn handler_name(&self) -> Option<&str> {
        match &self.kind {
            TopologyNodeKind::Deterministic { handler_name } => Some(handler_name),
            TopologyNodeKind::Llm { .. } | TopologyNodeKind::Continuation { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyEdge {
    pub id: String,
    pub source_node_id: String,
    pub target_node_id: String,
    pub routing: EdgeRouting,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EdgeRouting {
    Unconditional,
    Condition {
        field_path: String,
        equals: serde_json::Value,
    },
    FanOut {
        items_field: String,
    },
    Fork,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{path}: {message}")]
pub struct TopologyValidationProblem {
    pub path: String,
    pub message: String,
}

pub fn validate_graph(
    graph: &TopologyGraph,
    catalog: &TopologyEntityCatalog,
) -> Result<(), Vec<TopologyValidationProblem>> {
    let mut problems = Vec::new();
    let mut node_map = HashMap::new();
    let mut edge_ids = HashSet::new();

    for (index, node) in graph.nodes.iter().enumerate() {
        let path = format!("nodes[{index}] ({})", node.id);
        if node.id.trim().is_empty() {
            problems.push(problem(path.clone(), "node id must not be empty"));
            continue;
        }
        if node_map.insert(node.id.clone(), node).is_some() {
            problems.push(problem(path, format!("duplicate node id '{}'", node.id)));
        }
        validate_node(node, index, catalog, &mut problems);
    }

    if !node_map.contains_key(&graph.entry_node_id) {
        problems.push(problem(
            "entry_node_id",
            format!("entry node '{}' does not exist", graph.entry_node_id),
        ));
    }

    let mut outgoing: HashMap<&str, Vec<&TopologyEdge>> = HashMap::new();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

    for (index, edge) in graph.edges.iter().enumerate() {
        let path = format!("edges[{index}] ({})", edge.id);
        if edge.id.trim().is_empty() {
            problems.push(problem(path.clone(), "edge id must not be empty"));
        } else if !edge_ids.insert(edge.id.clone()) {
            problems.push(problem(
                path.clone(),
                format!("duplicate edge id '{}'", edge.id),
            ));
        }

        if !node_map.contains_key(edge.source_node_id.as_str()) {
            problems.push(problem(
                path.clone(),
                format!("source node '{}' does not exist", edge.source_node_id),
            ));
            continue;
        }
        if !node_map.contains_key(edge.target_node_id.as_str()) {
            problems.push(problem(
                path.clone(),
                format!("target node '{}' does not exist", edge.target_node_id),
            ));
            continue;
        }

        outgoing.entry(&edge.source_node_id).or_default().push(edge);
        adjacency
            .entry(&edge.source_node_id)
            .or_default()
            .push(&edge.target_node_id);

        validate_edge(edge, index, &node_map, graph, &mut problems);
    }

    if problems.is_empty() {
        validate_reachability(graph, &adjacency, &mut problems);
        validate_cycles(graph, &adjacency, &mut problems);
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn validate_node(
    node: &TopologyNode,
    index: usize,
    catalog: &TopologyEntityCatalog,
    problems: &mut Vec<TopologyValidationProblem>,
) {
    let path = format!("nodes[{index}] ({})", node.id);

    if matches!(node.iteration_limit, Some(0)) {
        problems.push(problem(
            path.clone(),
            "iteration_limit must be greater than zero",
        ));
    }

    match &node.kind {
        TopologyNodeKind::Llm {
            agent_id,
            task_name,
        } => {
            require_non_empty_field(&path, "agent_id", Some(agent_id.as_str()), problems);
            require_non_empty_field(&path, "task_name", task_name.as_deref(), problems);
            ensure_known(&path, "agent_id", agent_id, &catalog.known_agents, problems);
            if let Some(task_name) = task_name.as_deref() {
                ensure_known(
                    &path,
                    "task_name",
                    task_name,
                    &catalog.known_tasks,
                    problems,
                );
            }
        }
        TopologyNodeKind::Continuation {
            agent_id,
            task_name,
        } => {
            if let Some(agent_id) = agent_id.as_deref() {
                require_non_empty_field(&path, "agent_id", Some(agent_id), problems);
                ensure_known(&path, "agent_id", agent_id, &catalog.known_agents, problems);
            }
            require_non_empty_field(&path, "task_name", task_name.as_deref(), problems);
            if let Some(task_name) = task_name.as_deref() {
                ensure_known(
                    &path,
                    "task_name",
                    task_name,
                    &catalog.known_tasks,
                    problems,
                );
            }
        }
        TopologyNodeKind::Deterministic { handler_name } => {
            require_non_empty_field(&path, "handler_name", Some(handler_name.as_str()), problems);
            if !handler_name.trim().is_empty() {
                ensure_known(
                    &path,
                    "handler_name",
                    handler_name,
                    &catalog.known_handlers,
                    problems,
                );
            }
        }
    }
}

fn validate_edge(
    edge: &TopologyEdge,
    index: usize,
    node_map: &HashMap<String, &TopologyNode>,
    graph: &TopologyGraph,
    problems: &mut Vec<TopologyValidationProblem>,
) {
    let path = format!("edges[{index}] ({})", edge.id);
    let source = node_map
        .get(edge.source_node_id.as_str())
        .expect("source node existence already checked");

    match &edge.routing {
        EdgeRouting::Unconditional | EdgeRouting::Fork => {}
        EdgeRouting::Condition { field_path, .. } => {
            if !output_schema_has_path(source.output_schema.as_ref(), field_path) {
                problems.push(problem(
                    path,
                    format!(
                        "condition field '{}' is not declared in source node '{}' output schema",
                        field_path, edge.source_node_id
                    ),
                ));
            }
        }
        EdgeRouting::FanOut { items_field } => {
            if graph.budget.max_fan_out.unwrap_or(0) == 0 {
                problems.push(problem(
                    path.clone(),
                    "fan-out edges require a positive max_fan_out budget".to_string(),
                ));
            }
            if !output_schema_has_array_path(source.output_schema.as_ref(), items_field) {
                problems.push(problem(
                    path,
                    format!(
                        "fan-out field '{}' must be declared as an array in source node '{}' output schema",
                        items_field, edge.source_node_id
                    ),
                ));
            }
        }
    }
}

fn validate_reachability(
    graph: &TopologyGraph,
    adjacency: &HashMap<&str, Vec<&str>>,
    problems: &mut Vec<TopologyValidationProblem>,
) {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(graph.entry_node_id.as_str());

    while let Some(node_id) = queue.pop_front() {
        if !visited.insert(node_id.to_string()) {
            continue;
        }
        if let Some(neighbours) = adjacency.get(node_id) {
            for neighbour in neighbours {
                queue.push_back(neighbour);
            }
        }
    }

    for node in &graph.nodes {
        if !visited.contains(&node.id) {
            problems.push(problem(
                format!("nodes ({})", node.id),
                format!(
                    "node '{}' is unreachable from entry node '{}'",
                    node.id, graph.entry_node_id
                ),
            ));
        }
    }
}

fn validate_cycles(
    graph: &TopologyGraph,
    adjacency: &HashMap<&str, Vec<&str>>,
    problems: &mut Vec<TopologyValidationProblem>,
) {
    let mut reverse: HashMap<&str, Vec<&str>> = HashMap::new();
    for (source, targets) in adjacency {
        for target in targets {
            reverse.entry(target).or_default().push(source);
        }
    }

    let mut order = Vec::new();
    let mut seen = HashSet::new();
    for node in &graph.nodes {
        dfs_post_order(&node.id, adjacency, &mut seen, &mut order);
    }

    let mut assigned = HashSet::new();
    let node_map: HashMap<&str, &TopologyNode> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let edge_set: HashSet<(&str, &str)> = graph
        .edges
        .iter()
        .map(|edge| (edge.source_node_id.as_str(), edge.target_node_id.as_str()))
        .collect();

    for node_id in order.into_iter().rev() {
        if assigned.contains(node_id.as_str()) {
            continue;
        }
        let mut component = Vec::new();
        dfs_component(node_id.as_str(), &reverse, &mut assigned, &mut component);

        let has_self_loop = component
            .iter()
            .any(|node| edge_set.contains(&(node.as_str(), node.as_str())));
        let is_cycle = component.len() > 1 || has_self_loop;
        if !is_cycle {
            continue;
        }

        let bounded = component.iter().any(|node_id| {
            node_map
                .get(node_id.as_str())
                .and_then(|node| node.iteration_limit)
                .unwrap_or(0)
                > 0
        });

        if !bounded {
            problems.push(problem(
                "graph",
                format!(
                    "cycle involving [{}] has no iteration limit",
                    component.join(", ")
                ),
            ));
        }
    }
}

fn dfs_post_order<'a>(
    node_id: &'a str,
    adjacency: &HashMap<&'a str, Vec<&'a str>>,
    seen: &mut HashSet<String>,
    order: &mut Vec<String>,
) {
    if !seen.insert(node_id.to_string()) {
        return;
    }
    if let Some(targets) = adjacency.get(node_id) {
        for target in targets {
            dfs_post_order(target, adjacency, seen, order);
        }
    }
    order.push(node_id.to_string());
}

fn dfs_component<'a>(
    node_id: &'a str,
    reverse: &HashMap<&'a str, Vec<&'a str>>,
    assigned: &mut HashSet<String>,
    component: &mut Vec<String>,
) {
    if !assigned.insert(node_id.to_string()) {
        return;
    }
    component.push(node_id.to_string());
    if let Some(sources) = reverse.get(node_id) {
        for source in sources {
            dfs_component(source, reverse, assigned, component);
        }
    }
}

fn require_non_empty_field(
    path: &str,
    field_name: &str,
    value: Option<&str>,
    problems: &mut Vec<TopologyValidationProblem>,
) {
    if value.is_none_or(|value| value.trim().is_empty()) {
        problems.push(problem(
            path.to_string(),
            format!("{field_name} must be set for this node kind"),
        ));
    }
}

fn ensure_known(
    path: &str,
    field_name: &str,
    value: &str,
    known_values: &HashSet<String>,
    problems: &mut Vec<TopologyValidationProblem>,
) {
    if !known_values.contains(value) {
        problems.push(problem(
            path.to_string(),
            format!("{field_name} '{value}' does not exist"),
        ));
    }
}

fn output_schema_has_path(schema: Option<&serde_json::Value>, field_path: &str) -> bool {
    schema_subschema(schema, field_path).is_some()
}

fn output_schema_has_array_path(schema: Option<&serde_json::Value>, field_path: &str) -> bool {
    schema_subschema(schema, field_path).is_some_and(schema_declares_array)
}

fn schema_subschema<'a>(
    schema: Option<&'a serde_json::Value>,
    field_path: &str,
) -> Option<&'a serde_json::Value> {
    let mut current = schema?;
    for segment in field_path.split('.') {
        current = current.get("properties")?.get(segment)?;
    }
    Some(current)
}

fn schema_declares_array(schema: &serde_json::Value) -> bool {
    match schema.get("type") {
        Some(serde_json::Value::String(value)) => value == "array",
        Some(serde_json::Value::Array(values)) => {
            values.iter().any(|value| value.as_str() == Some("array"))
        }
        _ => false,
    }
}

fn problem(path: impl Into<String>, message: impl Into<String>) -> TopologyValidationProblem {
    TopologyValidationProblem {
        path: path.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> TopologyEntityCatalog {
        TopologyEntityCatalog {
            known_agents: HashSet::from(["planner".into(), "reviewer".into()]),
            known_tasks: HashSet::from(["draft-plan".into(), "review-plan".into()]),
            known_handlers: HashSet::from(["collect-results".into()]),
        }
    }

    fn object_schema(properties: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": properties,
        })
    }

    #[test]
    fn node_kind_payloads_keep_flat_wire_shape() {
        let node: TopologyNode = serde_json::from_value(serde_json::json!({
            "id": "plan",
            "kind": "llm",
            "agent_id": "planner",
            "task_name": "draft-plan",
            "output_schema": null,
            "iteration_limit": null
        }))
        .expect("flat node JSON should deserialize");

        assert_eq!(node.agent_id(), Some("planner"));
        assert_eq!(node.task_name(), Some("draft-plan"));

        let encoded = serde_json::to_value(&node).expect("node should serialize");
        assert_eq!(encoded["kind"], "llm");
        assert_eq!(encoded["agent_id"], "planner");
        assert!(encoded.get("kind_payload").is_none());
    }

    #[test]
    fn valid_graph_passes_validation() {
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: TopologyBudget {
                max_fan_out: Some(4),
                max_concurrency: Some(2),
                max_total_tokens: Some(10_000),
            },
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "planner".into(),
                        task_name: Some("draft-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({
                        "decision": { "type": "string" }
                    }))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "reviewer".into(),
                        task_name: Some("review-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
            ],
            edges: vec![TopologyEdge {
                id: "plan-to-review".into(),
                source_node_id: "plan".into(),
                target_node_id: "review".into(),
                routing: EdgeRouting::Condition {
                    field_path: "decision".into(),
                    equals: serde_json::json!("continue"),
                },
            }],
        };

        assert!(validate_graph(&graph, &catalog()).is_ok());
    }

    #[test]
    fn unknown_agent_is_rejected() {
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: TopologyBudget::default(),
            nodes: vec![TopologyNode {
                id: "plan".into(),
                kind: TopologyNodeKind::Llm {
                    agent_id: "ghost".into(),
                    task_name: Some("draft-plan".into()),
                },
                output_schema: Some(object_schema(serde_json::json!({}))),
                iteration_limit: None,
            }],
            edges: vec![],
        };

        let problems = validate_graph(&graph, &catalog()).expect_err("graph should fail");
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("agent_id 'ghost'"))
        );
    }

    #[test]
    fn invalid_condition_field_is_rejected() {
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: TopologyBudget::default(),
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "planner".into(),
                        task_name: Some("draft-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({
                        "decision": { "type": "string" }
                    }))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "reviewer".into(),
                        task_name: Some("review-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
            ],
            edges: vec![TopologyEdge {
                id: "bad-edge".into(),
                source_node_id: "plan".into(),
                target_node_id: "review".into(),
                routing: EdgeRouting::Condition {
                    field_path: "missing".into(),
                    equals: serde_json::json!(true),
                },
            }],
        };

        let problems = validate_graph(&graph, &catalog()).expect_err("graph should fail");
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("condition field 'missing'"))
        );
    }

    #[test]
    fn unreachable_node_is_rejected() {
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: TopologyBudget::default(),
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "planner".into(),
                        task_name: Some("draft-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "reviewer".into(),
                        task_name: Some("review-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
            ],
            edges: vec![],
        };

        let problems = validate_graph(&graph, &catalog()).expect_err("graph should fail");
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("unreachable"))
        );
    }

    #[test]
    fn unbounded_cycle_is_rejected() {
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: TopologyBudget::default(),
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "planner".into(),
                        task_name: Some("draft-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "reviewer".into(),
                        task_name: Some("review-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
            ],
            edges: vec![
                TopologyEdge {
                    id: "a".into(),
                    source_node_id: "plan".into(),
                    target_node_id: "review".into(),
                    routing: EdgeRouting::Unconditional,
                },
                TopologyEdge {
                    id: "b".into(),
                    source_node_id: "review".into(),
                    target_node_id: "plan".into(),
                    routing: EdgeRouting::Unconditional,
                },
            ],
        };

        let problems = validate_graph(&graph, &catalog()).expect_err("graph should fail");
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("has no iteration limit"))
        );
    }

    #[test]
    fn bounded_cycle_is_allowed_when_iteration_limit_exists() {
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: TopologyBudget::default(),
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "planner".into(),
                        task_name: Some("draft-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: Some(3),
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "reviewer".into(),
                        task_name: Some("review-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
            ],
            edges: vec![
                TopologyEdge {
                    id: "a".into(),
                    source_node_id: "plan".into(),
                    target_node_id: "review".into(),
                    routing: EdgeRouting::Unconditional,
                },
                TopologyEdge {
                    id: "b".into(),
                    source_node_id: "review".into(),
                    target_node_id: "plan".into(),
                    routing: EdgeRouting::Unconditional,
                },
            ],
        };

        assert!(validate_graph(&graph, &catalog()).is_ok());
    }

    #[test]
    fn fan_out_requires_array_field_and_budget() {
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: TopologyBudget {
                max_fan_out: None,
                max_concurrency: Some(2),
                max_total_tokens: Some(1000),
            },
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "planner".into(),
                        task_name: Some("draft-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({
                        "items": { "type": "string" }
                    }))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm {
                        agent_id: "reviewer".into(),
                        task_name: Some("review-plan".into()),
                    },
                    output_schema: Some(object_schema(serde_json::json!({}))),
                    iteration_limit: None,
                },
            ],
            edges: vec![TopologyEdge {
                id: "fan".into(),
                source_node_id: "plan".into(),
                target_node_id: "review".into(),
                routing: EdgeRouting::FanOut {
                    items_field: "items".into(),
                },
            }],
        };

        let problems = validate_graph(&graph, &catalog()).expect_err("graph should fail");
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("max_fan_out"))
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.message.contains("must be declared as an array"))
        );
    }
}
