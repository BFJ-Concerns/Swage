use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_agent::context::TurnExecutionContext;
use swage_agent::{AgentBuilder, AgentConfig, AgentEvent, TurnRequest};
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, ProviderError, Role,
    StreamChunk, TokenUsage, ToolCompletionRequest, ToolCompletionResponse,
};
use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::sqlite::SqliteStorage;
use swage_storage::{
    Bench, BenchStore, MessageStore, NewSession, NewThread, NewTurn, Session, SessionStore, Thread,
    ThreadState, ThreadStore, TurnState, TurnStore,
};

struct StreamingProvider {
    rounds: Mutex<Vec<Vec<StreamChunk>>>,
    calls: AtomicUsize,
}

impl StreamingProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: Mutex::new(rounds.into_iter().rev().collect()),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

struct CompactionStreamingProvider {
    stream_rounds: Mutex<Vec<Vec<StreamChunk>>>,
    last_stream_messages: Mutex<Vec<ChatMessage>>,
}

impl CompactionStreamingProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            stream_rounds: Mutex::new(rounds.into_iter().rev().collect()),
            last_stream_messages: Mutex::new(Vec::new()),
        }
    }

    fn last_stream_message_count(&self) -> usize {
        self.last_stream_messages.lock().unwrap().len()
    }
}

#[async_trait]
impl LlmProvider for CompactionStreamingProvider {
    fn model_id(&self) -> &str {
        "compaction-streaming-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(CompletionResponse {
            content: "summary from compacted history".to_string(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("streaming tests should not call complete_with_tools()")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        *self.last_stream_messages.lock().unwrap() = request.messages;
        let chunks = self
            .stream_rounds
            .lock()
            .unwrap()
            .pop()
            .expect("mock provider ran out of streaming rounds");
        Box::pin(stream::iter(chunks))
    }
}

#[async_trait]
impl LlmProvider for StreamingProvider {
    fn model_id(&self) -> &str {
        "streaming-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("streaming tests should not call complete()")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("streaming tests should not call complete_with_tools()")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = self
            .rounds
            .lock()
            .unwrap()
            .pop()
            .expect("mock provider ran out of streaming rounds");
        Box::pin(stream::iter(chunks))
    }
}

struct AppendAfterResponseHook {
    suffix: String,
}

#[async_trait]
impl Hook for AppendAfterResponseHook {
    fn name(&self) -> &str {
        "append-after-response"
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        match event {
            HookEvent::AfterResponse { content } => {
                HookOutcome::ContinueModified(Box::new(HookEvent::AfterResponse {
                    content: format!("{content}{}", self.suffix),
                }))
            }
            _ => HookOutcome::Continue,
        }
    }
}

async fn setup_conversation(agent_id: &str) -> (Arc<SqliteStorage>, Bench, Session, Thread) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory sqlite"));
    let bench = BenchStore::get_or_create_default(storage.as_ref())
        .await
        .expect("default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: agent_id.to_string(),
            metadata: None,
        })
        .await
        .expect("session");
    let thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: agent_id.to_string(),
        },
    )
    .await
    .expect("thread");
    (storage, bench, session, thread)
}

fn test_request(
    bench_id: Uuid,
    session_id: Uuid,
    thread_id: Uuid,
    user_message: &str,
    existing_turn_id: Option<Uuid>,
) -> TurnRequest {
    TurnRequest {
        execution: TurnExecutionContext {
            session_id,
            thread_id,
            turn_id: Uuid::new_v4(),
            bench_id: Some(bench_id),
        },
        user_message: user_message.to_string(),
        assembled_context: vec![ChatMessage::new(Role::User, user_message)],
        parent_turn_id: None,
        existing_turn_id,
        cancellation_token: CancellationToken::new(),
    }
}

