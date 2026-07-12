use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use serde_json::Value as JsonValue;
use swage_core::{GatewayError, StorageError};
use swage_storage::model::{EdgeEvaluation, NodeExecution, NodeRetry, TopologyRun};

use crate::{
    state::AppState,
    topology::{TopologyExecutionRequest, TopologyExecutor},
};

// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct CreateTopologyRunRequest {
    #[serde(default)]
    pub topology_source: String,
    #[serde(default)]
    pub flow_name: Option<String>,
    pub message: String,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct TopologyRunResponse {
    pub id: Uuid,
    pub team_id: Uuid,
    pub session_id: Uuid,
    pub state: String,
    pub final_output: Option<JsonValue>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct NodeExecutionResponse {
    pub id: Uuid,
    pub topology_run_id: Uuid,
    pub node_id: String,
    pub node_kind: String,
    pub agent_id: Option<String>,
    pub task_name: Option<String>,
    pub state: String,
    pub input_json: Option<JsonValue>,
    pub output_json: Option<JsonValue>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct EdgeEvaluationResponse {
    pub id: Uuid,
    pub topology_run_id: Uuid,
    pub source_node_execution_id: Uuid,
    pub edge_id: String,
    pub target_node_id: String,
    pub condition_expression: Option<String>,
    pub matched: bool,
    pub payload_json: Option<JsonValue>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct NodeRetryResponse {
    pub id: Uuid,
    pub node_execution_id: Uuid,
    pub attempt_number: i32,
    pub failure_kind: String,
    pub error_message: String,
    pub recovery_guidance: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct TopologyRunDetailResponse {
    pub run: TopologyRunResponse,
    pub nodes: Vec<NodeExecutionResponse>,
    pub edges: Vec<EdgeEvaluationResponse>,
    pub retries: Vec<NodeRetryResponse>,
}

#[derive(Clone)]
pub struct TopologyRunService {
    pub(crate) state: AppState,
}

impl TopologyRunService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub async fn create_run(
        &self,
        team_id: Uuid,
        req: CreateTopologyRunRequest,
    ) -> Result<TopologyRunDetailResponse, GatewayError> {
        if req.topology_source.trim().is_empty() && req.flow_name.is_none() {
            return Err(GatewayError::ValidationError(
                "topology_source or flow_name must be provided".into(),
            ));
        }
        if req.message.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "message must not be empty".into(),
            ));
        }

        let team = self.state.storage.get_team(team_id).await?.ok_or_else(|| {
            GatewayError::Storage(StorageError::NotFound {
                entity: "team".into(),
                id: team_id.to_string(),
            })
        })?;

        let mut known_agents: std::collections::HashSet<String> =
            team.agents.iter().map(|agent| agent.id.clone()).collect();
        known_agents.insert(team.entry_agent_id.clone());
        let catalog = swage_topology::TopologyEntityCatalog {
            known_agents,
            known_tasks: team.tasks.iter().map(|task| task.name.clone()).collect(),
            known_handlers: ["identity", "collect-results"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        };

        let graph = if let Some(flow_name) = req.flow_name.as_deref() {
            crate::authoring_definition::load_topology_flow_graph(
                &self.state,
                team.space_id,
                flow_name,
            )
            .await?
        } else {
            swage_python::topology::execute_topology_source_with_config(
                &self.state.config.tools.python_execute,
                &req.topology_source,
                &format!("team-run:{team_id}"),
                std::time::Duration::from_secs(self.state.config.context.timeout),
                catalog.clone(),
            )
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?
        };
        swage_topology::validate_graph(&graph, &catalog).map_err(|problems| {
            GatewayError::ValidationError(
                problems
                    .into_iter()
                    .map(|problem| problem.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;

        let result = TopologyExecutor::new(self.state.clone())
            .execute(TopologyExecutionRequest {
                team,
                graph,
                initial_input: serde_json::json!({ "message": req.message }),
                session_id: req.session_id,
                source_turn_id: None,
            })
            .await
            .map_err(|error| match error {
                crate::topology::TopologyEngineError::Storage(storage_error) => {
                    GatewayError::Storage(storage_error)
                }
                crate::topology::TopologyEngineError::TeamSpaceMissing { space_id, .. } => {
                    GatewayError::SpaceNotFound { id: space_id }
                }
                crate::topology::TopologyEngineError::SessionMissing { session_id } => {
                    GatewayError::ConversationNotFound { id: session_id }
                }
                other => GatewayError::ValidationError(other.to_string()),
            })?;

        self.get_run(result.topology_run_id).await
    }

    pub async fn get_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<TopologyRunDetailResponse, GatewayError> {
        let run = self
            .state
            .storage
            .get_topology_run(topology_run_id)
            .await?
            .ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "topology_run".into(),
                    id: topology_run_id.to_string(),
                })
            })?;
        let nodes = self
            .state
            .storage
            .list_node_executions_by_run(topology_run_id)
            .await?;
        let edges = self
            .state
            .storage
            .list_edge_evaluations_by_run(topology_run_id)
            .await?;
        let mut retries = Vec::new();
        for node in &nodes {
            retries.extend(self.state.storage.list_node_retries(node.id).await?);
        }

        Ok(TopologyRunDetailResponse {
            run: topology_run_to_response(run),
            nodes: nodes.into_iter().map(node_execution_to_response).collect(),
            edges: edges.into_iter().map(edge_evaluation_to_response).collect(),
            retries: retries.into_iter().map(node_retry_to_response).collect(),
        })
    }

    pub async fn list_runs(&self, team_id: Uuid) -> Result<Vec<TopologyRunResponse>, GatewayError> {
        self.state
            .storage
            .list_topology_runs_by_team(team_id)
            .await
            .map(|runs| runs.into_iter().map(topology_run_to_response).collect())
            .map_err(GatewayError::Storage)
    }
}

fn topology_run_to_response(run: TopologyRun) -> TopologyRunResponse {
    TopologyRunResponse {
        id: run.id,
        team_id: run.team_id,
        session_id: run.session_id,
        state: run.state.to_string(),
        final_output: run.final_output,
        error_message: run.error_message,
        created_at: run.created_at,
        completed_at: run.completed_at,
    }
}

fn node_execution_to_response(node: NodeExecution) -> NodeExecutionResponse {
    NodeExecutionResponse {
        id: node.id,
        topology_run_id: node.topology_run_id,
        node_id: node.node_id,
        node_kind: node.node_kind.to_string(),
        agent_id: node.agent_id,
        task_name: node.task_name,
        state: node.state.to_string(),
        input_json: node.input_json,
        output_json: node.output_json,
        error_message: node.error_message,
        created_at: node.created_at,
        completed_at: node.completed_at,
    }
}

fn edge_evaluation_to_response(edge: EdgeEvaluation) -> EdgeEvaluationResponse {
    EdgeEvaluationResponse {
        id: edge.id,
        topology_run_id: edge.topology_run_id,
        source_node_execution_id: edge.source_node_execution_id,
        edge_id: edge.edge_id,
        target_node_id: edge.target_node_id,
        condition_expression: edge.condition_expression,
        matched: edge.matched,
        payload_json: edge.payload_json,
        created_at: edge.created_at,
    }
}

fn node_retry_to_response(retry: NodeRetry) -> NodeRetryResponse {
    NodeRetryResponse {
        id: retry.id,
        node_execution_id: retry.node_execution_id,
        attempt_number: retry.attempt_number,
        failure_kind: retry.failure_kind,
        error_message: retry.error_message,
        recovery_guidance: retry.recovery_guidance,
        created_at: retry.created_at,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
