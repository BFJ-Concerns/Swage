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
use swage::core::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, ProviderError, Role,
    StreamChunk, TokenUsage, ToolCompletionRequest, ToolCompletionResponse,
};
use swage::hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
use swage::provider::LlmProvider;
use swage::safety::LeakDetector;
use swage::storage::{
    MessageStore, NewSession, NewThread, ProjectStore, SessionStore, SqliteStorage, ThreadState,
    ThreadStore, TurnState, TurnStore,
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
    let project = ProjectStore::get_or_create_default(storage.as_ref())
        .await
        .expect("default project");
    let session = storage
        .create_session(NewSession {
            project_id: project.id,
            agent_id: "agent-a".into(),
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
    (storage, project.id, session.id, thread.id)
}

fn turn_request(
    project_id: Uuid,
    session_id: Uuid,
    thread_id: Uuid,
    user_message: &str,
) -> TurnRequest {
    TurnRequest {
        execution: TurnExecutionContext {
            session_id,
            thread_id,
            turn_id: Uuid::new_v4(),
            project_id: Some(project_id),
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
    let (storage, project_id, session_id, thread_id) = setup().await;
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
            turn_request(project_id, session_id, thread_id, "Hello facade"),
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
    let (storage, project_id, session_id, thread_id) = setup().await;
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
            turn_request(project_id, session_id, thread_id, "Return checked JSON"),
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
