//! Cross-task integration tests for Era 0.2 Phase 02 (Gateway Integration & Observability).
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the assembly wiring, subscription protocol, token
//! tracking, context snapshots, and shutdown endpoint are all working together.
//!
//! Tests in this file are expected to FAIL until the phase 02 motes are
//! implemented. Each test documents which tasks it spans and why it currently fails.

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
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk, TokenUsage,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;

// ---------------------------------------------------------------------------
// Mock provider — streams tokens with usage metadata
// ---------------------------------------------------------------------------

/// A provider that streams a fixed response with token usage data.
/// The usage chunk is essential for verifying token persistence and cost tracking.
struct UsageTrackingProvider {
    response_text: String,
    input_tokens: u32,
    output_tokens: u32,
}

impl UsageTrackingProvider {
    fn new(response: &str, input: u32, output: u32) -> Self {
        Self {
            response_text: response.to_string(),
            input_tokens: input,
            output_tokens: output,
        }
    }
}

#[async_trait]
impl LlmProvider for UsageTrackingProvider {
    fn model_id(&self) -> &str {
        "mock-usage-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in streaming tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in streaming tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = vec![
            StreamChunk::Delta {
                content: self.response_text.clone(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ];
        Box::pin(futures::stream::iter(chunks))
    }
}

/// A provider that streams a tool call on the first invocation, then text
/// on the second. Used for testing that unsubscribed clients do not receive
/// tool events.
struct ToolCallingProvider {
    called: std::sync::atomic::AtomicBool,
}

impl ToolCallingProvider {
    fn new() -> Self {
        Self {
            called: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl LlmProvider for ToolCallingProvider {
    fn model_id(&self) -> &str {
        "mock-tool-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in streaming tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in streaming tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        // First call: emit a tool use. Second call (after tool result): emit text.
        let was_called = self.called.swap(true, std::sync::atomic::Ordering::SeqCst);

        if was_called {
            // Second round: respond with text after tool execution.
            let chunks = vec![
                StreamChunk::Delta {
                    content: "Tool done.".into(),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop),
                },
            ];
            Box::pin(futures::stream::iter(chunks))
        } else {
            // First round: request a tool call.
            let chunks = vec![
                StreamChunk::ToolUse {
                    call_id: "call_test_01".into(),
                    name: "echo".into(),
                    arguments: json!({"text": "ping"}),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::ToolUse),
                },
            ];
            Box::pin(futures::stream::iter(chunks))
        }
    }
}

// ---------------------------------------------------------------------------
// Mock tool
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

async fn start_test_server(
    provider: impl LlmProvider + 'static,
) -> (Router, SocketAddr, Arc<SqliteStorage>) {
    start_test_server_with_dispatcher(
        provider,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
    )
    .await
}

async fn start_test_server_with_dispatcher(
    provider: impl LlmProvider + 'static,
    dispatcher: swage_gateway::tool::ToolDispatcher,
) -> (Router, SocketAddr, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        dispatcher,
        None,
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

type WsSink = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Message,
>;
type WsStream = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn connect_ws(addr: SocketAddr, conversation_id: &str) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/api/conversations/{conversation_id}/ws");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WebSocket connection should succeed");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, payload: Value) {
    sink.send(tungstenite::Message::Text(payload.to_string()))
        .await
        .expect("ws send should succeed");
}

/// Read the next JSON text frame. Panics on close or error — use ws_recv_lenient
/// when close frames are expected.
#[allow(dead_code)]
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

/// Collect all WebSocket events within a timeout. Returns them in order.
async fn ws_collect_events(stream: &mut WsStream, timeout: Duration) -> Vec<Value> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, ws_recv_lenient(stream)).await {
            Ok(Some(event)) => events.push(event),
            Ok(None) => break, // Close frame
            Err(_) => break,   // Timeout
        }
    }
    events
}

