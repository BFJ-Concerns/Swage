#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;

use swage_client::http::GatewayClient;
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse, ToolError,
};
use swage_gateway::{
    device::{
        DeviceKind, RegisterDeviceRequest, register_attached_device_with_gateway,
        spawn_attached_device, spawn_transport_backed_device,
    },
    secret::CreateSecretRequest,
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;
use swage_tool::context::{MockToolContext, ToolContext};
use swage_tool::tool::Tool;

use crate::support::ProcessEnvGuard;

struct ChannelProvider {
    receivers:
        std::sync::Mutex<std::collections::VecDeque<tokio::sync::mpsc::Receiver<StreamChunk>>>,
}

impl ChannelProvider {
    fn new(receivers: Vec<tokio::sync::mpsc::Receiver<StreamChunk>>) -> Self {
        Self {
            receivers: std::sync::Mutex::new(receivers.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for ChannelProvider {
    fn model_id(&self) -> &str {
        "device-test"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("streaming only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("streaming only")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let rx = self
            .receivers
            .lock()
            .unwrap()
            .pop_front()
            .expect("missing configured provider round");
        Box::pin(futures::stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|chunk| (chunk, rx))
        }))
    }
}

struct DeviceIdentityTool {
    marker: &'static str,
}

#[async_trait]
impl Tool for DeviceIdentityTool {
    fn name(&self) -> &str {
        "device_identity"
    }

    fn description(&self) -> &str {
        "Returns the device marker."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        Ok(json!({ "device": self.marker }))
    }
}

fn with_secret_env() -> ProcessEnvGuard {
    let mut guard = ProcessEnvGuard::new();
    guard.set_managed_secret_key(15);
    guard
}

async fn start_test_server(
    provider: impl LlmProvider + 'static,
    dispatcher: swage_gateway::tool::ToolDispatcher,
) -> (SocketAddr, AppState) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let config = swage_config::ResolvedConfig::default();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
    );
    let router: Router = build_router(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, state)
}

async fn start_file_backed_test_server(
    db_path: &std::path::Path,
    provider: impl LlmProvider + 'static,
    dispatcher: swage_gateway::tool::ToolDispatcher,
    restore_devices: bool,
) -> (SocketAddr, AppState) {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    let config = swage_config::ResolvedConfig::default();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
    );
    if restore_devices {
        swage_gateway::device::restore_registered_devices(&state)
            .await
            .unwrap();
    }
    let router: Router = build_router(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, state)
}

async fn create_secret_ref(state: &AppState, name: &str, value: &str) -> String {
    state
        .secret_manager
        .create_secret(CreateSecretRequest {
            name: name.to_string(),
            material_kind: "device_credential".into(),
            owner_kind: "service".into(),
            backend_kind: "managed".into(),
            scope_kind: "gateway".into(),
            scope_id: None,
            metadata: None,
            value: Some(value.to_string()),
            external_reference: None,
        })
        .await
        .unwrap()
        .reference
}

type WsSink = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Message,
>;
type WsStream = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn connect_ws(addr: SocketAddr, conversation_id: &str) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/api/conversations/{conversation_id}/ws");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WebSocket connection should succeed");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, payload: Value) {
    sink.send(tungstenite::Message::Text(payload.to_string().into()))
        .await
        .expect("WebSocket send should succeed");
}

async fn ws_recv(stream: &mut WsStream) -> Value {
    loop {
        match stream.next().await {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return serde_json::from_str(&text).expect("event JSON should parse");
            }
            Some(Ok(tungstenite::Message::Close(_))) => panic!("WebSocket closed unexpectedly"),
            Some(Err(error)) => panic!("WebSocket error: {error}"),
            None => panic!("WebSocket stream ended unexpectedly"),
            _ => continue,
        }
    }
}

