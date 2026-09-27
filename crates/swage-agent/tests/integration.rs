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
use swage_agent::{AgentBuilder, AgentConfig, AgentError, AgentEvent, TurnRequest};
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, ProviderError,
    ProviderResponseWitness, Role, StreamChunk, TokenUsage, ToolCompletionRequest,
    ToolCompletionResponse, ToolError,
};
use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::sqlite::SqliteStorage;
use swage_storage::{
    MessageStore, NewSession, NewThread, NewTurn, Session, SessionStore, Thread, ThreadState,
    ThreadStore, ToolCallStore, Topic, TopicStore, TurnState, TurnStore,
};
use swage_tool::dispatch::ToolDispatcher;
use swage_tool::tool::Tool;

struct StreamingProvider {
    rounds: Mutex<Vec<Vec<StreamChunk>>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl StreamingProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: Mutex::new(rounds.into_iter().rev().collect()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<CompletionRequest> {
        self.requests
            .lock()
            .expect("mock provider request lock should not be poisoned")
            .clone()
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
        self.last_stream_messages
            .lock()
            .expect("mock provider message lock should not be poisoned")
            .len()
    }

    fn last_stream_messages(&self) -> Vec<ChatMessage> {
        self.last_stream_messages
            .lock()
            .expect("mock provider message lock should not be poisoned")
            .clone()
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
        *self
            .last_stream_messages
            .lock()
            .expect("mock provider message lock should not be poisoned") = request.messages;
        let chunks = self
            .stream_rounds
            .lock()
            .expect("mock provider stream lock should not be poisoned")
            .pop()
            .expect("mock provider ran out of streaming rounds");
        Box::pin(stream::iter(chunks))
    }
}

struct NativeCountCompactionProvider {
    count_calls: AtomicUsize,
    count_requests: Mutex<Vec<CompletionRequest>>,
    stream_requests: Mutex<Vec<CompletionRequest>>,
}

impl NativeCountCompactionProvider {
    fn new() -> Self {
        Self {
            count_calls: AtomicUsize::new(0),
            count_requests: Mutex::new(Vec::new()),
            stream_requests: Mutex::new(Vec::new()),
        }
    }

    fn count_requests(&self) -> Vec<CompletionRequest> {
        self.count_requests
            .lock()
            .expect("count request lock should not be poisoned")
            .clone()
    }

    fn stream_requests(&self) -> Vec<CompletionRequest> {
        self.stream_requests
            .lock()
            .expect("stream request lock should not be poisoned")
            .clone()
    }
}

#[async_trait]
impl LlmProvider for NativeCountCompactionProvider {
    fn model_id(&self) -> &str {
        "native-count-mock"
    }

