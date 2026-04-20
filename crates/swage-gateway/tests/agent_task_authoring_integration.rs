use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    store::{ProjectStore, Storage},
};

static SWAGE_HOME_LOCK: Mutex<()> = Mutex::new(());

struct MockStreamingProvider {
    responses: Mutex<VecDeque<Vec<StreamChunk>>>,
}

impl MockStreamingProvider {
    fn new(responses: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
        }
    }
}

#[async_trait]
impl LlmProvider for MockStreamingProvider {
    fn model_id(&self) -> &str {
        "agent-task-authoring-test"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("agent/task authoring integration uses streaming only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("agent/task authoring integration does not use tool completion")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> std::pin::Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let response = self
            .responses
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
            .expect("mock streaming response");
        Box::pin(futures::stream::iter(response))
    }
}

fn test_state(db_path: &Path, provider: Arc<dyn LlmProvider>) -> AppState {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn ProjectStore>,
        provider,
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
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            Value::String(String::from_utf8(bytes.to_vec()).expect("utf-8 response"))
        })
    }
}

async fn request_json(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let response = router
        .clone()
        .oneshot(
            builder
                .body(match body {
                    Some(body) => Body::from(body.to_string()),
                    None => Body::empty(),
                })
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

#[tokio::test(flavor = "multi_thread")]
async fn team_authoring_persists_agents_and_tasks_and_drives_topology_runs() {
    let _guard = SWAGE_HOME_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let swage_home = temp.path().join("swage-home");
    let workspace = temp.path().join("workspace");
    let db_path = temp.path().join("agent-task-authoring.db");
    std::fs::create_dir_all(&swage_home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", &swage_home) };

    let provider: Arc<dyn LlmProvider> = Arc::new(MockStreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "{\"result\":\"approved\"}".into(),
        },
        StreamChunk::Usage(swage_core::TokenUsage {
            input_tokens: 14,
            output_tokens: 4,
        }),
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let router = build_router(test_state(&db_path, provider));

    let (project_status, project_body) = request_json(
        &router,
        "POST",
        "/api/projects",
        Some(json!({
            "name": "Authoring Project",
            "workspace_path": workspace.to_string_lossy()
        })),
    )
    .await;
    assert_eq!(project_status, StatusCode::CREATED);
    let project_id = project_body["id"].as_str().unwrap();

    let (team_status, team_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/teams"),
        Some(json!({
            "name": "delivery-core",
            "description": "Owns delivery work.",
            "entry_agent_id": "default",
            "agents": [
                {
                    "id": "default",
                    "display_name": "Default",
                    "description": "Primary delivery agent.",
                    "instructions": "Own the delivery workflow.",
                    "model": "claude-sonnet-4-20250514"
                },
                {
                    "id": "reviewer",
                    "display_name": "Reviewer",
                    "description": "Reviews delivery work.",
                    "instructions": "Review precisely.",
                    "model": "claude-haiku-4-20250514"
                }
            ],
            "tasks": [
                {
                    "name": "review-plan",
                    "description": "Review the current plan.",
                    "instructions": "Return a concise approval decision.",
                    "model": "claude-haiku-4-20250514",
                    "output_schema": {
                        "type": "object",
                        "properties": {
                            "result": { "type": "string" }
                        },
                        "required": ["result"]
                    }
                }
            ]
        })),
    )
    .await;
    assert_eq!(team_status, StatusCode::CREATED);
    let team_id = team_body["id"].as_str().unwrap();
    assert_eq!(team_body["agents"].as_array().unwrap().len(), 2);
    assert_eq!(team_body["tasks"].as_array().unwrap().len(), 1);

    let (team_update_status, team_update_body) = request_json(
        &router,
        "PUT",
        &format!("/api/teams/{team_id}"),
        Some(json!({
            "description": "Owns the durable delivery workflow.",
            "agents": [
                {
                    "id": "default",
                    "display_name": "Default",
                    "description": "Primary delivery agent.",
                    "instructions": "Own the durable delivery workflow.",
                    "model": "claude-sonnet-4-20250514"
                },
                {
                    "id": "reviewer",
                    "display_name": "Reviewer",
                    "description": "Reviews delivery work.",
                    "instructions": "Review precisely and call out edge cases.",
                    "model": "claude-haiku-4-20250514"
                }
            ],
            "tasks": [
                {
                    "name": "review-plan",
                    "description": "Review the current plan.",
                    "instructions": "Return a concise approval decision with one sentence of rationale.",
                    "model": "claude-haiku-4-20250514",
                    "output_schema": {
                        "type": "object",
                        "properties": {
                            "result": { "type": "string" }
                        },
                        "required": ["result"]
                    }
                },
                {
                    "name": "qa-check",
                    "description": "Check delivery regressions.",
                    "instructions": "Call out any regression risk you spot.",
                    "model": null,
                    "output_schema": {
                        "type": "object",
                        "properties": {
                            "status": { "type": "string" }
                        }
                    }
                }
            ]
        })),
    )
    .await;
    assert_eq!(team_update_status, StatusCode::OK);
    assert_eq!(team_update_body["agents"].as_array().unwrap().len(), 2);
    assert_eq!(team_update_body["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(
        team_update_body["agents"][1]["instructions"],
        "Review precisely and call out edge cases."
    );

    let restarted_router = build_router(test_state(
        &db_path,
        Arc::new(MockStreamingProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "{\"result\":\"approved\"}".into(),
            },
            StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: 14,
                output_tokens: 4,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ]])),
    ));
    let (restarted_status, restarted_team_body) = request_json(
        &restarted_router,
        "GET",
        &format!("/api/teams/{team_id}"),
        None,
    )
    .await;
    assert_eq!(restarted_status, StatusCode::OK);
    assert_eq!(restarted_team_body["agents"].as_array().unwrap().len(), 2);
    assert_eq!(restarted_team_body["tasks"].as_array().unwrap().len(), 2);

    let topology_source = r#"
import swage_topology as topo

def build_topology():
    return topo.graph(
        "review",
        [
            topo.llm_node("review", "reviewer", "review-plan"),
        ],
        [],
        max_concurrency=1,
        max_total_tokens=100,
    )
"#;

    let (run_status, run_body) = request_json(
        &restarted_router,
        "POST",
        &format!("/api/teams/{team_id}/runs"),
        Some(json!({
            "topology_source": topology_source,
            "message": "Review the release plan."
        })),
    )
    .await;
    assert_eq!(run_status, StatusCode::CREATED);
    assert_eq!(run_body["run"]["state"], "completed");
    assert_eq!(run_body["run"]["final_output"]["result"], "approved");
    let nodes = run_body["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["agent_id"], "reviewer");
    assert_eq!(nodes[0]["task_name"], "review-plan");
}
