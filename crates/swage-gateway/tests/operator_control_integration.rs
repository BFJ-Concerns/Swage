use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    ApprovalTimeoutPolicy, CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{
    channel_gateway::{ChannelOrigin, DeliveryTarget, RecordedDelivery},
    execution_host::{RegisterExecutionHostRequest, register_remote_execution_host},
    heartbeat::{CreateHeartbeatRequest, create_heartbeat},
    schedule::{CreateScheduledJobRequest, ScheduleSpecRequest, create_scheduled_job},
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::{
    ApprovalKind, NewApprovalRequest, NewTurn, SqliteStorage, TurnState,
    store::{ProjectStore, Storage},
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "operator-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("operator tests use HTTP control paths only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("operator tests use HTTP control paths only")
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

fn test_state() -> AppState {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn ProjectStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
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

async fn create_conversation(router: &Router) -> Uuid {
    let (status, body) = post_json(router, "/api/conversations", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_overview_lists_sessions_jobs_approvals_channels_and_nodes() {
    let state = test_state();
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    state
        .active_connections
        .lock()
        .await
        .insert(session_id);
    state
        .active_turns
        .lock()
        .await
        .insert(session_id, CancellationToken::new());

    create_scheduled_job(
        &state,
        CreateScheduledJobRequest {
            session_id,
            message: "Nightly sweep".into(),
            schedule: ScheduleSpecRequest::Interval {
                every: "every 5 minutes".into(),
            },
            routing: swage_gateway::channel_gateway::DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: Vec::new(),
                store_local_output: true,
            },
        },
    )
    .await
    .unwrap();

    create_heartbeat(
        &state,
        CreateHeartbeatRequest {
            session_id,
            message: "Heartbeat".into(),
            interval_seconds: 60,
        },
    )
    .await
    .unwrap();

    let thread_id = state.storage.list_by_session(session_id).await.unwrap()[0].id;
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("deploy".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: Some(json!({"cmd": "deploy"})),
            hook_name: None,
            prompt: "Allow deploy".into(),
            context: None,
            timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .await
        .unwrap();

    register_remote_execution_host(
        &state,
        RegisterExecutionHostRequest {
            host_id: "docker-a".into(),
            label: Some("Docker A".into()),
            kind: "docker".into(),
            base_url: "http://127.0.0.1:4100".into(),
            shared_token: "secret".into(),
            workspace_root: "/srv/docker".into(),
            capabilities: vec!["shell".into()],
        },
    )
    .await
    .unwrap();
    swage_gateway::presence::disconnect_machine(&state, "docker-a").await;

    state.channel_routes.lock().await.insert(
        ChannelOrigin {
            integration: swage_gateway::channel_gateway::ExternalChannelIntegration::DevDm,
            account: "ops".into(),
            conversation: "alice".into(),
        },
        session_id,
    );
    state.delivery_log.lock().await.push(RecordedDelivery {
        session_id,
        target: DeliveryTarget::Channel {
            integration: swage_gateway::channel_gateway::ExternalChannelIntegration::DevDm,
            account: "ops".into(),
            conversation: "alice".into(),
        },
        content: "Delivered".into(),
        delivered_at: chrono::Utc::now(),
    });

    let (status, body) = get_json(&router, "/api/operator/overview").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["gateway"]["health"], "degraded");
    assert_eq!(body["sessions"][0]["id"], session_id.to_string());
    assert_eq!(body["sessions"][0]["has_active_connection"], true);
    assert_eq!(body["sessions"][0]["has_running_turn"], true);
    assert_eq!(body["approvals"][0]["tool_name"], "shell");
    assert_eq!(body["scheduled_jobs"]["jobs"][0]["message"], "Nightly sweep");
    assert_eq!(body["heartbeats"]["heartbeats"][0]["message"], "Heartbeat");
    assert_eq!(body["channels"]["channels"][0]["integration"], "dev_dm");
    assert_eq!(body["channels"]["channels"][0]["status"], "active");
    let docker_node = body["nodes"]
        .as_array()
        .and_then(|nodes| {
            nodes.iter()
                .find(|node| node["host_id"] == json!("docker-a"))
        })
        .expect("expected docker-a node in operator overview");
    assert_eq!(docker_node["state"], "disconnected");

    let (channel_status, channel_body) = get_json(&router, "/api/channels").await;
    assert_eq!(channel_status, StatusCode::OK);
    assert_eq!(channel_body["channels"][0]["integration"], "dev_dm");
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_controls_manage_approvals_jobs_and_heartbeats_without_full_restart() {
    let state = test_state();
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let scheduled = create_scheduled_job(
        &state,
        CreateScheduledJobRequest {
            session_id,
            message: "Digest".into(),
            schedule: ScheduleSpecRequest::Interval {
                every: "every 10 minutes".into(),
            },
            routing: swage_gateway::channel_gateway::DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: Vec::new(),
                store_local_output: true,
            },
        },
    )
    .await
    .unwrap();

    let heartbeat = create_heartbeat(
        &state,
        CreateHeartbeatRequest {
            session_id,
            message: "Check in".into(),
            interval_seconds: 120,
        },
    )
    .await
    .unwrap();

    let thread_id = state.storage.list_by_session(session_id).await.unwrap()[0].id;
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("danger".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let approval = state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: Some(json!({"cmd": "danger"})),
            hook_name: None,
            prompt: "Allow danger".into(),
            context: None,
            timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .await
        .unwrap();

    let (pause_status, pause_body) = post_json(
        &router,
        &format!("/api/scheduled-jobs/{}/pause", scheduled.id),
        json!({}),
    )
    .await;
    assert_eq!(pause_status, StatusCode::OK);
    assert_eq!(pause_body["status"], "paused");

    let (resume_status, resume_body) = post_json(
        &router,
        &format!("/api/scheduled-jobs/{}/resume", scheduled.id),
        json!({}),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK);
    assert_eq!(resume_body["status"], "active");

    let (reject_status, reject_body) = post_json(
        &router,
        &format!("/api/approvals/{}", approval.id),
        json!({
            "decision": "rejected",
            "reason": "operator review"
        }),
    )
    .await;
    assert_eq!(reject_status, StatusCode::OK);
    assert_eq!(reject_body["decision"], "rejected");

    let (disable_status, disable_body) = post_json(
        &router,
        &format!("/api/heartbeats/{}/disable", heartbeat.id),
        json!({}),
    )
    .await;
    assert_eq!(disable_status, StatusCode::OK);
    assert_eq!(disable_body["status"], "disabled");

    let (enable_status, enable_body) = post_json(
        &router,
        &format!("/api/heartbeats/{}/enable", heartbeat.id),
        json!({}),
    )
    .await;
    assert_eq!(enable_status, StatusCode::OK);
    assert_eq!(enable_body["status"], "enabled");
}
