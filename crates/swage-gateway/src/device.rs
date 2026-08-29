use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use swage_core::ToolError;
use swage_storage::model::NewDeviceRegistration;
use swage_tool::context::{ToolContext, serialise_tool_context};
use swage_tool::dispatch::ToolDispatcher;

use crate::secret::SecretManager;
use crate::state::AppState;
use crate::tool::workspace::resolve_workspace_root;

pub const LOCAL_DEVICE_ID: &str = "local";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Local,
    AttachedDevice,
    Docker,
    Ssh,
}

impl std::fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            DeviceKind::Local => "local",
            DeviceKind::AttachedDevice => "attached_device",
            DeviceKind::Docker => "docker",
            DeviceKind::Ssh => "ssh",
        };
        write!(f, "{value}")
    }
}

impl std::str::FromStr for DeviceKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "attached_device" => Ok(Self::AttachedDevice),
            "docker" => Ok(Self::Docker),
            "ssh" => Ok(Self::Ssh),
            other => Err(format!("unknown device kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegisteredDevice {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: DeviceKind,
    pub base_url: Option<String>,
    pub shared_token_secret_ref: Option<String>,
    pub workspace_root: PathBuf,
    pub workspace_roots: Vec<PathBuf>,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct DeviceRuntime {
    pub devices: HashMap<String, RegisteredDevice>,
}

pub use swage_protocol::device::{
    DeviceListResponse, DeviceResponse, RegisterDeviceRequest, RemoteToolExecutionRequest,
    RemoteToolExecutionResponse,
};

#[derive(Clone)]
struct TransportBackedDeviceState {
    kind: DeviceKind,
    shared_token: String,
    dispatcher: Arc<ToolDispatcher>,
    capabilities: Vec<String>,
    workspace_roots: Vec<PathBuf>,
}

pub fn make_local_device(
    config: &swage_config::ResolvedConfig,
    dispatcher: &ToolDispatcher,
) -> RegisteredDevice {
    let workspace_root = resolve_workspace_root(config);
    RegisteredDevice {
        device_id: LOCAL_DEVICE_ID.to_string(),
        label: Some("Local device".to_string()),
        kind: DeviceKind::Local,
        base_url: None,
        shared_token_secret_ref: None,
        workspace_root: workspace_root.clone(),
        workspace_roots: vec![workspace_root],
        capabilities: dispatcher
            .registry()
            .list()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        registered_at: Utc::now(),
    }
}

pub async fn register_local_device(state: &AppState) {
    let local_device = make_local_device(&state.config, state.tool_dispatcher.as_ref());
    let mut runtime = state.devices.lock().await;
    runtime
        .devices
        .insert(local_device.device_id.clone(), local_device);
}

pub async fn register_remote_device(
    state: &AppState,
    request: RegisterDeviceRequest,
) -> Result<RegisteredDevice, swage_core::GatewayError> {
    let workspace_root = PathBuf::from(&request.workspace_root);
    let workspace_roots =
        normalise_workspace_roots(workspace_root.clone(), request.workspace_roots);
    let device = RegisteredDevice {
        device_id: request.device_id,
        label: request.label.clone(),
        kind: DeviceKind::from_str(&request.kind).unwrap_or(DeviceKind::AttachedDevice),
        base_url: Some(request.base_url),
        shared_token_secret_ref: Some(request.shared_token_secret_ref),
        workspace_root,
        workspace_roots,
        capabilities: request.capabilities,
        registered_at: Utc::now(),
    };

    state
        .storage
        .upsert_device(NewDeviceRegistration {
            device_id: device.device_id.clone(),
            label: device.label.clone(),
            kind: device.kind.to_string(),
            base_url: device.base_url.clone().unwrap_or_default(),
            shared_token: device.shared_token_secret_ref.clone().unwrap_or_default(),
            workspace_root: device.workspace_root.to_string_lossy().into_owned(),
            workspace_roots: device
                .workspace_roots
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            capabilities: device.capabilities.clone(),
        })
        .await
        .map_err(swage_core::GatewayError::Storage)?;

    let mut runtime = state.devices.lock().await;
    runtime
        .devices
        .insert(device.device_id.clone(), device.clone());
    drop(runtime);

    crate::presence::upsert_device_heartbeat(
        state,
        crate::presence::DeviceHeartbeatRequest {
            device_id: device.device_id.clone(),
            label: device.label.clone(),
            stale_after_secs: 30,
        },
    )
    .await;

    Ok(device)
}

pub async fn restore_registered_devices(state: &AppState) -> Result<(), swage_core::StorageError> {
    let persisted_devices = state.storage.list_devices().await?;
    let mut runtime = state.devices.lock().await;

    for device in persisted_devices {
        runtime.devices.insert(
            device.device_id.clone(),
            RegisteredDevice {
                device_id: device.device_id,
                label: device.label,
                kind: DeviceKind::from_str(&device.kind).unwrap_or(DeviceKind::AttachedDevice),
                base_url: Some(device.base_url),
                shared_token_secret_ref: Some(device.shared_token),
                workspace_root: PathBuf::from(&device.workspace_root),
                workspace_roots: normalise_workspace_roots(
                    PathBuf::from(&device.workspace_root),
                    device.workspace_roots,
                ),
                capabilities: device.capabilities,
                registered_at: device.registered_at,
            },
        );
    }

    Ok(())
}

pub async fn list_devices(state: &AppState) -> DeviceListResponse {
    let runtime = state.devices.lock().await;
    let mut devices = runtime
        .devices
        .values()
        .cloned()
        .map(|device| DeviceResponse {
            device_id: device.device_id,
            label: device.label,
            kind: device.kind.to_string(),
            base_url: device.base_url,
            workspace_root: device.workspace_root.to_string_lossy().into_owned(),
            workspace_roots: device
                .workspace_roots
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            capabilities: device.capabilities,
            registered_at: device.registered_at,
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
    DeviceListResponse { devices }
}

pub async fn resolve_device(
    state: &AppState,
    session_metadata: Option<&serde_json::Value>,
) -> Option<RegisteredDevice> {
    let runtime = state.devices.lock().await;
    select_device(&runtime.devices, session_metadata)
}

fn select_device(
    devices: &HashMap<String, RegisteredDevice>,
    session_metadata: Option<&serde_json::Value>,
) -> Option<RegisteredDevice> {
    if let Some(device_id) = session_metadata
        .and_then(|metadata| metadata.get("device_id"))
        .and_then(|value| value.as_str())
        && let Some(device) = devices.get(device_id)
    {
        return Some(device.clone());
    }

    if let Some(preferred_kind) = session_metadata
        .and_then(|metadata| metadata.get("execution_backend"))
        .and_then(|value| value.as_str())
        .and_then(|value| DeviceKind::from_str(value).ok())
    {
        let mut matching_devices = devices
            .values()
            .filter(|device| device.kind == preferred_kind)
            .cloned()
            .collect::<Vec<_>>();
        matching_devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        if let Some(device) = matching_devices.into_iter().next() {
            return Some(device);
        }
    }

    if let Some(local) = devices.get(LOCAL_DEVICE_ID) {
        return Some(local.clone());
    }

    let mut attached_devices = devices
        .values()
        .filter(|device| device.kind != DeviceKind::Local)
        .cloned()
        .collect::<Vec<_>>();
    attached_devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
    attached_devices.into_iter().next()
}

pub async fn execute_on_remote_device(
    secret_manager: &SecretManager,
    device: &RegisteredDevice,
    tool_name: &str,
    params: &Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
    if device.kind == DeviceKind::Local {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "device '{}' uses the local backend and cannot be called through transport",
                device.device_id
            ),
        });
    }

    if !device
        .capabilities
        .iter()
        .any(|capability| capability == tool_name)
    {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "device '{}' does not expose tool '{}'",
                device.device_id, tool_name
            ),
        });
    }

    if !context_within_workspaces(ctx, &device.workspace_roots) {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "gateway context for '{}' escapes device workspace",
                device.device_id
            ),
        });
    }

    let url = format!(
        "{}/api/device/execute-tool",
        device
            .base_url
            .as_deref()
            .unwrap_or_default()
            .trim_end_matches('/')
    );
    let shared_token = match device.shared_token_secret_ref.as_deref() {
        Some(reference) => {
            secret_manager
                .resolve(reference)
                .await
                .map_err(|error| ToolError::ExecutionFailed {
                    name: tool_name.to_string(),
                    message: error.to_string(),
                })?
                .value
        }
        None => {
            return Err(ToolError::ExecutionFailed {
                name: tool_name.to_string(),
                message: format!(
                    "device '{}' is missing its shared token secret reference",
                    device.device_id
                ),
            });
        }
    };
    let request = RemoteToolExecutionRequest {
        shared_token,
        tool_name: tool_name.to_string(),
        params: params.clone(),
        context: serialise_tool_context(ctx),
    };

    let response = Client::new()
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!("failed to contact device '{}': {error}", device.device_id),
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "device '{}' rejected tool call with status {}: {}",
                device.device_id, status, body
            ),
        });
    }

    let payload: RemoteToolExecutionResponse =
        response
            .json()
            .await
            .map_err(|error| ToolError::ExecutionFailed {
                name: tool_name.to_string(),
                message: format!(
                    "device '{}' returned invalid JSON: {error}",
                    device.device_id
                ),
            })?;
    Ok(payload.output)
}

