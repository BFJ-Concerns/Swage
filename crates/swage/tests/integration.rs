//! Integration tests for the public `swage::` facade.
//!
//! These tests only use re-exported `swage::` paths to prove the public
//! streaming surface is complete after the sessionful API removal.

#![cfg(feature = "integration")]

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage::agent::context::TurnExecutionContext;
use swage::agent::{AgentBuilder, AgentConfig, AgentEvent, TurnRequest};
use swage::core::ToolError;
use swage::core::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, ProviderError, Role,
    StreamChunk, TokenUsage, ToolCompletionRequest, ToolCompletionResponse,
};
use swage::hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
use swage::provider::LlmProvider;
use swage::safety::LeakDetector;
use swage::storage::{
    MessageStore, NewSession, NewThread, SessionStore, SqliteStorage, ThreadState, ThreadStore,
    TopicStore, TurnState, TurnStore,
};
use swage::tool::context::ToolContext;
use swage::tool::dispatch::ToolDispatcher;
use swage::tool::tool::Tool;

struct StreamingProvider {
    rounds: Mutex<Vec<Vec<StreamChunk>>>,
    calls: AtomicUsize,
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo test tool"
    }

    fn parameters(&self) -> serde_json::Value {
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
        params: serde_json::Value,
        _ctx: &dyn ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        Ok(json!({ "echoed": params["text"].as_str().unwrap_or_default() }))
    }
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

#[async_trait]
impl LlmProvider for StreamingProvider {
    fn model_id(&self) -> &str {
        "facade-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("streaming integration tests should not call complete()")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("streaming integration tests should not call complete_with_tools()")
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
            .expect("mock provider ran out of rounds");
        Box::pin(stream::iter(chunks))
    }
}

struct AppendAfterResponseHook;

#[async_trait]
impl Hook for AppendAfterResponseHook {
    fn name(&self) -> &str {
        "append-after-response"
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        match event {
            HookEvent::AfterResponse { content } => {
                HookOutcome::ContinueModified(Box::new(HookEvent::AfterResponse {
                    content: format!("{content} [checked]"),
                }))
            }
            _ => HookOutcome::Continue,
        }
    }
}

async fn setup() -> (Arc<SqliteStorage>, Uuid, Uuid, Uuid) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory sqlite"));
    let topic = TopicStore::general_scope(storage.as_ref())
        .await
        .expect("default topic");
    let session = storage
        .create_session(NewSession {
            topic_id: topic.id,
            agent_id: "agent-a".into(),
            origin: swage_storage::SessionOrigin::Direct,
            metadata: None,
        })
        .await
        .expect("session");
    let thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: "agent-a".into(),
        },
    )
    .await
    .expect("thread");
    (storage, topic.id, session.id, thread.id)
}

fn turn_request(
    topic_id: Uuid,
    session_id: Uuid,
    thread_id: Uuid,
    user_message: &str,
) -> TurnRequest {
    TurnRequest {
        execution: TurnExecutionContext {
            session_id,
            thread_id,
            turn_id: Uuid::new_v4(),
            topic_id: Some(topic_id),
        },
        user_message: user_message.to_string(),
        assembled_context: vec![ChatMessage::new(Role::User, user_message)],
        parent_turn_id: None,
        existing_turn_id: None,
        cancellation_token: CancellationToken::new(),
    }
}

