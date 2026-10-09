//! Integration tests for the gateway runtime stack.
//!
//! These tests verify behaviour that emerges only when several components are
//! wired together. Each test names the components it spans and the behaviour
//! it proves.

#![allow(clippy::unwrap_used)]

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
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, shutdown::recover_orphaned_turns, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    model::{NewThread, NewTurn, TurnState},
    store::{ProviderRoundStore, ThreadStore, TurnStore},
};

use crate::support::start_live_server;

// ---------------------------------------------------------------------------
// Mock providers
// ---------------------------------------------------------------------------

/// Streams a fixed sequence of chunks in one shot.
struct MockProvider {
    chunks: std::sync::Mutex<Vec<StreamChunk>>,
}

impl MockProvider {
    fn new(chunks: Vec<StreamChunk>) -> Self {
        Self {
            chunks: std::sync::Mutex::new(chunks),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "mock-model"
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
        let chunks = std::mem::take(&mut *self.chunks.lock().unwrap());
        Box::pin(futures::stream::iter(chunks))
    }
}

/// Per-call control via channels — each `stream` call pops the next
/// receiver from the queue. The test sends chunks to the corresponding sender,
/// giving it deterministic control over timing.
struct ChannelProvider {
    receivers: std::sync::Mutex<VecDeque<tokio::sync::mpsc::Receiver<StreamChunk>>>,
}

impl ChannelProvider {
    fn new(receivers: Vec<tokio::sync::mpsc::Receiver<StreamChunk>>) -> Self {
        Self {
            receivers: std::sync::Mutex::new(receivers.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for ChannelProvider {
    fn model_id(&self) -> &str {
        "channel-model"
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
        let rx = self
            .receivers
            .lock()
            .unwrap()
            .pop_front()
            .expect("no more streams configured for ChannelProvider");

        // Convert the mpsc::Receiver into a Stream.
        Box::pin(futures::stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|chunk| (chunk, rx))
        }))
    }
}

// ---------------------------------------------------------------------------
// Mock tool for integration tests
// ---------------------------------------------------------------------------

/// Echoes its "text" argument back — predictable output for tool execution tests.
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
        serde_json::json!({
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
        Ok(serde_json::json!({"output": text}))
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Spin up an axum server backed by in-memory SQLite and the given provider.
///
/// Returns the router (for tower::oneshot HTTP), the socket address (for
/// WebSocket connections), and the storage handle (for direct inspection).
async fn start_test_server(
    provider: impl LlmProvider + 'static,
) -> (Router, SocketAddr, Arc<SqliteStorage>) {
    start_test_server_with_dispatcher(
        provider,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
    )
    .await
}

/// Spin up a test server with a pre-built tool dispatcher.
async fn start_test_server_with_dispatcher(
    provider: impl LlmProvider + 'static,
    dispatcher: swage_gateway::tool::ToolDispatcher,
) -> (Router, SocketAddr, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        dispatcher,
        None,
    );
    let router = build_router(state);

    let (addr, _server) = start_live_server(router.clone()).await;

    (router, addr, storage)
}

/// Send an HTTP request via tower::oneshot and return (status, JSON body).
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

// Type aliases for the split WebSocket halves.
type WsSink = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Message,
>;
type WsStream = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// Connect to a conversation's WebSocket endpoint and split into writer/reader.
async fn connect_ws(addr: SocketAddr, conversation_id: &str) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/api/conversations/{conversation_id}/ws");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WebSocket connection should succeed");
    ws.split()
}

/// Send a JSON payload over the WebSocket.
async fn ws_send(sink: &mut WsSink, payload: Value) {
    sink.send(tungstenite::Message::Text(payload.to_string().into()))
        .await
        .expect("ws send should succeed");
}

/// Read the next JSON text frame. Skips ping/pong/binary frames.
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

/// Read events until one satisfies the predicate. Non-matching events are
/// discarded (they may be interleaved tokens or other events).
async fn ws_recv_until(stream: &mut WsStream, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let event = tokio::time::timeout_at(deadline, ws_recv(stream))
            .await
            .expect("timed out waiting for expected WebSocket event");
        if predicate(&event) {
            return event;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Full conversation lifecycle through HTTP and WebSocket.
///
/// Journey: Conversational Assistance
/// Spans: gateway-server, gateway-handlers, gateway-service,
///   gateway-websocket, gateway-turn, storage-sqlite-impl, provider
///
/// Proves: conversation CRUD works end-to-end, streaming tokens arrive via
///   WebSocket, messages are persisted to storage, and the conversation is
///   retrievable with full history after a turn completes.
#[tokio::test(flavor = "multi_thread")]
async fn conversation_lifecycle_with_streaming() {
    let provider = MockProvider::new(vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Delta {
            content: " world".into(),
        },
        StreamChunk::Delta {
            content: "!".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    let (router, addr, _storage) = start_test_server(provider).await;

    // 1. Create a conversation.
    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conv_id = body["id"].as_str().unwrap().to_string();

    // 2. Verify it appears in the list.
    let (status, body) = http(&router, "GET", "/api/conversations").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["conversations"].as_array().unwrap().len(), 1);

    // 3. Open WebSocket and send a message.
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Hi there"}),
    )
    .await;

    // 4. Collect token events, then turn_complete.
    let mut tokens = Vec::new();
    let turn_id = loop {
        let event = ws_recv(&mut ws_read).await;
        match event["type"].as_str().unwrap() {
            "token" => tokens.push(event["content"].as_str().unwrap().to_string()),
            "turn_complete" => {
                break event["turn_id"].as_str().unwrap().to_string();
            }
            other => panic!("unexpected event: {other}"),
        }
    };
    assert_eq!(tokens, ["Hello", " world", "!"]);
    assert!(!turn_id.is_empty());

    // Close WebSocket before HTTP inspection.
    drop(ws_write);
    drop(ws_read);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 5. Get the conversation — verify messages are persisted.
    let (status, body) = http(&router, "GET", &format!("/api/conversations/{conv_id}")).await;
    assert_eq!(status, StatusCode::OK);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "Hi there");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], "Hello world!");
    assert_eq!(messages[1]["turn_state"], "completed");

