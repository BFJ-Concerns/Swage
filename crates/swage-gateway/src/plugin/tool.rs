//! A plugin's tool as the dispatcher sees it, and the host its handler
//! reaches through.
//!
//! `PluginTool` is an ordinary `Tool`: the dispatcher looks it up, checks the
//! session's permissions, validates its parameters and records its call like
//! any built-in's. Its `execute` hands the call to the plugin's isolate — or
//! to the session's brokered process, started on this first call where the
//! turn places it — and supplies the host for that one call: a `callTool`
//! from the plugin runs
//! through the turn's own dispatcher — the same registry, permissions and
//! execution strategy the model's calls meet — as a nested call recorded
//! beneath the plugin's, so an effect a plugin raises is gated exactly as
//! one the model raised.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use swage_core::ToolError;
use swage_plugin::{HostCallFailure, InvocationContext, PluginError, PluginHost, ToolDeclaration};
use swage_python::NestedToolHost;
use swage_storage::model::NewAuditEvent;
use swage_tool::context::{ToolContext, serialise_tool_context};
use uuid::Uuid;

use crate::tool::Tool;
use crate::tool::nested_tool_runtime::NestedToolRuntime;

pub struct PluginTool {
    plugin: String,
    declaration: ToolDeclaration,
    /// Whether the plugin holds the `tools` grant, so a call needs the
    /// turn's dispatcher to answer it.
    calls_tools: bool,
    execution: super::Execution,
}

impl PluginTool {
    pub fn new(
        plugin: String,
        declaration: ToolDeclaration,
        calls_tools: bool,
        execution: super::Execution,
    ) -> Self {
        Self {
            plugin,
            declaration,
            calls_tools,
            execution,
        }
    }

    pub fn plugin(&self) -> &str {
        &self.plugin
    }
}

#[async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.declaration.name
    }

    fn description(&self) -> &str {
        &self.declaration.description
    }

    fn parameters(&self) -> Value {
        self.declaration.parameters.clone()
    }

    /// The isolate lives in the gateway process, so the call never goes to
    /// the sandbox child; what the handler asks of the session runs where
    /// that tool would. A session bound to a remote device sends every tool
    /// to the device before this flag is read — the device model's rule,
    /// which this wrapper does not change.
    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let failed = |message: String| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message,
        };
        let gateway_ctx = super::super::tool::require_gateway_context(
            ctx,
            self.name(),
            "a plugin tool runs inside a gateway turn",
        )?;
        let call_id = ctx
            .call_id()
            .ok_or_else(|| failed("a plugin tool call needs a tool call ID".to_owned()))?
            .to_owned();
        let nested = if self.calls_tools {
            let turn_id = ctx
                .turn_id()
                .ok_or_else(|| failed("a plugin tool call needs its turn".to_owned()))?;
            let dispatcher = gateway_ctx.session_dispatcher().ok_or_else(|| {
                failed(
                    "the session's tool dispatcher is not reachable from this call, so the plugin's tool calls cannot be gated"
                        .to_owned(),
                )
            })?;
            Some(NestedToolRuntime::new(
                Arc::clone(dispatcher),
                gateway_ctx.ws_tx().clone(),
                Arc::clone(gateway_ctx.storage()),
                turn_id,
                call_id.clone(),
                serialise_tool_context(ctx),
            ))
        } else {
            None
        };
        let plugins = gateway_ctx
            .app_state()
            .map(|state| Arc::clone(&state.plugins))
            .ok_or_else(|| {
                failed(
                    "the installed plugins are not reachable from this call, so a plugin's call cannot be checked against them"
                        .to_owned(),
                )
            })?;
        let host = Arc::new(GatewayPluginHost {
            plugin: self.plugin.clone(),
            nested,
            plugins,
        });
        let context = InvocationContext {
            session_id: ctx.session_id().to_owned(),
            agent_id: ctx.agent_id().to_owned(),
            call_id: Some(call_id.clone()),
            working_directory: ctx.working_directory().display().to_string(),
        };
        let outcome = match &self.execution {
            super::Execution::Isolate(runtime) => {
                runtime
                    .invoke_tool(&self.declaration.name, &params, &context, host)
                    .await
            }
            super::Execution::Process(plugin) => {
                // The process runs where the turn places it; without a
                // placement there is no boundary to run it under.
                let placement = gateway_ctx.process_placement().ok_or_else(|| {
                    failed(
                        "the turn's process placement is not reachable from this call, so the plugin's process cannot be started"
                            .to_owned(),
                    )
                })?;
                let launcher = super::process::GatewayProcessLauncher::new(Arc::clone(placement));
                plugin
                    .invoke_tool(&self.declaration.name, &params, &context, host, &launcher)
                    .await
            }
        };
        match outcome {
            Ok(value) => Ok(value),
            Err(PluginError::Terminated(termination)) => {
                let session_id = ctx.session_id().parse::<Uuid>().ok();
                if let Err(error) = gateway_ctx
                    .storage()
                    .create_audit_event(NewAuditEvent {
                        id: Uuid::new_v4().to_string(),
                        session_id,
                        entity_kind: "plugin".to_owned(),
                        entity_id: self.plugin.clone(),
                        event_kind: "plugin_terminated".to_owned(),
                        summary: format!(
                            "Plugin {} was terminated serving {}: {}",
                            self.plugin,
                            self.name(),
                            termination.reason()
                        ),
                        metadata: serde_json::json!({
                            "reason": termination.reason(),
                            "tool": self.name(),
                            "call_id": call_id,
                        }),
                    })
                    .await
                {
                    tracing::warn!(plugin = %self.plugin, %error, "plugin termination could not be recorded");
                }
                Err(failed(format!(
                    "{termination}; the plugin starts afresh on its next call"
                )))
            }
            Err(error) => Err(failed(error.to_string())),
        }
    }
}