    async fn count_tokens(
        &self,
        request: &CompletionRequest,
    ) -> Result<Option<u64>, ProviderError> {
        self.count_requests
            .lock()
            .expect("count request lock should not be poisoned")
            .push(request.clone());
        if self.count_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Some(100))
        } else {
            Err(ProviderError::InvalidResponse(
                "transient native count failure".to_string(),
            ))
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(CompletionResponse {
            content: "brief".to_string(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("compaction test should not call complete_with_tools")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        self.stream_requests
            .lock()
            .expect("stream request lock should not be poisoned")
            .push(request);
        Box::pin(stream::iter(vec![
            StreamChunk::Delta {
                content: "done".to_string(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ]))
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
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("mock provider request lock should not be poisoned")
            .push(request);
        let chunks = self
            .rounds
            .lock()
            .expect("mock provider stream lock should not be poisoned")
            .pop()
            .expect("mock provider ran out of streaming rounds");
        Box::pin(stream::iter(chunks))
    }
}

struct AppendAfterResponseHook {
    suffix: String,
}

struct TruncatedMetadataTool;

struct CountingTool {
    calls: AtomicUsize,
}

impl CountingTool {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "counting"
    }

    fn description(&self) -> &str {
        "Counts each invocation."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(json!({ "call": call }))
    }
}

#[async_trait]
impl Tool for TruncatedMetadataTool {
    fn name(&self) -> &str {
        "truncated_metadata"
    }

    fn description(&self) -> &str {
        "Returns a deliberately shortened result."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        Ok(json!({
            "output": "preview",
            "truncated": true,
            "total_bytes": 4096,
            "follow_up": {
                "tool": "file_read",
                "arguments": {
                    "path": "large.txt",
                    "offset_bytes": 128
                }
            }
        }))
    }
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

async fn setup_conversation(agent_id: &str) -> (Arc<SqliteStorage>, Topic, Session, Thread) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory sqlite"));
    let topic = TopicStore::general_scope(storage.as_ref())
        .await
        .expect("default topic");
    let session = storage
        .create_session(NewSession {
            agent_id: agent_id.to_string(),
            ..swage_storage::fixtures::new_session(topic.id)
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
    (storage, topic, session, thread)
}

fn resolved_config(agent_id: &str) -> AgentConfig {
    AgentConfig::new(agent_id).with_workspace("/tmp/swage-agent-test", vec![])
}

fn test_request(
    topic_id: Uuid,
    session_id: Uuid,
    thread_id: Uuid,
    user_message: &str,
    existing_turn_id: Option<Uuid>,
) -> TurnRequest {
    TurnRequest {
        effort: None,
        execution: TurnExecutionContext {
            session_id,
            thread_id,
            turn_id: Uuid::new_v4(),
            topic_id: Some(topic_id),
        },
        user_message: user_message.to_string(),
        assembled_context: vec![ChatMessage::new(Role::User, user_message)],
        compaction_focus: None,
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
async fn streaming_turn_continues_past_ten_tool_rounds() {
    const TOOL_ROUNDS: usize = 25;

    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let mut rounds = (1..=TOOL_ROUNDS)
        .map(|round| {
            vec![
                StreamChunk::ToolUse {
                    call_id: format!("call-{round}"),
                    name: "counting".into(),
                    arguments: json!({}),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::ToolUse),
                },
            ]
        })
        .collect::<Vec<_>>();
    rounds.push(vec![
        StreamChunk::Delta {
            content: "All twenty-five tool calls completed successfully.".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]);
    let provider = Arc::new(StreamingProvider::new(rounds));
    let counting_tool = Arc::new(CountingTool::new());
    let dispatcher = ToolDispatcher::allow_all([counting_tool.clone() as Arc<dyn Tool>]);
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a"))
        .provider(provider.clone())
        .storage(storage.clone())
        .tool_dispatcher(dispatcher)
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Keep counting", None),
            event_tx,
        )
        .await
        .expect("streaming turn should continue until the provider stops calling tools");

    assert_eq!(provider.call_count(), TOOL_ROUNDS + 1);
    assert_eq!(counting_tool.call_count(), TOOL_ROUNDS);
    assert_eq!(
        outbound.response,
        "All twenty-five tool calls completed successfully."
    );

    let tool_calls = storage
        .list_tool_calls_by_turn(outbound.turn_id)
        .await
        .expect("tool calls");
    assert_eq!(tool_calls.len(), TOOL_ROUNDS);
    let turns = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turn list");
    assert_eq!(turns[0].state, TurnState::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_persists_completed_response_and_raw_input() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
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
        .config(resolved_config("agent-a"))
        .provider(provider.clone())
        .storage(storage.clone())
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Hello agent", None),
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
async fn streaming_turn_ignores_reasoning_chunks_in_visible_response() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Reasoning {
            content: "private thought".into(),
            details: Some(json!({"kind": "summary"})),
        },
        StreamChunk::Delta {
            content: "visible".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a"))
        .provider(provider.clone())
        .storage(storage.clone())
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Hello agent", None),
            event_tx,
        )
        .await
        .expect("streaming turn should succeed");

    assert_eq!(outbound.response, "visible");
    let messages = storage
        .list_messages_by_thread(thread.id)
        .await
        .expect("thread messages");
    assert_eq!(messages[0].content, "visible");

    let events = drain_events(event_rx);
    let tokens = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Token { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tokens, ["visible"]);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderReasoningCaptured {
            content,
            details: Some(details),
            ..
        } if content == "private thought" && details["kind"] == "summary"
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_persists_tool_result_truncation_metadata() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(CompactionStreamingProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "call-truncated".into(),
                name: "truncated_metadata".into(),
                arguments: json!({}),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::ToolUse),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    ]));
    let dispatcher = ToolDispatcher::allow_all([Arc::new(TruncatedMetadataTool) as Arc<dyn Tool>]);
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a"))
        .provider(provider.clone())
        .storage(storage.clone())
        .tool_dispatcher(dispatcher)
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Use the tool", None),
            event_tx,
        )
        .await
        .expect("streaming turn should succeed");

    let tool_calls = storage
        .list_tool_calls_by_turn(outbound.turn_id)
        .await
        .expect("tool calls");
    assert_eq!(tool_calls.len(), 1);
    assert!(tool_calls[0].truncated);
    assert_eq!(tool_calls[0].total_bytes, Some(4096));

    let second_request_messages = provider.last_stream_messages();
    let tool_result_content = second_request_messages
        .iter()
        .find_map(|message| {
            message.tool_replay()?.iter().find_map(|part| match part {
                swage_core::ToolReplayPart::ToolResult { content, .. } => Some(content),
                _ => None,
            })
        })
        .expect("second provider request should include tool-result replay");
    let tool_result: serde_json::Value =
        serde_json::from_str(tool_result_content).expect("tool result should be JSON");
    assert_eq!(tool_result["follow_up"]["tool"], json!("file_read"));
    assert_eq!(
        tool_result["follow_up"]["arguments"]["offset_bytes"],
        json!(128)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_compacts_context_before_provider_stream() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(CompactionStreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let mut config = resolved_config("agent-a");
    config.compaction_threshold = swage_agent::CompactionThreshold::MessageCount(3);
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider.clone())
        .storage(storage.clone())
        .build()
        .expect("agent");

    let mut request = test_request(topic.id, session.id, thread.id, "latest request", None);
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
async fn native_count_breach_keeps_its_model_and_survives_a_failed_recount() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(NativeCountCompactionProvider::new());
    let mut config = resolved_config("agent-a");
    config.compaction_threshold = swage_agent::CompactionThreshold::TokenCount(20);
    config.model_id = Some("selected-model".to_string());
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider.clone())
        .storage(storage)
        .build()
        .expect("agent");

    let mut request = test_request(topic.id, session.id, thread.id, "latest", None);
    request.assembled_context = vec![
        ChatMessage::new(Role::User, "first history message"),
        ChatMessage::new(Role::Assistant, "second history message"),
        ChatMessage::new(Role::User, "third history message"),
        ChatMessage::new(Role::Assistant, "fourth history message"),
    ];

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    // The summarised prompt is still too large for the byte upper bound to
    // prove safe, so the confirmed breach stands and the local estimate
    // cannot talk the turn past its capacity.
    let error = agent
        .process_turn_streaming(request, event_tx)
        .await
        .expect_err("an unproven prompt keeps the native-count breach");
    assert!(matches!(error, AgentError::CompactionFailed(_)));

    let count_requests = provider.count_requests();
    assert_eq!(count_requests[0].model.as_deref(), Some("selected-model"));
    assert_eq!(count_requests[1].model.as_deref(), Some("selected-model"));
    assert_eq!(count_requests[0].messages.len(), 4);
    assert_eq!(count_requests[1].messages.len(), 3);

    assert!(
        provider.stream_requests().is_empty(),
        "a prompt over capacity is never sent to the provider"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_turn_compaction_failure_marks_turn_failed_and_thread_idle() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(CompactionStreamingProvider::new(vec![]));
    let mut config = resolved_config("agent-a");
    config.compaction_threshold = swage_agent::CompactionThreshold::MessageCount(0);
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider)
        .storage(storage.clone())
        .build()
        .expect("agent");

    let mut request = test_request(topic.id, session.id, thread.id, "latest request", None);
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
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
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
        .config(resolved_config("agent-a"))
        .provider(provider)
        .storage(storage.clone())
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(
                topic.id,
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
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
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
        .config(resolved_config("agent-a"))
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
            test_request(topic.id, session.id, thread.id, "Check output", None),
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
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
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
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"],
        "additionalProperties": false
    });
    let config = resolved_config("agent-a").with_output_schema(schema.clone(), 1);
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider.clone())
        .storage(storage)
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Return JSON", None),
            event_tx,
        )
        .await
        .expect("retry path should recover with valid JSON");

    assert_eq!(provider.call_count(), 2);
    assert_eq!(outbound.structured_output, Some(json!({ "answer": "ok" })));
    assert_eq!(outbound.input_tokens, 5);
    assert_eq!(outbound.output_tokens, 5);

    let requests = provider.requests();
    let first_declaration = requests[0]
        .messages
        .last()
        .expect("first request should declare its output requirements");
    let first_declaration_text = first_declaration.text_content();
    assert_eq!(first_declaration.role(), Role::System);
    assert!(first_declaration_text.contains("## Output Requirements"));
    assert!(
        first_declaration_text.contains(
            &serde_json::to_string_pretty(&schema).expect("test schema should serialise")
        )
    );
    assert!(first_declaration_text.contains("\"additionalProperties\": false"));

    let retry_feedback = requests[1]
        .messages
        .last()
        .expect("retry request should contain validation feedback");
    let retry_feedback_text = retry_feedback.text_content();
    assert!(retry_feedback_text.contains("Validation errors:"));
    assert!(retry_feedback_text.contains("no JSON found in response"));
    assert!(retry_feedback_text.contains(&first_declaration_text));

    let events = drain_events(event_rx);
    let prepared_requests: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProviderRequestPrepared { request, .. } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(prepared_requests.len(), 2);
    assert!(
        prepared_requests
            .iter()
            .all(|request| request.to_string().contains("Output Requirements"))
    );

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

#[tokio::test(flavor = "multi_thread")]
async fn output_schema_accepts_json_after_prose_without_retrying() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "I checked the request. Final answer: {\"answer\":\"ok\"}".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a").with_output_schema(
            json!({
                "type": "object",
                "properties": { "answer": { "type": "string" } },
                "required": ["answer"],
                "additionalProperties": false,
            }),
            1,
        ))
        .provider(provider.clone())
        .storage(storage)
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Return JSON", None),
            event_tx,
        )
        .await
        .expect("JSON after prose should satisfy the output contract");

    assert_eq!(outbound.structured_output, Some(json!({"answer": "ok"})));
    assert_eq!(provider.call_count(), 1, "valid JSON must avoid a retry");
    assert!(
        !drain_events(event_rx)
            .iter()
            .any(|event| matches!(event, AgentEvent::OutputSchemaRetry { .. })),
        "the first response was already schema-valid"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_schema_uses_the_last_valid_json_value() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "```json\n{\"answer\":\"draft\"}\n```\nFinal: {\"answer\":\"ok\"}".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a").with_output_schema(
            json!({
                "type": "object",
                "properties": { "answer": { "type": "string" } },
                "required": ["answer"],
                "additionalProperties": false,
            }),
            1,
        ))
        .provider(provider.clone())
        .storage(storage)
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Return JSON", None),
            event_tx,
        )
        .await
        .expect("the final valid value should satisfy the output contract");

    assert_eq!(outbound.structured_output, Some(json!({"answer": "ok"})));
    assert_eq!(
        provider.call_count(),
        1,
        "the later valid value avoids a retry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_schema_accepts_a_scalar_json_value() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "Final result:\n```json\n\"done\"\n```".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a").with_output_schema(json!({"type": "string"}), 1))
        .provider(provider.clone())
        .storage(storage)
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Return a string", None),
            event_tx,
        )
        .await
        .expect("a schema-valid scalar should satisfy the output contract");

    assert_eq!(outbound.structured_output, Some(json!("done")));
    assert_eq!(provider.call_count(), 1, "valid scalar JSON avoids a retry");
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_anomaly_fails_before_output_schema_retry() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::ProviderResponseWitness(ProviderResponseWitness {
            http_status: 200,
            request_id: Some("req-empty".into()),
            terminal_event: Some("message_stop".into()),
            stop_reason: Some("end_turn".into()),
            usage: TokenUsage {
                input_tokens: 11,
                output_tokens: 0,
            },
            content_blocks: vec![],
        }),
        StreamChunk::ProviderAnomaly {
            message: "Anthropic stream completed without visible text or tool output".into(),
        },
    ]]));
    let config = resolved_config("agent-a")
        .with_output_schema(json!({"type": "object", "required": ["answer"]}), 2);
    let agent = AgentBuilder::new()
        .config(config)
        .provider(provider.clone())
        .storage(storage)
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let error = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Return JSON", None),
            event_tx,
        )
        .await
        .expect_err("provider anomaly should fail the provider round");

    assert!(matches!(
        error,
        swage_agent::AgentError::ProviderFailed(ProviderError::ProviderAnomaly(_))
    ));
    assert_eq!(provider.call_count(), 1, "schema retries must not run");
    let events = drain_events(event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderResponseCaptured { witness, .. }
            if witness.request_id.as_deref() == Some("req-empty")
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::OutputSchemaRetry { .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_schema_declaration_is_scoped_to_schema_configured_turns() {
    let valid_round = || {
        vec![
            StreamChunk::Delta {
                content: "{\"answer\":\"ok\"}".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ]
    };

    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let schema_provider = Arc::new(StreamingProvider::new(vec![valid_round()]));
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"],
        "additionalProperties": false
    });
    let schema_agent = AgentBuilder::new()
        .config(resolved_config("agent-a").with_output_schema(schema, 1))
        .provider(schema_provider.clone())
        .storage(storage)
        .build()
        .expect("agent");
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = schema_agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Return JSON", None),
            event_tx,
        )
        .await
        .expect("valid first response should complete");

    assert_eq!(schema_provider.call_count(), 1);
    assert_eq!(outbound.structured_output, Some(json!({ "answer": "ok" })));
    assert_eq!(schema_provider.requests()[0].messages.len(), 2);

    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let plain_provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "ordinary response".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]]));
    let plain_agent = AgentBuilder::new()
        .config(resolved_config("agent-a"))
        .provider(plain_provider.clone())
        .storage(storage)
        .build()
        .expect("agent");
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = plain_agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Respond normally", None),
            event_tx,
        )
        .await
        .expect("schema-free turn should complete");

    assert_eq!(outbound.structured_output, None);
    let plain_requests = plain_provider.requests();
    assert_eq!(plain_requests[0].messages.len(), 1);
    assert_eq!(plain_requests[0].messages[0].role(), Role::User);
    assert_eq!(
        plain_requests[0].messages[0].text_content(),
        "Respond normally"
    );
}