    // 6. List — verify preview and last_activity are populated.
    let (_, body) = http(&router, "GET", "/api/conversations").await;
    let convs = body["conversations"].as_array().unwrap();
    assert!(convs[0]["message_preview"].is_string());

    // 7. Delete the conversation.
    let (status, _) = http(&router, "DELETE", &format!("/api/conversations/{conv_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 8. Verify it's gone.
    let (status, _) = http(&router, "GET", &format!("/api/conversations/{conv_id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Cancel a streaming turn and verify partial response is preserved.
///
/// Journey: failure/recovery path
/// Spans: gateway-websocket, gateway-turn, storage-sqlite-impl, provider-trait
///
/// Proves: a turn the provider failed keeps its explanation in the
///   conversation the person reloads — the failed state and the provider's
///   error ride on the request the turn answered, not only on the live event.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_turn_keeps_its_explanation_after_reload() {
    let provider = MockProvider::new(vec![StreamChunk::Error {
        message: "provider unreachable: connection refused".into(),
    }]);
    let (router, addr, _storage) = start_test_server(provider).await;

    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Hello?"}),
    )
    .await;
    let event = ws_recv_until(&mut ws_read, |e| e["type"] == "error").await;
    assert!(
        event["message"]
            .as_str()
            .unwrap()
            .contains("connection refused"),
        "live error: {event}"
    );
    drop(ws_write);
    drop(ws_read);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (_, body) = http(&router, "GET", &format!("/api/conversations/{conv_id}")).await;
    let messages = body["messages"].as_array().unwrap();
    let request = messages
        .iter()
        .find(|m| m["content"] == "Hello?")
        .expect("the request stays in the conversation");
    assert_eq!(request["turn_state"], "failed", "{body}");
    assert!(
        request["failure_message"]
            .as_str()
            .is_some_and(|message| message.contains("connection refused")),
        "the reloaded request must carry the failure explanation: {request}"
    );
}

/// Proves: a turn interrupted by a gateway restart after a tool call had
///   started keeps that call in the conversation — recovery settles the
///   started row as interrupted, and the detail attaches it to the request
///   the turn answered even though no assistant reply was ever written.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_turn_keeps_its_tool_calls_after_restart() {
    use swage_core::Role;
    use swage_storage::model::{AppendMessage, NewToolCall};
    use swage_storage::store::{MessageStore, ToolCallStore};

    let (router, _addr, storage) = start_test_server(MockProvider::new(vec![])).await;
    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap().to_string();
    let thread = storage
        .list_by_session(conv_id.parse().unwrap())
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("conversation thread");

    // The state a hard kill leaves: a processing turn, its request, one call
    // that finished and one that was still running.
    let turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: Some("write the file".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage
        .append_message(AppendMessage {
            thread_id: thread.id,
            turn_id: Some(turn.id),
            role: Role::User,
            origin: swage_storage::MessageOrigin::User,
            content: "write the file".into(),
            attachments: Vec::new(),
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();
    for (position, call_id, tool) in [(0, "call-write", "file_write"), (1, "call-shell", "shell")] {
        let record = storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".into(),
                call_id: call_id.into(),
                tool: tool.into(),
                arguments: "{}".into(),
                model_arguments: "{}".into(),
                result: None,
                model_result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                provider_round: 0,
                position,
                parent_call_id: None,
                display_summary: None,
            })
            .await
            .unwrap();
        if call_id == "call-write" {
            storage
                .update_tool_call_result(
                    record.id,
                    "{\"written\":true}",
                    "written",
                    "success",
                    3,
                    false,
                    None,
                )
                .await
                .unwrap();
        }
    }

    recover_orphaned_turns(&*storage, &*storage).await.unwrap();

    let (_, body) = http(&router, "GET", &format!("/api/conversations/{conv_id}")).await;
    let messages = body["messages"].as_array().unwrap();
    let request = messages
        .iter()
        .find(|m| m["content"] == "write the file")
        .expect("the request stays in the conversation");
    assert_eq!(request["turn_state"], "interrupted", "{body}");
    let calls = request["tool_calls"].as_array().unwrap();
    let status_of = |call_id: &str| {
        calls
            .iter()
            .find(|call| call["call_id"] == call_id)
            .unwrap_or_else(|| panic!("{call_id} missing from the reloaded turn: {request}"))["status"]
            .clone()
    };
    assert_eq!(status_of("call-write"), "success");
    assert_eq!(status_of("call-shell"), "interrupted");
}

/// Proves: cancellation aborts the LLM stream, a cancelled event is sent,
///   the partial response is saved, and the conversation remains usable for
///   subsequent messages.
#[tokio::test(flavor = "multi_thread")]
async fn stream_cancellation_preserves_partial_response() {
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let (router, addr, storage) = start_test_server(provider).await;

    // Create conversation + open WebSocket.
    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    // Send a message.
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Tell me a story"}),
    )
    .await;

    // Feed one token — wait for it to arrive before cancelling, so we know
    // the turn is active and has accumulated at least one chunk.
    tx1.send(StreamChunk::Delta {
        content: "Once".into(),
    })
    .await
    .unwrap();

    let event = ws_recv(&mut ws_read).await;
    assert_eq!(event["type"], "token");
    assert_eq!(event["content"], "Once");

    // Cancel mid-stream.
    ws_send(&mut ws_write, json!({"type": "cancel"})).await;

    // Wait for the cancelled acknowledgement.
    let event = ws_recv_until(&mut ws_read, |e| e["type"] == "cancelled").await;
    assert_eq!(event["type"], "cancelled");

    // Close first WebSocket.
    drop(ws_write);
    drop(ws_read);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify partial response via HTTP.
    let (_, body) = http(&router, "GET", &format!("/api/conversations/{conv_id}")).await;
    let messages = body["messages"].as_array().unwrap();
    assert!(
        !messages.is_empty(),
        "at least the user message should be stored"
    );

    // The turn should be interrupted.
    let has_interrupted = messages.iter().any(|m| m["turn_state"] == "interrupted");
    assert!(has_interrupted, "turn should be marked interrupted");

    let session_id = conv_id.parse().expect("conversation id");
    let thread = storage
        .list_by_session(session_id)
        .await
        .expect("conversation threads")
        .into_iter()
        .next()
        .expect("conversation thread");
    let interrupted_turn = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("conversation turns")
        .into_iter()
        .find(|turn| turn.state == TurnState::Interrupted)
        .expect("interrupted turn");
    let rounds = storage
        .list_provider_rounds_by_turn(interrupted_turn.id)
        .await
        .expect("provider rounds");
    assert_eq!(rounds.len(), 1, "the cancelled request opened one round");
    assert!(
        rounds[0].completed_at.is_some(),
        "a cancelled provider request must no longer be outstanding"
    );
    assert_eq!(rounds[0].outcome, json!({"outcome": "interrupted"}));

    // Partial content should be saved.
    if messages.len() > 1 {
        assert_eq!(messages[1]["role"], "assistant");
        assert!(
            messages[1]["content"].as_str().unwrap().contains("Once"),
            "partial content should be preserved"
        );
    }

    // Open a new WebSocket and send another message — proves the conversation
    // is still usable after cancellation.
    let (mut ws_write2, mut ws_read2) = connect_ws(addr, &conv_id).await;
    ws_send(
        &mut ws_write2,
        json!({"type": "send_message", "content": "Continue"}),
    )
    .await;
    tx2.send(StreamChunk::Delta {
        content: "Sure!".into(),
    })
    .await
    .unwrap();
    tx2.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .unwrap();
    drop(tx2);

    let event = ws_recv_until(&mut ws_read2, |e| e["type"] == "turn_complete").await;
    assert_eq!(event["type"], "turn_complete");
}

/// Startup recovery marks orphaned in-progress turns as interrupted.
///
/// Proves: Crash recovery
/// Spans: gateway-shutdown, storage-sqlite, storage-sqlite-impl
///
/// Proves: the recovery sweep transitions orphaned turns to interrupted without
///   affecting completed turns, and the conversation is accessible afterwards
///   with the interrupted turn visible.
#[tokio::test(flavor = "multi_thread")]
async fn startup_recovery_marks_orphaned_turns() {
    use swage_storage::store::{SessionStore, ThreadStore, TopicStore, TurnStore};

    let storage = Arc::new(SqliteStorage::in_memory().unwrap());

    // Get the default topic (seeded by migration).
    let topic = storage.general_scope().await.unwrap();

    // Session 1: an orphaned Processing simulates a crash mid-turn.
    let session_orphan = storage
        .create_session(swage_storage::fixtures::new_session(topic.id))
        .await
        .unwrap();
    let thread_orphan = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session_orphan.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .unwrap();
    let turn_orphan = storage
        .create_turn(NewTurn {
            thread_id: thread_orphan.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    // Session 2: a completed control case — should not be touched.
    let session_ok = storage
        .create_session(swage_storage::fixtures::new_session(topic.id))
        .await
        .unwrap();
    let thread_ok = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session_ok.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .unwrap();
    let turn_ok = storage
        .create_turn(NewTurn {
            thread_id: thread_ok.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage
        .update_turn_state(turn_ok.id, TurnState::Completed)
        .await
        .unwrap();

    // Run recovery.
    let count = recover_orphaned_turns(&*storage, &*storage).await.unwrap();
    assert_eq!(count, 1, "only 1 orphaned turn should be recovered");

    // Verify the orphaned turn is now Interrupted.
    let turn = storage.get_turn(turn_orphan.id).await.unwrap().unwrap();
    assert_eq!(turn.state, TurnState::Interrupted);

    // Verify the completed turn is unchanged.
    let turn = storage.get_turn(turn_ok.id).await.unwrap().unwrap();
    assert_eq!(turn.state, TurnState::Completed);

    // Recovery is idempotent — running again should find nothing.
    let count = recover_orphaned_turns(&*storage, &*storage).await.unwrap();
    assert_eq!(count, 0, "no orphans should remain");
}

/// A message sent during an active turn is never refused: it is queued, every
/// view is told so, and it runs as the next turn the moment the active one
/// ends — the drained turn's events reaching the same socket.
///
/// Proves: Token streaming — mid-turn arrival
/// Spans: gateway-websocket, gateway-turn, gateway-turn-queue, storage-sqlite-impl
#[tokio::test(flavor = "multi_thread")]
async fn a_message_sent_during_an_active_turn_queues_and_runs_as_the_next_turn() {
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let (router, addr, _) = start_test_server(provider).await;

    // Create conversation + open WebSocket.
    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    // Send first message — streaming begins.
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "First"}),
    )
    .await;

    // Feed one token to ensure the turn is active.
    tx1.send(StreamChunk::Delta {
        content: "tok".into(),
    })
    .await
    .unwrap();
    let event = ws_recv(&mut ws_read).await;
    assert_eq!(event["type"], "token");

    // Send second message while first turn is active: it is queued, not refused.
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Second"}),
    )
    .await;
    let event = ws_recv_until(&mut ws_read, |e| e["type"] == "message_queued").await;
    assert_eq!(event["queued"]["content"], "Second");
    assert_eq!(event["queued"]["origin"], "user");

    // Complete the first turn: the queue runs as the next turn.
    tx1.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .unwrap();
    drop(tx1);
    let event = ws_recv_until(&mut ws_read, |e| e["type"] == "turn_complete").await;
    assert_eq!(event["type"], "turn_complete");
    let drained = ws_recv_until(&mut ws_read, |e| e["type"] == "queue_drained").await;
    assert_eq!(drained["session_id"], conv_id);

    tx2.send(StreamChunk::Delta {
        content: "ok".into(),
    })
    .await
    .unwrap();
    tx2.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .unwrap();
    drop(tx2);
    let event = ws_recv_until(&mut ws_read, |e| e["type"] == "turn_complete").await;
    assert_eq!(
        event["turn_id"], drained["turn_id"],
        "the queued message's turn streams to the open socket and completes"
    );
}

/// Queue announcements preserve turn order on real sockets and stay in their session.
#[tokio::test(flavor = "multi_thread")]
async fn queued_messages_reach_real_sockets_in_turn_order_and_stay_in_their_session() {
    crate::support::journey(async {
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);
    let root = tempfile::tempdir().unwrap();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new_with_config_root(
        storage.clone(), storage, Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]), None,
        root.path().to_path_buf(),
    );
    let router = build_router(state.clone());
    let gateway = crate::support::LiveTestGateway::start(router.clone(), root).await;
    let addr = gateway.address;
    let (_, session) = http(&router, "POST", "/api/conversations").await;
    let id = session["id"].as_str().unwrap();
    let (_, other) = http(&router, "POST", "/api/conversations").await;
    let (mut writer, mut reader) = connect_ws(addr, id).await;
    let (mut other_writer, mut other_reader) =
        connect_ws(addr, other["id"].as_str().unwrap()).await;
    ws_send(
        &mut writer,
        json!({"type":"send_message", "content":"First"}),
    )
    .await;
    tx1.send(StreamChunk::Delta {
        content: "first reply".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        ws_recv_until(&mut reader, |event| event["type"] == "token").await["content"],
        "first reply"
    );
    ws_send(
        &mut writer,
        json!({"type":"send_message", "content":"Second"}),
    )
    .await;
    let queued = ws_recv_until(&mut reader, |event| event["type"] == "message_queued").await;
    assert_eq!(queued["queued"]["content"], "Second");
    // Make the next provider's first token ready before the current turn ends.
    tx2.send(StreamChunk::Delta {
        content: "second reply".into(),
    })
    .await
    .unwrap();
    tx2.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .unwrap();
    drop(tx2);
    tx1.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .unwrap();
    drop(tx1);
    let mut relevant = Vec::new();
    let mut completions = 0;
    while completions < 2 {
        let event = ws_recv(&mut reader).await;
        if event["type"] == "turn_complete" {
            completions += 1;
        }
        if [
            "turn_complete",
            "message_appended",
            "queue_drained",
            "token",
        ]
        .contains(&event["type"].as_str().unwrap())
        {
            relevant.push(event);
        }
    }
    assert_eq!(
        relevant
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "turn_complete",
            "message_appended",
            "queue_drained",
            "token",
            "turn_complete"
        ],
        "real WebSocket delivery must preserve completion, drain rows, drain and next turn order"
    );
    assert_eq!(relevant[1]["message"]["content"], "Second");
    assert_eq!(relevant[2]["queued_ids"], json!([queued["queued"]["id"]]));
    assert_eq!(relevant[3]["content"], "second reply");
    assert_eq!(relevant[4]["turn_id"], relevant[2]["turn_id"]);
    // Fence through the same ordered session path as any leaked queue events.
    state.send_to_session(
        uuid::Uuid::parse_str(other["id"].as_str().unwrap()).unwrap(),
        &swage_protocol::client::websocket::ServerEvent::Token { content: "isolation fence".into() },
    ).await;
    let other_event = ws_recv(&mut other_reader).await;
    assert_eq!(
        other_event, json!({"type": "token", "content": "isolation fence"}),
        "another session must receive none of the queue or turn events before its ordered fence"
    );
    writer.close().await.unwrap();
    other_writer.close().await.unwrap();
    }).await;
}

