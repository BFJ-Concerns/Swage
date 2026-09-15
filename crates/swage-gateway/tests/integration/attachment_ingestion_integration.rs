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
use swage_gateway::{
    attachment,
    capabilities::{ImageReferences, WorkflowCapabilities},
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::{
    AttachmentStatus, BackgroundJobState, SqliteStorage,
    store::{AttachmentStore, BackgroundJobStore, SessionStore, Storage, TopicStore},
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "attachment-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("attachment ingestion tests do not call the completion API")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("attachment ingestion tests do not call the tool completion API")
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

fn test_state(
    db_path: &Path,
    workflow_capabilities: WorkflowCapabilities,
) -> (AppState, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    let mut state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    state.workflow_capabilities = workflow_capabilities;
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

fn encode_bytes(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

#[tokio::test(flavor = "multi_thread")]
async fn session_and_topic_attachments_survive_restart_with_queryable_records() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("attachments.db");
    let capabilities = WorkflowCapabilities {
        text: true,
        images: Some(ImageReferences::AnyUrl),
        embeddings: false,
        image_generation: false,
    };
    let (state, storage) = test_state(&db_path, capabilities.clone());
    let router = build_router(state.clone());

    let session_id = create_conversation(&router).await;
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    let topic_id = session.topic_id;

    let (text_status, text_body) = post_json(
        &router,
        "/api/attachments",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "filename": "note.md",
            "mime_type": "text/markdown",
            "content_base64": encode_bytes(b"# Note\nHello from the session.")
        }),
    )
    .await;
    assert_eq!(text_status, StatusCode::CREATED);
    let session_text_attachment_id = text_body["attachment"]["id"].as_str().unwrap();
    assert_eq!(text_body["attachment"]["status"], "pending");
    assert_eq!(text_body["job"]["state"], "queued");

    let (image_status, image_body) = post_json(
        &router,
        "/api/attachments",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "filename": "scan.png",
            "mime_type": "image/png",
            "content_base64": encode_bytes(b"\x89PNG\r\n\x1a\n")
        }),
    )
    .await;
    assert_eq!(image_status, StatusCode::CREATED);
    let session_image_attachment_id = image_body["attachment"]["id"].as_str().unwrap();

    let (topic_status, topic_body) = post_json(
        &router,
        "/api/attachments",
        json!({
            "scope_kind": "topic",
            "topic_id": topic_id,
            "filename": "topic.txt",
            "mime_type": "text/plain",
            "content_base64": encode_bytes(b"Topic context")
        }),
    )
    .await;
    assert_eq!(topic_status, StatusCode::CREATED);
    let topic_attachment_id = topic_body["attachment"]["id"].as_str().unwrap();

    assert_eq!(attachment::tick_once(&state).await.unwrap(), 3);

    let (session_status, session_body) =
        get_json(&router, &format!("/api/sessions/{session_id}/attachments")).await;
    assert_eq!(session_status, StatusCode::OK);
    let session_attachments = session_body["attachments"].as_array().unwrap();
    assert_eq!(session_attachments.len(), 2);
    assert!(session_attachments.iter().any(|attachment| {
        attachment["id"] == session_text_attachment_id
            && attachment["kind"] == "document"
            && attachment["status"] == "ready"
            && attachment["ingested_markdown"] == "# Note\nHello from the session."
    }));
    assert!(session_attachments.iter().any(|attachment| {
        attachment["id"] == session_image_attachment_id
            && attachment["kind"] == "image"
            && attachment["status"] == "ready"
            && attachment["ingested_markdown"]
                == format!("![scan.png](attachment://{session_image_attachment_id})")
    }));

    let (topic_list_status, topic_list_body) =
        get_json(&router, &format!("/api/topics/{topic_id}/attachments")).await;
    assert_eq!(topic_list_status, StatusCode::OK);
    let topic_attachments = topic_list_body["attachments"].as_array().unwrap();
    assert_eq!(topic_attachments.len(), 1);
    assert_eq!(topic_attachments[0]["id"], topic_attachment_id);
    assert_eq!(topic_attachments[0]["status"], "ready");
    assert_eq!(topic_attachments[0]["ingested_markdown"], "Topic context");

    let (restarted_state, _restarted_storage) = test_state(&db_path, capabilities);
    let restarted_router = build_router(restarted_state.clone());

    let (restarted_session_status, restarted_session_body) = get_json(
        &restarted_router,
        &format!("/api/sessions/{session_id}/attachments"),
    )
    .await;
    assert_eq!(restarted_session_status, StatusCode::OK);
    let restarted_session_attachments = restarted_session_body["attachments"].as_array().unwrap();
    assert_eq!(restarted_session_attachments.len(), 2);
    assert!(restarted_session_attachments.iter().any(|attachment| {
        attachment["id"] == session_text_attachment_id
            && attachment["status"] == "ready"
            && attachment["ingested_markdown"] == "# Note\nHello from the session."
    }));
    assert!(restarted_session_attachments.iter().any(|attachment| {
        attachment["id"] == session_image_attachment_id
            && attachment["status"] == "ready"
            && attachment["ingested_markdown"]
                == format!("![scan.png](attachment://{session_image_attachment_id})")
    }));

    let (restarted_topic_status, restarted_topic_body) = get_json(
        &restarted_router,
        &format!("/api/topics/{topic_id}/attachments"),
    )
    .await;
    assert_eq!(restarted_topic_status, StatusCode::OK);
    let restarted_topic_attachments = restarted_topic_body["attachments"].as_array().unwrap();
    assert_eq!(restarted_topic_attachments.len(), 1);
    assert_eq!(restarted_topic_attachments[0]["id"], topic_attachment_id);
    assert_eq!(restarted_topic_attachments[0]["status"], "ready");
}

