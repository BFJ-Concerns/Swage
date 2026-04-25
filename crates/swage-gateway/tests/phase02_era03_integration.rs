//! Cross-task integration tests for Era 0.3 Phase 02: Output Schema Validation.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the full stack is wired together.
//!
//! ## What these tests prove
//!
//! | Test | Journey / Proof | Spans tasks |
//! |------|----------------|-------------|
//! | `valid_output_delivers_structured_output_on_turn_complete` | Typed Agent Output — valid first attempt | config [0oq4, 8xkg] + extraction [6gak] + validation [xya3] + events [6y28] + storage [7qwi] + gateway [z4ov, i59a] |
//! | `invalid_output_retries_with_visible_event_then_succeeds` | Typed Agent Output — retry succeeds | extraction [6gak] + validation [xya3] + events [vd37, 6y28] + gateway [z4ov, i59a] |
//! | `zero_retries_causes_immediate_failure` | Zero-retry failure proof | config [0oq4, 8xkg] + validation [xya3] + events [vd37] + gateway [z4ov, i59a] |
//! | `no_schema_preserves_existing_behaviour` | No-schema preserved behaviour | config absence + gateway [i59a, z4ov] |
//! | `tokens_stream_during_both_initial_and_retry_attempts` | Streaming during validation proof | validation [xya3] + gateway [i59a] |

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

use swage_config::ResolvedConfig;
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{SqliteStorage, store::BenchStore};

// Serialise tests that mutate SWAGE_HOME — it's process-wide state.
static SWAGE_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard that points SWAGE_HOME at a non-existent path so the gateway
/// falls back to the injected `state.config` instead of loading from disk.
struct SwageHomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl SwageHomeGuard {
    fn isolate() -> Self {
        let lock = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Safety: test-only env mutation; set_var is unsafe in edition 2024.
        unsafe { std::env::set_var("SWAGE_HOME", "/tmp/swage_nonexistent_test_root") };
        Self { _lock: lock }
    }
}

impl Drop for SwageHomeGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("SWAGE_HOME") };
    }
}

// ---------------------------------------------------------------------------
// Mock provider — multi-round, returns different responses per stream() call
// ---------------------------------------------------------------------------

/// Each inner Vec is one LLM round's output. Successive stream() calls pop
/// the next round. For schema retry tests, the first round produces invalid
/// output and the second produces valid output.
struct MockProvider {
    rounds: std::sync::Mutex<Vec<Vec<StreamChunk>>>,
}

impl MockProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: std::sync::Mutex::new(rounds),
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
        let mut rounds = self.rounds.lock().unwrap();
        let chunks = if rounds.is_empty() {
            vec![StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            }]
        } else {
            rounds.remove(0)
        };
        Box::pin(futures::stream::iter(chunks))
    }
}

// ---------------------------------------------------------------------------
// Test helpers — match the phase01_era03 patterns
// ---------------------------------------------------------------------------

async fn start_test_server_with_config(
    provider: impl LlmProvider + 'static,
    config: ResolvedConfig,
) -> (Router, SocketAddr, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let dispatcher = swage_gateway::tool::ToolDispatcher::allow_all(vec![]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn BenchStore>,
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

async fn ws_collect_events(stream: &mut WsStream, timeout: Duration) -> Vec<Value> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, ws_recv_lenient(stream)).await {
        events.push(event);
    }
    events
}

/// Build a ResolvedConfig with an output schema and optional retry override.
fn config_with_schema(schema: Value, max_retries: u32) -> ResolvedConfig {
    let mut config = ResolvedConfig::default();
    config.output.schema = Some(schema);
    config.output.max_retries = max_retries;
    config
}

// ---------------------------------------------------------------------------
// Cross-task integration tests
// ---------------------------------------------------------------------------