fn context_within_workspaces(ctx: &dyn ToolContext, workspace_roots: &[PathBuf]) -> bool {
    let canonical_roots = workspace_roots
        .iter()
        .map(|root| root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
        .collect::<Vec<_>>();

    path_within_any_workspace(ctx.working_directory(), &canonical_roots)
        && ctx
            .allowed_paths()
            .iter()
            .all(|path| path_within_any_workspace(path, &canonical_roots))
}

fn path_within_any_workspace(path: &Path, workspace_roots: &[PathBuf]) -> bool {
    let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    workspace_roots
        .iter()
        .any(|root| candidate.starts_with(root))
}

fn normalise_workspace_roots(primary: PathBuf, roots: Vec<String>) -> Vec<PathBuf> {
    let mut normalised = vec![primary];
    for root in roots {
        let path = PathBuf::from(root);
        if !normalised.contains(&path) {
            normalised.push(path);
        }
    }
    normalised
}

async fn remote_device_execute_tool(
    State(state): State<TransportBackedDeviceState>,
    Json(request): Json<RemoteToolExecutionRequest>,
) -> Result<Json<RemoteToolExecutionResponse>, (StatusCode, String)> {
    if request.shared_token != state.shared_token {
        return Err((StatusCode::UNAUTHORIZED, "invalid shared token".to_string()));
    }

    if !state
        .capabilities
        .iter()
        .any(|capability| capability == &request.tool_name)
    {
        return Err((
            StatusCode::FORBIDDEN,
            format!("tool '{}' is not exposed by this device", request.tool_name),
        ));
    }

    if !context_within_workspaces(&request.context, &state.workspace_roots) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("gateway context escapes {} device workspace", state.kind),
        ));
    }

    let output = state
        .dispatcher
        .dispatch(
            &request.tool_name,
            request.params,
            &request.context,
            request.context.agent_id(),
        )
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;

    Ok(Json(RemoteToolExecutionResponse {
        output: output.result,
    }))
}

