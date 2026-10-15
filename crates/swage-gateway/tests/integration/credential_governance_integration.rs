#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::store::{Storage, TopicStore};
use swage_storage::{MANAGED_SECRET_KEY_ENV, SqliteStorage};

use crate::support::ProcessEnvGuard;

fn with_secret_env() -> ProcessEnvGuard {
    let mut guard = ProcessEnvGuard::new();
    guard.set_managed_secret_key(11);
    guard
}

fn without_secret_env() -> ProcessEnvGuard {
    let mut guard = ProcessEnvGuard::new();
    guard.remove_managed_secret_key();
    guard
}

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "credential-governance"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("provider calls are not used in credential governance tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("provider calls are not used in credential governance tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> std::pin::Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::empty())
    }
}

fn test_state() -> AppState {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    AppState::for_test(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    )
}

async fn json_response(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_json(
    router: &Router,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, json_response(response).await)
}

async fn get_json(router: &Router, path: &str) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, json_response(response).await)
}

#[tokio::test(flavor = "multi_thread")]
async fn secret_management_surface_covers_types_rotation_validation_and_audit() {
    let mut env_guard = with_secret_env();
    env_guard.set_var("SWAGE_TEST_PROVIDER_KEY", "env-provider-key");

    let state = test_state();
    let router = build_router(state.clone());

    let (provider_status, provider_body) = post_json(
        &router,
        "/api/secrets",
        serde_json::json!({
            "name": "Provider key",
            "material_kind": "provider_key",
            "owner_kind": "service",
            "backend_kind": "env",
            "scope_kind": "gateway",
            "external_reference": "SWAGE_TEST_PROVIDER_KEY"
        }),
    )
    .await;
    assert_eq!(provider_status, StatusCode::CREATED);
    let provider_id = provider_body["id"].as_str().unwrap().to_string();

    let (channel_status, channel_body) = post_json(
        &router,
        "/api/secrets",
        serde_json::json!({
            "name": "Channel token",
            "material_kind": "channel_token",
            "owner_kind": "service",
            "backend_kind": "managed",
            "scope_kind": "channel",
            "scope_id": "dev_dm:ops",
            "value": "channel-secret"
        }),
    )
    .await;
    assert_eq!(channel_status, StatusCode::CREATED);
    let channel_id = channel_body["id"].as_str().unwrap().to_string();

    let (plugin_status, plugin_body) = post_json(
        &router,
        "/api/secrets/generate",
        serde_json::json!({
            "name": "Plugin secret",
            "material_kind": "plugin_secret",
            "owner_kind": "service",
            "scope_kind": "plugin",
            "scope_id": "demo-plugin",
            "length": 24
        }),
    )
    .await;
    assert_eq!(plugin_status, StatusCode::CREATED);
    assert!(plugin_body["revealed_value"].as_str().unwrap().len() >= 16);

    let (totp_status, totp_body) = post_json(
        &router,
        "/api/secrets/generate",
        serde_json::json!({
            "name": "TOTP secret",
            "material_kind": "totp_secret",
            "owner_kind": "user",
            "scope_kind": "account",
            "scope_id": "alice"
        }),
    )
    .await;
    assert_eq!(totp_status, StatusCode::CREATED);
    let totp_id = totp_body["id"].as_str().unwrap().to_string();

    let (ssh_status, ssh_body) = post_json(
        &router,
        "/api/secrets",
        serde_json::json!({
            "name": "SSH deploy key",
            "material_kind": "ssh_key",
            "owner_kind": "service",
            "backend_kind": "managed",
            "scope_kind": "device",
            "scope_id": "docker-a",
            "value": "-----BEGIN OPENSSH PRIVATE KEY-----\nTEST\n-----END OPENSSH PRIVATE KEY-----"
        }),
    )
    .await;
    assert_eq!(ssh_status, StatusCode::CREATED);
    let ssh_id = ssh_body["id"].as_str().unwrap().to_string();

    let (validate_provider_status, validate_provider_body) = post_json(
        &router,
        &format!("/api/secrets/{provider_id}/validate"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(validate_provider_status, StatusCode::OK);
    assert_eq!(validate_provider_body["valid"], true);

    for id in [&channel_id, &totp_id, &ssh_id] {
        let (status, body) = post_json(
            &router,
            &format!("/api/secrets/{id}/validate"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["valid"], true, "validation failed for {id}: {body:?}");
    }

    let (rotate_status, rotate_body) = post_json(
        &router,
        &format!("/api/secrets/{channel_id}/rotate"),
        serde_json::json!({"value": "channel-secret-rotated"}),
    )
    .await;
    assert_eq!(rotate_status, StatusCode::OK);
    assert_eq!(rotate_body["revealed_value"], "channel-secret-rotated");

    let (revoke_status, revoke_body) = post_json(
        &router,
        &format!("/api/secrets/{channel_id}/revoke"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(revoke_status, StatusCode::OK);
    assert_eq!(revoke_body["status"], "revoked");

    let (validate_revoked_status, validate_revoked_body) = post_json(
        &router,
        &format!("/api/secrets/{channel_id}/validate"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(validate_revoked_status, StatusCode::OK);
    assert_eq!(validate_revoked_body["valid"], false);

    let (restore_status, restore_body) = post_json(
        &router,
        &format!("/api/secrets/{channel_id}/restore"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(restore_status, StatusCode::OK);
    assert_eq!(restore_body["status"], "active");

    let (list_status, list_body) = get_json(&router, "/api/secrets").await;
    assert_eq!(list_status, StatusCode::OK);
    assert!(
        list_body["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|secret| secret["material_kind"] == "plugin_secret")
    );
    assert!(
        list_body.to_string().contains("provider_key")
            && !list_body.to_string().contains("channel-secret-rotated"),
        "list response must not reveal raw secret values: {list_body:?}"
    );
    assert_eq!(list_body["managed_backend"]["status"], "ready");
    assert!(
        list_body["managed_backend"]["detail"]
            .as_str()
            .unwrap()
            .contains("encrypted at rest")
    );

    let (audit_status, audit_body) =
        get_json(&router, "/api/operator/search?kind=audit_event&q=secret").await;
    assert_eq!(audit_status, StatusCode::OK);
    let results = audit_body["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .any(|result| result["title"].as_str().unwrap().contains("secret created")),
        "expected secret creation audit event: {audit_body:?}"
    );
    assert!(
        results
            .iter()
            .any(|result| result["title"].as_str().unwrap().contains("secret rotated")),
        "expected secret rotation audit event: {audit_body:?}"
    );
    assert!(
        results
            .iter()
            .any(|result| result["title"].as_str().unwrap().contains("secret revoked")),
        "expected secret revocation audit event: {audit_body:?}"
    );
    assert!(
        !audit_body.to_string().contains("channel-secret")
            && !audit_body.to_string().contains("env-provider-key"),
        "audit surface must not reveal raw secret values: {audit_body:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn secret_list_reports_missing_managed_encryption_key() {
    let _guard = without_secret_env();
    let state = test_state();
    let router = build_router(state);

    let (status, body) = get_json(&router, "/api/secrets").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["managed_backend"]["status"], "missing");
    assert!(
        body["managed_backend"]["detail"]
            .as_str()
            .unwrap()
            .contains(MANAGED_SECRET_KEY_ENV)
    );
}
