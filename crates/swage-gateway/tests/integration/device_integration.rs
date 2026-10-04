#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite;

use swage_client::http::GatewayClient;
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse, ToolError,
};
use swage_device::{DeviceConnection, DeviceError, RegisterDeviceRequest};
use swage_gateway::{
    device::DeviceKind, secret::CreateSecretRequest, server::build_router, state::AppState,
    tool::shell::ShellTool,
};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;
use swage_tool::context::{MockToolContext, ToolContext};
use swage_tool::tool::Tool;
use tokio_util::sync::CancellationToken;

use crate::support::{ProcessEnvGuard, start_live_server};

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
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
    );
    let router: Router = build_router(state.clone());
    let (addr, _server) = start_live_server(router).await;
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

/// Start a device process in this test: it dials `addr`, and its run loop
/// redials whenever the connection drops until the returned token fires.
fn dial_device(
    addr: SocketAddr,
    device_id: &str,
    shared_token: &str,
    tools: Vec<Arc<dyn Tool>>,
    workspace: &std::path::Path,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), DeviceError>>,
) {
    let connection = Arc::new(DeviceConnection {
        gateway_url: format!("http://{addr}"),
        device_id: device_id.to_string(),
        shared_token: shared_token.to_string(),
        label: Some(format!("{device_id} label")),
        dispatcher: Arc::new(swage_gateway::tool::ToolDispatcher::allow_all(tools)),
        workspace_roots: vec![workspace.to_path_buf()],
        reconnect_delay: Duration::from_millis(100),
    });
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(connection.run(shutdown.clone()));
    (shutdown, task)
}

