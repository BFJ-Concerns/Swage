#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse, ToolError,
};
use swage_gateway::{
    execution_host::{register_local_execution_host, spawn_attached_node},
    secret::CreateSecretRequest,
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    store::{BenchStore, Storage},
};
use swage_tool::context::MockToolContext;
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
        "onboarding-recovery-test"
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

    async fn execute(
        &self,
        _params: Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, ToolError> {
        Ok(json!({ "host": self.marker }))
    }
}

fn with_secret_env() -> ProcessEnvGuard {
    let mut guard = ProcessEnvGuard::new();
    guard.set_managed_secret_key(13);
    guard
}

async fn start_test_server(
    db_path: Option<&Path>,
    provider: impl LlmProvider + 'static,
    dispatcher: swage_gateway::tool::ToolDispatcher,
    workspace_root: PathBuf,
    register_local_host: bool,
) -> (SocketAddr, AppState) {
    let storage = Arc::new(match db_path {
        Some(path) => SqliteStorage::open(path).unwrap(),
        None => SqliteStorage::in_memory().unwrap(),
    });
    let mut config = swage_config::ResolvedConfig::default();
    config.workspace.path = workspace_root.to_string_lossy().into_owned();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn BenchStore>,
        Arc::new(provider),
        config,
        dispatcher,
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

async fn post_json(addr: SocketAddr, path: &str, body: Value) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

async fn get_json(addr: SocketAddr, path: &str) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json::<Value>().await.unwrap();
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_exports_are_listed_and_downloadable() {
    let workspace = tempfile::TempDir::new().unwrap();
    let provider = ChannelProvider::new(vec![]);
    let dispatcher = swage_gateway::tool::ToolDispatcher::allow_all(vec![]);
    let (addr, _state) = start_test_server(
        None,
        provider,
        dispatcher,
        workspace.path().to_path_buf(),
        false,
    )
    .await;

    let (empty_status, empty_body) = get_json(addr, "/api/recovery/exports").await;
    assert_eq!(empty_status, StatusCode::OK);
    assert_eq!(empty_body["exports"].as_array().unwrap().len(), 0);

    let (export_status, export_body) = get_json(addr, "/api/recovery/export").await;
    assert_eq!(export_status, StatusCode::OK);

    let (list_status, list_body) = get_json(addr, "/api/recovery/exports").await;
    assert_eq!(list_status, StatusCode::OK);
    let exports = list_body["exports"].as_array().unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(
        exports[0]["benches"],
        json!(export_body["benches"].as_array().unwrap().len())
    );
    assert!(exports[0]["size_bytes"].as_u64().unwrap() > 0);

    let export_id = exports[0]["id"].as_str().unwrap();
    let (download_status, download_body) =
        get_json(addr, &format!("/api/recovery/exports/{export_id}")).await;
    assert_eq!(download_status, StatusCode::OK);
    assert_eq!(download_body, export_body);
}

#[tokio::test(flavor = "multi_thread")]
async fn channel_onboarding_pairs_node_and_delivers_first_message() {
    let _guard = with_secret_env();
    let workspace = tempfile::TempDir::new().unwrap();
    let (channel_tx, channel_rx) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![channel_rx]);
    let dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (addr, state) = start_test_server(
        None,
        provider,
        dispatcher,
        workspace.path().to_path_buf(),
        false,
    )
    .await;
    let shared_token_secret_ref = create_secret_ref(&state, "Pair token", "pair-secret").await;

    let node_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "paired-node",
        }) as Arc<dyn Tool>]);
    let node = spawn_attached_node(
        node_dispatcher,
        workspace.path().to_path_buf(),
        "pair-secret".to_string(),
    )
    .await;

    let (pair_status, pair_body) =
        post_json(addr, "/api/onboarding/nodes/pairing-codes", json!({})).await;
    assert_eq!(pair_status, StatusCode::CREATED);

    let (redeem_status, redeem_body) = post_json(
        addr,
        "/api/onboarding/nodes/redeem",
        json!({
            "pairing_code": pair_body["pairing_code"],
            "host_id": "paired-node-a",
            "label": "Paired Node A",
            "kind": "attached_node",
            "base_url": node.base_url,
            "shared_token_secret_ref": shared_token_secret_ref,
            "workspace_root": workspace.path().to_string_lossy(),
            "capabilities": ["host_identity"]
        }),
    )
    .await;
    assert_eq!(redeem_status, StatusCode::CREATED);
    assert_eq!(redeem_body["host_id"], "paired-node-a");

    let (link_status, link_body) = post_json(
        addr,
        "/api/onboarding/channels",
        json!({
            "integration": "dev_dm",
            "account": "workspace-a",
            "conversation": "alice"
        }),
    )
    .await;
    assert_eq!(link_status, StatusCode::CREATED);

    channel_tx
        .send(StreamChunk::Delta {
            content: "paired hello".into(),
        })
        .await
        .unwrap();
    channel_tx
        .send(StreamChunk::Done {
            finish_reason: None,
        })
        .await
        .unwrap();

    let (inbound_status, inbound_body) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "hello from linked channel"
        }),
    )
    .await;
    assert_eq!(inbound_status, StatusCode::ACCEPTED);
    assert_eq!(inbound_body["session_id"], link_body["session_id"]);
    assert_eq!(inbound_body["response"], "paired hello");

    let (hosts_status, hosts_body) = get_json(addr, "/api/execution-hosts").await;
    assert_eq!(hosts_status, StatusCode::OK);
    assert!(
        hosts_body["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|host| host["host_id"] == "paired-node-a"),
        "paired node should be present after redeeming a pairing code: {hosts_body:?}"
    );

    node.join_handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_bundle_exports_and_imports_operator_state_into_fresh_instance() {
    let _guard = with_secret_env();
    let gateway_workspace = tempfile::TempDir::new().unwrap();
    let node_workspace = tempfile::TempDir::new().unwrap();
    let origin_dir = tempfile::tempdir().unwrap();
    let destination_dir = tempfile::tempdir().unwrap();
    let origin_db = origin_dir.path().join("origin.db");
    let destination_db = destination_dir.path().join("destination.db");

    let origin_provider = ChannelProvider::new(vec![]);
    let origin_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (origin_addr, origin_state) = start_test_server(
        Some(&origin_db),
        origin_provider,
        origin_dispatcher,
        gateway_workspace.path().to_path_buf(),
        false,
    )
    .await;
    let recovery_secret_ref =
        create_secret_ref(&origin_state, "Recovery token", "recovery-secret").await;

    let node_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "recovery-node",
        }) as Arc<dyn Tool>]);
    let node = spawn_attached_node(
        node_dispatcher,
        node_workspace.path().to_path_buf(),
        "recovery-secret".to_string(),
    )
    .await;

    let (_, pair_body) = post_json(
        origin_addr,
        "/api/onboarding/nodes/pairing-codes",
        json!({}),
    )
    .await;
    let (redeem_status, _) = post_json(
        origin_addr,
        "/api/onboarding/nodes/redeem",
        json!({
            "pairing_code": pair_body["pairing_code"],
            "host_id": "recovery-node-a",
            "label": "Recovery Node A",
            "kind": "attached_node",
            "base_url": node.base_url,
            "shared_token_secret_ref": recovery_secret_ref,
            "workspace_root": node_workspace.path().to_string_lossy(),
            "capabilities": ["host_identity"]
        }),
    )
    .await;
    assert_eq!(redeem_status, StatusCode::CREATED);

    let (link_status, link_body) = post_json(
        origin_addr,
        "/api/onboarding/channels",
        json!({
            "integration": "dev_dm",
            "account": "workspace-a",
            "conversation": "alice",
            "home_target": { "kind": "local_storage" }
        }),
    )
    .await;
    assert_eq!(link_status, StatusCode::CREATED);
    let session_id = link_body["session_id"].as_str().unwrap();

    let (schedule_status, _) = post_json(
        origin_addr,
        "/api/schedules",
        json!({
            "message": "Nightly sweep",
            "spec": { "kind": "interval", "every": "every 10 minutes" },
            "session": { "policy": "continuation", "session_id": session_id },
            "routing": { "deliver_to_origin": false, "store_local_output": true }
        }),
    )
    .await;
    assert_eq!(schedule_status, StatusCode::CREATED);

    let (export_status, export_body) = get_json(origin_addr, "/api/recovery/export").await;
    assert_eq!(export_status, StatusCode::OK);

    let destination_provider = ChannelProvider::new(vec![]);
    let destination_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (destination_addr, _destination_state) = start_test_server(
        Some(&destination_db),
        destination_provider,
        destination_dispatcher,
        gateway_workspace.path().to_path_buf(),
        false,
    )
    .await;

    let (import_status, import_body) = post_json(
        destination_addr,
        "/api/recovery/import",
        json!({ "bundle": export_body }),
    )
    .await;
    assert_eq!(import_status, StatusCode::ACCEPTED, "{import_body:?}");

    let (_, links_body) = get_json(destination_addr, "/api/onboarding/channels").await;
    assert_eq!(links_body["links"][0]["integration"], "dev_dm");
    assert_eq!(links_body["links"][0]["account"], "workspace-a");

    let (_, hosts_body) = get_json(destination_addr, "/api/execution-hosts").await;
    assert!(
        hosts_body["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|host| host["host_id"] == "recovery-node-a"),
        "import should restore execution hosts: {hosts_body:?}"
    );

    let (_, schedules_body) = get_json(destination_addr, "/api/schedules").await;
    assert_eq!(schedules_body["schedules"][0]["message"], "Nightly sweep");
    assert_eq!(
        schedules_body["schedules"][0]["session"]["policy"],
        "continuation"
    );

    node.join_handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_bundle_restores_service_after_installation_move() {
    let _guard = with_secret_env();
    let gateway_workspace = tempfile::TempDir::new().unwrap();
    let node_workspace = tempfile::TempDir::new().unwrap();
    let origin_dir = tempfile::tempdir().unwrap();
    let destination_dir = tempfile::tempdir().unwrap();
    let origin_db = origin_dir.path().join("origin.db");
    let destination_db = destination_dir.path().join("destination.db");

    let origin_provider = ChannelProvider::new(vec![]);
    let origin_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (origin_addr, origin_state) = start_test_server(
        Some(&origin_db),
        origin_provider,
        origin_dispatcher,
        gateway_workspace.path().to_path_buf(),
        false,
    )
    .await;
    let moved_secret_ref = create_secret_ref(&origin_state, "Moved token", "moved-secret").await;

    let node_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "moved-node",
        }) as Arc<dyn Tool>]);
    let node = spawn_attached_node(
        node_dispatcher,
        node_workspace.path().to_path_buf(),
        "moved-secret".to_string(),
    )
    .await;

    let (_, pair_body) = post_json(
        origin_addr,
        "/api/onboarding/nodes/pairing-codes",
        json!({}),
    )
    .await;
    let _ = post_json(
        origin_addr,
        "/api/onboarding/nodes/redeem",
        json!({
            "pairing_code": pair_body["pairing_code"],
            "host_id": "moved-node-a",
            "label": "Moved Node A",
            "kind": "attached_node",
            "base_url": node.base_url,
            "shared_token_secret_ref": moved_secret_ref,
            "workspace_root": node_workspace.path().to_string_lossy(),
            "capabilities": ["host_identity"]
        }),
    )
    .await;
    let _ = post_json(
        origin_addr,
        "/api/onboarding/channels",
        json!({
            "integration": "dev_dm",
            "account": "workspace-a",
            "conversation": "alice"
        }),
    )
    .await;

    let (_, export_body) = get_json(origin_addr, "/api/recovery/export").await;

    let (channel_tx, channel_rx) = tokio::sync::mpsc::channel(10);
    let destination_provider = ChannelProvider::new(vec![channel_rx]);
    let destination_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(HostIdentityTool {
            marker: "gateway-unused",
        }) as Arc<dyn Tool>]);
    let (destination_addr, destination_state) = start_test_server(
        Some(&destination_db),
        destination_provider,
        destination_dispatcher,
        gateway_workspace.path().to_path_buf(),
        false,
    )
    .await;

    let (import_status, import_body) = post_json(
        destination_addr,
        "/api/recovery/import",
        json!({ "bundle": export_body }),
    )
    .await;
    assert_eq!(import_status, StatusCode::ACCEPTED, "{import_body:?}");

    channel_tx
        .send(StreamChunk::Delta {
            content: "restored channel reply".into(),
        })
        .await
        .unwrap();
    channel_tx
        .send(StreamChunk::Done {
            finish_reason: None,
        })
        .await
        .unwrap();

    let (inbound_status, inbound_body) = post_json(
        destination_addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "still there?"
        }),
    )
    .await;
    assert_eq!(inbound_status, StatusCode::ACCEPTED);
    assert_eq!(inbound_body["response"], "restored channel reply");

    let host = swage_gateway::execution_host::resolve_execution_host(
        &destination_state,
        Some(&json!({ "execution_host_id": "moved-node-a" })),
    )
    .await
    .expect("imported execution host should resolve after installation move");
    let mut ctx = MockToolContext::new("gateway", "recovery-session");
    ctx.working_directory = node_workspace.path().to_path_buf();
    let tool_result = swage_gateway::execution_host::execute_on_remote_host(
        &destination_state.secret_manager,
        &host,
        "host_identity",
        &json!({}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(tool_result["host"], "moved-node");

    node.join_handle.abort();
}
