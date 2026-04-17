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
use swage_storage::model::NewExecutionHostRegistration;
use swage_tool::context::{SerializableToolContext, ToolContext};
use swage_tool::dispatch::ToolDispatcher;

use crate::secret::SecretManager;
use crate::state::AppState;
use crate::tool::workspace::resolve_workspace_root;

pub const LOCAL_EXECUTION_HOST_ID: &str = "local";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionHostKind {
    Local,
    AttachedNode,
    Docker,
    Ssh,
}

impl std::fmt::Display for ExecutionHostKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            ExecutionHostKind::Local => "local",
            ExecutionHostKind::AttachedNode => "attached_node",
            ExecutionHostKind::Docker => "docker",
            ExecutionHostKind::Ssh => "ssh",
        };
        write!(f, "{value}")
    }
}

impl std::str::FromStr for ExecutionHostKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "attached_node" => Ok(Self::AttachedNode),
            "docker" => Ok(Self::Docker),
            "ssh" => Ok(Self::Ssh),
            other => Err(format!("unknown execution host kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegisteredExecutionHost {
    pub host_id: String,
    pub label: Option<String>,
    pub kind: ExecutionHostKind,
    pub base_url: Option<String>,
    pub shared_token_secret_ref: Option<String>,
    pub workspace_root: PathBuf,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct ExecutionHostRuntime {
    pub hosts: HashMap<String, RegisteredExecutionHost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionHostResponse {
    pub host_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: Option<String>,
    pub workspace_root: String,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionHostListResponse {
    pub hosts: Vec<ExecutionHostResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterExecutionHostRequest {
    pub host_id: String,
    pub label: Option<String>,
    #[serde(default = "default_remote_host_kind")]
    pub kind: String,
    pub base_url: String,
    pub shared_token_secret_ref: String,
    pub workspace_root: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn default_remote_host_kind() -> String {
    ExecutionHostKind::AttachedNode.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteToolExecutionRequest {
    pub shared_token: String,
    pub tool_name: String,
    pub params: Value,
    pub context: SerializableToolContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteToolExecutionResponse {
    pub output: Value,
}

#[derive(Clone)]
struct TransportBackedHostState {
    kind: ExecutionHostKind,
    shared_token: String,
    dispatcher: Arc<ToolDispatcher>,
    capabilities: Vec<String>,
    workspace_root: PathBuf,
}

pub fn make_local_execution_host(
    config: &swage_config::ResolvedConfig,
    dispatcher: &ToolDispatcher,
) -> RegisteredExecutionHost {
    RegisteredExecutionHost {
        host_id: LOCAL_EXECUTION_HOST_ID.to_string(),
        label: Some("Local machine".to_string()),
        kind: ExecutionHostKind::Local,
        base_url: None,
        shared_token_secret_ref: None,
        workspace_root: resolve_workspace_root(config),
        capabilities: dispatcher
            .registry()
            .list()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        registered_at: Utc::now(),
    }
}

pub async fn register_local_execution_host(state: &AppState) {
    let local_host = make_local_execution_host(&state.config, state.tool_dispatcher.as_ref());
    let mut runtime = state.execution_hosts.lock().await;
    runtime.hosts.insert(local_host.host_id.clone(), local_host);
}

pub async fn register_remote_execution_host(
    state: &AppState,
    request: RegisterExecutionHostRequest,
) -> Result<RegisteredExecutionHost, swage_core::GatewayError> {
    let host = RegisteredExecutionHost {
        host_id: request.host_id,
        label: request.label.clone(),
        kind: ExecutionHostKind::from_str(&request.kind).unwrap_or(ExecutionHostKind::AttachedNode),
        base_url: Some(request.base_url),
        shared_token_secret_ref: Some(request.shared_token_secret_ref),
        workspace_root: PathBuf::from(request.workspace_root),
        capabilities: request.capabilities,
        registered_at: Utc::now(),
    };

    state
        .storage
        .upsert_execution_host(NewExecutionHostRegistration {
            host_id: host.host_id.clone(),
            label: host.label.clone(),
            kind: host.kind.to_string(),
            base_url: host.base_url.clone().unwrap_or_default(),
            shared_token: host
                .shared_token_secret_ref
                .clone()
                .unwrap_or_default(),
            workspace_root: host.workspace_root.to_string_lossy().into_owned(),
            capabilities: host.capabilities.clone(),
        })
        .await
        .map_err(swage_core::GatewayError::Storage)?;

    let mut runtime = state.execution_hosts.lock().await;
    runtime.hosts.insert(host.host_id.clone(), host.clone());
    drop(runtime);

    crate::presence::upsert_machine_heartbeat(
        state,
        crate::presence::MachineHeartbeatRequest {
            machine_id: host.host_id.clone(),
            label: host.label.clone(),
            stale_after_secs: 30,
        },
    )
    .await;

    Ok(host)
}

pub async fn restore_registered_execution_hosts(
    state: &AppState,
) -> Result<(), swage_core::StorageError> {
    let persisted_hosts = state.storage.list_execution_hosts().await?;
    let mut runtime = state.execution_hosts.lock().await;

    for host in persisted_hosts {
        runtime.hosts.insert(
            host.host_id.clone(),
            RegisteredExecutionHost {
                host_id: host.host_id,
                label: host.label,
                kind: ExecutionHostKind::from_str(&host.kind)
                    .unwrap_or(ExecutionHostKind::AttachedNode),
                base_url: Some(host.base_url),
                shared_token_secret_ref: Some(host.shared_token),
                workspace_root: PathBuf::from(host.workspace_root),
                capabilities: host.capabilities,
                registered_at: host.registered_at,
            },
        );
    }

    Ok(())
}

pub async fn list_execution_hosts(state: &AppState) -> ExecutionHostListResponse {
    let runtime = state.execution_hosts.lock().await;
    let mut hosts = runtime
        .hosts
        .values()
        .cloned()
        .map(|host| ExecutionHostResponse {
            host_id: host.host_id,
            label: host.label,
            kind: host.kind.to_string(),
            base_url: host.base_url,
            workspace_root: host.workspace_root.to_string_lossy().into_owned(),
            capabilities: host.capabilities,
            registered_at: host.registered_at,
        })
        .collect::<Vec<_>>();
    hosts.sort_by(|left, right| left.host_id.cmp(&right.host_id));
    ExecutionHostListResponse { hosts }
}

pub async fn resolve_execution_host(
    state: &AppState,
    session_metadata: Option<&serde_json::Value>,
) -> Option<RegisteredExecutionHost> {
    let runtime = state.execution_hosts.lock().await;
    select_execution_host(&runtime.hosts, session_metadata)
}

fn select_execution_host(
    hosts: &HashMap<String, RegisteredExecutionHost>,
    session_metadata: Option<&serde_json::Value>,
) -> Option<RegisteredExecutionHost> {
    if let Some(host_id) = session_metadata
        .and_then(|metadata| metadata.get("execution_host_id"))
        .and_then(|value| value.as_str())
    {
        if let Some(host) = hosts.get(host_id) {
            return Some(host.clone());
        }
    }

    if let Some(preferred_kind) = session_metadata
        .and_then(|metadata| metadata.get("execution_backend"))
        .and_then(|value| value.as_str())
        .and_then(|value| ExecutionHostKind::from_str(value).ok())
    {
        let mut matching_hosts = hosts
            .values()
            .filter(|host| host.kind == preferred_kind)
            .cloned()
            .collect::<Vec<_>>();
        matching_hosts.sort_by(|left, right| left.host_id.cmp(&right.host_id));
        if let Some(host) = matching_hosts.into_iter().next() {
            return Some(host);
        }
    }

    if let Some(local) = hosts.get(LOCAL_EXECUTION_HOST_ID) {
        return Some(local.clone());
    }

    let mut attached_hosts = hosts
        .values()
        .filter(|host| host.kind != ExecutionHostKind::Local)
        .cloned()
        .collect::<Vec<_>>();
    attached_hosts.sort_by(|left, right| left.host_id.cmp(&right.host_id));
    attached_hosts.into_iter().next()
}

pub async fn execute_on_remote_host(
    secret_manager: &SecretManager,
    host: &RegisteredExecutionHost,
    tool_name: &str,
    params: &Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
    if host.kind == ExecutionHostKind::Local {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "execution host '{}' uses the local backend and cannot be called through transport",
                host.host_id
            ),
        });
    }

    if !host
        .capabilities
        .iter()
        .any(|capability| capability == tool_name)
    {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "execution host '{}' does not expose tool '{}'",
                host.host_id, tool_name
            ),
        });
    }

    if !context_within_workspace(ctx, &host.workspace_root) {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "gateway context for '{}' escapes execution host workspace",
                host.host_id
            ),
        });
    }

    let url = format!(
        "{}/api/node/execute-tool",
        host.base_url
            .as_deref()
            .unwrap_or_default()
            .trim_end_matches('/')
    );
    let shared_token = match host.shared_token_secret_ref.as_deref() {
        Some(reference) => secret_manager
            .resolve(reference)
            .await
            .map_err(|error| ToolError::ExecutionFailed {
                name: tool_name.to_string(),
                message: error.to_string(),
            })?
            .value,
        None => {
            return Err(ToolError::ExecutionFailed {
                name: tool_name.to_string(),
                message: format!(
                    "execution host '{}' is missing its shared token secret reference",
                    host.host_id
                ),
            });
        }
    };
    let request = RemoteToolExecutionRequest {
        shared_token,
        tool_name: tool_name.to_string(),
        params: params.clone(),
        context: SerializableToolContext::from_context(ctx),
    };

    let response = Client::new()
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "failed to contact execution host '{}': {error}",
                host.host_id
            ),
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "execution host '{}' rejected tool call with status {}: {}",
                host.host_id, status, body
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
                    "execution host '{}' returned invalid JSON: {error}",
                    host.host_id
                ),
            })?;
    Ok(payload.output)
}