async fn run_turn_and_collect_tool_result(
    addr: SocketAddr,
    state: &AppState,
    provider_tx1: tokio::sync::mpsc::Sender<StreamChunk>,
    provider_tx2: tokio::sync::mpsc::Sender<StreamChunk>,
    workspace_root: Option<&std::path::Path>,
    session_metadata: Option<Value>,
) -> serde_json::Value {
    let client = GatewayClient::new(&format!("http://{addr}"));
    let conversation = client.create_conversation().await.unwrap();
    if let Some(metadata) = session_metadata {
        state
            .storage
            .set_session_metadata(conversation.id, Some(metadata))
            .await
            .unwrap();
    }
    if let Some(workspace_root) = workspace_root {
        let response = reqwest::Client::new()
            .post(format!(
                "http://{addr}/api/spaces/{}/workspaces",
                conversation.space_id
            ))
            .json(&json!({
                "path": workspace_root,
                "label": "device test workspace",
                "active_session_id": conversation.id,
                "permissions": ["read", "write"],
                "is_trusted": true
            }))
            .send()
            .await
            .expect("workspace attachment request should succeed");
        assert!(
            response.status().is_success(),
            "workspace attachment failed with status {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conversation.id.to_string()).await;

    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Identify your device"}),
    )
    .await;

    provider_tx2
        .send(StreamChunk::Delta {
            content: "done".into(),
        })
        .await
        .unwrap();
    provider_tx2
        .send(StreamChunk::Done {
            finish_reason: None,
        })
        .await
        .unwrap();

    provider_tx1
        .send(StreamChunk::ToolUse {
            call_id: "call_device".into(),
            name: "device_identity".into(),
            arguments: json!({}),
        })
        .await
        .unwrap();
    provider_tx1
        .send(StreamChunk::Done {
            finish_reason: Some(FinishReason::ToolUse),
        })
        .await
        .unwrap();

    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), ws_recv(&mut ws_read))
            .await
            .expect("timed out waiting for turn completion");
        if event["type"] == "turn_complete" {
            break;
        }
    }

    let conversation = client.get_conversation(conversation.id).await.unwrap();
    let assistant = conversation
        .messages
        .iter()
        .find(|message| message.role == "assistant")
        .expect("assistant message should exist");
    let tool_call = assistant
        .tool_calls
        .iter()
        .find(|tool_call| tool_call.call_id == "call_device")
        .expect("device_identity tool call should persist");
    tool_call.result.clone().expect("tool result should exist")
}

#[tokio::test(flavor = "multi_thread")]
async fn same_machine_gateway_executes_turns_without_a_registered_local_device() {
    let _guard = with_secret_env();
    let workspace = tempfile::TempDir::new().unwrap();
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "local-device",
        }) as Arc<dyn Tool>]);
    let (addr, state) = start_test_server(provider, dispatcher).await;

    let devices: serde_json::Value = reqwest::get(format!("http://{addr}/api/devices"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices["devices"], json!([]));

    let result =
        run_turn_and_collect_tool_result(addr, &state, tx1, tx2, Some(workspace.path()), None)
            .await;
    assert_eq!(result["device"], "local-device");
}

#[tokio::test(flavor = "multi_thread")]
async fn attached_device_registers_with_remote_gateway_and_executes_work() {
    let _guard = with_secret_env();
    let device_workspace = tempfile::TempDir::new().unwrap();
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let (tx3, rx3) = tokio::sync::mpsc::channel(10);
    let (tx4, rx4) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2, rx3, rx4]);
    let gateway_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (addr, state) = start_test_server(provider, gateway_dispatcher).await;
    let shared_token_secret_ref =
        create_secret_ref(&state, "Remote device token", "device-secret").await;

    let device_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "remote-device",
        }) as Arc<dyn Tool>]);
    let device = spawn_attached_device(
        device_dispatcher,
        device_workspace.path().to_path_buf(),
        "device-secret".to_string(),
    )
    .await;
    let registration = RegisterDeviceRequest {
        device_id: "remote-device-a".to_string(),
        label: Some("Remote Device A".to_string()),
        kind: "attached_device".to_string(),
        base_url: device.base_url.clone(),
        shared_token_secret_ref,
        workspace_root: device_workspace.path().to_string_lossy().into_owned(),
        workspace_roots: vec![],
        capabilities: vec!["device_identity".to_string()],
    };
    register_attached_device_with_gateway(&format!("http://{addr}"), &registration)
        .await
        .unwrap();

    let result = run_turn_and_collect_tool_result(
        addr,
        &state,
        tx1,
        tx2,
        Some(device_workspace.path()),
        Some(json!({"device_id": "remote-device-a"})),
    )
    .await;
    assert_eq!(
        result["device"], "remote-device",
        "remote tool result: {result:?}"
    );

    let local_result = run_turn_and_collect_tool_result(addr, &state, tx3, tx4, None, None).await;
    assert_eq!(
        local_result["device"], "gateway-unused",
        "an unannotated turn must execute locally even with an attached device: {local_result:?}"
    );

    device.join_handle.abort();
}

