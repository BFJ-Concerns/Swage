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

use crate::support::{ProcessEnvGuard, start_live_server};
use swage_device::{DeviceConnection, RegisterDeviceRequest};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_client::http::GatewayClient;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{SqliteStorage, store::TopicStore};

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
        Arc::clone(&storage) as Arc<dyn TopicStore>,
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

/// A gateway whose state the test keeps, so it can register a device.
async fn start_device_test_server() -> (SocketAddr, AppState) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (addr, _server) = start_live_server(build_router(state.clone())).await;
    (addr, state)
}

async fn device_state(client: &GatewayClient, device_id: &str) -> Option<String> {
    client
        .presence()
        .await
        .unwrap()
        .devices
        .into_iter()
        .find(|device| device.device_id == device_id)
        .map(|device| device.state)
}

async fn wait_for_device_state(client: &GatewayClient, device_id: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while device_state(client, device_id).await.as_deref() != Some(expected) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "device {device_id} never read {expected}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_reads_a_device_from_its_dialled_connection_and_nothing_else() {
    let mut env = ProcessEnvGuard::new();
    env.set_managed_secret_key(15);
    let (addr, state) = start_device_test_server().await;
    let http = reqwest::Client::new();
    let client = GatewayClient::new(&format!("http://{addr}"));
    let workspace = tempfile::TempDir::new().unwrap();

    let secret_ref = state
        .secret_manager
        .create_secret(swage_gateway::secret::CreateSecretRequest {
            name: "Device A token".into(),
            material_kind: "device_credential".into(),
            owner_kind: "service".into(),
            backend_kind: "managed".into(),
            scope_kind: "gateway".into(),
            scope_id: None,
            metadata: None,
            value: Some("device-a-token".into()),
            external_reference: None,
        })
        .await
        .unwrap()
        .reference;
    DeviceConnection::register(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: "device-a".into(),
            label: Some("Device A".into()),
            kind: "attached_device".into(),
            shared_token_secret_ref: secret_ref,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();

    // Registered but not dialled in: no presence record yet.
    assert_eq!(device_state(&client, "device-a").await, None);

    // There is no HTTP heartbeat: a device is present through its
    // connection or not at all.
    let no_http_heartbeat = http
        .post(format!("http://{addr}/api/presence/devices/heartbeat"))
        .json(&serde_json::json!({ "device_id": "device-a" }))
        .send()
        .await
        .unwrap();
    assert!(
        no_http_heartbeat.status().is_client_error(),
        "an HTTP heartbeat is refused: {}",
        no_http_heartbeat.status()
    );
    assert_eq!(device_state(&client, "device-a").await, None);

    let connection = Arc::new(DeviceConnection {
        gateway_url: format!("http://{addr}"),
        device_id: "device-a".into(),
        shared_token: "device-a-token".into(),
        label: Some("Device A".into()),
        dispatcher: Arc::new(swage_gateway::tool::ToolDispatcher::allow_all(vec![])),
        workspace_roots: vec![workspace.path().to_path_buf()],
        reconnect_delay: Duration::from_millis(100),
    });
    let shutdown = CancellationToken::new();
    let device_task = tokio::spawn(Arc::clone(&connection).run(shutdown.clone()));
    wait_for_device_state(&client, "device-a", "connected").await;

    // The operator can cut the connection from the gateway's side.
    let disconnected = http
        .delete(format!("http://{addr}/api/presence/devices/device-a"))
        .send()
        .await
        .unwrap();
    assert_eq!(disconnected.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        device_state(&client, "device-a").await.as_deref(),
        Some("disconnected")
    );

    // The device's redial loop brings it back on its own.
    wait_for_device_state(&client, "device-a", "connected").await;

    // And the device going away reads as disconnected.
    shutdown.cancel();
    device_task.await.unwrap().unwrap();
    wait_for_device_state(&client, "device-a", "disconnected").await;

    let unknown = http
        .delete(format!("http://{addr}/api/presence/devices/nobody"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}
