use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{heartbeat, schedule, server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;

struct MockProvider {
    rounds: Mutex<VecDeque<Vec<StreamChunk>>>,
}

impl MockProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: Mutex::new(rounds.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "heartbeat-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("tests use streaming only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("tests use streaming only")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = self.rounds.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![StreamChunk::Done {
                finish_reason: None,
            }]
        });
        Box::pin(futures::stream::iter(chunks))
    }
}

fn test_state(db_path: &Path, provider: impl LlmProvider + 'static) -> AppState {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider),
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
    let status = response.status();
    let body = json_response(response).await;
    (status, body)
}

async fn put_json(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = json_response(response).await;
    (status, body)
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
    let status = response.status();
    let body = json_response(response).await;
    (status, body)
}

async fn delete_request(router: &Router, uri: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn create_conversation(router: &Router) -> Uuid {
    let (status, body) = post_json(router, "/api/conversations", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_configuration_survives_restart_and_can_be_enabled_or_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("heartbeat.db");
    let state = test_state(
        &db_path,
        MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "heartbeat".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let (create_status, create_body) = post_json(
        &router,
        "/api/heartbeats",
        json!({
            "session_id": session_id,
            "message": "check in",
            "interval_seconds": 45
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    let heartbeat_id = create_body["id"].as_str().unwrap();

    let (update_status, update_body) = put_json(
        &router,
        &format!("/api/heartbeats/{heartbeat_id}"),
        json!({
            "message": "edited check in",
            "interval_seconds": 90
        }),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK);
    assert_eq!(update_body["message"], "edited check in");
    assert_eq!(update_body["interval_seconds"], 90);

    let (disable_status, disable_body) = post_json(
        &router,
        &format!("/api/heartbeats/{heartbeat_id}/disable"),
        json!({}),
    )
    .await;
    assert_eq!(disable_status, StatusCode::OK);
    assert_eq!(disable_body["status"], "disabled");
    assert!(disable_body["next_beat_at"].is_null());

    let restarted_state = test_state(
        &db_path,
        MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "heartbeat".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]]),
    );
    let restarted_router = build_router(restarted_state.clone());
    let (list_status, list_body) = get_json(&restarted_router, "/api/heartbeats").await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_body["heartbeats"].as_array().unwrap().len(), 1);
    assert_eq!(
        list_body["heartbeats"][0]["session_id"],
        session_id.to_string()
    );
    assert_eq!(list_body["heartbeats"][0]["status"], "disabled");
    assert_eq!(list_body["heartbeats"][0]["interval_seconds"], 90);

    let (enable_status, enable_body) = post_json(
        &restarted_router,
        &format!("/api/heartbeats/{heartbeat_id}/enable"),
        json!({}),
    )
    .await;
    assert_eq!(enable_status, StatusCode::OK);
    assert_eq!(enable_body["status"], "enabled");
    assert!(enable_body["next_beat_at"].is_string());

    let delete_status = delete_request(
        &restarted_router,
        &format!("/api/heartbeats/{heartbeat_id}"),
    )
    .await;
    assert_eq!(delete_status, StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_ticks_repeat_without_scheduler_rows_or_duplicate_due_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("heartbeat-repeat.db");
    let state = test_state(
        &db_path,
        MockProvider::new(vec![
            vec![
                StreamChunk::Delta {
                    content: "beat-1".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "beat-2".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
        ]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let heartbeat = heartbeat::create_heartbeat(
        &state,
        heartbeat::CreateHeartbeatRequest {
            session_id,
            message: "heartbeat prompt".into(),
            interval_seconds: 5,
        },
    )
    .await
    .unwrap();

    assert!(
        state
            .storage
            .list_scheduled_jobs()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        heartbeat::tick_once(&state, heartbeat.next_beat_at.unwrap())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        heartbeat::tick_once(&state, heartbeat.next_beat_at.unwrap())
            .await
            .unwrap(),
        0
    );

    let refreshed = state
        .storage
        .get_heartbeat(heartbeat.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        heartbeat::tick_once(&state, refreshed.next_beat_at.unwrap())
            .await
            .unwrap(),
        1
    );

    let runs = state
        .storage
        .list_heartbeat_runs_by_heartbeat(heartbeat.id)
        .await
        .unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].output_text.as_deref(), Some("beat-1"));
    assert_eq!(runs[1].output_text.as_deref(), Some("beat-2"));
    assert!(
        state
            .storage
            .list_scheduled_jobs()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_runs_are_queryable_separately_from_scheduled_job_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("heartbeat-observability.db");
    let state = test_state(
        &db_path,
        MockProvider::new(vec![
            vec![
                StreamChunk::Delta {
                    content: "heartbeat output".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "scheduled output".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
        ]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let heartbeat = heartbeat::create_heartbeat(
        &state,
        heartbeat::CreateHeartbeatRequest {
            session_id,
            message: "heartbeat prompt".into(),
            interval_seconds: 5,
        },
    )
    .await
    .unwrap();
    let scheduled = schedule::create_scheduled_job(
        &state,
        schedule::CreateScheduledJobRequest {
            session_id,
            message: "scheduled prompt".into(),
            schedule: schedule::ScheduleSpecRequest::Interval {
                every: "every 5 seconds".into(),
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

    heartbeat::tick_once(&state, heartbeat.next_beat_at.unwrap())
        .await
        .unwrap();
    schedule::tick_once(&state, scheduled.next_run_at.unwrap())
        .await
        .unwrap();

    let (heartbeat_status, heartbeat_body) =
        get_json(&router, &format!("/api/heartbeats/{}/runs", heartbeat.id)).await;
    assert_eq!(heartbeat_status, StatusCode::OK);
    assert_eq!(heartbeat_body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(heartbeat_body["runs"][0]["output_text"], "heartbeat output");

    let (scheduled_status, scheduled_body) = get_json(
        &router,
        &format!("/api/scheduled-jobs/{}/runs", scheduled.id),
    )
    .await;
    assert_eq!(scheduled_status, StatusCode::OK);
    assert_eq!(scheduled_body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(scheduled_body["runs"][0]["output_text"], "scheduled output");

    let (turns_status, turns_body) =
        get_json(&router, &format!("/api/conversations/{session_id}/turns")).await;
    assert_eq!(turns_status, StatusCode::OK);
    assert_eq!(turns_body["turns"].as_array().unwrap().len(), 2);
}
