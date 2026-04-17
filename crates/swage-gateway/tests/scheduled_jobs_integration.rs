use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{Datelike, Duration, Timelike, Utc};
use serde_json::{Value, json};
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{schedule, server::build_router, state::AppState};
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
        "scheduled-mock"
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
        Arc::clone(&storage) as Arc<dyn swage_storage::store::ProjectStore>,
        Arc::new(provider),
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

use uuid::Uuid;

fn local_routing() -> swage_gateway::channel_gateway::DeliveryRoutingRequest {
    swage_gateway::channel_gateway::DeliveryRoutingRequest {
        deliver_to_origin: false,
        home_target: None,
        explicit_targets: Vec::new(),
        store_local_output: true,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn create_edit_pause_resume_trigger_delete_and_restart_job_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("scheduled.db");
    let state = test_state(
        &db_path,
        MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "manual result".into(),
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
        "/api/scheduled-jobs",
        json!({
            "session_id": session_id,
            "message": "initial",
            "schedule": {
                "kind": "interval",
                "every": "every 5 seconds"
            }
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    let job_id = create_body["id"].as_str().unwrap();

    let (list_status, list_body) = get_json(&router, "/api/scheduled-jobs").await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_body["jobs"].as_array().unwrap().len(), 1);

    let (update_status, update_body) = put_json(
        &router,
        &format!("/api/scheduled-jobs/{job_id}"),
        json!({
            "message": "edited",
            "schedule": {
                "kind": "interval",
                "every": "every 10 seconds"
            }
        }),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK);
    assert_eq!(update_body["message"], "edited");
    assert_eq!(update_body["schedule"]["every"], "every 10 seconds");

    let (pause_status, pause_body) = post_json(
        &router,
        &format!("/api/scheduled-jobs/{job_id}/pause"),
        json!({}),
    )
    .await;
    assert_eq!(pause_status, StatusCode::OK);
    assert_eq!(pause_body["status"], "paused");

    let restarted_state = test_state(
        &db_path,
        MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "manual result".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]]),
    );
    let restarted_router = build_router(restarted_state.clone());
    let (restarted_list_status, restarted_list_body) =
        get_json(&restarted_router, "/api/scheduled-jobs").await;
    assert_eq!(restarted_list_status, StatusCode::OK);
    assert_eq!(restarted_list_body["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(restarted_list_body["jobs"][0]["status"], "paused");
    assert_eq!(restarted_list_body["jobs"][0]["message"], "edited");

    let (resume_status, resume_body) = post_json(
        &restarted_router,
        &format!("/api/scheduled-jobs/{job_id}/resume"),
        json!({}),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK);
    assert_eq!(resume_body["status"], "active");

    let (trigger_status, trigger_body) = post_json(
        &restarted_router,
        &format!("/api/scheduled-jobs/{job_id}/trigger"),
        json!({}),
    )
    .await;
    assert_eq!(trigger_status, StatusCode::ACCEPTED);
    assert_eq!(trigger_body["run"]["state"], "completed");
    assert_eq!(trigger_body["run"]["output_text"], "manual result");

    let (runs_status, runs_body) = get_json(
        &restarted_router,
        &format!("/api/scheduled-jobs/{job_id}/runs"),
    )
    .await;
    assert_eq!(runs_status, StatusCode::OK);
    assert_eq!(runs_body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(runs_body["runs"][0]["output_text"], "manual result");

    let delete_status =
        delete_request(&restarted_router, &format!("/api/scheduled-jobs/{job_id}")).await;
    assert_eq!(delete_status, StatusCode::NO_CONTENT);
    let (_, empty_list_body) = get_json(&restarted_router, "/api/scheduled-jobs").await;
    assert!(empty_list_body["jobs"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn one_shot_interval_and_cron_jobs_execute_when_their_due_time_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("timing.db");
    let state = test_state(
        &db_path,
        MockProvider::new(vec![
            vec![
                StreamChunk::Delta {
                    content: "one-shot".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "interval-1".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "interval-2".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "cron".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
        ]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let now = Utc::now();
    let cron_due_at = now + Duration::seconds(12);
    let cron_expression = format!(
        "{} {} {} {} {} * {}",
        cron_due_at.second(),
        cron_due_at.minute(),
        cron_due_at.hour(),
        cron_due_at.day(),
        cron_due_at.month(),
        cron_due_at.year()
    );
    let one_shot = schedule::create_scheduled_job(
        &state,
        schedule::CreateScheduledJobRequest {
            session_id,
            message: "run one-shot".into(),
            schedule: schedule::ScheduleSpecRequest::OneShot {
                at: (now + Duration::seconds(1)).to_rfc3339(),
            },
            routing: local_routing(),
        },
    )
    .await
    .unwrap();
    let interval = schedule::create_scheduled_job(
        &state,
        schedule::CreateScheduledJobRequest {
            session_id,
            message: "run interval".into(),
            schedule: schedule::ScheduleSpecRequest::Interval {
                every: "every 5 seconds".into(),
            },
            routing: local_routing(),
        },
    )
    .await
    .unwrap();
    let cron = schedule::create_scheduled_job(
        &state,
        schedule::CreateScheduledJobRequest {
            session_id,
            message: "run cron".into(),
            schedule: schedule::ScheduleSpecRequest::Cron {
                cron: cron_expression,
            },
            routing: local_routing(),
        },
    )
    .await
    .unwrap();

    assert_eq!(schedule::tick_once(&state, now).await.unwrap(), 0);
    assert_eq!(
        schedule::tick_once(&state, one_shot.next_run_at.unwrap())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        schedule::tick_once(&state, interval.next_run_at.unwrap())
            .await
            .unwrap(),
        1
    );
    let refreshed_interval = state
        .storage
        .get_scheduled_job(interval.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        schedule::tick_once(&state, refreshed_interval.next_run_at.unwrap())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        schedule::tick_once(&state, cron.next_run_at.unwrap())
            .await
            .unwrap(),
        1
    );

    let one_shot_runs = state
        .storage
        .list_scheduled_runs_by_job(one_shot.id)
        .await
        .unwrap();
    let interval_runs = state
        .storage
        .list_scheduled_runs_by_job(interval.id)
        .await
        .unwrap();
    let cron_runs = state
        .storage
        .list_scheduled_runs_by_job(cron.id)
        .await
        .unwrap();
    assert_eq!(one_shot_runs.len(), 1);
    assert_eq!(interval_runs.len(), 2);
    assert_eq!(cron_runs.len(), 1);

    let one_shot_job = state
        .storage
        .get_scheduled_job(one_shot.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(one_shot_job.status.to_string(), "completed");
    assert!(one_shot_job.next_run_at.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_runs_record_output_and_deliver_to_local_and_channel_targets() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("delivery.db");
    let state = test_state(
        &db_path,
        MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "scheduled reply".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let job = schedule::create_scheduled_job(
        &state,
        schedule::CreateScheduledJobRequest {
            session_id,
            message: "deliver me".into(),
            schedule: schedule::ScheduleSpecRequest::Interval {
                every: "every 5 seconds".into(),
            },
            routing: swage_gateway::channel_gateway::DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: vec![swage_gateway::channel_gateway::DeliveryTarget::Channel {
                    integration:
                        swage_gateway::channel_gateway::ExternalChannelIntegration::DevThread,
                    account: "workspace-a".into(),
                    conversation: "room-1/topic-9".into(),
                }],
                store_local_output: true,
            },
        },
    )
    .await
    .unwrap();

    schedule::tick_once(&state, job.next_run_at.unwrap())
        .await
        .unwrap();

    let runs = state
        .storage
        .list_scheduled_runs_by_job(job.id)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].output_text.as_deref(), Some("scheduled reply"));

    let (_, delivery_log) = get_json(
        &router,
        &format!("/api/deliveries/log?session_id={session_id}"),
    )
    .await;
    let deliveries = delivery_log["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0]["target"]["kind"], "channel");
    assert_eq!(deliveries[1]["target"]["kind"], "local_storage");
    assert!(
        deliveries
            .iter()
            .all(|delivery| delivery["content"] == "scheduled reply")
    );
}
