#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{attachment, server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    store::{SessionStore, Storage, TopicStore},
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "material-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("material management tests do not call the completion API")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("material management tests do not call the tool completion API")
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

fn test_state(db_path: &Path) -> (AppState, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    let config = swage_config::ResolvedConfig::default();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(MockProvider),
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (state, storage)
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
    (response.status(), json_response(response).await)
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

async fn create_conversation(router: &Router) -> Uuid {
    let (status, body) = post_json(router, "/api/conversations", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn materials_can_be_created_read_archived_restored_and_promoted() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("materials.db");

    let (state, storage) = test_state(&db_path);
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    let topic_id = session.topic_id;

    let (create_status, create_body) = post_json(
        &router,
        "/api/materials",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "title": "Draft Notes",
            "relative_path": "notes/draft.md",
            "mime_type": "text/markdown",
            "content_text": "# Draft\nhello"
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED, "body: {create_body:?}");
    let material_id = create_body["id"].as_str().unwrap();
    let active_path = create_body["absolute_path"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(active_path).unwrap(),
        "# Draft\nhello"
    );

    let (get_status, get_body) = get_json(&router, &format!("/api/materials/{material_id}")).await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(get_body["content_text"], "# Draft\nhello");
    assert_eq!(get_body["status"], "active");

    let (archive_status, archive_body) = post_json(
        &router,
        &format!("/api/materials/{material_id}/archive"),
        json!({}),
    )
    .await;
    assert_eq!(archive_status, StatusCode::OK);
    assert_eq!(archive_body["status"], "archived");
    let archived_path = archive_body["absolute_path"].as_str().unwrap();
    assert!(!Path::new(active_path).exists());
    assert_eq!(
        std::fs::read_to_string(archived_path).unwrap(),
        "# Draft\nhello"
    );

    let (restore_status, restore_body) = post_json(
        &router,
        &format!("/api/materials/{material_id}/restore"),
        json!({}),
    )
    .await;
    assert_eq!(restore_status, StatusCode::OK);
    assert_eq!(restore_body["status"], "active");
    let restored_path = restore_body["absolute_path"].as_str().unwrap();
    assert_eq!(restored_path, active_path);
    assert_eq!(
        std::fs::read_to_string(restored_path).unwrap(),
        "# Draft\nhello"
    );

    let (promote_status, promote_body) = post_json(
        &router,
        &format!("/api/materials/{material_id}/promote"),
        json!({
            "scope_kind": "topic",
            "topic_id": topic_id,
            "relative_path": "shared/draft.md"
        }),
    )
    .await;
    assert_eq!(promote_status, StatusCode::OK);
    assert_eq!(promote_body["scope_kind"], "topic");
    assert!(promote_body["session_id"].is_null());
    assert_eq!(promote_body["topic_id"], topic_id.to_string());
    let promoted_path = promote_body["absolute_path"].as_str().unwrap();
    assert!(!Path::new(restored_path).exists());
    assert_eq!(
        std::fs::read_to_string(promoted_path).unwrap(),
        "# Draft\nhello"
    );

    let (session_list_status, session_list_body) =
        get_json(&router, &format!("/api/sessions/{session_id}/materials")).await;
    assert_eq!(session_list_status, StatusCode::OK);
    assert!(
        session_list_body["materials"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let (topic_list_status, topic_list_body) =
        get_json(&router, &format!("/api/topics/{topic_id}/materials")).await;
    assert_eq!(topic_list_status, StatusCode::OK);
    let topic_materials = topic_list_body["materials"].as_array().unwrap();
    assert_eq!(topic_materials.len(), 1);
    assert_eq!(topic_materials[0]["id"], material_id);
    assert_eq!(topic_materials[0]["relative_path"], "shared/draft.md");
}

#[tokio::test(flavor = "multi_thread")]
async fn material_paths_cannot_escape_the_managed_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("materials-permissions.db");

    let (state, _storage) = test_state(&db_path);
    let router = build_router(state);
    let session_id = create_conversation(&router).await;

    let (escape_status, escape_body) = post_json(
        &router,
        "/api/materials",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "title": "Escape",
            "relative_path": "../escape.md",
            "content_text": "nope"
        }),
    )
    .await;
    assert_eq!(escape_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(escape_body["code"], "validation_error");
    assert!(
        escape_body["message"]
            .as_str()
            .unwrap()
            .contains("outside the workspace")
    );

    let (absolute_status, absolute_body) = post_json(
        &router,
        "/api/materials",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "title": "Absolute",
            "relative_path": "/etc/passwd",
            "content_text": "nope"
        }),
    )
    .await;
    assert_eq!(absolute_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(absolute_body["code"], "validation_error");
}

#[tokio::test(flavor = "multi_thread")]
async fn attachment_ingested_and_manual_materials_share_one_surface() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("materials-attachment.db");

    let (state, storage) = test_state(&db_path);
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    let topic_id = session.topic_id;

    let (manual_status, manual_body) = post_json(
        &router,
        "/api/materials",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "title": "Builder Notes",
            "relative_path": "notes/builder.md",
            "content_text": "builder text"
        }),
    )
    .await;
    assert_eq!(manual_status, StatusCode::CREATED, "body: {manual_body:?}");
    let manual_id = manual_body["id"].as_str().unwrap().to_string();

    let (attachment_status, _attachment_body) = post_json(
        &router,
        "/api/attachments",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "filename": "brief.txt",
            "mime_type": "text/plain",
            "content_base64": BASE64.encode(b"attachment text")
        }),
    )
    .await;
    assert_eq!(attachment_status, StatusCode::CREATED);
    assert_eq!(attachment::tick_once(&state).await.unwrap(), 1);

    let (session_list_status, session_list_body) =
        get_json(&router, &format!("/api/sessions/{session_id}/materials")).await;
    assert_eq!(session_list_status, StatusCode::OK);
    let session_materials = session_list_body["materials"].as_array().unwrap();
    assert_eq!(session_materials.len(), 2);
    assert!(session_materials.iter().any(|material| {
        material["id"] == manual_id
            && material["source_kind"] == "manual"
            && material["content_text"] == "builder text"
    }));
    assert!(session_materials.iter().any(|material| {
        material["source_kind"] == "attachment_ingestion"
            && material["content_text"] == "attachment text"
            && material["relative_path"]
                .as_str()
                .unwrap()
                .starts_with("attachments/")
    }));

    let (topic_list_status, topic_list_body) =
        get_json(&router, &format!("/api/topics/{topic_id}/materials")).await;
    assert_eq!(topic_list_status, StatusCode::OK);
    assert!(topic_list_body["materials"].as_array().unwrap().is_empty());
}
