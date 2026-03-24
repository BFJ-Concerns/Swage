//! Cross-task integration tests for the tool system dispatch pipeline.
//!
//! These tests verify the complete invocation pipeline: a developer-defined tool
//! registered in a ToolRegistry, invoked through the dispatch pipeline with all
//! gates enforced (permission, rate limit, schema validation, execution, leak
//! detection, ToolOutput assembly).
//!
//! Spans motes: [dzjr] ToolContext, [acrt] Tool trait, [1vp7] ToolOutput,
//! [qgiy] ToolRegistry, [2rz3] PermissionResolver, [7uig] RateLimiter,
//! [9i98] Schema validation, [oym1] Dispatch pipeline, [3gqi] PermissionDenied reason.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use swage_core::ToolError;
use swage_safety::LeakDetector;
use swage_tool::context::MockToolContext;
use swage_tool::dispatch::ToolDispatcher;
use swage_tool::output::ToolOutput;
use swage_tool::permission::PermissionConfig;
use swage_tool::rate_limit::{RateLimitConfig, RateLimitDecision, RateLimiter};
use swage_tool::registry::ToolRegistry;
use swage_tool::tool::Tool;

// ---------------------------------------------------------------------------
// Test tools
// ---------------------------------------------------------------------------

/// A tool that echoes its input and appends a credential-like string.
/// Used to verify leak detection catches credential patterns in output.
struct LeakyEchoTool;

#[async_trait]
impl Tool for LeakyEchoTool {
    fn name(&self) -> &str {
        "leaky_echo"
    }

    fn description(&self) -> &str {
        "Echoes input and appends a fake credential for testing leak detection"
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

    async fn execute(
        &self,
        params: Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, ToolError> {
        let text = params["text"].as_str().unwrap_or("");
        // Include a fake API key that should be caught by leak detection.
        Ok(json!({
            "output": format!("{text}\nkey: sk-TESTKEY1234567890abcdefghij")
        }))
    }
}

/// A tool that declares a sensitive parameter key.
struct SensitiveTool;

#[async_trait]
impl Tool for SensitiveTool {
    fn name(&self) -> &str {
        "sensitive_tool"
    }

    fn description(&self) -> &str {
        "A tool with a sensitive parameter for testing redaction"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "api_key": { "type": "string" }
            },
            "required": ["query"]
        })
    }

    fn sensitive_keys(&self) -> &[&str] {
        &["api_key"]
    }

    async fn execute(
        &self,
        params: Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, ToolError> {
        let query = params["query"].as_str().unwrap_or("");
        Ok(json!({ "result": format!("searched: {query}") }))
    }
}

/// A minimal tool that always succeeds — used to test rate limiting by
/// invoking repeatedly without caring about the output content.
struct CounterTool;

#[async_trait]
impl Tool for CounterTool {
    fn name(&self) -> &str {
        "counter"
    }

    fn description(&self) -> &str {
        "Always succeeds with a counter value"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
        })
    }

    async fn execute(
        &self,
        _params: Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, ToolError> {
        Ok(json!({ "count": 1 }))
    }
}

// ---------------------------------------------------------------------------
// Helper: build a test context
// ---------------------------------------------------------------------------

fn test_context() -> MockToolContext {
    MockToolContext {
        agent_id: "test-agent".into(),
        session_id: "test-session".into(),
        working_directory: PathBuf::from("/tmp/test"),
        allowed_paths: vec![PathBuf::from("/tmp/test")],
        user_info: None,
    }
}

// ---------------------------------------------------------------------------
// End-to-end pipeline: allowed tool with leak detection
// ---------------------------------------------------------------------------

