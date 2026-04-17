use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
use swage_tool::context::{SerializableToolContext, ToolContext};
use swage_tool::dispatch::ToolDispatcher;

use crate::state::AppState;
use crate::tool::workspace::resolve_workspace_root;

pub const LOCAL_EXECUTION_HOST_ID: &str = "local";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionHostKind {
    Local,
    AttachedNode,
}

impl std::fmt::Display for ExecutionHostKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            ExecutionHostKind::Local => "local",
            ExecutionHostKind::AttachedNode => "attached_node",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone)]
pub struct RegisteredExecutionHost {
    pub host_id: String,
    pub label: Option<String>,
    pub kind: ExecutionHostKind,
    pub base_url: Option<String>,
    pub shared_token: Option<String>,
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
    pub base_url: String,
    pub shared_token: String,
    pub workspace_root: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
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
struct AttachedNodeState {
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
        shared_token: None,
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
) -> RegisteredExecutionHost {
    let host = RegisteredExecutionHost {
        host_id: request.host_id,
        label: request.label.clone(),
        kind: ExecutionHostKind::AttachedNode,
        base_url: Some(request.base_url),
        shared_token: Some(request.shared_token),
        workspace_root: PathBuf::from(request.workspace_root),
        capabilities: request.capabilities,
        registered_at: Utc::now(),
    };

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

    host
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

    if let Some(host_id) = session_metadata
        .and_then(|metadata| metadata.get("execution_host_id"))
        .and_then(|value| value.as_str())
    {
        if let Some(host) = runtime.hosts.get(host_id) {
            return Some(host.clone());
        }
    }

    if let Some(local) = runtime.hosts.get(LOCAL_EXECUTION_HOST_ID) {
        return Some(local.clone());
    }

    let mut attached_hosts = runtime
        .hosts
        .values()
        .filter(|host| host.kind == ExecutionHostKind::AttachedNode)
        .cloned()
        .collect::<Vec<_>>();
    attached_hosts.sort_by(|left, right| left.host_id.cmp(&right.host_id));
    attached_hosts.into_iter().next()
}

pub async fn execute_on_remote_host(
    host: &RegisteredExecutionHost,
    tool_name: &str,
    params: &Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
    if host.kind != ExecutionHostKind::AttachedNode {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!("execution host '{}' is not a remote node", host.host_id),
        });
    }

    if !host.capabilities.iter().any(|capability| capability == tool_name) {
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
    let request = RemoteToolExecutionRequest {
        shared_token: host.shared_token.clone().unwrap_or_default(),
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
    let candidate = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    candidate.starts_with(workspace_root)
}

async fn remote_node_execute_tool(
    State(state): State<AttachedNodeState>,
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
            format!("tool '{}' is not exposed by this execution host", request.tool_name),
        ));
    }

    if !context_within_workspace(&request.context, &state.workspace_root) {
        return Err((
            StatusCode::FORBIDDEN,
            "gateway context escapes attached-node workspace".to_string(),
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

pub struct AttachedNodeHandle {
    pub base_url: String,
    pub join_handle: JoinHandle<()>,
}

pub async fn spawn_attached_node(
    dispatcher: ToolDispatcher,
    workspace_root: PathBuf,
    shared_token: String,
) -> AttachedNodeHandle {
    let capabilities = dispatcher
        .registry()
        .list()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let state = AttachedNodeState {
        shared_token,
        dispatcher: Arc::new(dispatcher),
        capabilities,
        workspace_root,
    };
    let router = Router::new()
        .route("/api/node/execute-tool", post(remote_node_execute_tool))
        .route("/api/health", get(|| async { Json(serde_json::json!({ "status": "ok" })) }))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let join_handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    AttachedNodeHandle {
        base_url: format!("http://{addr}"),
        join_handle,
    }
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
    use swage_tool::context::MockToolContext;

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

    #[tokio::test]
    async fn execute_on_remote_host_rejects_unadvertised_tool_before_network_call() {
        let host = RegisteredExecutionHost {
            host_id: "remote-a".to_string(),
            label: None,
            kind: ExecutionHostKind::AttachedNode,
            base_url: Some("http://127.0.0.1:1".to_string()),
            shared_token: Some("secret".to_string()),
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

        let error = execute_on_remote_host(&host, "shell", &serde_json::json!({}), &ctx)
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
            shared_token: Some("secret".to_string()),
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

        let error = execute_on_remote_host(&host, "shell", &serde_json::json!({}), &ctx)
            .await
            .expect_err("out-of-root context should be rejected");
        assert!(
            error
                .to_string()
                .contains("escapes execution host workspace")
        );
    }
}
