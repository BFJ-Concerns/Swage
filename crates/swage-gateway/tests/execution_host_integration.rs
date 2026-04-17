use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
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
    execution_host::{
        ExecutionHostKind, RegisterExecutionHostRequest, register_attached_node_with_gateway,
        register_local_execution_host, spawn_attached_node, spawn_transport_backed_host,
    },
    secret::CreateSecretRequest,
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::{MANAGED_SECRET_KEY_ENV, SqliteStorage};
use swage_tool::context::{MockToolContext, ToolContext};
use swage_tool::tool::Tool;

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
        "execution-host-test"
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

struct HostIdentityTool {
    marker: &'static str,
}

#[async_trait]
impl Tool for HostIdentityTool {
    fn name(&self) -> &str {
        "host_identity"
    }

    fn description(&self) -> &str {
        "Returns the execution-host marker."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        Ok(json!({ "host": self.marker }))
    }
}

static SECRET_ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_secret_env() -> MutexGuard<'static, ()> {
    let guard = SECRET_ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(MANAGED_SECRET_KEY_ENV, BASE64.encode([15_u8; 32]));
    }
    guard
}

async fn start_test_server(
    provider: impl LlmProvider + 'static,
    dispatcher: swage_gateway::tool::ToolDispatcher,
    workspace_root: PathBuf,
    register_local_host: bool,
) -> (SocketAddr, AppState) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let mut config = swage_config::ResolvedConfig::default();
    config.workspace.path = workspace_root.to_string_lossy().into_owned();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::ProjectStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
        None,
    );
    if register_local_host {
        register_local_execution_host(&state).await;
    }
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
    workspace_root: PathBuf,
    restore_hosts: bool,
) -> (SocketAddr, AppState) {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    let mut config = swage_config::ResolvedConfig::default();
    config.workspace.path = workspace_root.to_string_lossy().into_owned();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::ProjectStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
        None,
    );
    register_local_execution_host(&state).await;
    if restore_hosts {
        swage_gateway::execution_host::restore_registered_execution_hosts(&state)
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
            material_kind: "node_credential".into(),
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
    sink.send(tungstenite::Message::Text(payload.to_string()))
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
    provider_tx1: tokio::sync::mpsc::Sender<StreamChunk>,
    provider_tx2: tokio::sync::mpsc::Sender<StreamChunk>,
) -> serde_json::Value {
    let client = GatewayClient::new(&format!("http://{addr}"));
    let conversation = client.create_conversation().await.unwrap();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conversation.id.to_string()).await;

    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Identify your execution host"}),
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
            call_id: "call_host".into(),
            name: "host_identity".into(),
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
        .find(|tool_call| tool_call.call_id == "call_host")
        .expect("host_identity tool call should persist");
    tool_call.result.clone().expect("tool result should exist")
}

#[tokio::test(flavor = "multi_thread")]
async fn same_machine_gateway_executes_turns_through_local_execution_host() {
    let _guard = with_secret_env();
    let workspace = tempfile::TempDir::new().unwrap();
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "local-host",
        }) as Arc<dyn Tool>]);
    let (addr, _state) =
        start_test_server(provider, dispatcher, workspace.path().to_path_buf(), true).await;

    let hosts: serde_json::Value = reqwest::get(format!("http://{addr}/api/execution-hosts"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hosts["hosts"][0]["host_id"], "local");

    let result = run_turn_and_collect_tool_result(addr, tx1, tx2).await;
    assert_eq!(result["host"], "local-host");
}

#[tokio::test(flavor = "multi_thread")]
async fn node_only_execution_host_registers_with_remote_gateway_and_executes_work() {
    let _guard = with_secret_env();
    let node_workspace = tempfile::TempDir::new().unwrap();
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let gateway_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (addr, state) = start_test_server(
        provider,
        gateway_dispatcher,
        node_workspace.path().to_path_buf(),
        false,
    )
    .await;
    let shared_token_secret_ref =
        create_secret_ref(&state, "Remote node token", "node-secret").await;

    let node_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "remote-node",
        }) as Arc<dyn Tool>]);
    let node = spawn_attached_node(
        node_dispatcher,
        node_workspace.path().to_path_buf(),
        "node-secret".to_string(),
    )
    .await;
    let registration = RegisterExecutionHostRequest {
        host_id: "remote-node-a".to_string(),
        label: Some("Remote Node A".to_string()),
        kind: "attached_node".to_string(),
        base_url: node.base_url.clone(),
        shared_token_secret_ref,
        workspace_root: node_workspace.path().to_string_lossy().into_owned(),
        capabilities: vec!["host_identity".to_string()],
    };
    register_attached_node_with_gateway(&format!("http://{addr}"), &registration)
        .await
        .unwrap();

    let result = run_turn_and_collect_tool_result(addr, tx1, tx2).await;
    assert_eq!(
        result["host"], "remote-node",
        "remote tool result: {result:?}"
    );

    node.join_handle.abort();
}