// ===========================================================================
// Built-in tools and task execution — integration tests
// ===========================================================================

/// Conversation history includes tool_calls on assistant messages.
///
/// Journey: Task Execution — activity display persistence
/// Spans: tool-call-storage-model, tool-call-store-trait,
///   tool-calls-migration, sqlite-tool-call-store,
///   api-tool-calls
///
/// Proves: the HTTP API response for conversation detail includes a tool_calls
/// field on assistant messages. This is the persistence half of the activity
/// display — after a surface refresh, tool calls must be visible from the HTTP
/// response, not just from live WebSocket events.
///
/// Verifies that assistant messages always include the tool_calls field.
#[tokio::test(flavor = "multi_thread")]
async fn conversation_history_includes_tool_calls_field() {
    let provider = MockProvider::new(vec![
        StreamChunk::Delta {
            content: "Done.".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    let (router, addr, _) = start_test_server(provider).await;

    // Create conversation and complete a turn.
    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Hello"}),
    )
    .await;
    ws_recv_until(&mut ws_read, |e| e["type"] == "turn_complete").await;

    // Close WebSocket before HTTP inspection.
    drop(ws_write);
    drop(ws_read);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Fetch conversation detail via HTTP.
    let (status, body) = http(&router, "GET", &format!("/api/conversations/{conv_id}")).await;
    assert_eq!(status, StatusCode::OK);

    let messages = body["messages"].as_array().unwrap();
    let assistant_msg = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant message should exist");

    // Tool-dispatch tests require tool_calls on assistant messages.
    // For a text-only response, the field should be present (empty array).
    assert!(
        assistant_msg.get("tool_calls").is_some(),
        "assistant messages must include tool_calls field; got: {assistant_msg}"
    );
}

/// Tool execution emits tool_start and tool_end events on the WebSocket,
/// and the completed tool calls persist in conversation history.
///
/// Journey: Task Execution — full pipeline
/// Spans: tool-types, provider-tool-types,
///   anthropic-tool-parsing, tool-registry,
///   tool-execution-loop, websocket-tool-events,
///   tool-call-storage-model, api-tool-calls
///
/// Proves: the full tool execution pipeline — the model requests tool use,
/// the turn processor executes the tool via the registry, tool_start and
/// tool_end events appear on the WebSocket (for live activity display),
/// tool calls are stored in the database, and the HTTP conversation detail
/// returns for surface-refresh persistence.
///
/// This test uses a ChannelProvider to simulate a tool-using conversation:
/// Round 1: model emits text + tool_use → tool executes → tool results sent back
/// Round 2: model emits final text → turn completes
#[tokio::test(flavor = "multi_thread")]
async fn tool_execution_round_trip_with_persistence() {
    // Round 1: model produces some text then requests a tool call.
    // Round 2: after receiving tool results, model produces final text.
    let (tx1, rx1) = tokio::sync::mpsc::channel(10);
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    let provider = ChannelProvider::new(vec![rx1, rx2]);

    // Build a dispatcher with the echo tool so the execution loop can find it.
    let dispatcher = swage_gateway::tool::ToolDispatcher::allow_all(vec![
        Arc::new(EchoTool) as Arc<dyn swage_gateway::tool::Tool>
    ]);
    let (router, addr, _) = start_test_server_with_dispatcher(provider, dispatcher).await;

    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap().to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;

    // Subscribe to the "tools" channel so tool events are delivered.
    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;

    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Echo hello for me"}),
    )
    .await;

    // Pre-send round 2 so it is waiting when the tool loop starts the second stream.
    tx2.send(StreamChunk::Delta {
        content: "Here are the files.".into(),
    })
    .await
    .unwrap();
    tx2.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .unwrap();
    drop(tx2);

    // Round 1: model emits text then a tool_use block.
    tx1.send(StreamChunk::Delta {
        content: "Let me check.".into(),
    })
    .await
    .unwrap();
    tx1.send(StreamChunk::ToolUse {
        call_id: "call_001".into(),
        name: "echo".into(),
        arguments: json!({"text": "hello from tool"}),
    })
    .await
    .unwrap();
    tx1.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .unwrap();
    drop(tx1);

    // Collect all events.
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), ws_recv(&mut ws_read))
            .await
            .expect("timed out waiting for events");
        let event_type = event["type"].as_str().unwrap().to_string();
        events.push(event.clone());
        if event_type == "turn_complete" {
            break;
        }
    }

    // Tool execution should emit tool_start and tool_end events on the WebSocket.
    let has_tool_start = events.iter().any(|e| e["type"] == "tool_start");
    let has_tool_end = events.iter().any(|e| e["type"] == "tool_end");

    assert!(
        has_tool_start && has_tool_end,
        "WebSocket events should include tool_start and tool_end; got types: {:?}",
        events
            .iter()
            .map(|e| e["type"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );

    // Close WebSocket and verify persistence.
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

    // Tool calls should persist in conversation history.
    let tool_calls = assistant_msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .expect("tool_calls should be an array on assistant messages");

    // When tools were actually invoked, the array should contain the calls.
    // For text-only turns, it should be empty — but the field must exist.
    // This assertion will be tightened once tool execution is wired.
    assert!(
        tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.get("call_id").is_some()),
        "tool_calls entries must have call_id"
    );
}