/// The host for one plugin call: the session's tools through the turn's
/// dispatcher, and the gateway's log.
struct GatewayPluginHost {
    plugin: String,
    nested: Option<NestedToolRuntime>,
    /// The installed plugins, to refuse a call to another plugin's tool —
    /// a check that must be made, so the host is not built without it.
    plugins: Arc<super::PluginService>,
}

#[async_trait]
impl PluginHost for GatewayPluginHost {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, HostCallFailure> {
        if self.plugins.provides_tool(name) {
            return Err(HostCallFailure {
                name: "HostCapabilityError".to_owned(),
                message: format!(
                    "'{name}' is a plugin's tool; a plugin's tools are the model's to call, not another plugin's"
                ),
            });
        }
        let Some(nested) = &self.nested else {
            return Err(HostCallFailure {
                name: "HostCapabilityError".to_owned(),
                message: "this plugin was not granted 'tools'".to_owned(),
            });
        };
        nested
            .dispatch_tool(name, arguments)
            .await
            .map_err(|error| HostCallFailure {
                name: "ToolError".to_owned(),
                message: error.message,
            })
    }

    fn log(&self, message: &str) {
        tracing::info!(plugin = %self.plugin, "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::tool::ToolDispatcher;
    use crate::tool_executor::GatewayExecutionStrategy;
    use swage_plugin::PluginManifest;
    use swage_safety::LeakDetector;
    use swage_storage::SqliteStorage;
    use swage_storage::model::{NewThread, NewTurn, TurnState};
    use swage_storage::store::{SessionStore, Storage, ThreadStore, TopicStore, TurnStore};
    use swage_tool::context::MockToolContext;
    use swage_tool::dispatch::ToolExecutionStrategy;
    use swage_tool::permission::PermissionConfig;
    use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};
    use swage_tool::registry::ToolRegistry;

    /// A session tool that reports it ran.
    struct Named(&'static str);

    #[async_trait]
    impl Tool for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "a session tool"
        }
        fn parameters(&self) -> Value {
            serde_json::json!({ "type": "object" })
        }
        async fn execute(&self, _: Value, _: &dyn ToolContext) -> Result<Value, ToolError> {
            Ok(serde_json::json!({ "ran": self.0 }))
        }
    }

    /// A turn to record the nested calls beneath: topic, session, thread, turn.
    async fn turn_in_a_session(storage: &SqliteStorage) -> (Uuid, Uuid) {
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .unwrap();
        let thread = ThreadStore::create(
            storage,
            NewThread {
                session_id: session.id,
                agent_id: session.agent_id.clone(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: session.agent_id.clone(),
                state: TurnState::Processing,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        (session.id, turn.id)
    }

    /// The effect boundary end to end: a plugin's `callTool` runs through
    /// the session dispatcher the turn carries, so the session's deny list
    /// applies to it, and another plugin's tool is refused before dispatch.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_call_through_the_session_dispatcher_meets_the_session_gates() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let (session_id, turn_id) = turn_in_a_session(&storage).await;
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(Vec::new())),
            swage_config::ResolvedConfig::default(),
            ToolDispatcher::allow_all(Vec::new()),
            None,
            dir.path().to_path_buf(),
        );

        // Another plugin, installed, so its tool is one a plugin may not call.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("plugin.toml"),
            r#"
            name = "other"
            version = "1"
            description = "Another plugin."
            script = "plugin.js"
            [[provides.tools]]
            name = "other_tool"
            description = "The other plugin's tool."
            parameters = { type = "object" }
            "#,
        )
        .unwrap();
        std::fs::write(
            other.join("plugin.js"),
            r#"swage.provide.tool("other_tool", () => 1);"#,
        )
        .unwrap();
        state
            .plugins
            .install(&other, &[], None)
            .await
            .expect("installs");