#[tokio::test(flavor = "multi_thread")]
async fn background_jobs_cover_queued_running_completed_failed_and_retried_states() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("attachment-jobs.db");
    let (state, storage) = test_state(
        &db_path,
        WorkflowCapabilities {
            text: true,
            images: Some(ImageReferences::AnyUrl),
            embeddings: false,
            image_generation: false,
        },
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let (invalid_status, invalid_body) = post_json(
        &router,
        "/api/attachments",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "filename": "broken.txt",
            "mime_type": "text/plain",
            "content_base64": encode_bytes(&[0xff])
        }),
    )
    .await;
    assert_eq!(invalid_status, StatusCode::CREATED);
    let invalid_attachment_id: Uuid = invalid_body["attachment"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let invalid_job_id: Uuid = invalid_body["job"]["id"].as_str().unwrap().parse().unwrap();

    let (queued_status, queued_body) =
        get_json(&router, &format!("/api/background-jobs/{invalid_job_id}")).await;
    assert_eq!(queued_status, StatusCode::OK);
    assert_eq!(queued_body["state"], "queued");

    let running = storage
        .mark_background_job_running(invalid_job_id)
        .await
        .unwrap();
    assert_eq!(running.state, BackgroundJobState::Running);

    let (running_status, running_body) =
        get_json(&router, &format!("/api/background-jobs/{invalid_job_id}")).await;
    assert_eq!(running_status, StatusCode::OK);
    assert_eq!(running_body["state"], "running");

    storage
        .update_attachment_ingestion(invalid_attachment_id, AttachmentStatus::Failed)
        .await
        .unwrap();
    let failed = storage
        .fail_background_job(invalid_job_id, "document attachment is not valid UTF-8")
        .await
        .unwrap();
    assert_eq!(failed.state, BackgroundJobState::Failed);

    let (failed_status, failed_body) =
        get_json(&router, &format!("/api/background-jobs/{invalid_job_id}")).await;
    assert_eq!(failed_status, StatusCode::OK);
    assert_eq!(failed_body["state"], "failed");
    assert!(
        failed_body["error_message"]
            .as_str()
            .unwrap()
            .contains("not valid UTF-8")
    );

    let (retried_status, retried_body) = post_json(
        &router,
        &format!("/api/background-jobs/{invalid_job_id}/retry"),
        json!({}),
    )
    .await;
    assert_eq!(retried_status, StatusCode::OK);
    assert_eq!(retried_body["state"], "retried");
    let retried_attachment = storage
        .get_attachment(invalid_attachment_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retried_attachment.status, AttachmentStatus::Pending);

    let (valid_status, valid_body) = post_json(
        &router,
        "/api/attachments",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "filename": "ok.txt",
            "mime_type": "text/plain",
            "content_base64": encode_bytes(b"ready for ingestion")
        }),
    )
    .await;
    assert_eq!(valid_status, StatusCode::CREATED);
    let valid_job_id: Uuid = valid_body["job"]["id"].as_str().unwrap().parse().unwrap();

    assert_eq!(attachment::tick_once(&state).await.unwrap(), 2);

    let (completed_status, completed_body) =
        get_json(&router, &format!("/api/background-jobs/{valid_job_id}")).await;
    assert_eq!(completed_status, StatusCode::OK);
    assert_eq!(completed_body["state"], "completed");
    assert_eq!(completed_body["result"]["status"], "ready");

    let (refailed_status, refailed_body) =
        get_json(&router, &format!("/api/background-jobs/{invalid_job_id}")).await;
    assert_eq!(refailed_status, StatusCode::OK);
    assert_eq!(refailed_body["state"], "failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_hide_and_reject_unsupported_attachment_workflows() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("attachment-capabilities.db");
    let (state, _storage) = test_state(
        &db_path,
        WorkflowCapabilities {
            text: true,
            images: None,
            embeddings: false,
            image_generation: false,
        },
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let (capability_status, capability_body) = get_json(&router, "/api/capabilities").await;
    assert_eq!(capability_status, StatusCode::OK);
    assert_eq!(capability_body["text"], true);
    assert_eq!(capability_body["vision"], false);
    assert_eq!(capability_body["embeddings"], false);
    assert_eq!(capability_body["image_generation"], false);
    assert_eq!(capability_body["tts"], false);
    assert_eq!(capability_body["transcription"], false);

    let (rejected_status, rejected_body) = post_json(
        &router,
        "/api/attachments",
        json!({
            "scope_kind": "session",
            "session_id": session_id,
            "filename": "vision.png",
            "mime_type": "image/png",
            "content_base64": encode_bytes(b"\x89PNG\r\n\x1a\n")
        }),
    )
    .await;
    assert_eq!(rejected_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(rejected_body["code"], "validation_error");
    assert!(
        rejected_body["message"]
            .as_str()
            .unwrap()
            .contains("vision ingestion is unavailable")
    );
}
