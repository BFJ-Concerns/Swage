//! Cross-task integration tests for programmatic tool calling.
//!
//! These tests exercise the child-worker `python_execute` path end to end:
//! worker execution, nested tool dispatch, nested-tool event delivery, and
//! persistence of `parent_call_id` for reloaded conversation detail.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;
use swage_storage::model::NewWorkspaceAttachment;
use swage_storage::store::{BenchStore, WorkspaceAttachmentStore};

// ---------------------------------------------------------------------------
// Mock providers
// ---------------------------------------------------------------------------

/// Multi-round provider — each stream_message call pops the next chunk sequence.
struct MultiRoundProvider {
    rounds: std::sync::Mutex<VecDeque<Vec<StreamChunk>>>,
}

impl MultiRoundProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: std::sync::Mutex::new(rounds.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for MultiRoundProvider {
    fn model_id(&self) -> &str {
        "mock-multi-round"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in PTC integration tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in PTC integration tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = self.rounds.lock().unwrap().pop_front().unwrap_or_default();
        Box::pin(futures::stream::iter(chunks))
    }
}

// ---------------------------------------------------------------------------
// Test helpers (mirrors integration.rs patterns)
// ---------------------------------------------------------------------------

/// Start a test server with programmatic tool-calling tools registered.
///
/// Registers `python_execute` with the real child-worker runtime and a warm
/// worker pool alongside the EchoTool. The EchoTool acts as a sub-tool target;
/// Python code calls `swage.shell("echo hi")["output"]` which dispatches to
/// the mock `shell` tool below.
async fn start_ptc_test_server(
    provider: impl LlmProvider + 'static,
) -> (Router, SocketAddr, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());

    // Build the sub-dispatcher: echo and shell, but NOT python_execute.
    // This prevents recursion when Python code dispatches sub-tool calls.
    let echo_tool: Arc<dyn swage_gateway::tool::Tool> = Arc::new(EchoTool);
    let shell_tool: Arc<dyn swage_gateway::tool::Tool> = Arc::new(MockShellTool);
    let sub_dispatcher = Arc::new(swage_gateway::tool::ToolDispatcher::allow_all(vec![
        Arc::clone(&echo_tool),
        Arc::clone(&shell_tool),
    ]));

    // Create the PythonExecuteTool with access to sub-tools and storage.
    let runtime =
        swage_python::PythonWorkerRuntime::new(swage_config::model::PythonExecuteToolConfig {
            require_sandbox: false,
            ..swage_config::model::PythonExecuteToolConfig::default()
        })
        .expect("Python worker runtime must be available for PTC integration tests");
    let pool = swage_python::WorkerPool::new(Arc::new(runtime))
        .expect("worker pool should initialise for PTC integration tests");
    let ptc_tool = swage_gateway::tool::python_execute::PythonExecuteTool::new(
        Arc::clone(pool.runtime()),
        Arc::clone(&pool),
        sub_dispatcher,
        Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
    );

    // Final dispatcher includes echo, shell, and python_execute.
    let dispatcher = swage_gateway::tool::ToolDispatcher::allow_all(vec![
        echo_tool,
        shell_tool,
        Arc::new(ptc_tool) as Arc<dyn swage_gateway::tool::Tool>,
    ]);
    let mut config = swage_config::ResolvedConfig::default();
    // These tests exercise Python tool chaining itself. Approval routing and
    // the workspace permission gate are covered separately, so keep
    // python_execute ungated and grant the bench full workspace permissions
    // in this harness.
    config.tools.permissions.require_approval = vec![];

    let default_bench = storage
        .get_or_create_default()
        .await
        .expect("default bench should be seeded for PTC harness");
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            bench_id: default_bench.id,
            label: "ptc-test-workspace".to_string(),
            path: "/tmp/ptc".to_string(),
            permissions: vec!["read".to_string(), "write".to_string()],
            is_trusted: true,
        })
        .await
        .expect("trusted workspace attachment should attach to default bench");

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
    );
    let router = build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_router = router.clone();
    tokio::spawn(async move {
        axum::serve(listener, serve_router).await.unwrap();
    });

    (router, addr, storage)
}