        let manifest = PluginManifest::parse(
            r#"
            name = "probe"
            version = "1"
            description = "Probes the boundary."
            script = "plugin.js"
            grants = ["tools"]
            [[provides.tools]]
            name = "probe"
            description = "Probe."
            parameters = { type = "object" }
            "#,
        )
        .unwrap();
        let runtime = swage_plugin::PluginRuntime::start(
            manifest.clone(),
            r#"swage.provide.tool("probe", async () => {
                const ran = await swage.callTool("probe_shell", {});
                let denied = null;
                try { await swage.callTool("probe_denied", {}); } catch (error) { denied = error.name + ": " + error.message; }
                let other = null;
                try { await swage.callTool("other_tool", {}); } catch (error) { other = error.name + ": " + error.message; }
                return { ran, denied, other };
            });"#
            .to_owned(),
        )
        .await
        .expect("starts");
        let probe = PluginTool::new(
            "probe".into(),
            manifest.provides.tools[0].clone(),
            true,
            super::super::Execution::Isolate(runtime),
        );

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(Named("probe_shell"))).unwrap();
        registry.register(Arc::new(Named("probe_denied"))).unwrap();
        for tool in state.plugins.tools() {
            registry.register(tool).unwrap();
        }
        let permissions = PermissionConfig {
            allow: vec!["*".into()],
            deny: vec!["probe_denied".into()],
            ..PermissionConfig::default()
        };
        let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let secret_manager = Arc::new(crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn Storage>
        ));
        let session_dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(registry),
                permissions,
                Arc::new(RateLimiter::new()),
                RateLimitConfig::default(),
                Arc::new(LeakDetector::default()),
                false,
            )
            .with_execution_strategy(Arc::new(GatewayExecutionStrategy::new(
                None,
                ws_tx.clone(),
                None,
                Arc::clone(&secret_manager),
                Arc::clone(&storage) as Arc<dyn Storage>,
                Some(state.clone()),
            ))),
        );
        let turn_strategy = GatewayExecutionStrategy::new(
            None,
            ws_tx,
            None,
            secret_manager,
            Arc::clone(&storage) as Arc<dyn Storage>,
            Some(state.clone()),
        )
        .with_session_dispatcher(session_dispatcher);
        let mut ctx = MockToolContext::new("agent-a", session_id.to_string());
        ctx.call_id = Some("call-probe".into());
        ctx.turn_id = Some(turn_id);

        let result = turn_strategy
            .execute(&probe, "probe", &serde_json::json!({}), &ctx)
            .await
            .expect("the plugin's call is served");
        assert_eq!(result["ran"], serde_json::json!({ "ran": "probe_shell" }));
        let denied = result["denied"]
            .as_str()
            .expect("the denied call is a catchable error");
        assert!(denied.contains("probe_denied"), "{denied}");
        let other = result["other"]
            .as_str()
            .expect("another plugin's tool is refused");
        assert!(other.contains("another plugin's"), "{other}");
    }

    /// Without the session dispatcher nothing can gate the plugin's calls,
    /// so a plugin that may call tools is not served at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tools_plugin_is_refused_when_no_session_dispatcher_can_gate_it() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let (session_id, turn_id) = turn_in_a_session(&storage).await;
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(Vec::new())),
            swage_config::ResolvedConfig::default(),
            ToolDispatcher::allow_all(Vec::new()),
            None,
            dir.path().to_path_buf(),
        );
        let manifest = PluginManifest::parse(
            r#"
            name = "probe"
            version = "1"
            description = "Probes the boundary."
            script = "plugin.js"
            grants = ["tools"]
            [[provides.tools]]
            name = "probe"
            description = "Probe."
            parameters = { type = "object" }
            "#,
        )
        .unwrap();
        let runtime = swage_plugin::PluginRuntime::start(
            manifest.clone(),
            r#"swage.provide.tool("probe", async () => swage.callTool("probe_shell", {}));"#
                .to_owned(),
        )
        .await
        .expect("starts");
        let probe = PluginTool::new(
            "probe".into(),
            manifest.provides.tools[0].clone(),
            true,
            super::super::Execution::Isolate(runtime),
        );
        let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let strategy = GatewayExecutionStrategy::new(
            None,
            ws_tx,
            None,
            Arc::new(crate::secret::SecretManager::new(
                Arc::clone(&storage) as Arc<dyn Storage>
            )),
            Arc::clone(&storage) as Arc<dyn Storage>,
            Some(state),
        );
        let mut ctx = MockToolContext::new("agent-a", session_id.to_string());
        ctx.call_id = Some("call-probe".into());
        ctx.turn_id = Some(turn_id);
        let error = strategy
            .execute(&probe, "probe", &serde_json::json!({}), &ctx)
            .await
            .expect_err("refused");
        assert!(
            error.to_string().contains("session's tool dispatcher"),
            "{error}"
        );
    }
}
