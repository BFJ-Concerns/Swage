#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use tokio::time::Duration;
use tower::ServiceExt;

use crate::support::start_live_server;
use uuid::Uuid;

use swage_client::http::GatewayClient;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{SqliteStorage, store::SpaceStore};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "presence-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("not used")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("not used")
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

async fn start_test_server() -> (Router, SocketAddr) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let router = build_router(state);

    let (addr, _server) = start_live_server(router.clone()).await;

    (router, addr)
}

async fn post_json(
    router: &Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
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
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn connect_ws(
    addr: SocketAddr,
    conversation_id: Uuid,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}/api/conversations/{conversation_id}/ws");
    tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws should connect")
        .0
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_snapshot_tracks_client_disconnect_and_reconnect() {
    let (router, addr) = start_test_server().await;
    let (status, body) = post_json(&router, "/api/conversations", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    let conversation_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();

    let client = GatewayClient::new(&format!("http://{addr}"));

    let mut ws = connect_ws(addr, conversation_id).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let connected = client.presence().await.unwrap();
    let record = connected
        .clients
        .iter()
        .find(|client| client.session_id == conversation_id)
        .expect("client presence record");
    assert_eq!(record.state, "connected");
    assert_eq!(connected.gateway.active_connection_count, 1);

    ws.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let disconnected = client.presence().await.unwrap();
    let record = disconnected
        .clients
        .iter()
        .find(|client| client.session_id == conversation_id)
        .expect("client presence record after disconnect");
    assert_eq!(record.state, "disconnected");
    assert_eq!(disconnected.gateway.active_connection_count, 0);

    let mut ws = connect_ws(addr, conversation_id).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let reconnected = client.presence().await.unwrap();
    let record = reconnected
        .clients
        .iter()
        .find(|client| client.session_id == conversation_id)
        .expect("client presence record after reconnect");
    assert_eq!(record.state, "connected");
    assert_eq!(reconnected.gateway.active_connection_count, 1);

    ws.close(None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_snapshot_marks_device_stale_after_missed_heartbeat() {
    let (_router, addr) = start_test_server().await;
    let http = reqwest::Client::new();
    let client = GatewayClient::new(&format!("http://{addr}"));

    let response = http
        .post(format!("http://{addr}/api/presence/devices/heartbeat"))
        .json(&serde_json::json!({
            "device_id": "device-a",
            "label": "Device A",
            "stale_after_secs": 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let obsolete_heartbeat = http
        .post(format!("http://{addr}/api/presence/machines/heartbeat"))
        .send()
        .await
        .unwrap();
    assert_eq!(obsolete_heartbeat.status(), StatusCode::NOT_FOUND);

    let connected = client.presence().await.unwrap();
    let device = connected
        .devices
        .iter()
        .find(|device| device.device_id == "device-a")
        .expect("device presence record");
    assert_eq!(device.state, "connected");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let stale = client.presence().await.unwrap();
    let device = stale
        .devices
        .iter()
        .find(|device| device.device_id == "device-a")
        .expect("stale device presence record");
    assert_eq!(device.state, "stale");

    let disconnected = http
        .delete(format!("http://{addr}/api/presence/devices/device-a"))
        .send()
        .await
        .unwrap();
    assert_eq!(disconnected.status(), StatusCode::NO_CONTENT);

    let obsolete_disconnect = http
        .delete(format!("http://{addr}/api/presence/machines/device-a"))
        .send()
        .await
        .unwrap();
    assert_eq!(obsolete_disconnect.status(), StatusCode::NOT_FOUND);
}
