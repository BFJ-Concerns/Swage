//! Gateway-specific tool execution strategies.
//!
//! Two strategies live here:
//!
//! - **`SandboxedExecutor`** — routes tool calls through a bubblewrap sandbox.
//! - **`GatewayExecutionStrategy`** — wraps an optional inner strategy and
//!   upgrades the tool context for `python_execute` so nested tool execution
//!   has access to the WebSocket sender without task-local state.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use swage_core::{SandboxError, ToolError};
use swage_sandbox::Sandbox;
use swage_storage::store::Storage;
use swage_tool::context::{SerializableToolContext, ToolContext};
use swage_tool::dispatch::ToolExecutionStrategy;
use swage_tool::tool::Tool;

use crate::execution_host::RegisteredExecutionHost;
use crate::secret::SecretManager;
use crate::websocket::ServerEvent;

pub struct GatewayToolContext {
    base: SerializableToolContext,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    storage: Arc<dyn Storage>,
}

impl GatewayToolContext {
    pub fn new(
        ctx: &dyn ToolContext,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        storage: Arc<dyn Storage>,
    ) -> Self {
        Self {
            base: SerializableToolContext::from_context(ctx),
            ws_tx,
            storage,
        }
    }

    pub fn ws_tx(&self) -> &mpsc::UnboundedSender<ServerEvent> {
        &self.ws_tx
    }

    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.storage
    }
}

impl ToolContext for GatewayToolContext {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn agent_id(&self) -> &str {
        self.base.agent_id()
    }

    fn session_id(&self) -> &str {
        self.base.session_id()
    }

    fn working_directory(&self) -> &std::path::Path {
        self.base.working_directory()
    }

    fn allowed_paths(&self) -> &[std::path::PathBuf] {
        self.base.allowed_paths()
    }

    fn user_info(&self) -> Option<&swage_tool::context::UserInfo> {
        self.base.user_info()
    }

    fn call_id(&self) -> Option<&str> {
        self.base.call_id()
    }

    fn turn_id(&self) -> Option<uuid::Uuid> {
        self.base.turn_id()
    }
}

/// Routes tool execution through a bubblewrap sandbox.
///
/// The sandbox looks up and runs the tool by name internally, so the `tool`
/// parameter from the strategy interface is unused. The ToolDispatcher's other
/// pipeline stages (permission check, rate limit, schema validation, leak
/// detection) still run before reaching this strategy.
pub struct SandboxedExecutor {
    sandbox: Arc<dyn Sandbox>,
}

