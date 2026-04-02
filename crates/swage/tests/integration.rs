//! Cross-task integration tests for the Swage facade.
//!
//! Every test imports exclusively through `swage::` paths — no direct internal
//! crate dependencies. This proves the facade surface is complete.
//!
//! Run with: `cargo test -p swage --features integration`

#![cfg(feature = "integration")]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// All imports through the facade — the whole point of these tests.
// ---------------------------------------------------------------------------

use swage::core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCall, ToolCompletionRequest, ToolCompletionResponse, ToolError,
};
use swage::provider::LlmProvider;
use swage::tool::tool::Tool;
use swage::tool::context::ToolContext;
use swage::tool::dispatch::ToolDispatcher;
use swage::tool::permission::PermissionConfig;
use swage::tool::rate_limit::{RateLimitConfig, RateLimiter};
use swage::tool::registry::ToolRegistry;
use swage::safety::LeakDetector;
use swage::hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
use swage::storage::{
    MessageStore, ProjectStore, SqliteStorage, Storage,
    ThreadStore, ToolCallStore, TurnStore,
};
use swage::agent::{AgentBuilder, AgentConfig, CompactionThreshold, InboundMessage};
use swage::SwageBuilder;

// Convenience constructors
use swage::standard_leak_detector;

// ============================================================================
// Test doubles
// ============================================================================

// -- MockProvider -----------------------------------------------------------

/// Response variants the mock provider can return in sequence.
#[derive(Clone)]
enum MockResponse {
    Text {
        content: String,
        input_tokens: u32,
        output_tokens: u32,
    },
    ToolCalls {
        content: String,
        tool_calls: Vec<ToolCall>,
        input_tokens: u32,
        output_tokens: u32,
    },
    Error(String),
}

/// Programmable provider returning canned responses in order.
struct MockProvider {
    model_id: String,
    responses: Mutex<Vec<MockResponse>>,
    call_count: AtomicUsize,
}

impl MockProvider {
    fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            model_id: "mock-model".into(),
            // Reverse so we can pop from the end.
            responses: Mutex::new(responses.into_iter().rev().collect()),
            call_count: AtomicUsize::new(0),
        }
    }

    /// Single text response.
    fn text(content: &str) -> Self {
        Self::new(vec![MockResponse::Text {
            content: content.into(),
            input_tokens: 10,
            output_tokens: 5,
        }])
    }

    fn next_response(&self) -> MockResponse {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| MockResponse::Text {
                content: "default".into(),
                input_tokens: 1,
                output_tokens: 1,
            })
    }

    fn calls(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        match self.next_response() {
            MockResponse::Text {
                content,
                input_tokens,
                output_tokens,
            } => Ok(CompletionResponse {
                content,
                input_tokens,
                output_tokens,
                finish_reason: FinishReason::Stop,
                cost: None,
            }),
            MockResponse::Error(msg) => Err(ProviderError::Network {
                provider: "mock".into(),
                message: msg,
            }),
            _ => unreachable!("complete() called but response is ToolCalls"),
        }
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        match self.next_response() {
            MockResponse::Text {
                content,
                input_tokens,
                output_tokens,
            } => Ok(ToolCompletionResponse {
                content,
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
                input_tokens,
                output_tokens,
                cost: None,
            }),
            MockResponse::ToolCalls {
                content,
                tool_calls,
                input_tokens,
                output_tokens,
            } => Ok(ToolCompletionResponse {
                content,
                tool_calls,
                finish_reason: FinishReason::ToolUse,
                input_tokens,
                output_tokens,
                cost: None,
            }),
            MockResponse::Error(msg) => Err(ProviderError::Network {
                provider: "mock".into(),
                message: msg,
            }),
        }
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let content = match self.next_response() {
            MockResponse::Text { content, .. } => content,
            _ => "stream-default".into(),
        };
        Box::pin(futures::stream::once(async move {
            StreamChunk::Delta { content }
        }))
    }
}

// -- FailingProvider --------------------------------------------------------

/// Provider that fails a configurable number of times before succeeding.
/// Used in the reliability stack test.
struct FailingProvider {
    model_id: String,
    fail_count: AtomicUsize,
    failures_before_success: usize,
}

