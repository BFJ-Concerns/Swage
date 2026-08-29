#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use reqwest::StatusCode;
use serde_json::{Value, json};

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse, ToolError,
};
use swage_gateway::{
    device::{register_local_device, spawn_attached_device},
    secret::CreateSecretRequest,
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    store::{SpaceStore, Storage},
};
use swage_tool::context::MockToolContext;
use swage_tool::tool::Tool;

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

    async fn execute(
        &self,
        _params: Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, ToolError> {
        Ok(json!({ "device": self.marker }))
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
    include_local_device: bool,
) -> (SocketAddr, AppState) {
    let storage = Arc::new(match db_path {
        Some(path) => SqliteStorage::open(path).unwrap(),
        None => SqliteStorage::in_memory().unwrap(),
    });
    let mut config = swage_config::ResolvedConfig::default();
    config.workspace.path = workspace_root.to_string_lossy().into_owned();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
    );
    if include_local_device {
        register_local_device(&state).await;
    }
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
        exports[0]["spaces"],
        json!(export_body["spaces"].as_array().unwrap().len())
    );
    assert!(exports[0]["size_bytes"].as_u64().unwrap() > 0);

    let export_id = exports[0]["id"].as_str().unwrap();
    let (download_status, download_body) =
        get_json(addr, &format!("/api/recovery/exports/{export_id}")).await;
    assert_eq!(download_status, StatusCode::OK);
    assert_eq!(download_body, export_body);
}

#[tokio::test(flavor = "multi_thread")]
async fn channel_onboarding_pairs_device_and_delivers_first_message() {
    let _guard = with_secret_env();
    let workspace = tempfile::TempDir::new().unwrap();
    let (channel_tx, channel_rx) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![channel_rx]);
    let dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
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

    let device_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "paired-device",
        }) as Arc<dyn Tool>]);
    let device = spawn_attached_device(
        device_dispatcher,
        workspace.path().to_path_buf(),
        "pair-secret".to_string(),
    )
    .await;

    let (pair_status, pair_body) =
        post_json(addr, "/api/onboarding/devices/pairing-codes", json!({})).await;
    assert_eq!(pair_status, StatusCode::CREATED);

    let (redeem_status, redeem_body) = post_json(
        addr,
        "/api/onboarding/devices/redeem",
        json!({
            "pairing_code": pair_body["pairing_code"],
            "device_id": "paired-device-a",
            "label": "Paired Device A",
            "kind": "attached_device",
            "base_url": device.base_url,
            "shared_token_secret_ref": shared_token_secret_ref,
            "workspace_root": workspace.path().to_string_lossy(),
            "capabilities": ["device_identity"]
        }),
    )
    .await;
    assert_eq!(redeem_status, StatusCode::CREATED);
    assert_eq!(redeem_body["device_id"], "paired-device-a");

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

    let (devices_status, devices_body) = get_json(addr, "/api/devices").await;
    assert_eq!(devices_status, StatusCode::OK);
    assert!(
        devices_body["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|device| device["device_id"] == "paired-device-a"),
        "paired device should be present after redeeming a pairing code: {devices_body:?}"
    );

    device.join_handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_bundle_exports_and_imports_operator_state_into_fresh_instance() {
    let _guard = with_secret_env();
    let gateway_workspace = tempfile::TempDir::new().unwrap();
    let device_workspace = tempfile::TempDir::new().unwrap();
    let origin_dir = tempfile::tempdir().unwrap();
    let destination_dir = tempfile::tempdir().unwrap();
    let origin_db = origin_dir.path().join("origin.db");
    let destination_db = destination_dir.path().join("destination.db");

    let origin_provider = ChannelProvider::new(vec![]);
    let origin_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
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

    let device_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "recovery-device",
        }) as Arc<dyn Tool>]);
    let device = spawn_attached_device(
        device_dispatcher,
        device_workspace.path().to_path_buf(),
        "recovery-secret".to_string(),
    )
    .await;

    let (_, pair_body) = post_json(
        origin_addr,
        "/api/onboarding/devices/pairing-codes",
        json!({}),
    )
    .await;
    let (redeem_status, _) = post_json(
        origin_addr,
        "/api/onboarding/devices/redeem",
        json!({
            "pairing_code": pair_body["pairing_code"],
            "device_id": "recovery-device-a",
            "label": "Recovery Device A",
            "kind": "attached_device",
            "base_url": device.base_url,
            "shared_token_secret_ref": recovery_secret_ref,
            "workspace_root": device_workspace.path().to_string_lossy(),
            "capabilities": ["device_identity"]
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
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
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

    let (_, devices_body) = get_json(destination_addr, "/api/devices").await;
    assert!(
        devices_body["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|device| device["device_id"] == "recovery-device-a"),
        "import should restore devices: {devices_body:?}"
    );

    let (_, schedules_body) = get_json(destination_addr, "/api/schedules").await;
    assert_eq!(schedules_body["schedules"][0]["message"], "Nightly sweep");
    assert_eq!(
        schedules_body["schedules"][0]["session"]["policy"],
        "continuation"
    );

    device.join_handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_bundle_restores_service_after_installation_move() {
    let _guard = with_secret_env();
    let gateway_workspace = tempfile::TempDir::new().unwrap();
    let device_workspace = tempfile::TempDir::new().unwrap();
    let origin_dir = tempfile::tempdir().unwrap();
    let destination_dir = tempfile::tempdir().unwrap();
    let origin_db = origin_dir.path().join("origin.db");
    let destination_db = destination_dir.path().join("destination.db");

    let origin_provider = ChannelProvider::new(vec![]);
    let origin_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
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

    let device_dispatcher =
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
            marker: "moved-device",
        }) as Arc<dyn Tool>]);
    let device = spawn_attached_device(
        device_dispatcher,
        device_workspace.path().to_path_buf(),
        "moved-secret".to_string(),
    )
    .await;

    let (_, pair_body) = post_json(
        origin_addr,
        "/api/onboarding/devices/pairing-codes",
        json!({}),
    )
    .await;
    let _ = post_json(
        origin_addr,
        "/api/onboarding/devices/redeem",
        json!({
            "pairing_code": pair_body["pairing_code"],
            "device_id": "moved-device-a",
            "label": "Moved Device A",
            "kind": "attached_device",
            "base_url": device.base_url,
            "shared_token_secret_ref": moved_secret_ref,
            "workspace_root": device_workspace.path().to_string_lossy(),
            "capabilities": ["device_identity"]
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
        swage_gateway::tool::ToolDispatcher::allow_all(vec![Arc::new(DeviceIdentityTool {
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

    let resolved_device = swage_gateway::device::resolve_device(
        &destination_state,
        Some(&json!({ "device_id": "moved-device-a" })),
    )
    .await
    .expect("imported device should resolve after installation move");
    let mut ctx = MockToolContext::new("gateway", "recovery-session");
    ctx.working_directory = device_workspace.path().to_path_buf();
    let tool_result = swage_gateway::device::execute_on_remote_device(
        &destination_state.secret_manager,
        &resolved_device,
        "device_identity",
        &json!({}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(tool_result["device"], "moved-device");

    device.join_handle.abort();
}