/// Configure a JSON Schema, send a message producing valid JSON, and verify
/// that `TurnComplete` carries `structured_output` AND the turn record in
/// storage contains the same value.
///
/// This is the happy path of Journey 1 (Typed Agent Output): model produces
/// valid JSON on the first attempt. The test traverses the full stack:
/// config → agent construction → extraction → validation → event emission →
/// storage persistence → WebSocket delivery.
///
/// Before implementation: FAILS — ResolvedConfig has no `output` field.
/// After implementation: TurnComplete includes structured_output; turn record
/// in storage contains the validated JSON.
#[tokio::test(flavor = "multi_thread")]
async fn valid_output_delivers_structured_output_on_turn_complete() {
    let _guard = SwageHomeGuard::isolate();
    // Schema: require an object with a "result" string field.
    let schema = json!({
        "type": "object",
        "properties": {
            "result": { "type": "string" }
        },
        "required": ["result"]
    });
    let config = config_with_schema(schema, 3);

    // Provider returns valid JSON matching the schema.
    let valid_json = r#"{"result": "success"}"#;
    let provider = MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: valid_json.to_string(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]);

    let (router, addr, storage) = start_test_server_with_config(provider, config).await;

    // Create conversation and connect WebSocket. Subscribe to context channel
    // so we receive usage fields on TurnComplete.
    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap();
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;

    // Send a message to trigger the turn.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "Give me JSON"}),
    )
    .await;

    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // TurnComplete should carry structured_output with the validated JSON.
    let turn_complete = events.iter().find(|e| e["type"] == "turn_complete");
    assert!(
        turn_complete.is_some(),
        "expected turn_complete event; got events: {events:?}"
    );
    let tc = turn_complete.unwrap();
    assert!(
        tc.get("structured_output").is_some(),
        "turn_complete should include structured_output; got: {tc:?}"
    );
    let structured = &tc["structured_output"];
    assert_eq!(
        structured["result"], "success",
        "structured_output should contain the validated JSON value"
    );

    // No schema retry or failure events should have been emitted.
    assert!(
        !events.iter().any(|e| e["type"] == "output_schema_retry"),
        "no retry events expected for valid first-attempt output"
    );
    assert!(
        !events.iter().any(|e| e["type"] == "output_schema_failed"),
        "no failure events expected for valid output"
    );

    // Verify the turn record in storage also has structured_output.
    let turn_id = tc["turn_id"].as_str().unwrap();
    let turn_uuid = uuid::Uuid::parse_str(turn_id).unwrap();
    use swage_storage::store::TurnStore;
    let turn = TurnStore::get_turn(&*storage, turn_uuid)
        .await
        .unwrap()
        .unwrap();
    assert!(
        turn.structured_output.is_some(),
        "turn record should persist structured_output"
    );
    assert_eq!(
        turn.structured_output.unwrap(),
        json!({"result": "success"}),
        "persisted structured_output should match validated JSON"
    );
}

/// Configure a strict schema, have the first response fail validation, verify
/// an `OutputSchemaRetry` event is emitted, then the second response passes
/// and `TurnComplete` carries `structured_output`.
///
/// This tests the retry path of Journey 1: model produces invalid output,
/// receives ephemeral feedback, and self-corrects on the next attempt.
///
/// Before implementation: FAILS — no retry logic exists.
/// After implementation: retry event visible to subscribed clients; second
/// attempt succeeds; structured_output present on completion.
#[tokio::test(flavor = "multi_thread")]
async fn invalid_output_retries_with_visible_event_then_succeeds() {
    let _guard = SwageHomeGuard::isolate();
    let schema = json!({
        "type": "object",
        "properties": {
            "score": { "type": "number" }
        },
        "required": ["score"]
    });
    let config = config_with_schema(schema, 3);

    // Round 1: invalid output (score is a string, not a number).
    // Round 2: valid output (score is a number).
    let provider = MockProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: r#"{"score": "high"}"#.to_string(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: r#"{"score": 95}"#.to_string(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    ]);

    let (router, addr, _storage) = start_test_server_with_config(provider, config).await;

    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap();
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;

    // Subscribe to context channel to receive retry events.
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "Score this"}),
    )
    .await;

    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // Should have received an OutputSchemaRetry event for the first failed attempt.
    let retry_event = events.iter().find(|e| e["type"] == "output_schema_retry");
    assert!(
        retry_event.is_some(),
        "expected output_schema_retry event; got events: {events:?}"
    );
    let re = retry_event.unwrap();
    assert_eq!(re["attempt"], 1, "first retry should be attempt 1");
    assert!(
        re["errors"].is_array() && !re["errors"].as_array().unwrap().is_empty(),
        "retry event should include validation errors"
    );

    // TurnComplete should carry the corrected structured_output.
    let turn_complete = events.iter().find(|e| e["type"] == "turn_complete");
    assert!(
        turn_complete.is_some(),
        "expected turn_complete after successful retry; got events: {events:?}"
    );
    let tc = turn_complete.unwrap();
    assert_eq!(
        tc["structured_output"]["score"], 95,
        "structured_output should contain the validated (corrected) JSON"
    );

    // No failure event — the retry succeeded.
    assert!(
        !events.iter().any(|e| e["type"] == "output_schema_failed"),
        "no failure event expected when retry succeeds"
    );
}