/// Wait until the gateway's presence reads the device in `expected` state.
async fn wait_for_device_state(addr: SocketAddr, device_id: &str, expected: &str) {
    let client = GatewayClient::new(&format!("http://{addr}"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let presence = client.presence().await.unwrap();
        let state = presence
            .devices
            .iter()
            .find(|device| device.device_id == device_id)
            .map(|device| device.state.clone());
        if state.as_deref() == Some(expected) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "device {device_id} never read {expected}; presence: {:?}",
            presence.devices
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn identity_tools(marker: &'static str) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(DeviceIdentityTool { marker }) as Arc<dyn Tool>]
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
    // Attaching a workspace needs a topic to attach it to, so the device
    // turns run in one; the general scope has no workspace attachments.
    let topic = match client.create_topic("device-tests").await {
        Ok(topic) => topic,
        Err(_) => client
            .list_topics()
            .await
            .unwrap()
            .topics
            .into_iter()
            .find(|topic| topic.name == "device-tests")
            .expect("device-tests topic exists"),
    };
    let conversation = client.create_topic_conversation(topic.id).await.unwrap();
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
                "http://{addr}/api/topics/{}/workspaces",
                conversation
                    .topic_id
                    .expect("a topic-scoped conversation projects its topic")
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
async fn a_paired_device_dials_in_advertises_and_serves_operations_over_its_connection() {
    let _guard = with_secret_env();
    let device_workspace = tempfile::TempDir::new().unwrap();
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let (tx3, rx3) = tokio::sync::mpsc::channel(10);
    let (tx4, rx4) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2, rx3, rx4]);
    let gateway_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(identity_tools("gateway-unused"));
    let (addr, state) = start_test_server(provider, gateway_dispatcher).await;
    let shared_token_secret_ref =
        create_secret_ref(&state, "Remote device token", "device-secret").await;

    // Registration names the credential and the workspace, nothing about
    // what the device can run — that is advertised when it connects.
    let registered = DeviceConnection::register(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: "remote-device-a".to_string(),
            label: Some("Remote Device A".to_string()),
            kind: "attached_device".to_string(),
            shared_token_secret_ref,
            workspace_root: device_workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();
    assert!(registered.capabilities.is_empty());

    let (device_shutdown, device_task) = dial_device(
        addr,
        "remote-device-a",
        "device-secret",
        identity_tools("remote-device"),
        device_workspace.path(),
    );
    wait_for_device_state(addr, "remote-device-a", "connected").await;

    let devices: serde_json::Value = reqwest::get(format!("http://{addr}/api/devices"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        devices["devices"][0]["capabilities"],
        json!(["device_identity"])
    );
    assert!(devices["devices"][0].get("base_url").is_none());

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

    // The gateway's own record shows the connection and the operation,
    // invoked and settled under one correlation id.
    let mut events = swage_storage::store::AuditStore::list_audit_events(state.storage.as_ref())
        .await
        .unwrap();
    events.sort_by_key(|event| event.created_at);
    let device_events = events
        .iter()
        .filter(|event| event.entity_kind == "device" && event.entity_id == "remote-device-a")
        .collect::<Vec<_>>();
    let kinds = device_events
        .iter()
        .map(|event| event.event_kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "device_connected",
            "device_operation_invoked",
            "device_operation_settled"
        ],
        "{device_events:?}"
    );
    assert_eq!(
        device_events[1].metadata["correlation_id"],
        device_events[2].metadata["correlation_id"]
    );
    assert_eq!(device_events[2].metadata["outcome"], "result");

    let local_result = run_turn_and_collect_tool_result(addr, &state, tx3, tx4, None, None).await;
    assert_eq!(
        local_result["device"], "gateway-unused",
        "an unannotated turn must execute locally even with a connected device: {local_result:?}"
    );

    device_shutdown.cancel();
    device_task.await.unwrap().unwrap();
    wait_for_device_state(addr, "remote-device-a", "disconnected").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_device_shell_operation_stops_its_child_process() {
    let _guard = with_secret_env();
    let workspace = tempfile::tempdir().unwrap();
    let provider = ChannelProvider::new(vec![]);
    let gateway_dispatcher = swage_gateway::tool::ToolDispatcher::allow_all(vec![]);
    let (addr, state) = start_test_server(provider, gateway_dispatcher).await;
    let shared_token_secret_ref =
        create_secret_ref(&state, "Shell device token", "shell-secret").await;
    DeviceConnection::register(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: "shell-device".to_string(),
            label: Some("Shell device".to_string()),
            kind: "attached_device".to_string(),
            shared_token_secret_ref,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();
    let (device_shutdown, device_task) = dial_device(
        addr,
        "shell-device",
        "shell-secret",
        vec![Arc::new(ShellTool {
            timeout: Duration::from_secs(10),
            max_output_bytes: 1024,
        })],
        workspace.path(),
    );
    wait_for_device_state(addr, "shell-device", "connected").await;
    let device = swage_gateway::device::resolve_device(
        &state,
        Some(&json!({ "device_id": "shell-device" })),
    )
    .await
    .expect("connected device");
    let mut context = MockToolContext::new("agent-test", "session-test");
    context.working_directory = workspace.path().to_path_buf();
    let command = json!({ "command": "sh -c 'sleep 2; touch child_finished' & touch ready; wait" });
    let ready = workspace.path().join("ready");
    let child_marker = workspace.path().join("child_finished");

    // The same command must reach its child-owned side effect without Stop.
    swage_gateway::device::execute_on_remote_device(&state, &device, "shell", &command, &context)
        .await
        .expect("control command completes");
    assert!(child_marker.exists(), "control command did not finish");
    std::fs::remove_file(&ready).unwrap();
    std::fs::remove_file(&child_marker).unwrap();

    let invocation = tokio::spawn({
        let state = state.clone();
        let device = device.clone();
        async move {
            swage_gateway::device::execute_on_remote_device(
                &state, &device, "shell", &command, &context,
            )
            .await
        }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !ready.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "device shell never started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    invocation.abort();
    let _ = invocation.await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let events = swage_storage::store::AuditStore::list_audit_events(state.storage.as_ref())
            .await
            .unwrap();
        if events.iter().any(|event| {
            event.event_kind == "device_operation_settled"
                && event.metadata["outcome"] == "cancelled"
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "gateway did not settle cancellation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(
        !child_marker.exists(),
        "the device shell's child continued after cancellation"
    );
    device_shutdown.cancel();
    device_task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_with_the_wrong_token_is_rejected_and_never_listed_as_connected() {
    let _guard = with_secret_env();
    let workspace = tempfile::TempDir::new().unwrap();
    let provider = ChannelProvider::new(vec![]);
    let (addr, state) = start_test_server(
        provider,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
    )
    .await;
    let shared_token_secret_ref = create_secret_ref(&state, "Right token", "right").await;
    DeviceConnection::register(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: "impostor".to_string(),
            label: None,
            kind: "attached_device".to_string(),
            shared_token_secret_ref,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();

    let (_shutdown, task) = dial_device(addr, "impostor", "wrong", vec![], workspace.path());
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("a rejected device stops dialling")
        .unwrap();
    assert!(
        matches!(outcome, Err(DeviceError::Rejected(message)) if message == "invalid shared token")
    );
    let presence = GatewayClient::new(&format!("http://{addr}"))
        .presence()
        .await
        .unwrap();
    assert!(
        presence
            .devices
            .iter()
            .all(|device| device.device_id != "impostor")
    );
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
        swage_gateway::tool::ToolDispatcher::allow_all(identity_tools("gateway-unused"));
    let (addr, state) = start_test_server(provider, gateway_dispatcher).await;
    let shared_token_secret_ref =
        create_secret_ref(&state, &format!("{backend_label} token"), "backend-secret").await;
    let device_id = format!("{backend_label}-device");

    DeviceConnection::register(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: device_id.clone(),
            label: Some(format!("{backend_label} Device")),
            kind: backend_kind.to_string(),
            shared_token_secret_ref,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();
    let (device_shutdown, _task) = dial_device(
        addr,
        &device_id,
        "backend-secret",
        identity_tools(marker),
        workspace.path(),
    );
    wait_for_device_state(addr, &device_id, "connected").await;

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
    device_shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_execution_backend_runs_through_shared_execution_interface() {
    run_transport_backend_round_trip(DeviceKind::Docker, "docker", "docker-device").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_execution_backend_runs_through_shared_execution_interface() {
    run_transport_backend_round_trip(DeviceKind::Ssh, "ssh", "ssh-device").await;
}

/// A gateway whose listener address is chosen by the test and which the
/// test can bring down, so a restarted gateway can take the same address.
async fn start_restartable_server(
    router: Router,
    addr: Option<SocketAddr>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener =
        tokio::net::TcpListener::bind(addr.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registered_device_reconnects_to_a_restarted_gateway_and_serves_it() {
    let _guard = with_secret_env();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("devices.db");
    let workspace = tempfile::TempDir::new().unwrap();

    // First life of the gateway: register, dial in, connected.
    let storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(ChannelProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(identity_tools("gateway-unused")),
        None,
    );
    let (addr, server) = start_restartable_server(build_router(state.clone()), None).await;
    let shared_token_secret_ref =
        create_secret_ref(&state, "Persisted backend token", "backend-secret").await;
    DeviceConnection::register(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: "docker-persisted".to_string(),
            label: Some("Docker Persisted".to_string()),
            kind: "docker".to_string(),
            shared_token_secret_ref,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();
    let (device_shutdown, _device_task) = dial_device(
        addr,
        "docker-persisted",
        "backend-secret",
        identity_tools("persisted-device"),
        workspace.path(),
    );
    wait_for_device_state(addr, "docker-persisted", "connected").await;

    // The gateway goes down: its links close, its listener is gone.
    state.shutdown_signal.cancel();
    server.abort();
    drop(state);
    drop(storage);

    // Second life on the same address, from the same store: the device's
    // redial finds it, advertises again, and serves it.
    let storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let restarted = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(ChannelProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(identity_tools("gateway-unused")),
        None,
    );
    swage_gateway::device::restore_registered_devices(&restarted)
        .await
        .unwrap();
    let (restarted_addr, _server) =
        start_restartable_server(build_router(restarted.clone()), Some(addr)).await;
    assert_eq!(restarted_addr, addr);
    wait_for_device_state(addr, "docker-persisted", "connected").await;

    let devices = swage_gateway::device::list_devices(&restarted).await;
    let device = devices
        .devices
        .iter()
        .find(|device| device.device_id == "docker-persisted")
        .expect("persisted remote device should be restored");
    assert_eq!(device.capabilities, vec!["device_identity"]);

    let resolved = swage_gateway::device::resolve_device(
        &restarted,
        Some(&json!({"execution_backend": "docker"})),
    )
    .await
    .expect("persisted device should resolve after restart");
    let mut ctx = MockToolContext::new("agent-test", "sess-test");
    ctx.working_directory = workspace.path().to_path_buf();
    let result = swage_gateway::device::execute_on_remote_device(
        &restarted,
        &resolved,
        "device_identity",
        &json!({}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(result["device"], "persisted-device");

    device_shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_device_executes_own_paths_and_refuses_gateway_read_only_targets() {
    use swage_gateway::tool::{file_edit::FileEditTool, file_write::FileWriteTool};
    use swage_gateway::tool_executor::GatewayExecutionStrategy;
    use swage_storage::{NewSession, SessionOrigin};
    use swage_tool::dispatch::ToolExecutionStrategy;
    let home = tempfile::tempdir().unwrap();
    let mut guard = with_secret_env();
    guard.set_swage_home(home.path());
    let workspace = tempfile::tempdir().unwrap();
    let protected = workspace.path().join("protected");
    std::fs::create_dir(&protected).unwrap();
    std::fs::write(protected.join("existing.txt"), "protected-original").unwrap();
    let (addr, state) = start_test_server(
        ChannelProvider::new(vec![]),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
    )
    .await;
    let credential = create_secret_ref(&state, "Native file device", "native-test-token").await;
    DeviceConnection::register(
        &format!("http://{addr}"),
        &RegisterDeviceRequest {
            device_id: "native-file-device".into(),
            label: None,
            kind: "attached_device".into(),
            shared_token_secret_ref: credential,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();
    // This is the same native tool registry and no-strategy dispatcher that
    // `swage device` uses, reached over an actual DeviceConnection.
    let (shutdown, device_task) = dial_device(
        addr,
        "native-file-device",
        "native-test-token",
        swage_gateway::startup::workspace_tools(&swage_config::ResolvedConfig::default()),
        workspace.path(),
    );
    wait_for_device_state(addr, "native-file-device", "connected").await;
    let topic = state.topic_store.general_scope().await.unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/topics/{}/workspaces", topic.id))
        .json(&json!({"path": protected, "permissions": ["read"], "is_trusted": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let session = state
        .storage
        .create_session(NewSession {
            topic_id: topic.id,
            agent_id: "default".into(),
            origin: SessionOrigin::Direct,
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .unwrap();
    let mut context = MockToolContext::new("default", session.id.to_string());
    context.working_directory = workspace.path().to_path_buf();
    let device = state
        .devices
        .lock()
        .await
        .devices
        .get("native-file-device")
        .unwrap()
        .clone();
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let strategy = GatewayExecutionStrategy::new(
        None,
        tx,
        Some(device),
        state.secret_manager.clone(),
        state.storage.clone(),
        Some(state.clone()),
    );
    let write = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": "own.txt", "content": "device-written"}),
            &context,
        )
        .await;
    let edit = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
        {"path": "own.txt", "edits": [{"type": "replace", "old_text": "device-written", "new_text": "device-edited"}]}
    ]}), &context).await;
    let denied_write = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": "protected/probe.txt", "content": "bad"}),
            &context,
        )
        .await;
    let denied_edit = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
        {"path": "protected/existing.txt", "edits": [{"type": "replace", "old_text": "protected-original", "new_text": "bad"}]}
    ]}), &context).await;
    // Clean up the real counterpart even when an assertion below fails.
    shutdown.cancel();
    device_task.await.unwrap().unwrap();
    assert_eq!(
        write.expect("device-owned file_write must succeed")["output"],
        "Wrote 14 bytes to own.txt"
    );
    assert_eq!(
        edit.expect("device-owned file_edit must succeed")["output"],
        "Applied 1 edit(s) to 1 file(s)"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("own.txt")).unwrap(),
        "device-edited"
    );
    assert!(
        denied_write
            .unwrap_err()
            .to_string()
            .contains("read-only access")
    );
    assert!(
        denied_edit
            .unwrap_err()
            .to_string()
            .contains("read-only access")
    );
    assert!(!protected.join("probe.txt").exists());
    assert_eq!(
        std::fs::read_to_string(protected.join("existing.txt")).unwrap(),
        "protected-original"
    );
}