fn context_within_workspace(ctx: &dyn ToolContext, workspace_root: &Path) -> bool {
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    path_within_workspace(ctx.working_directory(), &canonical_root)
        && ctx
            .allowed_paths()
            .iter()
            .all(|path| path_within_workspace(path, &canonical_root))
}

fn path_within_workspace(path: &Path, workspace_root: &Path) -> bool {
    let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    candidate.starts_with(workspace_root)
}

async fn remote_host_execute_tool(
    State(state): State<TransportBackedHostState>,
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
            format!(
                "tool '{}' is not exposed by this execution host",
                request.tool_name
            ),
        ));
    }

    if !context_within_workspace(&request.context, &state.workspace_root) {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "gateway context escapes {} execution-host workspace",
                state.kind
            ),
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

pub struct TransportBackedHostHandle {
    pub base_url: String,
    pub join_handle: JoinHandle<()>,
}

pub async fn spawn_transport_backed_host(
    kind: ExecutionHostKind,
    dispatcher: ToolDispatcher,
    workspace_root: PathBuf,
    shared_token: String,
) -> TransportBackedHostHandle {
    let capabilities = dispatcher
        .registry()
        .list()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let state = TransportBackedHostState {
        kind,
        shared_token,
        dispatcher: Arc::new(dispatcher),
        capabilities,
        workspace_root,
    };
    let router = Router::new()
        .route("/api/node/execute-tool", post(remote_host_execute_tool))
        .route(
            "/api/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let join_handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    TransportBackedHostHandle {
        base_url: format!("http://{addr}"),
        join_handle,
    }
}