impl SandboxedExecutor {
    /// Wrap a sandbox instance for use as a tool execution strategy.
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl ToolExecutionStrategy for SandboxedExecutor {
    async fn execute(
        &self,
        _tool: &dyn Tool,
        tool_name: &str,
        params: &Value,
        _ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        match self.sandbox.execute_tool(tool_name, params.clone()).await {
            Ok(result) => match result.error {
                // The sandboxed tool itself reported a failure.
                Some(message) => Err(ToolError::ExecutionFailed {
                    name: tool_name.to_owned(),
                    message,
                }),
                None => Ok(result.output),
            },
            Err(SandboxError::IpcError { message }) => Err(ToolError::ExecutionFailed {
                name: tool_name.to_owned(),
                message: format!("sandbox communication error: {message}"),
            }),
            Err(other) => Err(ToolError::ExecutionFailed {
                name: tool_name.to_owned(),
                message: other.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// GatewayExecutionStrategy — explicit python_execute context + optional sandbox
// ---------------------------------------------------------------------------

/// Composite execution strategy for the gateway turn loop.
///
/// Intercepts `python_execute` calls to provide an explicit context wrapper
/// carrying the per-turn WebSocket sender. All other tools delegate unchanged
/// to the optional inner strategy (typically `SandboxedExecutor`) or direct
/// execution.
pub struct GatewayExecutionStrategy {
    /// Optional inner strategy — e.g. `SandboxedExecutor` when sandbox is enabled.
    inner: Option<Arc<dyn ToolExecutionStrategy>>,
    /// WebSocket sender for sub-tool lifecycle events inside Python.
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    /// Resolved execution host for the current turn.
    execution_host: Option<RegisteredExecutionHost>,
    /// Canonical secret resolution surface.
    secret_manager: Arc<SecretManager>,
    /// Durable storage surfaced to authorised host-side tools.
    storage: Arc<dyn Storage>,
}

impl GatewayExecutionStrategy {
    pub fn new(
        inner: Option<Arc<dyn ToolExecutionStrategy>>,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        execution_host: Option<RegisteredExecutionHost>,
        secret_manager: Arc<SecretManager>,
        storage: Arc<dyn Storage>,
    ) -> Self {
        Self {
            inner,
            ws_tx,
            execution_host,
            secret_manager,
            storage,
        }
    }
}

#[async_trait]
impl ToolExecutionStrategy for GatewayExecutionStrategy {
    async fn execute(
        &self,
        tool: &dyn Tool,
        tool_name: &str,
        params: &Value,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        let resolved_params = if tool.host_secret_keys().is_empty() {
            params.clone()
        } else {
            resolve_secret_params(params, tool.host_secret_keys(), &self.secret_manager).await?
        };
        let gateway_ctx =
            GatewayToolContext::new(ctx, self.ws_tx.clone(), Arc::clone(&self.storage));

        if self
            .execution_host
            .as_ref()
            .is_some_and(|host| host.kind != crate::execution_host::ExecutionHostKind::Local)
        {
            return crate::execution_host::execute_on_remote_host(
                &self.secret_manager,
                self.execution_host
                    .as_ref()
                    .expect("checked execution host"),
                tool_name,
                &resolved_params,
                ctx,
            )
            .await;
        }

        if tool_name == "python_execute" {
            tool.execute(resolved_params, &gateway_ctx).await
        } else {
            // Secret-aware tools run on the host so raw values never enter sandbox IPC.
            if !tool.host_secret_keys().is_empty() {
                return tool.execute(resolved_params, &gateway_ctx).await;
            }
            match &self.inner {
                Some(strategy) => {
                    strategy
                        .execute(tool, tool_name, &resolved_params, &gateway_ctx)
                        .await
                }
                None => tool.execute(resolved_params, &gateway_ctx).await,
            }
        }
    }
}

async fn resolve_secret_params(
    params: &Value,
    keys: &[&str],
    secret_manager: &SecretManager,
) -> Result<Value, ToolError> {
    let mut resolved = params.clone();
    let Some(object) = resolved.as_object_mut() else {
        return Ok(resolved);
    };
    for key in keys {
        let Some(value) = object.get_mut(*key) else {
            continue;
        };
        let Some(reference) = value.as_str() else {
            continue;
        };
        if !reference.starts_with("secret://") {
            continue;
        }
        let secret = secret_manager.resolve(reference).await.map_err(|error| {
            ToolError::ExecutionFailed {
                name: "secret_resolution".into(),
                message: error.to_string(),
            }
        })?;
        *value = Value::String(secret.value);
    }
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use swage_sandbox::ToolCallResult;
    use swage_storage::SqliteStorage;
    use swage_tool::context::MockToolContext;
    use swage_tool::dispatch::ToolExecutionStrategy;
    use tokio::sync::Mutex;

    // -- Mock sandbox ---------------------------------------------------------

    /// Canned responses for the mock sandbox.
    enum MockResponse {
        Success(Value),
        ToolError(String),
        IpcError(String),
        Unavailable(String),
    }

    struct MockSandbox {
        /// The response to return from execute_tool.
        response: Mutex<MockResponse>,
        /// Records the (tool_name, params) from the last call.
        last_call: Mutex<Option<(String, Value)>>,
    }

    impl MockSandbox {
        fn new(response: MockResponse) -> Self {
            Self {
                response: Mutex::new(response),
                last_call: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Sandbox for MockSandbox {
        async fn spawn_agent(&mut self) -> Result<(), SandboxError> {
            Ok(())
        }

        async fn execute_tool(
            &self,
            tool_name: &str,
            params: Value,
        ) -> Result<ToolCallResult, SandboxError> {
            *self.last_call.lock().await = Some((tool_name.to_owned(), params.clone()));

            match &*self.response.lock().await {
                MockResponse::Success(output) => Ok(ToolCallResult {
                    output: output.clone(),
                    duration_ms: 0,
                    error: None,
                }),
                MockResponse::ToolError(msg) => Ok(ToolCallResult {
                    output: Value::Null,
                    duration_ms: 0,
                    error: Some(msg.clone()),
                }),
                MockResponse::IpcError(msg) => Err(SandboxError::IpcError {
                    message: msg.clone(),
                }),
                MockResponse::Unavailable(msg) => Err(SandboxError::Unavailable {
                    message: msg.clone(),
                }),
            }
        }

        async fn shutdown(&mut self) -> Result<(), SandboxError> {
            Ok(())
        }
    }

    // -- Dummy tool (unused by sandbox path, but required by the trait) --------

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }
        fn description(&self) -> &str {
            "unused"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(
            &self,
            _params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            panic!("sandbox path should never call tool.execute() directly")
        }
    }

    struct ObservingTool {
        output: Value,
        last_execution: Mutex<Option<(bool, Option<String>, Option<uuid::Uuid>)>>,
    }

    impl ObservingTool {
        fn new(output: Value) -> Self {
            Self {
                output,
                last_execution: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Tool for ObservingTool {
        fn name(&self) -> &str {
            "observing"
        }

        fn description(&self) -> &str {
            "observes its execution context"
        }

        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }

        async fn execute(&self, _params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            *self.last_execution.lock().await = Some((
                ctx.as_any().is::<GatewayToolContext>(),
                ctx.call_id().map(ToOwned::to_owned),
                ctx.turn_id(),
            ));
            Ok(self.output.clone())
        }
    }

    struct SecretAwareTool;

    #[async_trait]
    impl Tool for SecretAwareTool {
        fn name(&self) -> &str {
            "secret_aware"
        }

        fn description(&self) -> &str {
            "returns a host-injected secret"
        }

        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "secret": { "type": "string" }
                },
                "required": ["secret"]
            })
        }

        fn sensitive_keys(&self) -> &[&str] {
            &["secret"]
        }

        fn host_secret_keys(&self) -> &[&str] {
            &["secret"]
        }

        async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            Ok(json!({"secret": params["secret"].as_str().unwrap_or_default()}))
        }
    }

    struct RecordingStrategy {
        output: Value,
        last_call: Mutex<Option<(String, bool)>>,
    }

    impl RecordingStrategy {
        fn new(output: Value) -> Self {
            Self {
                output,
                last_call: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl ToolExecutionStrategy for RecordingStrategy {
        async fn execute(
            &self,
            _tool: &dyn Tool,
            tool_name: &str,
            _params: &Value,
            ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            *self.last_call.lock().await = Some((
                tool_name.to_string(),
                ctx.as_any().is::<GatewayToolContext>(),
            ));
            Ok(self.output.clone())
        }
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
    }

    // -- Tests ----------------------------------------------------------------

    #[tokio::test]
    async fn routes_call_through_sandbox_and_returns_output() {
        let sandbox = Arc::new(MockSandbox::new(MockResponse::Success(
            json!({"result": "ok"}),
        )));
        let executor = SandboxedExecutor::new(sandbox.clone());

        let result = executor
            .execute(&DummyTool, "echo", &json!({"text": "hi"}), &ctx())
            .await
            .expect("should succeed");

        assert_eq!(result, json!({"result": "ok"}));

        // Verify the sandbox received the correct tool name and params.
        let last = sandbox.last_call.lock().await;
        let (name, params) = last.as_ref().expect("sandbox should have been called");
        assert_eq!(name, "echo");
        assert_eq!(params, &json!({"text": "hi"}));
    }

    #[tokio::test]
    async fn tool_error_maps_to_execution_failed() {
        let sandbox = Arc::new(MockSandbox::new(MockResponse::ToolError(
            "division by zero".to_string(),
        )));
        let executor = SandboxedExecutor::new(sandbox);

        let err = executor
            .execute(&DummyTool, "calc", &json!({}), &ctx())
            .await
            .expect_err("tool error should propagate");

        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("division by zero"));
    }

    #[tokio::test]
    async fn ipc_error_maps_with_prefix() {
        let sandbox = Arc::new(MockSandbox::new(MockResponse::IpcError(
            "pipe broken".to_string(),
        )));
        let executor = SandboxedExecutor::new(sandbox);

        let err = executor
            .execute(&DummyTool, "shell", &json!({}), &ctx())
            .await
            .expect_err("IPC error should propagate");

        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(
            err.to_string()
                .contains("sandbox communication error: pipe broken"),
            "expected prefixed message, got: {err}",
        );
    }

    #[tokio::test]
    async fn other_sandbox_error_maps_to_execution_failed() {
        let sandbox = Arc::new(MockSandbox::new(MockResponse::Unavailable(
            "bwrap not installed".to_string(),
        )));
        let executor = SandboxedExecutor::new(sandbox);

        let err = executor
            .execute(&DummyTool, "shell", &json!({}), &ctx())
            .await
            .expect_err("unavailable error should propagate");

        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("bwrap not installed"));
    }

    #[tokio::test]
    async fn gateway_strategy_executes_python_on_host_with_gateway_context() {
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(RecordingStrategy::new(json!({"via": "strategy"})));
        let storage: Arc<dyn swage_storage::store::Storage> =
            Arc::new(SqliteStorage::in_memory().unwrap());
        let secret_manager = Arc::new(crate::secret::SecretManager::new(Arc::clone(&storage)));
        let executor = GatewayExecutionStrategy::new(
            Some(inner.clone()),
            ws_tx,
            None,
            secret_manager,
            storage,
        );
        let tool = ObservingTool::new(json!({"via": "tool"}));
        let turn_id =
            uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000123").expect("valid UUID");
        let mut tool_ctx = ctx();
        tool_ctx.call_id = Some("call-123".to_string());
        tool_ctx.turn_id = Some(turn_id);

        let result = executor
            .execute(
                &tool,
                "python_execute",
                &json!({"code": "print(1)"}),
                &tool_ctx,
            )
            .await
            .expect("python_execute should run on the host");

        assert_eq!(result, json!({"via": "tool"}));
        assert!(
            inner.last_call.lock().await.is_none(),
            "python_execute should bypass the inner strategy",
        );

        let observed = tool
            .last_execution
            .lock()
            .await
            .clone()
            .expect("tool should have been executed");
        assert!(
            observed.0,
            "python_execute should receive GatewayToolContext"
        );
        assert_eq!(observed.1.as_deref(), Some("call-123"));
        assert_eq!(observed.2, Some(turn_id));
    }

    #[tokio::test]
    async fn gateway_strategy_keeps_non_python_tools_on_inner_strategy() {
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(RecordingStrategy::new(json!({"via": "strategy"})));
        let storage: Arc<dyn swage_storage::store::Storage> =
            Arc::new(SqliteStorage::in_memory().unwrap());
        let secret_manager = Arc::new(crate::secret::SecretManager::new(Arc::clone(&storage)));
        let executor = GatewayExecutionStrategy::new(
            Some(inner.clone()),
            ws_tx,
            None,
            secret_manager,
            storage,
        );
        let tool = ObservingTool::new(json!({"via": "tool"}));

        let result = executor
            .execute(&tool, "shell", &json!({"command": "pwd"}), &ctx())
            .await
            .expect("non-python tools should still use the inner strategy");

        assert_eq!(result, json!({"via": "strategy"}));
        assert!(
            tool.last_execution.lock().await.is_none(),
            "tool should not be executed directly when an inner strategy is present",
        );

        let last_call = inner
            .last_call
            .lock()
            .await
            .clone()
            .expect("inner strategy should have been called");
        assert_eq!(last_call.0, "shell");
        assert!(
            last_call.1,
            "gateway tools should receive the gateway execution context",
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gateway_strategy_resolves_secret_refs_on_host_and_bypasses_inner_strategy() {
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(RecordingStrategy::new(json!({"via": "strategy"})));
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let secret_manager = Arc::new(crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        ));
        let secret = secret_manager
            .create_secret(crate::secret::CreateSecretRequest {
                name: "Tool secret".into(),
                material_kind: "plugin_secret".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "plugin".into(),
                scope_id: Some("demo".into()),
                metadata: None,
                value: Some("host-only-secret".into()),
                external_reference: None,
            })
            .await
            .unwrap();
        let executor = GatewayExecutionStrategy::new(
            Some(inner.clone()),
            ws_tx,
            None,
            secret_manager,
            storage,
        );

        let result = executor
            .execute(
                &SecretAwareTool,
                "secret_aware",
                &json!({"secret": secret.reference}),
                &ctx(),
            )
            .await
            .expect("secret-aware tool should resolve on the host");

        assert_eq!(result["secret"], "host-only-secret");
        assert!(
            inner.last_call.lock().await.is_none(),
            "secret-aware tool should bypass the inner strategy so raw secrets do not enter sandbox IPC"
        );
    }
}