/// Like ws_recv but returns None on close instead of panicking.
async fn ws_recv_lenient(stream: &mut WsStream) -> Option<Value> {
    loop {
        match stream.next().await {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return Some(serde_json::from_str(&text).unwrap_or(Value::Null));
            }
            Some(Ok(tungstenite::Message::Close(_))) => return None,
            None => return None,
            _ => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-task integration tests
// ---------------------------------------------------------------------------

/// Journey: Execution Visibility (Core Journey 4)
/// Spans: [ogpt] event types + [6qgr] subscription + [81oq] WS handling +
///        [x05a] assembly wiring + [bxdp] report delivery
///
/// Proves: A subscribed client receives a ContextAssembled event before the
/// first streaming token, with populated elements and non-zero token counts.
///
/// Currently fails because: Subscribe is not a recognised ClientEvent, the
/// assembly engine is not wired into the turn loop, and ContextAssembled
/// is not a ServerEvent variant.
#[tokio::test(flavor = "multi_thread")]
async fn subscribed_client_receives_assembly_report_before_tokens() {
    let provider = UsageTrackingProvider::new("Hello!", 150, 20);
    let (router, addr, _storage) = start_test_server(provider).await;

    // Create a conversation.
    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conv_id = body["id"].as_str().unwrap();

    // Connect and subscribe to the "context" channel.
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;

    // Send a message to trigger a turn.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "hello"}),
    )
    .await;

    // Collect all events from the turn.
    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // Find the ContextAssembled event and the first Token event.
    let context_assembled_idx = events.iter().position(|e| e["type"] == "context_assembled");
    let first_token_idx = events.iter().position(|e| e["type"] == "token");

    // ContextAssembled must exist.
    assert!(
        context_assembled_idx.is_some(),
        "expected a context_assembled event; got events: {events:?}"
    );

    // ContextAssembled must precede the first token.
    if let (Some(ca_idx), Some(tok_idx)) = (context_assembled_idx, first_token_idx) {
        assert!(
            ca_idx < tok_idx,
            "context_assembled (index {ca_idx}) must precede first token (index {tok_idx})"
        );
    }

    // Report should have populated fields.
    let report = &events[context_assembled_idx.unwrap()]["report"];
    assert!(
        report["total_tokens"].as_u64().unwrap_or(0) > 0,
        "report should have non-zero total_tokens"
    );
    assert!(
        !report["elements"].as_array().unwrap_or(&vec![]).is_empty(),
        "report should have at least one element"
    );
}

/// Journey: Execution Visibility (Core Journey 4) — negative case
/// Spans: [6qgr] subscription + [0bzx] tool gating + [81oq] WS handling
///
/// Proves: An unsubscribed client receives no observability events (no
/// ContextAssembled, no ToolStart, no ToolEnd). Only token, turn_complete,
/// and error events reach the client.
///
/// Currently fails because: ToolStart and ToolEnd are emitted unconditionally
/// to all WebSocket clients, regardless of subscription.
#[tokio::test(flavor = "multi_thread")]
async fn unsubscribed_client_receives_no_observability_events() {
    let echo_tool: Arc<dyn swage_gateway::tool::Tool> = Arc::new(EchoTool);
    let dispatcher = swage_gateway::tool::ToolDispatcher::allow_all(vec![echo_tool]);
    let (_router, addr, _storage) =
        start_test_server_with_dispatcher(ToolCallingProvider::new(), dispatcher).await;

    // Create a conversation via HTTP (creates session + default thread).
    let (status, body) = http(&_router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conv_id = body["id"].as_str().unwrap();

    // Connect WITHOUT subscribing to any channel.
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;

    // Send a message that will trigger a tool call.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "use echo"}),
    )
    .await;

    // Collect all events.
    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // There should be no tool_start, tool_end, or context_assembled events.
    let observability_events: Vec<_> = events
        .iter()
        .filter(|e| {
            let event_type = e["type"].as_str().unwrap_or("");
            matches!(event_type, "tool_start" | "tool_end" | "context_assembled")
        })
        .collect();

    assert!(
        observability_events.is_empty(),
        "unsubscribed client should receive no observability events, but got: {observability_events:?}"
    );

    // Should still receive token and turn_complete events.
    let has_turn_complete = events.iter().any(|e| e["type"] == "turn_complete");
    assert!(has_turn_complete, "should still receive turn_complete");
}

/// Journey: Execution Visibility (Core Journey 4) — context persistence
/// Spans: [bxdp] report delivery + [e84f] snapshot storage + [yts4] context endpoint
///
/// Proves: The assembly report delivered via WebSocket is also stored and
/// retrievable via GET /api/turns/:id/context with matching content.
///
/// Currently fails because: The ContextAssembled event doesn't exist,
/// context snapshots are not stored, and the REST endpoint doesn't exist.
#[tokio::test(flavor = "multi_thread")]
async fn past_turn_report_matches_live_websocket_report() {
    let provider = UsageTrackingProvider::new("Stored!", 200, 30);
    let (router, addr, _storage) = start_test_server(provider).await;

    // Create a conversation.
    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conv_id = body["id"].as_str().unwrap();

    // Connect and subscribe to context.
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;

    // Send a message.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "store this"}),
    )
    .await;

    // Collect events.
    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // Extract the live ContextAssembled report and the turn_id.
    let context_event = events.iter().find(|e| e["type"] == "context_assembled");
    assert!(
        context_event.is_some(),
        "expected context_assembled event; got: {events:?}"
    );
    let context_event = context_event.unwrap();
    let turn_id = context_event["turn_id"]
        .as_str()
        .expect("turn_id in context_assembled");
    let live_report = &context_event["report"];

    // Query the REST endpoint for the same turn.
    let (status, stored_body) =
        http(&router, "GET", &format!("/api/turns/{turn_id}/context")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "past turn context endpoint should return 200"
    );

    // The stored report should match the live one.
    assert_eq!(
        stored_body["report"], *live_report,
        "stored report should match the live WebSocket report"
    );
}