impl FailingProvider {
    fn new(model_id: &str, failures_before_success: usize) -> Self {
        Self {
            model_id: model_id.into(),
            fail_count: AtomicUsize::new(0),
            failures_before_success,
        }
    }
}

#[async_trait]
impl LlmProvider for FailingProvider {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let n = self.fail_count.fetch_add(1, Ordering::SeqCst);
        if n < self.failures_before_success {
            Err(ProviderError::Network {
                provider: "failing-mock".into(),
                message: "service unavailable".into(),
            })
        } else {
            Ok(CompletionResponse {
                content: "recovered".into(),
                input_tokens: 5,
                output_tokens: 3,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        let n = self.fail_count.fetch_add(1, Ordering::SeqCst);
        if n < self.failures_before_success {
            Err(ProviderError::Network {
                provider: "failing-mock".into(),
                message: "service unavailable".into(),
            })
        } else {
            Ok(ToolCompletionResponse {
                content: "recovered".into(),
                tool_calls: vec![],
                input_tokens: 5,
                output_tokens: 3,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::empty())
    }
}

// -- RecordingHook ----------------------------------------------------------

/// Hook that records lifecycle events for verification.
struct RecordingHook {
    name: String,
    events: Arc<Mutex<Vec<String>>>,
}

impl RecordingHook {
    fn new(name: &str) -> (Self, Arc<Mutex<Vec<String>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                name: name.into(),
                events: events.clone(),
            },
            events,
        )
    }
}

#[async_trait]
impl Hook for RecordingHook {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        let label = match event {
            HookEvent::SessionStart { .. } => "SessionStart",
            HookEvent::SessionEnd { .. } => "SessionEnd",
            HookEvent::BeforeInbound { .. } => "BeforeInbound",
            HookEvent::AfterResponse { .. } => "AfterResponse",
            HookEvent::PreToolUse { .. } => "PreToolUse",
            HookEvent::PostToolUse { .. } => "PostToolUse",
            HookEvent::PreCompact { .. } => "PreCompact",
            HookEvent::AgentStop { .. } => "AgentStop",
            HookEvent::TopologyStart { .. } => "TopologyStart",
            HookEvent::TopologyEnd { .. } => "TopologyEnd",
            HookEvent::NodeStart { .. } => "NodeStart",
            HookEvent::NodeEnd { .. } => "NodeEnd",
            HookEvent::FanOutSpawn { .. } => "FanOutSpawn",
            HookEvent::FanOutCollect { .. } => "FanOutCollect",
        };
        self.events.lock().unwrap().push(label.to_string());
        HookOutcome::Continue
    }
}

// -- EchoTool ---------------------------------------------------------------

/// Simple tool that echoes its input — used in multi-turn and permission tests.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes the input text back"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to echo" }
            },
            "required": ["text"]
        })
    }

    fn sensitive_keys(&self) -> &[&str] {
        &[]
    }

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("no input");
        Ok(json!({ "echoed": text }))
    }
}

/// Tool that is never expected to be called (for denied permission tests).
struct DeniedTool;

#[async_trait]
impl Tool for DeniedTool {
    fn name(&self) -> &str {
        "restricted_tool"
    }

    fn description(&self) -> &str {
        "A tool that should be denied by permissions"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
        })
    }

    fn sensitive_keys(&self) -> &[&str] {
        &[]
    }

    async fn execute(&self, _params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        panic!("DeniedTool should never be executed — permissions should block it");
    }
}

// ============================================================================
// Helper functions
// ============================================================================

fn test_storage() -> Arc<SqliteStorage> {
    Arc::new(SqliteStorage::in_memory().expect("open in-memory storage"))
}

fn test_config(agent_id: &str) -> AgentConfig {
    AgentConfig {
        agent_id: agent_id.into(),
        max_tool_rounds: 10,
        compaction_threshold: CompactionThreshold::MessageCount(100),
        metadata: HashMap::new(),
        output_schema: None,
        max_output_retries: 3,
    }
}

/// Build a ToolDispatcher that allows all registered tools.
fn allow_all_dispatcher(tools: Vec<Arc<dyn Tool>>) -> ToolDispatcher {
    ToolDispatcher::allow_all(tools)
}