/// Configure `max_retries = 0`, send a message producing invalid JSON, and
/// verify immediate `OutputSchemaFailed` with no retry events and no
/// `structured_output` on `TurnComplete`.
///
/// Proves the zero-retry edge case: when max_retries is 0, the first
/// validation failure is final. The turn fails with a schema violation error.
///
/// Before implementation: FAILS — config has no output section.
/// After implementation: OutputSchemaFailed emitted immediately; turn_complete
/// omits structured_output.
#[tokio::test(flavor = "multi_thread")]
async fn zero_retries_causes_immediate_failure() {
    let _guard = SwageHomeGuard::isolate();
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "integer" }
        },
        "required": ["answer"]
    });
    // max_retries = 0: no retries, first failure is final.
    let config = config_with_schema(schema, 0);

    // Provider returns invalid output — answer is a string, not an integer.
    let provider = MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: r#"{"answer": "forty-two"}"#.to_string(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]);

    let (router, addr, _storage) = start_test_server_with_config(provider, config).await;

    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap();
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;

    // Subscribe to see all events. OutputSchemaFailed should arrive regardless
    // of subscription state, but subscribing ensures we see everything.
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "Answer me"}),
    )
    .await;

    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // OutputSchemaFailed should be emitted immediately (no retries).
    let failed_event = events.iter().find(|e| e["type"] == "output_schema_failed");
    assert!(
        failed_event.is_some(),
        "expected output_schema_failed event; got events: {events:?}"
    );
    let fe = failed_event.unwrap();
    assert_eq!(
        fe["attempts"], 1,
        "should report 1 attempt (the initial attempt that failed)"
    );
    assert!(
        fe["errors"].is_array() && !fe["errors"].as_array().unwrap().is_empty(),
        "failure event should include validation errors from the final attempt"
    );

    // No retry events — max_retries is 0.
    assert!(
        !events.iter().any(|e| e["type"] == "output_schema_retry"),
        "no retry events expected when max_retries = 0"
    );

    // The turn should have failed — either no turn_complete or an error event.
    // The agent's response is still persisted for debugging, but structured_output
    // is absent.
    let turn_complete = events.iter().find(|e| e["type"] == "turn_complete");
    if let Some(tc) = turn_complete {
        // If a TurnComplete is emitted for a failed schema, it must NOT carry
        // structured_output.
        assert!(
            tc.get("structured_output").is_none() || tc["structured_output"].is_null(),
            "turn_complete after schema failure must not carry structured_output"
        );
    }
}

