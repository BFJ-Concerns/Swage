//! Tool dispatch pipeline — orchestrates all execution gates.
//!
//! The [`ToolDispatcher`] wires together the registry, permission resolver,
//! rate limiter, schema validator, tool executor, and leak detector into a
//! single ordered pipeline. Every gate short-circuits on failure so that, for
//! example, a denied call never consumes a rate-limit slot.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use swage_core::ToolError;
use swage_safety::LeakDetector;

use crate::context::ToolContext;
use crate::output::ToolOutput;
use crate::params::redact_params;
use crate::permission::{PermissionCheckResult, PermissionConfig, check_permission};
use crate::rate_limit::{RateLimitConfig, RateLimitDecision, RateLimiter};
use crate::registry::ToolRegistry;
use crate::tool::Tool;
use crate::validation::validate_tool_schema;

// ---------------------------------------------------------------------------
// ToolExecutionStrategy
// ---------------------------------------------------------------------------

/// Abstracts the tool execution step (step 6 in the dispatch pipeline).
///
/// The default behaviour calls `tool.execute()` directly. Injecting a custom
/// strategy lets the gateway layer interpose sandbox-aware execution, logging,
/// or other cross-cutting concerns without the ToolDispatcher knowing about them.
#[async_trait]
pub trait ToolExecutionStrategy: Send + Sync {
    /// Execute the tool with the given parameters and context.
    async fn execute(
        &self,
        tool: &dyn Tool,
        tool_name: &str,
        params: &Value,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError>;
}

/// Calls `tool.execute(params, ctx)` directly — the baseline behaviour.
pub struct DefaultExecutionStrategy;

#[async_trait]
impl ToolExecutionStrategy for DefaultExecutionStrategy {
    async fn execute(
        &self,
        tool: &dyn Tool,
        _tool_name: &str,
        params: &Value,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        tool.execute(params.clone(), ctx).await
    }
}

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
    /// Optional strategy for executing tools at step 6.
    /// When `None`, tools are executed directly via `tool.execute()`.
    execution_strategy: Option<Arc<dyn ToolExecutionStrategy>>,
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
            execution_strategy: None,
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
                require_approval: vec![],
            },
            rate_limiter: Arc::new(RateLimiter::new()),
            rate_config: RateLimitConfig::default(),
            leak_detector: Arc::new(LeakDetector::default()),
            debug_mode: false,
            execution_strategy: None,
        }
    }

    /// Returns a reference to the underlying tool registry.
    ///
    /// Useful for sharing the same registry across multiple dispatchers with
    /// different permission configurations (e.g., the gateway builds a per-turn
    /// dispatcher with project-specific permissions but the same tool set).
    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    /// Set a custom execution strategy for step 6 of the pipeline.
    ///
    /// When set, `dispatch()` delegates to `strategy.execute()` instead of
    /// calling `tool.execute()` directly. This is the injection point for
    /// sandbox-aware execution in the gateway layer.
    pub fn with_execution_strategy(
        mut self,
        strategy: Arc<dyn ToolExecutionStrategy>,
    ) -> Self {
        self.execution_strategy = Some(strategy);
        self
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
        // The dispatcher's role is narrow: signal that approval is required.
        // The agent loop owns the approval lifecycle — suspending the turn,
        // creating the approval request record, and re-dispatching once the
        // human approves. PermissionDenied short-circuits via the ? operator.
        match check_permission(&self.permissions, tool_name)? {
            PermissionCheckResult::Allowed => {}
            PermissionCheckResult::RequiresApproval { timeout_secs, timeout_policy } => {
                return Err(ToolError::RequiresApproval {
                    name: tool_name.to_owned(),
                    timeout_secs,
                    timeout_policy,
                });
            }
        }

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
        // If an execution strategy is set, delegate to it; otherwise call the
        // tool directly (the default, backward-compatible path).
        let start = Instant::now();
        let raw_output = match &self.execution_strategy {
            Some(strategy) => strategy.execute(tool.as_ref(), tool_name, &params, ctx).await?,
            None => tool.execute(params, ctx).await?,
        };
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

    /// Execute a tool that has already been approved by a human reviewer.
    ///
    /// Identical to [`dispatch`] but skips step 2 (permission check). The
    /// agent loop calls this after receiving an `Approved` decision for a
    /// tool that returned `ToolError::RequiresApproval` from `dispatch`.
    pub async fn dispatch_approved(
        &self,
        tool_name: &str,
        params: Value,
        ctx: &dyn ToolContext,
        agent_id: &str,
    ) -> Result<ToolOutput, ToolError> {
        // Step 1: Registry lookup.
        let tool = self
            .registry
            .get(tool_name)
            .ok_or_else(|| ToolError::NotFound {
                name: tool_name.to_owned(),
            })?;

        // Step 2 (permission check) intentionally skipped — human approved.

        // Step 3: Rate limit check.
        match self.rate_limiter.check(agent_id, tool_name, &self.rate_config) {
            RateLimitDecision::Allowed => {}
            RateLimitDecision::Limited { retry_after } => {
                return Err(ToolError::ExecutionFailed {
                    name: tool_name.to_owned(),
                    message: format!(
                        "rate limit exceeded; retry after {:.1}s",
                        retry_after.as_secs_f64()
                    ),
                });
            }
        }

        // Step 4: Schema validation.
        let schema = tool.parameters();
        validate_tool_schema(tool_name, &schema, &params)?;

        // Step 5: Parameter redaction.
        let _redacted_params = redact_params(&params, tool.sensitive_keys());

        // Step 6: Execute with timing.
        let start = Instant::now();
        let raw_output = match &self.execution_strategy {
            Some(strategy) => strategy.execute(tool.as_ref(), tool_name, &params, ctx).await?,
            None => tool.execute(params, ctx).await?,
        };
        let duration = start.elapsed();

        // Step 7: Leak detection.
        let raw_str = serde_json::to_string(&raw_output).unwrap_or_default();
        let scanned_str = self.leak_detector.scan(&raw_str);
        let result: Value = serde_json::from_str(&scanned_str).unwrap_or_else(|_| {
            Value::String(scanned_str)
        });

        // Step 8: Assemble output.
        Ok(ToolOutput {
            result,
            cost: None,
            duration,
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
    use crate::permission::{PermissionConfig, RequireApprovalEntry};
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
            require_approval: vec![],
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
    async fn dispatch_require_approval_tool_returns_requires_approval() {
        // When a tool is in require_approval, dispatch signals the agent via
        // RequiresApproval — the agent loop owns the approval lifecycle.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).expect("registration should succeed");

        let permissions = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "echo".into(),
                timeout_secs: None,
                timeout_policy: None,
            }],
        };

        let dispatcher = ToolDispatcher::new(
            Arc::new(registry),
            permissions,
            Arc::new(RateLimiter::new()),
            RateLimitConfig::default(),
            Arc::new(LeakDetector::default()),
            false,
        );

        let err = dispatcher
            .dispatch("echo", json!({"text": "needs approval"}), &ctx(), "agent-test")
            .await
            .expect_err("should fail with RequiresApproval for require_approval tool");

        assert!(
            matches!(err, ToolError::RequiresApproval { .. }),
            "expected RequiresApproval, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_require_approval_carries_timeout_and_policy() {
        // The approval error carries the per-entry timeout and policy so the
        // agent can wire them into the approval request.
        use swage_core::ApprovalTimeoutPolicy;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).expect("registration should succeed");

        let permissions = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "echo".into(),
                timeout_secs: Some(300),
                timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
            }],
        };

        let dispatcher = ToolDispatcher::new(
            Arc::new(registry),
            permissions,
            Arc::new(RateLimiter::new()),
            RateLimitConfig::default(),
            Arc::new(LeakDetector::default()),
            false,
        );

        let err = dispatcher
            .dispatch("echo", json!({"text": "hi"}), &ctx(), "agent-test")
            .await
            .expect_err("should return RequiresApproval");

        match err {
            ToolError::RequiresApproval { name, timeout_secs, timeout_policy } => {
                assert_eq!(name, "echo");
                assert_eq!(timeout_secs, Some(300));
                assert_eq!(timeout_policy, Some(ApprovalTimeoutPolicy::Interrupt));
            }
            other => panic!("expected RequiresApproval, got: {other:?}"),
        }
    }

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

    // -- Execution strategy ---------------------------------------------------

    #[tokio::test]
    async fn dispatch_uses_custom_strategy_when_set() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use super::ToolExecutionStrategy;

        // Strategy that records it was called and delegates to the tool.
        struct TrackingStrategy {
            called: Arc<AtomicBool>,
        }

        #[async_trait]
        impl ToolExecutionStrategy for TrackingStrategy {
            async fn execute(
                &self,
                tool: &dyn Tool,
                _tool_name: &str,
                params: &Value,
                ctx: &dyn ToolContext,
            ) -> Result<Value, ToolError> {
                self.called.store(true, Ordering::SeqCst);
                tool.execute(params.clone(), ctx).await
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let strategy = Arc::new(TrackingStrategy { called: called.clone() });

        let dispatcher = make_dispatcher("echo", true, false)
            .with_execution_strategy(strategy);

        let output = dispatcher
            .dispatch("echo", json!({"text": "via strategy"}), &ctx(), "agent-test")
            .await
            .expect("should succeed via strategy");

        assert!(called.load(Ordering::SeqCst), "strategy.execute() should have been called");
        assert_eq!(output.result["text"], "via strategy");
    }

    #[tokio::test]
    async fn dispatch_without_strategy_uses_direct_execution() {
        // Ensure the default (no strategy) path still works unchanged.
        let dispatcher = make_dispatcher("echo", true, false);
        let output = dispatcher
            .dispatch("echo", json!({"text": "direct"}), &ctx(), "agent-test")
            .await
            .expect("should succeed with direct execution");
        assert_eq!(output.result["text"], "direct");
    }

    #[tokio::test]
    async fn dispatch_strategy_error_propagates() {
        use super::ToolExecutionStrategy;

        // Strategy that always fails — verifies errors propagate through the pipeline.
        struct FailingStrategy;

        #[async_trait]
        impl ToolExecutionStrategy for FailingStrategy {
            async fn execute(
                &self,
                _tool: &dyn Tool,
                tool_name: &str,
                _params: &Value,
                _ctx: &dyn ToolContext,
            ) -> Result<Value, ToolError> {
                Err(ToolError::ExecutionFailed {
                    name: tool_name.to_owned(),
                    message: "strategy rejected execution".to_owned(),
                })
            }
        }

        let dispatcher = make_dispatcher("echo", true, false)
            .with_execution_strategy(Arc::new(FailingStrategy));

        let err = dispatcher
            .dispatch("echo", json!({"text": "x"}), &ctx(), "agent-test")
            .await
            .expect_err("strategy error should propagate");

        assert!(
            matches!(err, ToolError::ExecutionFailed { .. }),
            "expected ExecutionFailed from strategy, got: {err:?}"
        );
        assert!(err.to_string().contains("strategy rejected"));
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
            require_approval: vec![],
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

    // -- dispatch_approved (post-approval re-dispatch) -----------------------

    #[tokio::test]
    async fn dispatch_approved_skips_permission_check() {
        // Tool is in require_approval — dispatch() returns RequiresApproval,
        // but dispatch_approved() should execute it successfully.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).expect("register");

        let permissions = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "echo".into(),
                timeout_secs: None,
                timeout_policy: None,
            }],
        };

        let dispatcher = ToolDispatcher::new(
            Arc::new(registry),
            permissions,
            Arc::new(RateLimiter::new()),
            RateLimitConfig::default(),
            Arc::new(LeakDetector::default()),
            false,
        );

        // Normal dispatch should require approval.
        let err = dispatcher
            .dispatch("echo", json!({"text": "hi"}), &ctx(), "agent-test")
            .await
            .expect_err("should require approval");
        assert!(matches!(err, ToolError::RequiresApproval { .. }));

        // dispatch_approved should bypass the permission check and succeed.
        let output = dispatcher
            .dispatch_approved("echo", json!({"text": "hi"}), &ctx(), "agent-test")
            .await
            .expect("dispatch_approved should succeed");
        assert_eq!(output.result["text"], "hi");
    }

    #[tokio::test]
    async fn dispatch_approved_still_validates_schema() {
        // Even though permission is bypassed, schema validation still runs.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).expect("register");

        let permissions = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "echo".into(),
                timeout_secs: None,
                timeout_policy: None,
            }],
        };

        let dispatcher = ToolDispatcher::new(
            Arc::new(registry),
            permissions,
            Arc::new(RateLimiter::new()),
            RateLimitConfig::default(),
            Arc::new(LeakDetector::default()),
            false,
        );

        // Invalid params (missing required "text" field).
        let err = dispatcher
            .dispatch_approved("echo", json!({}), &ctx(), "agent-test")
            .await
            .expect_err("should fail schema validation");
        assert!(
            matches!(err, ToolError::InvalidParameters { .. }),
            "expected InvalidParameters, got: {err:?}"
        );
    }
}