/// Proves Core Journey: tool dispatch pipeline composes into a working whole.
///
/// Exercises: registry lookup → permission allow → rate limit pass →
/// schema validation → execute → leak detection → ToolOutput assembly.
///
/// Cross-task: requires ToolRegistry [qgiy], PermissionResolver [2rz3],
/// RateLimiter [7uig], schema validation [9i98], dispatch pipeline [oym1],
/// Tool trait [acrt], ToolContext [dzjr], ToolOutput [1vp7].
#[tokio::test]
async fn dispatch_pipeline_end_to_end_with_leak_detection() {
    // -- Setup: register tool, configure permissions, create dispatcher --
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(LeakyEchoTool))
        .expect("registration should succeed");

    let permissions = PermissionConfig {
        allow: vec!["leaky_echo".into()],
        deny: vec![],
        bundles: HashMap::new(),
        require_approval: vec![],
    };

    let rate_limiter = RateLimiter::new();
    let rate_config = RateLimitConfig {
        requests_per_minute: None,
        requests_per_hour: None,
    };
    let leak_detector = LeakDetector::default();

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        Arc::new(rate_limiter),
        rate_config,
        Arc::new(leak_detector),
        false, // debug_mode off — raw should be None
    );

    let ctx = test_context();

    // -- Invoke through the full pipeline --
    let result = dispatcher
        .dispatch(
            "leaky_echo",
            json!({ "text": "hello world" }),
            &ctx,
            "test-agent",
        )
        .await;

    let output = result.expect("dispatch should succeed for allowed tool");

    // -- Verify ToolOutput assembly --
    // Duration should be non-zero (framework measured execution time).
    assert!(
        !output.duration.is_zero(),
        "framework should measure non-zero execution duration"
    );

    // Result should have the credential redacted by leak detection.
    let result_str = output.result.to_string();
    assert!(
        result_str.contains("hello world"),
        "output should contain the echoed text"
    );
    assert!(
        !result_str.contains("sk-TESTKEY1234567890"),
        "leak detection should have redacted the API key from result"
    );
    assert!(
        result_str.contains("[REDACTED:"),
        "result should contain redaction marker"
    );

    // Raw should be None when debug_mode is off.
    assert!(
        output.raw.is_none(),
        "raw should be None when debug mode is disabled"
    );
}

// ---------------------------------------------------------------------------
// End-to-end pipeline: debug mode preserves raw output
// ---------------------------------------------------------------------------

/// Verifies ToolOutput.raw preserves the original unredacted output when
/// debug_mode is enabled — proving leak detection and ToolOutput assembly
/// handle the two-view pattern correctly.
#[tokio::test]
async fn dispatch_pipeline_debug_mode_preserves_raw_output() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(LeakyEchoTool))
        .expect("registration should succeed");

    let permissions = PermissionConfig {
        allow: vec!["leaky_echo".into()],
        deny: vec![],
        bundles: HashMap::new(),
        require_approval: vec![],
    };

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig {
            requests_per_minute: None,
            requests_per_hour: None,
        },
        Arc::new(LeakDetector::default()),
        true, // debug_mode ON
    );

    let ctx = test_context();
    let output = dispatcher
        .dispatch(
            "leaky_echo",
            json!({ "text": "debug test" }),
            &ctx,
            "test-agent",
        )
        .await
        .expect("dispatch should succeed");

    // Raw should be Some and contain the original unredacted output.
    let raw = output.raw.expect("raw should be populated in debug mode");
    let raw_str = raw.to_string();
    assert!(
        raw_str.contains("sk-TESTKEY1234567890"),
        "raw output should preserve the unredacted API key"
    );

    // Result should still be redacted.
    let result_str = output.result.to_string();
    assert!(
        !result_str.contains("sk-TESTKEY1234567890"),
        "result should have the API key redacted"
    );
}

// ---------------------------------------------------------------------------
// Permission denial short-circuits the pipeline
// ---------------------------------------------------------------------------

/// Proves Delivery Closure: permission enforcement produces PermissionDenied
/// with a structured reason when a tool is explicitly denied.
///
/// Cross-task: requires PermissionResolver [2rz3], dispatch pipeline [oym1],
/// PermissionDenied reason [3gqi].
#[tokio::test]
async fn dispatch_denies_explicitly_denied_tool() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(CounterTool))
        .expect("registration should succeed");

    // Allow wildcard, but explicitly deny "counter".
    let permissions = PermissionConfig {
        allow: vec!["*".into()],
        deny: vec!["counter".into()],
        bundles: HashMap::new(),
        require_approval: vec![],
    };

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig {
            requests_per_minute: None,
            requests_per_hour: None,
        },
        Arc::new(LeakDetector::default()),
        false,
    );

    let ctx = test_context();
    let result = dispatcher
        .dispatch("counter", json!({}), &ctx, "test-agent")
        .await;

    match result {
        Err(ToolError::PermissionDenied { name, reason }) => {
            assert_eq!(name, "counter");
            // The reason should indicate explicit denial, not just "not allowed".
            assert_eq!(
                reason,
                swage_core::PermissionDeniedReason::ExplicitlyDenied,
                "should report ExplicitlyDenied when tool is in deny list"
            );
        }
        other => panic!("expected PermissionDenied, got: {other:?}"),
    }
}