fn drain_events(mut rx: mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_persists_completed_response_and_raw_input() {
    let (storage, bench, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "hello ".into(),
        },
        StreamChunk::Delta {
            content: "world".into(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 11,
            output_tokens: 7,
        }),
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let agent = AgentBuilder::new()
        .config(AgentConfig::new("agent-a"))
        .provider(provider)
        .storage(storage.clone())
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(bench.id, session.id, thread.id, "Hello agent", None),
            event_tx,
        )
        .await
        .expect("streaming turn should succeed");

    assert_eq!(outbound.response, "hello world");
    assert_eq!(outbound.input_tokens, 11);
    assert_eq!(outbound.output_tokens, 7);

    let turn = storage
        .get_turn(outbound.turn_id)
        .await
        .expect("turn lookup")
        .expect("turn exists");
    assert_eq!(turn.state, TurnState::Completed);
    assert_eq!(turn.user_input.as_deref(), Some("Hello agent"));
    assert_eq!(turn.assistant_response.as_deref(), Some("hello world"));

    let thread = ThreadStore::get(storage.as_ref(), thread.id)
        .await
        .expect("thread lookup")
        .expect("thread exists");
    assert_eq!(thread.state, ThreadState::Idle);

    let messages = storage
        .list_messages_by_thread(thread.id)
        .await
        .expect("thread messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, swage_core::Role::Assistant);
    assert_eq!(messages[0].content, "hello world");

    let events = drain_events(event_rx);
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::TurnStarted { turn_id } if *turn_id == outbound.turn_id)
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Token { content } if content == "hello "))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::TurnCompleted {
            turn_id,
            input_tokens,
            output_tokens,
            ..
        } if *turn_id == outbound.turn_id && *input_tokens == 11 && *output_tokens == 7
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_compacts_context_before_provider_stream() {
    let (storage, bench, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(CompactionStreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let mut config = AgentConfig::new("agent-a");
    config.compaction_threshold = swage_agent::CompactionThreshold::MessageCount(3);
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider.clone())
        .storage(storage.clone())
        .build()
        .expect("agent");

    let mut request = test_request(bench.id, session.id, thread.id, "latest request", None);
    request.assembled_context = vec![
        ChatMessage::new(Role::System, "system"),
        ChatMessage::new(Role::User, "old user"),
        ChatMessage::new(Role::Assistant, "old assistant"),
        ChatMessage::new(Role::User, "new user"),
        ChatMessage::new(Role::Assistant, "new assistant"),
    ];

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    agent
        .process_turn_streaming(request, event_tx)
        .await
        .expect("streaming turn should succeed");

    assert!(
        provider.last_stream_message_count() <= 3,
        "provider should receive compacted context"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_compaction_failure_marks_turn_failed_and_thread_idle() {
    let (storage, bench, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(CompactionStreamingProvider::new(vec![]));
    let mut config = AgentConfig::new("agent-a");
    config.compaction_threshold = swage_agent::CompactionThreshold::MessageCount(0);
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider)
        .storage(storage.clone())
        .build()
        .expect("agent");

    let mut request = test_request(bench.id, session.id, thread.id, "latest request", None);
    request.assembled_context = vec![ChatMessage::new(Role::System, "preserved system prompt")];

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let error = agent
        .process_turn_streaming(request, event_tx)
        .await
        .expect_err("uncompactable context should fail the turn");

    assert!(
        matches!(error, swage_agent::AgentError::CompactionFailed(_)),
        "expected compaction failure, got {error:?}"
    );

    let turns = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turn list");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].state, TurnState::Failed);

    let thread = ThreadStore::get(storage.as_ref(), thread.id)
        .await
        .expect("thread lookup")
        .expect("thread exists");
    assert_eq!(thread.state, ThreadState::Idle);
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_reuses_existing_pending_turn() {
    let (storage, bench, session, thread) = setup_conversation("agent-a").await;
    let pending_turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "agent-a".into(),
            state: TurnState::Pending,
            user_input: Some("Queued input".into()),
            parent_turn_id: None,
        })
        .await
        .expect("pending turn");
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "reused turn".into(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 3,
            output_tokens: 2,
        }),
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let agent = AgentBuilder::new()
        .config(AgentConfig::new("agent-a"))
        .provider(provider)
        .storage(storage.clone())
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(
                bench.id,
                session.id,
                thread.id,
                "Queued input",
                Some(pending_turn.id),
            ),
            event_tx,
        )
        .await
        .expect("streaming turn should reuse pending turn");

    assert_eq!(outbound.turn_id, pending_turn.id);
    let turns = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turn list");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, pending_turn.id);
    assert_eq!(turns[0].state, TurnState::Completed);
    assert_eq!(turns[0].assistant_response.as_deref(), Some("reused turn"));
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_applies_after_response_hook_and_leak_scan() {
    let (storage, bench, session, thread) = setup_conversation("agent-a").await;
    let raw_secret = "sk-proj-abc123def456ghi789jklmnopqrstuvwxyz1234567890ABCDEFGHIJ";
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: format!("secret: {raw_secret}"),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 5,
            output_tokens: 5,
        }),
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));

    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(AppendAfterResponseHook {
        suffix: " [approved]".into(),
    })));

    let agent = AgentBuilder::new()
        .config(AgentConfig::new("agent-a"))
        .provider(provider)
        .storage(storage.clone())
        .hook_registry(Arc::new(hook_registry))
        .leak_detector(Arc::new(
            LeakDetector::new(vec![]).expect("default leak patterns"),
        ))
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(bench.id, session.id, thread.id, "Check output", None),
            event_tx,
        )
        .await
        .expect("streaming turn should succeed");

    assert!(
        outbound.response.contains("[approved]"),
        "after-response hook suffix missing: {}",
        outbound.response
    );
    assert!(
        !outbound.response.contains(raw_secret),
        "leak detector should redact the raw secret: {}",
        outbound.response
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_schema_retry_reuses_streaming_runtime() {
    let (storage, bench, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "not json".into(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 2,
                output_tokens: 1,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "{\"answer\":\"ok\"}".into(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 3,
                output_tokens: 4,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    ]));
    let config = AgentConfig::new("agent-a").with_output_schema(
        json!({
            "type": "object",
            "properties": {
                "answer": { "type": "string" }
            },
            "required": ["answer"],
            "additionalProperties": false
        }),
        1,
    );
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider.clone())
        .storage(storage)
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(bench.id, session.id, thread.id, "Return JSON", None),
            event_tx,
        )
        .await
        .expect("retry path should recover with valid JSON");

    assert_eq!(provider.call_count(), 2);
    assert_eq!(outbound.structured_output, Some(json!({ "answer": "ok" })));
    assert_eq!(outbound.input_tokens, 5);
    assert_eq!(outbound.output_tokens, 5);

    let events = drain_events(event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::OutputSchemaRetry {
            attempt,
            max_retries,
            ..
        } if *attempt == 1 && *max_retries == 1
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::TurnCompleted {
            structured_output: Some(value),
            ..
        } if *value == json!({ "answer": "ok" })
    )));
}