/// Signals when its execution starts, then runs until the turn is cancelled.
struct HangingTool {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Tool for HangingTool {
    fn name(&self) -> &str {
        "hanging"
    }

    fn description(&self) -> &str {
        "Runs until cancelled."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        self.started.notify_one();
        std::future::pending().await
    }
}

/// Returns a secret the leak scan must keep from the model.
struct SecretEchoTool;

/// Matched only by the agent's own configured leak pattern, so the scan the
/// agent runs is the one that redacts it.
const TOOL_SECRET: &str = "swage-agent-private-marker-7f3c";

#[async_trait]
impl Tool for SecretEchoTool {
    fn name(&self) -> &str {
        "secret_echo"
    }

    fn description(&self) -> &str {
        "Echoes a secret."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        Ok(json!({ "key": TOOL_SECRET }))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_is_recorded_before_it_runs_and_settled_as_interrupted_on_cancel() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::ToolUse {
            call_id: "call-hang".into(),
            name: "hanging".into(),
            arguments: json!({}),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::ToolUse),
        },
    ]]));
    let started = Arc::new(tokio::sync::Notify::new());
    let dispatcher = ToolDispatcher::allow_all([Arc::new(HangingTool {
        started: started.clone(),
    }) as Arc<dyn Tool>]);
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a"))
        .provider(provider)
        .storage(storage.clone())
        .tool_dispatcher(dispatcher)
        .build()
        .expect("agent");

    let request = test_request(topic.id, session.id, thread.id, "Hang", None);
    let cancel = request.cancellation_token.clone();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let turn = tokio::spawn(async move { agent.process_turn_streaming(request, event_tx).await });

    started.notified().await;
    // Mid-execution — the state a killed gateway leaves behind — the call
    // already has its started row.
    let turn_id = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turns")[0]
        .id;
    let mid_run = storage
        .list_tool_calls_by_turn(turn_id)
        .await
        .expect("calls");
    assert_eq!(
        mid_run.len(),
        1,
        "the selected call must be recorded before it runs"
    );
    assert_eq!(mid_run[0].call_id, "call-hang");
    assert!(
        mid_run[0].status.is_none(),
        "a running call is started, not settled"
    );
    assert!(mid_run[0].result.is_none());

    cancel.cancel();
    let outcome = turn.await.expect("turn task");
    assert!(matches!(outcome, Err(AgentError::Cancelled)), "{outcome:?}");

    let settled = storage
        .list_tool_calls_by_turn(turn_id)
        .await
        .expect("calls");
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].status.as_deref(), Some("interrupted"));
    assert_eq!(
        settled[0].result.as_deref(),
        Some(swage_storage::store::INTERRUPTED_TOOL_RESULT)
    );
    assert!(
        settled[0].model_result.is_some(),
        "an interrupted call replays"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settled_tool_call_keeps_the_raw_result_and_the_scanned_model_result() {
    let (storage, topic, session, thread) = setup_conversation("agent-a").await;
    let provider = Arc::new(StreamingProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "call-secret".into(),
                name: "secret_echo".into(),
                arguments: json!({}),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::ToolUse),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    ]));
    let agent = AgentBuilder::new()
        .config(resolved_config("agent-a"))
        .provider(provider)
        .storage(storage.clone())
        .tool_dispatcher(ToolDispatcher::allow_all([
            Arc::new(SecretEchoTool) as Arc<dyn Tool>
        ]))
        .leak_detector(Arc::new(
            LeakDetector::new(vec![swage_safety::LeakPattern {
                name: "private marker".into(),
                kind: swage_safety::PatternKind::Literal(TOOL_SECRET.into()),
                redaction_label: "private_marker".into(),
            }])
            .expect("leak patterns"),
        ))
        .build()
        .expect("agent");

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            test_request(topic.id, session.id, thread.id, "Echo", None),
            event_tx,
        )
        .await
        .expect("turn");

    let calls = storage
        .list_tool_calls_by_turn(outbound.turn_id)
        .await
        .expect("calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].status.as_deref(), Some("success"));
    assert!(
        calls[0].result.as_deref().unwrap().contains(TOOL_SECRET),
        "the store keeps the raw result: {:?}",
        calls[0].result
    );
    let model_result = calls[0].model_result.as_deref().expect("model result");
    assert!(
        !model_result.contains(TOOL_SECRET),
        "the model-facing result must be the scanned text: {model_result}"
    );
}
