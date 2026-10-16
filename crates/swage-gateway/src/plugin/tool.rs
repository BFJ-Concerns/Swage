//! A plugin's tool as the dispatcher sees it, and the host its handler
//! reaches through.
//!
//! `PluginTool` is an ordinary `Tool`: the dispatcher looks it up, checks the
//! session's permissions, validates its parameters and records its call like
//! any built-in's. Its `execute` hands the call to the plugin's isolate and
//! supplies the host for that one call: a `callTool` from the script runs
//! through the turn's own dispatcher — the same registry, permissions and
//! execution strategy the model's calls meet — as a nested call recorded
//! beneath the plugin's, so an effect a plugin raises is gated exactly as
//! one the model raised.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use swage_core::ToolError;
use swage_plugin::{
    HostCallFailure, InvocationContext, PluginError, PluginHost, PluginRuntime, ToolDeclaration,
};
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
    runtime: Arc<PluginRuntime>,
}

impl PluginTool {
    pub fn new(
        plugin: String,
        declaration: ToolDeclaration,
        calls_tools: bool,
        runtime: Arc<PluginRuntime>,
    ) -> Self {
        Self {
            plugin,
            declaration,
            calls_tools,
            runtime,
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
        let host = Arc::new(GatewayPluginHost {
            plugin: self.plugin.clone(),
            nested,
            provides_tool: gateway_ctx
                .app_state()
                .map(|state| Arc::clone(&state.plugins)),
        });
        let context = InvocationContext {
            session_id: ctx.session_id().to_owned(),
            agent_id: ctx.agent_id().to_owned(),
            call_id: Some(call_id.clone()),
            working_directory: ctx.working_directory().display().to_string(),
        };
        match self
            .runtime
            .invoke_tool(&self.declaration.name, &params, &context, host)
            .await
        {
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
    provides_tool: Option<Arc<super::PluginService>>,
}

#[async_trait]
impl PluginHost for GatewayPluginHost {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, HostCallFailure> {
        if self
            .provides_tool
            .as_ref()
            .is_some_and(|plugins| plugins.provides_tool(name))
        {
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