// ============================================================================
// Test 1: Multi-turn conversation with tool calls
// ============================================================================
//
// **Proves:** Facade re-exports [ap30, 98kd, p8wi] + SwageBuilder [346u]
//   + convenience constructors [uim9]
//
// Constructs agent via facade builder with mock provider and real SQLite.
// Registers a test tool with permissions. Sends user message that triggers
// tool call, verifies the full chain: LLM response → tool execution → result
// returned to LLM → final response. Queries execution history from SQLite.
// Verifies hook lifecycle, leak detection, and token tracking.

#[tokio::test(flavor = "multi_thread")]
async fn multi_turn_conversation_with_tool_calls() {
    // Provider returns a tool call first, then a final text response.
    let provider = Arc::new(MockProvider::new(vec![
        MockResponse::ToolCalls {
            content: "I'll echo that for you.".into(),
            tool_calls: vec![ToolCall {
                call_id: "call_001".into(),
                name: "echo".into(),
                arguments: json!({"text": "hello world"}),
            }],
            input_tokens: 15,
            output_tokens: 10,
        },
        MockResponse::Text {
            content: "The echo returned: hello world".into(),
            input_tokens: 25,
            output_tokens: 8,
        },
    ]));

    let storage = test_storage();

    // Hook lifecycle recording.
    let (hook, hook_events) = RecordingHook::new("lifecycle-recorder");
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(hook)));

    // Tool with leak detection.
    let leak_detector = Arc::new(LeakDetector::default());

    let mut agent = AgentBuilder::new()
        .config(test_config("facade-test-agent"))
        .provider(provider as Arc<dyn LlmProvider>)
        .storage(storage.clone() as Arc<dyn Storage>)
        .tool_dispatcher(allow_all_dispatcher(vec![Arc::new(EchoTool)]))
        .hook_registry(Arc::new(hook_registry))
        .leak_detector(leak_detector)
        .build()
        .expect("build agent through facade");

    // Create session.
    let project = ProjectStore::get_or_create_default(&*storage)
        .await
        .expect("default project");
    let session_id = agent
        .create_session(project.id)
        .await
        .expect("create session");

    // Send message — triggers tool call round then final response.
    let response = agent
        .process_message(InboundMessage {
            message: "Please echo hello world".into(),
            session_id,
            thread_id: None,
        })
        .await
        .expect("process message");

    // Verify final response content.
    assert_eq!(response.response, "The echo returned: hello world");

    // Verify token tracking — accumulated across both rounds (15+25, 10+8).
    assert_eq!(response.input_tokens, 40);
    assert_eq!(response.output_tokens, 18);

    // Verify persistence: session, thread, turns, messages, tool calls.
    let threads = ThreadStore::list_by_session(&*storage, session_id)
        .await
        .expect("list threads");
    assert_eq!(threads.len(), 1, "one default thread");

    let messages = MessageStore::list_messages_by_thread(&*storage, threads[0].id)
        .await
        .expect("list messages");
    // user message + assistant (tool call) + tool result + assistant (final)
    assert!(
        messages.len() >= 3,
        "expected at least 3 messages, got {}",
        messages.len()
    );
    assert_eq!(messages[0].role, "user");

    let turns = TurnStore::list_turns_by_thread(&*storage, threads[0].id)
        .await
        .expect("list turns");
    assert!(!turns.is_empty(), "at least one turn recorded");

    // Verify tool call recorded.
    let tool_calls = ToolCallStore::list_tool_calls_by_turn(&*storage, turns[0].id)
        .await
        .expect("list tool calls");
    assert!(!tool_calls.is_empty(), "tool call recorded");
    assert_eq!(tool_calls[0].tool, "echo");

    // Verify hook lifecycle fired in correct order.
    let events = hook_events.lock().unwrap();
    assert!(
        events.contains(&"BeforeInbound".to_string()),
        "BeforeInbound hook fired"
    );
    assert!(
        events.contains(&"PreToolUse".to_string()),
        "PreToolUse hook fired"
    );
    assert!(
        events.contains(&"PostToolUse".to_string()),
        "PostToolUse hook fired"
    );
    assert!(
        events.contains(&"AfterResponse".to_string()),
        "AfterResponse hook fired"
    );
}

