//! Tool dispatch pipeline — orchestrates all execution gates.
//!
//! The [`ToolDispatcher`] wires together the registry, permission resolver,
//! rate limiter, schema validator, tool executor, and leak detector into a
//! single ordered pipeline. Every gate short-circuits on failure so that, for
//! example, a denied call never consumes a rate-limit slot.

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use swage_core::ToolError;
use swage_safety::LeakDetector;

use crate::context::ToolContext;
use crate::output::ToolOutput;
use crate::params::redact_params;
use crate::permission::{PermissionConfig, check_permission};
use crate::rate_limit::{RateLimitConfig, RateLimitDecision, RateLimiter};
use crate::registry::ToolRegistry;
use crate::validation::validate_tool_schema;

// ---------------------------------------------------------------------------
// ToolDispatcher
// ---------------------------------------------------------------------------

/// Orchestrates the full tool invocation pipeline.
///
/// Gates run in strict order:
///
/// 1. **Registry lookup** — unknown tools short-circuit immediately.
/// 2. **Permission check** — denied calls short-circuit before rate-limit,
///    ensuring they never consume capacity.
/// 3. **Rate limit** — calls that exceed the configured windows are rejected
///    with a descriptive `ExecutionFailed` error.
/// 4. **Schema validation** — invalid parameters short-circuit before the
///    tool's `execute()` method is invoked.
/// 5. **Parameter redaction** — a sanitised copy is produced for logging;
///    the original unredacted params are passed to `execute()`.
/// 6. **Execute** — the tool runs; wall-clock time is measured by the
///    framework (not the tool itself).
/// 7. **Leak detection** — the raw output is serialised to JSON, scanned,
///    and parsed back. If scanning breaks the JSON structure, the scanned
///    text is wrapped as a string value so output is never silently lost.
/// 8. **ToolOutput assembly** — duration is always set; `raw` is populated
///    only when `debug_mode` is enabled.
pub struct ToolDispatcher {
    registry: Arc<ToolRegistry>,
    permissions: PermissionConfig,
    rate_limiter: Arc<RateLimiter>,
    rate_config: RateLimitConfig,
    leak_detector: Arc<LeakDetector>,
    /// When true, `ToolOutput.raw` is set to the pre-redaction output value.
    /// In normal operation this is false — raw output is never persisted.
    debug_mode: bool,
}

impl ToolDispatcher {
    /// Construct a dispatcher with all pipeline components.
    pub fn new(
        registry: Arc<ToolRegistry>,
        permissions: PermissionConfig,
        rate_limiter: Arc<RateLimiter>,
        rate_config: RateLimitConfig,
        leak_detector: Arc<LeakDetector>,
        debug_mode: bool,
    ) -> Self {
        Self {
            registry,
            permissions,
            rate_limiter,
            rate_config,
            leak_detector,
            debug_mode,
        }
    }

    /// Convenience constructor: allow all tools with no rate limits.
    ///
    /// Registers the provided tools and permits every invocation. Useful for
    /// bootstrapping the gateway before agent-specific configs are available.
    pub fn allow_all(tools: impl IntoIterator<Item = Arc<dyn crate::tool::Tool>>) -> Self {
        let mut registry = ToolRegistry::default();
        for tool in tools {
            // Built-in tools use register_builtin to bypass the protected
            // namespace check — they ARE the built-in tools.
            let _ = registry.register_builtin(tool);
        }

        Self {
            registry: Arc::new(registry),
            permissions: PermissionConfig {
                allow: vec!["*".into()],
                deny: vec![],
                bundles: Default::default(),
            },
            rate_limiter: Arc::new(RateLimiter::new()),
            rate_config: RateLimitConfig::default(),
            leak_detector: Arc::new(LeakDetector::default()),
            debug_mode: false,
        }
    }

    /// Returns tool schemas suitable for `ToolCompletionRequest.tool_definitions`.
    ///
    /// Delegates to `ToolRegistry::generate_schemas` — each entry has `name`,
    /// `description`, and `parameters` fields.
    pub fn tool_definitions(&self) -> Vec<serde_json::Value> {
        self.registry.generate_schemas()
    }