async fn run_transport_backend_round_trip(
    backend_kind: ExecutionHostKind,
    backend_label: &str,
    marker: &'static str,
) {
    let _guard = with_secret_env();
    let workspace = tempfile::TempDir::new().unwrap();
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let gateway_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (addr, state) = start_test_server(
        provider,
        gateway_dispatcher,
        workspace.path().to_path_buf(),
        false,
    )
    .await;
    let shared_token_secret_ref =
        create_secret_ref(&state, &format!("{backend_label} token"), "backend-secret").await;

    let backend_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![
            Arc::new(HostIdentityTool { marker }) as Arc<dyn Tool>
        ]);
    let backend = spawn_transport_backed_host(
        backend_kind.clone(),
        backend_dispatcher,
        workspace.path().to_path_buf(),
        "backend-secret".to_string(),
    )
    .await;
    let registration = RegisterExecutionHostRequest {
        host_id: format!("{backend_label}-host"),
        label: Some(format!("{backend_label} Host")),
        kind: backend_kind.to_string(),
        base_url: backend.base_url.clone(),
        shared_token_secret_ref,
        workspace_root: workspace.path().to_string_lossy().into_owned(),
        capabilities: vec!["host_identity".to_string()],
    };
    register_attached_node_with_gateway(&format!("http://{addr}"), &registration)
        .await
        .unwrap();

    let result = run_turn_and_collect_tool_result(addr, tx1, tx2).await;
    assert_eq!(result["host"], marker, "{backend_label} result: {result:?}");

    backend.join_handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_execution_backend_runs_through_shared_execution_interface() {
    run_transport_backend_round_trip(ExecutionHostKind::Docker, "docker", "docker-host").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_execution_backend_runs_through_shared_execution_interface() {
    run_transport_backend_round_trip(ExecutionHostKind::Ssh, "ssh", "ssh-host").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn registered_remote_execution_host_survives_restart() {
    let _guard = with_secret_env();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("execution-hosts.db");
    let workspace = tempfile::TempDir::new().unwrap();

    let (_tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (_tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let gateway_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (addr, state) = start_file_backed_test_server(
        &db_path,
        provider,
        gateway_dispatcher,
        workspace.path().to_path_buf(),
        false,
    )
    .await;
    let shared_token_secret_ref =
        create_secret_ref(&state, "Persisted backend token", "backend-secret").await;

    let backend_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "persisted-host",
        }) as Arc<dyn Tool>]);
    let backend = spawn_transport_backed_host(
        ExecutionHostKind::Docker,
        backend_dispatcher,
        workspace.path().to_path_buf(),
        "backend-secret".to_string(),
    )
    .await;
    register_attached_node_with_gateway(
        &format!("http://{addr}"),
        &RegisterExecutionHostRequest {
            host_id: "docker-persisted".to_string(),
            label: Some("Docker Persisted".to_string()),
            kind: "docker".to_string(),
            base_url: backend.base_url.clone(),
            shared_token_secret_ref,
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            capabilities: vec!["host_identity".to_string()],
        },
    )
    .await
    .unwrap();

    let restarted_provider = ChannelProvider::new(vec![]);
    let restarted_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (_restarted_addr, restarted_state) = start_file_backed_test_server(
        &db_path,
        restarted_provider,
        restarted_dispatcher,
        workspace.path().to_path_buf(),
        true,
    )
    .await;

    let hosts = swage_gateway::execution_host::list_execution_hosts(&restarted_state).await;
    assert!(
        hosts
            .hosts
            .iter()
            .any(|host| host.host_id == "docker-persisted"),
        "persisted remote host should be restored: {:?}",
        hosts.hosts
    );

    let host = swage_gateway::execution_host::resolve_execution_host(
        &restarted_state,
        Some(&json!({"execution_backend": "docker"})),
    )
    .await
    .expect("persisted host should resolve after restart");
    let mut ctx = MockToolContext::new("agent-test", "sess-test");
    ctx.working_directory = workspace.path().to_path_buf();
    let result = swage_gateway::execution_host::execute_on_remote_host(
        &restarted_state.secret_manager,
        &host,
        "host_identity",
        &json!({}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(result["host"], "persisted-host");

    backend.join_handle.abort();
}