fn collect_events(mut rx: mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test(flavor = "multi_thread")]
async fn facade_streaming_turn_persists_turn_and_messages() {
    let (storage, topic_id, session_id, thread_id) = setup().await;
    let provider = Arc::new(StreamingProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "hello facade".into(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 4,
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

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            turn_request(topic_id, session_id, thread_id, "Hello facade"),
            event_tx,
        )
        .await
        .expect("streaming turn");

    assert_eq!(outbound.response, "hello facade");
    let turn = storage
        .get_turn(outbound.turn_id)
        .await
        .expect("turn lookup")
        .expect("turn exists");
    assert_eq!(turn.state, TurnState::Completed);
    assert_eq!(turn.user_input.as_deref(), Some("Hello facade"));

    let thread = ThreadStore::get(storage.as_ref(), thread_id)
        .await
        .expect("thread lookup")
        .expect("thread exists");
    assert_eq!(thread.state, ThreadState::Idle);

    let messages = storage
        .list_messages_by_thread(thread_id)
        .await
        .expect("message list");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "hello facade");

    let events = collect_events(event_rx);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnStarted { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnCompleted { .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn facade_streaming_turn_supports_hooks_redaction_and_schema_retry() {
    let (storage, topic_id, session_id, thread_id) = setup().await;
    let provider = Arc::new(StreamingProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "not json".into(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 1,
                output_tokens: 1,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "{\"answer\":\"sk-proj-abc123def456ghi789jklmnopqrstuvwxyz1234567890ABCDEFGHIJ\"}".into(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 2,
                output_tokens: 3,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    ]));
    let mut registry = HookRegistry::new();
    registry.register(HookRegistration::new(Arc::new(AppendAfterResponseHook)));

    let agent = AgentBuilder::new()
        .config(AgentConfig::new("agent-a").with_output_schema(
            json!({
                "type": "object",
                "properties": {
                    "answer": { "type": "string" }
                },
                "required": ["answer"],
                "additionalProperties": false
            }),
            1,
        ))
        .provider(provider.clone())
        .storage(storage)
        .hook_registry(Arc::new(registry))
        .leak_detector(Arc::new(
            LeakDetector::new(vec![]).expect("default leak patterns"),
        ))
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let outbound = agent
        .process_turn_streaming(
            turn_request(topic_id, session_id, thread_id, "Return checked JSON"),
            event_tx,
        )
        .await
        .expect("streaming turn");

    assert_eq!(provider.call_count(), 2);
    assert!(outbound.response.contains("[checked]"));
    assert!(
        !outbound
            .response
            .contains("sk-proj-abc123def456ghi789jklmnopqrstuvwxyz1234567890ABCDEFGHIJ")
    );
    assert_eq!(
        outbound.structured_output,
        Some(json!({
            "answer": "sk-proj-abc123def456ghi789jklmnopqrstuvwxyz1234567890ABCDEFGHIJ"
        }))
    );

    let events = collect_events(event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::OutputSchemaRetry { attempt, .. } if *attempt == 1
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn facade_streaming_turn_emits_tool_and_turn_lifecycle_events() {
    let (storage, topic_id, session_id, thread_id) = setup().await;
    let provider = Arc::new(StreamingProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "call_echo".into(),
                name: "echo".into(),
                arguments: json!({ "text": "from tool" }),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::ToolUse),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "tool complete".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    ]));
    let agent = AgentBuilder::new()
        .config(AgentConfig::new("agent-a"))
        .provider(provider)
        .storage(storage)
        .tool_dispatcher(ToolDispatcher::allow_all(vec![
            Arc::new(EchoTool) as Arc<dyn Tool>
        ]))
        .build()
        .expect("agent");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let response = agent
        .process_turn_streaming(
            turn_request(topic_id, session_id, thread_id, "Use the echo tool"),
            event_tx,
        )
        .await
        .expect("streaming turn with tool");

    assert_eq!(response.response, "tool complete");

    let events = collect_events(event_rx);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnStarted { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolStart {
            call_id,
            tool_name,
            ..
            display_summary: None,
        }
        if call_id == "call_echo" && tool_name == "echo"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolEnd {
            call_id,
            tool_name,
            status,
            ..
        }
        if call_id == "call_echo" && tool_name == "echo" && status == "success"
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnCompleted { .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn facade_storage_round_trips_session_metadata() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory sqlite"));
    let topic = TopicStore::general_scope(storage.as_ref())
        .await
        .expect("default topic");
    let metadata = json!({
        "owner": "host-app",
        "labels": ["embedder", "demo"]
    });

    let session = storage
        .create_session(NewSession {
            topic_id: topic.id,
            agent_id: "agent-a".into(),
            origin: swage_storage::SessionOrigin::Direct,
            metadata: Some(metadata.clone()),
        })
        .await
        .expect("session with metadata");

    let fetched = storage
        .get_session(session.id)
        .await
        .expect("session lookup")
        .expect("session exists");

    assert_eq!(fetched.metadata, Some(metadata));
}
