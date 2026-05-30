#![allow(clippy::unwrap_used, clippy::disallowed_methods)]

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    store::{BenchStore, Storage},
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "lens-catalog-test"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("lens catalog tests do not call the completion API")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("lens catalog tests do not call the tool completion API")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> std::pin::Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(vec![StreamChunk::Done {
            finish_reason: None,
        }]))
    }
}

fn test_state(db_path: &Path) -> AppState {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
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
    (response.status(), json_response(response).await)
}

#[tokio::test(flavor = "multi_thread")]
async fn authored_and_installed_lenses_survive_restart_and_shape_runtime_data() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("lens-catalog.db");

    let router = build_router(test_state(&db_path));
    let (benches_status, benches_body) = get_json(&router, "/api/benches").await;
    assert_eq!(benches_status, StatusCode::OK);
    let bench_id = benches_body["benches"][0]["id"]
        .as_str()
        .expect("default bench id");

    let (before_status, before_caps) = get_json(&router, "/api/capabilities").await;
    assert_eq!(before_status, StatusCode::OK);

    let (create_status, create_body) = post_json(
        &router,
        "/api/lenses",
        json!({
            "id": "focus",
            "title": "Focus lens",
            "summary": "Operator-heavy lens for review work.",
            "default_surface": "operator",
            "ui_modules": ["operator", "conversation"],
            "prominent_team_ids": ["reviewer", "qa"]
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    assert_eq!(create_body["id"], "focus");
    assert_eq!(create_body["source_kind"], "authored");

    let (update_status, update_body) = put_json(
        &router,
        "/api/lenses/focus",
        json!({
            "title": "Focus lens",
            "summary": "Operator-first lens for review and audit work.",
            "default_surface": "operator",
            "ui_modules": ["operator", "conversation"],
            "prominent_team_ids": ["reviewer", "auditor"]
        }),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK);
    assert_eq!(
        update_body["prominent_team_ids"],
        json!(["reviewer", "auditor"])
    );

    let (install_status, install_body) = post_json(
        &router,
        "/api/lenses/install",
        json!({ "preset_id": "research" }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);
    assert_eq!(install_body["id"], "research");
    assert_eq!(install_body["source_kind"], "installed");

    let (list_status, list_body) = get_json(&router, "/api/lenses").await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_body["lenses"].as_array().unwrap().len(), 4);

    let (activate_status, runtime_body) = put_json(
        &router,
        &format!("/api/benches/{bench_id}/lens-runtime"),
        json!({ "lens": "focus" }),
    )
    .await;
    assert_eq!(activate_status, StatusCode::OK);
    assert_eq!(runtime_body["active_lens"], "focus");
    let focus = runtime_body["available_lenses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lens| lens["id"] == "focus")
        .expect("focus lens should be listed");
    assert_eq!(focus["default_surface"], "operator");
    assert_eq!(focus["ui_modules"], json!(["operator", "conversation"]));
    assert_eq!(focus["prominent_team_ids"], json!(["reviewer", "auditor"]));

    let (after_status, after_caps) = get_json(&router, "/api/capabilities").await;
    assert_eq!(after_status, StatusCode::OK);
    assert_eq!(before_caps, after_caps);

    let restarted_router = build_router(test_state(&db_path));
    let (restart_status, restart_runtime) = get_json(
        &restarted_router,
        &format!("/api/benches/{bench_id}/lens-runtime"),
    )
    .await;
    assert_eq!(restart_status, StatusCode::OK);
    assert_eq!(restart_runtime["active_lens"], "focus");
    assert!(
        restart_runtime["available_lenses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lens| lens["id"] == "research"),
        "installed research lens should survive restart"
    );
    assert!(
        restart_runtime["available_lenses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lens| lens["id"] == "focus"),
        "authored focus lens should survive restart"
    );
}
