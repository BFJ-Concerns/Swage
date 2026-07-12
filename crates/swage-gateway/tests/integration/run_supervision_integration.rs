#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    NewNodeExecution, NewSchedule, NewScheduleRun, NewSession, NewTeam, NewThread, NewTopologyRun,
    NewTurn, NodeExecutionState, ScheduleCreator, ScheduleKind, ScheduleRunState,
    ScheduleRunTrigger, ScheduleRunUpdate, ScheduleStatus, SessionPolicy, SqliteStorage,
    TopologyRunState, TurnState,
    model::TopologyNodeRecordKind,
    store::{
        ScheduleStore, SessionStore, SpaceStore, Storage, TeamStore, ThreadStore,
        TopologyTraceStore, TurnStore,
    },
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "run-supervision-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("run supervision tests inspect persisted state only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("run supervision tests inspect persisted state only")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(vec![StreamChunk::Done {
            finish_reason: None,
        }]))
    }
}

fn test_state(storage: Arc<SqliteStorage>) -> AppState {
    AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    )
}

async fn json_response(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    }
}

async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

async fn seed_session(storage: &Arc<SqliteStorage>) -> (Uuid, Uuid, Uuid) {
    let space = storage.get_or_create_default().await.unwrap();
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".into(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .unwrap();
    let thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: "gateway".into(),
        },
    )
    .await
    .unwrap();
    (space.id, session.id, thread.id)
}

async fn seed_thread(storage: &Arc<SqliteStorage>, session_id: Uuid) -> Uuid {
    ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id,
            agent_id: "gateway".into(),
        },
    )
    .await
    .unwrap()
    .id
}