/// Mock tool echoing its "text" argument — sub-tool target in PTC tests.
struct EchoTool;

#[async_trait]
impl swage_gateway::tool::Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echo the input back"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        })
    }
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        let text = arguments["text"].as_str().unwrap_or("").to_string();
        Ok(json!({"output": text}))
    }
}

/// Mock shell tool — simulates the real `shell` tool for PTC tests.
///
/// Python code calls `swage.shell("echo hello")["output"]`, which dispatches
/// to the tool named `"shell"` with `{"command": "echo hello"}`. This mock
/// just returns the command string as output, avoiding real subprocess
/// execution in tests.
struct MockShellTool;

#[async_trait]
impl swage_gateway::tool::Tool for MockShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "Mock shell — returns the command as output"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        })
    }
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        let command = arguments["command"].as_str().unwrap_or("").to_string();
        // Simulate `echo <arg>` by extracting the argument after "echo ".
        let output = if let Some(rest) = command.strip_prefix("echo ") {
            format!("{rest}\n")
        } else {
            command
        };
        Ok(json!({"output": output}))
    }
}

// WebSocket helpers (mirrors integration.rs).
type WsSink = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Message,
>;
type WsStream = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn http(router: &Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn connect_ws(addr: SocketAddr, conversation_id: &str) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/api/conversations/{conversation_id}/ws");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WebSocket connection should succeed");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, payload: Value) {
    sink.send(tungstenite::Message::Text(payload.to_string().into()))
        .await
        .expect("ws send should succeed");
}

async fn ws_recv(stream: &mut WsStream) -> Value {
    loop {
        match stream.next().await {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return serde_json::from_str(&text).expect("ws text should be valid JSON");
            }
            Some(Ok(tungstenite::Message::Close(_))) => panic!("WebSocket closed unexpectedly"),
            Some(Err(e)) => panic!("WebSocket error: {e}"),
            None => panic!("WebSocket stream ended unexpectedly"),
            _ => continue,
        }
    }
}