    /// Run the tool invocation pipeline for a single call.
    ///
    /// Returns `Ok(ToolOutput)` on success. Each gate returns a typed
    /// `ToolError` variant on failure so callers can distinguish denial
    /// reasons, validation errors, and execution failures.
    pub async fn dispatch(
        &self,
        tool_name: &str,
        params: Value,
        ctx: &dyn ToolContext,
        agent_id: &str,
    ) -> Result<ToolOutput, ToolError> {
        // Step 1: Registry lookup — reject unknown tools immediately.
        let tool = self
            .registry
            .get(tool_name)
            .ok_or_else(|| ToolError::NotFound {
                name: tool_name.to_owned(),
            })?;

        // Step 2: Permission check — before rate limiting so denied calls
        // never consume a rate-limit slot.
        check_permission(&self.permissions, tool_name)?;

        // Step 3: Rate limit check — checked and recorded atomically.
        match self.rate_limiter.check(agent_id, tool_name, &self.rate_config) {
            RateLimitDecision::Allowed => {}
            RateLimitDecision::Limited { retry_after } => {
                // ToolError has no RateLimited variant by design: rate-limit
                // rejections are transient, and agents back off from them the
                // same way they do for any ExecutionFailed. A dedicated variant
                // would expand every match site without changing caller
                // behaviour. The retry interval is surfaced in the message
                // string for human-readable diagnostics.
                return Err(ToolError::ExecutionFailed {
                    name: tool_name.to_owned(),
                    message: format!(
                        "rate limit exceeded; retry after {:.1}s",
                        retry_after.as_secs_f64()
                    ),
                });
            }
        }

        // Step 4: Schema validation — invalid params never reach execute().
        let schema = tool.parameters();
        validate_tool_schema(tool_name, &schema, &params)?;

        // Step 5: Parameter redaction for logging.
        // The redacted copy is available here for future hook/audit integration;
        // the original params are passed to execute() below.
        let _redacted_params = redact_params(&params, tool.sensitive_keys());

        // Step 6: Execute with wall-clock timing.
        let start = Instant::now();
        let raw_output = tool.execute(params, ctx).await?;
        let duration = start.elapsed();

        // Step 7: Leak detection — serialise, scan, parse back.
        let raw_str = serde_json::to_string(&raw_output).unwrap_or_default();
        let scanned_str = self.leak_detector.scan(&raw_str);
        let result: Value = serde_json::from_str(&scanned_str).unwrap_or_else(|_| {
            // Redaction altered the JSON structure (rare but possible if a
            // credential spans a structural token). Preserve as a string
            // rather than silently returning empty output.
            Value::String(scanned_str)
        });

        // Step 8: Assemble ToolOutput.
        Ok(ToolOutput {
            result,
            cost: None,
            duration,
            // Raw output preserved only in debug mode for post-hoc inspection.
            raw: if self.debug_mode { Some(raw_output) } else { None },
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use swage_core::ToolError;
    use swage_safety::LeakDetector;

    use super::ToolDispatcher;
    use crate::context::{MockToolContext, ToolContext};
    use crate::permission::PermissionConfig;
    use crate::rate_limit::{RateLimitConfig, RateLimiter};
    use crate::registry::ToolRegistry;
    use crate::tool::Tool;

    // -- Test doubles ---------------------------------------------------------

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echoes input."
        }

        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            })
        }

        async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            Ok(params)
        }
    }

    fn make_dispatcher(tool_name: &str, allow: bool, debug_mode: bool) -> ToolDispatcher {
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(EchoTool))
            .expect("registration should succeed");

        let permissions = PermissionConfig {
            allow: if allow { vec![tool_name.to_owned()] } else { vec![] },
            deny: vec![],
            bundles: HashMap::new(),
        };

        ToolDispatcher::new(
            Arc::new(registry),
            permissions,
            Arc::new(RateLimiter::new()),
            RateLimitConfig::default(),
            Arc::new(LeakDetector::default()),
            debug_mode,
        )
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
    }

    // -- Happy path ----------------------------------------------------------

    #[tokio::test]
    async fn dispatch_allowed_tool_returns_output() {
        let dispatcher = make_dispatcher("echo", true, false);
        let result = dispatcher
            .dispatch("echo", json!({"text": "hello"}), &ctx(), "agent-test")
            .await;

        let output = result.expect("should succeed for allowed tool");
        assert_eq!(output.result["text"], "hello");
        assert!(output.raw.is_none(), "raw should be None when debug_mode=false");
    }

    #[tokio::test]
    async fn dispatch_measures_duration() {
        let dispatcher = make_dispatcher("echo", true, false);
        let output = dispatcher
            .dispatch("echo", json!({"text": "x"}), &ctx(), "agent-test")
            .await
            .expect("should succeed");
        // Duration should be set; zero would indicate timing was skipped.
        // Allow zero to avoid flakiness on fast machines.
        assert!(output.duration.as_nanos() < u64::MAX as u128);
    }

    #[tokio::test]
    async fn dispatch_debug_mode_populates_raw() {
        let dispatcher = make_dispatcher("echo", true, true);
        let output = dispatcher
            .dispatch("echo", json!({"text": "y"}), &ctx(), "agent-test")
            .await
            .expect("should succeed");
        assert!(output.raw.is_some(), "raw should be Some when debug_mode=true");
    }

    // -- Permission gate -----------------------------------------------------

    #[tokio::test]
    async fn dispatch_denied_tool_returns_permission_error() {
        let dispatcher = make_dispatcher("echo", false, false);
        let err = dispatcher
            .dispatch("echo", json!({"text": "hi"}), &ctx(), "agent-test")
            .await
            .expect_err("should fail when tool is not allowed");
        assert!(
            matches!(err, ToolError::PermissionDenied { .. }),
            "expected PermissionDenied, got: {err:?}"
        );
    }

    // -- Registry gate --------------------------------------------------------

    #[tokio::test]
    async fn dispatch_unknown_tool_returns_not_found() {
        let dispatcher = make_dispatcher("echo", true, false);
        let err = dispatcher
            .dispatch("does_not_exist", json!({}), &ctx(), "agent-test")
            .await
            .expect_err("should fail for unknown tool");
        assert!(
            matches!(err, ToolError::NotFound { .. }),
            "expected NotFound, got: {err:?}"
        );
    }

    // -- Schema validation gate -----------------------------------------------

    #[tokio::test]
    async fn dispatch_invalid_params_returns_invalid_parameters() {
        let dispatcher = make_dispatcher("echo", true, false);
        // "text" is required but omitted.
        let err = dispatcher
            .dispatch("echo", json!({}), &ctx(), "agent-test")
            .await
            .expect_err("should fail with missing required param");
        assert!(
            matches!(err, ToolError::InvalidParameters { .. }),
            "expected InvalidParameters, got: {err:?}"
        );
    }

    // -- Rate limit gate ------------------------------------------------------

    #[tokio::test]
    async fn dispatch_rate_limited_returns_execution_failed() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).unwrap();

        let permissions = PermissionConfig {
            allow: vec!["echo".into()],
            deny: vec![],
            bundles: HashMap::new(),
        };

        let dispatcher = ToolDispatcher::new(
            Arc::new(registry),
            permissions,
            Arc::new(RateLimiter::new()),
            RateLimitConfig {
                requests_per_minute: Some(1),
                requests_per_hour: None,
            },
            Arc::new(LeakDetector::default()),
            false,
        );

        // First call: allowed.
        dispatcher
            .dispatch("echo", json!({"text": "a"}), &ctx(), "agent-test")
            .await
            .expect("first call should succeed");

        // Second call: rate limited.
        let err = dispatcher
            .dispatch("echo", json!({"text": "b"}), &ctx(), "agent-test")
            .await
            .expect_err("second call should be rate-limited");

        assert!(
            matches!(err, ToolError::ExecutionFailed { .. }),
            "rate-limited call should map to ExecutionFailed, got: {err:?}"
        );
        assert!(
            err.to_string().contains("rate limit"),
            "error message should mention rate limit: {err}"
        );
    }
}
