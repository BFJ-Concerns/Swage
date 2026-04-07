use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    EdgeEvaluation, NewEdgeEvaluation, NewNodeExecution, NewNodeRetry, NewTopologyRun,
    NodeExecution, NodeExecutionState, NodeRetry, TopologyRun, TopologyRunState,
};
use crate::sqlite::SqliteStorage;
use crate::store::TopologyTraceStore;

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn parse_json_value_opt(s: Option<String>) -> rusqlite::Result<Option<serde_json::Value>> {
    s.map(|json| {
        serde_json::from_str(&json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

fn row_to_topology_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<TopologyRun> {
    let id: String = row.get(0)?;
    let team_id: String = row.get(1)?;
    let session_id: String = row.get(2)?;
    let source_turn_id: Option<String> = row.get(3)?;
    let state: String = row.get(4)?;
    let final_output_json: Option<String> = row.get(5)?;
    let created_at: String = row.get(7)?;
    let completed_at: Option<String> = row.get(8)?;

    Ok(TopologyRun {
        id: parse_uuid(&id)?,
        team_id: parse_uuid(&team_id)?,
        session_id: parse_uuid(&session_id)?,
        source_turn_id: source_turn_id.map(|s| parse_uuid(&s)).transpose()?,
        state: TopologyRunState::from_str(&state).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?,
        final_output: parse_json_value_opt(final_output_json)?,
        error_message: row.get(6)?,
        created_at: parse_dt(&created_at)?,
        completed_at: completed_at.map(|s| parse_dt(&s)).transpose()?,
    })
}

fn row_to_node_execution(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeExecution> {
    let id: String = row.get(0)?;
    let topology_run_id: String = row.get(1)?;
    let state: String = row.get(6)?;
    let input_json: Option<String> = row.get(7)?;
    let output_json: Option<String> = row.get(8)?;
    let created_at: String = row.get(10)?;
    let completed_at: Option<String> = row.get(11)?;

    Ok(NodeExecution {
        id: parse_uuid(&id)?,
        topology_run_id: parse_uuid(&topology_run_id)?,
        node_id: row.get(2)?,
        node_kind: row.get(3)?,
        agent_id: row.get(4)?,
        task_name: row.get(5)?,
        state: NodeExecutionState::from_str(&state).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?,
        input_json: parse_json_value_opt(input_json)?,
        output_json: parse_json_value_opt(output_json)?,
        error_message: row.get(9)?,
        created_at: parse_dt(&created_at)?,
        completed_at: completed_at.map(|s| parse_dt(&s)).transpose()?,
    })
}

fn row_to_edge_evaluation(row: &rusqlite::Row<'_>) -> rusqlite::Result<EdgeEvaluation> {
    let id: String = row.get(0)?;
    let topology_run_id: String = row.get(1)?;
    let source_node_execution_id: String = row.get(2)?;
    let payload_json: Option<String> = row.get(7)?;
    let created_at: String = row.get(8)?;

    Ok(EdgeEvaluation {
        id: parse_uuid(&id)?,
        topology_run_id: parse_uuid(&topology_run_id)?,
        source_node_execution_id: parse_uuid(&source_node_execution_id)?,
        edge_id: row.get(3)?,
        target_node_id: row.get(4)?,
        condition_expression: row.get(5)?,
        matched: row.get::<_, i64>(6)? != 0,
        payload_json: parse_json_value_opt(payload_json)?,
        created_at: parse_dt(&created_at)?,
    })
}

fn row_to_node_retry(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRetry> {
    let id: String = row.get(0)?;
    let node_execution_id: String = row.get(1)?;
    let created_at: String = row.get(6)?;

    Ok(NodeRetry {
        id: parse_uuid(&id)?,
        node_execution_id: parse_uuid(&node_execution_id)?,
        attempt_number: row.get(2)?,
        failure_kind: row.get(3)?,
        error_message: row.get(4)?,
        recovery_guidance: row.get(5)?,
        created_at: parse_dt(&created_at)?,
    })
}

fn is_terminal_topology_state(state: &TopologyRunState) -> bool {
    matches!(
        state,
        TopologyRunState::Completed | TopologyRunState::Failed | TopologyRunState::Interrupted
    )
}

fn is_terminal_node_state(state: &NodeExecutionState) -> bool {
    matches!(
        state,
        NodeExecutionState::Completed | NodeExecutionState::Failed | NodeExecutionState::Skipped
    )
}

#[async_trait]
impl TopologyTraceStore for SqliteStorage {
    async fn create_topology_run(&self, run: NewTopologyRun) -> Result<TopologyRun, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let source_turn_id = run.source_turn_id.clone();
            let state = run.state.clone();
            let final_output = run.final_output.clone();
            let error_message = run.error_message.clone();
            let final_output_json = run
                .final_output
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let completed_at = if is_terminal_topology_state(&state) {
                Some(now_str.clone())
            } else {
                None
            };

            conn.execute(
                "INSERT INTO topology_runs (id, team_id, session_id, source_turn_id, state, final_output_json, error_message, created_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.to_string(),
                    run.team_id.to_string(),
                    run.session_id.to_string(),
                    source_turn_id.as_ref().map(ToString::to_string),
                    state.to_string(),
                    final_output_json,
                    &error_message,
                    &now_str,
                    completed_at,
                ],
            )?;

            Ok(TopologyRun {
                id,
                team_id: run.team_id,
                session_id: run.session_id,
                source_turn_id,
                state: state.clone(),
                final_output,
                error_message,
                created_at: now,
                completed_at: if is_terminal_topology_state(&state) { Some(now) } else { None },
            })
        })
        .await
    }

    async fn get_topology_run(&self, id: Uuid) -> Result<Option<TopologyRun>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, team_id, session_id, source_turn_id, state, final_output_json, error_message, created_at, completed_at
                 FROM topology_runs WHERE id = ?1",
                params![id.to_string()],
                row_to_topology_run,
            )
            .optional()
        })
        .await
    }

    async fn list_topology_runs_by_team(
        &self,
        team_id: Uuid,
    ) -> Result<Vec<TopologyRun>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, team_id, session_id, source_turn_id, state, final_output_json, error_message, created_at, completed_at
                 FROM topology_runs WHERE team_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![team_id.to_string()], row_to_topology_run)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn list_topology_runs_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<TopologyRun>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, team_id, session_id, source_turn_id, state, final_output_json, error_message, created_at, completed_at
                 FROM topology_runs WHERE session_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_topology_run)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn update_topology_run(
        &self,
        id: Uuid,
        state: TopologyRunState,
        final_output: Option<&serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        let final_output_json = final_output
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StorageError::Serialisation {
                message: e.to_string(),
            })?;

        self.with_conn(move |conn| {
            let completed_at = if is_terminal_topology_state(&state) {
                Some(Utc::now().to_rfc3339())
            } else {
                None
            };

            conn.execute(
                "UPDATE topology_runs
                 SET state = ?2,
                     final_output_json = COALESCE(?3, final_output_json),
                     error_message = COALESCE(?4, error_message),
                     completed_at = COALESCE(?5, completed_at)
                 WHERE id = ?1",
                params![
                    id.to_string(),
                    state.to_string(),
                    final_output_json,
                    error_message,
                    completed_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn create_node_execution(
        &self,
        execution: NewNodeExecution,
    ) -> Result<NodeExecution, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let state = execution.state.clone();
            let input_json_value = execution.input_json.clone();
            let output_json_value = execution.output_json.clone();
            let error_message = execution.error_message.clone();
            let input_json = execution
                .input_json
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let output_json = execution
                .output_json
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let completed_at = if is_terminal_node_state(&state) {
                Some(now_str.clone())
            } else {
                None
            };

            conn.execute(
                "INSERT INTO node_executions (id, topology_run_id, node_id, node_kind, agent_id, task_name, state, input_json, output_json, error_message, created_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    id.to_string(),
                    execution.topology_run_id.to_string(),
                    &execution.node_id,
                    &execution.node_kind,
                    &execution.agent_id,
                    &execution.task_name,
                    state.to_string(),
                    input_json,
                    output_json,
                    &error_message,
                    &now_str,
                    completed_at,
                ],
            )?;

            Ok(NodeExecution {
                id,
                topology_run_id: execution.topology_run_id,
                node_id: execution.node_id,
                node_kind: execution.node_kind,
                agent_id: execution.agent_id,
                task_name: execution.task_name,
                state: state.clone(),
                input_json: input_json_value,
                output_json: output_json_value,
                error_message,
                created_at: now,
                completed_at: if is_terminal_node_state(&state) { Some(now) } else { None },
            })
        })
        .await
    }

    async fn get_node_execution(&self, id: Uuid) -> Result<Option<NodeExecution>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, topology_run_id, node_id, node_kind, agent_id, task_name, state, input_json, output_json, error_message, created_at, completed_at
                 FROM node_executions WHERE id = ?1",
                params![id.to_string()],
                row_to_node_execution,
            )
            .optional()
        })
        .await
    }

    async fn list_node_executions_by_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<Vec<NodeExecution>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, topology_run_id, node_id, node_kind, agent_id, task_name, state, input_json, output_json, error_message, created_at, completed_at
                 FROM node_executions WHERE topology_run_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![topology_run_id.to_string()], row_to_node_execution)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn update_node_execution(
        &self,
        id: Uuid,
        state: NodeExecutionState,
        output_json: Option<&serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        let output_json = output_json
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StorageError::Serialisation {
                message: e.to_string(),
            })?;

        self.with_conn(move |conn| {
            let completed_at = if is_terminal_node_state(&state) {
                Some(Utc::now().to_rfc3339())
            } else {
                None
            };

            conn.execute(
                "UPDATE node_executions
                 SET state = ?2,
                     output_json = COALESCE(?3, output_json),
                     error_message = COALESCE(?4, error_message),
                     completed_at = COALESCE(?5, completed_at)
                 WHERE id = ?1",
                params![
                    id.to_string(),
                    state.to_string(),
                    output_json,
                    error_message,
                    completed_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn create_edge_evaluation(
        &self,
        evaluation: NewEdgeEvaluation,
    ) -> Result<EdgeEvaluation, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let payload_json = evaluation
                .payload_json
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

            conn.execute(
                "INSERT INTO edge_evaluations (id, topology_run_id, source_node_execution_id, edge_id, target_node_id, condition_expression, matched, payload_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.to_string(),
                    evaluation.topology_run_id.to_string(),
                    evaluation.source_node_execution_id.to_string(),
                    &evaluation.edge_id,
                    &evaluation.target_node_id,
                    &evaluation.condition_expression,
                    if evaluation.matched { 1 } else { 0 },
                    payload_json,
                    &now_str,
                ],
            )?;

            Ok(EdgeEvaluation {
                id,
                topology_run_id: evaluation.topology_run_id,
                source_node_execution_id: evaluation.source_node_execution_id,
                edge_id: evaluation.edge_id,
                target_node_id: evaluation.target_node_id,
                condition_expression: evaluation.condition_expression,
                matched: evaluation.matched,
                payload_json: evaluation.payload_json,
                created_at: now,
            })
        })
        .await
    }

    async fn list_edge_evaluations_by_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<Vec<EdgeEvaluation>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, topology_run_id, source_node_execution_id, edge_id, target_node_id, condition_expression, matched, payload_json, created_at
                 FROM edge_evaluations WHERE topology_run_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![topology_run_id.to_string()], row_to_edge_evaluation)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn create_node_retry(&self, retry: NewNodeRetry) -> Result<NodeRetry, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO node_retries (id, node_execution_id, attempt_number, failure_kind, error_message, recovery_guidance, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id.to_string(),
                    retry.node_execution_id.to_string(),
                    retry.attempt_number,
                    &retry.failure_kind,
                    &retry.error_message,
                    &retry.recovery_guidance,
                    &now_str,
                ],
            )?;

            Ok(NodeRetry {
                id,
                node_execution_id: retry.node_execution_id,
                attempt_number: retry.attempt_number,
                failure_kind: retry.failure_kind,
                error_message: retry.error_message,
                recovery_guidance: retry.recovery_guidance,
                created_at: now,
            })
        })
        .await
    }

    async fn list_node_retries(
        &self,
        node_execution_id: Uuid,
    ) -> Result<Vec<NodeRetry>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, node_execution_id, attempt_number, failure_kind, error_message, recovery_guidance, created_at
                 FROM node_retries WHERE node_execution_id = ?1 ORDER BY attempt_number ASC, created_at ASC",
            )?;
            let rows = stmt.query_map(params![node_execution_id.to_string()], row_to_node_retry)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewProject, NewSession, NewTeam};
    use crate::store::{ProjectStore, SessionStore, TeamStore};

    fn json(value: serde_json::Value) -> Option<serde_json::Value> {
        Some(value)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_topology_run() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject {
                name: "trace-project".into(),
                slug: "trace-project".into(),
                workspace_path: None,
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "analysis".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                members: vec!["planner".into()],
            })
            .await
            .unwrap();

        let run = storage
            .create_topology_run(NewTopologyRun {
                team_id: team.id,
                session_id: session.id,
                source_turn_id: None,
                state: TopologyRunState::Running,
                final_output: None,
                error_message: None,
            })
            .await
            .unwrap();

        let fetched = storage
            .get_topology_run(run.id)
            .await
            .unwrap()
            .expect("run exists");
        assert_eq!(fetched.team_id, team.id);
        assert_eq!(fetched.session_id, session.id);
        assert_eq!(fetched.state, TopologyRunState::Running);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn topology_run_persists_node_edge_and_retry_trace() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject {
                name: "trace-query-project".into(),
                slug: "trace-query-project".into(),
                workspace_path: None,
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "research".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                members: vec!["planner".into(), "reviewer".into()],
            })
            .await
            .unwrap();
        let run = storage
            .create_topology_run(NewTopologyRun {
                team_id: team.id,
                session_id: session.id,
                source_turn_id: None,
                state: TopologyRunState::Running,
                final_output: None,
                error_message: None,
            })
            .await
            .unwrap();

        let node = storage
            .create_node_execution(NewNodeExecution {
                topology_run_id: run.id,
                node_id: "plan".into(),
                node_kind: "llm".into(),
                agent_id: Some("planner".into()),
                task_name: Some("draft-plan".into()),
                state: NodeExecutionState::Running,
                input_json: json(serde_json::json!({"question": "hello"})),
                output_json: None,
                error_message: None,
            })
            .await
            .unwrap();
        storage
            .create_edge_evaluation(NewEdgeEvaluation {
                topology_run_id: run.id,
                source_node_execution_id: node.id,
                edge_id: "plan->review".into(),
                target_node_id: "review".into(),
                condition_expression: Some("decision == 'continue'".into()),
                matched: true,
                payload_json: json(serde_json::json!({"decision": "continue"})),
            })
            .await
            .unwrap();
        storage
            .create_node_retry(NewNodeRetry {
                node_execution_id: node.id,
                attempt_number: 2,
                failure_kind: "schema_validation".into(),
                error_message: "missing field".into(),
                recovery_guidance: Some("return valid JSON".into()),
            })
            .await
            .unwrap();

        let nodes = storage.list_node_executions_by_run(run.id).await.unwrap();
        let edges = storage.list_edge_evaluations_by_run(run.id).await.unwrap();
        let retries = storage.list_node_retries(node.id).await.unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_id, "plan");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_node_id, "review");
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0].attempt_number, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_topology_run_and_node_execution_persist_outputs() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "delivery".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                members: vec!["planner".into()],
            })
            .await
            .unwrap();
        let run = storage
            .create_topology_run(NewTopologyRun {
                team_id: team.id,
                session_id: session.id,
                source_turn_id: None,
                state: TopologyRunState::Running,
                final_output: None,
                error_message: None,
            })
            .await
            .unwrap();
        let node = storage
            .create_node_execution(NewNodeExecution {
                topology_run_id: run.id,
                node_id: "finalise".into(),
                node_kind: "deterministic".into(),
                agent_id: None,
                task_name: Some("package-results".into()),
                state: NodeExecutionState::Running,
                input_json: None,
                output_json: None,
                error_message: None,
            })
            .await
            .unwrap();

        storage
            .update_node_execution(
                node.id,
                NodeExecutionState::Completed,
                Some(&serde_json::json!({"status": "ok"})),
                None,
            )
            .await
            .unwrap();
        storage
            .update_topology_run(
                run.id,
                TopologyRunState::Completed,
                Some(&serde_json::json!({"result": "done"})),
                None,
            )
            .await
            .unwrap();

        let updated_node = storage
            .get_node_execution(node.id)
            .await
            .unwrap()
            .expect("node exists");
        let updated_run = storage
            .get_topology_run(run.id)
            .await
            .unwrap()
            .expect("run exists");

        assert_eq!(updated_node.state, NodeExecutionState::Completed);
        assert_eq!(
            updated_node.output_json,
            json(serde_json::json!({"status": "ok"}))
        );
        assert!(updated_node.completed_at.is_some());
        assert_eq!(updated_run.state, TopologyRunState::Completed);
        assert_eq!(
            updated_run.final_output,
            json(serde_json::json!({"result": "done"}))
        );
        assert!(updated_run.completed_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_team_cascades_topology_trace_records() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "cascade-team".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                members: vec!["planner".into()],
            })
            .await
            .unwrap();
        let run = storage
            .create_topology_run(NewTopologyRun {
                team_id: team.id,
                session_id: session.id,
                source_turn_id: None,
                state: TopologyRunState::Running,
                final_output: None,
                error_message: None,
            })
            .await
            .unwrap();
        let node = storage
            .create_node_execution(NewNodeExecution {
                topology_run_id: run.id,
                node_id: "plan".into(),
                node_kind: "llm".into(),
                agent_id: Some("planner".into()),
                task_name: None,
                state: NodeExecutionState::Running,
                input_json: None,
                output_json: None,
                error_message: None,
            })
            .await
            .unwrap();
        storage
            .create_edge_evaluation(NewEdgeEvaluation {
                topology_run_id: run.id,
                source_node_execution_id: node.id,
                edge_id: "e1".into(),
                target_node_id: "review".into(),
                condition_expression: None,
                matched: true,
                payload_json: None,
            })
            .await
            .unwrap();
        storage
            .create_node_retry(NewNodeRetry {
                node_execution_id: node.id,
                attempt_number: 1,
                failure_kind: "timeout".into(),
                error_message: "timed out".into(),
                recovery_guidance: None,
            })
            .await
            .unwrap();

        assert!(storage.delete_team(team.id).await.unwrap());
        assert!(storage.get_topology_run(run.id).await.unwrap().is_none());
        assert!(storage.get_node_execution(node.id).await.unwrap().is_none());
        assert!(
            storage
                .list_edge_evaluations_by_run(run.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(storage.list_node_retries(node.id).await.unwrap().is_empty());
    }
}