pub struct TransportBackedDeviceHandle {
    pub base_url: String,
    pub join_handle: JoinHandle<()>,
}

pub async fn spawn_transport_backed_device(
    kind: DeviceKind,
    dispatcher: ToolDispatcher,
    workspace_root: PathBuf,
    shared_token: String,
) -> TransportBackedDeviceHandle {
    let capabilities = dispatcher
        .registry()
        .list()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let state = TransportBackedDeviceState {
        kind,
        shared_token,
        dispatcher: Arc::new(dispatcher),
        capabilities,
        workspace_roots: vec![workspace_root],
    };
    let router = Router::new()
        .route("/api/device/execute-tool", post(remote_device_execute_tool))
        .route(
            "/api/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .expect("test server should expose its local address");
    let join_handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("test server should run");
    });

    TransportBackedDeviceHandle {
        base_url: format!("http://{addr}"),
        join_handle,
    }
}

pub async fn spawn_attached_device(
    dispatcher: ToolDispatcher,
    workspace_root: PathBuf,
    shared_token: String,
) -> TransportBackedDeviceHandle {
    spawn_transport_backed_device(
        DeviceKind::AttachedDevice,
        dispatcher,
        workspace_root,
        shared_token,
    )
    .await
}

pub async fn register_attached_device_with_gateway(
    gateway_url: &str,
    request: &RegisterDeviceRequest,
) -> reqwest::Result<()> {
    Client::new()
        .post(format!(
            "{}/api/devices/register",
            gateway_url.trim_end_matches('/')
        ))
        .json(request)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;
    use swage_core::ToolError;
    use swage_storage::SqliteStorage;
    use swage_tool::context::{MockToolContext, UserInfo};
    use swage_tool::tool::Tool;
    use uuid::Uuid;

    fn test_secret_manager() -> crate::secret::SecretManager {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        crate::secret::SecretManager::new(storage)
    }

    fn registered_device(device_id: &str, kind: DeviceKind) -> RegisteredDevice {
        RegisteredDevice {
            device_id: device_id.to_string(),
            label: None,
            kind,
            base_url: Some("http://127.0.0.1:1".to_string()),
            shared_token_secret_ref: Some(
                "secret://managed/00000000-0000-0000-0000-000000000001".to_string(),
            ),
            workspace_root: PathBuf::from("/tmp/workspace"),
            workspace_roots: vec![PathBuf::from("/tmp/workspace")],
            capabilities: vec!["device_identity".to_string()],
            registered_at: Utc::now(),
        }
    }

    struct ObservingTool;

    #[async_trait]
    impl Tool for ObservingTool {
        fn name(&self) -> &str {
            "observe_context"
        }

        fn description(&self) -> &str {
            "Return the tool context observed by the device."
        }

        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        }

        async fn execute(&self, _params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            Ok(json!({
                "agent_id": ctx.agent_id(),
                "session_id": ctx.session_id(),
                "working_directory": ctx.working_directory(),
                "allowed_paths": ctx.allowed_paths(),
                "user_name": ctx.user_info().map(|user| user.name.clone()),
                "call_id": ctx.call_id(),
                "turn_id": ctx.turn_id(),
            }))
        }
    }

    #[test]
    fn context_within_workspace_rejects_outside_allowed_path() {
        let ctx = MockToolContext {
            agent_id: "gateway".to_string(),
            session_id: "session".to_string(),
            working_directory: PathBuf::from("/tmp/workspace"),
            allowed_paths: vec![PathBuf::from("/etc")],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        assert!(!context_within_workspaces(
            &ctx,
            &[PathBuf::from("/tmp/workspace")]
        ));
    }

    #[test]
    fn context_within_workspaces_accepts_advertised_attachment_root() {
        let ctx = MockToolContext {
            agent_id: "gateway".to_string(),
            session_id: "session".to_string(),
            working_directory: PathBuf::from("/tmp/workspace"),
            allowed_paths: vec![PathBuf::from("/mnt/shared")],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        assert!(context_within_workspaces(
            &ctx,
            &[
                PathBuf::from("/tmp/workspace"),
                PathBuf::from("/mnt/shared")
            ]
        ));
    }

    #[test]
    fn select_device_prefers_metadata_backend_kind() {
        let devices = HashMap::from([
            (
                LOCAL_DEVICE_ID.to_string(),
                registered_device(LOCAL_DEVICE_ID, DeviceKind::Local),
            ),
            (
                "docker-a".to_string(),
                registered_device("docker-a", DeviceKind::Docker),
            ),
            (
                "ssh-a".to_string(),
                registered_device("ssh-a", DeviceKind::Ssh),
            ),
        ]);

        let selected = select_device(
            &devices,
            Some(&serde_json::json!({"execution_backend": "ssh"})),
        )
        .expect("device should resolve");

        assert_eq!(selected.device_id, "ssh-a");
    }

    #[test]
    fn select_device_prefers_explicit_device_id_over_backend_kind() {
        let devices = HashMap::from([
            (
                LOCAL_DEVICE_ID.to_string(),
                registered_device(LOCAL_DEVICE_ID, DeviceKind::Local),
            ),
            (
                "docker-a".to_string(),
                registered_device("docker-a", DeviceKind::Docker),
            ),
            (
                "ssh-a".to_string(),
                registered_device("ssh-a", DeviceKind::Ssh),
            ),
        ]);

        let selected = select_device(
            &devices,
            Some(&serde_json::json!({
                "device_id": "docker-a",
                "execution_backend": "ssh"
            })),
        )
        .expect("device should resolve");

        assert_eq!(selected.device_id, "docker-a");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_on_remote_device_rejects_unadvertised_tool_before_network_call() {
        let device = RegisteredDevice {
            device_id: "remote-a".to_string(),
            label: None,
            kind: DeviceKind::AttachedDevice,
            base_url: Some("http://127.0.0.1:1".to_string()),
            shared_token_secret_ref: Some(
                "secret://managed/00000000-0000-0000-0000-000000000001".to_string(),
            ),
            workspace_root: PathBuf::from("/tmp/workspace"),
            workspace_roots: vec![PathBuf::from("/tmp/workspace")],
            capabilities: vec!["echo".to_string()],
            registered_at: Utc::now(),
        };
        let ctx = MockToolContext {
            agent_id: "gateway".to_string(),
            session_id: "session".to_string(),
            working_directory: PathBuf::from("/tmp/workspace"),
            allowed_paths: vec![PathBuf::from("/tmp/workspace")],
            user_info: None,
            call_id: None,
            turn_id: None,
        };

        let secret_manager = test_secret_manager();
        let error = execute_on_remote_device(
            &secret_manager,
            &device,
            "shell",
            &serde_json::json!({}),
            &ctx,
        )
        .await
        .expect_err("unadvertised tool should be rejected");
        assert!(error.to_string().contains("does not expose tool 'shell'"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_on_remote_device_rejects_context_outside_workspace_before_network_call() {
        let device = RegisteredDevice {
            device_id: "remote-a".to_string(),
            label: None,
            kind: DeviceKind::AttachedDevice,
            base_url: Some("http://127.0.0.1:1".to_string()),
            shared_token_secret_ref: Some(
                "secret://managed/00000000-0000-0000-0000-000000000001".to_string(),
            ),
            workspace_root: PathBuf::from("/tmp/workspace"),
            workspace_roots: vec![PathBuf::from("/tmp/workspace")],
            capabilities: vec!["shell".to_string()],
            registered_at: Utc::now(),
        };
        let ctx = MockToolContext {
            agent_id: "gateway".to_string(),
            session_id: "session".to_string(),
            working_directory: PathBuf::from("/tmp/workspace"),
            allowed_paths: vec![PathBuf::from("/etc")],
            user_info: None,
            call_id: None,
            turn_id: None,
        };

        let secret_manager = test_secret_manager();
        let error = execute_on_remote_device(
            &secret_manager,
            &device,
            "shell",
            &serde_json::json!({}),
            &ctx,
        )
        .await
        .expect_err("out-of-root context should be rejected");
        assert!(error.to_string().contains("escapes device workspace"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_on_remote_device_preserves_tool_context_across_transport() {
        let workspace = tempfile::TempDir::new().expect("workspace root");
        let workspace_root = workspace.path().to_path_buf();
        let handle = spawn_attached_device(
            ToolDispatcher::allow_all(vec![Arc::new(ObservingTool) as Arc<dyn Tool>]),
            workspace_root.clone(),
            "shared-token".to_string(),
        )
        .await;

        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        let token_variable = format!("SWAGE_TEST_DEVICE_TOKEN_{}", Uuid::new_v4().simple());
        unsafe { std::env::set_var(&token_variable, "shared-token") };
        let secret = secret_manager
            .create_secret(crate::secret::CreateSecretRequest {
                name: "Device token".into(),
                material_kind: "plugin_secret".into(),
                owner_kind: "service".into(),
                backend_kind: "env".into(),
                scope_kind: "plugin".into(),
                scope_id: Some("device".into()),
                metadata: None,
                value: None,
                external_reference: Some(token_variable.clone()),
            })
            .await
            .unwrap();

        let turn_id = Uuid::new_v4();
        let device = RegisteredDevice {
            device_id: "attached-a".to_string(),
            label: None,
            kind: DeviceKind::AttachedDevice,
            base_url: Some(handle.base_url.clone()),
            shared_token_secret_ref: Some(secret.reference),
            workspace_root: workspace_root.clone(),
            workspace_roots: vec![workspace_root.clone()],
            capabilities: vec!["observe_context".to_string()],
            registered_at: Utc::now(),
        };
        let ctx = MockToolContext {
            agent_id: "agent-a".to_string(),
            session_id: "session-1".to_string(),
            working_directory: workspace_root.clone(),
            allowed_paths: vec![workspace_root.clone()],
            user_info: Some(UserInfo {
                id: "user-1".into(),
                name: "Taylor".into(),
                metadata: HashMap::new(),
            }),
            call_id: Some("call-1".to_string()),
            turn_id: Some(turn_id),
        };

        let output = execute_on_remote_device(
            &secret_manager,
            &device,
            "observe_context",
            &json!({}),
            &ctx,
        )
        .await
        .expect("remote device should execute advertised tool");

        assert_eq!(output["agent_id"], "agent-a");
        assert_eq!(output["session_id"], "session-1");
        assert_eq!(
            output["working_directory"],
            workspace_root.to_string_lossy().to_string()
        );
        assert_eq!(
            output["allowed_paths"][0],
            workspace_root.to_string_lossy().to_string()
        );
        assert_eq!(output["user_name"], "Taylor");
        assert_eq!(output["call_id"], "call-1");
        assert_eq!(output["turn_id"], turn_id.to_string());

        handle.join_handle.abort();
        unsafe { std::env::remove_var(token_variable) };
    }
}
