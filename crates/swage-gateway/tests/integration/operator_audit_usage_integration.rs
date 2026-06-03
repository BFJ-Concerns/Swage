#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    ApprovalTimeoutPolicy, CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{
    channel_gateway::{DeliveryTarget, deliver_to_targets},
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::{
    ApprovalKind, HeartbeatRunState, HeartbeatStatus, NewAgentMemoryRecord, NewApprovalRequest,
    NewHeartbeat, NewNodeExecution, NewScheduledJob, NewScheduledRun, NewSession, NewTeam,
    NewThread, NewTopologyRun, NewTurn, NodeExecutionState, ScheduledJobKind, ScheduledJobStatus,
    ScheduledRunState, ScheduledRunTrigger, SqliteStorage, TopologyRunState, TurnState,
    model::{AppendMessage, TopologyNodeRecordKind},
    store::{
        AgentMemoryStore, ApprovalStore, BenchStore, HeartbeatStore, MessageStore, ScheduleStore,
        SessionStore, Storage, TeamStore, ThreadStore, TopologyTraceStore, TurnStore,
    },
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "operator-audit-usage-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("operator audit tests inspect persisted state only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("operator audit tests inspect persisted state only")
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
        Arc::clone(&storage) as Arc<dyn BenchStore>,
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

async fn post_json(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

async fn seed_session(storage: &Arc<SqliteStorage>) -> (Uuid, Uuid, Uuid) {
    let bench = storage.get_or_create_default().await.unwrap();
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".into(),
            metadata: Some(json!({"label": "Ops session"})),
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
    (bench.id, session.id, thread.id)
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_search_queries_sessions_messages_approvals_runs_and_deliveries() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state.clone());
    let (_bench_id, session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("searchable user input".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let message = storage
        .append_message(AppendMessage {
            thread_id,
            turn_id: Some(turn.id),
            role: swage_core::Role::User,
            content: "message needle".into(),
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();
    let approval = storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: None,
            hook_name: None,
            prompt: "approval needle".into(),
            context: None,
            timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .await
        .unwrap();
    let job = storage
        .create_scheduled_job(NewScheduledJob {
            session_id,
            agent_id: "gateway".into(),
            message: "schedule needle".into(),
            status: ScheduledJobStatus::Active,
            kind: ScheduledJobKind::Interval,
            schedule_expression: "every minute".into(),
            delivery_routing: json!({"store_local_output": true}),
            next_run_at: None,
            last_run_at: None,
        })
        .await
        .unwrap();
    let scheduled_run = storage
        .create_scheduled_run(NewScheduledRun {
            job_id: job.id,
            trigger: ScheduledRunTrigger::Manual,
            state: ScheduledRunState::Completed,
            scheduled_for: None,
            turn_id: Some(turn.id),
            output_text: Some("run output".into()),
            error_message: None,
            started_at: Some(chrono::Utc::now()),
            completed_at: Some(chrono::Utc::now()),
        })
        .await
        .unwrap();
    deliver_to_targets(
        &state,
        session_id,
        &[DeliveryTarget::LocalStorage],
        "delivery needle",
    )
    .await
    .unwrap();

    let (session_status, session_body) =
        get_json(&router, "/api/operator/search?kind=session&q=Ops%20session").await;
    assert_eq!(session_status, StatusCode::OK);
    assert_eq!(session_body["results"][0]["id"], session_id.to_string());

    let (message_status, message_body) = get_json(
        &router,
        "/api/operator/search?kind=message&q=message%20needle",
    )
    .await;
    assert_eq!(message_status, StatusCode::OK);
    assert_eq!(message_body["results"][0]["id"], message.id.to_string());

    let (approval_status, approval_body) = get_json(
        &router,
        "/api/operator/search?kind=approval&q=approval%20needle",
    )
    .await;
    assert_eq!(approval_status, StatusCode::OK);
    assert_eq!(approval_body["results"][0]["id"], approval.id.to_string());

    let (run_status, run_body) = get_json(
        &router,
        "/api/operator/search?kind=scheduled_run&q=schedule%20needle",
    )
    .await;
    assert_eq!(run_status, StatusCode::OK);
    assert_eq!(run_body["results"][0]["id"], scheduled_run.id.to_string());

    let (delivery_status, delivery_body) = get_json(
        &router,
        "/api/operator/search?kind=delivery&q=delivery%20needle",
    )
    .await;
    assert_eq!(delivery_status, StatusCode::OK);
    assert_eq!(delivery_body["results"][0]["kind"], "delivery");
    assert_eq!(
        delivery_body["results"][0]["session_id"],
        session_id.to_string()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_memory_surfaces_expose_provenance_and_searchability() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state);
    let (bench_id, session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Completed,
            user_input: Some("remember this".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let team = storage
        .create_team(NewTeam {
            bench_id,
            name: "ops".into(),
            description: "ops".into(),
            entry_agent_id: "gateway".into(),
            agents: vec![swage_storage::model::TeamAgentDefinition {
                id: "gateway".into(),
                display_name: "Gateway".into(),
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
            source_turn_id: Some(turn.id),
            state: TopologyRunState::Completed,
            final_output: Some(json!({"result": "done"})),
            error_message: None,
        })
        .await
        .unwrap();
    let node = storage
        .create_node_execution(NewNodeExecution {
            topology_run_id: topology_run.id,
            node_id: "node-a".into(),
            node_kind: TopologyNodeRecordKind::Llm,
            agent_id: Some("gateway".into()),
            task_name: Some("research".into()),
            state: NodeExecutionState::Completed,
            input_json: None,
            output_json: Some(json!({"summary": "done"})),
            error_message: None,
        })
        .await
        .unwrap();
    let memory = storage
        .create_agent_memory(NewAgentMemoryRecord {
            id: Uuid::new_v4(),
            bench_id,
            session_id,
            turn_id: turn.id,
            topology_run_id: Some(topology_run.id),
            node_execution_id: Some(node.id),
            agent_id: "gateway".into(),
            memory_kind: "outcome".into(),
            title: "Preview route requirement".into(),
            content: "The builder runtime still depends on the preview route.".into(),
            tags: vec!["builder".into(), "preview".into()],
            outcome_status: Some("succeeded".into()),
            source_summary: "topology node research".into(),
            metadata: json!({"confidence": "high"}),
        })
        .await
        .unwrap();

    let (list_status, list_body) = get_json(
        &router,
        &format!("/api/operator/memories?session_id={session_id}"),
    )
    .await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_body["memories"][0]["id"], memory.id.to_string());
    assert_eq!(
        list_body["memories"][0]["topology_run_id"],
        topology_run.id.to_string()
    );
    assert_eq!(
        list_body["memories"][0]["node_execution_id"],
        node.id.to_string()
    );

    let (detail_status, detail_body) =
        get_json(&router, &format!("/api/operator/memories/{}", memory.id)).await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail_body["title"], "Preview route requirement");

    let (search_status, search_body) = get_json(
        &router,
        "/api/operator/search?kind=agent_memory&q=preview%20route",
    )
    .await;
    assert_eq!(search_status, StatusCode::OK);
    assert_eq!(search_body["results"][0]["id"], memory.id.to_string());
    assert_eq!(search_body["results"][0]["kind"], "agent_memory");
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_trail_survives_restart_for_approvals_deliveries_runs_and_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("operator-audit.db");
    let storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state.clone());
    let (_bench_id, session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("approval".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let approval = storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: None,
            hook_name: None,
            prompt: "approve me".into(),
            context: None,
            timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .await
        .unwrap();
    let (approval_status, _) = post_json(
        &router,
        &format!("/api/approvals/{}", approval.id),
        json!({"decision": "approved", "reason": "operator approved"}),
    )
    .await;
    assert_eq!(approval_status, StatusCode::OK);

    let job = storage
        .create_scheduled_job(NewScheduledJob {
            session_id,
            agent_id: "gateway".into(),
            message: "nightly".into(),
            status: ScheduledJobStatus::Active,
            kind: ScheduledJobKind::Interval,
            schedule_expression: "every minute".into(),
            delivery_routing: json!({"store_local_output": true}),
            next_run_at: None,
            last_run_at: None,
        })
        .await
        .unwrap();
    let run = storage
        .create_scheduled_run(NewScheduledRun {
            job_id: job.id,
            trigger: ScheduledRunTrigger::Scheduled,
            state: ScheduledRunState::Running,
            scheduled_for: None,
            turn_id: None,
            output_text: None,
            error_message: None,
            started_at: Some(chrono::Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();
    storage
        .update_scheduled_run_state(
            run.id,
            ScheduledRunState::Failed,
            None,
            None,
            Some("failed later"),
        )
        .await
        .unwrap();

    let heartbeat = storage
        .create_heartbeat(NewHeartbeat {
            session_id,
            agent_id: "gateway".into(),
            message: "hb".into(),
            interval_seconds: 60,
            status: HeartbeatStatus::Enabled,
            last_beat_at: None,
            next_beat_at: None,
        })
        .await
        .unwrap();
    let _heartbeat_run = storage
        .create_heartbeat_run(swage_storage::NewHeartbeatRun {
            heartbeat_id: heartbeat.id,
            state: HeartbeatRunState::Completed,
            scheduled_for: None,
            turn_id: None,
            output_text: Some("ok".into()),
            error_message: None,
            started_at: Some(chrono::Utc::now()),
            completed_at: Some(chrono::Utc::now()),
        })
        .await
        .unwrap();

    let team = storage
        .create_team(NewTeam {
            bench_id: storage.get_or_create_default().await.unwrap().id,
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
            source_turn_id: Some(turn.id),
            state: TopologyRunState::Running,
            final_output: None,
            error_message: None,
        })
        .await
        .unwrap();
    let node = storage
        .create_node_execution(NewNodeExecution {
            topology_run_id: topology_run.id,
            node_id: "node-a".into(),
            node_kind: TopologyNodeRecordKind::Llm,
            agent_id: Some("gateway".into()),
            task_name: Some("research".into()),
            state: NodeExecutionState::Running,
            input_json: Some(json!({"task": "research"})),
            output_json: None,
            error_message: None,
        })
        .await
        .unwrap();
    storage
        .update_node_execution(
            node.id,
            NodeExecutionState::Failed,
            None,
            Some("node failed"),
        )
        .await
        .unwrap();

    deliver_to_targets(
        &state,
        session_id,
        &[DeliveryTarget::Channel {
            integration: swage_gateway::channel_gateway::ExternalChannelIntegration::DevDm,
            account: "ops".into(),
            conversation: "alice".into(),
        }],
        "delivered after routing",
    )
    .await
    .unwrap();

    drop(router);
    drop(state);
    drop(storage);

    let restarted_storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let restarted_state = test_state(Arc::clone(&restarted_storage));
    let restarted_router = build_router(restarted_state);

    let (status, body) = get_json(
        &restarted_router,
        &format!("/api/operator/search?kind=audit_event&session_id={session_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let excerpts = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| {
            format!(
                "{} {}",
                result["title"].as_str().unwrap_or_default(),
                result["excerpt"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();

    assert!(
        excerpts.iter().any(|excerpt| {
            excerpt.contains("approval state pending -> approved")
                && excerpt.contains("operator approved")
        }),
        "expected approval audit event, got: {body:?}"
    );
    assert!(
        excerpts.iter().any(|excerpt| {
            excerpt.contains("scheduled run state running -> failed")
                && excerpt.contains("\"from_state\":\"running\"")
        }),
        "expected scheduled-run transition audit event, got: {body:?}"
    );
    assert!(
        excerpts.iter().any(|excerpt| {
            excerpt.contains("node execution created") && excerpt.contains("\"node_id\":\"node-a\"")
        }),
        "expected node creation audit event, got: {body:?}"
    );
    assert!(
        excerpts.iter().any(|excerpt| {
            excerpt.contains("delivery recorded") && excerpt.contains("delivered after routing")
        }),
        "expected delivery audit event, got: {body:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_usage_reports_run_level_and_aggregate_costs() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state);
    let (_bench_id, session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("usage".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage.update_tokens(turn.id, 120, 45).await.unwrap();
    storage.update_cost(turn.id, 700).await.unwrap();
    storage
        .update_turn_state(turn.id, TurnState::Completed)
        .await
        .unwrap();

    let job = storage
        .create_scheduled_job(NewScheduledJob {
            session_id,
            agent_id: "gateway".into(),
            message: "usage job".into(),
            status: ScheduledJobStatus::Active,
            kind: ScheduledJobKind::Interval,
            schedule_expression: "every minute".into(),
            delivery_routing: json!({"store_local_output": true}),
            next_run_at: None,
            last_run_at: None,
        })
        .await
        .unwrap();
    storage
        .create_scheduled_run(NewScheduledRun {
            job_id: job.id,
            trigger: ScheduledRunTrigger::Manual,
            state: ScheduledRunState::Completed,
            scheduled_for: None,
            turn_id: Some(turn.id),
            output_text: Some("done".into()),
            error_message: None,
            started_at: Some(chrono::Utc::now()),
            completed_at: Some(chrono::Utc::now()),
        })
        .await
        .unwrap();

    let (status, body) = get_json(
        &router,
        &format!(
            "/api/operator/usage?run_kind=user_turn&run_id={}&session_id={}&scheduled_job_id={}",
            turn.id, session_id, job.id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["run"]["input_tokens"], 120);
    assert_eq!(body["run"]["output_tokens"], 45);
    assert_eq!(body["run"]["cost_microcents"], 700);
    assert_eq!(body["session"]["input_tokens"], 120);
    assert_eq!(body["session"]["output_tokens"], 45);
    assert_eq!(body["session"]["cost_microcents"], 700);
    assert_eq!(body["scheduled_job"]["run_count"], 1);
    assert_eq!(body["scheduled_job"]["input_tokens"], 120);
    assert_eq!(body["scheduled_job"]["cost_microcents"], 700);
}