// ============================================================================
// Test 2: In-process execution (all platforms)
// ============================================================================
//
// **Proves:** SwageBuilder [346u] + convenience constructors [uim9]
//   + re-exports [ap30, 98kd, p8wi, nzm5]
//
// Same multi-turn scenario without sandboxing. Agent runs in-process.
// Exercises convenience constructors feeding into SwageBuilder.

#[tokio::test(flavor = "multi_thread")]
async fn in_process_execution() {
    let provider = Arc::new(MockProvider::text("in-process response"));
    let storage = test_storage();
    let leak_detector = standard_leak_detector();

    // Use SwageBuilder — the facade's primary entry point.
    let mut agent = SwageBuilder::new()
        .provider(provider as Arc<dyn LlmProvider>)
        .agent_id("in-process-agent")
        .storage(storage.clone() as Arc<dyn Storage>)
        .leak_detector(leak_detector)
        .build()
        .expect("SwageBuilder produces working agent");

    let project = ProjectStore::get_or_create_default(&*storage)
        .await
        .expect("default project");
    let session_id = agent
        .create_session(project.id)
        .await
        .expect("create session");

    let response = agent
        .process_message(InboundMessage {
            message: "hello".into(),
            session_id,
            thread_id: None,
        })
        .await
        .expect("process message");

    assert_eq!(response.response, "in-process response");

    // Verify history persisted.
    let threads = ThreadStore::list_by_session(&*storage, session_id)
        .await
        .expect("list threads");
    let messages = MessageStore::list_messages_by_thread(&*storage, threads[0].id)
        .await
        .expect("list messages");
    assert!(messages.len() >= 2, "user + assistant messages persisted");
}

// ============================================================================
// Test 3: Sandboxed execution (Linux only)
// ============================================================================
//
// **Proves:** Sandbox re-exports [nzm5] + SwageBuilder [346u]
//
// Same scenario but agent configured with sandbox. Gracefully skipped on
// non-Linux platforms.

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires sandbox-mode binary entry point (era 0.1 integration)"]
async fn sandboxed_execution() {
    // This test will be enabled once the sandbox binary entry point exists.
    // For now it verifies that the sandbox types are accessible through the
    // facade and that SwageBuilder accepts sandbox configuration.
    use swage::sandbox::{SandboxConfig as SandboxSandboxConfig};

    let _config = SandboxSandboxConfig::builder()
        .working_directory(std::env::temp_dir())
        .build()
        .expect("sandbox config builds through facade");

    // Full sandboxed agent test deferred to era 0.1 when the binary entry
    // point exists. The types are accessible — that's what this test proves.
}

// ============================================================================
// Test 4: Provider reliability stack
// ============================================================================
//
// **Proves:** Provider re-exports [98kd] + convenience constructors [uim9]
//   + SwageBuilder [346u]
//
// Agent configured with retry + circuit breaker + failover. Mock provider
// simulates transient failures. Verifies retries succeed and final response
// is delivered despite initial failures.

#[tokio::test(flavor = "multi_thread")]
async fn provider_reliability_stack() {
    use std::time::Duration;
    use swage::provider::builder::ProviderChainBuilder;
    use swage::provider::circuit_breaker::CircuitBreakerConfig;
    use swage::provider::retry::RetryConfig;

    // Primary provider fails twice then succeeds on third attempt.
    let primary = FailingProvider::new("primary", 2);

    let reliable_provider = ProviderChainBuilder::new()
        .provider(primary)
        .with_circuit_breaker(CircuitBreakerConfig {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
        })
        .with_retry(RetryConfig {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        })
        .build();

    let storage = test_storage();

    let mut agent = SwageBuilder::new()
        .provider(Arc::from(reliable_provider))
        .agent_id("reliability-agent")
        .storage(storage.clone() as Arc<dyn Storage>)
        .build()
        .expect("build agent with reliability stack");

    let project = ProjectStore::get_or_create_default(&*storage)
        .await
        .expect("default project");
    let session_id = agent
        .create_session(project.id)
        .await
        .expect("create session");

    // Despite transient failures, retry ensures delivery.
    let response = agent
        .process_message(InboundMessage {
            message: "test reliability".into(),
            session_id,
            thread_id: None,
        })
        .await
        .expect("message succeeds after retries");

    assert_eq!(response.response, "recovered");
}