pub async fn spawn_attached_node(
    dispatcher: ToolDispatcher,
    workspace_root: PathBuf,
    shared_token: String,
) -> TransportBackedHostHandle {
    spawn_transport_backed_host(
        ExecutionHostKind::AttachedNode,
        dispatcher,
        workspace_root,
        shared_token,
    )
    .await
}

pub async fn register_attached_node_with_gateway(
    gateway_url: &str,
    request: &RegisterExecutionHostRequest,
) -> reqwest::Result<()> {
    Client::new()
        .post(format!(
            "{}/api/execution-hosts/register",
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
    use std::sync::Arc;
    use swage_storage::SqliteStorage;
    use swage_tool::context::MockToolContext;

    async fn test_secret_manager() -> crate::secret::SecretManager {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let manager = crate::secret::SecretManager::new(storage.clone());
        manager
            .create_secret(crate::secret::CreateSecretRequest {
                name: "remote token".into(),
                material_kind: "node_credential".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                metadata: None,
                value: Some("secret".into()),
                external_reference: None,
            })
            .await
            .unwrap();
        manager
    }

    fn registered_host(host_id: &str, kind: ExecutionHostKind) -> RegisteredExecutionHost {
        RegisteredExecutionHost {
            host_id: host_id.to_string(),
            label: None,
            kind,
        base_url: Some("http://127.0.0.1:1".to_string()),
        shared_token_secret_ref: Some("secret://managed/00000000-0000-0000-0000-000000000001".to_string()),
        workspace_root: PathBuf::from("/tmp/workspace"),
        capabilities: vec!["host_identity".to_string()],
        registered_at: Utc::now(),
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
        assert!(!context_within_workspace(&ctx, Path::new("/tmp/workspace")));
    }

    #[test]
    fn select_execution_host_prefers_metadata_backend_kind() {
        let hosts = HashMap::from([
            (
                LOCAL_EXECUTION_HOST_ID.to_string(),
                registered_host(LOCAL_EXECUTION_HOST_ID, ExecutionHostKind::Local),
            ),
            (
                "docker-a".to_string(),
                registered_host("docker-a", ExecutionHostKind::Docker),
            ),
            (
                "ssh-a".to_string(),
                registered_host("ssh-a", ExecutionHostKind::Ssh),
            ),
        ]);

        let selected = select_execution_host(
            &hosts,
            Some(&serde_json::json!({"execution_backend": "ssh"})),
        )
        .expect("host should resolve");

        assert_eq!(selected.host_id, "ssh-a");
    }

    #[test]
    fn select_execution_host_prefers_explicit_host_id_over_backend_kind() {
        let hosts = HashMap::from([
            (
                LOCAL_EXECUTION_HOST_ID.to_string(),
                registered_host(LOCAL_EXECUTION_HOST_ID, ExecutionHostKind::Local),
            ),
            (
                "docker-a".to_string(),
                registered_host("docker-a", ExecutionHostKind::Docker),
            ),
            (
                "ssh-a".to_string(),
                registered_host("ssh-a", ExecutionHostKind::Ssh),
            ),
        ]);

        let selected = select_execution_host(
            &hosts,
            Some(&serde_json::json!({
                "execution_host_id": "docker-a",
                "execution_backend": "ssh"
            })),
        )
        .expect("host should resolve");

        assert_eq!(selected.host_id, "docker-a");
    }

    #[tokio::test]
    async fn execute_on_remote_host_rejects_unadvertised_tool_before_network_call() {
        let host = RegisteredExecutionHost {
            host_id: "remote-a".to_string(),
            label: None,
            kind: ExecutionHostKind::AttachedNode,
            base_url: Some("http://127.0.0.1:1".to_string()),
            shared_token_secret_ref: Some("secret://managed/00000000-0000-0000-0000-000000000001".to_string()),
            workspace_root: PathBuf::from("/tmp/workspace"),
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

        let secret_manager = test_secret_manager().await;
        let error = execute_on_remote_host(&secret_manager, &host, "shell", &serde_json::json!({}), &ctx)
            .await
            .expect_err("unadvertised tool should be rejected");
        assert!(error.to_string().contains("does not expose tool 'shell'"));
    }

    #[tokio::test]
    async fn execute_on_remote_host_rejects_context_outside_workspace_before_network_call() {
        let host = RegisteredExecutionHost {
            host_id: "remote-a".to_string(),
            label: None,
            kind: ExecutionHostKind::AttachedNode,
            base_url: Some("http://127.0.0.1:1".to_string()),
            shared_token_secret_ref: Some("secret://managed/00000000-0000-0000-0000-000000000001".to_string()),
            workspace_root: PathBuf::from("/tmp/workspace"),
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

        let secret_manager = test_secret_manager().await;
        let error = execute_on_remote_host(&secret_manager, &host, "shell", &serde_json::json!({}), &ctx)
            .await
            .expect_err("out-of-root context should be rejected");
        assert!(
            error
                .to_string()
                .contains("escapes execution host workspace")
        );
    }
}