/// Collect all WebSocket events until turn_complete, with timeout.
async fn collect_events_until_complete(stream: &mut WsStream) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout_at(deadline, ws_recv(stream))
            .await
            .expect("timed out waiting for turn_complete");
        let is_complete = event["type"] == "turn_complete";
        events.push(event);
        if is_complete {
            break;
        }
    }
    events
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Python code calling multiple tool functions, chaining results.
///
/// Journey: Core Journey 2 (Task Execution) — PTC proof of delivery
/// Spans: Python worker runtime, nested tool runtime, ToolDispatcher,
///   PythonExecuteTool
///
/// Proves: the agent writes Python code that calls multiple tool functions,
/// processes intermediate results with variables and control flow, and returns
/// a correct answer. This is the canonical proof of delivery from the phase doc.
///
#[tokio::test(flavor = "multi_thread")]
async fn ptc_chained_tool_calls_produce_correct_output() {
    // Python code that the model would submit: call two tools and combine.
    let python_code = r#"
result1 = swage.shell("echo hello")["output"]
result2 = swage.shell("echo world")["output"]
print(f"{result1.strip()} {result2.strip()}")
"#;

    // Round 1: model requests python_execute with chaining code.
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "ptc_001".into(),
            name: "python_execute".into(),
            arguments: json!({ "code": python_code }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    // Round 2: model uses the Python output in its response.
    let round_2 = vec![
        StreamChunk::Delta {
            content: "The result is: hello world".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = MultiRoundProvider::new(vec![round_1, round_2]);
    let (_, addr, _) = start_ptc_test_server(provider).await;

    // Create conversation + open WebSocket.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/conversations"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    // Subscribe to tools channel so tool events are delivered.
    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;

    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Run the chained task"}),
    )
    .await;

    let events = collect_events_until_complete(&mut ws_read).await;

    // The python_execute tool_end should show success with Python output.
    let ptc_tool_end = events
        .iter()
        .find(|e| e["type"] == "tool_end" && e["tool"] == "python_execute");

    assert!(
        ptc_tool_end.is_some(),
        "Expected a tool_end event for python_execute. \
         The tool must be registered and execute Python code. Events: {:?}",
        events
            .iter()
            .map(|e| e["type"].as_str())
            .collect::<Vec<_>>()
    );

    let result = ptc_tool_end.unwrap();
    assert_eq!(
        result["status"], "success",
        "python_execute should succeed; got: {result}"
    );

    // The Python output should contain the chained result.
    // Under agent delegation, the result is a structured object with an
    // "output" key from PythonExecuteTool, not a flat string.
    let output = result["result"]["output"]
        .as_str()
        .unwrap_or_else(|| result["result"].as_str().unwrap_or(""));
    assert!(
        output.contains("hello") && output.contains("world"),
        "Python output should contain chained tool results; got: {output}"
    );
}

/// Sub-tool calls from Python emit events with parent_call_id for nesting.
///
/// Journey: Core Journey 2 (Task Execution) — activity display nesting
/// Spans: nested tool runtime, sub-tool events, PythonExecuteTool,
///   frontend nesting
///
/// Proves: when Python code calls swage.shell(), the WebSocket receives
/// tool_start/tool_end events with a parent_call_id linking them to the
/// enclosing python_execute call. The frontend uses this to render nesting.
///
#[tokio::test(flavor = "multi_thread")]
async fn ptc_sub_tool_events_include_parent_call_id() {
    // Simple Python code calling one tool.
    let python_code = r#"result = swage.shell("echo nested")["output"]"#;

    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "ptc_002".into(),
            name: "python_execute".into(),
            arguments: json!({ "code": python_code }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "Done.".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = MultiRoundProvider::new(vec![round_1, round_2]);
    let (_, addr, _) = start_ptc_test_server(provider).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/conversations"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    // Subscribe to tools channel so tool events are delivered.
    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;

    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Run nested tool"}),
    )
    .await;

    let events = collect_events_until_complete(&mut ws_read).await;

    // There should be a tool_start for python_execute (no parent).
    let ptc_start = events
        .iter()
        .find(|e| e["type"] == "tool_start" && e["tool"] == "python_execute");
    assert!(
        ptc_start.is_some(),
        "Expected tool_start for python_execute; events: {:?}",
        events
            .iter()
            .map(|e| format!("{}:{}", e["type"], e.get("tool").unwrap_or(&Value::Null)))
            .collect::<Vec<_>>()
    );

    // There should be sub-tool events (tool_start/tool_end for shell)
    // with parent_call_id pointing to the python_execute call.
    let sub_tool_starts: Vec<_> = events
        .iter()
        .filter(|e| {
            e["type"] == "tool_start"
                && e["tool"] != "python_execute"
                && e.get("parent_call_id").is_some()
        })
        .collect();

    assert!(
        !sub_tool_starts.is_empty(),
        "Expected sub-tool tool_start events with parent_call_id for nested \
         tool calls from Python. Sub-tool events must include parent_call_id \
         linking them to the python_execute invocation. Events: {:?}",
        events
            .iter()
            .map(|e| e["type"].as_str())
            .collect::<Vec<_>>()
    );

    // The parent_call_id should match the python_execute call_id.
    let parent_id = sub_tool_starts[0]["parent_call_id"].as_str().unwrap();
    assert_eq!(
        parent_id, "ptc_002",
        "Sub-tool parent_call_id should match the python_execute call_id"
    );
}

/// Python errors surface as tool result with traceback.
///
/// Journey: Core Journey 2 (Task Execution) — error surfacing
/// Spans: Python worker runtime, PythonExecuteTool, sub-tool events
///
/// Proves: when Python code raises an error, the tool call fails and the
/// traceback appears in the error result visible to both the model and the
/// user in the activity display.
#[tokio::test(flavor = "multi_thread")]
async fn ptc_python_error_surfaces_as_traceback() {
    // Code with a deliberate runtime error.
    let python_code = r#"
x = 1 / 0  # ZeroDivisionError
"#;

    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "ptc_003".into(),
            name: "python_execute".into(),
            arguments: json!({ "code": python_code }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    // Model sees the error and explains it.
    let round_2 = vec![
        StreamChunk::Delta {
            content: "The code raised a ZeroDivisionError.".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = MultiRoundProvider::new(vec![round_1, round_2]);
    let (_, addr, _) = start_ptc_test_server(provider).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/conversations"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    // Subscribe to tools channel so tool events are delivered.
    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;

    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Run the bad code"}),
    )
    .await;

    let events = collect_events_until_complete(&mut ws_read).await;

    // The python_execute tool_end should report failure with a traceback.
    let ptc_tool_end = events
        .iter()
        .find(|e| e["type"] == "tool_end" && e["tool"] == "python_execute");

    assert!(
        ptc_tool_end.is_some(),
        "Expected tool_end for python_execute (even on error). Events: {:?}",
        events
            .iter()
            .map(|e| e["type"].as_str())
            .collect::<Vec<_>>()
    );

    let ptc_tool_end = ptc_tool_end.unwrap();
    assert_eq!(
        ptc_tool_end["status"], "error",
        "python_execute should fail when Python raises; got: {ptc_tool_end}"
    );

    let result_text = ptc_tool_end["result"].as_str().unwrap_or("");
    assert!(
        result_text.contains("ZeroDivisionError"),
        "Python runtime errors should produce a traceback containing the \
         exception type. Got: {result_text}"
    );
}

/// Sub-tool calls from Python persist in conversation history with nesting.
///
/// Journey: Core Journey 2 (Task Execution) — activity display persistence
/// Spans: nested tool runtime, sub-tool events, PythonExecuteTool,
///   tool-call storage, API tool-call views
///
/// Proves: after a turn with python_execute, the HTTP conversation detail
/// API returns tool_calls with parent_call_id — so the frontend can render
/// nested sub-tool calls on page reload, not just from live WebSocket events.
///
#[tokio::test(flavor = "multi_thread")]
async fn ptc_sub_tool_calls_persist_with_parent_id() {
    let python_code = r#"swage.shell("echo persisted")["output"]"#;

    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "ptc_004".into(),
            name: "python_execute".into(),
            arguments: json!({ "code": python_code }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "Done.".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = MultiRoundProvider::new(vec![round_1, round_2]);
    let (router, addr, _) = start_ptc_test_server(provider).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/conversations"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    // Subscribe to tools channel so tool events are delivered.
    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;

    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Persist test"}),
    )
    .await;

    // Wait for turn to complete.
    let _events = collect_events_until_complete(&mut ws_read).await;

    // Close WebSocket and check persistence via HTTP.
    drop(ws_write);
    drop(ws_read);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (status, body) = http(&router, "GET", &format!("/api/conversations/{conv_id}")).await;
    assert_eq!(status, StatusCode::OK);

    let messages = body["messages"].as_array().unwrap();
    let assistant_msg = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant message should exist after turn");

    let tool_calls = assistant_msg["tool_calls"]
        .as_array()
        .expect("tool_calls field should be present");

    // There should be at least the python_execute call and a sub-tool call.
    assert!(
        tool_calls.len() >= 2,
        "Expected at least 2 tool calls (python_execute + sub-tool); got {}. \
         Tool calls: {tool_calls:?}",
        tool_calls.len()
    );

    // Sub-tool calls should have parent_call_id in the API response.
    let sub_calls: Vec<_> = tool_calls
        .iter()
        .filter(|tc| tc.get("parent_call_id").is_some() && tc["parent_call_id"].as_str().is_some())
        .collect();

    assert!(
        !sub_calls.is_empty(),
        "At least one tool call should have parent_call_id for sub-tool nesting. \
         Tool calls: {tool_calls:?}"
    );

    // The parent_call_id should reference the python_execute call.
    let parent_id = sub_calls[0]["parent_call_id"].as_str().unwrap();
    assert_eq!(
        parent_id, "ptc_004",
        "Sub-tool parent_call_id should match the python_execute call_id"
    );
}