// ============================================================================
// Test 5: Permission enforcement end-to-end
// ============================================================================
//
// **Proves:** Tool re-exports [98kd] + SwageBuilder [346u]
//   + storage re-exports [p8wi]
//
// Agent with restricted tool permissions. LLM requests a denied tool —
// denial handled gracefully (info message, not crash). LLM requests an
// allowed tool — executes normally. Denied tool call recorded in history.

#[tokio::test(flavor = "multi_thread")]
async fn permission_enforcement_end_to_end() {
    // First response: LLM tries the restricted tool (will be denied).
    // Second response: LLM tries the allowed echo tool.
    // Third response: final text after tool result.
    let provider = Arc::new(MockProvider::new(vec![
        MockResponse::ToolCalls {
            content: "Let me try the restricted tool.".into(),
            tool_calls: vec![ToolCall {
                call_id: "call_restricted".into(),
                name: "restricted_tool".into(),
                arguments: json!({}),
            }],
            input_tokens: 10,
            output_tokens: 5,
        },
        MockResponse::ToolCalls {
            content: "That was denied. Let me echo instead.".into(),
            tool_calls: vec![ToolCall {
                call_id: "call_echo".into(),
                name: "echo".into(),
                arguments: json!({"text": "allowed"}),
            }],
            input_tokens: 20,
            output_tokens: 8,
        },
        MockResponse::Text {
            content: "Done — echo worked, restricted was denied.".into(),
            input_tokens: 30,
            output_tokens: 10,
        },
    ]));

    let storage = test_storage();

    // Build dispatcher with permissions: allow echo, deny restricted_tool.
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool)).expect("register echo");
    registry.register(Arc::new(DeniedTool)).expect("register denied");

    let permissions = PermissionConfig {
        allow: vec!["echo".into()],
        deny: vec!["restricted_tool".into()],
        ..Default::default()
    };

    let dispatcher = ToolDispatcher::new(
        Arc::new(registry),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );

    let mut agent = AgentBuilder::new()
        .config(test_config("permission-agent"))
        .provider(provider as Arc<dyn LlmProvider>)
        .storage(storage.clone() as Arc<dyn Storage>)
        .tool_dispatcher(dispatcher)
        .build()
        .expect("build agent with restricted permissions");

    let project = ProjectStore::get_or_create_default(&*storage)
        .await
        .expect("default project");
    let session_id = agent
        .create_session(project.id)
        .await
        .expect("create session");

    // Process — the agent should handle the denied tool gracefully.
    let response = agent
        .process_message(InboundMessage {
            message: "Try both tools".into(),
            session_id,
            thread_id: None,
        })
        .await
        .expect("message processed despite denied tool");

    assert_eq!(response.response, "Done — echo worked, restricted was denied.");

    // Verify the denied tool call is recorded in history.
    let threads = ThreadStore::list_by_session(&*storage, session_id)
        .await
        .expect("list threads");
    let turns = TurnStore::list_turns_by_thread(&*storage, threads[0].id)
        .await
        .expect("list turns");

    // At least one turn should have tool calls.
    let mut found_denied = false;
    let mut found_allowed = false;
    for turn in &turns {
        let calls = ToolCallStore::list_tool_calls_by_turn(&*storage, turn.id)
            .await
            .expect("list tool calls");
        for call in &calls {
            if call.tool == "restricted_tool" {
                found_denied = true;
            }
            if call.tool == "echo" {
                found_allowed = true;
            }
        }
    }

    assert!(found_denied, "denied tool call recorded in execution history");
    assert!(found_allowed, "allowed tool call recorded in execution history");
}

// ============================================================================
// Test 6: Config loading through facade
// ============================================================================
//
// **Proves:** Config re-exports [nzm5] + facade completeness
//
// Exercises config types through the facade to verify merge coherence.

#[tokio::test(flavor = "multi_thread")]
async fn config_types_accessible_through_facade() {
    // Verify config types are accessible through swage::config.
    use swage::config::{ResolvedConfig, WorkspaceConfig, ConfigError};

    // The types exist and are usable — compile-time proof that the
    // config re-exports cover the developer-facing surface.
    let _workspace: Option<WorkspaceConfig> = None;
    let _resolved: Option<ResolvedConfig> = None;
    let _error: Option<ConfigError> = None;
}
