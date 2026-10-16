//! Tool trait — the contract for all tool implementations.

use async_trait::async_trait;
use serde_json::Value;
use swage_core::{ToolDefinition, ToolError};

use crate::context::ToolContext;

/// The contract every tool must satisfy.
///
/// Tools are Send + Sync so they can be held in a shared registry and invoked
/// from async workers. The framework calls `execute` after permission checks,
/// rate limiting, and schema validation; tools receive already-validated params.
///
/// # Implementing a tool
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use serde_json::{Value, json};
/// use swage_core::ToolError;
/// use swage_tool::{context::ToolContext, tool::Tool};
///
/// struct EchoTool;
///
/// #[async_trait]
/// impl Tool for EchoTool {
///     fn name(&self) -> &str { "echo" }
///     fn description(&self) -> &str { "Returns the input unchanged." }
///     fn parameters(&self) -> Value {
///         json!({"type": "object", "properties": {"text": {"type": "string"}}})
///     }
///     async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
///         Ok(params)
///     }
/// }
/// ```
/// Declarative rendering hints a tool carries about its own calls.
///
/// Renderers consume these instead of keeping per-tool-name heuristics: a
/// new tool that declares hints renders sensibly on every surface without
/// any renderer growing a matching branch, and a tool that declares none
/// gets each surface's generic fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolDisplayHints {
    /// The parameter whose value is the call's one-line headline — the
    /// command for a shell tool, the path for a file tool, the query for a
    /// search tool. The framework reads it from the call's arguments at
    /// execution time; a missing or non-scalar value falls back to the
    /// generic rendering rather than erroring.
    pub summary_param: Option<&'static str>,
}