/// Send a message to an agent with no output schema configured. Verify
/// that TurnComplete has no `structured_output`, no schema-related events
/// are emitted, and tokens stream normally — identical to pre-schema behaviour.
///
/// Proves backward compatibility: agents without `[output]` config behave
/// exactly as they did before output schema validation was added.
///
/// Before implementation: PASSES (trivially, once the types compile).
/// After implementation: continues to pass.
#[tokio::test(flavor = "multi_thread")]
async fn no_schema_preserves_existing_behaviour() {
    let _guard = SwageHomeGuard::isolate();
    // Default config — no output schema.
    let config = ResolvedConfig::default();

    let provider = MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "Hello, ".to_string(),
        },
        StreamChunk::Delta {
            content: "world!".to_string(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]);

    let (router, addr, _storage) = start_test_server_with_config(provider, config).await;

    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap();
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;
    ws_send(&mut sink, json!({"type": "send_message", "content": "Hi"})).await;

    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // Tokens should stream normally.
    let tokens: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "token")
        .filter_map(|e| e["content"].as_str())
        .collect();
    assert_eq!(
        tokens,
        vec!["Hello, ", "world!"],
        "tokens should stream unchanged"
    );

    // TurnComplete should be present without structured_output.
    let turn_complete = events.iter().find(|e| e["type"] == "turn_complete");
    assert!(
        turn_complete.is_some(),
        "expected turn_complete; got events: {events:?}"
    );
    let tc = turn_complete.unwrap();
    assert!(
        tc.get("structured_output").is_none() || tc["structured_output"].is_null(),
        "no structured_output expected when no schema is configured; got: {tc:?}"
    );

    // No schema-related events at all.
    assert!(
        !events.iter().any(|e| e["type"] == "output_schema_retry"),
        "no retry events without a schema"
    );
    assert!(
        !events.iter().any(|e| e["type"] == "output_schema_failed"),
        "no failure events without a schema"
    );
}

/// Configure a schema and have the first response fail validation. Verify
/// that tokens stream for both the initial attempt and the retry attempt,
/// proving that validation does not block the token stream.
///
/// Proves the streaming-during-validation requirement: each retry attempt
/// streams independently, and the client receives tokens from each attempt.
///
/// Before implementation: FAILS — no retry streaming exists.
/// After implementation: tokens from both attempts arrive, separated by
/// the retry event.
#[tokio::test(flavor = "multi_thread")]
async fn tokens_stream_during_both_initial_and_retry_attempts() {
    let _guard = SwageHomeGuard::isolate();
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        },
        "required": ["name"]
    });
    let config = config_with_schema(schema, 3);

    // Round 1: invalid JSON (missing "name" field) — streamed as tokens.
    // Round 2: valid JSON — streamed as tokens.
    let provider = MockProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "{\"wrong\":".to_string(),
            },
            StreamChunk::Delta {
                content: " \"field\"}".to_string(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "{\"name\":".to_string(),
            },
            StreamChunk::Delta {
                content: " \"Alice\"}".to_string(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    ]);

    let (router, addr, _storage) = start_test_server_with_config(provider, config).await;

    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().unwrap();
    let (mut sink, mut stream) = connect_ws(addr, conv_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "Name?"}),
    )
    .await;

    let events = ws_collect_events(&mut stream, Duration::from_secs(5)).await;

    // Should have tokens from both attempts. The exact content depends on
    // whether the gateway forwards tokens during validation retries.
    let all_tokens: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "token")
        .filter_map(|e| e["content"].as_str())
        .collect();

    // At minimum, tokens from the first attempt and the retry attempt.
    assert!(
        all_tokens.len() >= 4,
        "expected tokens from both attempts (at least 4); got {}: {:?}",
        all_tokens.len(),
        all_tokens,
    );

    // The retry event should appear between the two sets of tokens.
    let retry_event = events.iter().find(|e| e["type"] == "output_schema_retry");
    assert!(
        retry_event.is_some(),
        "expected output_schema_retry event between streaming attempts; got events: {events:?}"
    );

    // And the turn should complete successfully with structured_output.
    let turn_complete = events.iter().find(|e| e["type"] == "turn_complete");
    assert!(
        turn_complete.is_some(),
        "expected turn_complete; got events: {events:?}"
    );
    let tc = turn_complete.unwrap();
    assert_eq!(
        tc["structured_output"]["name"], "Alice",
        "structured_output should carry the validated JSON from the successful retry"
    );
}