async fn run_transport_backend_round_trip(
    backend_kind: DeviceKind,
    backend_label: &str,
    marker: &'static str,
) {
    let _guard = with_secret_env();
    let workspace = tempfile::TempDir::new().unwrap();
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let gateway_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (addr, state) = start_test_server(provider, gateway_dispatcher).await;
    let shared_token_secret_ref =
        create_secret_ref(&state, &format!("{backend_label} token"), "backend-secret").await;

    let backend_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![
            Arc::new(DeviceIdentityTool { marker }) as Arc<dyn Tool>,
        ]);
    let backend = spawn_transport_backed_device(
        backend_kind.clone(),
        backend_dispatcher,
        workspace.path().to_path_buf(),
        "backend-secret".to_string(),
    )
    .await;
    let registration = RegisterDeviceRequest {
        device_id: format!("{backend_label}-device"),
        label: Some(format!("{backend_label} Device")),
        kind: backend_kind.to_string(),
        base_url: backend.base_url.clone(),
        shared_token_secret_ref,
        workspace_root: workspace.path().to_string_lossy().into_owned(),
        workspace_roots: vec![],
        capabilities: vec!["device_identity".to_string()],
    };
    register_attached_device_with_gateway(&format!("http://{addr}"), &registration)
        .await
        .unwrap();

    let result = run_turn_and_collect_tool_result(
        addr,
        &state,
        tx1,
        tx2,
        Some(workspace.path()),
        Some(json!({"execution_backend": backend_kind.to_string()})),
    )
    .await;
    assert_eq!(
        result["device"], marker,
        "{backend_label} result: {result:?}"
    );

    backend.join_handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_execution_backend_runs_through_shared_execution_interface() {
    run_transport_backend_round_trip(DeviceKind::Docker, "docker", "docker-device").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_execution_backend_runs_through_shared_execution_interface() {
    run_transport_backend_round_trip(DeviceKind::Ssh, "ssh", "ssh-device").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn registered_remote_device_survives_restart() {
    let _guard = with_secret_env();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("devices.db");
    let workspace = tempfile::TempDir::new().unwrap();

    let (_tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (_tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let gateway_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (addr, state) =
        start_file_backed_test_server(&db_path, provider, gateway_dispatcher, false).await;
    let shared_token_secret_ref =
        create_secret_ref(&state, "Persisted backend token", "backend-secret").await;

    let backend_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "persisted-device",
        }) as Arc<dyn Tool>]);
    let backend = spawn_transport_backed_device(
        DeviceKind::Docker,
        backend_dispatcher,
        workspace.path().to_path_buf(),
        "backend-secret".to_string(),
    )
    .await;
    register_attached_device_with_gateway(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: "docker-persisted".to_string(),
            label: Some("Docker Persisted".to_string()),
            kind: "docker".to_string(),
            base_url: backend.base_url.clone(),
            shared_token_secret_ref,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
            capabilities: vec!["device_identity".to_string()],
        },
    )
    .await
    .unwrap();

    let restarted_provider = ChannelProvider::new(vec![]);
    let restarted_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (_restarted_addr, restarted_state) =
        start_file_backed_test_server(&db_path, restarted_provider, restarted_dispatcher, true)
            .await;

    let devices = swage_gateway::device::list_devices(&restarted_state).await;
    assert!(
        devices
            .devices
            .iter()
            .any(|device| device.device_id == "docker-persisted"),
        "persisted remote device should be restored: {:?}",
        devices.devices
    );

    let device = swage_gateway::device::resolve_device(
        &restarted_state,
        Some(&json!({"execution_backend": "docker"})),
    )
    .await
    .expect("persisted device should resolve after restart");
    let mut ctx = MockToolContext::new("agent-test", "sess-test");
    ctx.working_directory = workspace.path().to_path_buf();
    let result = swage_gateway::device::execute_on_remote_device(
        &restarted_state.secret_manager,
        &device,
        "device_identity",
        &json!({}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(result["device"], "persisted-device");

    backend.join_handle.abort();
}