/// Derive the one-line display summary for a call from its tool's hints and
/// the call's arguments. `None` means "no hint applies" — renderers fall
/// back to their generic rendering, never to a per-tool branch.
pub fn display_summary(hints: &ToolDisplayHints, arguments: &Value) -> Option<String> {
    let param = hints.summary_param?;
    match arguments.get(param)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name used to look up this tool in the registry.
    fn name(&self) -> &str;

    /// Human-readable description shown to the model.
    fn description(&self) -> &str;

    /// Rendering hints for this tool's calls. Defaults to none — override
    /// where one argument is the call's natural headline.
    fn display_hints(&self) -> ToolDisplayHints {
        ToolDisplayHints::default()
    }

    /// JSON Schema object describing the parameters this tool accepts.
    fn parameters(&self) -> Value;

    /// Parameter keys whose values should be redacted in logs and hook output.
    ///
    /// Defaults to an empty slice — override for tools that accept secrets,
    /// tokens, or other sensitive data.
    fn sensitive_keys(&self) -> &[&str] {
        &[]
    }

    /// Parameter keys whose values may be secret references.
    ///
    /// The gateway resolves these references at the host boundary before
    /// execution. Tools that opt in here can receive injected raw secrets
    /// without exposing them to the model or sandbox IPC by default.
    fn device_secret_keys(&self) -> &[&str] {
        &[]
    }

    /// Whether the tool must execute in the gateway process instead of the
    /// sandbox child.
    ///
    /// Tools should opt in when they need gateway-only state, host networking,
    /// or typed errors that the sandbox IPC layer cannot preserve.
    fn requires_device_execution(&self) -> bool {
        false
    }

    /// The one platform agent that may hold this tool, by agent id — `None`
    /// for a tool any agent may be granted. A held tool is withheld from
    /// every other agent before permissions are consulted, so no grant, not
    /// even a wildcard, offers it elsewhere.
    fn platform_holder(&self) -> Option<&'static str> {
        None
    }

    /// Classify a structured response whose transport succeeded.
    fn execution_outcome(
        &self,
        _result: &Value,
    ) -> swage_protocol::client::conversation::ToolExecution {
        swage_protocol::client::conversation::ToolExecution::succeeded()
    }

    /// Check arguments before any work. An error here records a refusal
    /// without execution. Implementations must perform no I/O in this hook.
    fn validate_call(&self, _params: &Value) -> Result<(), ToolError> {
        Ok(())
    }

    /// Execute the tool with already-validated parameters.
    ///
    /// Returns the raw output `Value`; the framework wraps it into a
    /// `ToolOutput` (measuring wall-clock time, running leak detection, etc.).
    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError>;

    /// Build a `ToolDefinition` from this tool's metadata.
    ///
    /// Convenience for registering tools with the provider layer. The
    /// `input_schema` field maps directly to `parameters()`.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_owned(),
            description: self.description().to_owned(),
            input_schema: self.parameters(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use swage_core::ToolError;

    use crate::context::MockToolContext;

    /// Minimal tool used as a test double.
    struct AddTool;

    #[async_trait]
    impl Tool for AddTool {
        fn name(&self) -> &str {
            "add"
        }

        fn description(&self) -> &str {
            "Adds two numbers."
        }

        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["a", "b"]
            })
        }

        async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            let a = params["a"]
                .as_f64()
                .ok_or_else(|| ToolError::InvalidParameters {
                    name: self.name().to_owned(),
                    message: "a must be a number".to_owned(),
                })?;
            let b = params["b"]
                .as_f64()
                .ok_or_else(|| ToolError::InvalidParameters {
                    name: self.name().to_owned(),
                    message: "b must be a number".to_owned(),
                })?;
            Ok(json!(a + b))
        }
    }

    /// Tool that always returns an error — verifies ToolError variants flow through.
    struct BrokenTool;

    #[async_trait]
    impl Tool for BrokenTool {
        fn name(&self) -> &str {
            "broken"
        }

        fn description(&self) -> &str {
            "Always fails."
        }

        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }

        fn sensitive_keys(&self) -> &[&str] {
            &["secret", "token"]
        }

        async fn execute(
            &self,
            _params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: "intentional failure".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn tool_executes_with_mock_context() {
        let tool = AddTool;
        let ctx = MockToolContext::new("agent-1", "sess-1");
        let result = tool.execute(json!({"a": 3, "b": 4}), &ctx).await;
        assert_eq!(result.unwrap(), json!(7.0));
    }

    #[tokio::test]
    async fn tool_receives_context_identity() {
        // Implement a tool that reads the agent_id from context.
        struct IdentityTool;

        #[async_trait]
        impl Tool for IdentityTool {
            fn name(&self) -> &str {
                "identity"
            }

            fn description(&self) -> &str {
                "Returns the calling agent's ID."
            }

            fn parameters(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }

            async fn execute(
                &self,
                _params: Value,
                ctx: &dyn ToolContext,
            ) -> Result<Value, ToolError> {
                Ok(json!(ctx.agent_id()))
            }
        }

        let tool = IdentityTool;
        let ctx = MockToolContext::new("agent-xyz", "sess-abc");
        let result = tool.execute(json!({}), &ctx).await.unwrap();
        assert_eq!(result, json!("agent-xyz"));
    }

    #[tokio::test]
    async fn tool_returns_tool_error_on_failure() {
        let tool = BrokenTool;
        let ctx = MockToolContext::new("agent-1", "sess-1");
        let err = tool.execute(json!({}), &ctx).await.unwrap_err();

        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("broken"));
    }

    #[test]
    fn tool_definition_built_from_metadata() {
        let tool = AddTool;
        let def = tool.definition();

        assert_eq!(def.name, "add");
        assert_eq!(def.description, "Adds two numbers.");
        assert_eq!(def.input_schema["type"], "object");
        assert!(def.input_schema["properties"]["a"].is_object());
    }

    #[test]
    fn sensitive_keys_default_is_empty() {
        let tool = AddTool;
        assert!(tool.sensitive_keys().is_empty());
    }

    #[test]
    fn display_hints_default_to_none_and_summary_reads_the_named_param() {
        assert_eq!(AddTool.display_hints(), ToolDisplayHints::default());
        assert_eq!(
            display_summary(&ToolDisplayHints::default(), &json!({"a": 1})),
            None
        );

        let hints = ToolDisplayHints {
            summary_param: Some("command"),
        };
        assert_eq!(
            display_summary(&hints, &json!({"command": "ls -la"})).as_deref(),
            Some("ls -la")
        );
        // Missing or non-scalar values fall back to the generic rendering.
        assert_eq!(display_summary(&hints, &json!({"other": true})), None);
        assert_eq!(display_summary(&hints, &json!({"command": ["ls"]})), None);
    }

    #[test]
    fn sensitive_keys_overridden_when_specified() {
        let tool = BrokenTool;
        let keys = tool.sensitive_keys();
        assert_eq!(keys, &["secret", "token"]);
    }

    #[tokio::test]
    async fn tool_invalid_params_returns_error() {
        let tool = AddTool;
        let ctx = MockToolContext::new("agent-1", "sess-1");
        // Pass string where number expected
        let err = tool
            .execute(json!({"a": "not-a-number", "b": 1}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }
}