/// Proves Delivery Closure: default-deny rejects tools not in any allow entry.
#[tokio::test]
async fn dispatch_denies_unlisted_tool() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(CounterTool))
        .expect("registration should succeed");

    // Empty allow list — default-deny should reject everything.
    let permissions = PermissionConfig {
        allow: vec![],
        deny: vec![],
        bundles: HashMap::new(),
        require_approval: vec![],
    };

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig {
            requests_per_minute: None,
            requests_per_hour: None,
        },
        Arc::new(LeakDetector::default()),
        false,
    );

    let ctx = test_context();
    let result = dispatcher
        .dispatch("counter", json!({}), &ctx, "test-agent")
        .await;

    match result {
        Err(ToolError::PermissionDenied { name, reason }) => {
            assert_eq!(name, "counter");
            assert_eq!(
                reason,
                swage_core::PermissionDeniedReason::NotAllowed,
                "should report NotAllowed for default-deny"
            );
        }
        other => panic!("expected PermissionDenied, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Rate limiting enforcement through the pipeline
// ---------------------------------------------------------------------------

/// Proves Delivery Closure: rate limiter enforces limits through the dispatch
/// pipeline, returning Limited with retry_after when threshold exceeded.
///
/// Cross-task: requires RateLimiter [7uig], dispatch pipeline [oym1].
#[tokio::test]
async fn dispatch_rate_limits_after_threshold() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(CounterTool))
        .expect("registration should succeed");

    let permissions = PermissionConfig {
        allow: vec!["counter".into()],
        deny: vec![],
        bundles: HashMap::new(),
        require_approval: vec![],
    };

    let rate_limiter = Arc::new(RateLimiter::new());

    // Allow only 2 calls per minute.
    let rate_config = RateLimitConfig {
        requests_per_minute: Some(2),
        requests_per_hour: None,
    };

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        rate_limiter,
        rate_config,
        Arc::new(LeakDetector::default()),
        false,
    );

    let ctx = test_context();

    // First two calls should succeed.
    for i in 0..2 {
        let result = dispatcher
            .dispatch("counter", json!({}), &ctx, "test-agent")
            .await;
        assert!(
            result.is_ok(),
            "call {i} should succeed within rate limit"
        );
    }

    // Third call should be rate-limited.
    // The dispatcher may return an error or a specific RateLimited result —
    // the exact type depends on the implementation decision in [oym1].
    // We check that it's NOT a successful ToolOutput.
    let result = dispatcher
        .dispatch("counter", json!({}), &ctx, "test-agent")
        .await;

    assert!(
        result.is_err(),
        "third call should be rate-limited and fail"
    );
}

// ---------------------------------------------------------------------------
// Schema validation short-circuits before execute
// ---------------------------------------------------------------------------

/// Proves Delivery Closure: schema validation rejects invalid parameters
/// before they reach execute().
///
/// Cross-task: requires schema validation [9i98], dispatch pipeline [oym1],
/// Tool trait [acrt].
#[tokio::test]
async fn dispatch_rejects_invalid_parameters() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(LeakyEchoTool))
        .expect("registration should succeed");

    let permissions = PermissionConfig {
        allow: vec!["leaky_echo".into()],
        deny: vec![],
        bundles: HashMap::new(),
        require_approval: vec![],
    };

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig {
            requests_per_minute: None,
            requests_per_hour: None,
        },
        Arc::new(LeakDetector::default()),
        false,
    );

    let ctx = test_context();

    // "text" is required but we pass "wrong_field" instead.
    let result = dispatcher
        .dispatch(
            "leaky_echo",
            json!({ "wrong_field": "hello" }),
            &ctx,
            "test-agent",
        )
        .await;

    match result {
        Err(ToolError::InvalidParameters { name, message }) => {
            assert_eq!(name, "leaky_echo");
            assert!(
                message.contains("text"),
                "error should mention the missing required field 'text', got: {message}"
            );
        }
        other => panic!("expected InvalidParameters, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Registry lookup: unknown tool returns NotFound
// ---------------------------------------------------------------------------

/// Proves the pipeline short-circuits at registry lookup for unknown tools.
#[tokio::test]
async fn dispatch_returns_not_found_for_unknown_tool() {
    let registry = ToolRegistry::new(); // empty

    let permissions = PermissionConfig {
        allow: vec!["*".into()],
        deny: vec![],
        bundles: HashMap::new(),
        require_approval: vec![],
    };

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig {
            requests_per_minute: None,
            requests_per_hour: None,
        },
        Arc::new(LeakDetector::default()),
        false,
    );

    let ctx = test_context();
    let result = dispatcher
        .dispatch("nonexistent", json!({}), &ctx, "test-agent")
        .await;

    match result {
        Err(ToolError::NotFound { name }) => {
            assert_eq!(name, "nonexistent");
        }
        other => panic!("expected NotFound, got: {other:?}"),
    }
}