async fn age_turn(storage: &Arc<SqliteStorage>, turn_id: Uuid, age: Duration) {
    let stale_time = (Utc::now() - age).to_rfc3339();
    storage
        .with_conn(|conn| {
            conn.execute(
                "UPDATE turns SET created_at = ?1 WHERE id = ?2",
                (stale_time, turn_id.to_string()),
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

fn run_status_by_id(body: &Value) -> std::collections::HashMap<Uuid, (String, String, Value)> {
    body["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| {
            (
                run["id"].as_str().unwrap().parse().unwrap(),
                (
                    run["kind"].as_str().unwrap().to_string(),
                    run["status"].as_str().unwrap().to_string(),
                    run.clone(),
                ),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_runs_report_live_statuses_across_turn_states() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state);
    let (_space_id, session_id, thread_id) = seed_session(&storage).await;
    let awaiting_thread_id = seed_thread(&storage, session_id).await;
    let delegating_thread_id = seed_thread(&storage, session_id).await;
    let stuck_thread_id = seed_thread(&storage, session_id).await;
    let failed_thread_id = seed_thread(&storage, session_id).await;
    let completed_thread_id = seed_thread(&storage, session_id).await;

    let running_turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("fresh".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let awaiting_turn = storage
        .create_turn(NewTurn {
            thread_id: awaiting_thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("approval".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let delegating_turn = storage
        .create_turn(NewTurn {
            thread_id: delegating_thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Delegating,
            user_input: Some("delegate".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let stuck_turn = storage
        .create_turn(NewTurn {
            thread_id: stuck_thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("stale".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    age_turn(&storage, stuck_turn.id, Duration::minutes(6)).await;

    let failed_turn = storage
        .create_turn(NewTurn {
            thread_id: failed_thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("fail".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage
        .update_turn_state(failed_turn.id, TurnState::Failed)
        .await
        .unwrap();

    let completed_turn = storage
        .create_turn(NewTurn {
            thread_id: completed_thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("done".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage
        .update_response(completed_turn.id, "done")
        .await
        .unwrap();
    storage
        .update_turn_state(completed_turn.id, TurnState::Completed)
        .await
        .unwrap();

    let (status, body) = get_json(&router, "/api/operator/runs").await;
    assert_eq!(status, StatusCode::OK);

    let statuses = run_status_by_id(&body);
    assert_eq!(statuses[&running_turn.id].1, "running");
    assert_eq!(statuses[&awaiting_turn.id].1, "awaiting_approval");
    assert_eq!(statuses[&delegating_turn.id].1, "delegating");
    assert_eq!(statuses[&stuck_turn.id].1, "stuck");
    assert_eq!(statuses[&failed_turn.id].1, "failed");
    assert_eq!(statuses[&completed_turn.id].1, "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_runs_survive_restart_with_durable_history() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("run-supervision.db");

    let storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let state = test_state(Arc::clone(&storage));
    let (_space_id, session_id, thread_id) = seed_session(&storage).await;

    let completed_turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("user history".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage
        .update_response(completed_turn.id, "user complete")
        .await
        .unwrap();
    storage
        .update_turn_state(completed_turn.id, TurnState::Completed)
        .await
        .unwrap();

    let delegated_turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "delegate".into(),
            state: TurnState::Processing,
            user_input: Some("delegated history".into()),
            parent_turn_id: Some(completed_turn.id),
        })
        .await
        .unwrap();
    storage
        .update_response(delegated_turn.id, "delegated complete")
        .await
        .unwrap();
    storage
        .update_turn_state(delegated_turn.id, TurnState::Completed)
        .await
        .unwrap();

    let schedule = storage
        .create_schedule(NewSchedule {
            message: "nightly".into(),
            status: ScheduleStatus::Active,
            kind: ScheduleKind::Cron,
            expression: "0 * * * * *".into(),
            session_policy: SessionPolicy::Continuation { session_id },
            creator: ScheduleCreator::User,
            delivery_routing: json!({"store_local_output": true}),
            gate: None,
            next_run_at: None,
            last_run_at: None,
        })
        .await
        .unwrap();
    let failed_run = storage
        .create_schedule_run(NewScheduleRun::pending(
            schedule.id,
            ScheduleRunTrigger::Tick,
            Some(Utc::now()),
        ))
        .await
        .unwrap();
    storage
        .update_schedule_run(
            failed_run.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Failed),
                error_message: Some("cron failed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed_run = storage
        .create_schedule_run(NewScheduleRun::pending(
            schedule.id,
            ScheduleRunTrigger::Tick,
            Some(Utc::now()),
        ))
        .await
        .unwrap();
    storage
        .update_schedule_run(
            completed_run.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Completed),
                output_text: Some("beat ok".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let team = storage
        .create_team(NewTeam {
            space_id: storage.get_or_create_default().await.unwrap().id,
            name: "ops".into(),
            description: "ops".into(),
            entry_agent_id: "gateway".into(),
            agents: vec![swage_storage::model::TeamAgentDefinition {
                id: "gateway".into(),
                display_name: "gateway".into(),
                description: String::new(),
                instructions: String::new(),
                model: None,
            }],
            tasks: vec![],
        })
        .await
        .unwrap();
    let topology_run = storage
        .create_topology_run(NewTopologyRun {
            team_id: team.id,
            session_id,
            source_turn_id: Some(completed_turn.id),
            state: TopologyRunState::Running,
            final_output: None,
            error_message: None,
        })
        .await
        .unwrap();
    let node_execution = storage
        .create_node_execution(NewNodeExecution {
            topology_run_id: topology_run.id,
            node_id: "delegate-node".into(),
            node_kind: TopologyNodeRecordKind::Llm,
            agent_id: Some("delegate".into()),
            task_name: Some("delegate task".into()),
            state: NodeExecutionState::Running,
            input_json: Some(json!({"work": "x"})),
            output_json: None,
            error_message: None,
        })
        .await
        .unwrap();
    storage
        .update_node_execution(
            node_execution.id,
            NodeExecutionState::Failed,
            None,
            Some("node failed"),
        )
        .await
        .unwrap();
    storage
        .update_topology_run(
            topology_run.id,
            TopologyRunState::Completed,
            Some(&json!({"result": "complete"})),
            None,
        )
        .await
        .unwrap();

    drop(state);
    drop(storage);

    let restarted_storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let restarted_state = test_state(Arc::clone(&restarted_storage));
    let restarted_router = build_router(restarted_state);

    let (status, body) = get_json(&restarted_router, "/api/operator/runs").await;
    assert_eq!(status, StatusCode::OK);

    let runs = run_status_by_id(&body);
    assert_eq!(runs[&completed_turn.id].0, "user_turn");
    assert_eq!(runs[&completed_turn.id].1, "completed");
    assert!(runs[&completed_turn.id].2["completed_at"].is_string());

    assert_eq!(runs[&delegated_turn.id].0, "delegated_turn");
    assert_eq!(runs[&delegated_turn.id].1, "completed");
    assert!(runs[&delegated_turn.id].2["completed_at"].is_string());

    assert_eq!(runs[&failed_run.id].0, "schedule_run");
    assert_eq!(runs[&failed_run.id].1, "failed");
    assert!(runs[&failed_run.id].2["completed_at"].is_string());

    assert_eq!(runs[&completed_run.id].0, "schedule_run");
    assert_eq!(runs[&completed_run.id].1, "completed");
    assert!(runs[&completed_run.id].2["completed_at"].is_string());

    assert_eq!(runs[&node_execution.id].0, "node_execution");
    assert_eq!(runs[&node_execution.id].1, "failed");
    assert!(runs[&node_execution.id].2["completed_at"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_run_detail_tracks_live_execution_into_history() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state);
    let (_space_id, _session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("inspect me".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let (running_status, running_body) = get_json(
        &router,
        &format!("/api/operator/runs/user_turn/{}", turn.id),
    )
    .await;
    assert_eq!(running_status, StatusCode::OK);
    assert_eq!(running_body["run"]["status"], "running");
    assert_eq!(running_body["detail"]["user_input"], "inspect me");
    assert_eq!(running_body["detail"]["assistant_response"], Value::Null);

    storage.update_response(turn.id, "finished").await.unwrap();
    storage
        .update_turn_state(turn.id, TurnState::Completed)
        .await
        .unwrap();

    let (completed_status, completed_body) = get_json(
        &router,
        &format!("/api/operator/runs/user_turn/{}", turn.id),
    )
    .await;
    assert_eq!(completed_status, StatusCode::OK);
    assert_eq!(completed_body["run"]["status"], "completed");
    assert_eq!(completed_body["detail"]["assistant_response"], "finished");
    assert!(completed_body["run"]["completed_at"].is_string());
}