/// Journey: Execution Visibility (Core Journey 4) — token tracking
/// Spans: [95z3] token persistence + [1qwx] TurnComplete extension +
///        [9di0] pricing config
///
/// Proves: After a turn, token counts from the provider are persisted to
/// storage and included in the extended TurnComplete event for subscribed
/// clients.
///
/// Currently fails because: stored_usage is discarded (let _ = stored_usage),
/// TurnComplete has no optional usage fields, and storage.update_tokens()
/// is never called.
#[tokio::test(flavor = "multi_thread")]
async fn token_usage_persisted_and_in_turn_complete() {
    let provider = UsageTrackingProvider::new("Counted!", 1240, 380);
    let (router, addr, storage) = start_test_server(provider).await;

    // Create a conversation.
    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conv_id = body["id"].as_str().unwrap();

    // Connect and subscribe to context (to get extended TurnComplete).
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;

    // Send a message.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "count tokens"}),
    )
    .await;

    // Collect events.
    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // Find the TurnComplete event.
    let turn_complete = events.iter().find(|e| e["type"] == "turn_complete");
    assert!(
        turn_complete.is_some(),
        "expected turn_complete event; got: {events:?}"
    );
    let turn_complete = turn_complete.unwrap();
    let turn_id = turn_complete["turn_id"]
        .as_str()
        .expect("turn_id in turn_complete");

    // For a context-subscribed client, TurnComplete should include usage.
    assert_eq!(
        turn_complete["input_tokens"].as_u64(),
        Some(1240),
        "turn_complete should include input_tokens for subscribed client"
    );
    assert_eq!(
        turn_complete["output_tokens"].as_u64(),
        Some(380),
        "turn_complete should include output_tokens for subscribed client"
    );

    // Verify storage was updated — query via REST (get conversation with turns).
    // The turn record should have non-null token counts.
    use swage_storage::store::TurnStore;
    let turn_uuid = uuid::Uuid::parse_str(turn_id).unwrap();
    let turn = storage.get_turn(turn_uuid).await.unwrap();
    assert!(turn.is_some(), "turn should exist in storage");
    let turn = turn.unwrap();
    assert_eq!(
        turn.input_tokens,
        Some(1240),
        "input_tokens should be persisted"
    );
    assert_eq!(
        turn.output_tokens,
        Some(380),
        "output_tokens should be persisted"
    );
}

/// Journey: Terminal Workflow (Core Journey 2) — clean shutdown
/// Spans: [tddj] shutdown endpoint + [lync] broadcast + [ogpt] event types
///
/// Proves: POST /api/shutdown returns 200 from localhost, notifies connected
/// WebSocket clients with a Shutdown event, and the gateway terminates.
///
/// Currently fails because: The /api/shutdown endpoint does not exist (404),
/// the Shutdown ServerEvent variant does not exist, and there is no broadcast
/// channel on AppState.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_endpoint_notifies_websocket_clients() {
    let provider = UsageTrackingProvider::new("bye", 10, 5);
    let (router, addr, _storage) = start_test_server(provider).await;

    // Create a conversation and connect.
    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conv_id = body["id"].as_str().unwrap();
    let (_sink, mut stream) = connect_ws(addr, conv_id).await;

    // POST /api/shutdown from localhost.
    let (status, body) = http(&router, "POST", "/api/shutdown").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "shutdown from localhost should return 200; got {status}"
    );
    assert_eq!(body["status"], "shutting_down");

    // The WebSocket client should receive a Shutdown event.
    let events = ws_collect_events(&mut stream, Duration::from_secs(3)).await;
    let shutdown_event = events.iter().find(|e| e["type"] == "shutdown");
    assert!(
        shutdown_event.is_some(),
        "expected a shutdown event on WebSocket; got: {events:?}"
    );
    assert!(
        shutdown_event.unwrap()["reason"].is_string(),
        "shutdown event should include a reason string"
    );
}
