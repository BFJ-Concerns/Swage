use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use tokio::sync::{Mutex, Notify, mpsc};

use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, DelegateCompletionMode,
    MessageStatus, ProviderError, SandboxError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse, ToolError, ToolReplayPart,
};
use swage_core::{FinishReason, StorageError};
use swage_hook::{Hook, HookOutcome, HookRegistration, HookRegistry};
use swage_protocol::client::turn::MessageAttachment;
use swage_provider::LlmProvider;
use swage_storage::{
    model::{
        ActiveSkillRecord, AgentMessage, AppendMessage, ApprovalRequest, ApprovalState, Attachment,
        AttachmentScopeKind, AttachmentStatus, BackgroundJob, ChannelLink, DeviceRegistration,
        Material, MaterialStatus, Message, NewAgentMessage, NewApprovalRequest, NewAttachment,
        NewAuditEvent, NewBackgroundJob, NewChannelLink, NewDeviceRegistration, NewMaterial,
        NewMessage, NewSchedule, NewScheduleRun, NewSession, NewSpace, NewThread, NewToolCall,
        NewTurn, NewWorkspaceAttachment, PageAnnotation, Schedule, ScheduleRun, ScheduleRunState,
        ScheduleRunUpdate, ScheduleStatus, Session, Space, SpaceSlug, Thread, ThreadState,
        ToolCallRecord, Turn, TurnState, UpdateSchedule, WorkspaceAttachment,
    },
    store::{
        AgentDefinitionStore, AgentMessageStore, ApprovalStore, AttachmentStore,
        BackgroundJobStore, ChannelLinkStore, ContextSnapshotStore, DeviceStore, MaterialStore,
        MessageStore, ScheduleStore, SessionStore, SkillActivationStore, SpaceStore,
        TaskDefinitionStore, ThreadStore, ToolCallStore, TurnStore, WorkspaceAttachmentStore,
    },
};

use super::history::parse_model_arguments;

use swage_agent::{AgentEvent, DelegationOutcome};
use swage_sandbox::ToolCallResult;

use super::*;
use crate::{
    state::AppState, subscription::SubscriptionState, test_support::SwageHomeGuard,
    websocket::ServerEvent,
};

#[test]
fn provider_response_witness_is_appended_without_content_fields() {
    let mut report = serde_json::json!({});
    append_provider_visible_response(
        &mut report,
        2,
        swage_core::ProviderResponseWitness {
            http_status: 200,
            request_id: Some("req-123".into()),
            terminal_event: Some("message_stop".into()),
            stop_reason: Some("end_turn".into()),
            usage: swage_core::TokenUsage {
                input_tokens: 9,
                output_tokens: 0,
            },
            content_blocks: vec![swage_core::ProviderContentBlockWitness {
                block_type: "thinking".into(),
                count: 1,
                bytes: 42,
            }],
        },
    );

    let response = &report["provider_visible"]["responses"][0];
    assert_eq!(response["round"], 2);
    assert_eq!(response["witness"]["request_id"], "req-123");
    assert_eq!(response["witness"]["content_blocks"][0]["bytes"], 42);
    assert!(response["witness"].get("content").is_none());
    assert!(response["witness"].get("thinking").is_none());
}

#[tokio::test]
async fn ordinary_and_delegated_snapshot_paths_record_provider_events_identically() {
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage(vec![], Arc::clone(&storage)).await;
    let ordinary_turn_id = Uuid::new_v4();
    let delegated_turn_id = Uuid::new_v4();
    let mut ordinary_report = serde_json::json!({"assembly": "same input"});
    let delegated_report = serde_json::json!({"assembly": "same input"});

    let witness = swage_core::ProviderResponseWitness {
        http_status: 200,
        request_id: Some("request-3".into()),
        terminal_event: Some("message_stop".into()),
        stop_reason: Some("end_turn".into()),
        usage: swage_core::TokenUsage {
            input_tokens: 11,
            output_tokens: 7,
        },
        content_blocks: vec![swage_core::ProviderContentBlockWitness {
            block_type: "text".into(),
            count: 1,
            bytes: 7,
        }],
    };
    let expected_provider_visible = serde_json::json!({
        "requests": [{
            "round": 3,
            "request": {"messages": ["same request"]},
        }],
        "responses": [{
            "round": 3,
            "witness": witness,
        }],
        "reasoning": [{
            "round": 3,
            "content": "same reasoning",
            "details": {"source": "provider"},
        }],
    });

    for event in [
        AgentEvent::ProviderRequestPrepared {
            turn_id: ordinary_turn_id,
            round: 3,
            request: serde_json::json!({"messages": ["same request"]}),
        },
        AgentEvent::ProviderResponseCaptured {
            turn_id: ordinary_turn_id,
            round: 3,
            witness: witness.clone(),
        },
        AgentEvent::ProviderReasoningCaptured {
            turn_id: ordinary_turn_id,
            round: 3,
            content: "same reasoning".into(),
            details: Some(serde_json::json!({"source": "provider"})),
        },
    ] {
        apply_provider_snapshot_event(
            &state,
            &mut ordinary_report,
            event,
            ProviderSnapshotCapturePath::Ordinary,
        )
        .await;
    }

    storage
        .store_snapshot(delegated_turn_id, &delegated_report.to_string())
        .await
        .expect("seed delegated snapshot");
    for event in [
        AgentEvent::ProviderRequestPrepared {
            turn_id: delegated_turn_id,
            round: 3,
            request: serde_json::json!({"messages": ["same request"]}),
        },
        AgentEvent::ProviderResponseCaptured {
            turn_id: delegated_turn_id,
            round: 3,
            witness,
        },
        AgentEvent::ProviderReasoningCaptured {
            turn_id: delegated_turn_id,
            round: 3,
            content: "same reasoning".into(),
            details: Some(serde_json::json!({"source": "provider"})),
        },
    ] {
        delegation_snapshot_recorder_for_state(state.clone())
            .record_event(session_id, event)
            .await
            .expect("record delegated provider event");
    }
    let delegated_report: serde_json::Value = serde_json::from_str(
        &storage
            .get_snapshot(delegated_turn_id)
            .await
            .expect("read delegated snapshot")
            .expect("delegated snapshot"),
    )
    .expect("delegated snapshot JSON");

    assert_eq!(
        ordinary_report["provider_visible"],
        expected_provider_visible
    );
    assert_eq!(
        delegated_report["provider_visible"],
        expected_provider_visible
    );
}

#[test]
fn ordinary_and_delegated_snapshot_paths_fail_closed_with_their_audit_wording() {
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    #[derive(Default)]
    struct MessageRecorder(StdArc<StdMutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for MessageRecorder {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct MessageVisitor<'a>(&'a mut Option<String>);

            impl tracing::field::Visit for MessageVisitor<'_> {
                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    if field.name() == "message" {
                        *self.0 = Some(value.to_owned());
                    }
                }

                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        *self.0 = Some(format!("{value:?}"));
                    }
                }
            }

            let mut message = None;
            event.record(&mut MessageVisitor(&mut message));
            if let Some(message) = message {
                self.0.lock().unwrap().push(message);
            }
        }
    }

    let messages = MessageRecorder::default();
    let captured_messages = StdArc::clone(&messages.0);
    let subscriber = Registry::default().with(messages);

    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime")
            .block_on(async {
                let storage = Arc::new(MockStorage::new());
                let (state, session_id) =
                    make_state_with_storage(vec![], Arc::clone(&storage)).await;
                let ordinary_turn_id = Uuid::new_v4();
                let delegated_turn_id = Uuid::new_v4();
                let mut ordinary_report = serde_json::json!({"assembly": "same input"});
                let delegated_report = serde_json::json!({"assembly": "same input"});
                let expected_provider_visible = serde_json::json!({
                    "capture_errors": [{
                        "round": 4,
                        "kind": "secret_redaction_unavailable",
                    }],
                });

                storage
                    .store_snapshot(delegated_turn_id, &delegated_report.to_string())
                    .await
                    .expect("seed delegated snapshot");
                storage.fail_secret_reads();
                tracing::callsite::rebuild_interest_cache();

                apply_provider_snapshot_event(
                    &state,
                    &mut ordinary_report,
                    AgentEvent::ProviderRequestPrepared {
                        turn_id: ordinary_turn_id,
                        round: 4,
                        request: serde_json::json!({"messages": ["secret request"]}),
                    },
                    ProviderSnapshotCapturePath::Ordinary,
                )
                .await;
                delegation_snapshot_recorder_for_state(state.clone())
                    .record_event(
                        session_id,
                        AgentEvent::ProviderRequestPrepared {
                            turn_id: delegated_turn_id,
                            round: 4,
                            request: serde_json::json!({"messages": ["secret request"]}),
                        },
                    )
                    .await
                    .expect("record delegated redaction failure");
                let delegated_report: serde_json::Value = serde_json::from_str(
                    &storage
                        .get_snapshot(delegated_turn_id)
                        .await
                        .expect("read delegated snapshot")
                        .expect("delegated snapshot"),
                )
                .expect("delegated snapshot JSON");

                assert_eq!(
                    ordinary_report["provider_visible"],
                    expected_provider_visible
                );
                assert_eq!(
                    delegated_report["provider_visible"],
                    expected_provider_visible
                );
                assert!(ordinary_report["provider_visible"]["requests"].is_null());
                assert!(delegated_report["provider_visible"]["requests"].is_null());
            });
    });

    let messages = captured_messages.lock().expect("recorded messages");
    assert!(messages.iter().any(|message| {
        message
            == "provider-visible request capture omitted because secret redaction was unavailable"
    }));
    assert!(messages.iter().any(|message| {
        message
            == "delegated provider-visible request capture omitted because secret redaction was unavailable"
    }));
}

#[test]
fn delegate_worktree_identity_is_stable_per_tool_call() {
    let sender_turn_id = Uuid::new_v4();
    assert_eq!(
        delegate_worktree_owner(sender_turn_id, "delegate-call-17"),
        delegate_worktree_owner(sender_turn_id, "delegate-call-17")
    );
    assert_ne!(
        delegate_worktree_owner(sender_turn_id, "delegate-call-17"),
        delegate_worktree_owner(sender_turn_id, "delegate-call-18")
    );
    assert_ne!(
        delegate_worktree_owner(sender_turn_id, "delegate-call-17"),
        delegate_worktree_owner(Uuid::new_v4(), "delegate-call-17")
    );
}

fn init_test_git_repository(repo: &std::path::Path) {
    for args in [
        &["init"][..],
        &["config", "user.email", "test@example.invalid"][..],
        &["config", "user.name", "Test"][..],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .status()
                .expect("run git repository setup")
                .success()
        );
    }
    std::fs::write(repo.join("seed"), "seed").expect("seed repository");
    for args in [&["add", "seed"][..], &["commit", "-m", "seed"][..]] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .status()
                .expect("commit repository")
                .success()
        );
    }
}

fn provider_witness_chunks() -> Vec<StreamChunk> {
    vec![
        StreamChunk::ProviderResponseWitness(swage_core::ProviderResponseWitness {
            http_status: 200,
            request_id: Some("req-123".into()),
            terminal_event: Some("message_stop".into()),
            stop_reason: Some("end_turn".into()),
            usage: swage_core::TokenUsage {
                input_tokens: 9,
                output_tokens: 2,
            },
            content_blocks: vec![swage_core::ProviderContentBlockWitness {
                block_type: "text".into(),
                count: 1,
                bytes: 6,
            }],
        }),
        StreamChunk::Delta {
            content: "answer".into(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ]
}

#[tokio::test]
async fn provider_response_event_persists_witness_through_gateway_turn() {
    let (state, session_id) = make_state_with_session(provider_witness_chunks()).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("Hello"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let thread = state.storage.list_by_session(session_id).await.unwrap()[0].clone();
    let turn = state.storage.list_turns_by_thread(thread.id).await.unwrap()[0].clone();
    let snapshot = state
        .storage
        .get_snapshot(turn.id)
        .await
        .unwrap()
        .expect("provider response event should persist a snapshot");
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    let response = &snapshot["provider_visible"]["responses"][0];
    assert_eq!(response["round"], 1);
    assert_eq!(response["witness"]["request_id"], "req-123");
}

#[tokio::test]
async fn provider_response_snapshot_failure_records_reconstruction_gap() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_snapshot_writes();
    let (state, session_id) =
        make_state_with_storage(provider_witness_chunks(), Arc::clone(&storage)).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("Hello"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("a recorded reconstruction gap should allow the turn to complete");

    let db = storage.db.lock().await;
    assert!(db.audit_events.iter().any(|event| {
        event.event_kind == "turn_record_degraded"
            && event.metadata["record"] == "provider_response_witness"
    }));
}

#[tokio::test]
async fn compaction_snapshot_failure_records_reconstruction_gap_through_turn_loop() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_snapshot_writes();
    let (state, session_id) =
        make_state_with_storage(provider_witness_chunks(), Arc::clone(&storage)).await;
    let thread_id = storage
        .list_by_session(session_id)
        .await
        .expect("list test thread")
        .into_iter()
        .next()
        .expect("default thread")
        .id;

    for index in 0..101 {
        storage
            .append_message(AppendMessage {
                thread_id,
                attachments: Vec::new(),
                turn_id: None,
                role: if index % 2 == 0 {
                    swage_core::Role::User
                } else {
                    swage_core::Role::Assistant
                },
                content: format!("history message {index}"),
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("seed history for compaction");
    }

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("continue"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("a recorded reconstruction gap should allow the turn to complete");

    let db = storage.db.lock().await;
    assert!(db.audit_events.iter().any(|event| {
        event.event_kind == "turn_record_degraded"
            && event.metadata["record"] == "compaction_snapshot"
    }));
}

// -----------------------------------------------------------------------
// Sandbox guard
// -----------------------------------------------------------------------

struct RetryingShutdownSandbox {
    shutdown_attempts: AtomicUsize,
    failures_before_success: usize,
}

impl RetryingShutdownSandbox {
    fn new(failures_before_success: usize) -> Self {
        Self {
            shutdown_attempts: AtomicUsize::new(0),
            failures_before_success,
        }
    }

    fn shutdown_attempts(&self) -> usize {
        self.shutdown_attempts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Sandbox for RetryingShutdownSandbox {
    async fn spawn_agent(&mut self) -> Result<(), SandboxError> {
        Ok(())
    }

    async fn execute_tool(
        &self,
        _tool_name: &str,
        _params: serde_json::Value,
    ) -> Result<ToolCallResult, SandboxError> {
        unimplemented!("not used by SandboxGuard tests")
    }

    async fn shutdown(&self) -> Result<(), SandboxError> {
        let attempt = self.shutdown_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.failures_before_success {
            return Err(SandboxError::SpawnFailed {
                message: "shutdown failed".to_string(),
            });
        }
        Ok(())
    }
}

#[tokio::test]
async fn sandbox_guard_retries_shutdown_after_failure() {
    let sandbox = Arc::new(RetryingShutdownSandbox::new(1));
    let guard = SandboxGuard::new(sandbox.clone());

    assert!(guard.shutdown().await.is_err());
    assert_eq!(sandbox.shutdown_attempts(), 1);

    guard.shutdown().await.unwrap();
    assert_eq!(sandbox.shutdown_attempts(), 2);

    guard.shutdown().await.unwrap();
    assert_eq!(sandbox.shutdown_attempts(), 2);
}

// -----------------------------------------------------------------------
// Mock LLM providers
// -----------------------------------------------------------------------

/// A mock provider that streams a fixed sequence of chunks.
pub(crate) struct MockProvider {
    chunks: std::sync::Mutex<Vec<StreamChunk>>,
}

impl MockProvider {
    pub(crate) fn new(chunks: Vec<StreamChunk>) -> Self {
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
        Ok(CompletionResponse {
            content: "Compacted history".to_string(),
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
        unimplemented!("not used in streaming tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = {
            let mut lock = self.chunks.lock().expect("mock lock poisoned");
            std::mem::take(&mut *lock)
        };
        Box::pin(stream::iter(chunks))
    }
}

/// A mock provider supporting multiple streaming rounds.
pub(crate) struct MultiRoundMockProvider {
    rounds: std::sync::Mutex<std::collections::VecDeque<Vec<StreamChunk>>>,
}

impl MultiRoundMockProvider {
    pub(crate) fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: std::sync::Mutex::new(rounds.into_iter().collect()),
        }
    }
}

pub(crate) struct SandboxedFileTool {
    name: &'static str,
}

impl SandboxedFileTool {
    pub(crate) fn new(name: &'static str) -> Self {
        Self { name }
    }
}

#[async_trait]
impl Tool for SandboxedFileTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "Exercise a file operation inside the turn sandbox"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        panic!("sandboxed file tools execute in the sandbox child")
    }
}

/// A multi-round provider whose final configured round remains open after its chunks.
struct PendingFinalRoundProvider {
    rounds: std::sync::Mutex<std::collections::VecDeque<Vec<StreamChunk>>>,
}

impl PendingFinalRoundProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: std::sync::Mutex::new(rounds.into_iter().collect()),
        }
    }
}

#[async_trait]
impl LlmProvider for PendingFinalRoundProvider {
    fn model_id(&self) -> &str {
        "mock-pending-final-round"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round cancellation tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round cancellation tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let (chunks, is_final_round) = {
            let mut rounds = self.rounds.lock().expect("mock lock poisoned");
            let chunks = rounds.pop_front().unwrap_or_default();
            (chunks, rounds.is_empty())
        };
        let chunks = stream::iter(chunks);
        if is_final_round {
            Box::pin(chunks.chain(stream::pending()))
        } else {
            Box::pin(chunks)
        }
    }
}

/// A multi-round provider that records every incoming request.
pub(crate) struct CapturingMultiRoundProvider {
    rounds: std::sync::Mutex<std::collections::VecDeque<Vec<StreamChunk>>>,
    captured: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
}

impl CapturingMultiRoundProvider {
    pub(crate) fn new(
        rounds: Vec<Vec<StreamChunk>>,
    ) -> (Self, Arc<std::sync::Mutex<Vec<CompletionRequest>>>) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                rounds: std::sync::Mutex::new(rounds.into_iter().collect()),
                captured: Arc::clone(&captured),
            },
            captured,
        )
    }
}

#[async_trait]
impl LlmProvider for CapturingMultiRoundProvider {
    fn model_id(&self) -> &str {
        "mock-capturing-multi-round"
    }

    fn provider_request_snapshot(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "provider": "capturing-multi-round",
            "stream": stream,
            "message_count": request.messages.len(),
        })
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        self.captured.lock().unwrap().push(request);
        let chunks = self
            .rounds
            .lock()
            .expect("mock lock poisoned")
            .pop_front()
            .unwrap_or_default();
        Box::pin(stream::iter(chunks))
    }
}

#[async_trait]
impl LlmProvider for MultiRoundMockProvider {
    fn model_id(&self) -> &str {
        "mock-multi-round"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = self
            .rounds
            .lock()
            .expect("mock lock poisoned")
            .pop_front()
            .unwrap_or_default();
        Box::pin(stream::iter(chunks))
    }
}

struct DetachedGatedProvider {
    child_started: Arc<Notify>,
    child_release: Arc<Notify>,
    child_answer: String,
    initial_parent_request_seen: AtomicUsize,
    child_round: AtomicUsize,
    captured: Arc<std::sync::Mutex<Vec<String>>>,
}

impl DetachedGatedProvider {
    fn new(
        child_started: Arc<Notify>,
        child_release: Arc<Notify>,
        child_answer: String,
    ) -> (Self, Arc<std::sync::Mutex<Vec<String>>>) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                child_started,
                child_release,
                child_answer,
                initial_parent_request_seen: AtomicUsize::new(0),
                child_round: AtomicUsize::new(0),
                captured: captured.clone(),
            },
            captured,
        )
    }
}

#[async_trait]
impl LlmProvider for DetachedGatedProvider {
    fn model_id(&self) -> &str {
        "mock-detached-gated"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("streaming provider only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("streaming provider only")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let request_text = request
            .messages
            .iter()
            .map(ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n\n");
        self.captured.lock().unwrap().push(request_text.clone());

        if request_text.contains("Delegated Task\n\nSummarise independently") {
            if self.child_round.fetch_add(1, Ordering::SeqCst) == 0 {
                return Box::pin(stream::iter(vec![
                    StreamChunk::ToolUse {
                        call_id: "delegated-marker-write".into(),
                        name: "delegated_marker_write".into(),
                        arguments: serde_json::json!({}),
                    },
                    StreamChunk::Done {
                        finish_reason: None,
                    },
                ]));
            }
            let started = self.child_started.clone();
            let release = self.child_release.clone();
            let child_answer = self.child_answer.clone();
            return Box::pin(stream::unfold(0_u8, move |state| {
                let started = started.clone();
                let release = release.clone();
                let child_answer = child_answer.clone();
                async move {
                    match state {
                        0 => {
                            started.notify_one();
                            release.notified().await;
                            Some((
                                StreamChunk::Delta {
                                    content: child_answer,
                                },
                                1,
                            ))
                        }
                        1 => Some((
                            StreamChunk::Done {
                                finish_reason: None,
                            },
                            2,
                        )),
                        _ => None,
                    }
                }
            }));
        }

        if self
            .initial_parent_request_seen
            .fetch_add(1, Ordering::SeqCst)
            > 0
        {
            return Box::pin(stream::iter(vec![
                StreamChunk::Delta {
                    content: "Parent continued without waiting".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ]));
        }

        Box::pin(stream::iter(vec![
            StreamChunk::ToolUse {
                call_id: "delegate-detached".into(),
                name: "delegate".into(),
                arguments: serde_json::json!({
                    "target_agent": "agent-b",
                    "task": "Summarise independently",
                    "completion_mode": "detach",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]))
    }
}

struct DelegatedMarkerWriteTool;

#[async_trait]
impl Tool for DelegatedMarkerWriteTool {
    fn name(&self) -> &str {
        "delegated_marker_write"
    }

    fn description(&self) -> &str {
        "Write a marker in the delegated worker's current directory"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        std::fs::write(ctx.working_directory().join("delegated-marker"), "placed").map_err(
            |error| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: error.to_string(),
            },
        )?;
        Ok(serde_json::json!({"written": true}))
    }
}

const INTERRUPTED_PARTIAL: &str = "The response shown before cancellation.";

/// A mock provider whose stream emits a known partial response, then blocks forever.
pub(crate) struct PendingProvider;

#[async_trait]
impl LlmProvider for PendingProvider {
    fn model_id(&self) -> &str {
        "pending-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in cancellation tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in cancellation tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(
            futures::stream::once(async {
                StreamChunk::Delta {
                    content: INTERRUPTED_PARTIAL.to_string(),
                }
            })
            .chain(futures::stream::pending()),
        )
    }
}

/// A mock provider whose stream reports its first poll, then remains open without emitting.
struct EmptyPendingProvider {
    stream_polled: Arc<Notify>,
}

#[async_trait]
impl LlmProvider for EmptyPendingProvider {
    fn model_id(&self) -> &str {
        "empty-pending-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in cancellation tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in cancellation tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let stream_polled = Arc::clone(&self.stream_polled);
        Box::pin(futures::stream::poll_fn(move |_cx| {
            stream_polled.notify_one();
            std::task::Poll::Pending
        }))
    }
}

struct BlockInboundHook;

#[async_trait]
impl Hook for BlockInboundHook {
    fn name(&self) -> &str {
        "block-inbound"
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        HookOutcome::Block("blocked by test hook".to_string())
    }
}

struct ModifyInboundHook;

#[async_trait]
impl Hook for ModifyInboundHook {
    fn name(&self) -> &str {
        "modify-inbound"
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        match event {
            HookEvent::BeforeInbound { content } => {
                HookOutcome::ContinueModified(Box::new(HookEvent::BeforeInbound {
                    content: format!("{content} modified"),
                }))
            }
            _ => HookOutcome::Continue,
        }
    }
}

struct PausingPostToolHook {
    reached: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Hook for PausingPostToolHook {
    fn name(&self) -> &str {
        "pause-post-tool"
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        if matches!(event, HookEvent::PostToolUse { .. }) {
            self.reached.notify_one();
            self.release.notified().await;
        }
        HookOutcome::Continue
    }
}

struct ApprovePreToolHook;

#[async_trait]
impl Hook for ApprovePreToolHook {
    fn name(&self) -> &str {
        "approve-pre-tool"
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        if matches!(event, HookEvent::PreToolUse { .. }) {
            HookOutcome::RequestApproval {
                prompt: "Approve the test tool".to_string(),
                context: None,
                timeout: None,
            }
        } else {
            HookOutcome::Continue
        }
    }
}

// -----------------------------------------------------------------------
// Mock storage — in-memory Vec/HashMap backed, usable in tests.
// -----------------------------------------------------------------------

/// In-memory storage for testing.
pub(crate) struct MockStorage {
    pub(crate) db: Mutex<InMemoryDb>,
    fail_turn_state_writes: AtomicBool,
    fail_turn_state_write_at: AtomicUsize,
    turn_state_write_calls: AtomicUsize,
    idle_thread_state_write_calls: AtomicUsize,
    fail_inbound_audit_writes: AtomicBool,
    fail_append_message_at: AtomicUsize,
    append_message_calls: AtomicUsize,
    fail_message_lists: AtomicBool,
    fail_session_variable_lists: AtomicBool,
    fail_secret_reads: AtomicBool,
    fail_skill_activations: AtomicBool,
    fail_audit_writes: AtomicBool,
    fail_snapshot_writes: AtomicBool,
    last_snapshot_list_limit: AtomicUsize,
}

#[async_trait]
impl swage_storage::WorkflowStore for MockStorage {
    async fn create_workflow_script(
        &self,
        _: swage_storage::NewWorkflowScript,
    ) -> Result<
        (
            swage_storage::WorkflowScriptRecord,
            swage_storage::WorkflowScriptVersion,
        ),
        StorageError,
    > {
        unimplemented!()
    }
    async fn append_workflow_script_version(
        &self,
        _: Uuid,
        _: &str,
    ) -> Result<swage_storage::WorkflowScriptVersion, StorageError> {
        unimplemented!()
    }
    async fn get_workflow_script(
        &self,
        _: Uuid,
    ) -> Result<Option<swage_storage::WorkflowScriptRecord>, StorageError> {
        unimplemented!()
    }
    async fn get_workflow_script_head(
        &self,
        _: Uuid,
    ) -> Result<Option<swage_storage::WorkflowScriptVersion>, StorageError> {
        unimplemented!()
    }
    async fn get_workflow_script_version(
        &self,
        _: Uuid,
    ) -> Result<Option<swage_storage::WorkflowScriptVersion>, StorageError> {
        unimplemented!()
    }
    async fn update_workflow_script(
        &self,
        _: Uuid,
        _: Option<&str>,
        _: Option<serde_json::Value>,
        _: Option<Uuid>,
    ) -> Result<swage_storage::WorkflowScriptRecord, StorageError> {
        unimplemented!()
    }
    async fn list_workflow_scripts(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::WorkflowScriptRecord>, StorageError> {
        unimplemented!()
    }
    async fn create_workflow_run(
        &self,
        _: swage_storage::NewWorkflowRun,
    ) -> Result<swage_storage::WorkflowRunRecord, StorageError> {
        unimplemented!()
    }
    async fn get_workflow_run(
        &self,
        _: Uuid,
    ) -> Result<Option<swage_storage::WorkflowRunRecord>, StorageError> {
        Ok(None)
    }
    async fn find_child_workflow_run(
        &self,
        _: Uuid,
        _: u64,
    ) -> Result<Option<swage_storage::WorkflowRunRecord>, StorageError> {
        unimplemented!()
    }
    async fn fail_workflow_journal_entry(
        &self,
        _: Uuid,
        _: &str,
    ) -> Result<swage_storage::WorkflowJournalRecord, StorageError> {
        unimplemented!()
    }
    async fn list_workflow_runs(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::WorkflowRunRecord>, StorageError> {
        unimplemented!()
    }
    async fn update_workflow_run_state(
        &self,
        _: Uuid,
        _: &str,
        _: Option<serde_json::Value>,
        _: Option<serde_json::Value>,
        _: Option<&str>,
    ) -> Result<(), StorageError> {
        unimplemented!()
    }
    async fn interrupt_running_workflow_runs(&self) -> Result<u64, StorageError> {
        unimplemented!()
    }
    async fn try_transition_workflow_run_state(
        &self,
        _: Uuid,
        _: &str,
        _: &str,
    ) -> Result<bool, StorageError> {
        unimplemented!()
    }
    async fn set_workflow_run_parked(
        &self,
        _: Uuid,
        _: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StorageError> {
        unimplemented!()
    }
    async fn park_workflow_run_if_no_matching_events(
        &self,
        _: Uuid,
        _: chrono::DateTime<chrono::Utc>,
        _: &[String],
    ) -> Result<bool, StorageError> {
        unimplemented!()
    }
    async fn make_parked_workflow_run_due(&self, _: Uuid) -> Result<bool, StorageError> {
        unimplemented!()
    }
    async fn list_due_parked_workflow_runs(
        &self,
        _: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<swage_storage::WorkflowRunRecord>, StorageError> {
        unimplemented!()
    }
    async fn list_parked_workflow_runs(
        &self,
    ) -> Result<Vec<swage_storage::WorkflowRunRecord>, StorageError> {
        unimplemented!()
    }
    async fn list_interrupted_workflow_runs(
        &self,
    ) -> Result<Vec<swage_storage::WorkflowRunRecord>, StorageError> {
        unimplemented!()
    }
    async fn create_workflow_run_event(
        &self,
        _: swage_storage::NewWorkflowRunEvent,
    ) -> Result<swage_storage::WorkflowRunEventRecord, StorageError> {
        unimplemented!()
    }
    async fn list_pending_workflow_run_events(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::WorkflowRunEventRecord>, StorageError> {
        unimplemented!()
    }
    async fn list_workflow_run_events(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::WorkflowRunEventRecord>, StorageError> {
        unimplemented!()
    }
    async fn mark_workflow_run_events_delivered(
        &self,
        _: Uuid,
        _: u64,
        _: u64,
    ) -> Result<u64, StorageError> {
        unimplemented!()
    }
    async fn mark_workflow_run_event_delivered(
        &self,
        _: Uuid,
        _: u64,
        _: u64,
    ) -> Result<bool, StorageError> {
        unimplemented!()
    }
    async fn create_workflow_journal_intent(
        &self,
        _: swage_storage::NewWorkflowJournalIntent,
    ) -> Result<swage_storage::WorkflowJournalRecord, StorageError> {
        unimplemented!()
    }
    async fn create_failed_workflow_journal_entry(
        &self,
        _: swage_storage::NewWorkflowJournalIntent,
        _: &str,
    ) -> Result<swage_storage::WorkflowJournalRecord, StorageError> {
        unimplemented!()
    }
    async fn complete_workflow_journal_entry(
        &self,
        _: Uuid,
        _: serde_json::Value,
    ) -> Result<swage_storage::WorkflowJournalRecord, StorageError> {
        unimplemented!()
    }
    async fn list_workflow_journal(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::WorkflowJournalRecord>, StorageError> {
        unimplemented!()
    }
}

#[async_trait]
impl swage_storage::SessionVariableStore for MockStorage {
    async fn set_session_variable(
        &self,
        _: Uuid,
        _: &str,
        _: &str,
    ) -> Result<swage_storage::SessionVariable, StorageError> {
        unimplemented!()
    }

    async fn list_session_variables(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::SessionVariable>, StorageError> {
        if self.fail_session_variable_lists.load(Ordering::SeqCst) {
            return Err(StorageError::QueryFailed(
                "injected session variable list failure".into(),
            ));
        }
        Ok(Vec::new())
    }

    async fn delete_session_variable(&self, _: Uuid, _: &str) -> Result<bool, StorageError> {
        unimplemented!()
    }
}

#[derive(Default)]
pub(crate) struct InMemoryDb {
    pub(crate) spaces: HashMap<Uuid, Space>,
    pub(crate) thread_branches: Vec<swage_storage::ThreadBranch>,
    pub(crate) approval_memory: std::collections::HashSet<(Uuid, String, String)>,
    pub(crate) sessions: HashMap<Uuid, Session>,
    pub(crate) devices: HashMap<String, DeviceRegistration>,
    pub(crate) channel_links: HashMap<(String, String, String), ChannelLink>,
    pub(crate) threads: HashMap<Uuid, Thread>,
    pub(crate) codex_thread_ids: HashMap<Uuid, String>,
    pub(crate) codex_tools_json: HashMap<Uuid, String>,
    pub(crate) turns: HashMap<Uuid, Turn>,
    pub(crate) messages: Vec<Message>,
    pub(crate) tool_calls: Vec<ToolCallRecord>,
    pub(crate) snapshots: HashMap<Uuid, (String, chrono::DateTime<chrono::Utc>)>,
    pub(crate) approvals: HashMap<Uuid, ApprovalRequest>,
    pub(crate) audit_events: Vec<swage_storage::AuditEvent>,
    pub(crate) agent_memories: Vec<swage_storage::AgentMemoryRecord>,
    pub(crate) agent_messages: HashMap<Uuid, AgentMessage>,
    pub(crate) schedules: HashMap<Uuid, Schedule>,
    pub(crate) schedule_runs: HashMap<Uuid, ScheduleRun>,
    pub(crate) attachments: HashMap<Uuid, Attachment>,
    pub(crate) background_jobs: HashMap<Uuid, BackgroundJob>,
    pub(crate) materials: HashMap<Uuid, Material>,
    pub(crate) workspace_attachments: HashMap<Uuid, WorkspaceAttachment>,
    pub(crate) secrets: HashMap<Uuid, swage_storage::SecretRecord>,
    pub(crate) managed_secret_values: HashMap<Uuid, String>,
}

impl MockStorage {
    pub(crate) fn new() -> Self {
        Self {
            db: Mutex::new(InMemoryDb::default()),
            fail_turn_state_writes: AtomicBool::new(false),
            fail_turn_state_write_at: AtomicUsize::new(0),
            turn_state_write_calls: AtomicUsize::new(0),
            idle_thread_state_write_calls: AtomicUsize::new(0),
            fail_inbound_audit_writes: AtomicBool::new(false),
            fail_append_message_at: AtomicUsize::new(0),
            append_message_calls: AtomicUsize::new(0),
            fail_message_lists: AtomicBool::new(false),
            fail_session_variable_lists: AtomicBool::new(false),
            fail_secret_reads: AtomicBool::new(false),
            fail_skill_activations: AtomicBool::new(false),
            fail_audit_writes: AtomicBool::new(false),
            fail_snapshot_writes: AtomicBool::new(false),
            last_snapshot_list_limit: AtomicUsize::new(0),
        }
    }

    pub(crate) fn fail_skill_activations(&self) {
        self.fail_skill_activations.store(true, Ordering::SeqCst);
    }

    fn fail_turn_state_writes(&self) {
        self.fail_turn_state_writes.store(true, Ordering::SeqCst);
    }

    fn fail_turn_state_write_at(&self, call: usize) {
        self.fail_turn_state_write_at.store(call, Ordering::SeqCst);
    }

    pub(crate) fn turn_state_write_calls(&self) -> usize {
        self.turn_state_write_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn idle_thread_state_write_calls(&self) -> usize {
        self.idle_thread_state_write_calls.load(Ordering::SeqCst)
    }

    fn fail_inbound_audit_writes(&self) {
        self.fail_inbound_audit_writes.store(true, Ordering::SeqCst);
    }

    fn fail_append_message_at(&self, call: usize) {
        self.fail_append_message_at.store(call, Ordering::SeqCst);
    }

    fn fail_message_lists(&self) {
        self.fail_message_lists.store(true, Ordering::SeqCst);
    }

    fn fail_session_variable_lists(&self) {
        self.fail_session_variable_lists
            .store(true, Ordering::SeqCst);
    }

    fn fail_secret_reads(&self) {
        self.fail_secret_reads.store(true, Ordering::SeqCst);
    }

    pub(crate) fn fail_audit_writes(&self) {
        self.fail_audit_writes.store(true, Ordering::SeqCst);
    }

    fn fail_snapshot_writes(&self) {
        self.fail_snapshot_writes.store(true, Ordering::SeqCst);
    }

    pub(crate) fn last_snapshot_list_limit(&self) -> usize {
        self.last_snapshot_list_limit.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SpaceStore for MockStorage {
    async fn create(&self, space: NewSpace) -> Result<Space, StorageError> {
        let p = Space {
            id: Uuid::new_v4(),
            name: space.name,
            description: String::new(),
            is_default: false,
            slug: space.slug,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.spaces.insert(p.id, p.clone());
        Ok(p)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Space>, StorageError> {
        Ok(self.db.lock().await.spaces.get(&id).cloned())
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Space>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .spaces
            .values()
            .find(|p| p.name == name)
            .cloned())
    }

    async fn get_or_create_default(&self) -> Result<Space, StorageError> {
        if let Some(p) = self
            .db
            .lock()
            .await
            .spaces
            .values()
            .find(|space| space.is_default)
            .cloned()
        {
            return Ok(p);
        }
        let space = Space {
            id: Uuid::new_v4(),
            name: "default".to_string(),
            description: String::new(),
            is_default: true,
            slug: "default".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.spaces.insert(space.id, space.clone());
        Ok(space)
    }

    async fn list_all(&self) -> Result<Vec<Space>, StorageError> {
        let mut spaces: Vec<Space> = self.db.lock().await.spaces.values().cloned().collect();
        spaces.sort_by_key(|p| p.created_at);
        Ok(spaces)
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<Space>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .spaces
            .values()
            .find(|p| p.slug == slug)
            .cloned())
    }

    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        slug: Option<SpaceSlug>,
    ) -> Result<Space, StorageError> {
        let mut db = self.db.lock().await;
        match db.spaces.get_mut(&id) {
            None => Err(StorageError::NotFound {
                entity: "Space".into(),
                id: id.to_string(),
            }),
            Some(p) => {
                if let Some(n) = name {
                    p.name = n;
                }
                if let Some(d) = description {
                    p.description = d;
                }
                if let Some(s) = slug {
                    p.slug = s;
                }
                p.updated_at = chrono::Utc::now();
                Ok(p.clone())
            }
        }
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        let mut db = self.db.lock().await;
        match db.spaces.get(&id) {
            None => Ok(false),
            Some(p) if p.name == "default" => Err(StorageError::ConstraintViolation(
                "cannot delete the default space".into(),
            )),
            _ => Ok(db.spaces.remove(&id).is_some()),
        }
    }
}

#[async_trait]
impl SessionStore for MockStorage {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError> {
        let s = Session {
            id: Uuid::new_v4(),
            space_id: session.space_id,
            agent_id: session.agent_id,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: session.metadata,
            selected_model_id: None,
            active_workspace_attachment_id: session.active_workspace_attachment_id,
        };
        self.db.lock().await.sessions.insert(s.id, s.clone());
        Ok(s)
    }

    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
        Ok(self.db.lock().await.sessions.get(&id).cloned())
    }

    async fn list_by_space(&self, space_id: Uuid) -> Result<Vec<Session>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .sessions
            .values()
            .filter(|s| s.space_id == space_id)
            .cloned()
            .collect())
    }

    async fn list_sessions_by_agent_id(
        &self,
        agent_id: &str,
    ) -> Result<Vec<Session>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .sessions
            .values()
            .filter(|s| s.agent_id == agent_id)
            .cloned()
            .collect())
    }

    async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.sessions.remove(&id).is_some())
    }

    async fn update_activity(&self, id: Uuid) -> Result<(), StorageError> {
        if let Some(session) = self.db.lock().await.sessions.get_mut(&id) {
            session.updated_at = chrono::Utc::now();
        }
        Ok(())
    }

    async fn set_session_metadata(
        &self,
        id: Uuid,
        metadata: Option<serde_json::Value>,
    ) -> Result<Session, StorageError> {
        let mut db = self.db.lock().await;
        let session = db
            .sessions
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })?;
        session.metadata = metadata;
        session.updated_at = chrono::Utc::now();
        Ok(session.clone())
    }

    async fn set_session_active_workspace(
        &self,
        id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<Session, StorageError> {
        let mut db = self.db.lock().await;
        let session = db
            .sessions
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })?;
        session.active_workspace_attachment_id = workspace_id;
        session.updated_at = chrono::Utc::now();
        Ok(session.clone())
    }

    async fn set_session_selected_model(
        &self,
        id: Uuid,
        selected_model_id: Option<String>,
    ) -> Result<Session, StorageError> {
        let mut db = self.db.lock().await;
        let session = db
            .sessions
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })?;
        session.selected_model_id = selected_model_id;
        session.updated_at = chrono::Utc::now();
        Ok(session.clone())
    }

    async fn find_session_by_channel_origin(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<Session>, StorageError> {
        let db = self.db.lock().await;
        Ok(db
            .sessions
            .values()
            .find(|session| {
                let Some(origin) = session
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("channel_origin"))
                else {
                    return false;
                };
                origin.get("integration").and_then(|value| value.as_str()) == Some(integration)
                    && origin.get("account").and_then(|value| value.as_str()) == Some(account)
                    && origin.get("conversation").and_then(|value| value.as_str())
                        == Some(conversation)
            })
            .cloned())
    }
}

#[async_trait]
impl WorkspaceAttachmentStore for MockStorage {
    async fn create_workspace_attachment(
        &self,
        workspace: NewWorkspaceAttachment,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let now = chrono::Utc::now();
        let record = WorkspaceAttachment {
            id: Uuid::new_v4(),
            space_id: workspace.space_id,
            label: workspace.label,

            path: workspace.path.clone(),
            canonical_path: workspace.path,
            is_missing: false,
            is_default: false,
            permissions: workspace.permissions,
            is_trusted: workspace.is_trusted,
            created_at: now,
            updated_at: now,
        };
        self.db
            .lock()
            .await
            .workspace_attachments
            .insert(record.id, record.clone());
        Ok(record)
    }

    async fn get_workspace_attachment(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkspaceAttachment>, StorageError> {
        Ok(self.db.lock().await.workspace_attachments.get(&id).cloned())
    }

    async fn list_workspace_attachments_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<WorkspaceAttachment>, StorageError> {
        let mut workspaces = self
            .db
            .lock()
            .await
            .workspace_attachments
            .values()
            .filter(|workspace| workspace.space_id == space_id)
            .cloned()
            .collect::<Vec<_>>();
        workspaces.sort_by_key(|workspace| workspace.created_at);
        Ok(workspaces)
    }

    async fn set_workspace_trust(
        &self,
        id: Uuid,
        is_trusted: bool,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let mut db = self.db.lock().await;
        let workspace =
            db.workspace_attachments
                .get_mut(&id)
                .ok_or_else(|| StorageError::NotFound {
                    entity: "workspace_attachment".to_string(),
                    id: id.to_string(),
                })?;
        workspace.is_trusted = is_trusted;
        workspace.updated_at = chrono::Utc::now();
        Ok(workspace.clone())
    }

    async fn set_workspace_missing_state(
        &self,
        id: Uuid,
        is_missing: bool,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let mut db = self.db.lock().await;
        let workspace =
            db.workspace_attachments
                .get_mut(&id)
                .ok_or_else(|| StorageError::NotFound {
                    entity: "workspace_attachment".to_string(),
                    id: id.to_string(),
                })?;
        workspace.is_missing = is_missing;
        workspace.updated_at = chrono::Utc::now();
        Ok(workspace.clone())
    }

    async fn set_space_default_workspace(
        &self,
        space_id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        if let Some(workspace_id) = workspace_id {
            let workspace = db.workspace_attachments.get(&workspace_id).ok_or_else(|| {
                StorageError::NotFound {
                    entity: "workspace_attachment".to_string(),
                    id: workspace_id.to_string(),
                }
            })?;
            if workspace.space_id != space_id {
                return Err(StorageError::ConstraintViolation(format!(
                    "workspace attachment {workspace_id} does not belong to space {space_id}"
                )));
            }
        }
        for workspace in db.workspace_attachments.values_mut() {
            if workspace.space_id == space_id {
                workspace.is_default = Some(workspace.id) == workspace_id;
                workspace.updated_at = chrono::Utc::now();
            }
        }
        Ok(())
    }

    async fn delete_workspace_attachment(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .workspace_attachments
            .remove(&id)
            .is_some())
    }
}

#[async_trait]
impl DeviceStore for MockStorage {
    async fn upsert_device(
        &self,
        device: NewDeviceRegistration,
    ) -> Result<DeviceRegistration, StorageError> {
        let registration = DeviceRegistration {
            device_id: device.device_id,
            label: device.label,
            kind: device.kind,
            base_url: device.base_url,
            shared_token: device.shared_token,
            workspace_root: device.workspace_root,
            workspace_roots: device.workspace_roots,
            capabilities: device.capabilities,
            registered_at: chrono::Utc::now(),
        };
        self.db
            .lock()
            .await
            .devices
            .insert(registration.device_id.clone(), registration.clone());
        Ok(registration)
    }

    async fn list_devices(&self) -> Result<Vec<DeviceRegistration>, StorageError> {
        let mut devices: Vec<DeviceRegistration> =
            self.db.lock().await.devices.values().cloned().collect();
        devices.sort_by(|left, right| {
            left.registered_at
                .cmp(&right.registered_at)
                .then_with(|| left.device_id.cmp(&right.device_id))
        });
        Ok(devices)
    }

    async fn delete_device(&self, device_id: &str) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.devices.remove(device_id).is_some())
    }
}

#[async_trait]
impl ChannelLinkStore for MockStorage {
    async fn upsert_channel_link(&self, link: NewChannelLink) -> Result<ChannelLink, StorageError> {
        let now = chrono::Utc::now();
        let key = (
            link.integration.clone(),
            link.account.clone(),
            link.conversation.clone(),
        );
        let linked_at = self
            .db
            .lock()
            .await
            .channel_links
            .get(&key)
            .map(|existing| existing.linked_at)
            .unwrap_or(now);
        let created = ChannelLink {
            integration: link.integration,
            account: link.account,
            conversation: link.conversation,
            space_id: link.space_id,
            agent_id: link.agent_id,
            session_id: link.session_id,
            home_target: link.home_target,
            linked_at,
            updated_at: now,
        };
        self.db
            .lock()
            .await
            .channel_links
            .insert(key, created.clone());
        Ok(created)
    }

    async fn get_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<ChannelLink>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .channel_links
            .get(&(
                integration.to_string(),
                account.to_string(),
                conversation.to_string(),
            ))
            .cloned())
    }

    async fn list_channel_links(&self) -> Result<Vec<ChannelLink>, StorageError> {
        let mut links: Vec<ChannelLink> = self
            .db
            .lock()
            .await
            .channel_links
            .values()
            .cloned()
            .collect();
        links.sort_by_key(|link| link.linked_at);
        Ok(links)
    }

    async fn delete_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<bool, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .channel_links
            .remove(&(
                integration.to_string(),
                account.to_string(),
                conversation.to_string(),
            ))
            .is_some())
    }
}

#[async_trait]
impl ScheduleStore for MockStorage {
    async fn create_schedule(&self, schedule: NewSchedule) -> Result<Schedule, StorageError> {
        let now = chrono::Utc::now();
        let created = Schedule {
            id: Uuid::new_v4(),
            message: schedule.message,
            status: schedule.status,
            kind: schedule.kind,
            expression: schedule.expression,
            session_policy: schedule.session_policy,
            creator: schedule.creator,
            delivery_routing: schedule.delivery_routing,
            gate: schedule.gate,
            next_run_at: schedule.next_run_at,
            last_run_at: schedule.last_run_at,
            created_at: now,
            updated_at: now,
        };
        self.db
            .lock()
            .await
            .schedules
            .insert(created.id, created.clone());
        Ok(created)
    }

    async fn get_schedule(&self, id: Uuid) -> Result<Option<Schedule>, StorageError> {
        Ok(self.db.lock().await.schedules.get(&id).cloned())
    }

    async fn list_schedules(&self) -> Result<Vec<Schedule>, StorageError> {
        let mut schedules: Vec<Schedule> =
            self.db.lock().await.schedules.values().cloned().collect();
        schedules.sort_by_key(|schedule| schedule.created_at);
        Ok(schedules)
    }

    async fn list_due_schedules(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Schedule>, StorageError> {
        let mut schedules: Vec<Schedule> = self
            .db
            .lock()
            .await
            .schedules
            .values()
            .filter(|schedule| {
                schedule.status == ScheduleStatus::Active
                    && schedule.next_run_at.is_some_and(|next| next <= now)
            })
            .cloned()
            .collect();
        schedules.sort_by_key(|schedule| schedule.next_run_at);
        Ok(schedules)
    }

    async fn update_schedule(
        &self,
        id: Uuid,
        update: UpdateSchedule,
    ) -> Result<Schedule, StorageError> {
        let mut db = self.db.lock().await;
        let schedule = db
            .schedules
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Schedule".into(),
                id: id.to_string(),
            })?;
        if let Some(message) = update.message {
            schedule.message = message;
        }
        if let Some(status) = update.status {
            schedule.status = status;
        }
        if let Some(kind) = update.kind {
            schedule.kind = kind;
        }
        if let Some(expression) = update.expression {
            schedule.expression = expression;
        }
        if let Some(delivery_routing) = update.delivery_routing {
            schedule.delivery_routing = delivery_routing;
        }
        if let Some(gate) = update.gate {
            schedule.gate = gate;
        }
        if let Some(next_run_at) = update.next_run_at {
            schedule.next_run_at = next_run_at;
        }
        if let Some(last_run_at) = update.last_run_at {
            schedule.last_run_at = last_run_at;
        }
        schedule.updated_at = chrono::Utc::now();
        Ok(schedule.clone())
    }

    async fn delete_schedule(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.schedules.remove(&id).is_some())
    }

    async fn create_schedule_run(&self, run: NewScheduleRun) -> Result<ScheduleRun, StorageError> {
        let terminal = matches!(
            run.state,
            ScheduleRunState::Completed
                | ScheduleRunState::CompletedEmpty
                | ScheduleRunState::Failed
                | ScheduleRunState::Skipped
        );
        let created = ScheduleRun {
            id: Uuid::new_v4(),
            schedule_id: run.schedule_id,
            trigger: run.trigger,
            state: run.state,
            skip_reason: run.skip_reason,
            scheduled_for: run.scheduled_for,
            session_id: None,
            turn_id: None,
            gate: None,
            output_text: None,
            error_message: run.error_message,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: terminal.then(chrono::Utc::now),
        };
        self.db
            .lock()
            .await
            .schedule_runs
            .insert(created.id, created.clone());
        Ok(created)
    }

    async fn get_schedule_run(&self, id: Uuid) -> Result<Option<ScheduleRun>, StorageError> {
        Ok(self.db.lock().await.schedule_runs.get(&id).cloned())
    }

    async fn list_schedule_runs(
        &self,
        schedule_id: Uuid,
    ) -> Result<Vec<ScheduleRun>, StorageError> {
        let mut runs: Vec<ScheduleRun> = self
            .db
            .lock()
            .await
            .schedule_runs
            .values()
            .filter(|run| run.schedule_id == schedule_id)
            .cloned()
            .collect();
        runs.sort_by_key(|run| run.created_at);
        Ok(runs)
    }

    async fn update_schedule_run(
        &self,
        id: Uuid,
        update: ScheduleRunUpdate,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        let run = db
            .schedule_runs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "ScheduleRun".into(),
                id: id.to_string(),
            })?;
        if let Some(state) = update.state {
            run.state = state;
            if matches!(state, ScheduleRunState::Running) && run.started_at.is_none() {
                run.started_at = Some(chrono::Utc::now());
            }
            if matches!(
                state,
                ScheduleRunState::Completed
                    | ScheduleRunState::CompletedEmpty
                    | ScheduleRunState::Failed
                    | ScheduleRunState::Skipped
            ) {
                run.completed_at = Some(chrono::Utc::now());
            }
        }
        if let Some(skip_reason) = update.skip_reason {
            run.skip_reason = Some(skip_reason);
        }
        if let Some(session_id) = update.session_id {
            run.session_id = Some(session_id);
        }
        if let Some(turn_id) = update.turn_id {
            run.turn_id = Some(turn_id);
        }
        if let Some(gate) = update.gate {
            run.gate = Some(gate);
        }
        if let Some(output_text) = update.output_text {
            run.output_text = Some(output_text);
        }
        if let Some(error_message) = update.error_message {
            run.error_message = Some(error_message);
        }
        Ok(())
    }

    async fn prune_schedule_runs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        max_age: std::time::Duration,
        max_per_schedule: u32,
    ) -> Result<u64, StorageError> {
        let cutoff = now
            - chrono::Duration::from_std(max_age)
                .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        let mut db = self.db.lock().await;
        let mut terminal: Vec<(Uuid, Uuid, chrono::DateTime<chrono::Utc>)> = db
            .schedule_runs
            .values()
            .filter(|run| {
                matches!(
                    run.state,
                    ScheduleRunState::Completed
                        | ScheduleRunState::CompletedEmpty
                        | ScheduleRunState::Failed
                        | ScheduleRunState::Skipped
                )
            })
            .map(|run| (run.id, run.schedule_id, run.created_at))
            .collect();

        let mut remove: std::collections::HashSet<Uuid> = terminal
            .iter()
            .filter_map(|(id, _, created_at)| (*created_at < cutoff).then_some(*id))
            .collect();

        terminal.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| right.0.cmp(&left.0))
        });

        let mut seen_by_schedule: std::collections::HashMap<Uuid, u32> =
            std::collections::HashMap::new();
        for (id, schedule_id, _) in terminal {
            let seen = seen_by_schedule.entry(schedule_id).or_default();
            *seen += 1;
            if *seen > max_per_schedule {
                remove.insert(id);
            }
        }

        let count = remove.len() as u64;
        for id in remove {
            db.schedule_runs.remove(&id);
        }
        Ok(count)
    }

    async fn recover_incomplete_schedule_runs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError> {
        let mut db = self.db.lock().await;
        let now = chrono::Utc::now();
        let mut recovered = 0;
        for run in db.schedule_runs.values_mut() {
            if matches!(
                run.state,
                ScheduleRunState::Pending | ScheduleRunState::Running
            ) {
                run.state = ScheduleRunState::Failed;
                run.error_message = Some(error_message.to_string());
                run.completed_at = Some(now);
                recovered += 1;
            }
        }
        Ok(recovered)
    }
}

#[async_trait]
impl AttachmentStore for MockStorage {
    async fn queue_attachment_ingestion(
        &self,
        attachment: NewAttachment,
        job: NewBackgroundJob,
    ) -> Result<(Attachment, BackgroundJob), StorageError> {
        let mut db = self.db.lock().await;
        let now = chrono::Utc::now();
        let byte_count = i64::try_from(attachment.bytes.len()).unwrap_or(i64::MAX);
        let attachment_record = Attachment {
            id: attachment.id,
            scope_kind: attachment.scope_kind,
            session_id: attachment.session_id,
            space_id: attachment.space_id,
            kind: attachment.kind,
            status: AttachmentStatus::Pending,
            filename: attachment.filename,
            mime_type: attachment.mime_type,
            bytes: attachment.bytes,
            byte_count,
            created_at: now,
            updated_at: now,
        };
        let job_record = BackgroundJob {
            id: job.id,
            kind: job.kind,
            state: swage_storage::BackgroundJobState::Queued,
            attachment_id: job.attachment_id,
            payload: job.payload,
            result: None,
            error_message: None,
            attempt_count: 0,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
        };
        db.attachments
            .insert(attachment_record.id, attachment_record.clone());
        db.background_jobs.insert(job_record.id, job_record.clone());
        Ok((attachment_record, job_record))
    }

    async fn get_attachment(&self, id: Uuid) -> Result<Option<Attachment>, StorageError> {
        Ok(self.db.lock().await.attachments.get(&id).cloned())
    }

    async fn list_attachments_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError> {
        let mut attachments: Vec<Attachment> = self
            .db
            .lock()
            .await
            .attachments
            .values()
            .filter(|attachment| attachment.session_id == Some(session_id))
            .cloned()
            .collect();
        attachments.sort_by_key(|attachment| attachment.created_at);
        Ok(attachments)
    }

    async fn list_attachments_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError> {
        let mut attachments: Vec<Attachment> = self
            .db
            .lock()
            .await
            .attachments
            .values()
            .filter(|attachment| attachment.space_id == Some(space_id))
            .cloned()
            .collect();
        attachments.sort_by_key(|attachment| attachment.created_at);
        Ok(attachments)
    }

    async fn update_attachment_ingestion(
        &self,
        id: Uuid,
        status: AttachmentStatus,
    ) -> Result<Attachment, StorageError> {
        let mut db = self.db.lock().await;
        let attachment = db
            .attachments
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Attachment".into(),
                id: id.to_string(),
            })?;
        attachment.status = status;
        attachment.updated_at = chrono::Utc::now();
        Ok(attachment.clone())
    }
}

#[async_trait]
impl BackgroundJobStore for MockStorage {
    async fn get_background_job(&self, id: Uuid) -> Result<Option<BackgroundJob>, StorageError> {
        Ok(self.db.lock().await.background_jobs.get(&id).cloned())
    }

    async fn list_runnable_background_jobs(&self) -> Result<Vec<BackgroundJob>, StorageError> {
        let mut jobs: Vec<BackgroundJob> = self
            .db
            .lock()
            .await
            .background_jobs
            .values()
            .filter(|job| {
                matches!(
                    job.state,
                    swage_storage::BackgroundJobState::Queued
                        | swage_storage::BackgroundJobState::Retried
                )
            })
            .cloned()
            .collect();
        jobs.sort_by_key(|job| job.created_at);
        Ok(jobs)
    }

    async fn mark_background_job_running(&self, id: Uuid) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        let now = chrono::Utc::now();
        job.state = swage_storage::BackgroundJobState::Running;
        job.attempt_count += 1;
        job.started_at = Some(now);
        job.completed_at = None;
        job.updated_at = now;
        Ok(job.clone())
    }

    async fn complete_background_job(
        &self,
        id: Uuid,
        result: Option<&serde_json::Value>,
    ) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        let now = chrono::Utc::now();
        job.state = swage_storage::BackgroundJobState::Completed;
        job.result = result.cloned();
        job.error_message = None;
        job.completed_at = Some(now);
        job.updated_at = now;
        Ok(job.clone())
    }

    async fn fail_background_job(
        &self,
        id: Uuid,
        error_message: &str,
    ) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        let now = chrono::Utc::now();
        job.state = swage_storage::BackgroundJobState::Failed;
        job.error_message = Some(error_message.to_string());
        job.completed_at = Some(now);
        job.updated_at = now;
        Ok(job.clone())
    }

    async fn retry_background_job(&self, id: Uuid) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        job.state = swage_storage::BackgroundJobState::Retried;
        job.result = None;
        job.error_message = None;
        job.started_at = None;
        job.completed_at = None;
        job.updated_at = chrono::Utc::now();
        Ok(job.clone())
    }

    async fn list_background_jobs_by_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Vec<BackgroundJob>, StorageError> {
        let mut jobs: Vec<BackgroundJob> = self
            .db
            .lock()
            .await
            .background_jobs
            .values()
            .filter(|job| job.attachment_id == Some(attachment_id))
            .cloned()
            .collect();
        jobs.sort_by_key(|job| job.created_at);
        Ok(jobs)
    }

    async fn recover_running_background_jobs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError> {
        let mut db = self.db.lock().await;
        let now = chrono::Utc::now();
        let mut recovered = 0;
        for job in db.background_jobs.values_mut() {
            if job.state == swage_storage::BackgroundJobState::Running {
                job.state = swage_storage::BackgroundJobState::Retried;
                job.error_message = Some(error_message.to_string());
                job.completed_at = None;
                job.updated_at = now;
                recovered += 1;
            }
        }
        Ok(recovered)
    }
}

#[async_trait]
impl MaterialStore for MockStorage {
    async fn create_material(&self, material: NewMaterial) -> Result<Material, StorageError> {
        let now = chrono::Utc::now();
        let record = Material {
            id: material.id,
            scope_kind: material.scope_kind,
            session_id: material.session_id,
            space_id: material.space_id,
            source_kind: material.source_kind,
            source_attachment_id: material.source_attachment_id,
            status: MaterialStatus::Active,
            title: material.title,
            relative_path: material.relative_path,
            mime_type: material.mime_type,
            content_text: material.content_text,
            created_at: now,
            updated_at: now,
            archived_at: None,
        };
        self.db
            .lock()
            .await
            .materials
            .insert(record.id, record.clone());
        Ok(record)
    }

    async fn get_material(&self, id: Uuid) -> Result<Option<Material>, StorageError> {
        Ok(self.db.lock().await.materials.get(&id).cloned())
    }

    async fn get_material_by_source_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Option<Material>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .materials
            .values()
            .find(|material| material.source_attachment_id == Some(attachment_id))
            .cloned())
    }

    async fn list_materials_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Material>, StorageError> {
        let mut materials: Vec<Material> = self
            .db
            .lock()
            .await
            .materials
            .values()
            .filter(|material| material.session_id == Some(session_id))
            .cloned()
            .collect();
        materials.sort_by_key(|material| material.created_at);
        Ok(materials)
    }

    async fn list_materials_by_space(&self, space_id: Uuid) -> Result<Vec<Material>, StorageError> {
        let mut materials: Vec<Material> = self
            .db
            .lock()
            .await
            .materials
            .values()
            .filter(|material| {
                material.space_id == Some(space_id)
                    && material.scope_kind == AttachmentScopeKind::Space
            })
            .cloned()
            .collect();
        materials.sort_by_key(|material| material.created_at);
        Ok(materials)
    }

    async fn update_material_content(
        &self,
        id: Uuid,
        content_text: &str,
        mime_type: &str,
    ) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.content_text = content_text.to_string();
        material.mime_type = mime_type.to_string();
        material.status = MaterialStatus::Active;
        material.archived_at = None;
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }

    async fn archive_material(&self, id: Uuid) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.status = MaterialStatus::Archived;
        material.archived_at = Some(chrono::Utc::now());
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }

    async fn restore_material(&self, id: Uuid) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.status = MaterialStatus::Active;
        material.archived_at = None;
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }

    async fn promote_material(
        &self,
        id: Uuid,
        scope_kind: AttachmentScopeKind,
        session_id: Option<Uuid>,
        space_id: Option<Uuid>,
        relative_path: &str,
    ) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.scope_kind = scope_kind;
        material.session_id = session_id;
        material.space_id = space_id;
        material.relative_path = relative_path.to_string();
        material.status = MaterialStatus::Active;
        material.archived_at = None;
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }
}

#[async_trait]
impl ThreadStore for MockStorage {
    async fn create(&self, thread: NewThread) -> Result<Thread, StorageError> {
        let t = Thread {
            id: Uuid::new_v4(),
            session_id: thread.session_id,
            agent_id: thread.agent_id,
            state: ThreadState::Idle,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.threads.insert(t.id, t.clone());
        Ok(t)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Thread>, StorageError> {
        Ok(self.db.lock().await.threads.get(&id).cloned())
    }

    async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<Thread>, StorageError> {
        let mut threads: Vec<Thread> = self
            .db
            .lock()
            .await
            .threads
            .values()
            .filter(|t| t.session_id == session_id)
            .cloned()
            .collect();
        threads.sort_by_key(|t| t.created_at);
        Ok(threads)
    }

    async fn list_threads_by_agent_id(&self, agent_id: &str) -> Result<Vec<Thread>, StorageError> {
        let mut threads: Vec<Thread> = self
            .db
            .lock()
            .await
            .threads
            .values()
            .filter(|t| t.agent_id == agent_id)
            .cloned()
            .collect();
        threads.sort_by_key(|t| t.created_at);
        Ok(threads)
    }

    async fn update_state(&self, id: Uuid, state: ThreadState) -> Result<(), StorageError> {
        if state == ThreadState::Idle {
            self.idle_thread_state_write_calls
                .fetch_add(1, Ordering::SeqCst);
        }
        let mut db = self.db.lock().await;
        match db.threads.get_mut(&id) {
            None => Err(StorageError::NotFound {
                entity: "Thread".into(),
                id: id.to_string(),
            }),
            Some(t) => {
                if !t.state.can_transition_to(&state) {
                    return Err(StorageError::ConstraintViolation(format!(
                        "invalid thread state transition: {:?} → {:?}",
                        t.state, state
                    )));
                }
                t.state = state;
                t.updated_at = chrono::Utc::now();
                Ok(())
            }
        }
    }

    async fn get_codex_thread_id(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        Ok(self.db.lock().await.codex_thread_ids.get(&id).cloned())
    }

    async fn set_codex_thread_id(
        &self,
        id: Uuid,
        codex_thread_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        match codex_thread_id {
            Some(value) => {
                db.codex_thread_ids.insert(id, value.to_string());
            }
            None => {
                db.codex_thread_ids.remove(&id);
            }
        }
        Ok(())
    }

    async fn get_codex_tools_json(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        Ok(self.db.lock().await.codex_tools_json.get(&id).cloned())
    }

    async fn set_codex_tools_json(
        &self,
        id: Uuid,
        tools_json: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        match tools_json {
            Some(value) => {
                db.codex_tools_json.insert(id, value.to_string());
            }
            None => {
                db.codex_tools_json.remove(&id);
            }
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.threads.remove(&id).is_some())
    }

    async fn create_branch(
        &self,
        parent_thread_id: Uuid,
        branched_from_message_id: Uuid,
    ) -> Result<swage_storage::ThreadBranch, StorageError> {
        let mut db = self.db.lock().await;
        let parent =
            db.threads
                .get(&parent_thread_id)
                .cloned()
                .ok_or_else(|| StorageError::NotFound {
                    entity: "Thread".into(),
                    id: parent_thread_id.to_string(),
                })?;
        let source_position = db
            .messages
            .iter()
            .find(|message| {
                message.id == branched_from_message_id && message.thread_id == parent_thread_id
            })
            .map(|message| message.position)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Message".into(),
                id: branched_from_message_id.to_string(),
            })?;
        let branch = swage_storage::ThreadBranch {
            child_thread_id: Uuid::new_v4(),
            parent_thread_id,
            branched_from_message_id,
            created_at: chrono::Utc::now(),
        };
        db.threads.insert(
            branch.child_thread_id,
            Thread {
                id: branch.child_thread_id,
                session_id: parent.session_id,
                agent_id: parent.agent_id,
                state: ThreadState::Idle,
                created_at: branch.created_at,
                updated_at: branch.created_at,
            },
        );
        let messages_to_copy: Vec<_> = db
            .messages
            .iter()
            .filter(|message| {
                message.thread_id == parent_thread_id && message.position <= source_position
            })
            .cloned()
            .collect();
        for message in messages_to_copy {
            let message_id = Uuid::new_v4();
            db.messages.push(swage_storage::Message {
                id: message_id,
                thread_id: branch.child_thread_id,
                attachments: Vec::new(),
                turn_id: message.turn_id,
                role: message.role,
                content: message.content,
                position: message.position,
                agent_id: message.agent_id,
                tool_call_id: message.tool_call_id,
                tool_name: message.tool_name,
                created_at: message.created_at,
            });
        }
        db.thread_branches.push(branch.clone());
        Ok(branch)
    }

    async fn list_branches_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<swage_storage::ThreadBranch>, StorageError> {
        let db = self.db.lock().await;
        Ok(db
            .thread_branches
            .iter()
            .filter(|branch| {
                db.threads
                    .get(&branch.child_thread_id)
                    .map(|thread| thread.session_id == session_id)
                    .unwrap_or(false)
            })
            .cloned()
            .collect())
    }

    async fn get_branch(
        &self,
        child_thread_id: Uuid,
    ) -> Result<Option<swage_storage::ThreadBranch>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .thread_branches
            .iter()
            .find(|branch| branch.child_thread_id == child_thread_id)
            .cloned())
    }
}

#[async_trait]
impl swage_storage::PageStore for MockStorage {
    async fn create_page(
        &self,
        _: swage_storage::NewPage,
    ) -> Result<swage_storage::Page, StorageError> {
        unimplemented!()
    }
    async fn get_page(&self, _: Uuid) -> Result<Option<swage_storage::Page>, StorageError> {
        unimplemented!()
    }
    async fn list_pages_by_space(&self, _: Uuid) -> Result<Vec<swage_storage::Page>, StorageError> {
        unimplemented!()
    }
    async fn create_page_module(
        &self,
        _: swage_storage::NewPageModule,
    ) -> Result<swage_storage::PageModule, StorageError> {
        unimplemented!()
    }
    async fn get_page_module(
        &self,
        _: Uuid,
    ) -> Result<Option<swage_storage::PageModule>, StorageError> {
        unimplemented!()
    }
    async fn list_page_modules(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::PageModule>, StorageError> {
        unimplemented!()
    }
    async fn update_page_module_source(&self, _: Uuid, _: &str) -> Result<(), StorageError> {
        unimplemented!()
    }
    async fn create_page_template(
        &self,
        _: swage_storage::NewPageTemplate,
    ) -> Result<swage_storage::PageTemplate, StorageError> {
        unimplemented!()
    }
    async fn create_page_template_with_modules(
        &self,
        _: swage_storage::NewPageTemplate,
        _: Vec<swage_storage::NewPageTemplateModuleInput>,
    ) -> Result<swage_storage::PageTemplate, StorageError> {
        unimplemented!()
    }
    async fn get_page_template(
        &self,
        _: Uuid,
    ) -> Result<Option<swage_storage::PageTemplate>, StorageError> {
        unimplemented!()
    }
    async fn list_page_templates_by_space(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::PageTemplate>, StorageError> {
        unimplemented!()
    }
    async fn create_page_template_module(
        &self,
        _: swage_storage::NewPageTemplateModule,
    ) -> Result<swage_storage::PageTemplateModule, StorageError> {
        unimplemented!()
    }
    async fn list_page_template_modules(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::PageTemplateModule>, StorageError> {
        unimplemented!()
    }
    async fn create_page_from_template(
        &self,
        _: swage_storage::NewPage,
        _: Uuid,
    ) -> Result<swage_storage::Page, StorageError> {
        unimplemented!()
    }
}

#[async_trait]
impl swage_storage::PageAnnotationStore for MockStorage {
    async fn create_page_annotation(
        &self,
        _: swage_storage::NewPageAnnotation,
    ) -> Result<swage_storage::PageAnnotation, StorageError> {
        unimplemented!()
    }
    async fn get_page_annotation(
        &self,
        _: Uuid,
    ) -> Result<Option<swage_storage::PageAnnotation>, StorageError> {
        unimplemented!()
    }
    async fn list_page_annotations(
        &self,
        _: Uuid,
    ) -> Result<Vec<swage_storage::PageAnnotation>, StorageError> {
        unimplemented!()
    }
    async fn update_page_annotation(
        &self,
        _: Uuid,
        _: &str,
        _: &[String],
    ) -> Result<(), StorageError> {
        unimplemented!()
    }
    async fn delete_page_annotation_with_audit(
        &self,
        _: PageAnnotation,
        _: NewAuditEvent,
    ) -> Result<bool, StorageError> {
        unimplemented!()
    }
}

#[async_trait]
impl TurnStore for MockStorage {
    async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError> {
        let t = Turn {
            id: Uuid::new_v4(),
            thread_id: turn.thread_id,
            agent_id: turn.agent_id,
            state: turn.state,
            user_input: turn.user_input,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: None,
            parent_turn_id: turn.parent_turn_id,
            created_at: chrono::Utc::now(),
            completed_at: None,
            delegated_input_tokens: None,
            delegated_output_tokens: None,
        };
        self.db.lock().await.turns.insert(t.id, t.clone());
        Ok(t)
    }

    async fn update_structured_output(
        &self,
        id: Uuid,
        output: &serde_json::Value,
    ) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.structured_output = Some(output.clone());
        }
        Ok(())
    }

    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
        Ok(self.db.lock().await.turns.get(&id).cloned())
    }

    async fn list_turns_by_thread(&self, thread_id: Uuid) -> Result<Vec<Turn>, StorageError> {
        let mut turns: Vec<Turn> = self
            .db
            .lock()
            .await
            .turns
            .values()
            .filter(|t| t.thread_id == thread_id)
            .cloned()
            .collect();
        turns.sort_by_key(|t| t.created_at);
        Ok(turns)
    }

    async fn update_turn_state(&self, id: Uuid, state: TurnState) -> Result<(), StorageError> {
        let call = self.turn_state_write_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_turn_state_writes.load(Ordering::SeqCst)
            || self.fail_turn_state_write_at.load(Ordering::SeqCst) == call
        {
            return Err(StorageError::QueryFailed(
                "injected turn state write failure".into(),
            ));
        }
        let mut db = self.db.lock().await;
        if let Some(turn) = db.turns.get_mut(&id) {
            if matches!(
                state,
                TurnState::Completed | TurnState::Failed | TurnState::Interrupted
            ) {
                turn.completed_at = Some(chrono::Utc::now());
            }
            turn.state = state;
        }
        Ok(())
    }

    async fn force_turn_failure(&self, id: Uuid) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        if let Some(turn) = db.turns.get_mut(&id) {
            turn.completed_at.get_or_insert_with(chrono::Utc::now);
            turn.state = TurnState::Failed;
        }
        Ok(())
    }

    async fn find_in_progress_by_agent(&self, agent_id: &str) -> Result<Vec<Turn>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .turns
            .values()
            .filter(|t| {
                t.agent_id == agent_id
                    && matches!(
                        t.state,
                        TurnState::Pending
                            | TurnState::Processing
                            | TurnState::AwaitingApproval
                            | TurnState::Delegating
                    )
            })
            .cloned()
            .collect())
    }

    async fn update_response(&self, id: Uuid, response: &str) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.assistant_response = Some(response.to_string());
        }
        Ok(())
    }

    async fn update_tokens(&self, id: Uuid, input: i64, output: i64) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.input_tokens = Some(input);
            t.output_tokens = Some(output);
        }
        Ok(())
    }

    async fn add_delegated_tokens(
        &self,
        id: Uuid,
        input: i64,
        output: i64,
    ) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.delegated_input_tokens = Some(t.delegated_input_tokens.unwrap_or(0) + input);
            t.delegated_output_tokens = Some(t.delegated_output_tokens.unwrap_or(0) + output);
        }
        Ok(())
    }

    async fn update_cost(&self, id: Uuid, cost: i64) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.cost_microcents = Some(cost);
        }
        Ok(())
    }

    async fn update_inbound_audit(
        &self,
        id: Uuid,
        audit: &swage_storage::InboundAudit,
    ) -> Result<(), StorageError> {
        if self.fail_inbound_audit_writes.load(Ordering::SeqCst) {
            return Err(StorageError::QueryFailed(
                "injected inbound audit write failure".into(),
            ));
        }
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.inbound_audit = Some(audit.clone());
        }
        Ok(())
    }
}

#[async_trait]
impl ContextSnapshotStore for MockStorage {
    async fn store_snapshot(&self, turn_id: Uuid, report_json: &str) -> Result<(), StorageError> {
        if self.fail_snapshot_writes.load(Ordering::SeqCst) {
            return Err(StorageError::QueryFailed(
                "injected snapshot write failure".into(),
            ));
        }
        self.db
            .lock()
            .await
            .snapshots
            .insert(turn_id, (report_json.to_string(), chrono::Utc::now()));
        Ok(())
    }

    async fn get_snapshot(&self, turn_id: Uuid) -> Result<Option<String>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .snapshots
            .get(&turn_id)
            .map(|(report_json, _)| report_json.clone()))
    }

    async fn list_snapshots_by_space(
        &self,
        space_id: Uuid,
        limit: usize,
    ) -> Result<Vec<swage_storage::ContextSnapshotRecord>, StorageError> {
        self.last_snapshot_list_limit.store(limit, Ordering::SeqCst);
        let db = self.db.lock().await;
        let mut records = Vec::new();
        for (turn_id, (report_json, created_at)) in &db.snapshots {
            let Some(turn) = db.turns.get(turn_id) else {
                continue;
            };
            let Some(thread) = db.threads.get(&turn.thread_id) else {
                continue;
            };
            let Some(session) = db.sessions.get(&thread.session_id) else {
                continue;
            };
            if session.space_id != space_id {
                continue;
            }
            records.push(swage_storage::ContextSnapshotRecord {
                turn_id: *turn_id,
                session_id: session.id,
                space_id,
                created_at: *created_at,
                report_json: report_json.clone(),
            });
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        records.truncate(limit);
        Ok(records)
    }
}

#[async_trait]
impl MessageStore for MockStorage {
    async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError> {
        let m = Message {
            id: Uuid::new_v4(),
            thread_id: message.thread_id,
            attachments: message.attachments,
            turn_id: message.turn_id,
            role: message.role,
            content: message.content,
            position: message.position,
            agent_id: message.agent_id,
            tool_call_id: None,
            tool_name: None,
            created_at: chrono::Utc::now(),
        };
        self.db.lock().await.messages.push(m.clone());
        Ok(m)
    }

    async fn append_message(&self, message: AppendMessage) -> Result<Message, StorageError> {
        let call = self.append_message_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_append_message_at.load(Ordering::SeqCst) == call {
            return Err(StorageError::QueryFailed(
                "injected append message failure".into(),
            ));
        }
        let mut db = self.db.lock().await;
        let position = db
            .messages
            .iter()
            .filter(|existing| existing.thread_id == message.thread_id)
            .map(|existing| existing.position)
            .max()
            .map_or(0, |max_position| max_position + 1);
        let created = Message {
            id: Uuid::new_v4(),
            thread_id: message.thread_id,
            attachments: message.attachments,
            turn_id: message.turn_id,
            role: message.role,
            content: message.content,
            position,
            agent_id: message.agent_id,
            tool_call_id: None,
            tool_name: None,
            created_at: chrono::Utc::now(),
        };
        db.messages.push(created.clone());
        Ok(created)
    }

    async fn list_messages_by_thread(&self, thread_id: Uuid) -> Result<Vec<Message>, StorageError> {
        if self.fail_message_lists.load(Ordering::SeqCst) {
            return Err(StorageError::QueryFailed(
                "injected message list failure".into(),
            ));
        }
        let db = self.db.lock().await;
        let mut msgs: Vec<Message> = db
            .messages
            .iter()
            .filter(|m| m.thread_id == thread_id)
            .cloned()
            .collect();
        msgs.sort_by_key(|m| m.position);
        Ok(msgs)
    }

    async fn get_latest_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Message>, StorageError> {
        let db = self.db.lock().await;
        // Collect thread_ids belonging to this session.
        let thread_ids: std::collections::HashSet<Uuid> = db
            .threads
            .values()
            .filter(|t| t.session_id == session_id)
            .map(|t| t.id)
            .collect();
        // Return the message with the highest position across all threads.
        Ok(db
            .messages
            .iter()
            .filter(|m| thread_ids.contains(&m.thread_id))
            .max_by_key(|m| (m.created_at, m.position))
            .cloned())
    }

    async fn get_message(&self, id: Uuid) -> Result<Option<Message>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .messages
            .iter()
            .find(|m| m.id == id)
            .cloned())
    }

    async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError> {
        self.db
            .lock()
            .await
            .messages
            .retain(|m| m.thread_id != thread_id);
        Ok(())
    }
}

#[async_trait]
impl ToolCallStore for MockStorage {
    async fn create_tool_call(
        &self,
        tool_call: NewToolCall,
    ) -> Result<ToolCallRecord, StorageError> {
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: tool_call.turn_id,
            agent_id: tool_call.agent_id,
            call_id: tool_call.call_id,
            tool: tool_call.tool,
            arguments: tool_call.arguments,
            model_arguments: tool_call.model_arguments,
            result: tool_call.result,
            model_result: tool_call.model_result,
            status: tool_call.status,
            duration_ms: tool_call.duration_ms,
            truncated: tool_call.truncated,
            total_bytes: tool_call.total_bytes,
            provider_round: tool_call.provider_round,
            position: tool_call.position,
            created_at: chrono::Utc::now(),
            parent_call_id: tool_call.parent_call_id,
            display_summary: None,
        };
        self.db.lock().await.tool_calls.push(record.clone());
        Ok(record)
    }

    async fn get_tool_call(&self, id: Uuid) -> Result<Option<ToolCallRecord>, StorageError> {
        let db = self.db.lock().await;
        Ok(db.tool_calls.iter().find(|tc| tc.id == id).cloned())
    }

    async fn get_tool_call_by_turn_and_call_id(
        &self,
        turn_id: Uuid,
        call_id: &str,
    ) -> Result<Option<ToolCallRecord>, StorageError> {
        let db = self.db.lock().await;
        Ok(db
            .tool_calls
            .iter()
            .find(|tc| tc.turn_id == turn_id && tc.call_id == call_id)
            .cloned())
    }

    async fn update_tool_call_result(
        &self,
        call_id: &str,
        turn_id: Uuid,
        result: &str,
        status: &str,
        duration_ms: i64,
        truncated: bool,
        total_bytes: Option<i64>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        if let Some(tc) = db
            .tool_calls
            .iter_mut()
            .find(|tc| tc.call_id == call_id && tc.turn_id == turn_id)
        {
            tc.result = Some(result.to_string());
            tc.model_result = Some(swage_core::compact_tool_result_for_model_with_reference(
                result,
                status != "success",
                swage_core::tool_result_reference(&turn_id.to_string(), call_id),
            ));
            tc.status = Some(status.to_string());
            tc.duration_ms = Some(duration_ms);
            tc.truncated = truncated;
            tc.total_bytes = total_bytes;
        }
        Ok(())
    }

    async fn list_tool_calls_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ToolCallRecord>, StorageError> {
        let db = self.db.lock().await;
        let mut calls: Vec<ToolCallRecord> = db
            .tool_calls
            .iter()
            .filter(|tc| tc.turn_id == turn_id)
            .cloned()
            .collect();
        calls.sort_by_key(|tc| tc.position);
        Ok(calls)
    }
}

// Stub: skill activation is not exercised in gateway turn tests.
#[async_trait]
impl SkillActivationStore for MockStorage {
    async fn activate_skill(
        &self,
        _session_id: Uuid,
        _skill_name: &str,
    ) -> Result<(), StorageError> {
        if self.fail_skill_activations.load(Ordering::SeqCst) {
            return Err(StorageError::QueryFailed(
                "injected skill activation failure".to_owned(),
            ));
        }
        Ok(())
    }
    async fn deactivate_skill(
        &self,
        _session_id: Uuid,
        _skill_name: &str,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }
    async fn list_active_skills(
        &self,
        _session_id: Uuid,
    ) -> Result<Vec<ActiveSkillRecord>, StorageError> {
        Ok(vec![])
    }
    async fn clear_active_skills(&self, _session_id: Uuid) -> Result<(), StorageError> {
        Ok(())
    }
}

#[async_trait]
impl ApprovalStore for MockStorage {
    async fn create_approval(
        &self,
        approval: NewApprovalRequest,
    ) -> Result<ApprovalRequest, StorageError> {
        let approval_req = ApprovalRequest {
            id: Uuid::new_v4(),
            turn_id: approval.turn_id,
            agent_id: approval.agent_id,
            kind: approval.kind,
            tool_name: approval.tool_name,
            tool_arguments: approval.tool_arguments,
            hook_name: approval.hook_name,
            prompt: approval.prompt,
            context: approval.context,
            timeout_at: approval.timeout_at,
            timeout_policy: approval.timeout_policy,
            state: ApprovalState::Pending,
            decision_reason: None,
            decided_at: None,
            created_at: chrono::Utc::now(),
        };
        self.db
            .lock()
            .await
            .approvals
            .insert(approval_req.id, approval_req.clone());
        Ok(approval_req)
    }

    async fn get_approval(&self, id: Uuid) -> Result<Option<ApprovalRequest>, StorageError> {
        Ok(self.db.lock().await.approvals.get(&id).cloned())
    }

    async fn list_pending_by_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<ApprovalRequest>, StorageError> {
        let db = self.db.lock().await;
        let mut result: Vec<_> = db
            .approvals
            .values()
            .filter(|a| a.agent_id == agent_id && a.state == ApprovalState::Pending)
            .cloned()
            .collect();
        result.sort_by_key(|a| a.created_at);
        Ok(result)
    }

    async fn list_pending_by_session(&self, _: Uuid) -> Result<Vec<ApprovalRequest>, StorageError> {
        unimplemented!()
    }

    async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
        let db = self.db.lock().await;
        let mut result: Vec<_> = db
            .approvals
            .values()
            .filter(|a| a.state == ApprovalState::Pending)
            .cloned()
            .collect();
        result.sort_by_key(|a| a.created_at);
        Ok(result)
    }

    async fn list_approvals(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
        let db = self.db.lock().await;
        let mut result: Vec<_> = db.approvals.values().cloned().collect();
        result.sort_by_key(|approval| std::cmp::Reverse(approval.created_at));
        Ok(result)
    }

    async fn update_approval_state(
        &self,
        id: Uuid,
        state: ApprovalState,
        reason: Option<&str>,
    ) -> Result<bool, StorageError> {
        let mut db = self.db.lock().await;
        match db.approvals.get_mut(&id) {
            None => Ok(false),
            Some(approval) => {
                if approval.state != ApprovalState::Pending {
                    return Ok(false);
                }
                approval.state = state;
                approval.decision_reason = reason.map(|s| s.to_string());
                approval.decided_at = Some(chrono::Utc::now());
                Ok(true)
            }
        }
    }

    async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError> {
        let mut db = self.db.lock().await;
        let expired_ids: Vec<_> = db
            .approvals
            .iter()
            .filter(|(_, a)| !db.turns.contains_key(&a.turn_id))
            .map(|(id, _)| *id)
            .collect();
        let count = expired_ids.len() as u64;
        for id in expired_ids {
            if let Some(approval) = db.approvals.get_mut(&id)
                && approval.state == ApprovalState::Pending
            {
                approval.state = ApprovalState::Expired;
                approval.decided_at = Some(chrono::Utc::now());
            }
        }
        Ok(count)
    }

    async fn remember_approval(
        &self,
        session_id: Uuid,
        tool_name: &str,
        request_fingerprint: &str,
    ) -> Result<(), StorageError> {
        self.db.lock().await.approval_memory.insert((
            session_id,
            tool_name.to_string(),
            request_fingerprint.to_string(),
        ));
        Ok(())
    }

    async fn has_approval_memory(
        &self,
        session_id: Uuid,
        tool_name: &str,
        request_fingerprint: &str,
    ) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.approval_memory.contains(&(
            session_id,
            tool_name.to_string(),
            request_fingerprint.to_string(),
        )))
    }
}

#[async_trait]
impl swage_storage::store::DeliveryStore for MockStorage {
    async fn create_delivery_record(
        &self,
        delivery: swage_storage::NewDeliveryRecord,
    ) -> Result<swage_storage::DeliveryRecord, StorageError> {
        Ok(swage_storage::DeliveryRecord {
            id: Uuid::new_v4(),
            session_id: delivery.session_id,
            run_id: delivery.run_id,
            turn_id: delivery.turn_id,
            target: delivery.target,
            content: delivery.content,
            status: delivery.status,
            error: delivery.error,
            delivered_at: chrono::Utc::now(),
        })
    }

    async fn list_delivery_records(
        &self,
    ) -> Result<Vec<swage_storage::DeliveryRecord>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_delivery_records_by_session(
        &self,
        _session_id: Uuid,
    ) -> Result<Vec<swage_storage::DeliveryRecord>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl swage_storage::store::ChannelQueueStore for MockStorage {
    async fn ingest_channel_inbound(
        &self,
        _messages: Vec<swage_storage::NewChannelInboundMessage>,
        _cursor_integration: &str,
        _cursor_account: &str,
        _cursor: &str,
    ) -> Result<usize, StorageError> {
        Ok(0)
    }

    async fn get_channel_cursor(
        &self,
        _integration: &str,
        _account: &str,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn list_pending_channel_origins(
        &self,
    ) -> Result<Vec<swage_storage::PendingChannelOrigin>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_pending_by_origin(
        &self,
        _origin: swage_storage::PendingChannelOrigin,
    ) -> Result<Vec<swage_storage::ChannelInboundMessage>, StorageError> {
        Ok(Vec::new())
    }

    async fn begin_channel_turn(
        &self,
        _thread_id: Uuid,
        _agent_id: &str,
        _composed_input: &str,
        _message_ids: Vec<Uuid>,
    ) -> Result<Turn, StorageError> {
        unimplemented!("MockStorage does not support channel queue turn claims")
    }

    async fn discard_channel_messages(
        &self,
        _message_ids: Vec<Uuid>,
        _reason: &str,
    ) -> Result<u64, StorageError> {
        Ok(0)
    }

    async fn requeue_presented_for_inflight_turns(&self) -> Result<u64, StorageError> {
        Ok(0)
    }
}

#[async_trait]
impl swage_storage::store::AuditStore for MockStorage {
    async fn create_audit_event(
        &self,
        event: swage_storage::NewAuditEvent,
    ) -> Result<swage_storage::AuditEvent, StorageError> {
        if self.fail_audit_writes.load(Ordering::SeqCst) {
            return Err(StorageError::QueryFailed(
                "injected audit write failure".into(),
            ));
        }
        let event = swage_storage::AuditEvent {
            id: event.id,
            session_id: event.session_id,
            entity_kind: event.entity_kind,
            entity_id: event.entity_id,
            event_kind: event.event_kind,
            summary: event.summary,
            metadata: event.metadata,
            created_at: chrono::Utc::now(),
        };
        self.db.lock().await.audit_events.push(event.clone());
        Ok(event)
    }

    async fn list_audit_events(&self) -> Result<Vec<swage_storage::AuditEvent>, StorageError> {
        Ok(self.db.lock().await.audit_events.clone())
    }

    async fn list_audit_events_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<swage_storage::AuditEvent>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .audit_events
            .iter()
            .filter(|event| event.session_id == Some(session_id))
            .cloned()
            .collect())
    }
}

#[async_trait]
impl swage_storage::store::AgentMemoryStore for MockStorage {
    async fn create_agent_memory(
        &self,
        record: swage_storage::NewAgentMemoryRecord,
    ) -> Result<swage_storage::AgentMemoryRecord, StorageError> {
        let record = swage_storage::AgentMemoryRecord {
            id: record.id,
            space_id: record.space_id,
            session_id: record.session_id,
            turn_id: record.turn_id,
            agent_id: record.agent_id,
            memory_kind: record.memory_kind,
            title: record.title,
            content: record.content,
            tags: record.tags,
            outcome_status: record.outcome_status,
            source_summary: record.source_summary,
            metadata: record.metadata,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.agent_memories.push(record.clone());
        Ok(record)
    }

    async fn get_agent_memory(
        &self,
        id: Uuid,
    ) -> Result<Option<swage_storage::AgentMemoryRecord>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .agent_memories
            .iter()
            .find(|record| record.id == id)
            .cloned())
    }

    async fn list_agent_memories(
        &self,
        space_id: Uuid,
        agent_id: Option<&str>,
        session_id: Option<Uuid>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<swage_storage::AgentMemoryRecord>, StorageError> {
        let query = query.map(|value| value.to_lowercase());
        let mut records = self
            .db
            .lock()
            .await
            .agent_memories
            .iter()
            .filter(|record| record.space_id == space_id)
            .filter(|record| agent_id.is_none_or(|id| record.agent_id == id))
            .filter(|record| session_id.is_none_or(|id| record.session_id == id))
            .filter(|record| {
                query.as_ref().is_none_or(|needle| {
                    record.title.to_lowercase().contains(needle)
                        || record.content.to_lowercase().contains(needle)
                        || record.source_summary.to_lowercase().contains(needle)
                        || record
                            .tags
                            .iter()
                            .any(|tag| tag.to_lowercase().contains(needle))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
        records.truncate(limit);
        Ok(records)
    }
}

#[async_trait]
impl swage_storage::store::AgentDefinitionStore for MockStorage {
    async fn upsert_agent_definition(
        &self,
        _: swage_storage::NewAgentDefinition,
    ) -> Result<swage_storage::AgentDefinition, StorageError> {
        unimplemented!("turn tests do not author agent definitions")
    }

    async fn resolve_agent_definition(
        &self,
        _: &str,
        _: uuid::Uuid,
    ) -> Result<Option<swage_storage::AgentDefinition>, StorageError> {
        // Sessions without definitions are the ordinary mock case.
        Ok(None)
    }

    async fn list_agent_definitions(
        &self,
        _: Option<uuid::Uuid>,
    ) -> Result<Vec<swage_storage::AgentDefinition>, StorageError> {
        Ok(vec![])
    }

    async fn delete_agent_definition(
        &self,
        _: &str,
        _: Option<uuid::Uuid>,
    ) -> Result<bool, StorageError> {
        unimplemented!("turn tests do not author agent definitions")
    }
}

#[async_trait]
impl swage_storage::TaskDefinitionStore for MockStorage {
    async fn upsert_task_definition(
        &self,
        _: swage_storage::NewTaskDefinition,
    ) -> Result<swage_storage::TaskDefinition, StorageError> {
        unimplemented!("turn tests use SQLite for task definitions")
    }
    async fn get_task_definition(
        &self,
        _: &str,
    ) -> Result<Option<swage_storage::TaskDefinition>, StorageError> {
        Ok(None)
    }
    async fn list_task_definitions(
        &self,
    ) -> Result<Vec<swage_storage::TaskDefinition>, StorageError> {
        Ok(vec![])
    }
    async fn delete_task_definition(&self, _: &str) -> Result<bool, StorageError> {
        unimplemented!("turn tests use SQLite for task definitions")
    }
}

#[async_trait]
impl swage_storage::store::ObservationStore for MockStorage {
    async fn record_observation(
        &self,
        _: swage_storage::NewSourceObservation,
    ) -> Result<swage_storage::SourceObservation, StorageError> {
        unimplemented!("turn tests do not exercise the knowledge base")
    }

    async fn get_observation(
        &self,
        _: uuid::Uuid,
    ) -> Result<Option<swage_storage::SourceObservation>, StorageError> {
        unimplemented!("turn tests do not exercise the knowledge base")
    }

    async fn list_observations_by_space(
        &self,
        _: uuid::Uuid,
    ) -> Result<Vec<swage_storage::SourceObservation>, StorageError> {
        unimplemented!("turn tests do not exercise the knowledge base")
    }

    async fn add_extraction(
        &self,
        _: uuid::Uuid,
        _: String,
        _: String,
    ) -> Result<swage_storage::ExtractionDerivation, StorageError> {
        unimplemented!("turn tests do not exercise the knowledge base")
    }

    async fn get_extraction(
        &self,
        _: uuid::Uuid,
        _: u32,
    ) -> Result<Option<swage_storage::ExtractionDerivation>, StorageError> {
        unimplemented!("turn tests do not exercise the knowledge base")
    }

    async fn latest_extraction(
        &self,
        _: uuid::Uuid,
    ) -> Result<Option<swage_storage::ExtractionDerivation>, StorageError> {
        unimplemented!("turn tests do not exercise the knowledge base")
    }

    async fn search_extractions(
        &self,
        _: uuid::Uuid,
        _: &str,
        _: usize,
    ) -> Result<Vec<swage_storage::KnowledgeSearchHit>, StorageError> {
        unimplemented!("turn tests do not exercise the knowledge base")
    }
}

#[async_trait]
impl swage_storage::store::CurationAssertionStore for MockStorage {
    async fn add_curation_assertion(
        &self,
        _: swage_storage::NewCurationAssertion,
    ) -> Result<swage_storage::CurationAssertion, StorageError> {
        unimplemented!("turn tests do not exercise knowledge curation")
    }

    async fn effective_curation_by_space(
        &self,
        _: uuid::Uuid,
    ) -> Result<Vec<swage_storage::CurationAssertion>, StorageError> {
        unimplemented!("turn tests do not exercise knowledge curation")
    }
}

#[async_trait]
impl swage_storage::store::CitationStore for MockStorage {
    async fn add_citation(
        &self,
        _: swage_storage::NewKnowledgeCitation,
    ) -> Result<swage_storage::KnowledgeCitation, StorageError> {
        unimplemented!("turn tests do not exercise knowledge citations")
    }

    async fn get_citation(
        &self,
        _: uuid::Uuid,
    ) -> Result<Option<swage_storage::KnowledgeCitation>, StorageError> {
        unimplemented!("turn tests do not exercise knowledge citations")
    }
}

#[async_trait]
impl swage_storage::store::OrientationCardStore for MockStorage {
    async fn create_orientation_card(
        &self,
        _: swage_storage::NewOrientationCard,
        _: Option<uuid::Uuid>,
    ) -> Result<swage_storage::OrientationCard, StorageError> {
        unimplemented!("turn tests do not exercise orientation cards")
    }
    async fn find_orientation_cards(
        &self,
        _: uuid::Uuid,
        _: Option<&str>,
        _: usize,
    ) -> Result<Vec<swage_storage::OrientationCard>, StorageError> {
        unimplemented!("turn tests do not exercise orientation cards")
    }
    async fn supersede_orientation_card(
        &self,
        _: uuid::Uuid,
        _: uuid::Uuid,
        _: uuid::Uuid,
        _: String,
        _: Option<uuid::Uuid>,
    ) -> Result<swage_storage::OrientationCardLifecycle, StorageError> {
        unimplemented!("turn tests do not exercise orientation cards")
    }
    async fn retire_orientation_card(
        &self,
        _: uuid::Uuid,
        _: uuid::Uuid,
        _: String,
        _: Option<uuid::Uuid>,
    ) -> Result<swage_storage::OrientationCardLifecycle, StorageError> {
        unimplemented!("turn tests do not exercise orientation cards")
    }
}

#[async_trait]
impl swage_storage::store::SecretStore for MockStorage {
    async fn create_secret(
        &self,
        secret: swage_storage::NewSecretRecord,
    ) -> Result<swage_storage::SecretRecord, StorageError> {
        let record = swage_storage::SecretRecord {
            id: secret.id,
            name: secret.name,
            material_kind: secret.material_kind,
            owner_kind: secret.owner_kind,
            backend_kind: secret.backend_kind.clone(),
            scope_kind: secret.scope_kind,
            scope_id: secret.scope_id,
            reference: format!("secret://{}/{}", secret.backend_kind, secret.id),
            external_reference: secret.external_reference,
            metadata: secret.metadata,
            status: "active".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            revoked_at: None,
        };
        let mut db = self.db.lock().await;
        if let Some(value) = secret.value {
            db.managed_secret_values.insert(record.id, value);
        }
        db.secrets.insert(record.id, record.clone());
        Ok(record)
    }

    async fn list_secrets(&self) -> Result<Vec<swage_storage::SecretRecord>, StorageError> {
        if self.fail_secret_reads.load(Ordering::SeqCst) {
            return Err(StorageError::ConnectionFailed(
                "injected secret storage failure".into(),
            ));
        }
        Ok(self.db.lock().await.secrets.values().cloned().collect())
    }

    async fn get_secret(
        &self,
        id: Uuid,
    ) -> Result<Option<swage_storage::SecretRecord>, StorageError> {
        Ok(self.db.lock().await.secrets.get(&id).cloned())
    }

    async fn update_secret_value(
        &self,
        id: Uuid,
        value: Option<String>,
        external_reference: Option<String>,
        metadata: serde_json::Value,
    ) -> Result<swage_storage::SecretRecord, StorageError> {
        let mut db = self.db.lock().await;
        if let Some(value) = value {
            db.managed_secret_values.insert(id, value);
        }
        let record = db
            .secrets
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            })?;
        record.external_reference = external_reference;
        record.metadata = metadata;
        record.updated_at = chrono::Utc::now();
        Ok(record.clone())
    }

    async fn revoke_secret(&self, id: Uuid) -> Result<swage_storage::SecretRecord, StorageError> {
        let mut db = self.db.lock().await;
        let record = db
            .secrets
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            })?;
        record.status = "revoked".into();
        record.updated_at = chrono::Utc::now();
        record.revoked_at = Some(chrono::Utc::now());
        Ok(record.clone())
    }

    async fn restore_secret(&self, id: Uuid) -> Result<swage_storage::SecretRecord, StorageError> {
        let mut db = self.db.lock().await;
        let record = db
            .secrets
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            })?;
        record.status = "active".into();
        record.updated_at = chrono::Utc::now();
        record.revoked_at = None;
        Ok(record.clone())
    }

    async fn get_managed_secret_value(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        if self.fail_secret_reads.load(Ordering::SeqCst) {
            return Err(StorageError::ConnectionFailed(
                "injected managed-secret storage failure".into(),
            ));
        }
        Ok(self.db.lock().await.managed_secret_values.get(&id).cloned())
    }
}

#[async_trait]
impl swage_storage::store::AgentMessageStore for MockStorage {
    async fn create_agent_message(
        &self,
        msg: NewAgentMessage,
    ) -> Result<AgentMessage, StorageError> {
        msg.validate()?;

        let now = chrono::Utc::now();
        let record = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: msg.sender_agent_id,
            recipient_agent_id: msg.recipient_agent_id,
            kind: msg.kind,
            correlation_id: msg.correlation_id,
            payload: msg.payload,
            status: swage_core::MessageStatus::Pending,
            failure_reason: None,
            created_at: now,
            delivered_at: None,
        };
        self.db
            .lock()
            .await
            .agent_messages
            .insert(record.id, record.clone());
        Ok(record)
    }
    async fn get_agent_message(&self, id: Uuid) -> Result<Option<AgentMessage>, StorageError> {
        Ok(self.db.lock().await.agent_messages.get(&id).cloned())
    }
    async fn list_pending_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let mut messages: Vec<_> = self
            .db
            .lock()
            .await
            .agent_messages
            .values()
            .filter(|message| {
                message.recipient_agent_id == agent_id
                    && message.status == swage_core::MessageStatus::Pending
            })
            .cloned()
            .collect();
        messages.sort_by_key(|message| message.created_at);
        Ok(messages)
    }
    async fn update_agent_message_status(
        &self,
        id: Uuid,
        status: swage_core::MessageStatus,
        failure_reason: Option<&str>,
    ) -> Result<bool, StorageError> {
        let mut db = self.db.lock().await;
        let Some(message) = db.agent_messages.get_mut(&id) else {
            return Ok(false);
        };
        if message.status != swage_core::MessageStatus::Pending {
            return Ok(false);
        }

        message.status = status.clone();
        message.failure_reason = failure_reason.map(str::to_string);
        message.delivered_at = if status == swage_core::MessageStatus::Delivered {
            Some(chrono::Utc::now())
        } else {
            None
        };
        Ok(true)
    }
    async fn update_agent_message_payload(
        &self,
        id: Uuid,
        payload: serde_json::Value,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        let message = db
            .agent_messages
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "agent_message".into(),
                id: id.to_string(),
            })?;
        message.payload = payload;
        Ok(())
    }
    async fn list_by_correlation(
        &self,
        correlation_id: Uuid,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let mut messages: Vec<_> = self
            .db
            .lock()
            .await
            .agent_messages
            .values()
            .filter(|message| message.correlation_id == Some(correlation_id))
            .cloned()
            .collect();
        messages.sort_by_key(|message| message.created_at);
        Ok(messages)
    }
    async fn expire_stale_messages(
        &self,
        max_age: std::time::Duration,
    ) -> Result<u64, StorageError> {
        let now = chrono::Utc::now();
        let mut db = self.db.lock().await;
        let mut expired = 0;
        for message in db.agent_messages.values_mut() {
            if message.status != swage_core::MessageStatus::Pending {
                continue;
            }
            let Ok(age) = now.signed_duration_since(message.created_at).to_std() else {
                continue;
            };
            if age > max_age {
                message.status = swage_core::MessageStatus::Failed;
                message.failure_reason = Some("delivery timeout".into());
                message.delivered_at = None;
                expired += 1;
            }
        }
        Ok(expired)
    }
}

// -----------------------------------------------------------------------
// Test helpers
// -----------------------------------------------------------------------

/// Build an AppState with a pre-populated session and its default thread.
///
/// Returns the state and the session_id. The thread is created automatically
/// so that `process_turn` can look it up via `list_by_session`.
pub(crate) async fn make_state_with_session(provider_chunks: Vec<StreamChunk>) -> (AppState, Uuid) {
    let storage = Arc::new(MockStorage::new());
    make_state_with_storage(provider_chunks, storage).await
}

#[tokio::test(flavor = "multi_thread")]
async fn session_config_uses_root_retained_at_state_construction() {
    let constructed_root = tempfile::tempdir().expect("constructed config root");
    let redirected_root = tempfile::tempdir().expect("redirected config root");
    for (root, ttl) in [(&constructed_root, 111_u64), (&redirected_root, 222_u64)] {
        let space_dir = root.path().join("spaces/default");
        std::fs::create_dir_all(&space_dir).expect("create space config directory");
        std::fs::write(
            space_dir.join("config.toml"),
            format!("[communication]\nmessage_ttl_secs = {ttl}\n"),
        )
        .expect("write discriminating session config");
    }

    let storage = Arc::new(MockStorage::new());
    let state = make_state_with_storage_and_root(
        vec![],
        storage,
        Some(constructed_root.path().to_path_buf()),
    )
    .await
    .0;
    assert_eq!(state.config_root, constructed_root.path());
    let _redirect = SwageHomeGuard::set_to(redirected_root.path());

    let loaded =
        load_session_config(&state, "default", "gateway").expect("load config from retained root");
    assert_eq!(loaded.communication.message_ttl_secs, 111);
}

#[tokio::test(flavor = "multi_thread")]
async fn active_skill_loading_keeps_agent_skills_out_of_reserved_workflow_contexts() {
    let root = SwageHomeGuard::new();
    let normal_skill = root
        .path()
        .join("spaces/default/agents/gateway/skills/normal-agent-skill/SKILL.md");
    let reserved_skill = root
        .path()
        .join("spaces/default/agents")
        .join(swage_config::RESERVED_WORKFLOW_AGENT_ID)
        .join("skills/reserved-agent-skill/SKILL.md");
    for (path, instructions) in [
        (&normal_skill, "Normal agent skill instructions."),
        (
            &reserved_skill,
            "Reserved worker instructions must not load.",
        ),
    ] {
        std::fs::create_dir_all(path.parent().expect("skill directory"))
            .expect("create skill directory");
        std::fs::write(
            path,
            format!("---\ndescription: Test skill\n---\n{instructions}\n"),
        )
        .expect("write skill fixture");
    }

    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create normal-agent session");
    let session_id = session.id;
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
        root.path().to_path_buf(),
    );
    let (available, warnings) = crate::skill::SkillManager::discover_available(
        root.path(),
        &root.path().join("spaces/default"),
        Some(&root.path().join("spaces/default/agents/gateway")),
    );
    assert!(
        warnings.is_empty(),
        "fixture skills must parse: {warnings:?}"
    );
    assert!(
        available
            .iter()
            .any(|skill| skill.name == "normal-agent-skill"),
        "normal fixture skill must be discoverable: {available:?}"
    );
    let normal = load_active_skills_for_agent(
        &state,
        session_id,
        "default",
        "gateway",
        Some(root.path()),
        true,
        &[],
        &[],
    )
    .await
    .expect("load normal agent skills");
    assert_eq!(
        normal
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["normal-agent-skill"],
        "a normal agent must asynchronously load and activate its agent-level skill"
    );

    let reserved = load_active_skills_for_agent(
        &state,
        session_id,
        "default",
        swage_config::RESERVED_WORKFLOW_AGENT_ID,
        Some(root.path()),
        false,
        &[],
        &[],
    )
    .await
    .expect("load reserved workflow agent skills");
    assert!(
        reserved.is_empty(),
        "the reserved workflow agent must not discover either agent-directory skill: {reserved:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn process_turn_loads_agent_skills_and_excludes_reserved_workflow_skills() {
    let root = SwageHomeGuard::new();
    let normal_skill = root
        .path()
        .join("spaces/default/agents/gateway/skills/normal-agent-skill/SKILL.md");
    let reserved_skill = root
        .path()
        .join("spaces/default/agents")
        .join(swage_config::RESERVED_WORKFLOW_AGENT_ID)
        .join("skills/reserved-agent-skill/SKILL.md");
    for (path, instructions) in [
        (&normal_skill, "Normal agent skill instructions."),
        (
            &reserved_skill,
            "Reserved worker instructions must not reach a provider.",
        ),
    ] {
        std::fs::create_dir_all(path.parent().expect("skill directory"))
            .expect("create skill directory");
        std::fs::write(
            path,
            format!("---\ndescription: Test skill\n---\n{instructions}\n"),
        )
        .expect("write skill fixture");
    }

    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let mut session_ids = Vec::new();
    for agent_id in ["gateway", swage_config::RESERVED_WORKFLOW_AGENT_ID] {
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: agent_id.to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("create skill fixture session");
        ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: agent_id.to_string(),
            },
        )
        .await
        .expect("create skill fixture thread");
        session_ids.push(session.id);
    }
    let (provider, captured_requests) = CapturingMultiRoundProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "normal reply".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "reserved reply".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
        root.path().to_path_buf(),
    );
    for session_id in session_ids {
        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(
            &state,
            session_id,
            None,
            TurnInput::from("exercise agent skill loading"),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");
    }

    let requests = captured_requests.lock().expect("captured requests");
    assert_eq!(
        requests.len(),
        2,
        "one provider request per foreground turn"
    );
    let request_text = |request: &CompletionRequest| {
        request
            .messages
            .iter()
            .map(ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        request_text(&requests[0]).contains("Normal agent skill instructions."),
        "process_turn must load the normal agent-level skill before calling the provider"
    );
    assert!(
        !request_text(&requests[1])
            .contains("Reserved worker instructions must not reach a provider."),
        "process_turn must exclude reserved workflow-agent directory skills from the provider context"
    );
}

pub(crate) async fn make_state_with_storage(
    provider_chunks: Vec<StreamChunk>,
    storage: Arc<MockStorage>,
) -> (AppState, Uuid) {
    make_state_with_storage_and_root(provider_chunks, storage, None).await
}

async fn make_state_with_storage_and_root(
    provider_chunks: Vec<StreamChunk>,
    storage: Arc<MockStorage>,
    config_root: Option<std::path::PathBuf>,
) -> (AppState, Uuid) {
    // Create the default space via SpaceStore.
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");

    // Create a session.
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    // Create the default thread for this session.
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let provider = Arc::new(MockProvider::new(provider_chunks));
    let storage_backend = Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>;
    let space_store = Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>;
    let provider = provider as Arc<dyn LlmProvider>;
    let config = swage_config::ResolvedConfig::default();
    let tool_dispatcher = crate::tool::ToolDispatcher::allow_all(vec![]);
    let state = match config_root {
        Some(config_root) => AppState::new_with_config_root(
            storage_backend,
            space_store,
            provider,
            config,
            tool_dispatcher,
            None,
            config_root,
        ),
        None => AppState::new(
            storage_backend,
            space_store,
            provider,
            config,
            tool_dispatcher,
            None,
        ),
    };
    (state, session_id)
}

/// Like `make_state_with_session` but accepts an explicit `ResolvedConfig`.
///
/// Used for tests that need non-default config (e.g. output schema configured).
pub(crate) async fn make_state_with_config(
    provider_chunks: Vec<StreamChunk>,
    config: swage_config::ResolvedConfig,
) -> (AppState, Uuid) {
    let storage = Arc::new(MockStorage::new());
    make_state_with_config_and_storage(provider_chunks, config, storage).await
}

async fn make_state_with_config_and_storage(
    provider_chunks: Vec<StreamChunk>,
    config: swage_config::ResolvedConfig,
    storage: Arc<MockStorage>,
) -> (AppState, Uuid) {
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");

    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let provider = Arc::new(MockProvider::new(provider_chunks));
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        provider as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (state, session_id)
}

fn codex_turn_config() -> swage_config::ResolvedConfig {
    let mut config = swage_config::ResolvedConfig::default();
    config.providers.entries.insert(
        "codex".into(),
        swage_config::ProviderEndpointConfig {
            protocol: swage_config::Protocol::Codex,
            ..Default::default()
        },
    );
    config.models.default = "codex".into();
    config.models.entries.insert(
        "codex".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "codex".into(),
            model_id: "fake-codex".into(),
            ..Default::default()
        },
    );
    config
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn fresh_codex_turn_completes_without_degraded_execution_opt_in_or_audit() {
    let _guard = SwageHomeGuard::isolate();
    let config = codex_turn_config();
    assert!(!config.sandbox.allow_degraded_execution);
    let (mut state, session_id) = make_state_with_config(Vec::new(), config).await;

    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let client = swage_codex::AppServerClient::new(
        client_write,
        client_read,
        std::time::Duration::from_secs(2),
    );
    state.codex_supervisor = swage_codex::AppServerSupervisor::with_client(
        swage_codex::AppServerConfig::codex("/tmp/swage-codex-turn-scenario".into()),
        client,
    );
    let scenario = serde_json::json!({"keepOpen":true,"steps":[
        {
            "receive":{"method":"thread/start"},
            "send":[{"jsonrpc":"2.0","id":"$request_id","result":{"thread":{"id":"fake-thread"}}}]
        },
        {
            "receive":{"method":"turn/start","params":{"sandboxPolicy":{"type":"workspaceWrite","networkAccess":false,"excludeSlashTmp":true,"excludeTmpdirEnvVar":true}}},
            "send":[
                {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
                {"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"delta":"done"}},
                {"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"fake-turn","status":"completed","error":null}}}
            ]
        }
    ]});
    let server = tokio::spawn(async move {
        swage_codex::fake::run_scenario(
            &scenario,
            tokio::io::BufReader::new(server_read),
            server_write,
        )
        .await
    });
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("Hello Codex"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("a fresh Codex turn should complete under App Server sandbox policy");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::TurnComplete { .. })),
        "expected a completed Codex turn, got {events:?}"
    );
    let audit_events = state.storage.list_audit_events().await.unwrap();
    assert!(
        audit_events
            .iter()
            .all(|event| { event.metadata["reason"] != "sandbox_disabled_by_configuration" }),
        "App Server policy must not be recorded as degraded execution: {audit_events:?}"
    );
    server.abort();
    assert!(
        server
            .await
            .expect_err("fake App Server should be stopped after the turn")
            .is_cancelled()
    );
}

#[tokio::test]
async fn full_turn_stores_messages_and_completes() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Delta {
            content: ", world".to_string(),
        },
        StreamChunk::Delta {
            content: "!".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    let (turn_result, memory_loads) = Box::pin(observe_space_memory_loads(process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )))
    .await;
    turn_result.expect("turn should complete without error");
    assert_eq!(
        memory_loads, 0,
        "a full turn using the default assembly must not reach the space-memory loading seam"
    );

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    let token_contents: Vec<String> = events
        .iter()
        .filter_map(|e| {
            if let ServerEvent::Token { content } = e {
                Some(content.clone())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(token_contents, ["Hello", ", world", "!"]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );

    // Get thread_id and verify messages stored in the DB.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;

    let messages = state
        .storage
        .list_messages_by_thread(thread_id)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2, "user message + assistant message");
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[0].content, "Hi");
    assert_eq!(messages[1].role, Role::Assistant);
    assert_eq!(messages[1].content, "Hello, world!");

    // Verify the turn was marked Completed.
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].state, TurnState::Completed);
}

#[tokio::test]
async fn cancelled_turn_stores_partial_and_marks_interrupted() {
    let storage = Arc::new(MockStorage::new());

    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");

    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let state = Arc::new(AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(PendingProvider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    ));

    let (tx, mut rx) = mpsc::unbounded_channel();

    let state_clone = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        process_turn(
            &state_clone,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    loop {
        match rx.recv().await.expect("partial token should be emitted") {
            ServerEvent::Token { content } => {
                assert_eq!(content, INTERRUPTED_PARTIAL);
                break;
            }
            _ => continue,
        }
    }

    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed");

    handle
        .await
        .expect("task should not panic")
        .expect("process_turn ok");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Cancelled {})),
        "expected Cancelled event"
    );

    // Verify turn is Interrupted in storage.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Interrupted);
    assert_eq!(
        turns[0].assistant_response.as_deref(),
        Some(INTERRUPTED_PARTIAL)
    );

    let conversation = crate::service::ConversationService::new(
        Arc::clone(&state.storage),
        Arc::clone(&state.space_store),
    )
    .get_conversation(session_id)
    .await
    .expect("reload-shaped conversation read should succeed");
    assert_eq!(conversation.messages.len(), 2, "user and partial assistant");
    assert_eq!(conversation.messages[1].role, "assistant");
    assert_eq!(conversation.messages[1].content, INTERRUPTED_PARTIAL);
    assert_eq!(
        conversation.messages[1].turn_state,
        Some(swage_protocol::client::conversation::TurnState::Interrupted)
    );
}

#[tokio::test]
async fn cancelled_turn_without_partial_stores_no_assistant_response() {
    let storage = Arc::new(MockStorage::new());
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let stream_polled = Arc::new(Notify::new());
    let state = Arc::new(AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(EmptyPendingProvider {
            stream_polled: Arc::clone(&stream_polled),
        }) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let state_clone = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        process_turn(
            &state_clone,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    stream_polled.notified().await;
    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed");
    handle
        .await
        .expect("task should not panic")
        .expect("process_turn ok");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    assert_eq!(turns[0].state, TurnState::Interrupted);
    assert_eq!(turns[0].assistant_response, None);

    let conversation = crate::service::ConversationService::new(
        Arc::clone(&state.storage),
        Arc::clone(&state.space_store),
    )
    .get_conversation(session_id)
    .await
    .expect("reload-shaped conversation read should succeed");
    assert_eq!(conversation.messages.len(), 1, "only the user message");
    assert_eq!(conversation.messages[0].role, "user");
}

const COMPLETED_ROUND_PARTIAL: &str = "Text shown before the tool calls.";
const LATER_ROUND_PARTIAL: &str = " Text shown during the next response.";

#[tokio::test]
async fn cancelled_before_next_tool_persists_completed_round_text() {
    let rounds = vec![vec![
        StreamChunk::Delta {
            content: COMPLETED_ROUND_PARTIAL.into(),
        },
        StreamChunk::ToolUse {
            call_id: "first".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::ToolUse {
            call_id: "second".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "second" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]];
    let (mut state, session_id) = make_state_with_tools(rounds).await;
    let hook_reached = Arc::new(Notify::new());
    let hook_release = Arc::new(Notify::new());
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(PausingPostToolHook {
        reached: Arc::clone(&hook_reached),
        release: Arc::clone(&hook_release),
    })));
    state.hook_registry = Some(Arc::new(hook_registry));
    let state = Arc::new(state);

    let (tx, _rx) = mpsc::unbounded_channel();
    let state_for_turn = Arc::clone(&state);
    let turn_task = tokio::spawn(async move {
        process_turn(
            &state_for_turn,
            session_id,
            None,
            "Use both tools".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    hook_reached.notified().await;
    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed while the first tool's post-hook is open");
    hook_release.notify_one();
    turn_task
        .await
        .expect("turn task should not panic")
        .expect("cancelled turn should settle cleanly");

    let conversation = crate::service::ConversationService::new(
        Arc::clone(&state.storage),
        Arc::clone(&state.space_store),
    )
    .get_conversation(session_id)
    .await
    .expect("reload-shaped conversation read should succeed");
    assert_eq!(conversation.messages.len(), 2, "user and partial assistant");
    assert_eq!(conversation.messages[1].role, "assistant");
    assert_eq!(conversation.messages[1].content, COMPLETED_ROUND_PARTIAL);
    assert_eq!(
        conversation.messages[1].turn_state,
        Some(swage_protocol::client::conversation::TurnState::Interrupted)
    );
}

#[tokio::test]
async fn cancelled_before_next_round_persists_completed_round_text() {
    let rounds = vec![vec![
        StreamChunk::Delta {
            content: COMPLETED_ROUND_PARTIAL.into(),
        },
        StreamChunk::ToolUse {
            call_id: "first".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]];
    let (mut state, session_id) = make_state_with_tools(rounds).await;
    let hook_reached = Arc::new(Notify::new());
    let hook_release = Arc::new(Notify::new());
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(PausingPostToolHook {
        reached: Arc::clone(&hook_reached),
        release: Arc::clone(&hook_release),
    })));
    state.hook_registry = Some(Arc::new(hook_registry));
    let state = Arc::new(state);

    let (tx, _rx) = mpsc::unbounded_channel();
    let state_for_turn = Arc::clone(&state);
    let turn_task = tokio::spawn(async move {
        process_turn(
            &state_for_turn,
            session_id,
            None,
            "Use one tool".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    hook_reached.notified().await;
    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed after the first round's tool completes");
    hook_release.notify_one();
    turn_task
        .await
        .expect("turn task should not panic")
        .expect("cancelled turn should settle cleanly");

    let conversation = crate::service::ConversationService::new(
        Arc::clone(&state.storage),
        Arc::clone(&state.space_store),
    )
    .get_conversation(session_id)
    .await
    .expect("reload-shaped conversation read should succeed");
    assert_eq!(conversation.messages.len(), 2, "user and partial assistant");
    assert_eq!(conversation.messages[1].role, "assistant");
    assert_eq!(conversation.messages[1].content, COMPLETED_ROUND_PARTIAL);
    assert_eq!(
        conversation.messages[1].turn_state,
        Some(swage_protocol::client::conversation::TurnState::Interrupted)
    );
}

#[tokio::test]
async fn cancelled_during_later_stream_persists_all_accumulated_text() {
    let provider = Arc::new(PendingFinalRoundProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: COMPLETED_ROUND_PARTIAL.into(),
            },
            StreamChunk::ToolUse {
                call_id: "first".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "first" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![StreamChunk::Delta {
            content: LATER_ROUND_PARTIAL.into(),
        }],
    ]));
    let (state, session_id) = make_state_with_tool_provider(provider).await;
    let state = Arc::new(state);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let state_for_turn = Arc::clone(&state);
    let turn_task = tokio::spawn(async move {
        process_turn(
            &state_for_turn,
            session_id,
            None,
            "Use one tool, then continue".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    loop {
        if matches!(
            rx.recv().await.expect("later-round token should be emitted"),
            ServerEvent::Token { content } if content == LATER_ROUND_PARTIAL
        ) {
            break;
        }
    }
    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed while the later provider stream is open");
    turn_task
        .await
        .expect("turn task should not panic")
        .expect("cancelled turn should settle cleanly");

    let expected = format!("{COMPLETED_ROUND_PARTIAL}{LATER_ROUND_PARTIAL}");
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    assert_eq!(turns[0].state, TurnState::Interrupted);
    assert_eq!(
        turns[0].assistant_response.as_deref(),
        Some(expected.as_str())
    );

    let conversation = crate::service::ConversationService::new(
        Arc::clone(&state.storage),
        Arc::clone(&state.space_store),
    )
    .get_conversation(session_id)
    .await
    .expect("reload-shaped conversation read should succeed");
    assert_eq!(conversation.messages.len(), 2, "user and partial assistant");
    assert_eq!(conversation.messages[1].role, "assistant");
    assert_eq!(conversation.messages[1].content, expected);
    assert_eq!(
        conversation.messages[1].turn_state,
        Some(swage_protocol::client::conversation::TurnState::Interrupted)
    );
    let thread = ThreadStore::get(&*state.storage, threads[0].id)
        .await
        .expect("thread lookup should succeed")
        .expect("thread should exist");
    assert_eq!(thread.state, ThreadState::Idle);
}

#[tokio::test]
async fn cancelled_hook_approval_persists_completed_round_text() {
    let rounds = vec![vec![
        StreamChunk::Delta {
            content: COMPLETED_ROUND_PARTIAL.into(),
        },
        StreamChunk::ToolUse {
            call_id: "approval".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "gated" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]];
    let (mut state, session_id) = make_state_with_tools(rounds).await;
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(ApprovePreToolHook)));
    state.hook_registry = Some(Arc::new(hook_registry));
    let state = Arc::new(state);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let state_for_turn = Arc::clone(&state);
    let turn_task = tokio::spawn(async move {
        process_turn(
            &state_for_turn,
            session_id,
            None,
            "Use the gated tool".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    loop {
        if matches!(
            rx.recv().await.expect("approval event should be emitted"),
            ServerEvent::ApprovalRequired { .. }
        ) {
            break;
        }
    }
    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed while hook approval is open");
    turn_task
        .await
        .expect("turn task should not panic")
        .expect("cancelled turn should settle cleanly");

    let conversation = crate::service::ConversationService::new(
        Arc::clone(&state.storage),
        Arc::clone(&state.space_store),
    )
    .get_conversation(session_id)
    .await
    .expect("reload-shaped conversation read should succeed");
    assert_eq!(conversation.messages.len(), 2, "user and partial assistant");
    assert_eq!(conversation.messages[1].role, "assistant");
    assert_eq!(conversation.messages[1].content, COMPLETED_ROUND_PARTIAL);
    assert_eq!(
        conversation.messages[1].turn_state,
        Some(swage_protocol::client::conversation::TurnState::Interrupted)
    );
}

#[tokio::test]
async fn provider_error_marks_turn_failed() {
    let (state, session_id) = make_state_with_session(vec![StreamChunk::Error {
        message: "provider overloaded".to_string(),
    }])
    .await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("error is surfaced via event, not Err return");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    // Agent delegates error categorisation — provider errors surface as agent_error.
    assert!(events.iter().any(|e| matches!(
        e,
        ServerEvent::Error { code, .. } if code == "agent_error"
    )));

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
}

#[tokio::test]
async fn concurrent_turn_rejected_with_turn_in_progress() {
    // Manually create a Processing turn to simulate an active turn.
    let (state, session_id) = make_state_with_session(vec![]).await;

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;

    // Create a turn and advance it to Processing.
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();
    state
        .storage
        .update_turn_state(turn.id, TurnState::Processing)
        .await
        .unwrap();

    let (tx, _rx) = mpsc::unbounded_channel();
    let result = process_turn(
        &state,
        session_id,
        None,
        "Another message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await;

    assert!(
        matches!(result, Err(GatewayError::TurnInProgress { .. })),
        "expected TurnInProgress, got: {result:?}"
    );
}

#[tokio::test]
async fn pending_turn_rejected_with_turn_in_progress() {
    let (state, session_id) = make_state_with_session(vec![]).await;

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;

    state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let (tx, _rx) = mpsc::unbounded_channel();
    let result = process_turn(
        &state,
        session_id,
        None,
        "Another message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await;

    assert!(
        matches!(result, Err(GatewayError::TurnInProgress { .. })),
        "expected TurnInProgress for pending turn, got: {result:?}"
    );
}

#[tokio::test]
async fn blocked_inbound_hook_marks_turn_failed_and_clears_active_turn() {
    let (mut state, session_id) = make_state_with_session(vec![]).await;
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(BlockInboundHook)));
    state.hook_registry = Some(Arc::new(hook_registry));

    let (tx, _rx) = mpsc::unbounded_channel();
    let result = process_turn(
        &state,
        session_id,
        None,
        "blocked message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await;

    assert!(
        matches!(result, Err(GatewayError::ConfigError(ref message)) if message.contains("blocked by test hook")),
        "expected blocked inbound hook error, got: {result:?}"
    );
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "blocked inbound hook must not leave a stale active_turns entry"
    );

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns.len(), 1, "expected placeholder turn to be created");
    assert_eq!(turns[0].state, TurnState::Failed);
}

#[tokio::test]
async fn terminal_state_write_failure_still_clears_active_turn() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_turn_state_writes();
    let (mut state, session_id) = make_state_with_storage(vec![], storage).await;
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(BlockInboundHook)));
    state.hook_registry = Some(Arc::new(hook_registry));

    let (tx, _rx) = mpsc::unbounded_channel();
    let error = process_turn(
        &state,
        session_id,
        None,
        "blocked message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("the injected terminal-state write must fail");

    assert!(
        error
            .to_string()
            .contains("injected turn state write failure")
    );
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "terminal-state persistence failure must release the turn guard"
    );
}

async fn assert_subsequent_turn_is_not_blocked(state: &AppState, session_id: Uuid) {
    let (tx, _rx) = mpsc::unbounded_channel();
    let result = process_turn(
        state,
        session_id,
        None,
        "retry message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await;

    assert!(
        !matches!(result, Err(GatewayError::TurnInProgress { .. })),
        "terminal cleanup must allow a subsequent turn to start, got: {result:?}"
    );
}

#[tokio::test]
async fn inbound_audit_write_failure_clears_active_turn() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_inbound_audit_writes();
    let (mut state, session_id) = make_state_with_storage(vec![], storage).await;
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(ModifyInboundHook)));
    state.hook_registry = Some(Arc::new(hook_registry));

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("the injected inbound-audit write must fail");

    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "inbound-audit persistence failure must release the turn guard"
    );
    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .remove(0);
    let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
    assert_subsequent_turn_is_not_blocked(&state, session_id).await;
}

#[tokio::test]
async fn initial_message_write_failure_clears_active_turn() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_append_message_at(1);
    let (state, session_id) = make_state_with_storage(vec![], storage).await;

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("the injected user-message write must fail");

    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "user-message persistence failure must release the turn guard"
    );
    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .remove(0);
    let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
    assert_subsequent_turn_is_not_blocked(&state, session_id).await;
}

#[tokio::test]
async fn delegated_result_delivery_failure_clears_active_turn() {
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage(vec![], Arc::clone(&storage)).await;
    let result = storage
        .create_agent_message(NewAgentMessage {
            sender_agent_id: "researcher".into(),
            recipient_agent_id: "gateway".into(),
            kind: swage_core::MessageKind::Result,
            correlation_id: Some(Uuid::new_v4()),
            payload: serde_json::json!({"response": "delegated answer"}),
        })
        .await
        .expect("pending delegated result");
    storage.fail_append_message_at(2);

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("the injected delegated-result append must fail");

    let thread = storage.list_by_session(session_id).await.unwrap().remove(0);
    let turns = storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
    assert_eq!(
        storage
            .get_agent_message(result.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        MessageStatus::Pending,
        "a result whose append failed must remain available for redelivery"
    );
    assert_subsequent_turn_is_not_blocked(&state, session_id).await;
}

#[tokio::test]
async fn history_rebuild_failure_clears_active_turn() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_message_lists();
    let (state, session_id) = make_state_with_storage(vec![], storage).await;

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("the injected history read must fail");

    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "history rebuild failure must release the turn guard"
    );
    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .remove(0);
    let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
    assert_subsequent_turn_is_not_blocked(&state, session_id).await;
}

#[tokio::test]
async fn session_variable_read_failure_clears_active_turn() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_session_variable_lists();
    let (state, session_id) = make_state_with_storage(vec![], storage).await;

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("the injected session variable read must fail");

    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "session variable read failure must release the turn guard"
    );
    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .remove(0);
    let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
    assert_subsequent_turn_is_not_blocked(&state, session_id).await;
}

#[tokio::test]
async fn approved_sandbox_fallback_build_failure_clears_active_turn() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    state.fail_next_dispatcher_builds(2);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let approval_channels = Arc::clone(&state.approval_channels);
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        process_turn(
            &state_for_task,
            session_id,
            None,
            "message".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    let approval_id = match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired { approval_id, .. } => approval_id,
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    };
    approval_channels
        .lock()
        .unwrap()
        .remove(&approval_id)
        .expect("approval channel should be registered")
        .send(swage_agent::ApprovalDecision {
            decision: swage_agent::approval::ApprovalDecisionValue::Approved,
            scope: swage_agent::approval::ApprovalDecisionScope::Once,
            reason: None,
        })
        .expect("approval decision should be delivered");

    let error = task
        .await
        .expect("turn task should finish")
        .expect_err("the injected fallback dispatcher build must fail");
    assert!(
        error
            .to_string()
            .contains("injected dispatcher build failure"),
        "unexpected error: {error}"
    );
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "fallback dispatcher failure must release the turn guard"
    );
    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .remove(0);
    let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
}

#[tokio::test]
async fn approved_sandbox_degradation_resumes_to_tool_approval_and_completion() {
    let rounds = vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "fallback-echo".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "continued without isolation" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "The fallback turn completed.".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ];
    let (state, session_id) = make_state_with_approval_gated_echo(rounds).await;
    state.fail_next_dispatcher_builds(1);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        process_turn(
            &task_state,
            session_id,
            None,
            TurnInput::from("Use echo after accepting degraded execution"),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    let mut saw_degradation_approval = false;
    let mut saw_tool_approval = false;
    let mut saw_tool_start = false;
    let mut saw_tool_end = false;
    let mut saw_token = false;
    let mut saw_complete = false;
    while !saw_complete {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("turn should continue emitting events after each approval")
            .expect("turn event channel should stay open until completion");
        match event {
            ServerEvent::ApprovalRequired {
                approval_id,
                kind,
                tool_name,
                ..
            } if kind == "hook" => {
                assert!(
                    !saw_degradation_approval,
                    "the sandbox fallback should require one degradation approval"
                );
                assert_eq!(tool_name, None, "the fallback approval is not a tool call");
                saw_degradation_approval = true;
                state
                    .approval_channels
                    .lock()
                    .unwrap()
                    .remove(&approval_id)
                    .expect("degradation approval channel should be registered")
                    .send(swage_agent::ApprovalDecision {
                        decision: swage_agent::approval::ApprovalDecisionValue::Approved,
                        scope: swage_agent::approval::ApprovalDecisionScope::Once,
                        reason: None,
                    })
                    .expect("degradation approval should resume the turn");
            }
            ServerEvent::ApprovalRequired {
                approval_id,
                kind,
                tool_name,
                ..
            } if kind == "tool" => {
                assert!(
                    saw_degradation_approval,
                    "ordinary tool approval must follow the accepted degradation approval"
                );
                assert_eq!(tool_name.as_deref(), Some("echo"));
                saw_tool_approval = true;
                state
                    .approval_channels
                    .lock()
                    .unwrap()
                    .remove(&approval_id)
                    .expect("tool approval channel should be registered")
                    .send(swage_agent::ApprovalDecision {
                        decision: swage_agent::approval::ApprovalDecisionValue::Approved,
                        scope: swage_agent::approval::ApprovalDecisionScope::Once,
                        reason: None,
                    })
                    .expect("tool approval should resume execution");
            }
            ServerEvent::ToolStart { tool, .. } if tool == "echo" => saw_tool_start = true,
            ServerEvent::ToolEnd { tool, status, .. } if tool == "echo" && status == "success" => {
                saw_tool_end = true
            }
            ServerEvent::Token { content } if content.contains("fallback turn completed") => {
                saw_token = true
            }
            ServerEvent::TurnComplete { .. } => saw_complete = true,
            ServerEvent::Error { message, .. } => panic!("fallback turn must not fail: {message}"),
            _ => {}
        }
    }

    task.await
        .expect("turn task should finish")
        .expect("accepted degradation should continue the turn");
    assert!(
        saw_tool_approval,
        "ordinary tool approval should be rendered"
    );
    assert!(saw_tool_start, "approved tool should execute");
    assert!(saw_tool_end, "tool execution should finish successfully");
    assert!(saw_token, "the final assistant output should stream");

    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .remove(0);
    let turn = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(turn.state, TurnState::Completed);
    assert_eq!(
        turn.assistant_response.as_deref(),
        Some("The fallback turn completed."),
        "the assistant result should persist after the approved fallback"
    );
    let calls = state
        .storage
        .list_tool_calls_by_turn(turn.id)
        .await
        .unwrap();
    assert_eq!(calls.len(), 1, "the approved echo call should persist");
    assert_eq!(calls[0].status.as_deref(), Some("success"));
}

#[tokio::test]
async fn attended_sandbox_failure_records_interactive_attendance() {
    let mut config = swage_config::ResolvedConfig::default();
    config.sandbox.enabled = false;
    let (state, session_id) = make_state_with_config(vec![], config).await;
    state.fail_next_dispatcher_builds(1);

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("disabled sandbox build failure should block the attended turn");

    let audit = state.storage.list_audit_events().await.unwrap();
    assert!(audit.iter().any(|event| {
        event.event_kind == "execution_isolation_degraded"
            && event.metadata["execution_context"] == "interactive turn"
    }));
}

#[tokio::test]
async fn unattended_sandbox_failure_records_unattended_attendance() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    state.fail_next_dispatcher_builds(1);
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn_with_composition(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Unattended,
        DispatchContextComposition::default(),
        None,
    )
    .await
    .expect_err("unattended degradation should fail closed");

    let audit = state.storage.list_audit_events().await.unwrap();
    assert!(audit.iter().any(|event| {
        event.event_kind == "execution_isolation_degraded"
            && event.metadata["execution_context"] == "unattended turn"
    }));
}

#[tokio::test]
async fn unattended_turn_rejects_a_relative_managed_scratch_root() {
    swage_agent::AgentConfig::new("agent")
        .with_workspace("relative", vec![])
        .validate()
        .expect("generic agent validation must leave execution-root policy to the turn owner");

    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage_and_root(
        vec![],
        Arc::clone(&storage),
        Some(std::path::PathBuf::from(".")),
    )
    .await;

    let error = process_turn_with_composition(
        &state,
        session_id,
        None,
        "message".to_string(),
        mpsc::unbounded_channel().0,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Unattended,
        DispatchContextComposition::default(),
        None,
    )
    .await
    .expect_err("an unattended relative scratch root must not inherit the launch directory");

    assert!(
        error
            .to_string()
            .contains("attach a workspace or configure an absolute managed scratch root"),
        "the refusal must explain how to establish a deliberate root: {error}"
    );
    let thread = storage
        .list_by_session(session_id)
        .await
        .expect("thread")
        .remove(0);
    let turns = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turns");
    assert_eq!(turns[0].state, TurnState::Failed);
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "the rejected turn must release its active-turn guard"
    );
}

#[tokio::test]
async fn unattended_turn_accepts_an_active_workspace_when_scratch_root_is_relative() {
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage_and_root(
        vec![
            StreamChunk::Delta {
                content: "configured root".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        Arc::clone(&storage),
        Some(std::path::PathBuf::from(".")),
    )
    .await;
    let workspace = tempfile::tempdir().expect("workspace root");
    let session = storage
        .get_session(session_id)
        .await
        .expect("session lookup")
        .expect("session");
    let attachment = storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id: session.space_id,
            label: "configured workspace".into(),
            path: workspace.path().to_string_lossy().into_owned(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .expect("workspace attachment");
    storage
        .set_session_active_workspace(session_id, Some(attachment.id))
        .await
        .expect("select workspace attachment");

    process_turn_with_composition(
        &state,
        session_id,
        None,
        "message".to_string(),
        mpsc::unbounded_channel().0,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Unattended,
        DispatchContextComposition::default(),
        None,
    )
    .await
    .expect("an active workspace is a deliberate unattended tool root");
}

#[tokio::test]
async fn degradation_audit_and_terminal_state_failure_clears_active_turn() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_audit_writes();
    storage.fail_turn_state_write_at(1);
    let mut config = swage_config::ResolvedConfig::default();
    config.sandbox.enabled = false;
    config.sandbox.allow_degraded_execution = true;
    let (state, session_id) =
        make_state_with_config_and_storage(vec![], config, Arc::clone(&storage)).await;

    let (tx, _rx) = mpsc::unbounded_channel();
    let error = process_turn(
        &state,
        session_id,
        None,
        "message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("the injected degradation terminal-state write must fail");
    assert!(
        error
            .to_string()
            .contains("injected turn state write failure"),
        "unexpected error: {error}"
    );
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "degradation audit failure must release the turn guard"
    );
}

// -----------------------------------------------------------------------
// Mock tool for the execution loop
// -----------------------------------------------------------------------

struct EchoMockTool;

#[async_trait::async_trait]
impl crate::tool::Tool for EchoMockTool {
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

struct ShellMockTool;

#[async_trait::async_trait]
impl crate::tool::Tool for ShellMockTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Test shell"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        Ok(serde_json::json!({ "unexpected": "execution" }))
    }
}

struct FileReadMockTool;

#[async_trait::async_trait]
impl crate::tool::Tool for FileReadMockTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Test file read"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        Ok(serde_json::json!({ "unexpected": "execution" }))
    }
}

struct BlockingMockTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl crate::tool::Tool for BlockingMockTool {
    fn name(&self) -> &str {
        "block"
    }
    fn description(&self) -> &str {
        "Block until the test releases execution"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": true
        })
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        self.started.notify_waiters();
        self.release.notified().await;
        Ok(serde_json::json!({"output": "released"}))
    }
}

/// Build an AppState with a multi-round provider and an EchoMockTool.
async fn make_state_with_tools(rounds: Vec<Vec<StreamChunk>>) -> (AppState, Uuid) {
    make_state_with_tool_provider(Arc::new(MultiRoundMockProvider::new(rounds))).await
}

async fn make_state_with_tool_provider(provider: Arc<dyn LlmProvider>) -> (AppState, Uuid) {
    let storage = Arc::new(MockStorage::new());

    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");

    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        provider,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );
    (state, session_id)
}

pub(crate) async fn make_state_with_capturing_tools(
    rounds: Vec<Vec<StreamChunk>>,
) -> (
    AppState,
    Uuid,
    Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
) {
    let storage = Arc::new(MockStorage::new());

    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");

    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let (provider, captured) = CapturingMultiRoundProvider::new(rounds);

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    (state, session_id, captured)
}

pub(crate) async fn make_state_with_approval_gated_echo(
    rounds: Vec<Vec<StreamChunk>>,
) -> (AppState, Uuid) {
    let (state, session_id, _) = make_state_with_capturing_tools(rounds).await;
    let mut runtime_config = state.active_config();
    runtime_config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "echo".into(),
        )];
    state.replace_runtime(
        runtime_config,
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );
    (state, session_id)
}

// -----------------------------------------------------------------------
// Tool execution loop tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn tool_call_sends_tool_start_and_tool_end_events() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hello" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "The tool said: hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolStart { tool, call_id, .. }
            if tool == "echo" && call_id == "call_1"
        )),
        "expected ToolStart event, got: {events:?}"
    );

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolEnd { tool, call_id, status, .. }
            if tool == "echo" && call_id == "call_1" && status == "success"
        )),
        "expected ToolEnd event, got: {events:?}"
    );

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::Token { content } if content.contains("hello")
        )),
        "expected token with 'hello', got: {events:?}"
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );
}

#[tokio::test]
async fn tool_call_stores_tool_call_record() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "call_store".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "stored" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Store this".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    assert_eq!(tool_calls.len(), 1, "expected 1 tool call record");
    assert_eq!(tool_calls[0].call_id, "call_store");
    assert_eq!(tool_calls[0].tool, "echo");
    assert!(
        tool_calls[0].result.is_some(),
        "tool call should have a result"
    );
    // The agent serialises the full ToolOutput.result JSON as the stored string.
    assert_eq!(
        tool_calls[0].result.as_deref(),
        Some("{\"output\":\"stored\"}")
    );
}

#[tokio::test]
async fn unknown_tool_returns_error_to_model() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "call_unknown".into(),
            name: "nonexistent_tool".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "Sorry, that tool doesn't exist.".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use unknown".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should still complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolEnd { status, .. } if status == "error"
        )),
        "expected ToolEnd with error status, got: {events:?}"
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );
}

#[tokio::test]
async fn cancelled_during_tool_dispatch_persists_partial_without_waiting() {
    let round_1 = vec![
        StreamChunk::Delta {
            content: COMPLETED_ROUND_PARTIAL.into(),
        },
        StreamChunk::ToolUse {
            call_id: "call_block".into(),
            name: "block".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let storage = Arc::new(MockStorage::new());
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(MultiRoundMockProvider::new(vec![round_1])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![Arc::new(BlockingMockTool {
            started: started.clone(),
            release: release.clone(),
        }) as Arc<dyn crate::tool::Tool>]),
        None,
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let state = Arc::new(state);
    let worker_state = state.clone();
    let handle = tokio::spawn(async move {
        process_turn(
            &worker_state,
            session_id,
            None,
            "Run block".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    started.notified().await;
    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed while tool is running");

    handle
        .await
        .expect("task should not panic")
        .expect("process_turn ok");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Cancelled {})),
        "expected Cancelled event, got: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolEnd { tool, call_id, status, .. }
            if tool == "block" && call_id == "call_block" && status == "cancelled"
        )),
        "expected cancelled ToolEnd event, got: {events:?}"
    );

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Interrupted);
    assert_eq!(
        turns[0].assistant_response.as_deref(),
        Some(COMPLETED_ROUND_PARTIAL)
    );

    let conversation = crate::service::ConversationService::new(
        Arc::clone(&state.storage),
        Arc::clone(&state.space_store),
    )
    .get_conversation(session_id)
    .await
    .expect("reload-shaped conversation read should succeed");
    assert_eq!(conversation.messages.len(), 2, "user and partial assistant");
    assert_eq!(conversation.messages[1].role, "assistant");
    assert_eq!(conversation.messages[1].content, COMPLETED_ROUND_PARTIAL);
    assert_eq!(
        conversation.messages[1].turn_state,
        Some(swage_protocol::client::conversation::TurnState::Interrupted)
    );

    let thread = ThreadStore::get(&*state.storage, thread_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(thread.state, ThreadState::Idle);

    release.notify_waiters();
}

#[tokio::test]
async fn multi_round_tool_calling() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::ToolUse {
            call_id: "c2".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "second" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::ToolUse {
            call_id: "c3".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "third" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_3 = vec![
        StreamChunk::Delta {
            content: "All done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2, round_3]).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Multi-round".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    let tool_start_count = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::ToolStart { .. }))
        .count();
    let tool_end_count = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::ToolEnd { .. }))
        .count();
    assert_eq!(tool_start_count, 3, "expected 3 ToolStart events");
    assert_eq!(tool_end_count, 3, "expected 3 ToolEnd events");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    assert_eq!(tool_calls.len(), 3, "expected 3 tool call records");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );
}

#[tokio::test]
async fn second_tool_round_request_keeps_structured_tool_messages() {
    let _swage_home = SwageHomeGuard::isolate();
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");

    let second = &requests[1];
    assert_eq!(second.messages.len(), 5);
    assert_eq!(second.messages[0].role(), Role::System);
    assert_eq!(second.messages[1].role(), Role::System);
    assert_eq!(second.messages[2].role(), Role::User);
    assert_eq!(second.messages[3].role(), Role::Assistant);
    assert_eq!(second.messages[4].role(), Role::User);
    assert!(
        second.messages.iter().any(|message| {
            message.role() == Role::System
                && message
                    .text_content()
                    .contains("You are an agent running on Swage")
        }),
        "second request should retain orientation"
    );
    assert!(
        second.messages.iter().any(|message| {
            message.role() == Role::System && message.text_content().contains("## Working norms")
        }),
        "second request should retain the working norms"
    );
    let assistant_replay = second
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay.iter().any(|part| {
                        matches!(
                            part,
                            ToolReplayPart::ToolCall { call_id, name, arguments }
                                if call_id == "c1" && name == "echo"
                                    && arguments == &serde_json::json!({ "text": "first" })
                        )
                    })
                })
        })
        .expect("assistant tool-use message should retain the structured echo call");
    assert_eq!(
        assistant_replay
            .tool_replay()
            .expect("assistant replay")
            .iter()
            .filter(
                |part| matches!(part, ToolReplayPart::ToolCall { call_id, .. } if call_id == "c1")
            )
            .count(),
        1,
        "the structured tool call must be replayed exactly once"
    );
    let user_replay = second
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::User
                && message.tool_replay().is_some_and(|replay| {
                    replay.iter().any(|part| {
                        matches!(
                            part,
                            ToolReplayPart::ToolResult { call_id, is_error, .. }
                                if call_id == "c1" && !is_error
                        )
                    })
                })
        })
        .expect("user tool-result message should retain the structured echo result");
    assert_eq!(
        user_replay
            .tool_replay()
            .expect("tool-result replay")
            .iter()
            .filter(
                |part| matches!(part, ToolReplayPart::ToolResult { call_id, .. } if call_id == "c1")
            )
            .count(),
        1,
        "the structured tool result must be replayed exactly once"
    );
}

#[tokio::test]
async fn second_tool_round_request_does_not_replay_reasoning_by_default() {
    let round_1 = vec![
        StreamChunk::Reasoning {
            content: "Private provider reasoning".into(),
            details: Some(serde_json::json!({ "source": "test" })),
        },
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    assert!(
        !requests[1].messages.iter().any(|message| {
            message.tool_replay().is_some_and(|replay| {
                replay
                    .iter()
                    .any(|part| matches!(part, swage_core::ToolReplayPart::Reasoning { .. }))
            })
        }),
        "reasoning side-channels must stay out of replay unless the model opts in"
    );
}

#[tokio::test]
async fn second_tool_round_request_replays_reasoning_when_model_capability_opts_in() {
    let round_1 = vec![
        StreamChunk::Reasoning {
            content: "Private provider reasoning".into(),
            details: Some(serde_json::json!({ "source": "test" })),
        },
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "reasoner".to_string();
    config.models.entries = [(
        "reasoner".to_string(),
        swage_config::ModelEndpointConfig {
            provider_id: "mock".to_string(),
            model_id: "mock-reasoning".to_string(),
            aliases: Vec::new(),
            context_window: None,
            generation_preset: None,
            default_effort: None,
            capabilities: swage_config::ModelCapabilitiesConfig {
                reasoning_replay: true,
                effort: None,
            },
        },
    )]
    .into_iter()
    .collect();
    state.replace_runtime(
        config,
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let assistant_replay = requests[1]
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::Reasoning { .. }))
                })
        })
        .and_then(|message| message.tool_replay())
        .expect("opted-in model should receive reasoning replay");

    assert!(matches!(
        assistant_replay.first(),
        Some(swage_core::ToolReplayPart::Reasoning { content, details })
            if content == "Private provider reasoning"
                && details.as_ref().is_some_and(|details| details["source"] == "test")
    ));
    assert!(
        assistant_replay
            .iter()
            .any(|part| matches!(part, swage_core::ToolReplayPart::ToolCall { .. })),
        "reasoning replay should travel with the assistant tool-use replay"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn later_turn_rebuilds_tool_replay_from_persisted_compact_rows() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let first_provider = Arc::new(MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "first" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "first complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        first_provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("first turn should complete");

    let (second_provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "second complete".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    state.replace_runtime(
        state.active_config(),
        Arc::new(second_provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Continue".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("second turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("second provider request should be captured");
    let assistant_tool_message = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolCall { .. }))
                })
        })
        .expect("later turn should include assistant tool-use replay");
    let tool_use = assistant_tool_message
        .tool_replay()
        .unwrap()
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolCall {
                call_id,
                name,
                arguments,
            } => Some((call_id, name, arguments)),
            _ => None,
        })
        .expect("tool-use replay");
    assert_eq!(tool_use.0, "c1");
    assert_eq!(tool_use.1, "echo");
    assert_eq!(tool_use.2["kind"], "tool_arguments");
    assert_eq!(tool_use.2["preview"], r#"{"text":"first"}"#);

    let tool_result_message = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::User
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolResult { .. }))
                })
        })
        .expect("later turn should include user tool-result replay");
    let tool_result = tool_result_message
        .tool_replay()
        .unwrap()
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult {
                call_id, content, ..
            } => Some((call_id, content)),
            _ => None,
        })
        .expect("tool-result replay");
    assert_eq!(tool_result.0, "c1");
    let envelope: swage_core::CompactToolReplay = serde_json::from_str(tool_result.1)
        .expect("tool result content should be compact replay JSON");
    assert_eq!(envelope.kind, "tool_result");
    assert_eq!(envelope.preview, r#"{"output":"first"}"#);
    assert!(
        envelope
            .full_result_ref
            .as_deref()
            .is_some_and(|reference| reference.ends_with(":c1:result")),
        "persisted replay should include a durable full-result reference"
    );
    assert_eq!(envelope.is_error, Some(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn later_turn_rebuilds_multi_round_tool_replay_as_separate_pairs() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let first_provider = Arc::new(MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "first" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "c2".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "second" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        first_provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Use echo twice".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("first turn should complete");

    let threads = storage
        .list_by_session(session_id)
        .await
        .expect("list threads");
    let turns = storage
        .list_turns_by_thread(threads[0].id)
        .await
        .expect("list turns");
    let first_turn_calls = storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .expect("list first-turn tool calls");
    assert_eq!(
        first_turn_calls
            .iter()
            .map(|call| (call.call_id.as_str(), call.provider_round))
            .collect::<Vec<_>>(),
        vec![("c1", 1), ("c2", 2)]
    );

    let (second_provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "second complete".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    state.replace_runtime(
        state.active_config(),
        Arc::new(second_provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Continue".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("second turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("second provider request should be captured");
    let replay_tool_use_ids: Vec<Vec<String>> = captured
        .messages
        .iter()
        .filter(|message| message.role() == Role::Assistant)
        .filter_map(|message| message.tool_replay())
        .map(|replay| {
            replay
                .iter()
                .filter_map(|part| match part {
                    swage_core::ToolReplayPart::ToolCall { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .filter(|ids| !ids.is_empty())
        .collect();

    assert_eq!(
        replay_tool_use_ids,
        vec![vec!["c1".to_string()], vec!["c2".to_string()]],
        "later-turn replay must preserve provider tool-round boundaries"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_turn_rebuilds_tool_replay_from_copied_history() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    let parent_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create parent thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: parent_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Completed,
            user_input: Some("Use echo".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");
    storage
        .create_message(NewMessage {
            thread_id: parent_thread.id,
            attachments: Vec::new(),
            turn_id: Some(parent_turn.id),
            role: Role::User,
            content: "Use echo".to_string(),
            position: 0,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create parent user message");
    let assistant_message = storage
        .create_message(NewMessage {
            thread_id: parent_thread.id,
            attachments: Vec::new(),
            turn_id: Some(parent_turn.id),
            role: Role::Assistant,
            content: "Echo complete".to_string(),
            position: 1,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create parent assistant message");
    storage
        .create_tool_call(NewToolCall {
            turn_id: parent_turn.id,
            agent_id: "gateway".to_string(),
            call_id: "call_1".to_string(),
            tool: "echo".to_string(),
            arguments: r#"{"text":"first"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({ "text": "first" }),
            ),
            result: Some(r#"{"output":"first"}"#.to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model_with_reference(
                r#"{"output":"first"}"#,
                false,
                "turn:parent:call_1:result",
            )),
            status: Some("success".to_string()),
            duration_ms: Some(12),
            truncated: false,
            total_bytes: Some(18),
            provider_round: 1,
            position: 0,
            parent_call_id: None,
            display_summary: None,
        })
        .await
        .expect("create parent tool call");

    let branch = storage
        .create_branch(parent_thread.id, assistant_message.id)
        .await
        .expect("create branch");
    let child_turn = storage
        .list_turns_by_thread(branch.child_thread_id)
        .await
        .expect("list child turns")
        .into_iter()
        .next()
        .expect("branch should copy parent turn");
    let (provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "branch complete".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        Some(branch.child_thread_id),
        "Continue from branch".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("branch turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("provider request should be captured");
    let assistant_tool_replay = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolCall { .. }))
                })
        })
        .and_then(|message| message.tool_replay())
        .expect("branch history should rebuild assistant tool replay");
    assert!(assistant_tool_replay.iter().any(|part| matches!(
        part,
        swage_core::ToolReplayPart::ToolCall { call_id, name, arguments }
            if call_id == "call_1"
                && name == "echo"
                && arguments["kind"] == "tool_arguments"
                && arguments["preview"] == r#"{"text":"first"}"#
    )));

    let tool_result_replay = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::User
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolResult { .. }))
                })
        })
        .and_then(|message| message.tool_replay())
        .expect("branch history should rebuild tool result replay");
    let result_content = tool_result_replay
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult {
                call_id, content, ..
            } if call_id == "call_1" => Some(content),
            _ => None,
        })
        .expect("tool result replay for copied branch call");
    let envelope: swage_core::CompactToolReplay =
        serde_json::from_str(result_content).expect("tool result replay should stay compact");
    assert_eq!(envelope.preview, r#"{"output":"first"}"#);
    let expected_ref = swage_core::tool_result_reference(&child_turn.id.to_string(), "call_1");
    assert_eq!(
        envelope.full_result_ref.as_deref(),
        Some(expected_ref.as_str())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn later_turn_rebuilds_failed_tool_replay_as_error() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,

            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    let thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create thread");
    let failed_turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Completed,
            user_input: Some("Run the command".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create failed-call turn");
    storage
        .create_message(NewMessage {
            thread_id: thread.id,
            attachments: Vec::new(),
            turn_id: Some(failed_turn.id),
            role: Role::User,
            content: "Run the command".to_string(),
            position: 0,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create user message");
    storage
        .create_message(NewMessage {
            thread_id: thread.id,
            attachments: Vec::new(),
            turn_id: Some(failed_turn.id),
            role: Role::Assistant,
            content: "Trying the command".to_string(),
            position: 1,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create assistant message");
    storage
        .create_tool_call(NewToolCall {
            turn_id: failed_turn.id,
            agent_id: "gateway".to_string(),
            call_id: "call_fail".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"missing-command"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({ "command": "missing-command" }),
            ),
            result: Some("command not found".to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model_with_reference(
                "command not found",
                true,
                "turn:failed:call_fail:result",
            )),
            status: Some("error".to_string()),
            duration_ms: Some(7),
            truncated: false,
            total_bytes: Some(17),
            provider_round: 1,
            position: 0,
            parent_call_id: None,
            display_summary: None,
        })
        .await
        .expect("create failed tool call");

    let (provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "next answer".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        Some(thread.id),
        "Continue".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("later turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("provider request should be captured");
    let failed_replay = captured
        .messages
        .iter()
        .find_map(|message| {
            message.tool_replay().and_then(|replay| {
                replay.iter().find_map(|part| match part {
                    swage_core::ToolReplayPart::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } if call_id == "call_fail" => Some((content, *is_error)),
                    _ => None,
                })
            })
        })
        .expect("failed tool result should be replayed");

    assert!(failed_replay.1, "failed calls must replay as error results");
    let envelope: swage_core::CompactToolReplay =
        serde_json::from_str(failed_replay.0).expect("failed tool replay should stay compact");
    assert_eq!(envelope.preview, "command not found");
    assert_eq!(envelope.is_error, Some(true));
    assert_eq!(
        envelope.full_result_ref.as_deref(),
        Some("turn:failed:call_fail:result")
    );
}

#[test]
fn compact_tool_replay_messages_includes_nested_compact_rows() {
    let turn_id = Uuid::new_v4();
    let top_level_call_id = "python_parent".to_string();
    let records = vec![
        ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id,
            agent_id: "gateway".to_string(),
            call_id: top_level_call_id.clone(),
            tool: "python_execute".to_string(),
            arguments: r#"{"code":"run()"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({"code": "run()"}),
            ),
            result: Some(r#"{"ok":true}"#.to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model(
                r#"{"ok":true}"#,
                false,
            )),
            status: Some("success".to_string()),
            duration_ms: Some(20),
            truncated: false,
            total_bytes: Some(11),
            provider_round: 2,
            position: 0,
            created_at: chrono::Utc::now(),
            parent_call_id: None,
            display_summary: None,
        },
        ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id,
            agent_id: "gateway".to_string(),
            call_id: "nested_read".to_string(),
            tool: "file_read".to_string(),
            arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({"path": "Cargo.toml"}),
            ),
            result: Some(r#"{"content":"workspace"}"#.to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model(
                r#"{"content":"workspace"}"#,
                false,
            )),
            status: Some("success".to_string()),
            duration_ms: Some(3),
            truncated: false,
            total_bytes: Some(23),
            provider_round: 0,
            position: 1,
            created_at: chrono::Utc::now(),
            parent_call_id: Some(top_level_call_id),
            display_summary: None,
        },
    ];

    let replay = compact_tool_replay_messages(&records)
        .expect("complete nested replay records should be valid");
    assert_eq!(replay.len(), 2);

    let assistant_replay = replay[0]
        .tool_replay()
        .expect("assistant replay should use canonical tool replay");
    let tool_use_ids: Vec<_> = assistant_replay
        .iter()
        .filter_map(|part| match part {
            swage_core::ToolReplayPart::ToolCall { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_use_ids, vec!["python_parent", "nested_read"]);

    let result_replay = replay[1]
        .tool_replay()
        .expect("tool result replay should use canonical tool replay");
    let result_ids: Vec<_> = result_replay
        .iter()
        .filter_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(result_ids, vec!["python_parent", "nested_read"]);

    let nested_result = result_replay
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult {
                call_id, content, ..
            } if call_id == "nested_read" => Some(content),
            _ => None,
        })
        .expect("nested tool result should be present");
    let envelope: swage_core::CompactToolReplay = serde_json::from_str(nested_result)
        .expect("nested result should contain compact replay JSON");
    assert_eq!(envelope.preview, r#"{"content":"workspace"}"#);
}

fn complete_replay_record(call_id: &str) -> ToolCallRecord {
    ToolCallRecord {
        id: Uuid::new_v4(),
        turn_id: Uuid::new_v4(),
        agent_id: "gateway".into(),
        call_id: call_id.into(),
        tool: "file_read".into(),
        arguments: r#"{"path":"Cargo.toml"}"#.into(),
        model_arguments: swage_core::compact_tool_arguments_for_model(
            &serde_json::json!({"path": "Cargo.toml"}),
        ),
        result: Some("workspace".into()),
        model_result: Some(swage_core::compact_tool_result_for_model(
            "workspace",
            false,
        )),
        status: Some("success".into()),
        duration_ms: Some(1),
        truncated: false,
        total_bytes: Some(9),
        provider_round: 1,
        position: 0,
        created_at: chrono::Utc::now(),
        parent_call_id: None,
        display_summary: None,
    }
}

#[test]
fn replay_omits_rejected_tool_calls_with_empty_names() {
    let mut rejected = complete_replay_record("rejected-call");
    rejected.tool.clear();
    rejected.status = Some("error".into());
    let accepted = complete_replay_record("accepted-call");

    let replay = compact_tool_replay_messages(&[rejected, accepted])
        .expect("a rejected provider call should not make history unreadable");
    let replayed_call_ids: Vec<_> = replay
        .iter()
        .filter_map(ChatMessage::tool_replay)
        .flatten()
        .filter_map(|part| match part {
            ToolReplayPart::ToolCall { call_id, .. }
            | ToolReplayPart::ToolResult { call_id, .. } => Some(call_id.as_str()),
            ToolReplayPart::Text { .. } | ToolReplayPart::Reasoning { .. } => None,
        })
        .collect();

    assert_eq!(
        replayed_call_ids,
        vec!["accepted-call", "accepted-call"],
        "the valid call and result should remain while the rejected pair stays out of provider replay"
    );
}

#[test]
fn replay_uses_valid_raw_arguments_when_compact_arguments_are_unreadable() {
    let mut record = complete_replay_record("legacy-call");
    record.model_arguments = "not-json".into();

    let arguments = parse_model_arguments(&record)
        .expect("valid raw arguments are a safe legacy replay fallback");

    assert_eq!(arguments["path"], "Cargo.toml");
}

#[test]
fn replay_omits_calls_without_model_safe_results() {
    let mut record = complete_replay_record("incomplete-call");
    record.result = Some("raw result must not be replayed".into());
    record.model_result = None;

    let replay = compact_tool_replay_messages(&[record])
        .expect("an incomplete call should be omitted rather than replaying raw data");

    assert!(replay.is_empty());
}

#[test]
fn corrupt_replay_arguments_are_rejected() {
    let mut record = complete_replay_record("corrupt-call");
    record.model_arguments = "{not-json".into();
    record.arguments = "{also-not-json".into();

    let error = compact_tool_replay_messages(&[record])
        .expect_err("corrupt replay arguments must not be sent to the provider");

    assert!(matches!(error, StorageError::Corruption { .. }));
}

#[test]
fn orphaned_nested_replay_is_rejected() {
    let mut record = complete_replay_record("nested-call");
    record.parent_call_id = Some("missing-parent".into());

    let error = compact_tool_replay_messages(&[record])
        .expect_err("orphaned nested replay must not be regrouped silently");

    assert!(matches!(error, StorageError::Corruption { .. }));
}

#[tokio::test]
async fn attended_turn_prompts_and_records_rejected_granted_tool_call() {
    let rounds = vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "attended-rejection".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "must not execute" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "The operator rejected the call.".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ];
    let (state, session_id) = make_state_with_approval_gated_echo(rounds).await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        process_turn(
            &task_state,
            session_id,
            None,
            TurnInput::from("Use echo"),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    let approval_id = loop {
        match rx.recv().await.expect("attended turn should emit events") {
            ServerEvent::ApprovalRequired { approval_id, .. } => break approval_id,
            _ => continue,
        }
    };
    state
        .approval_channels
        .lock()
        .unwrap()
        .remove(&approval_id)
        .expect("approval decision channel should be registered")
        .send(swage_agent::ApprovalDecision {
            decision: swage_agent::approval::ApprovalDecisionValue::Rejected,
            scope: swage_agent::approval::ApprovalDecisionScope::Once,
            reason: Some("operator declined".into()),
        })
        .expect("rejection should be delivered");
    task.await
        .expect("turn task should finish")
        .expect("rejection should be returned to the model visibly");

    let approval = state
        .storage
        .get_approval(approval_id)
        .await
        .expect("approval lookup")
        .expect("approval row should remain recorded");
    assert_eq!(approval.state, ApprovalState::Rejected);
    let thread = state.storage.list_by_session(session_id).await.unwrap()[0].clone();
    let turn = state.storage.list_turns_by_thread(thread.id).await.unwrap()[0].clone();
    let calls = state
        .storage
        .list_tool_calls_by_turn(turn.id)
        .await
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].status.as_deref(), Some("rejected"));
    assert!(
        calls[0]
            .result
            .as_deref()
            .is_some_and(|result| result.contains("operator declined")),
        "the recorded call should expose the rejection to the model"
    );
}

#[tokio::test]
async fn approval_resumption_keeps_structured_tool_messages() {
    // Another test's space context element would otherwise reach this turn
    // through the process-wide swage home, adding a sixth message.
    let _home = crate::test_support::SwageHomeGuard::isolate();
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let mut runtime_config = state.active_config();
    runtime_config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "echo".into(),
        )];
    state.replace_runtime(
        runtime_config,
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let subscription = Arc::new(Mutex::new(SubscriptionState::new(true)));
    let (tx, _rx) = mpsc::unbounded_channel();
    let approval_channels = Arc::clone(&state.approval_channels);

    let turn_task = tokio::spawn(async move {
        process_turn(
            &state,
            session_id,
            None,
            "Use echo".to_string(),
            tx,
            subscription,
            None,
        )
        .await
    });

    let approval_sender = loop {
        let maybe_sender = {
            let mut locked = approval_channels.lock().unwrap();
            let approval_id = locked.keys().next().copied();
            approval_id.and_then(|id| locked.remove(&id))
        };
        if let Some(sender) = maybe_sender {
            break sender;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };

    approval_sender
        .send(swage_agent::ApprovalDecision {
            decision: swage_agent::approval::ApprovalDecisionValue::Approved,
            scope: swage_agent::approval::ApprovalDecisionScope::Once,
            reason: None,
        })
        .expect("approval decision should be delivered");

    turn_task
        .await
        .expect("turn task should finish")
        .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");

    let second = &requests[1];
    assert_eq!(second.messages.len(), 5);
    assert_eq!(second.messages[0].role(), Role::System);
    assert_eq!(second.messages[1].role(), Role::System);
    assert_eq!(second.messages[2].role(), Role::User);
    assert_eq!(second.messages[3].role(), Role::Assistant);
    assert_eq!(second.messages[4].role(), Role::User);
    assert!(
        second.messages.iter().any(|message| {
            message.role() == Role::System
                && message
                    .text_content()
                    .contains("You are an agent running on Swage")
        }),
        "resumed request should retain orientation"
    );
    assert!(
        second.messages.iter().any(|message| {
            message.role() == Role::System && message.text_content().contains("## Working norms")
        }),
        "resumed request should retain the working norms"
    );
    let assistant_replay = second
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay.iter().any(|part| {
                        matches!(
                            part,
                            ToolReplayPart::ToolCall { call_id, name, arguments }
                                if call_id == "c1" && name == "echo"
                                    && arguments == &serde_json::json!({ "text": "first" })
                        )
                    })
                })
        })
        .expect("assistant tool-use message should retain the structured echo call after approval");
    assert_eq!(
        assistant_replay
            .tool_replay()
            .expect("assistant replay after approval")
            .iter()
            .filter(
                |part| matches!(part, ToolReplayPart::ToolCall { call_id, .. } if call_id == "c1")
            )
            .count(),
        1,
        "the approved tool call must be replayed exactly once"
    );
    let user_replay = second
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::User
                && message.tool_replay().is_some_and(|replay| {
                    replay.iter().any(|part| {
                        matches!(
                            part,
                            ToolReplayPart::ToolResult { call_id, is_error, .. }
                                if call_id == "c1" && !is_error
                        )
                    })
                })
        })
        .expect("user tool-result message should retain the structured echo result after approval");
    assert_eq!(
        user_replay
            .tool_replay()
            .expect("tool-result replay after approval")
            .iter()
            .filter(
                |part| matches!(part, ToolReplayPart::ToolResult { call_id, .. } if call_id == "c1")
            )
            .count(),
        1,
        "the approved tool result must be replayed exactly once"
    );
}

async fn assert_delegation_fails_before_child_creation(
    state: &AppState,
    root_turn_id: Uuid,
    space_id: Uuid,
    task_definition_id: Option<&str>,
    request_id: &str,
    expected_error: &str,
    assertion_message: &str,
) {
    let child_count_before_failure = state
        .storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list child sessions before delegation failure")
        .len();
    let error = match GatewayDelegationHandler::new(state.clone())
        .handle_delegation(
            "agent-b",
            "This must not start",
            task_definition_id,
            None,
            None,
            None,
            None,
            None,
            DispatchContextComposition::default(),
            request_id,
            "gateway",
            root_turn_id,
            space_id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
    {
        Ok(_) => panic!("delegation must fail before child creation"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(expected_error),
        "unexpected delegation failure: {error}"
    );
    assert_eq!(
        state
            .storage
            .list_sessions_by_agent_id("agent-b")
            .await
            .expect("list child sessions after delegation failure")
            .len(),
        child_count_before_failure,
        "{assertion_message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delegate_tool_executes_child_agent_and_resumes_parent_turn() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: dispatch_bound_metadata(&["upstream".to_string()]),
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");
    storage
        .upsert_agent_definition(swage_storage::NewAgentDefinition {
            id: "gateway".into(),
            space_id: Some(space.id),
            display_name: "Gateway".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: Some(serde_json::json!({
                "allow": ["delegate", "shell"], "deny": [], "bundles": {}, "require_approval": []
            })),
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store dispatching agent definition");
    storage
        .upsert_agent_definition(swage_storage::NewAgentDefinition {
            id: "upstream".into(),
            space_id: Some(space.id),
            display_name: "Upstream".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: Some(serde_json::json!({
                "allow": ["delegate"], "deny": [], "bundles": {}, "require_approval": []
            })),
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store inherited dispatch-bound definition");
    storage
        .upsert_agent_definition(swage_storage::NewAgentDefinition {
            id: "agent-b".into(),
            space_id: Some(space.id),
            display_name: "Agent B".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: Some(serde_json::json!({
                "allow": ["shell"], "deny": [], "bundles": {}
            })),
            skills: vec!["definition-declared".into()],
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store delegated agent definition");
    storage
        .upsert_task_definition(swage_storage::NewTaskDefinition {
            id: "strict-review".into(),
            instructions: "Task-defined delegated instruction.".into(),
            model_id: Some("task-review-model".into()),
            tool_permissions: Some(serde_json::json!({
                "allow": ["shell"], "deny": [], "bundles": {}, "require_approval": []
            })),
            output_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["result"],
                "properties": {
                    "result": { "type": "string" }
                }
            })),
            output_max_retries: Some(1),
        })
        .await
        .expect("store delegated agent definition");

    let parent_round = vec![
        StreamChunk::ToolUse {
            call_id: "delegate-1".into(),
            name: "delegate".into(),
            arguments: serde_json::json!({
                "target_agent": "agent-b",
                "task": "Summarise this request",
                "task_definition": "strict-review",
                "skills": ["delegated-specialist"],
                "elements": ["delegated-brief"],
                "variables": {"owner": "delegating parent"},
                "assembly": "delegated-focus",
            }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let child_marker = Uuid::new_v4().to_string();
    let child_response = format!(r#"{{"result":"{child_marker}"}}"#);
    let child_round = vec![
        StreamChunk::ToolUse {
            call_id: "excluded-shell".into(),
            name: "shell".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let diagnostic_round = |response: String| {
        vec![
            StreamChunk::ProviderResponseWitness(swage_core::ProviderResponseWitness {
                http_status: 200,
                request_id: Some("delegated-diagnostics".into()),
                terminal_event: Some("message_stop".into()),
                stop_reason: Some("end_turn".into()),
                usage: swage_core::TokenUsage {
                    input_tokens: 9,
                    output_tokens: 2,
                },
                content_blocks: vec![swage_core::ProviderContentBlockWitness {
                    block_type: "text".into(),
                    count: 1,
                    bytes: response.len() as u64,
                }],
            }),
            StreamChunk::Reasoning {
                content: "delegated diagnostic reasoning".into(),
                details: Some(serde_json::json!({"kind": "summary"})),
            },
            StreamChunk::Delta { content: response },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]
    };
    let child_denied_round = diagnostic_round(child_response.clone());
    let parent_resume_round = vec![
        StreamChunk::Delta {
            content: "Parent resumed after delegation".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let parent_next_round = vec![
        StreamChunk::Delta {
            content: "Parent continued on its next turn".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let child_next_round = vec![
        StreamChunk::Delta {
            content: r#"{"result":"Child continued within its bound"}"#.into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let control_round = diagnostic_round(child_response.clone());

    let (provider, captured_requests) = CapturingMultiRoundProvider::new(vec![
        parent_round,
        child_round,
        child_denied_round,
        parent_resume_round,
        parent_next_round,
        child_next_round,
        control_round,
    ]);

    let swage_home = tempfile::TempDir::new().expect("temp swage home");
    std::fs::create_dir_all(swage_home.path().join("assemblies"))
        .expect("create assembly directory");
    std::fs::write(
        swage_home.path().join("assemblies/delegated-focus.js"),
        "for (const element of elements) add(element.name === 'delegated-brief' ? element.render() : element);",
    )
    .expect("write delegated assembly");
    std::fs::create_dir_all(swage_home.path().join("elements")).expect("create element directory");
    std::fs::write(
        swage_home.path().join("elements/delegated-brief.md"),
        "Delegated owner: {{owner}}",
    )
    .expect("write delegated briefing");
    let space_skill_dir = swage_home
        .path()
        .join("spaces/default/skills/foreground-only");
    std::fs::create_dir_all(&space_skill_dir).expect("create foreground skill directory");
    std::fs::write(
        space_skill_dir.join("SKILL.md"),
        "---\ndescription: Foreground only\n---\nUse the parent foreground skill.\n",
    )
    .expect("write foreground skill");
    let declared_skill_dir = swage_home
        .path()
        .join("spaces/default/skills/definition-declared");
    std::fs::create_dir_all(&declared_skill_dir).expect("create declared skill directory");
    std::fs::write(
        declared_skill_dir.join("SKILL.md"),
        "---\ndescription: Definition-declared behaviour\n---\nUse the definition-declared skill.\n",
    )
    .expect("write declared skill");
    let requested_skill_dir = swage_home
        .path()
        .join("spaces/default/skills/delegated-specialist");
    std::fs::create_dir_all(&requested_skill_dir)
        .expect("create requested delegated skill directory");
    std::fs::write(
            requested_skill_dir.join("SKILL.md"),
            "---\ndescription: Delegated specialist behaviour\n---\nUse the delegated specialist skill.\n",
        )
        .expect("write requested delegated skill");
    let agent_skill_dir = swage_home
        .path()
        .join("spaces/default/agents/agent-b/skills/agent-owned-specialist");
    std::fs::create_dir_all(&agent_skill_dir)
        .expect("create delegated agent-level skill directory");
    std::fs::write(
        agent_skill_dir.join("SKILL.md"),
        "---\ndescription: Agent-owned specialist behaviour\n---\nUse the agent-owned specialist skill.\n",
    )
    .expect("write delegated agent-level skill");
    let _guard = SwageHomeGuard::set_to(swage_home.path());
    storage
        .activate_skill(session_id, "foreground-only")
        .await
        .expect("activate foreground skill");

    let dispatcher = crate::tool::ToolDispatcher::allow_all(vec![
        Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn crate::tool::Tool>,
        Arc::new(ShellMockTool) as Arc<dyn crate::tool::Tool>,
    ]);

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig {
            models: swage_config::ModelCatalogConfig {
                default: "task-review-model".into(),
                entries: [(
                    "task-review-model".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "test".into(),
                        model_id: "task-review-wire".into(),
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
            },
            communication: swage_config::CommunicationConfig {
                context_transfer: swage_config::ContextTransferConfig {
                    mode: swage_config::ContextTransferMode::Explicit,
                    allowed_fields: vec![swage_config::ContextTransferField::User],
                    summary_budget: None,
                    targets: std::collections::HashMap::new(),
                },
                ..Default::default()
            },
            ..Default::default()
        },
        dispatcher,
        None,
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Delegate this".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        events.iter().any(|event| matches!(
            event,
            ServerEvent::DelegationStarted { target_agent_id, parent_turn_id, .. }
            if target_agent_id == "agent-b" && *parent_turn_id != Uuid::nil()
        )),
        "expected DelegationStarted event, got: {events:?}"
    );

    assert!(
        events.iter().any(|event| matches!(
            event,
            ServerEvent::DelegationCompleted { target_agent_id, success, .. }
            if target_agent_id == "agent-b" && *success
        )),
        "expected DelegationCompleted event, got: {events:?}"
    );

    let root_turns = state
        .storage
        .list_turns_by_thread(root_thread.id)
        .await
        .expect("list root turns");
    assert_eq!(root_turns.len(), 1, "expected exactly one root turn");
    let root_turn = &root_turns[0];
    assert_eq!(root_turn.state, TurnState::Completed);
    assert_eq!(
        root_turn.assistant_response.as_deref(),
        Some("Parent resumed after delegation"),
    );

    let child_sessions = state
        .storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list child sessions");
    assert_eq!(child_sessions.len(), 1, "expected one child session");
    assert_eq!(
        child_sessions[0]
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(crate::turn::DISPATCH_BOUND_METADATA_KEY))
            .and_then(serde_json::Value::as_array),
        Some(&vec![
            serde_json::Value::String("upstream".into()),
            serde_json::Value::String("gateway".into()),
        ]),
        "delegated child should persist its complete dispatch-bound lineage"
    );
    assert_eq!(
        child_sessions[0]
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("task_definition"))
            .and_then(serde_json::Value::as_str),
        Some("strict-review"),
        "delegated child should persist its task definition for later turns"
    );

    let child_threads = state
        .storage
        .list_by_session(child_sessions[0].id)
        .await
        .expect("list child threads");
    assert_eq!(child_threads.len(), 1, "expected one child thread");

    let child_turns = state
        .storage
        .list_turns_by_thread(child_threads[0].id)
        .await
        .expect("list child turns");
    assert_eq!(child_turns.len(), 1, "expected one child turn");
    assert_eq!(child_turns[0].state, TurnState::Completed);
    assert_eq!(child_turns[0].parent_turn_id, Some(root_turn.id));
    assert_eq!(
        child_turns[0].assistant_response.as_deref(),
        Some(child_response.as_str())
    );
    let child_snapshot = state
        .storage
        .get_snapshot(child_turns[0].id)
        .await
        .expect("read child context snapshot")
        .expect("delegated assembly report should be persisted");
    let child_snapshot: serde_json::Value =
        serde_json::from_str(&child_snapshot).expect("child context snapshot should be JSON");
    assert!(
        child_snapshot["elements"].is_array(),
        "delegated assembly report should contain its element decisions"
    );
    assert_eq!(child_snapshot["assembly_name"], "delegated-focus");
    let skills = child_snapshot["elements"]
        .as_array()
        .expect("delegated assembly report should list elements")
        .iter()
        .find(|element| element["name"] == "skills")
        .expect("delegated child report should record its requested skills");
    assert!(
        skills["source"]
            .as_str()
            .is_some_and(|source| source.contains("delegated-specialist")),
        "delegated child report should name the requested skill: {skills}"
    );
    assert!(
        child_snapshot["elements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["name"] == "delegated-brief")
    );
    assert_eq!(
        child_snapshot["effective_tool_permissions"],
        serde_json::json!({
            "allow": [],
            "deny": [],
            "require_approval": [],
        }),
        "the upstream bound must keep shell out even though both immediate sender and child grant it"
    );
    assert_eq!(
        child_snapshot["provider_visible"]["requests"][0]["request"]["provider"],
        serde_json::json!("capturing-multi-round"),
        "the delegated child's provider diagnostic must survive into its stored context record"
    );
    let child_audit = state
        .storage
        .list_audit_events_by_session(child_sessions[0].id)
        .await
        .expect("list child audit events");
    assert!(
        child_audit.iter().any(|event| {
            event.entity_id == crate::experimentation::LOCATION_CONTEXT_ASSEMBLY
                && event.event_kind == "variant_selected"
                && event.metadata["turn_id"] == serde_json::json!(child_turns[0].id)
        }),
        "delegated assembly selection should be recorded against the child turn"
    );
    let child_tool_calls = state
        .storage
        .list_tool_calls_by_turn(child_turns[0].id)
        .await
        .expect("list delegated tool calls");
    assert_eq!(child_tool_calls.len(), 1);
    assert_eq!(child_tool_calls[0].tool, "shell");
    assert_eq!(child_tool_calls[0].status.as_deref(), Some("error"));
    assert!(
        child_tool_calls[0]
            .result
            .as_deref()
            .is_some_and(|result| result.contains("not permitted")),
        "the inherited-bound tool call should leave a denied audit record: {child_tool_calls:?}"
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Continue after the foreground delegation".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("parent next turn should complete");

    state
        .storage
        .upsert_task_definition(swage_storage::NewTaskDefinition {
            id: "strict-review".into(),
            instructions: "Edited task-defined delegated instruction.".into(),
            model_id: Some("task-review-model".into()),
            tool_permissions: Some(serde_json::json!({
                "allow": ["shell"], "deny": [], "bundles": {}, "require_approval": []
            })),
            output_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["result"],
                "properties": { "result": { "type": "string" } }
            })),
            output_max_retries: Some(1),
        })
        .await
        .expect("edit delegated task definition");
    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        child_sessions[0].id,
        None,
        "Continue the delegated work".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("child next turn should remain dispatch-bound");

    // A direct turn is the control: it records the same provider diagnostics
    // through the ordinary event path. The delegated child's stored snapshot
    // must preserve each diagnostic kind with the same values.
    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        child_sessions[0].id,
        None,
        "Run the diagnostic control".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("undelegated control turn should complete");
    let control_turn = state
        .storage
        .list_turns_by_thread(child_threads[0].id)
        .await
        .expect("list control turns")
        .into_iter()
        .last()
        .expect("control turn");
    let control_snapshot = state
        .storage
        .get_snapshot(control_turn.id)
        .await
        .expect("read control context snapshot")
        .expect("control context snapshot should be persisted");
    let control_snapshot: serde_json::Value =
        serde_json::from_str(&control_snapshot).expect("control snapshot should be JSON");
    let diagnostic_kinds = |snapshot: &serde_json::Value| {
        serde_json::json!({
            "request": snapshot["provider_visible"]["requests"][0]["request"]["provider"],
            "response": snapshot["provider_visible"]["responses"][0]["witness"],
            "reasoning": {
                "content": snapshot["provider_visible"]["reasoning"][0]["content"],
                "details": snapshot["provider_visible"]["reasoning"][0]["details"],
            },
        })
    };
    assert_eq!(
        diagnostic_kinds(&child_snapshot),
        diagnostic_kinds(&control_snapshot),
        "the delegated child record must retain the request, response, and reasoning diagnostics its undelegated twin records"
    );

    let captured_requests = captured_requests.lock().unwrap();
    assert_eq!(
        captured_requests.len(),
        7,
        "expected parent, two child rounds, resumed-parent, parent continuation, child continuation, and direct control requests"
    );
    let delegated_result = captured_requests[3]
        .messages
        .iter()
        .filter_map(ChatMessage::tool_replay)
        .flatten()
        .find_map(|part| match part {
            ToolReplayPart::ToolResult {
                call_id, content, ..
            } if call_id == "delegate-1" => Some(content),
            _ => None,
        })
        .expect("resumed parent request should contain the delegated result");
    let delegated_result: serde_json::Value = serde_json::from_str(delegated_result)
        .expect("delegated result payload should be valid JSON");
    assert_eq!(
        delegated_result["structured_output"],
        serde_json::json!({ "result": child_marker.as_str() }),
        "the delegated definition's output contract should produce structured output in the result payload"
    );
    let parent_request_text = captured_requests[0]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        parent_request_text.contains("Use the parent foreground skill."),
        "parent provider request should include the activated foreground skill: {parent_request_text}"
    );
    let child_request_text = captured_requests[1]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        child_request_text.contains("Delegated owner: delegating parent"),
        "delegated provider request should render call-scoped variables: {child_request_text}"
    );
    assert!(
        captured_requests[1]
            .tool_definitions
            .iter()
            .all(|definition| definition["name"] != "shell"),
        "the initial delegated request must not advertise a target-only tool"
    );
    assert!(
        child_request_text.contains("You are an agent running on Swage"),
        "delegated provider request should include platform identity: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Space: the default space")
            && child_request_text.contains("Working directory:")
            && child_request_text.contains("Session started:"),
        "delegated provider request should include the child session's place: {child_request_text}"
    );
    assert!(
        child_request_text.contains("## Skills available in this session")
            && child_request_text
                .contains("agent-owned-specialist — Agent-owned specialist behaviour"),
        "delegated provider request should catalogue the target agent's own skill: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Inherited Background Context"),
        "delegated provider request should include transferred context: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Delegate this"),
        "explicit user context transfer should carry the parent request: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Delegated Task\n\nSummarise this request"),
        "delegated provider request should fence the task separately: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Use the agent-owned specialist skill."),
        "delegated provider request should activate the target agent's own skill instructions: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Use the definition-declared skill."),
        "delegated provider request should include definition-declared skill instructions: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Task-defined delegated instruction."),
        "a genuinely reached child must receive its named task instructions: {child_request_text}"
    );
    assert_eq!(
        captured_requests[1].model.as_deref(),
        Some("task-review-wire"),
        "the delegated child must use the named task's model preference"
    );
    let child_audit = state
        .storage
        .list_audit_events()
        .await
        .expect("list child task audit events");
    assert!(child_audit.iter().any(|event| {
        event.event_kind == "variant_selected"
            && event.entity_id == crate::experimentation::LOCATION_TASK_PROMPT
    }));
    assert!(
        !child_request_text.contains("Use the parent foreground skill."),
        "delegated provider request must not inherit user-activated parent skills: {child_request_text}"
    );
    assert!(
        state
            .storage
            .list_active_skills(child_sessions[0].id)
            .await
            .expect("list child skill activations")
            .iter()
            .all(|activation| activation.skill_name != "definition-declared"),
        "definition declarations must not be persisted as user activations"
    );
    assert!(
        !child_request_text.contains("Parent resumed after delegation"),
        "child provider request must not inherit future parent continuation"
    );
    let resumed_parent_request_text = captured_requests[3]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        !resumed_parent_request_text.contains("Use the agent-owned specialist skill."),
        "the target agent's own skill instructions must not bleed into the resumed parent request"
    );
    let next_parent_request_text = captured_requests[4]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_eq!(
        next_parent_request_text.matches(&child_marker).count(),
        0,
        "wait-mode result marker {child_marker} should not be delivered again on the parent's next turn; request: {next_parent_request_text}"
    );
    assert!(
        captured_requests[5]
            .tool_definitions
            .iter()
            .all(|definition| definition["name"] != "shell"),
        "a subsequent child turn must re-read its metadata and remain bounded"
    );
    let next_child_request_text = captured_requests[5]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        next_child_request_text.contains("Edited task-defined delegated instruction.")
            && !next_child_request_text.contains("\n\nTask-defined delegated instruction."),
        "a subsequent child turn must re-resolve the edited task definition: {next_child_request_text}"
    );
    drop(captured_requests);

    assert_delegation_fails_before_child_creation(
        &state,
        root_turn.id,
        space.id,
        Some("missing-task"),
        "missing-task-delegate",
        "no task definition 'missing-task' exists",
        "task-definition resolution failure must happen before child creation",
    )
    .await;

    let sender_config_dir = swage_home.path().join("spaces/default/agents/gateway");
    std::fs::create_dir_all(&sender_config_dir).expect("create sender config directory");
    std::fs::write(sender_config_dir.join("config.toml"), "not valid toml =")
        .expect("write broken dispatch-bound config");
    assert_delegation_fails_before_child_creation(
        &state,
        root_turn.id,
        space.id,
        None,
        "missing-lineage-delegate",
        "failed to load dispatch bound",
        "dispatch-bound resolution failure must prevent child execution",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delegated_child_snapshot_records_exact_non_empty_effective_tool_grant() {
    let _swage_home = SwageHomeGuard::isolate();
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    // This fixture needs an authored restrictive parent; `gateway` always
    // resolves to its shipped definition and intentionally ignores stored rows.
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "agent-a".into(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "agent-a".into(),
        },
    )
    .await
    .expect("create root thread");
    storage
        .upsert_agent_definition(swage_storage::NewAgentDefinition {
            id: "agent-a".into(),
            space_id: Some(space.id),
            display_name: "Agent A".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: Some(serde_json::json!({
                "allow": ["delegate", "file_read"], "deny": ["file_read"], "bundles": {},
                "require_approval": []
            })),
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store dispatching agent definition");
    storage
        .upsert_agent_definition(swage_storage::NewAgentDefinition {
            id: "agent-b".into(),
            space_id: Some(space.id),
            display_name: "Agent B".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: Some(serde_json::json!({
                "allow": ["shell", "file_read"], "deny": [], "bundles": {},
                "require_approval": []
            })),
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store delegated agent definition");

    let (provider, _captured_requests) = CapturingMultiRoundProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "delegate-1".into(),
                name: "delegate".into(),
                arguments: serde_json::json!({
                    "target_agent": "agent-b",
                    "task": "Read safely",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "child complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "parent resumed".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let dispatcher = crate::tool::ToolDispatcher::allow_all(vec![
        Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn crate::tool::Tool>,
        Arc::new(ShellMockTool) as Arc<dyn crate::tool::Tool>,
        Arc::new(FileReadMockTool) as Arc<dyn crate::tool::Tool>,
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        dispatcher,
        None,
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session.id,
        None,
        TurnInput::from("Delegate this"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let child_session = state
        .storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list child sessions")
        .into_iter()
        .next()
        .expect("delegated child session");
    let child_thread = state
        .storage
        .list_by_session(child_session.id)
        .await
        .expect("list child threads")
        .into_iter()
        .next()
        .expect("delegated child thread");
    let child_turn = state
        .storage
        .list_turns_by_thread(child_thread.id)
        .await
        .expect("list child turns")
        .into_iter()
        .next()
        .expect("delegated child turn");
    let snapshot = state
        .storage
        .get_snapshot(child_turn.id)
        .await
        .expect("read child snapshot")
        .expect("delegated child snapshot");
    let snapshot: serde_json::Value =
        serde_json::from_str(&snapshot).expect("child snapshot should be JSON");
    assert_eq!(
        snapshot["effective_tool_permissions"],
        serde_json::json!({
            "allow": ["file_read"],
            "deny": ["file_read"],
            "require_approval": [],
        }),
        "the audit record must carry the exact permissions used to build the dispatcher"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delegated_snapshot_loss_records_degradation_and_double_failure_escalates() {
    let storage = Arc::new(MockStorage::new());
    storage.fail_snapshot_writes();
    let recorder = delegation_snapshot_recorder(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
    );
    let turn_id = Uuid::new_v4();

    recorder
        .record(Uuid::new_v4(), turn_id, &serde_json::json!({}))
        .await
        .expect("a recorded reconstructive gap should allow the turn to continue");
    let db = storage.db.lock().await;
    assert!(db.audit_events.iter().any(|event| {
        event.entity_id == turn_id.to_string()
            && event.event_kind == "turn_record_degraded"
            && event.metadata["record"] == "delegated_context_snapshot"
    }));
    drop(db);

    let broken_storage = Arc::new(MockStorage::new());
    broken_storage.fail_snapshot_writes();
    broken_storage.fail_audit_writes();
    let child_turn = broken_storage
        .create_turn(NewTurn {
            thread_id: Uuid::new_v4(),
            agent_id: "agent-b".to_string(),
            state: TurnState::Pending,
            user_input: Some("delegated work".to_string()),
            parent_turn_id: Some(Uuid::new_v4()),
        })
        .await
        .expect("create delegated child turn");
    let recorder = delegation_snapshot_recorder(
        Arc::clone(&broken_storage) as Arc<dyn swage_storage::store::Storage>
    );
    let error = recorder
        .record(Uuid::new_v4(), child_turn.id, &serde_json::json!({}))
        .await
        .expect_err("losing both the snapshot and its degradation record must fail loudly");
    assert!(error.to_string().contains("loss could not be recorded"));
    assert_eq!(
        broken_storage
            .get_turn(child_turn.id)
            .await
            .expect("read delegated child turn")
            .expect("delegated child turn should remain stored")
            .state,
        TurnState::Failed,
        "an unrecorded reconstructive gap must force the delegated child turn to fail"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_detached_result_reaches_parent_next_turn() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");

    let child_started = Arc::new(Notify::new());
    let child_release = Arc::new(Notify::new());
    let child_marker = Uuid::new_v4().to_string();
    let (provider, captured_requests) = DetachedGatedProvider::new(
        child_started.clone(),
        child_release.clone(),
        child_marker.clone(),
    );

    let dispatcher =
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn crate::tool::Tool>
        ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        dispatcher,
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Delegate independently".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("detached parent turn should complete before child release");

    tokio::time::timeout(std::time::Duration::from_secs(1), child_started.notified())
        .await
        .expect("detached child provider stream should have started");

    let root_turns = state
        .storage
        .list_turns_by_thread(root_thread.id)
        .await
        .expect("list root turns");
    assert_eq!(root_turns.len(), 1, "expected one root turn");
    let root_turn = &root_turns[0];
    assert_eq!(root_turn.state, TurnState::Completed);
    assert_eq!(
        root_turn.assistant_response.as_deref(),
        Some("Parent continued without waiting"),
    );

    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(root_turn.id)
        .await
        .expect("list tool calls");
    let delegate_tool_call = tool_calls
        .iter()
        .find(|call| call.tool == "delegate")
        .expect("delegate tool call should be stored");
    let detached_output: serde_json::Value = serde_json::from_str(
        delegate_tool_call
            .result
            .as_deref()
            .expect("delegate tool result should be stored"),
    )
    .expect("delegate tool result should be JSON");
    assert_eq!(detached_output["status"], "detached");
    assert_eq!(
        detached_output["result_delivery"],
        "The delegated work continues independently. Its durable result message will be stored with correlation_id equal to delegate_message_id when the child finishes."
    );

    let delegate_message_id = detached_output["delegate_message_id"]
        .as_str()
        .expect("delegate message id should be a string")
        .parse::<Uuid>()
        .expect("delegate message id should be a uuid");
    let delegate_message = state
        .storage
        .get_agent_message(delegate_message_id)
        .await
        .expect("get delegate message")
        .expect("delegate message should exist");
    assert_eq!(delegate_message.status, MessageStatus::Delivered);
    assert_eq!(
        state
            .storage
            .list_by_correlation(delegate_message_id)
            .await
            .expect("list correlated messages before release")
            .len(),
        0,
        "child result should not exist until the gated provider is released"
    );

    let child_session_id = detached_output["child_session_id"]
        .as_str()
        .expect("child session id should be a string")
        .parse::<Uuid>()
        .expect("child session id should be a uuid");
    let child_threads = state
        .storage
        .list_by_session(child_session_id)
        .await
        .expect("list child threads before release");
    assert_eq!(child_threads.len(), 1, "expected one child thread");
    let child_turns = state
        .storage
        .list_turns_by_thread(child_threads[0].id)
        .await
        .expect("list child turns before release");
    assert_eq!(child_turns.len(), 1, "expected one child turn");
    assert!(
        state
            .storage
            .get_snapshot(child_turns[0].id)
            .await
            .expect("read child context snapshot before release")
            .is_some(),
        "delegated assembly snapshot should be persisted before provider execution finishes"
    );

    child_release.notify_one();

    let mut result_messages = Vec::new();
    for _ in 0..20 {
        result_messages = state
            .storage
            .list_by_correlation(delegate_message_id)
            .await
            .expect("list correlated messages after release");
        if !result_messages.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert_eq!(result_messages.len(), 1, "expected one result message");
    let result_message = &result_messages[0];
    assert_eq!(result_message.payload["response"], child_marker);
    let pre_turn_result_status = result_message.status.clone();

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Continue after the detached child completed".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("parent next turn should complete");

    let child_turns = state
        .storage
        .list_turns_by_thread(child_threads[0].id)
        .await
        .expect("list child turns");
    assert_eq!(child_turns.len(), 1, "expected one child turn");
    assert_eq!(child_turns[0].state, TurnState::Completed);
    assert_eq!(child_turns[0].parent_turn_id, Some(root_turn.id));

    let captured = captured_requests.lock().unwrap();
    assert!(
        captured.len() >= 4,
        "expected initial parent, detached child, resumed parent, and next-turn requests: {captured:?}"
    );
    let next_turn_request = captured.last().expect("next-turn provider request");
    assert!(
        next_turn_request.contains(&child_marker),
        "completed detached result marker {child_marker} absent from parent next turn while result row status was {pre_turn_result_status:?}; request: {next_turn_request}"
    );
    let delivered_result = state
        .storage
        .get_agent_message(result_message.id)
        .await
        .expect("get result after parent next turn")
        .expect("result should still exist");
    assert_eq!(delivered_result.status, MessageStatus::Delivered);
    assert!(delivered_result.delivered_at.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn delegated_read_only_location_approval_allows_read_and_refuses_write() {
    let fixture = tempfile::tempdir().expect("placement fixture");
    let attachment = fixture.path().join("read-only");
    std::fs::create_dir(&attachment).expect("read-only attachment");
    std::fs::write(attachment.join("readable.txt"), "delegated readable proof")
        .expect("readable fixture");
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: Some("start delegated read-only work".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");
    let provider = MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "delegated-read".into(),
                name: "file_read".into(),
                arguments: serde_json::json!({"path": "readable.txt"}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "delegated-write".into(),
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "blocked.txt",
                    "content": "must not be written",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "read succeeded and write was refused".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(SandboxedFileTool::new("file_read")) as Arc<dyn Tool>,
            Arc::new(SandboxedFileTool::new("file_write")) as Arc<dyn Tool>,
        ]),
        None,
        fixture.path().join("swage-home"),
    )
    .with_worktree_root(fixture.path().join("worker-worktrees"))
    .with_test_sandbox();
    state
        .session_approval_grants
        .lock()
        .expect("location approval grants")
        .insert(crate::state::ApprovalGrantKey::location(
            None,
            Some(space.id),
            attachment.to_string_lossy(),
            "read",
        ));

    let outcome = GatewayDelegationHandler::new(state.clone())
        .handle_delegation(
            "agent-b",
            "Exercise read-only placement",
            None,
            None,
            Some(attachment.to_string_lossy().into_owned()),
            None,
            None,
            None,
            DispatchContextComposition::default(),
            "read-only-placement-delegate",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
        .expect("read-only delegation should complete");
    assert!(matches!(outcome, DelegationOutcome::Completed(_)));

    let child_session = storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list child sessions")
        .into_iter()
        .next()
        .expect("delegated child session");
    let child_thread = storage
        .list_by_session(child_session.id)
        .await
        .expect("list child threads")
        .into_iter()
        .next()
        .expect("delegated child thread");
    let child_turn = storage
        .list_turns_by_thread(child_thread.id)
        .await
        .expect("list child turns")
        .into_iter()
        .next()
        .expect("delegated child turn");
    let calls = storage
        .list_tool_calls_by_turn(child_turn.id)
        .await
        .expect("list delegated tool calls");
    assert_eq!(
        calls.len(),
        2,
        "read and write calls should both be audited"
    );
    assert_eq!(calls[0].tool, "file_read");
    assert_eq!(calls[0].status.as_deref(), Some("success"));
    assert!(
        calls[0]
            .result
            .as_deref()
            .is_some_and(|result| result.contains("delegated readable proof")),
        "read should succeed inside the delegated attachment: {calls:?}"
    );
    assert_eq!(calls[1].tool, "file_write");
    assert_eq!(calls[1].status.as_deref(), Some("error"));
    assert!(
        !attachment.join("blocked.txt").exists(),
        "delegated read-only placement must reject the sandboxed write"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_named_child_is_delegated_to_again_with_everything_it_was_told_before() {
    let _swage_home = SwageHomeGuard::isolate();
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "agent-a".into(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create parent session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "agent-a".into(),
        },
    )
    .await
    .expect("create parent thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "agent-a".into(),
            state: TurnState::Processing,
            user_input: Some("delegate twice to the same child".into()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");
    for agent_id in ["agent-a", "agent-b", "agent-c"] {
        storage
            .upsert_agent_definition(swage_storage::NewAgentDefinition {
                id: agent_id.into(),
                space_id: Some(space.id),
                display_name: agent_id.into(),
                description: String::new(),
                instructions: String::new(),
                model_id: None,
                tool_permissions: Some(serde_json::json!({
                    "allow": ["delegate"], "deny": [], "bundles": {},
                    "require_approval": []
                })),
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            })
            .await
            .expect("store agent definition");
    }
    let (provider, captured_requests) = CapturingMultiRoundProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "the eastern approach is clear".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "as I said, the eastern approach".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn Tool>
        ]),
        None,
    );
    let handler = GatewayDelegationHandler::new(state.clone());

    handler
        .handle_delegation(
            "agent-b",
            "scout the eastern approach",
            None,
            None,
            None,
            None,
            None,
            Some("scout"),
            DispatchContextComposition::default(),
            "delegate-first",
            "agent-a",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
        .expect("first delegation completes");

    let sessions_after_first = storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list child sessions");
    assert_eq!(sessions_after_first.len(), 1);
    let child_session_id = sessions_after_first[0].id;

    handler
        .handle_delegation(
            "agent-b",
            "now report what you found",
            None,
            None,
            None,
            None,
            None,
            Some("scout"),
            DispatchContextComposition::default(),
            "delegate-second",
            "agent-a",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
        .expect("second delegation completes");

    let sessions_after_second = storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list child sessions");
    assert_eq!(
        sessions_after_second.len(),
        1,
        "naming a child already delegated to continues it rather than opening a second"
    );
    assert_eq!(sessions_after_second[0].id, child_session_id);

    let requests = captured_requests.lock().expect("captured requests");
    assert_eq!(requests.len(), 2, "two child turns ran");
    let second = requests[1]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second.contains("scout the eastern approach")
            && second.contains("the eastern approach is clear"),
        "the returned-to child still sees what it was told and what it answered: {second}"
    );
    drop(requests);

    let parent = storage
        .get_session(session.id)
        .await
        .expect("session query")
        .expect("parent session");
    let registered = parent
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(DELEGATED_CHILDREN_METADATA_KEY))
        .and_then(|children| children.get("scout"))
        .expect("the parent records the child it named");
    assert_eq!(registered["agent_id"], "agent-b");
    assert_eq!(registered["session_id"], child_session_id.to_string());

    let mismatch = handler
        .handle_delegation(
            "agent-c",
            "take over the scouting",
            None,
            None,
            None,
            None,
            None,
            Some("scout"),
            DispatchContextComposition::default(),
            "delegate-mismatch",
            "agent-a",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await;
    let mismatch = match mismatch {
        Err(error) => error,
        Ok(_) => panic!("a name already used for another agent must be refused"),
    };
    assert!(
        mismatch.to_string().contains("scout"),
        "the refusal names the child: {mismatch}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_call_tool_bound_survives_delegation_and_child_continuation() {
    let _swage_home = SwageHomeGuard::isolate();
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "agent-a".into(),
            metadata: Some(serde_json::json!({
                WORKFLOW_CALL_TOOLS_METADATA_KEY: ["delegate"],
            })),
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create workflow worker session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "agent-a".into(),
        },
    )
    .await
    .expect("create workflow worker thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "agent-a".into(),
            state: TurnState::Processing,
            user_input: Some("delegate within the workflow call bound".into()),
            parent_turn_id: None,
        })
        .await
        .expect("create workflow worker turn");
    for agent_id in ["agent-a", "agent-b"] {
        storage
            .upsert_agent_definition(swage_storage::NewAgentDefinition {
                id: agent_id.into(),
                space_id: Some(space.id),
                display_name: agent_id.into(),
                description: String::new(),
                instructions: String::new(),
                model_id: None,
                tool_permissions: Some(serde_json::json!({
                    "allow": ["delegate", "shell"], "deny": [], "bundles": {},
                    "require_approval": []
                })),
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            })
            .await
            .expect("store broad agent definition");
    }
    let (provider, captured_requests) = CapturingMultiRoundProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "delegated child complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "delegated child continued".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn Tool>,
            Arc::new(ShellMockTool) as Arc<dyn Tool>,
        ]),
        None,
    );

    let outcome = GatewayDelegationHandler::new(state.clone())
        .handle_delegation(
            "agent-b",
            "complete the delegated work",
            None,
            None,
            None,
            None,
            None,
            None,
            DispatchContextComposition::default(),
            "workflow-bounded-delegate",
            "agent-a",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
        .expect("workflow-bounded delegation should complete");
    assert!(matches!(outcome, DelegationOutcome::Completed(_)));

    let child_session = storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list delegated sessions")
        .into_iter()
        .next()
        .expect("delegated child session");
    assert_eq!(
        child_session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(WORKFLOW_CALL_TOOLS_METADATA_KEY)),
        Some(&serde_json::json!(["delegate"])),
        "the delegated session must persist its caller's workflow tool bound"
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        child_session.id,
        None,
        TurnInput::from("continue the delegated work"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("delegated child continuation should complete");

    let captured_requests = captured_requests.lock().unwrap();
    assert_eq!(captured_requests.len(), 2);
    for request in captured_requests.iter() {
        assert!(
            request
                .tool_definitions
                .iter()
                .any(|definition| definition["name"] == "delegate"),
            "the workflow call's delegated child should retain delegate"
        );
        assert!(
            request
                .tool_definitions
                .iter()
                .all(|definition| definition["name"] != "shell"),
            "the broad sender and target definitions must not restore shell"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn delegated_skill_selection_is_carried_by_the_composition_once() {
    let swage_home = tempfile::TempDir::new().expect("temp swage home");
    for skill in ["validated-skill", "composition-skill"] {
        let skill_dir = swage_home.path().join("spaces/default/skills").join(skill);
        std::fs::create_dir_all(&skill_dir).expect("create delegated skill directory");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\ndescription: {skill}\n---\nUse {skill}.\n"),
        )
        .expect("write delegated skill");
    }
    let _guard = SwageHomeGuard::set_to(swage_home.path());
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".into(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("parent session");
    let thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: "gateway".into(),
        },
    )
    .await
    .expect("parent thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("delegate with a selected skill".into()),
            parent_turn_id: None,
        })
        .await
        .expect("parent turn");
    storage
        .upsert_agent_definition(swage_storage::NewAgentDefinition {
            id: "agent-b".into(),
            space_id: Some(space.id),
            display_name: "Agent B".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: None,
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("target agent definition");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(MockProvider::new(vec![
            StreamChunk::Delta {
                content: "delegated".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let outcome = GatewayDelegationHandler::new(state)
        .handle_delegation(
            "agent-b",
            "carry the composition",
            None,
            None,
            None,
            None,
            Some(vec!["validated-skill".into()]),
            DispatchContextComposition {
                skills: Some(vec!["composition-skill".into()]),
                ..Default::default()
            },
            "composition-skill-owner",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Detach,
        )
        .await
        .expect("detached delegation should start");
    let detached = match outcome {
        DelegationOutcome::Detached(detached) => detached,
        DelegationOutcome::Completed(_) => panic!("expected detached delegation"),
    };
    let message = storage
        .get_agent_message(detached.delegate_message_id)
        .await
        .expect("delegate message query")
        .expect("durable delegate message");
    assert_eq!(
        message.payload["skills"],
        serde_json::json!(["composition-skill"]),
        "the composition must remain the sole delegate-payload carrier"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn task_scoped_delegate_keeps_its_permission_bound_when_delegating_again() {
    let fixture = tempfile::tempdir().expect("delegation fixture");
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    storage
        .upsert_task_definition(swage_storage::NewTaskDefinition {
            id: "delegation-only".into(),
            instructions: "Delegate the requested unit of work.".into(),
            model_id: None,
            tool_permissions: Some(serde_json::json!({
                "allow": ["delegate"], "deny": [], "bundles": {}, "require_approval": []
            })),
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store task permission overlay");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: Some("start task-scoped delegated work".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");
    let provider = MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "nested-task-delegate".into(),
                name: "delegate".into(),
                arguments: serde_json::json!({
                    "target_agent": "agent-c",
                    "task": "Try a file write",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "task-bounded-write".into(),
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "must-not-exist.txt",
                    "content": "must not be written",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "write was refused".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "nested delegation complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn Tool>,
            Arc::new(SandboxedFileTool::new("file_write")) as Arc<dyn Tool>,
        ]),
        None,
        fixture.path().join("swage-home"),
    )
    .with_worktree_root(fixture.path().join("worker-worktrees"))
    .with_test_sandbox();

    let outcome = GatewayDelegationHandler::new(state)
        .handle_delegation(
            "agent-b",
            "Delegate once more",
            Some("delegation-only"),
            None,
            None,
            None,
            None,
            None,
            DispatchContextComposition::default(),
            "outer-task-delegate",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
        .expect("nested delegation should complete");
    assert!(matches!(outcome, DelegationOutcome::Completed(_)));

    let nested_session = storage
        .list_sessions_by_agent_id("agent-c")
        .await
        .expect("list nested sessions")
        .into_iter()
        .next()
        .expect("nested child session");
    assert_eq!(
        nested_session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(DISPATCH_BOUND_TASK_METADATA_KEY))
            .and_then(serde_json::Value::as_array),
        Some(&vec![serde_json::Value::String("delegation-only".into())]),
        "the sender's task overlay must become a durable descendant bound"
    );
    let nested_thread = storage
        .list_by_session(nested_session.id)
        .await
        .expect("list nested threads")
        .into_iter()
        .next()
        .expect("nested thread");
    let nested_turn = storage
        .list_turns_by_thread(nested_thread.id)
        .await
        .expect("list nested turns")
        .into_iter()
        .next()
        .expect("nested turn");
    let calls = storage
        .list_tool_calls_by_turn(nested_turn.id)
        .await
        .expect("list nested tool calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool, "file_write");
    assert_eq!(calls[0].status.as_deref(), Some("error"));
}

#[tokio::test(flavor = "multi_thread")]
async fn placed_delegate_keeps_exact_read_only_authority_when_delegating_again() {
    let fixture = tempfile::tempdir().expect("placement fixture");
    let default_workspace = fixture.path().join("default-workspace");
    let placed_workspace = fixture.path().join("placed-read-only");
    std::fs::create_dir(&default_workspace).expect("default workspace");
    std::fs::create_dir(&placed_workspace).expect("placed workspace");
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let default_attachment = storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id: space.id,
            label: "writable default".into(),
            path: default_workspace.to_string_lossy().into_owned(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .expect("grant writable default");
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id: space.id,
            label: "placed read-only".into(),
            path: placed_workspace.to_string_lossy().into_owned(),
            permissions: vec!["read".into()],
            is_trusted: true,
        })
        .await
        .expect("grant read-only placement");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: Some(default_attachment.id),
        })
        .await
        .expect("create session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: Some("start nested delegated work".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");
    let provider = MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "nested-delegate".into(),
                name: "delegate".into(),
                arguments: serde_json::json!({
                    "target_agent": "agent-c",
                    "task": "Try to write without choosing another cwd",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "nested-write".into(),
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "nested-write.txt",
                    "content": "must not be written",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "nested write was refused".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "nested delegation complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn Tool>,
            Arc::new(SandboxedFileTool::new("file_write")) as Arc<dyn Tool>,
        ]),
        None,
        fixture.path().join("swage-home"),
    )
    .with_worktree_root(fixture.path().join("worker-worktrees"))
    .with_test_sandbox();

    let outcome = GatewayDelegationHandler::new(state)
        .handle_delegation(
            "agent-b",
            "Delegate once more",
            None,
            None,
            Some(placed_workspace.to_string_lossy().into_owned()),
            None,
            None,
            None,
            DispatchContextComposition::default(),
            "outer-placed-delegate",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
        .expect("nested delegation should complete");
    assert!(matches!(outcome, DelegationOutcome::Completed(_)));

    let nested_session = storage
        .list_sessions_by_agent_id("agent-c")
        .await
        .expect("list nested sessions")
        .into_iter()
        .next()
        .expect("nested child session");
    assert_eq!(
        nested_session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("worker_placement_cwd"))
            .and_then(serde_json::Value::as_str),
        Some(placed_workspace.to_string_lossy().as_ref())
    );
    assert_eq!(
        nested_session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("worker_placement_can_write"))
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert!(
        !default_workspace.join("nested-write.txt").exists(),
        "nested delegation must not regain the writable default workspace"
    );
    assert!(
        !placed_workspace.join("nested-write.txt").exists(),
        "nested delegation must retain the placed worker's read-only authority"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delegate_worktree_provisions_isolated_cwd_and_records_resolved_placement() {
    let fixture = tempfile::tempdir().expect("worktree fixture");
    let repo = fixture.path().join("repo");
    std::fs::create_dir(&repo).expect("repository directory");
    init_test_git_repository(&repo);

    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let repo_attachment = storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id: space.id,
            label: "worktree source".into(),
            path: repo.to_string_lossy().into_owned(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .expect("grant worktree source");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: Some(repo_attachment.id),
        })
        .await
        .expect("create session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: Some("start isolated delegated work".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");
    let provider = MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "write-in-wait-worktree".into(),
                name: "delegated_marker_write".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "wait-mode isolated write complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "write-in-worktree".into(),
                name: "delegated_marker_write".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "isolated write complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let worktree_root = fixture.path().join("worker-worktrees");
    let mut config = swage_config::ResolvedConfig::default();
    config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "delegated_marker_write".into(),
        )];
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(DelegatedMarkerWriteTool) as Arc<dyn Tool>
        ]),
        None,
        fixture.path().join("swage-home"),
    )
    .with_worktree_root(worktree_root.clone());

    let wait_owner_id = delegate_worktree_owner(parent_turn.id, "wait-worktree-delegate-call");
    let wait_worktree = worktree_root
        .join(wait_owner_id.to_string())
        .join("worker-0");
    let wait_outcome = GatewayDelegationHandler::new(state.clone())
        .handle_delegation(
            "agent-b",
            "Write only in an isolated worktree and wait",
            None,
            None,
            Some(repo.to_string_lossy().into_owned()),
            Some(swage_protocol::client::workflow::WorkerIsolation::Worktree),
            None,
            None,
            DispatchContextComposition::default(),
            "wait-worktree-delegate-call",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
        .expect("wait-mode worktree delegation should complete");
    let wait_result = match wait_outcome {
        DelegationOutcome::Completed(result) => result,
        DelegationOutcome::Detached(_) => panic!("wait-mode delegation must complete"),
    };
    assert_eq!(
        wait_result["working_directory"],
        wait_worktree.to_string_lossy().as_ref(),
        "the waiting dispatcher receives the real provisioned directory"
    );
    assert!(
        storage.list_approvals().await.unwrap().is_empty(),
        "the waiting delegated turn must use its enforced grant without parking"
    );
    assert!(
        state
            .worktree_host
            .list_owner(wait_owner_id)
            .await
            .is_empty(),
        "successful wait-mode delegation must release its worktree owner"
    );
    assert!(
        !wait_worktree.exists(),
        "successful wait-mode delegation must remove its worktree"
    );

    let outcome = GatewayDelegationHandler::new(state.clone())
        .handle_delegation(
            "agent-b",
            "Write only in an isolated worktree",
            None,
            None,
            Some(repo.to_string_lossy().into_owned()),
            Some(swage_protocol::client::workflow::WorkerIsolation::Worktree),
            None,
            None,
            DispatchContextComposition::default(),
            "worktree-delegate-call",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Detach,
        )
        .await
        .expect("worktree delegation should start");
    let detached = match outcome {
        DelegationOutcome::Detached(detached) => detached,
        DelegationOutcome::Completed(_) => panic!("expected detached delegation"),
    };

    let delegate = storage
        .get_agent_message(detached.delegate_message_id)
        .await
        .expect("read delegate record")
        .expect("durable delegate record");
    let owner_id = delegate_worktree_owner(parent_turn.id, "worktree-delegate-call");
    let worktree = std::path::PathBuf::from(
        delegate.payload["placement"]["cwd"]
            .as_str()
            .expect("resolved placement path"),
    );
    assert_eq!(
        delegate.payload["placement"]["requested_cwd"],
        repo.to_string_lossy().as_ref()
    );
    assert_eq!(
        delegate.payload["placement"]["cwd"],
        worktree.to_string_lossy().as_ref()
    );
    assert_eq!(
        delegate.payload["placement"]["isolation"],
        serde_json::to_value(swage_protocol::client::workflow::WorkerIsolation::Worktree)
            .expect("serialise isolation")
    );
    assert_eq!(
        delegate.payload["placement"]["owner_id"],
        owner_id.to_string()
    );

    let mut result_messages = Vec::new();
    for _ in 0..20 {
        result_messages = storage
            .list_by_correlation(detached.delegate_message_id)
            .await
            .expect("list delegated result messages");
        if !storage.list_approvals().await.unwrap().is_empty()
            || result_messages
                .iter()
                .any(|message| message.kind == swage_core::MessageKind::Result)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        storage.list_approvals().await.unwrap().is_empty(),
        "the detached delegated turn must use its enforced grant without parking"
    );
    let result = result_messages
        .iter()
        .find(|message| {
            message.kind == swage_core::MessageKind::Result
                && message.payload.get("error").is_none()
        })
        .expect("the detached delegate should record a successful terminal result");
    assert_eq!(
        result.payload["working_directory"],
        worktree.to_string_lossy().as_ref(),
        "the durable completion reports the real provisioned directory"
    );
    for _ in 0..20 {
        if state.worktree_host.list_owner(owner_id).await.is_empty() && !worktree.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        state.worktree_host.list_owner(owner_id).await.is_empty(),
        "successful detached delegation must release its worktree owner"
    );
    assert!(
        !worktree.exists(),
        "successful detached delegation must remove its worktree"
    );
    assert!(
        !repo.join("delegated-marker").exists(),
        "delegate writes must not touch the source repository"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn detached_delegate_can_be_cancelled_through_active_registry() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: Some("start detached work".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");

    let child_started = Arc::new(Notify::new());
    let child_release = Arc::new(Notify::new());
    let (provider, _captured_requests) = DetachedGatedProvider::new(
        child_started.clone(),
        child_release,
        "Detached child answer".to_string(),
    );
    let placement_fixture = tempfile::tempdir().expect("placement fixture");
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(DelegatedMarkerWriteTool) as Arc<dyn Tool>
        ]),
        None,
        placement_fixture.path().join("swage-home"),
    )
    .with_worktree_root(placement_fixture.path().join("worker-worktrees"));
    let handler = GatewayDelegationHandler::new(state.clone());
    let delegated_cwd = tempfile::tempdir().expect("delegated cwd");
    init_test_git_repository(delegated_cwd.path());
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id: space.id,
            label: "delegated cwd".into(),
            path: delegated_cwd.path().to_string_lossy().into_owned(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .expect("grant delegated cwd");

    let outcome = handler
        .handle_delegation(
            "agent-b",
            "Summarise independently",
            None,
            None,
            Some(delegated_cwd.path().to_string_lossy().into_owned()),
            Some(swage_protocol::client::workflow::WorkerIsolation::Worktree),
            None,
            None,
            DispatchContextComposition::default(),
            "detached-placement-delegate",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Detach,
        )
        .await
        .expect("detached delegation should start");
    let detached = match outcome {
        DelegationOutcome::Detached(detached) => detached,
        DelegationOutcome::Completed(_) => panic!("expected detached outcome"),
    };

    tokio::time::timeout(std::time::Duration::from_secs(1), child_started.notified())
        .await
        .expect("detached child should enter provider stream");
    let worktree_owner_id = delegate_worktree_owner(parent_turn.id, "detached-placement-delegate");
    let provisioned = state.worktree_host.list_owner(worktree_owner_id).await;
    assert_eq!(provisioned.len(), 1, "cancelled delegate worktree owner");
    let delegated_worktree = provisioned[0].path.clone();
    assert_eq!(
        std::fs::read_to_string(delegated_worktree.join("delegated-marker"))
            .expect("delegated tool should write at the resolved placement"),
        "placed"
    );

    let denied_cwd = tempfile::tempdir().expect("denied cwd");
    let denied = match handler
        .handle_delegation(
            "agent-b",
            "This placement must be rejected",
            None,
            None,
            Some(denied_cwd.path().to_string_lossy().into_owned()),
            None,
            None,
            None,
            DispatchContextComposition::default(),
            "denied-placement-delegate",
            "gateway",
            parent_turn.id,
            space.id,
            CancellationToken::new(),
            DelegateCompletionMode::Wait,
        )
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("delegation must not expand the sender's filesystem authority"),
    };
    assert!(
        denied
            .to_string()
            .contains("outside the dispatching context's permitted workspace roots"),
        "unexpected placement rejection: {denied}"
    );

    assert!(
        handler
            .active_delegations
            .cancellation_target(detached.delegate_message_id)
            .await
            .is_some(),
        "detached child should be registered for precise cancellation"
    );

    let cancel = storage
        .create_agent_message(NewAgentMessage {
            sender_agent_id: "gateway".to_string(),
            recipient_agent_id: "agent-b".to_string(),
            kind: swage_core::MessageKind::Cancel,
            correlation_id: Some(detached.delegate_message_id),
            payload: serde_json::json!({}),
        })
        .await
        .expect("create cancel message");
    swage_agent::cancel::handle_cancel(
        storage.as_ref(),
        &cancel,
        Some(&handler.active_delegations),
    )
    .await
    .expect("active-registry cancel should succeed");

    let mut child_turns = Vec::new();
    for _ in 0..20 {
        child_turns = state
            .storage
            .list_turns_by_thread(detached.child_thread_id)
            .await
            .expect("list child turns");
        if child_turns
            .iter()
            .any(|turn| turn.state == TurnState::Interrupted)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert_eq!(child_turns.len(), 1, "expected one child turn");
    assert_eq!(child_turns[0].state, TurnState::Interrupted);

    let delegate = state
        .storage
        .get_agent_message(detached.delegate_message_id)
        .await
        .expect("get delegate message")
        .expect("delegate message should exist");
    assert_eq!(delegate.status, MessageStatus::Delivered);
    assert_eq!(
        delegate.payload["placement"]["cwd"],
        delegated_worktree.to_string_lossy().as_ref(),
        "the child's durable delegate record must carry the resolved placement"
    );
    assert_eq!(
        delegate.payload["placement"]["requested_cwd"],
        delegated_cwd.path().to_string_lossy().as_ref()
    );
    assert_eq!(
        delegate.payload["placement"]["owner_id"],
        worktree_owner_id.to_string()
    );

    let mut result_messages = Vec::new();
    for _ in 0..20 {
        result_messages = state
            .storage
            .list_by_correlation(detached.delegate_message_id)
            .await
            .expect("list correlated messages after cancellation");
        if result_messages
            .iter()
            .any(|message| message.payload["error"] == "turn cancelled")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let cancellation_results = result_messages
        .iter()
        .filter(|message| message.kind == swage_core::MessageKind::Result)
        .collect::<Vec<_>>();
    assert_eq!(
        cancellation_results.len(),
        1,
        "expected one cancellation result"
    );
    assert_eq!(cancellation_results[0].payload["error"], "turn cancelled");

    for _ in 0..20 {
        if handler
            .active_delegations
            .cancellation_target(detached.delegate_message_id)
            .await
            .is_none()
        {
            assert_eq!(
                state
                    .worktree_host
                    .list_owner(worktree_owner_id)
                    .await
                    .len(),
                1,
                "cancelled delegation must retain its worktree until shutdown"
            );
            assert!(delegated_worktree.exists());
            state.worktree_host.shutdown().await;
            assert!(!delegated_worktree.exists());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("detached delegation registry entry should be removed after cancellation");
}

// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Space context and config resolution tests
// -----------------------------------------------------------------------

struct CapturingProvider {
    captured: Arc<std::sync::Mutex<Option<CompletionRequest>>>,
    chunks: Vec<StreamChunk>,
}

impl CapturingProvider {
    fn new(chunks: Vec<StreamChunk>) -> (Self, Arc<std::sync::Mutex<Option<CompletionRequest>>>) {
        let captured = Arc::new(std::sync::Mutex::new(None));
        (
            Self {
                captured: Arc::clone(&captured),
                chunks,
            },
            captured,
        )
    }
}

#[async_trait]
impl LlmProvider for CapturingProvider {
    fn model_id(&self) -> &str {
        "capturing-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!()
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!()
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        *self.captured.lock().unwrap() = Some(request);
        Box::pin(stream::iter(self.chunks.clone()))
    }
}

struct ContextCaptureTool {
    captured: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
}

#[async_trait::async_trait]
impl crate::tool::Tool for ContextCaptureTool {
    fn name(&self) -> &str {
        "capture_context"
    }
    fn description(&self) -> &str {
        "Captures the working_directory from the tool context"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        *self.captured.lock().unwrap() = Some(ctx.working_directory().to_path_buf());
        Ok(serde_json::json!({"output": "captured"}))
    }
}

/// Create a storage with a session + thread and return (storage, session_id).
async fn make_storage_with_session() -> (Arc<MockStorage>, Uuid) {
    make_storage_with_session_metadata(None).await
}

async fn make_storage_with_session_metadata(
    metadata: Option<serde_json::Value>,
) -> (Arc<MockStorage>, Uuid) {
    let storage = Arc::new(MockStorage::new());
    let space = storage.get_or_create_default().await.expect("space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("session");
    let session_id = session.id;
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("thread");
    (storage, session_id)
}

async fn capture_provider_effort_for_session(
    metadata: Option<serde_json::Value>,
    config: swage_config::ResolvedConfig,
) -> Option<swage_core::Effort> {
    Box::pin(capture_provider_effort_for_selected_model(
        metadata, None, config,
    ))
    .await
}

async fn capture_provider_effort_for_selected_model(
    metadata: Option<serde_json::Value>,
    selected_model_id: Option<String>,
    config: swage_config::ResolvedConfig,
) -> Option<swage_core::Effort> {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session_metadata(metadata).await;
    if selected_model_id.is_some() {
        storage
            .set_session_selected_model(session_id, selected_model_id)
            .await
            .expect("select session model");
    }
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hi"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider should receive the turn request")
        .effort
        .map(|resolved| resolved.requested)
}

#[tokio::test]
async fn process_turn_forwards_persisted_session_effort_to_provider() {
    assert_eq!(
        capture_provider_effort_for_session(
            Some(serde_json::json!({"effort": "high"})),
            swage_config::ResolvedConfig::default(),
        )
        .await,
        Some(swage_core::Effort::High)
    );
    assert_eq!(
        capture_provider_effort_for_session(None, swage_config::ResolvedConfig::default()).await,
        None
    );
}

#[tokio::test]
async fn process_turn_uses_effective_models_default_effort_without_session_metadata() {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "model-a".to_string();
    config.models.entries.insert(
        "model-a".to_string(),
        swage_config::ModelEndpointConfig {
            default_effort: Some(swage_core::Effort::Medium),
            ..Default::default()
        },
    );

    assert_eq!(
        capture_provider_effort_for_session(None, config).await,
        Some(swage_core::Effort::Medium)
    );
}

#[tokio::test]
async fn process_turn_omits_default_effort_for_unknown_selected_model() {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "model-a".to_string();
    config.models.entries.insert(
        "model-a".to_string(),
        swage_config::ModelEndpointConfig {
            default_effort: Some(swage_core::Effort::Medium),
            ..Default::default()
        },
    );

    assert_eq!(
        Box::pin(capture_provider_effort_for_selected_model(
            None,
            Some("unknown-model".to_string()),
            config,
        ))
        .await,
        None
    );
}

#[tokio::test]
async fn process_turn_uses_selected_model_default_effort_without_session_metadata() {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "model-a".to_string();
    config.models.entries.insert(
        "model-a".to_string(),
        swage_config::ModelEndpointConfig::default(),
    );
    config.models.entries.insert(
        "model-b".to_string(),
        swage_config::ModelEndpointConfig {
            default_effort: Some(swage_core::Effort::Medium),
            ..Default::default()
        },
    );

    assert_eq!(
        Box::pin(capture_provider_effort_for_selected_model(
            None,
            Some("model-b".to_string()),
            config,
        ))
        .await,
        Some(swage_core::Effort::Medium)
    );
}

#[tokio::test]
async fn process_turn_uses_aliased_model_default_effort_without_session_metadata() {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "model-a".to_string();
    config.models.entries.insert(
        "model-a".to_string(),
        swage_config::ModelEndpointConfig {
            default_effort: Some(swage_core::Effort::Low),
            ..Default::default()
        },
    );
    config.models.entries.insert(
        "model-b".to_string(),
        swage_config::ModelEndpointConfig {
            aliases: vec!["friendly-b".to_string()],
            default_effort: Some(swage_core::Effort::Medium),
            ..Default::default()
        },
    );

    assert_eq!(
        Box::pin(capture_provider_effort_for_selected_model(
            None,
            Some("friendly-b".to_string()),
            config,
        ))
        .await,
        Some(swage_core::Effort::Medium)
    );
}

/// The effort exemplar's turn seam: the intent resolves once against the
/// effective model's declared capability, the provider receives the resolved
/// control, and the turn's audit snapshot records the outcome — a clamp
/// included.
#[tokio::test]
async fn process_turn_resolves_effort_against_the_declared_capability_and_records_it() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) =
        make_storage_with_session_metadata(Some(serde_json::json!({"effort": "max"}))).await;
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "local".into();
    config.models.entries.insert(
        "local".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "local".into(),
            model_id: "local-reasoner".into(),
            capabilities: swage_config::ModelCapabilitiesConfig {
                reasoning_replay: false,
                effort: Some(swage_core::EffortCapability::Levels {
                    levels: vec![swage_core::Effort::Low, swage_core::Effort::High],
                    can_disable: false,
                }),
            },
            ..Default::default()
        },
    );
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hi"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let resolved = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider request")
        .effort
        .expect("effort reached the provider");
    assert_eq!(resolved.requested, swage_core::Effort::Max);
    assert_eq!(
        resolved.control,
        swage_core::EffortControl::Level {
            level: swage_core::Effort::High
        }
    );
    assert_eq!(resolved.clamped_to, Some(swage_core::Effort::High));

    let snapshot = storage
        .db
        .lock()
        .await
        .snapshots
        .values()
        .next()
        .map(|(report, _)| report.clone())
        .expect("context snapshot recorded");
    let report: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(report["provider_visible"]["effort"]["requested"], "max");
    assert_eq!(report["provider_visible"]["effort"]["clamped_to"], "high");
    assert_eq!(
        report["provider_visible"]["effort"]["note"],
        "max unsupported; clamped to high"
    );
}

/// With no catalogue declaration, the provider adapter's own account of the
/// documented control resolves the intent.
#[test]
fn effort_capability_falls_back_from_the_catalogue_to_the_adapter() {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "claude".into();
    config.models.entries.insert(
        "claude".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "anthropic".into(),
            model_id: "claude-opus-5".into(),
            ..Default::default()
        },
    );
    let anthropic = swage_provider::anthropic::AnthropicProvider::new("key", "claude-opus-5");
    let capability = super::effort_capability_for_model(&config, "claude", &anthropic);
    assert!(matches!(
        capability,
        swage_core::EffortCapability::Levels {
            can_disable: true,
            ..
        }
    ));

    let declared = swage_core::EffortCapability::None;
    config
        .models
        .entries
        .get_mut("claude")
        .unwrap()
        .capabilities
        .effort = Some(declared.clone());
    assert_eq!(
        super::effort_capability_for_model(&config, "claude", &anthropic),
        declared,
        "the catalogue's own declaration wins over the adapter's"
    );

    let mock = MockProvider::new(vec![]);
    assert_eq!(
        super::effort_capability_for_model(&config, "unknown", &mock),
        swage_core::EffortCapability::None
    );
}

#[tokio::test]
async fn process_turn_passes_tool_context_to_tools() {
    let captured = Arc::new(std::sync::Mutex::new(None));
    let capture_tool = Arc::new(ContextCaptureTool {
        captured: Arc::clone(&captured),
    });

    let (storage, session_id) = make_storage_with_session().await;

    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "cc_1".into(),
            name: "capture_context".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![capture_tool as Arc<dyn crate::tool::Tool>]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("capture"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let working_dir = captured
        .lock()
        .unwrap()
        .take()
        .expect("ToolContext should have been passed during tool execution");

    assert!(
        working_dir.is_absolute(),
        "working_directory should be absolute: {working_dir:?}"
    );
}

#[tokio::test]
async fn process_turn_resolves_config_per_space_and_completes_turn() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);

    let storage = Arc::new(MockStorage::new());
    let space = SpaceStore::create(
        &*storage,
        swage_storage::model::NewSpace {
            name: "Test Config Space".to_string(),
            slug: "test-config-resolution-no-space-toml".into(),
        },
    )
    .await
    .expect("create test space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("session");
    let session_id = session.id;
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("thread");

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hi"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    // Verify the provider was invoked. No configured model exists in this
    // test, so the request should still leave provider defaults in charge.
    let req = captured_request.lock().unwrap().take();
    assert!(
        req.is_some(),
        "provider should have been invoked for config-resolved turn"
    );
    assert_eq!(req.and_then(|request| request.model), None);
}

#[tokio::test]
async fn process_turn_uses_hot_swapped_runtime_config_for_next_turn() {
    let _guard = SwageHomeGuard::isolate();
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let runtime_config = swage_config::ResolvedConfig {
        models: swage_config::ModelCatalogConfig {
            default: "fast".to_string(),
            entries: [(
                "fast".to_string(),
                swage_config::ModelEndpointConfig {
                    provider_id: "local".to_string(),
                    model_id: "fast-wire".to_string(),
                    aliases: vec![],
                    context_window: None,
                    generation_preset: Some("quick".to_string()),
                    default_effort: None,
                    capabilities: swage_config::ModelCapabilitiesConfig::default(),
                },
            )]
            .into_iter()
            .collect(),
        },
        generation: swage_config::GenerationConfig {
            presets: [(
                "quick".to_string(),
                swage_config::GenerationPresetConfig {
                    max_tokens: Some(123),
                    temperature: Some(0.2),
                    stop_sequences: vec!["STOP".to_string()],
                    metadata: Some(serde_json::json!({"preset": "quick"})),
                },
            )]
            .into_iter()
            .collect(),
        },
        context: swage_config::ContextConfig {
            assembly: "default".to_string(),
            alternatives: Vec::new(),
            timeout: 7,
            budget: Some(2048),
        },
        ..Default::default()
    };
    state.replace_runtime(
        runtime_config,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hi"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let request = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider should receive the new-turn request");
    assert_eq!(request.model.as_deref(), Some("fast-wire"));
    assert_eq!(request.max_tokens, Some(123));
    assert_eq!(request.temperature, Some(0.2));
    assert_eq!(request.stop_sequences, Some(vec!["STOP".to_string()]));
    assert_eq!(
        request.metadata,
        Some(serde_json::json!({"preset": "quick"}))
    );
}

#[tokio::test]
async fn invalid_call_scoped_assembly_is_rejected_before_provider_dispatch() {
    let _guard = SwageHomeGuard::isolate();
    let (provider, captured_request) = CapturingProvider::new(vec![]);
    let (storage, session_id) = make_storage_with_session().await;
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    let error = process_turn_with_composition(
        &state,
        session_id,
        None,
        TurnInput::from("hi"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Attended,
        DispatchContextComposition {
            assembly: Some("../../outside".into()),
            ..Default::default()
        },
        None,
    )
    .await
    .expect_err("unsafe call-scoped assembly must fail the turn");

    assert!(error.to_string().contains("invalid context assembly name"));
    assert!(
        captured_request.lock().unwrap().is_none(),
        "the provider must not run for an unsafe assembly override"
    );
}

#[tokio::test]
async fn unknown_forced_context_assembly_rejects_call_scoped_override() {
    let _guard = SwageHomeGuard::isolate();
    let (provider, captured_request) = CapturingProvider::new(vec![]);
    let (storage, session_id) = make_storage_with_session().await;
    let mut config = swage_config::ResolvedConfig::default();
    config.experimentation.locations.insert(
        crate::experimentation::LOCATION_CONTEXT_ASSEMBLY.to_owned(),
        swage_config::model::ExperimentLocationConfig {
            enabled: true,
            forced_variant: Some("missing".to_owned()),
        },
    );
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    let error = process_turn_with_composition(
        &state,
        session_id,
        None,
        TurnInput::from("hi"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Attended,
        DispatchContextComposition {
            assembly: Some("default".into()),
            ..Default::default()
        },
        None,
    )
    .await
    .expect_err("unknown forced assembly must fail the turn");

    assert_eq!(
        error.to_string(),
        "config error: experimentation location 'context.assembly' forced unknown variant 'missing'; available variants: [\"default\"]"
    );
    assert!(
        captured_request.lock().unwrap().is_none(),
        "the provider must not run for a rejected force"
    );
    let audit = state
        .storage
        .list_audit_events_by_session(session_id)
        .await
        .expect("list audit events");
    assert!(
        !audit.iter().any(|event| {
            event.entity_id == crate::experimentation::LOCATION_CONTEXT_ASSEMBLY
                && event.event_kind == "variant_selected"
        }),
        "a rejected force must not create a phantom selection"
    );
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "context-assembly selection failure must release the turn guard"
    );
}

#[tokio::test]
async fn registered_context_assembly_alternative_can_be_selected_and_is_audited() {
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage(vec![], Arc::clone(&storage)).await;
    let mut config = swage_config::ResolvedConfig::default();
    config.context.alternatives = vec!["candidate".to_owned()];

    let choices = select_context_assembly(&state, &config, session_id, Uuid::from_u128(1), None)
        .await
        .expect("registered alternative should be selectable");

    assert_eq!(choices.assembly_name, "candidate");
    let audit = state
        .storage
        .list_audit_events_by_session(session_id)
        .await
        .unwrap();
    assert!(audit.iter().any(|event| {
        event.entity_id == crate::experimentation::LOCATION_CONTEXT_ASSEMBLY
            && event.metadata["variant"] == serde_json::json!("candidate")
            && event.metadata["reason"] == serde_json::json!("drawn")
    }));
}

#[tokio::test]
async fn forced_default_context_assembly_runs_and_records_forced_reason() {
    let _guard = SwageHomeGuard::isolate();
    let chunks = vec![StreamChunk::Done {
        finish_reason: None,
    }];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;
    let mut config = swage_config::ResolvedConfig::default();
    config.experimentation.locations.insert(
        crate::experimentation::LOCATION_CONTEXT_ASSEMBLY.to_owned(),
        swage_config::model::ExperimentLocationConfig {
            enabled: true,
            forced_variant: Some("default".to_owned()),
        },
    );
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hi"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("forcing the registered default should succeed");

    assert!(
        captured_request.lock().unwrap().is_some(),
        "the provider should run for a valid force"
    );
    let audit = state
        .storage
        .list_audit_events_by_session(session_id)
        .await
        .expect("list audit events");
    assert!(audit.iter().any(|event| {
        event.entity_id == crate::experimentation::LOCATION_CONTEXT_ASSEMBLY
            && event.event_kind == "variant_selected"
            && event.metadata["variant"] == serde_json::json!("default")
            && event.metadata["reason"] == serde_json::json!("forced")
    }));
}

#[tokio::test]
async fn working_norms_selection_honours_disabled_forced_and_seeded_choices() {
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage(vec![], Arc::clone(&storage)).await;

    let mut disabled = swage_config::ResolvedConfig::default();
    disabled.experimentation.enabled = false;
    let choices = select_context_assembly(&state, &disabled, session_id, Uuid::nil(), None)
        .await
        .expect("disabled experimentation should serve defaults");
    assert_eq!(
        choices.working_norms_scaffold,
        swage_context::working_norms::WorkingNormsScaffold::Native
    );

    let mut forced = swage_config::ResolvedConfig::default();
    forced.experimentation.enabled = true;
    forced.experimentation.locations.insert(
        crate::experimentation::LOCATION_WORKING_NORMS.to_owned(),
        swage_config::model::ExperimentLocationConfig {
            enabled: true,
            forced_variant: Some(WORKING_NORMS_FAMILY_FALLBACK.to_owned()),
        },
    );
    let choices = select_context_assembly(&state, &forced, session_id, Uuid::nil(), None)
        .await
        .expect("registered fallback should be forceable");
    assert_eq!(
        choices.working_norms_scaffold,
        swage_context::working_norms::WorkingNormsScaffold::FamilyFallback
    );

    forced
        .experimentation
        .locations
        .get_mut(crate::experimentation::LOCATION_WORKING_NORMS)
        .unwrap()
        .forced_variant = None;
    let native = select_context_assembly(&state, &forced, session_id, Uuid::nil(), None)
        .await
        .expect("seed zero should draw the default");
    let fallback = select_context_assembly(&state, &forced, session_id, Uuid::from_u128(1), None)
        .await
        .expect("seed one should draw the registered alternative");
    assert_eq!(
        native.working_norms_scaffold,
        swage_context::working_norms::WorkingNormsScaffold::Native
    );
    assert_eq!(
        fallback.working_norms_scaffold,
        swage_context::working_norms::WorkingNormsScaffold::FamilyFallback
    );

    let audit = state.storage.list_audit_events().await.unwrap();
    assert!(
        audit.iter().any(|event| {
            event.entity_id == crate::experimentation::LOCATION_WORKING_NORMS
                && event.metadata["reason"] == serde_json::json!("disabled")
        }),
        "missing disabled working-norms audit record: {audit:?}"
    );
    assert!(audit.iter().any(|event| {
        event.entity_id == crate::experimentation::LOCATION_WORKING_NORMS
            && event.metadata["reason"] == serde_json::json!("forced")
    }));
    assert!(audit.iter().any(|event| {
        event.entity_id == crate::experimentation::LOCATION_WORKING_NORMS
            && event.metadata["variant"] == serde_json::json!("authored-v1")
    }));
}

#[tokio::test]
async fn unrecorded_working_norms_draw_falls_back_before_content_is_chosen() {
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage(vec![], Arc::clone(&storage)).await;
    storage.fail_audit_writes();
    let mut config = swage_config::ResolvedConfig::default();
    config.experimentation.enabled = true;
    config.experimentation.locations.insert(
        crate::experimentation::LOCATION_WORKING_NORMS.to_owned(),
        swage_config::model::ExperimentLocationConfig {
            enabled: true,
            forced_variant: Some(WORKING_NORMS_FAMILY_FALLBACK.to_owned()),
        },
    );

    let choices = select_context_assembly(&state, &config, session_id, Uuid::new_v4(), None)
        .await
        .expect("record failure should fail closed to the default content");

    assert_eq!(
        choices.working_norms_scaffold,
        swage_context::working_norms::WorkingNormsScaffold::Native
    );
}

#[tokio::test]
async fn process_turn_uses_hot_swapped_runtime_tool_policy_for_next_turn() {
    let _guard = SwageHomeGuard::isolate();
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let mut runtime_config = swage_config::ResolvedConfig::default();
    runtime_config.tools.permissions.deny = vec!["file_read".to_string()];
    state.replace_runtime(
        runtime_config,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkspaceGatedReadTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hello"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let request = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider should receive the new-turn request");
    assert!(
        request.tool_definitions.is_empty(),
        "hot-applied deny policy should hide file_read from the next turn"
    );
}

#[tokio::test]
async fn process_turn_stream_request_includes_registered_tool_definitions() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkingDirReadTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hello"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let req = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider request should be captured");
    assert_eq!(req.tool_definitions.len(), 1);
    assert_eq!(req.tool_definitions[0]["name"], "working_dir_read");
}

#[test]
fn effective_session_config_uses_known_selected_model_as_default() {
    let config = swage_config::ResolvedConfig {
        models: swage_config::ModelCatalogConfig {
            default: "global".to_string(),
            entries: [
                (
                    "global".to_string(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "openai".to_string(),
                        model_id: "global-wire".to_string(),
                        aliases: vec![],
                        context_window: None,
                        generation_preset: None,
                        default_effort: None,
                        capabilities: swage_config::ModelCapabilitiesConfig::default(),
                    },
                ),
                (
                    "conversation".to_string(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "openai".to_string(),
                        model_id: "conversation-wire".to_string(),
                        aliases: vec![],
                        context_window: None,
                        generation_preset: None,
                        default_effort: None,
                        capabilities: swage_config::ModelCapabilitiesConfig::default(),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        },
        ..Default::default()
    };

    let effective = effective_session_config(config, Some("conversation"));

    assert_eq!(effective.models.default, "conversation");
    assert_eq!(effective.default_model_id(), "conversation-wire");
}

#[test]
fn effective_session_config_resolves_aliased_selected_model_as_default() {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "global".to_string();
    config.models.entries.insert(
        "global".to_string(),
        swage_config::ModelEndpointConfig {
            model_id: "global-wire".to_string(),
            ..Default::default()
        },
    );
    config.models.entries.insert(
        "conversation".to_string(),
        swage_config::ModelEndpointConfig {
            model_id: "conversation-wire".to_string(),
            aliases: vec!["friendly-conversation".to_string()],
            ..Default::default()
        },
    );

    let effective = effective_session_config(config, Some("friendly-conversation"));

    assert_eq!(effective.models.default, "conversation");
    assert_eq!(effective.default_model_id(), "conversation-wire");
}

#[tokio::test]
async fn turn_model_snapshot_records_the_effective_model_and_its_source() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "default".to_string();
    config.models.entries.insert(
        "default".to_string(),
        swage_config::ModelEndpointConfig {
            provider_id: "first-provider".to_string(),
            model_id: "default-wire".to_string(),
            ..Default::default()
        },
    );
    config.models.entries.insert(
        "alternate".to_string(),
        swage_config::ModelEndpointConfig {
            provider_id: "second-provider".to_string(),
            model_id: "alternate-wire".to_string(),
            aliases: vec!["fast".to_string()],
            ..Default::default()
        },
    );
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn_with_existing_turn_and_focus_and_model(
        &state,
        session_id,
        None,
        "hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Attended,
        None,
        Some("fast".to_string()),
    )
    .await
    .expect("selected turn should complete");

    assert_eq!(
        captured_request
            .lock()
            .expect("capture lock")
            .as_ref()
            .expect("provider receives request")
            .model
            .as_deref(),
        Some("alternate-wire")
    );
    let session = storage
        .get_session(session_id)
        .await
        .expect("get session")
        .expect("session remains");
    assert_eq!(
        session.selected_model_id, None,
        "request must not persist selection"
    );
    let thread = storage.list_by_session(session_id).await.expect("threads")[0].clone();
    let turn = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turns")[0]
        .clone();
    let snapshot: serde_json::Value = serde_json::from_str(
        &storage
            .get_snapshot(turn.id)
            .await
            .expect("read snapshot")
            .expect("turn snapshot"),
    )
    .expect("snapshot json");
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["catalogue_entry"],
        "alternate"
    );
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["model_id"],
        "alternate-wire"
    );
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["provider_id"],
        "second-provider"
    );
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["source"],
        "request"
    );

    process_turn_with_existing_turn_and_focus_and_model(
        &state,
        session_id,
        None,
        "hello again".to_string(),
        mpsc::unbounded_channel().0,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Attended,
        None,
        None,
    )
    .await
    .expect("default-model turn should complete");

    let turns = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turns");
    let default_turn = turns.last().expect("default turn");
    let default_snapshot: serde_json::Value = serde_json::from_str(
        &storage
            .get_snapshot(default_turn.id)
            .await
            .expect("read default snapshot")
            .expect("default turn snapshot"),
    )
    .expect("default snapshot json");
    assert_eq!(
        default_snapshot["provider_visible"]["effective_model"]["catalogue_entry"],
        "default"
    );
    assert_eq!(
        default_snapshot["provider_visible"]["effective_model"]["source"],
        "configured_default"
    );

    storage
        .set_session_selected_model(session_id, Some("removed-model".to_string()))
        .await
        .expect("record stale conversation model");
    process_turn_with_existing_turn_and_focus_and_model(
        &state,
        session_id,
        None,
        "hello after a catalogue change".to_string(),
        mpsc::unbounded_channel().0,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        None,
        TurnAttendance::Attended,
        None,
        None,
    )
    .await
    .expect("stale conversation-model turn should use the configured default");

    let turns = storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("turns");
    let stale_model_turn = turns.last().expect("stale conversation-model turn");
    let stale_model_snapshot: serde_json::Value = serde_json::from_str(
        &storage
            .get_snapshot(stale_model_turn.id)
            .await
            .expect("read stale conversation-model snapshot")
            .expect("stale conversation-model turn snapshot"),
    )
    .expect("stale conversation-model snapshot json");
    assert_eq!(
        stale_model_snapshot["provider_visible"]["effective_model"]["catalogue_entry"],
        "default"
    );
    assert_eq!(
        stale_model_snapshot["provider_visible"]["effective_model"]["source"],
        "configured_default"
    );
}

#[tokio::test]
async fn process_turn_stream_request_inherits_global_permissions_without_workspace_grants() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkspaceGatedReadTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        TurnInput::from("hello"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let req = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider request should be captured");
    assert_eq!(req.tool_definitions.len(), 1);
    assert_eq!(req.tool_definitions[0]["name"], "file_read");
}

#[tokio::test]
async fn text_only_turn_still_works_with_tool_registry() {
    let (state, session_id) = make_state_with_tools(vec![vec![
        StreamChunk::Delta {
            content: "Just text".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]])
    .await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Token { content } if content == "Just text"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ServerEvent::ToolStart { .. }))
    );
}

// -----------------------------------------------------------------------
// ToolContext propagation tests
// -----------------------------------------------------------------------

struct WorkingDirReadTool;

#[async_trait::async_trait]
impl crate::tool::Tool for WorkingDirReadTool {
    fn name(&self) -> &str {
        "working_dir_read"
    }
    fn description(&self) -> &str {
        "Returns the working_directory from the ToolContext"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        let path = ctx.working_directory().to_string_lossy().into_owned();
        Ok(serde_json::json!({"output": path}))
    }
}

struct WorkspaceGatedReadTool;

#[async_trait::async_trait]
impl crate::tool::Tool for WorkspaceGatedReadTool {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Represents a workspace-read tool in agent-facing schema tests"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        Ok(serde_json::json!({}))
    }
}

fn registered_remote_host(
    capabilities: Vec<String>,
    workspace_roots: Vec<std::path::PathBuf>,
) -> crate::device::RegisteredDevice {
    crate::device::RegisteredDevice {
        device_id: "device-a".into(),
        label: None,
        kind: crate::device::DeviceKind::AttachedDevice,
        base_url: Some("http://127.0.0.1:32001".into()),
        shared_token_secret_ref: Some("secret://device/device-a".into()),
        workspace_root: workspace_roots
            .first()
            .cloned()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp")),
        workspace_roots,
        capabilities,
        registered_at: chrono::Utc::now(),
    }
}

#[test]
fn gateway_advertisement_filter_hides_tools_absent_from_remote_host_capabilities() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let agent_config = AgentConfig::new("agent")
        .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
    let host = registered_remote_host(
        vec!["other_tool".into()],
        vec![workspace.path().to_path_buf()],
    );
    let filter = build_gateway_advertisement_filter(Some(&host), &agent_config)
        .expect("remote host should produce an advertisement filter");

    assert!(
        !filter(&WorkingDirReadTool),
        "tools the selected remote host cannot execute must not be advertised"
    );
}

#[test]
fn gateway_advertisement_filter_keeps_remote_capability_with_matching_workspace() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let agent_config = AgentConfig::new("agent")
        .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
    let host = registered_remote_host(
        vec!["working_dir_read".into()],
        vec![workspace.path().to_path_buf()],
    );
    let filter = build_gateway_advertisement_filter(Some(&host), &agent_config)
        .expect("remote host should produce an advertisement filter");

    assert!(
        filter(&WorkingDirReadTool),
        "remote-capable tools inside the host workspace should remain advertised"
    );
}

#[tokio::test]
async fn tool_context_working_directory_is_passed_during_tool_execution() {
    let storage = Arc::new(MockStorage::new());
    let space = SpaceStore::create(
        &*storage,
        NewSpace {
            name: "my-space".to_string(),
            slug: "my-space".into(),
        },
    )
    .await
    .expect("create space");

    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("thread");

    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "ctx-check".into(),
            name: "working_dir_read".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkingDirReadTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Check context".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    assert_eq!(tool_calls.len(), 1);
    // The agent stores the full JSON-serialised tool output.
    let result = tool_calls[0].result.as_deref().unwrap_or("");
    let result_json: serde_json::Value =
        serde_json::from_str(result).expect("tool result should be valid JSON");
    let path_str = result_json["output"]
        .as_str()
        .expect("output field should be a string");
    assert!(
        std::path::Path::new(path_str).is_absolute(),
        "working_directory should be an absolute path, got: {path_str}"
    );
}

// -----------------------------------------------------------------------
// Turn ID correlation span test
// -----------------------------------------------------------------------

/// The `process_turn` function creates a tracing span named "turn" carrying
/// `turn_id` and `session_id` fields so that all log entries within the turn
/// are correlated by the same identifiers.
///
/// Uses a custom `Layer` that records span metadata to an in-process buffer,
/// avoiding external log capture and filesystem dependencies.
#[test]
fn process_turn_creates_correlation_span() {
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    // Accumulates (span_name, [field_names]) for every span created
    // during process_turn. Shared between the subscriber and the assertions.
    type RecordedFields = Vec<(String, Vec<String>)>;

    let recorded: StdArc<StdMutex<RecordedFields>> = StdArc::new(StdMutex::new(Vec::new()));
    let captured = StdArc::clone(&recorded);

    // Minimal Layer that records span names and field names on creation.
    struct SpanFieldRecorder(StdArc<StdMutex<RecordedFields>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanFieldRecorder {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let name = attrs.metadata().name().to_string();
            let mut fields = Vec::new();

            // Collect field names from metadata — this captures both
            // value-filled and Empty (deferred) fields, so turn_id is
            // visible even though it's recorded after span creation.
            attrs.metadata().fields().iter().for_each(|f| {
                fields.push(f.name().to_string());
            });
            self.0.lock().unwrap().push((name, fields));
        }
    }

    let subscriber = Registry::default().with(SpanFieldRecorder(captured));

    // Scope the subscriber around the entire async execution via with_default.
    // This is more reliable than set_default inside #[tokio::test] because
    // with_default brackets the runtime creation itself, ensuring the subscriber
    // is active when tracing rebuilds callsite interest caches (which happens
    // when SCOPED_COUNT transitions 0→1). A gap between set_default and
    // process_turn can let concurrent tests drain SCOPED_COUNT to 0, causing
    // tracing to rebuild with a noop subscriber and cache "never" interest —
    // so spans are silently dropped before reaching our recorder.
    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let chunks = vec![
                    StreamChunk::Delta {
                        content: "ok".to_string(),
                    },
                    StreamChunk::Done {
                        finish_reason: None,
                    },
                ];
                let (state, session_id) = make_state_with_session(chunks).await;
                let (tx, _rx) = mpsc::unbounded_channel();

                // Re-seed callsite interest immediately before process_turn.
                // In a parallel test run, concurrent tests can trigger
                // rebuild_interest_cache() on their own threads (when their
                // scoped subscriber guards drop), resetting our callsite's
                // interest to "never". Calling it here — with our subscriber
                // still active — re-establishes "sometimes" interest just
                // before the span would be created.
                tracing::callsite::rebuild_interest_cache();

                process_turn(
                    &state,
                    session_id,
                    None,
                    "test".to_string(),
                    tx,
                    Arc::new(Mutex::new(SubscriptionState::new(true))),
                    None,
                )
                .await
                .expect("turn should complete");
            });
    });

    let spans = recorded.lock().unwrap();
    let turn_spans: Vec<_> = spans.iter().filter(|(name, _)| name == "turn").collect();

    assert!(
        !turn_spans.is_empty(),
        "expected a 'turn' span during process_turn; saw spans: {:?}",
        spans.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
    );

    let (_, fields) = &turn_spans[0];
    assert!(
        fields.contains(&"turn_id".to_string()),
        "turn span should have 'turn_id' field, got: {fields:?}",
    );
    assert!(
        fields.contains(&"session_id".to_string()),
        "turn span should have 'session_id' field, got: {fields:?}",
    );
}

// -----------------------------------------------------------------------
// Token usage structured log tests
// -----------------------------------------------------------------------

/// Verifies that process_turn emits an INFO-level tracing event at turn
/// completion that includes input_tokens, output_tokens, and
/// cost_microcents as structured fields. This event extends the turn span
/// with cost-visibility for operators reading JSON logs.
#[test]
fn process_turn_logs_token_usage_at_completion() {
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use swage_core::TokenUsage;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    // Accumulates (level_str, [field_names]) for every event emitted
    // during process_turn. Shared between the layer and the assertions.
    type RecordedFields = Vec<(String, Vec<String>)>;

    let recorded: StdArc<StdMutex<RecordedFields>> = StdArc::new(StdMutex::new(Vec::new()));
    let captured = StdArc::clone(&recorded);

    struct EventFieldRecorder(StdArc<StdMutex<RecordedFields>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventFieldRecorder {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let level = event.metadata().level().to_string();
            let fields: Vec<String> = event
                .metadata()
                .fields()
                .iter()
                .map(|f| f.name().to_string())
                .collect();
            self.0.lock().unwrap().push((level, fields));
        }
    }

    let subscriber = Registry::default().with(EventFieldRecorder(captured));

    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let chunks = vec![
                    StreamChunk::Delta {
                        content: "ok".to_string(),
                    },
                    StreamChunk::Usage(TokenUsage {
                        input_tokens: 100,
                        output_tokens: 50,
                    }),
                    StreamChunk::Done {
                        finish_reason: None,
                    },
                ];
                let (state, session_id) = make_state_with_session(chunks).await;
                let (tx, _rx) = mpsc::unbounded_channel();

                tracing::callsite::rebuild_interest_cache();

                process_turn(
                    &state,
                    session_id,
                    None,
                    "test".to_string(),
                    tx,
                    Arc::new(Mutex::new(SubscriptionState::new(true))),
                    None,
                )
                .await
                .expect("turn should complete");
            });
    });

    let events = recorded.lock().unwrap();

    // Find an INFO event that carries token usage fields.
    let usage_events: Vec<_> = events
        .iter()
        .filter(|(level, fields)| level == "INFO" && fields.contains(&"input_tokens".to_string()))
        .collect();

    assert!(
        !usage_events.is_empty(),
        "expected an INFO event with input_tokens field; got events: {events:?}",
    );

    let (_, fields) = &usage_events[0];
    assert!(
        fields.contains(&"output_tokens".to_string()),
        "usage event should have output_tokens field, got: {fields:?}",
    );
    assert!(
        fields.contains(&"cost_microcents".to_string()),
        "usage event should have cost_microcents field, got: {fields:?}",
    );
}

// -----------------------------------------------------------------------
// Context assembly tests
// -----------------------------------------------------------------------

// Imports from super::* via `use super::*` — ContextElement, HistoryElement,
// PromptsElement, TiktokenTokeniser, PromptRule are all available.

/// Assembly from a PromptRule and HistoryElement produces a system message
/// first, followed by history, with a report showing non-zero token counts.
#[test]
fn assembly_with_instructions_produces_system_message_first() {
    let rule = PromptRule {
        priority: 0,
        content: "You are a helpful coding assistant.".to_string(),
        source_level: swage_config::HierarchyLevel::Global,
        file_path: std::path::PathBuf::from("test.md"),
    };
    let prompts = PromptsElement::new(vec![rule]);
    let history = HistoryElement::new(vec![ChatMessage::new(Role::User, "Hello")]);

    let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(prompts), Box::new(history)];

    let config = swage_config::ContextConfig::default();
    let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

    // System message from instructions appears first.
    assert_eq!(
        output.messages[0].role(),
        Role::System,
        "first message should be system instructions"
    );
    // User message from history follows.
    assert_eq!(
        output.messages[1].role(),
        Role::User,
        "second message should be user history"
    );

    // Report should have non-zero token counts for both elements.
    let prompts_report = output
        .report
        .elements
        .iter()
        .find(|e| e.name == "prompts")
        .expect("report should include prompts element");
    assert!(
        prompts_report.token_count > 0,
        "prompts element should have non-zero token count"
    );

    let history_report = output
        .report
        .elements
        .iter()
        .find(|e| e.name == "history")
        .expect("report should include history element");
    assert!(
        history_report.token_count > 0,
        "history element should have non-zero token count"
    );
}

/// Assembly with empty instructions produces no system message — only
/// history messages.
#[test]
fn assembly_without_instructions_omits_system_message() {
    let prompts = PromptsElement::new(vec![]);
    let history = HistoryElement::new(vec![ChatMessage::new(Role::User, "Hello")]);

    let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(prompts), Box::new(history)];

    let config = swage_config::ContextConfig::default();
    let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

    // No system message when instructions are empty.
    assert!(
        output.messages.iter().all(|m| m.role() != Role::System),
        "expected no system message when prompts are empty"
    );
    assert_eq!(
        output.messages.len(),
        1,
        "should only contain the user message"
    );
}

/// Constructing a PromptRule from the config.instructions string produces
/// a PromptsElement that generates equivalent output to the old flat prepend.
#[test]
fn prompt_rule_from_instructions_string_matches_flat_prepend() {
    let instructions = "You are a helpful assistant.\n\nBe concise.";

    // Old approach: flat system message prepend.
    let flat_system = ChatMessage::new(Role::System, instructions);

    // New approach: PromptRule → PromptsElement → assembly.
    let rule = PromptRule {
        priority: 0,
        content: instructions.to_string(),
        source_level: swage_config::HierarchyLevel::Global,
        file_path: std::path::PathBuf::from("instructions"),
    };
    let prompts = PromptsElement::new(vec![rule]);
    let content = prompts.content();

    assert_eq!(
        content.len(),
        1,
        "should produce exactly one system message"
    );
    assert_eq!(content[0].role(), flat_system.role());
    // Content should be identical — single rule, no joining needed.
    assert_eq!(content[0].content(), flat_system.content());
}

// -----------------------------------------------------------------------
// Workspace permission gate
// -----------------------------------------------------------------------

/// Convenience for invoking the private build_permission_config from tests
/// without having to repeat the full ResolvedConfig wiring.
fn build_permission_config_for_test(config: &swage_config::ResolvedConfig) -> PermissionConfig {
    super::build_permission_config(
        config,
        config,
        None,
        None,
        &std::collections::HashSet::new(),
    )
}

#[test]
fn space_scoped_approval_grant_only_removes_gate_for_matching_space() {
    let matching_space_id = Uuid::new_v4();
    let other_space_id = Uuid::new_v4();
    let mut config = swage_config::ResolvedConfig::default();
    config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "file_write".into(),
        )];

    let grants = std::collections::HashSet::from([crate::state::ApprovalGrantKey {
        session_id: None,
        space_id: Some(matching_space_id),
        kind: swage_storage::model::ApprovalKind::Tool,
        name: "file_write".into(),
        access: None,
    }]);

    let matching =
        super::build_permission_config(&config, &config, Some(matching_space_id), None, &grants);
    let other =
        super::build_permission_config(&config, &config, Some(other_space_id), None, &grants);
    let no_space = super::build_permission_config(&config, &config, None, None, &grants);

    assert!(matching.require_approval.is_empty());
    assert_eq!(other.require_approval.len(), 1);
    assert_eq!(no_space.require_approval.len(), 1);
}

#[test]
fn session_scoped_approval_grant_only_removes_gate_for_granting_session() {
    let granting_session_id = Uuid::new_v4();
    let other_session_id = Uuid::new_v4();
    let mut config = swage_config::ResolvedConfig::default();
    config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "file_write".into(),
        )];
    let grants = std::collections::HashSet::from([crate::state::ApprovalGrantKey {
        session_id: Some(granting_session_id),
        space_id: None,
        kind: swage_storage::model::ApprovalKind::Tool,
        name: "file_write".into(),
        access: None,
    }]);

    let granting =
        super::build_permission_config(&config, &config, None, Some(granting_session_id), &grants);
    let other =
        super::build_permission_config(&config, &config, None, Some(other_session_id), &grants);

    assert!(granting.require_approval.is_empty());
    assert_eq!(other.require_approval.len(), 1);
}

#[tokio::test]
async fn session_scoped_location_grant_only_authorises_granting_sessions_workers() {
    let location = tempfile::tempdir().expect("approved location");
    let (state, granting_session_id) = make_state_with_session(vec![]).await;
    let granting_session = state
        .storage
        .get_session(granting_session_id)
        .await
        .expect("session query")
        .expect("granting session");
    let other_session_id = Uuid::new_v4();
    state
        .session_approval_grants
        .lock()
        .expect("location grants")
        .insert(crate::state::ApprovalGrantKey::location(
            Some(granting_session_id),
            None,
            location.path().to_string_lossy(),
            "write",
        ));

    let authorised = resolve_authorised_worker_directory(
        &state,
        granting_session.space_id,
        Uuid::new_v4(),
        Some(granting_session_id),
        None,
        None,
        &location.path().to_string_lossy(),
    )
    .await
    .expect("granting session should retain its worker location authority");
    assert!(authorised.can_write);

    let error = resolve_authorised_worker_directory(
        &state,
        granting_session.space_id,
        Uuid::new_v4(),
        Some(other_session_id),
        None,
        None,
        &location.path().to_string_lossy(),
    )
    .await
    .expect_err("another session must not inherit the location grant");
    assert!(error.contains("outside the dispatching context"));
}

#[tokio::test]
async fn workspace_summary_is_empty_when_no_attachments_exist() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let space_id = Uuid::new_v4();

    let summary = summarise_trusted_workspace_permissions(&storage, space_id).await;

    assert!(!summary.has_read);
    assert!(!summary.has_write);
}

#[tokio::test]
async fn workspace_summary_ignores_untrusted_attachments() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let space_id = Uuid::new_v4();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id,
            label: "untrusted".into(),
            path: "/tmp/untrusted".into(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: false,
        })
        .await
        .unwrap();

    let summary = summarise_trusted_workspace_permissions(&storage, space_id).await;

    assert!(
        !summary.has_read,
        "untrusted attachment must not grant read"
    );
    assert!(
        !summary.has_write,
        "untrusted attachment must not grant write"
    );
}

#[tokio::test]
async fn workspace_summary_combines_grants_across_trusted_attachments() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let space_id = Uuid::new_v4();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id,
            label: "read-only".into(),
            path: "/tmp/read".into(),
            permissions: vec!["read".into()],
            is_trusted: true,
        })
        .await
        .unwrap();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id,
            label: "write-only".into(),
            path: "/tmp/write".into(),
            permissions: vec!["write".into()],
            is_trusted: true,
        })
        .await
        .unwrap();

    let summary = summarise_trusted_workspace_permissions(&storage, space_id).await;

    assert!(summary.has_read);
    assert!(summary.has_write);
}

#[tokio::test]
async fn durable_worker_placement_rederives_write_and_fails_closed_after_revocation() {
    let placement = tempfile::tempdir().expect("placement");
    let (state, session_id) = make_state_with_session(vec![]).await;
    let mut session = state
        .storage
        .get_session(session_id)
        .await
        .expect("session query")
        .expect("session");
    let authority_context_id = Uuid::new_v4();
    session.metadata = Some(serde_json::json!({
        WORKER_PLACEMENT_CWD_METADATA_KEY: placement.path(),
        WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY: true,
        WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY: placement.path(),
        WORKER_PLACEMENT_AUTHORITY_CONTEXT_METADATA_KEY: authority_context_id,
    }));
    {
        let mut grants = state
            .session_approval_grants
            .lock()
            .expect("location grants");
        grants.insert(crate::state::ApprovalGrantKey::location(
            None,
            Some(session.space_id),
            placement.path().to_string_lossy(),
            "write",
        ));
    }

    reauthorise_worker_placement_for_turn(&state, &mut session)
        .await
        .expect("write placement remains authorised");
    assert_eq!(
        session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );

    {
        let mut grants = state
            .session_approval_grants
            .lock()
            .expect("location grants");
        grants.clear();
        grants.insert(crate::state::ApprovalGrantKey::location(
            None,
            Some(session.space_id),
            placement.path().to_string_lossy(),
            "read",
        ));
    }
    reauthorise_worker_placement_for_turn(&state, &mut session)
        .await
        .expect("read-only placement remains authorised");
    assert_eq!(
        session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY))
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "the recorded write bit must be replaced by current authority"
    );

    state
        .session_approval_grants
        .lock()
        .expect("location grants")
        .clear();
    let error = reauthorise_worker_placement_for_turn(&state, &mut session)
        .await
        .expect_err("revoked placement must fail closed");
    assert!(error.to_string().contains("no longer authorised"));
}

#[tokio::test]
async fn workspace_summary_only_aggregates_target_space() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let target_space = Uuid::new_v4();
    let other_space = Uuid::new_v4();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id: other_space,
            label: "other".into(),
            path: "/tmp/other".into(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .unwrap();

    let summary = summarise_trusted_workspace_permissions(&storage, target_space).await;

    assert!(!summary.has_read);
    assert!(!summary.has_write);
}

#[tokio::test]
async fn delegation_rejects_missing_sender_turn_lineage() {
    let (state, _) = make_state_with_session(vec![]).await;
    let handler = GatewayDelegationHandler::new(state);
    let missing_turn_id = Uuid::new_v4();

    let error = handler
        .active_workspace_attachment_for_turn(missing_turn_id)
        .await
        .expect_err("a missing sender turn must not look like an unattached workspace");

    assert!(matches!(
        error,
        AgentError::StorageFailed(StorageError::NotFound { entity, id })
            if entity == "turn" && id == missing_turn_id.to_string()
    ));
}

#[test]
fn sandbox_config_skips_missing_trusted_workspace_paths() {
    let temp_dir = tempfile::tempdir().unwrap();
    let working_directory = temp_dir.path().join("work");
    let existing_workspace = temp_dir.path().join("existing");
    let missing_workspace = temp_dir.path().join("missing");
    std::fs::create_dir_all(&working_directory).unwrap();
    std::fs::create_dir_all(&existing_workspace).unwrap();

    let agent_config = AgentConfig::new("agent")
        .with_workspace(&working_directory, vec![working_directory.clone()]);
    let policy = TrustedWorkspacePolicy {
        permissions: WorkspacePermissionSummary {
            has_read: true,
            has_write: true,
        },
        active_workspace: Some(TrustedWorkspacePath {
            path: working_directory,
            is_trusted: true,
            can_read: true,
            can_write: true,
        }),
        attachments: vec![
            TrustedWorkspacePath {
                path: existing_workspace.clone(),
                is_trusted: true,
                can_read: true,
                can_write: false,
            },
            TrustedWorkspacePath {
                path: missing_workspace.clone(),
                is_trusted: true,
                can_read: true,
                can_write: true,
            },
        ],
    };

    let sandbox_config = build_sandbox_config(&agent_config, Some(&policy)).unwrap();
    let existing_workspace = sandbox_path(&existing_workspace);

    assert!(
        sandbox_config
            .read_only_mounts
            .contains(&existing_workspace)
    );
    assert!(
        sandbox_config
            .landlock_read_paths
            .contains(&existing_workspace)
    );
    assert!(
        !sandbox_config
            .read_write_mounts
            .contains(&missing_workspace)
    );
    assert!(
        !sandbox_config
            .landlock_write_paths
            .contains(&missing_workspace)
    );
}

#[test]
fn worker_placement_replaces_other_filesystem_authority() {
    let temp_dir = tempfile::tempdir().unwrap();
    let placement = temp_dir.path().join("placement");
    let sibling_workspace = temp_dir.path().join("sibling");
    let approved_location = temp_dir.path().join("approved");
    std::fs::create_dir_all(&placement).unwrap();
    std::fs::create_dir_all(&sibling_workspace).unwrap();
    std::fs::create_dir_all(&approved_location).unwrap();

    let agent_config =
        AgentConfig::new("worker").with_workspace(&placement, vec![placement.clone()]);
    let mut policy = TrustedWorkspacePolicy {
        permissions: WorkspacePermissionSummary {
            has_read: true,
            has_write: true,
        },
        active_workspace: None,
        attachments: vec![TrustedWorkspacePath {
            path: sibling_workspace.clone(),
            is_trusted: true,
            can_read: true,
            can_write: true,
        }],
    };
    let metadata = serde_json::json!({
        "worker_placement_cwd": placement,
        "worker_placement_can_write": false,
    });

    apply_worker_placement_permission(&mut policy, &agent_config, Some(&metadata));

    assert!(policy.attachments.is_empty());
    let sandbox_config = build_sandbox_config(&agent_config, Some(&policy)).unwrap();
    let placement = sandbox_path(&placement);
    let sibling_workspace = sandbox_path(&sibling_workspace);
    assert!(sandbox_config.read_only_mounts.contains(&placement));
    assert!(!sandbox_config.read_write_mounts.contains(&placement));
    assert!(!sandbox_config.read_only_mounts.contains(&sibling_workspace));
    assert!(
        !sandbox_config
            .read_write_mounts
            .contains(&sibling_workspace)
    );

    let grants = std::collections::HashSet::from([crate::state::ApprovalGrantKey::location(
        None,
        None,
        approved_location.to_string_lossy(),
        "write",
    )]);
    let mut allowed_paths = vec![placement.clone()];
    append_approved_location_paths(
        &mut allowed_paths,
        &grants,
        Uuid::new_v4(),
        Uuid::new_v4(),
        Some(&metadata),
    );
    assert_eq!(allowed_paths, vec![placement]);
}

#[test]
fn agent_workspace_uses_active_workspace_and_attachment_grants() {
    let temp_dir = tempfile::tempdir().unwrap();
    let active_workspace = temp_dir.path().join("active");
    let attachment_workspace = temp_dir.path().join("attachment");
    std::fs::create_dir_all(&active_workspace).unwrap();
    std::fs::create_dir_all(&attachment_workspace).unwrap();

    let policy = TrustedWorkspacePolicy {
        permissions: WorkspacePermissionSummary {
            has_read: true,
            has_write: true,
        },
        active_workspace: Some(TrustedWorkspacePath {
            path: active_workspace.clone(),
            is_trusted: true,
            can_read: true,
            can_write: true,
        }),
        attachments: vec![TrustedWorkspacePath {
            path: attachment_workspace.clone(),
            is_trusted: true,
            can_read: true,
            can_write: false,
        }],
    };

    let (workspace_root, allowed_paths) = agent_workspace_from_policy(
        std::path::Path::new(".swage"),
        &policy,
        Uuid::new_v4(),
        Uuid::new_v4(),
    );

    assert_eq!(workspace_root, active_workspace);
    assert_eq!(allowed_paths, vec![active_workspace, attachment_workspace]);
}

#[test]
fn permission_config_inherits_global_defaults_when_workspace_grants_are_missing() {
    let config = swage_config::ResolvedConfig::default();
    let policy = build_permission_config_for_test(&config);

    assert!(
        policy.deny.is_empty(),
        "missing space workspace grants must inherit global permissions rather than denying"
    );
}

#[test]
fn permission_config_does_not_duplicate_already_denied_tools() {
    let mut config = swage_config::ResolvedConfig::default();
    config.tools.permissions.deny = vec!["file_write".into()];

    let policy = build_permission_config_for_test(&config);

    let file_write_count = policy
        .deny
        .iter()
        .filter(|entry| entry.as_str() == "file_write")
        .count();
    assert_eq!(file_write_count, 1, "file_write should appear once in deny");
}

/// Test-shaped orientation inputs: a bare environment for a non-channel
/// session in an unnamed space.
fn test_environment() -> swage_context::orientation::SessionEnvironment {
    swage_context::orientation::SessionEnvironment {
        space_name: String::new(),
        space_description: String::new(),
        working_directory: None,
        started_at: chrono::Utc::now(),
        channel: None,
    }
}

/// The build_assembly_elements helper constructs the correct elements for
/// the turn's context assembly. This function is tested directly to verify
/// the wiring before it's used in process_turn.
#[test]
fn named_assembly_elements_include_prompts_skills_memory_and_history() {
    let instructions = "Be helpful.";
    let history = vec![
        ChatMessage::new(Role::User, "Hi"),
        ChatMessage::new(Role::Assistant, "Hello!"),
    ];

    let elements = build_assembly_elements(
        instructions,
        vec![],
        vec![],
        history,
        Some((String::new(), "My Space".to_string())),
        test_environment(),
        vec![],
        "claude-test-model",
        swage_context::working_norms::WorkingNormsScaffold::Native,
        &[],
        None,
    );

    // Should produce six elements: orientation, working norms, prompts,
    // skills (empty), memory, and history.
    assert_eq!(elements.len(), 6);
    let names: Vec<&str> = elements.iter().map(|e| e.name()).collect();
    assert!(
        names.contains(&"orientation"),
        "should include the orientation baseline"
    );

    let names: Vec<&str> = elements.iter().map(|e| e.name()).collect();
    assert!(names.contains(&"prompts"), "should include prompts element");
    assert!(names.contains(&"skills"), "should include skills element");
    assert!(names.contains(&"memory"), "should include memory element");
    assert!(names.contains(&"history"), "should include history element");
}

/// When instructions are empty, build_assembly_elements still includes the
/// prompts element (which will produce no system message), the skills element,
/// and history. A default assembly does not receive a memory element.
#[test]
fn default_assembly_elements_exclude_memory() {
    let elements = build_assembly_elements(
        "",
        vec![],
        vec![],
        vec![ChatMessage::new(Role::User, "Hello")],
        None,
        test_environment(),
        vec![],
        "claude-test-model",
        swage_context::working_norms::WorkingNormsScaffold::Native,
        &[],
        None,
    );

    assert_eq!(elements.len(), 5);
    // PromptsElement with empty rules produces no messages; the orientation
    // baseline is what keeps an unconfigured session's context non-empty.
    let orientation = elements.iter().find(|e| e.name() == "orientation").unwrap();
    assert!(!orientation.content().is_empty());
    let prompts = elements.iter().find(|e| e.name() == "prompts").unwrap();
    assert!(prompts.content().is_empty());
    assert!(
        elements.iter().all(|element| element.name() != "memory"),
        "default element construction should omit memory"
    );
}

#[tokio::test]
async fn default_assembly_does_not_invoke_memory_loader() {
    let invoked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let loader_invoked = invoked.clone();

    let memory = load_memory_for_assembly("default", || async move {
        loader_invoked.store(true, std::sync::atomic::Ordering::SeqCst);
        "should not be loaded".to_string()
    })
    .await;

    assert!(
        memory.is_none(),
        "default assembly should not receive memory"
    );
    assert!(
        !invoked.load(std::sync::atomic::Ordering::SeqCst),
        "default assembly must not invoke the memory-loading seam"
    );
}

#[tokio::test]
async fn named_assembly_loads_memory_and_can_compose_it() {
    let memory = load_memory_for_assembly("memory-aware", || async {
        "Important space context".to_string()
    })
    .await;
    let elements = build_assembly_elements(
        "Be helpful.",
        vec![],
        vec![],
        vec![],
        memory.map(|content| (content, "My Space".to_string())),
        test_environment(),
        vec![],
        "claude-test-model",
        swage_context::working_norms::WorkingNormsScaffold::Native,
        &[],
        None,
    );

    let dir = tempfile::tempdir().unwrap();
    let assembly_path = dir.path().join("memory-aware.js");
    std::fs::write(
        &assembly_path,
        r#"
        const byName = Object.fromEntries(elements.map(element => [element.name, element]));
        add(byName.memory);
        "#,
    )
    .unwrap();
    let output = swage_context::js_assembly::assemble_named(
        "memory-aware",
        Some(&assembly_path),
        &elements,
        &swage_config::model::ContextConfig::default(),
        &TiktokenTokeniser::new("gpt-5"),
        &std::collections::BTreeMap::new(),
    );

    assert!(
        output.messages[0]
            .text_content()
            .contains("Important space context"),
        "a named assembly should be able to compose the loaded memory element"
    );
    assert_eq!(output.report.assembly_name, "memory-aware");
}

#[tokio::test(flavor = "multi_thread")]
async fn delegated_assembly_applies_call_scoped_composition_over_live_state() {
    let swage_home = tempfile::TempDir::new().expect("temp swage home");
    std::fs::create_dir_all(swage_home.path().join("assemblies"))
        .expect("create assembly directory");
    std::fs::write(
        swage_home.path().join("assemblies/briefing.js"),
        "add(elements.find(element => element.name === 'current-work').render());",
    )
    .expect("write assembly");
    let _guard = SwageHomeGuard::set_to(swage_home.path());

    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("create default space");
    let element_path = swage_home
        .path()
        .join("spaces")
        .join(&space.slug)
        .join("elements/current-work.md");
    std::fs::create_dir_all(element_path.parent().unwrap()).expect("create element directory");
    std::fs::write(&element_path, "Task: {{task}}; owner: {{owner}}")
        .expect("write space briefing");
    std::fs::write(
        element_path.parent().unwrap().join("unrequested.md"),
        "This briefing must not be composed",
    )
    .expect("write unrequested briefing");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "delegate".into(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create delegated session");
    swage_storage::SessionVariableStore::set_session_variable(
        storage.as_ref(),
        session.id,
        "task",
        "inspect delegated context",
    )
    .await
    .expect("save delegated session value");

    let mut config = swage_config::ResolvedConfig::default();
    config.context.assembly = "default".into();
    config.instructions = "Task: {{task}}; owner: {{owner}}".into();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::SpaceStore>,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        config.clone(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let host_context =
        swage_tool::context::MockToolContext::new("delegate", session.id.to_string());
    let (tool_event_tx, _tool_event_rx) = mpsc::unbounded_channel();
    let tool_context = crate::tool_executor::GatewayToolContext::new(
        &host_context,
        tool_event_tx,
        state.storage.clone(),
    )
    .with_app_state(Some(state.clone()));
    let session_value_dispatcher = crate::tool::ToolDispatcher::allow_all(vec![Arc::new(
        crate::tool::session_variable::SessionVariableTool,
    )
        as Arc<dyn crate::tool::Tool>]);
    let (context_event_tx, _context_event_rx) = mpsc::unbounded_channel();
    let assembler = crate::turn::delegation::delegated_context_assembler(
        state,
        space,
        config,
        "delegate".into(),
        "/workspace".into(),
        context_event_tx,
        serde_json::json!({}),
        DispatchContextComposition {
            elements: Some(vec!["current-work".into()]),
            variables: Some(std::collections::BTreeMap::from([(
                "owner".into(),
                "dispatch owner".into(),
            )])),
            assembly: Some("briefing".into()),
            skills: None,
        },
    );

    let output = assembler
        .assemble(&session, Uuid::new_v4(), vec![])
        .await
        .expect("assemble delegated context");
    let context = output
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        context.contains("Task: inspect delegated context; owner: dispatch owner"),
        "delegated assembly should combine session and call-scoped values: {context}"
    );
    assert!(
        output.report["elements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["name"] == "current-work"),
        "the delegated assembly report should identify the named briefing"
    );
    assert_eq!(output.report["assembly_name"], "briefing");
    assert!(
        output.report["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|element| element["name"] != "unrequested"),
        "only requested named briefings should reach the child assembly report"
    );

    std::fs::write(&element_path, "Updated task: {{task}}; owner: {{owner}}")
        .expect("edit space briefing");
    session_value_dispatcher
        .dispatch(
            "session_values",
            serde_json::json!({
                "action": "set",
                "name": "task",
                "value": "verify current state"
            }),
            &tool_context,
            "delegate",
        )
        .await
        .expect("update delegated session value through the session_values tool");
    let recomposed = assembler
        .assemble(&session, Uuid::new_v4(), vec![])
        .await
        .expect("reassemble delegated context");
    let recomposed_context = recomposed
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        recomposed_context.contains("Updated task: verify current state; owner: dispatch owner"),
        "a later dispatch should re-read the file and current values: {recomposed_context}"
    );
    assert!(!recomposed_context.contains("inspect delegated context"));
}

/// When memory_content is non-empty, the memory element produces a system
/// message containing that content.
#[test]
fn build_assembly_elements_memory_element_contains_provided_content() {
    let elements = build_assembly_elements(
        "Be helpful.",
        vec![],
        vec![],
        vec![],
        Some((
            "Important space context".to_string(),
            "My Space".to_string(),
        )),
        test_environment(),
        vec![],
        "claude-test-model",
        swage_context::working_norms::WorkingNormsScaffold::Native,
        &[],
        None,
    );

    let memory = elements.iter().find(|e| e.name() == "memory").unwrap();
    let messages = memory.content();

    assert_eq!(
        messages.len(),
        1,
        "non-empty memory should produce one message"
    );
    // content is Vec<ContentPart> — extract the text to check the value.
    let text = messages[0].content().iter().find_map(|p| {
        if let swage_core::ContentPart::Text { text } = p {
            Some(text.as_str())
        } else {
            None
        }
    });
    assert!(
        text.is_some_and(|t| t.contains("Important space context")),
        "memory message should contain the provided content"
    );
}

fn guidance_workspace(path: &std::path::Path, is_trusted: bool) -> TrustedWorkspacePath {
    TrustedWorkspacePath {
        path: path.to_path_buf(),
        is_trusted,
        can_read: true,
        can_write: false,
    }
}

/// Operator guidance reads the config root's AGENTS.md first, then a trusted
/// workspace's — global before workspace, each rule naming its source file.
#[test]
fn operator_guidance_reads_global_before_trusted_workspace() {
    let config_root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(config_root.path().join("AGENTS.md"), "GLOBAL GUIDANCE").unwrap();
    std::fs::write(workspace.path().join("AGENTS.md"), "WORKSPACE GUIDANCE").unwrap();

    let policy = TrustedWorkspacePolicy {
        active_workspace: Some(guidance_workspace(workspace.path(), true)),
        ..Default::default()
    };

    let rules = operator_guidance_rules(Some(config_root.path()), &policy);
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].content, "GLOBAL GUIDANCE");
    assert_eq!(rules[0].source_level, HierarchyLevel::Global);
    assert_eq!(rules[0].file_path, config_root.path().join("AGENTS.md"));
    assert_eq!(rules[1].content, "WORKSPACE GUIDANCE");
    assert_eq!(rules[1].source_level, HierarchyLevel::Space);
    assert_eq!(rules[1].file_path, workspace.path().join("AGENTS.md"));
}

/// An untrusted workspace contributes nothing, even when its AGENTS.md
/// exists and the attachment is readable.
#[test]
fn operator_guidance_ignores_untrusted_workspace() {
    let config_root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(config_root.path().join("AGENTS.md"), "GLOBAL GUIDANCE").unwrap();
    std::fs::write(workspace.path().join("AGENTS.md"), "MUST NOT APPEAR").unwrap();

    let policy = TrustedWorkspacePolicy {
        active_workspace: Some(guidance_workspace(workspace.path(), false)),
        ..Default::default()
    };

    let rules = operator_guidance_rules(Some(config_root.path()), &policy);
    assert_eq!(
        rules.len(),
        1,
        "untrusted workspace must contribute nothing"
    );
    assert_eq!(rules[0].content, "GLOBAL GUIDANCE");
}

/// Absent guidance files are not an error: roots without AGENTS.md simply
/// contribute no rules, and a missing config root contributes nothing.
#[test]
fn operator_guidance_absent_files_contribute_nothing() {
    let config_root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let policy = TrustedWorkspacePolicy {
        active_workspace: Some(guidance_workspace(workspace.path(), true)),
        ..Default::default()
    };

    assert!(operator_guidance_rules(Some(config_root.path()), &policy).is_empty());
    assert!(operator_guidance_rules(None, &policy).is_empty());
}

/// Trusted attachment grants contribute alongside the active workspace,
/// deduplicated by path, active workspace first.
#[test]
fn operator_guidance_includes_trusted_attachments_once() {
    let active = tempfile::tempdir().unwrap();
    let attachment = tempfile::tempdir().unwrap();
    std::fs::write(active.path().join("AGENTS.md"), "ACTIVE").unwrap();
    std::fs::write(attachment.path().join("AGENTS.md"), "ATTACHED").unwrap();

    let policy = TrustedWorkspacePolicy {
        active_workspace: Some(guidance_workspace(active.path(), true)),
        attachments: vec![
            guidance_workspace(active.path(), true),
            guidance_workspace(attachment.path(), true),
        ],
        ..Default::default()
    };

    let rules = operator_guidance_rules(None, &policy);
    let contents: Vec<&str> = rules.iter().map(|rule| rule.content.as_str()).collect();
    assert_eq!(contents, vec!["ACTIVE", "ATTACHED"]);
}

/// The trust gate holds through the real policy loader, not just
/// hand-built policy literals: two stored attachments differing only in
/// their trust bit produce guidance for the trusted one and nothing for
/// the untrusted one.
#[tokio::test]
async fn operator_guidance_honours_trust_from_stored_attachments() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("AGENTS.md"), "STORED GUIDANCE").unwrap();

    let mut rules_by_trust = Vec::new();
    for is_trusted in [false, true] {
        let space_id = Uuid::new_v4();
        let attachment = storage
            .create_workspace_attachment(NewWorkspaceAttachment {
                space_id,
                label: "workspace".into(),
                path: workspace.path().display().to_string(),
                permissions: vec!["read".into()],
                is_trusted,
            })
            .await
            .unwrap();

        let policy = load_trusted_workspace_policy(&storage, space_id, Some(attachment.id)).await;
        rules_by_trust.push(operator_guidance_rules(None, &policy));
    }

    assert!(
        rules_by_trust[0].is_empty(),
        "an untrusted stored attachment must contribute no guidance"
    );
    assert_eq!(
        rules_by_trust[1].len(),
        1,
        "a trusted stored attachment must contribute its guidance"
    );
    assert_eq!(rules_by_trust[1][0].content, "STORED GUIDANCE");
}

/// Guidance rules extend the resolved instruction baseline rather than
/// replacing it: the prompts element carries the resolved instructions
/// first, then the guidance content, in one system message.
#[test]
fn assembly_elements_layer_operator_guidance_after_instructions() {
    let config_root = tempfile::tempdir().unwrap();
    std::fs::write(config_root.path().join("AGENTS.md"), "GUIDANCE TEXT").unwrap();
    let guidance =
        operator_guidance_rules(Some(config_root.path()), &TrustedWorkspacePolicy::default());

    let elements = build_assembly_elements(
        "BASELINE INSTRUCTIONS",
        guidance,
        vec![],
        vec![],
        None,
        test_environment(),
        vec![],
        "claude-test-model",
        swage_context::working_norms::WorkingNormsScaffold::Native,
        &[],
        None,
    );

    let prompts = elements.iter().find(|e| e.name() == "prompts").unwrap();
    let text = prompts.content()[0].text_content();
    let baseline = text.find("BASELINE INSTRUCTIONS").unwrap();
    let guidance = text.find("GUIDANCE TEXT").unwrap();
    assert!(
        baseline < guidance,
        "resolved instructions must precede operator guidance"
    );
}

/// load_space_memory returns an empty string when the space has no
/// memory directory.
#[tokio::test]
async fn load_space_memory_returns_empty_when_dir_absent() {
    let dir = tempfile::tempdir().unwrap();
    // No memory directory created — the helper should not panic.
    let result = load_space_memory(dir.path(), "no-such-space").await;
    assert!(
        result.is_empty(),
        "should return empty string when memory dir is absent"
    );
}

/// load_space_memory reads all .md files from the memory directory and
/// joins them in alphabetical filename order.
#[tokio::test]
async fn load_space_memory_concatenates_md_files_alphabetically() {
    let dir = tempfile::tempdir().unwrap();
    let memory_dir = dir.path().join("spaces").join("my-space").join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();

    // b_notes sorts after a_context.
    std::fs::write(memory_dir.join("b_notes.md"), "Second file content").unwrap();
    std::fs::write(memory_dir.join("a_context.md"), "First file content").unwrap();
    // Non-.md file must not be included.
    std::fs::write(memory_dir.join("other.txt"), "Should be ignored").unwrap();

    let result = load_space_memory(dir.path(), "my-space").await;

    assert!(
        result.contains("First file content"),
        "should include first .md file"
    );
    assert!(
        result.contains("Second file content"),
        "should include second .md file"
    );
    assert!(
        !result.contains("Should be ignored"),
        "should not include non-.md files"
    );

    // a_context.md sorts before b_notes.md alphabetically.
    let first_pos = result.find("First file content").unwrap();
    let second_pos = result.find("Second file content").unwrap();
    assert!(
        first_pos < second_pos,
        "files should appear in alphabetical order"
    );
}

/// load_space_memory skips files with only whitespace content.
#[tokio::test]
async fn load_space_memory_skips_empty_files() {
    let dir = tempfile::tempdir().unwrap();
    let memory_dir = dir.path().join("spaces").join("my-space").join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();

    std::fs::write(memory_dir.join("empty.md"), "   \n  ").unwrap();
    std::fs::write(memory_dir.join("content.md"), "Real content").unwrap();

    let result = load_space_memory(dir.path(), "my-space").await;

    assert!(result.contains("Real content"));
    assert!(
        !result.contains("   "),
        "whitespace-only files should be excluded"
    );
}

// -----------------------------------------------------------------------
// Token usage persistence tests
// -----------------------------------------------------------------------

/// When the provider returns a Usage chunk, the turn record should have
/// non-null input_tokens and output_tokens in storage after completion.
#[tokio::test]
async fn token_usage_persisted_to_turn_record() {
    use swage_core::TokenUsage;

    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 42,
            output_tokens: 17,
        }),
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns.len(), 1);

    let turn = &turns[0];
    assert_eq!(
        turn.input_tokens,
        Some(42),
        "input_tokens should be persisted from provider usage"
    );
    assert_eq!(
        turn.output_tokens,
        Some(17),
        "output_tokens should be persisted from provider usage"
    );
}

/// When the provider does not return a Usage chunk, token fields should
/// remain NULL (None) in the turn record.
#[tokio::test]
async fn token_usage_remains_null_when_provider_sends_no_usage() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();

    // The agent always persists token counts — zero when the provider
    // sends no Usage chunk (the old gateway left them NULL, the agent
    // stores the accumulated total which starts at 0).
    let turn = &turns[0];
    assert_eq!(
        turn.input_tokens,
        Some(0),
        "input_tokens should be zero when no usage reported"
    );
    assert_eq!(
        turn.output_tokens,
        Some(0),
        "output_tokens should be zero when no usage reported"
    );
}

// -----------------------------------------------------------------------
// ContextAssembled event and snapshot persistence tests
// -----------------------------------------------------------------------

/// ContextAssembled must be the first event before any Token events.
#[tokio::test]
async fn context_assembled_precedes_first_token() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // Find positions of ContextAssembled and first Token.
    let assembled_pos = events
        .iter()
        .position(|e| matches!(e, ServerEvent::ContextAssembled { .. }))
        .expect("expected a ContextAssembled event");
    let first_token_pos = events
        .iter()
        .position(|e| matches!(e, ServerEvent::Token { .. }))
        .expect("expected at least one Token event");

    assert!(
        assembled_pos < first_token_pos,
        "ContextAssembled (pos {assembled_pos}) must precede first Token (pos {first_token_pos})"
    );

    // Verify the event carries the correct turn_id.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let turn_id = turns[0].id;

    match &events[assembled_pos] {
        ServerEvent::ContextAssembled {
            turn_id: event_turn_id,
            report,
        } => {
            assert_eq!(*event_turn_id, turn_id, "turn_id should match");
            // Report should have an "elements" array from the assembly engine.
            assert!(
                !report.elements.is_empty(),
                "report should contain elements key"
            );
            let session = state
                .storage
                .get_session(session_id)
                .await
                .expect("session lookup")
                .expect("session");
            assert_eq!(
                report
                    .tool_scope
                    .as_ref()
                    .and_then(|scope| scope.get("effective_root")),
                Some(&serde_json::json!(managed_scratch_workspace(
                    &state.config_root,
                    session.space_id,
                    session_id
                ))),
                "live turn diagnostics must report the root passed to tools"
            );
        }
        other => panic!("expected ContextAssembled, got: {other:?}"),
    }
}

/// The assembly report should be persisted as a context snapshot.
#[tokio::test]
async fn assembly_report_persisted_as_context_snapshot() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let turn_id = turns[0].id;

    let snapshot = state
        .storage
        .get_snapshot(turn_id)
        .await
        .expect("get_snapshot should succeed");

    assert!(
        snapshot.is_some(),
        "context snapshot should be persisted for the turn"
    );

    // Verify it parses as valid JSON with the expected structure.
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot.unwrap()).expect("snapshot should be valid JSON");
    assert!(
        parsed.get("elements").is_some(),
        "snapshot should contain elements key"
    );
    let session = state
        .storage
        .get_session(session_id)
        .await
        .expect("session lookup")
        .expect("session");
    assert_eq!(
        parsed["tool_scope"]["effective_root"],
        serde_json::json!(managed_scratch_workspace(
            &state.config_root,
            session.space_id,
            session_id
        )),
        "turn diagnostics must record the root passed to tools"
    );
    let provider_request = &parsed["provider_visible"]["requests"][0];
    assert_eq!(provider_request["round"], serde_json::json!(1));
    assert_eq!(
        provider_request["request"]["stream"],
        serde_json::json!(true)
    );
    assert!(
        provider_request["request"]["body"]["messages"].is_array(),
        "provider-visible snapshot should include converted request messages"
    );
}

#[tokio::test]
async fn existing_placeholder_turn_receives_provider_visible_snapshot() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Async answer".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("session should have a thread");
    let placeholder = state
        .storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("Async hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create placeholder turn");
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn_with_existing_turn(
        &state,
        session_id,
        Some(thread.id),
        "Async hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        None,
        None,
        None,
        Some(placeholder.id),
        TurnAttendance::Attended,
    )
    .await
    .expect("placeholder turn should complete");

    let turns = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("list turns");
    assert_eq!(turns.len(), 1, "existing placeholder must be reused");
    assert_eq!(turns[0].id, placeholder.id);
    assert_eq!(turns[0].state, TurnState::Completed);
    assert_eq!(turns[0].assistant_response.as_deref(), Some("Async answer"));

    let snapshot = state
        .storage
        .get_snapshot(placeholder.id)
        .await
        .expect("get snapshot")
        .expect("provider-visible snapshot should be persisted on placeholder turn");
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot).expect("snapshot should be valid JSON");
    let provider_request = &parsed["provider_visible"]["requests"][0];
    assert_eq!(provider_request["round"], serde_json::json!(1));
    assert!(
        provider_request["request"]["body"]["messages"].is_array(),
        "provider-visible request should be attached to the reused turn"
    );
}

#[tokio::test]
async fn failed_agent_build_marks_turn_failed_and_leaves_thread_usable() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Recovered".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    let malformed_schema = serde_json::json!({"type": 17});
    let error = process_turn(
        &state,
        session_id,
        None,
        "First attempt".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        Some(malformed_schema),
    )
    .await
    .expect_err("malformed output schema must fail agent construction");
    assert!(error.to_string().contains("agent construction failed"));
    assert!(matches!(
        rx.recv().await,
        Some(ServerEvent::Error { code, .. }) if code == "agent_construction_error"
    ));
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "failed construction must release the in-memory turn guard"
    );

    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].state, TurnState::Failed);

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        Some(thread.id),
        "Second attempt".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("the same thread must accept a later valid turn");

    let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].state, TurnState::Failed);
    assert_eq!(turns[1].state, TurnState::Completed);
}

#[tokio::test]
async fn provider_reasoning_is_persisted_in_context_snapshot_not_visible_response() {
    let chunks = vec![
        StreamChunk::Reasoning {
            content: "private thought".to_string(),
            details: Some(serde_json::json!({"kind": "summary"})),
        },
        StreamChunk::Delta {
            content: "Visible answer".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let messages = state
        .storage
        .list_messages_by_thread(threads[0].id)
        .await
        .unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.content == "Visible answer"),
        "visible assistant transcript should contain only visible content"
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains("private thought")),
        "reasoning side channel must not be persisted as transcript text"
    );

    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let snapshot = state
        .storage
        .get_snapshot(turns[0].id)
        .await
        .expect("get_snapshot should succeed")
        .expect("snapshot should be persisted");
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot).expect("snapshot should be valid JSON");
    let reasoning = &parsed["provider_visible"]["reasoning"][0];
    assert_eq!(reasoning["round"], serde_json::json!(1));
    assert_eq!(reasoning["content"], "private thought");
    assert_eq!(reasoning["details"]["kind"], "summary");
}

#[tokio::test]
async fn secret_refs_stay_out_of_snapshots_and_persisted_tool_arguments() {
    struct SecretEchoTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for SecretEchoTool {
        fn name(&self) -> &str {
            "secret_echo"
        }

        fn description(&self) -> &str {
            "returns a resolved secret"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "secret": { "type": "string" }
                },
                "required": ["secret"]
            })
        }

        fn sensitive_keys(&self) -> &[&str] {
            &["secret"]
        }

        fn device_secret_keys(&self) -> &[&str] {
            &["secret"]
        }

        async fn execute(
            &self,
            arguments: serde_json::Value,
            _ctx: &dyn swage_tool::context::ToolContext,
        ) -> Result<serde_json::Value, swage_core::ToolError> {
            Ok(serde_json::json!({
                "resolved": arguments["secret"].as_str().unwrap_or_default()
            }))
        }
    }

    let secret_value = "host-only-secret";
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(vec![]).await;
    let secret = state
        .secret_manager
        .create_secret(crate::secret::CreateSecretRequest {
            name: "Turn secret".into(),
            material_kind: "plugin_secret".into(),
            owner_kind: "service".into(),
            backend_kind: "managed".into(),
            scope_kind: "plugin".into(),
            scope_id: Some("turn-test".into()),
            metadata: None,
            value: Some(secret_value.into()),
            external_reference: None,
        })
        .await
        .unwrap();
    state.replace_runtime(
        state.active_config(),
        Arc::new(MultiRoundMockProvider::new(vec![
            vec![
                StreamChunk::ToolUse {
                    call_id: "secret-call".into(),
                    name: "secret_echo".into(),
                    arguments: serde_json::json!({
                        "secret": secret.reference
                    }),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::ToolUse),
                },
            ],
            round_2.clone(),
        ])) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(SecretEchoTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Use the governed secret".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    let snapshot = state
        .storage
        .get_snapshot(turns[0].id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(tool_calls.len(), 1);
    assert!(tool_calls[0].arguments.contains("secret://"));
    assert!(!tool_calls[0].arguments.contains(secret_value));
    assert!(!snapshot.contains(secret_value));
}

#[tokio::test]
async fn provider_visible_capture_fails_closed_when_secret_inventory_is_unavailable() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Safe response".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage(chunks, Arc::clone(&storage)).await;
    let secret_value = "must-not-reach-context-snapshot";
    state
        .secret_manager
        .create_secret(crate::secret::CreateSecretRequest {
            name: "Snapshot guard".into(),
            material_kind: "provider_secret".into(),
            owner_kind: "provider".into(),
            backend_kind: "managed".into(),
            scope_kind: "global".into(),
            scope_id: None,
            metadata: None,
            value: Some(secret_value.into()),
            external_reference: None,
        })
        .await
        .expect("create managed secret");
    storage.fail_secret_reads();

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        format!("Never persist {secret_value}"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("redaction degradation should not abort the model turn");

    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .expect("list threads")
        .into_iter()
        .next()
        .expect("session should have a thread");
    let turn = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("list turns")
        .into_iter()
        .next()
        .expect("turn should exist");
    let snapshot = state
        .storage
        .get_snapshot(turn.id)
        .await
        .expect("get snapshot")
        .expect("safe degradation marker should be persisted");
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot).expect("snapshot should remain valid JSON");

    assert!(!snapshot.contains(secret_value));
    assert_eq!(
        parsed["provider_visible"]["capture_errors"][0]["kind"],
        "secret_redaction_unavailable"
    );
    assert_eq!(
        parsed["provider_visible"]["capture_errors"][0]["round"],
        serde_json::json!(1)
    );
}

/// When subscription is off, ContextAssembled is not delivered but
/// snapshot persistence still happens.
#[tokio::test]
async fn context_assembled_not_sent_when_unsubscribed() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    // force_on = false, no channels subscribed.
    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(false))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // No ContextAssembled in the event stream.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ServerEvent::ContextAssembled { .. })),
        "ContextAssembled should not be sent when unsubscribed"
    );

    // But snapshot should still be persisted.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let snapshot = state
        .storage
        .get_snapshot(turns[0].id)
        .await
        .expect("get_snapshot should succeed");
    assert!(
        snapshot.is_some(),
        "snapshot should still be persisted even without subscription"
    );
}

// -----------------------------------------------------------------------
// TurnComplete usage field tests
// -----------------------------------------------------------------------

/// When subscribed to context channel, TurnComplete includes token usage.
#[tokio::test]
async fn turn_complete_includes_usage_when_context_subscribed() {
    use swage_core::TokenUsage;

    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        }),
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    // force_on = true means all channels are subscribed.
    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    let complete = events
        .iter()
        .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        .expect("expected TurnComplete event");

    match complete {
        ServerEvent::TurnComplete {
            input_tokens,
            output_tokens,
            ..
        } => {
            assert_eq!(*input_tokens, Some(100), "input_tokens should be populated");
            assert_eq!(
                *output_tokens,
                Some(50),
                "output_tokens should be populated"
            );
        }
        other => panic!("expected TurnComplete, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Output schema wiring tests
// -----------------------------------------------------------------------

/// When the config has an output schema and the provider returns valid JSON,
/// TurnComplete carries structured_output and the turn record is persisted.
#[tokio::test]
async fn turn_complete_carries_structured_output_when_schema_passes() {
    let _guard = SwageHomeGuard::isolate();
    use swage_config::OutputConfig;
    use swage_core::FinishReason;

    let chunks = vec![
        StreamChunk::Delta {
            content: r#"{"result":"ok"}"#.to_string(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ];

    // Build a config with an output schema requiring {"result": <string>}.
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "result": { "type": "string" } },
        "required": ["result"]
    });
    let config = swage_config::ResolvedConfig {
        output: OutputConfig {
            schema: Some(schema),
            max_retries: 1,
        },
        ..Default::default()
    };

    let (state, session_id) = make_state_with_config(chunks, config).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "give me json".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    let complete = events
        .iter()
        .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        .expect("expected TurnComplete");

    match complete {
        ServerEvent::TurnComplete {
            structured_output, ..
        } => {
            assert!(
                structured_output.is_some(),
                "TurnComplete should carry structured_output when schema passes"
            );
            assert_eq!(structured_output.as_ref().unwrap()["result"], "ok");
        }
        other => panic!("expected TurnComplete, got: {other:?}"),
    }
}

/// After a valid schema turn, the turn record in storage has structured_output.
#[tokio::test]
async fn structured_output_persisted_on_turn_after_schema_validation() {
    let _guard = SwageHomeGuard::isolate();
    use swage_config::OutputConfig;
    use swage_core::FinishReason;
    use swage_storage::store::TurnStore;

    let chunks = vec![
        StreamChunk::Delta {
            content: r#"{"x":42}"#.to_string(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ];

    let schema = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "integer" } },
        "required": ["x"]
    });
    let config = swage_config::ResolvedConfig {
        output: OutputConfig {
            schema: Some(schema),
            max_retries: 1,
        },
        ..Default::default()
    };

    let (state, session_id) = make_state_with_config(chunks, config).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "give me x".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(false))),
        None,
    )
    .await
    .expect("turn should complete");

    // Drain events to let the turn finish.
    while rx.try_recv().is_ok() {}

    // Look up the turn in storage and verify structured_output was persisted.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = TurnStore::list_turns_by_thread(&*state.storage, thread_id)
        .await
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert!(
        turns[0].structured_output.is_some(),
        "turn record should have structured_output persisted"
    );
    assert_eq!(turns[0].structured_output.as_ref().unwrap()["x"], 42);
}

/// When NOT subscribed to context channel, TurnComplete omits usage fields.
#[tokio::test]
async fn turn_complete_omits_usage_when_not_subscribed() {
    use swage_core::TokenUsage;

    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        }),
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    // force_on = false, no subscriptions.
    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(false))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    let complete = events
        .iter()
        .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        .expect("expected TurnComplete event");

    match complete {
        ServerEvent::TurnComplete {
            input_tokens,
            output_tokens,
            cost_microcents,
            ..
        } => {
            assert_eq!(
                *input_tokens, None,
                "input_tokens should be None when unsubscribed"
            );
            assert_eq!(
                *output_tokens, None,
                "output_tokens should be None when unsubscribed"
            );
            assert_eq!(
                *cost_microcents, None,
                "cost_microcents should be None when unsubscribed"
            );
        }
        other => panic!("expected TurnComplete, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Approval event routing tests [byi8]
// -----------------------------------------------------------------------

#[tokio::test]
async fn sandbox_degradation_approval_uses_resolved_hitl_settings() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: threads[0].id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let mut turn_config = state.config.clone();
    turn_config.hitl.timeout = 7;
    turn_config.hitl.timeout_policy = swage_core::ApprovalTimeoutPolicy::Interrupt;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let approval_channels = Arc::clone(&state.approval_channels);
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        request_sandbox_degradation_approval(
            &state_for_task,
            SandboxDegradationApprovalRequest {
                config: &turn_config,
                ws_tx: &tx,
                turn_id: turn.id,
                agent_id: "gateway",
                prompt: "Sandbox setup failed. Approve unsandboxed execution?".to_string(),
                context: serde_json::json!({ "degradation": "unsandboxed_execution" }),
                cancel_token: &cancel_token,
            },
        )
        .await
    });

    let approval_id = match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired {
            approval_id,
            timeout_policy,
            ..
        } => {
            assert_eq!(timeout_policy, "interrupt");
            approval_id
        }
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    };

    let approval = state
        .storage
        .get_approval(approval_id)
        .await
        .unwrap()
        .expect("approval should be stored");
    assert_eq!(
        approval.timeout_policy,
        swage_core::ApprovalTimeoutPolicy::Interrupt
    );

    approval_channels
        .lock()
        .unwrap()
        .remove(&approval_id)
        .expect("approval channel should be registered")
        .send(swage_agent::ApprovalDecision {
            decision: swage_agent::approval::ApprovalDecisionValue::Approved,
            scope: swage_agent::approval::ApprovalDecisionScope::Once,
            reason: None,
        })
        .expect("approval decision should be delivered");

    let outcome = task
        .await
        .expect("approval task should finish")
        .expect("approval should succeed");
    assert_eq!(outcome, SandboxDegradationApproval::Approved);
}

#[tokio::test]
async fn sandbox_degradation_rejection_refuses_the_attended_fallback() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: threads[0].id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let turn_config = state.config.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let approval_channels = Arc::clone(&state.approval_channels);
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        request_sandbox_degradation_approval(
            &state_for_task,
            SandboxDegradationApprovalRequest {
                config: &turn_config,
                ws_tx: &tx,
                turn_id: turn.id,
                agent_id: "gateway",
                prompt: "Sandbox setup failed. Approve unsandboxed execution?".to_string(),
                context: serde_json::json!({ "degradation": "unsandboxed_execution" }),
                cancel_token: &cancel_token,
            },
        )
        .await
    });

    let approval_id = match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired { approval_id, .. } => approval_id,
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    };
    approval_channels
        .lock()
        .unwrap()
        .remove(&approval_id)
        .expect("approval channel should be registered")
        .send(swage_agent::ApprovalDecision {
            decision: swage_agent::approval::ApprovalDecisionValue::Rejected,
            scope: swage_agent::approval::ApprovalDecisionScope::Once,
            reason: Some("Keep isolation mandatory".into()),
        })
        .expect("rejection should be delivered");

    let outcome = task
        .await
        .expect("approval task should finish")
        .expect("rejection should be a resolved approval outcome");
    assert_eq!(outcome, SandboxDegradationApproval::Rejected);
}

#[tokio::test]
async fn sandbox_degradation_approval_preserves_cancelled_wait() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: threads[0].id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let turn_config = state.config.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let cancel_for_task = cancel_token.clone();
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        request_sandbox_degradation_approval(
            &state_for_task,
            SandboxDegradationApprovalRequest {
                config: &turn_config,
                ws_tx: &tx,
                turn_id: turn.id,
                agent_id: "gateway",
                prompt: "Sandbox setup failed. Approve unsandboxed execution?".to_string(),
                context: serde_json::json!({ "degradation": "unsandboxed_execution" }),
                cancel_token: &cancel_for_task,
            },
        )
        .await
    });

    match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired { .. } => {}
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    }

    cancel_token.cancel();

    let outcome = task
        .await
        .expect("approval task should finish")
        .expect("approval should complete as cancelled");
    assert_eq!(outcome, SandboxDegradationApproval::Interrupted);

    let stored_turn = state
        .storage
        .get_turn(turn.id)
        .await
        .unwrap()
        .expect("turn should exist");
    assert_eq!(stored_turn.state, TurnState::AwaitingApproval);
}

#[tokio::test]
async fn sandbox_degradation_timeout_interrupts_and_cleans_channel() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: threads[0].id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let mut turn_config = state.config.clone();
    turn_config.hitl.timeout = 0;
    turn_config.hitl.timeout_policy = ApprovalTimeoutPolicy::Interrupt;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        request_sandbox_degradation_approval(
            &state_for_task,
            SandboxDegradationApprovalRequest {
                config: &turn_config,
                ws_tx: &tx,
                turn_id: turn.id,
                agent_id: "gateway",
                prompt: "Sandbox setup failed. Approve unsandboxed execution?".to_string(),
                context: serde_json::json!({ "degradation": "unsandboxed_execution" }),
                cancel_token: &cancel_token,
            },
        )
        .await
    });

    let approval_id = match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired { approval_id, .. } => approval_id,
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    };

    let outcome = task
        .await
        .expect("approval task should finish")
        .expect("approval should complete as interrupted");
    assert_eq!(outcome, SandboxDegradationApproval::Interrupted);

    assert!(
        state
            .approval_channels
            .lock()
            .unwrap()
            .get(&approval_id)
            .is_none(),
        "timed-out degradation approvals must not leave stale decision channels"
    );
    let approval = state
        .storage
        .get_approval(approval_id)
        .await
        .unwrap()
        .expect("approval should exist");
    assert_eq!(approval.state, ApprovalState::Expired);
}

#[test]
fn approval_required_routes_to_ws_tx() {
    // When the agent emits ApprovalRequired, the session-scoped ws_tx
    // should receive the corresponding ServerEvent — not the broadcast channel.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let approval_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();

    route_approval_agent_event(
        AgentEvent::ApprovalRequired {
            approval_id,
            turn_id,
            agent_id: "agent-1".to_string(),
            kind: "tool".to_string(),
            tool_name: Some("shell".to_string()),
            tool_arguments: Some(serde_json::json!({"cmd": "ls"})),
            hook_name: None,
            prompt: "Allow shell?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        },
        &tx,
    );

    match rx.try_recv().expect("ws_tx must receive the event") {
        ServerEvent::ApprovalRequired {
            approval_id: aid,
            turn_id: tid,
            agent_id,
            kind,
            tool_name,
            ..
        } => {
            assert_eq!(aid, approval_id);
            assert_eq!(tid, turn_id);
            assert_eq!(agent_id, "agent-1");
            assert_eq!(kind, "tool");
            assert_eq!(tool_name, Some("shell".to_string()));
        }
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    }
}

#[test]
fn approval_confirmed_routes_to_ws_tx() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let approval_id = Uuid::new_v4();

    route_approval_agent_event(
        AgentEvent::ApprovalConfirmed {
            approval_id,
            decision: swage_agent::approval::ApprovalDecisionValue::Approved,
            reason: Some("looks safe".to_string()),
        },
        &tx,
    );

    match rx.try_recv().expect("ws_tx must receive the event") {
        ServerEvent::ApprovalConfirmed {
            approval_id: aid,
            decision,
            reason,
        } => {
            assert_eq!(aid, approval_id);
            assert_eq!(decision, "approved");
            assert_eq!(reason, Some("looks safe".to_string()));
        }
        other => panic!("expected ApprovalConfirmed, got: {other:?}"),
    }
}

#[test]
fn approval_expired_routes_to_ws_tx() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let approval_id = Uuid::new_v4();

    route_approval_agent_event(
        AgentEvent::ApprovalExpired {
            approval_id,
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        },
        &tx,
    );

    match rx.try_recv().expect("ws_tx must receive the event") {
        ServerEvent::ApprovalExpired {
            approval_id: aid,
            timeout_policy,
        } => {
            assert_eq!(aid, approval_id);
            assert_eq!(timeout_policy, "reject");
        }
        other => panic!("expected ApprovalExpired, got: {other:?}"),
    }
}

#[test]
fn approval_events_routed_to_ws_tx_not_broadcast() {
    // Two separate mpsc channels simulating two different sessions.
    // Only the channel passed to route_approval_agent_event should receive
    // the event — the other remains empty, proving session-scoped routing.
    let (tx_a, mut rx_a) = mpsc::unbounded_channel::<ServerEvent>();
    let (_tx_b, mut rx_b) = mpsc::unbounded_channel::<ServerEvent>();

    route_approval_agent_event(
        AgentEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "agent-a".to_string(),
            kind: "tool".to_string(),
            tool_name: None,
            tool_arguments: None,
            hook_name: None,
            prompt: "Allow?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        },
        &tx_a, // only agent-a's channel
    );

    // agent-a's client should receive the event.
    assert!(
        rx_a.try_recv().is_ok(),
        "agent-a's ws_tx must receive the ApprovalRequired event"
    );
    // agent-b's client must NOT receive it — routing is session-scoped.
    assert!(
        rx_b.try_recv().is_err(),
        "agent-b's ws_tx must not receive agent-a's approval event"
    );
}

#[test]
fn delegation_result_renders_response_and_structured_output() {
    let message = swage_storage::model::AgentMessage {
        id: Uuid::new_v4(),
        sender_agent_id: "researcher".into(),
        recipient_agent_id: "gateway".into(),
        kind: swage_core::MessageKind::Result,
        correlation_id: Some(Uuid::new_v4()),
        payload: serde_json::json!({
            "response": "Gathered 3 sources.",
            "structured_output": {"sources": 3},
            "input_tokens": 10,
            "output_tokens": 20,
        }),
        status: swage_core::MessageStatus::Pending,
        failure_reason: None,
        created_at: chrono::Utc::now(),
        delivered_at: None,
    };

    let rendered = render_delegation_result(&message);
    assert!(rendered.contains("researcher"));
    assert!(rendered.contains("Gathered 3 sources."));
    assert!(rendered.contains("\"sources\": 3"));
}

#[test]
fn delegation_result_renders_failure_and_unknown_shapes() {
    let base = |payload| swage_storage::model::AgentMessage {
        id: Uuid::new_v4(),
        sender_agent_id: "researcher".into(),
        recipient_agent_id: "gateway".into(),
        kind: swage_core::MessageKind::Result,
        correlation_id: None,
        payload,
        status: swage_core::MessageStatus::Pending,
        failure_reason: None,
        created_at: chrono::Utc::now(),
        delivered_at: None,
    };

    let failed = render_delegation_result(&base(serde_json::json!({"error": "budget exhausted"})));
    assert!(failed.contains("failed"));
    assert!(failed.contains("budget exhausted"));

    // An unrecognised payload is still delivered, never dropped.
    let unknown = render_delegation_result(&base(serde_json::json!({"verdict": "unclear"})));
    assert!(unknown.contains("verdict"));
}

fn vision_capable_config() -> swage_config::ResolvedConfig {
    // The default provider protocol (OpenAI) advertises vision, which is
    // what the intake gate reads.
    let mut config = swage_config::ResolvedConfig::default();
    config.providers.entries.insert(
        "openai".into(),
        swage_config::ProviderEndpointConfig {
            protocol: swage_config::Protocol::OpenAi,
            ..Default::default()
        },
    );
    config.models.default = "gpt".into();
    config.models.entries.insert(
        "gpt".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "openai".into(),
            model_id: "gpt-4o".into(),
            ..Default::default()
        },
    );
    config
}

fn text_only_config() -> swage_config::ResolvedConfig {
    let mut config = swage_config::ResolvedConfig::default();
    config.providers.entries.insert(
        "codex".into(),
        swage_config::ProviderEndpointConfig {
            protocol: swage_config::Protocol::Codex,
            ..Default::default()
        },
    );
    config.models.default = "codex".into();
    config.models.entries.insert(
        "codex".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "codex".into(),
            model_id: "fake-codex".into(),
            ..Default::default()
        },
    );
    config
}

/// The image intake journey at the gateway seam: an image part submitted
/// beside the text is recorded with the user message in the wire's own
/// shape, rebuilt into history as a content part, and reaches the provider
/// request as `ContentPart::ImageUrl` in submission order.
#[tokio::test(flavor = "multi_thread")]
async fn an_image_attachment_is_recorded_and_reaches_the_provider_as_a_content_part() {
    let storage = Arc::new(MockStorage::new());
    let (provider, captured_requests) = CapturingMultiRoundProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "a cat".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]]);
    let space = storage.get_or_create_default().await.expect("space");
    let session = storage
        .create_session(NewSession {
            space_id: space.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("session");
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("thread");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        vision_capable_config(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    assert!(state.workflow_capabilities.vision);

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session.id,
        None,
        TurnInput::with_attachments(
            "What is in this picture?",
            vec![MessageAttachment::ImageUrl {
                url: "https://example.test/cat.png".into(),
            }],
        ),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn with an image completes");

    // Recorded beside the text, in the wire's own shape.
    let stored = storage.db.lock().await.messages.clone();
    let user = stored
        .iter()
        .find(|message| message.role == Role::User)
        .expect("stored user message");
    assert_eq!(user.content, "What is in this picture?");
    assert_eq!(
        user.attachments,
        vec![serde_json::json!({"kind": "image_url", "url": "https://example.test/cat.png"})]
    );

    // On the provider request as content parts, text first.
    let requests = captured_requests.lock().expect("captured requests");
    let user_message = requests[0]
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::User
                && message.text_content().contains("What is in this picture?")
        })
        .expect("user message reached the provider");
    assert_eq!(
        user_message.content(),
        &[
            ContentPart::Text {
                text: "What is in this picture?".into()
            },
            ContentPart::ImageUrl {
                url: "https://example.test/cat.png".into()
            },
        ]
    );
}

/// A provider that cannot consume images refuses the submission before any
/// record is written: a part the model never sees must not ride along.
#[tokio::test(flavor = "multi_thread")]
async fn an_image_attachment_is_refused_before_persistence_when_the_provider_lacks_vision() {
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) =
        make_state_with_config_and_storage(vec![], text_only_config(), Arc::clone(&storage)).await;
    assert!(!state.workflow_capabilities.vision);

    let (tx, _rx) = mpsc::unbounded_channel();
    let error = process_turn(
        &state,
        session_id,
        None,
        TurnInput::with_attachments(
            "What is in this picture?",
            vec![MessageAttachment::ImageUrl {
                url: "https://example.test/cat.png".into(),
            }],
        ),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect_err("an unsupported attachment must fail the turn");
    assert!(
        matches!(&error, GatewayError::ValidationError(message) if message.contains("image")),
        "{error}"
    );

    let db = storage.db.lock().await;
    assert!(
        db.messages.is_empty(),
        "nothing is persisted for a refused turn"
    );
    assert!(
        db.turns.is_empty(),
        "no turn record is created for a refused turn"
    );
}

/// A stored attachment outside the wire contract is corruption, refused
/// when history is rebuilt rather than dropped.
#[tokio::test(flavor = "multi_thread")]
async fn a_stored_attachment_outside_the_contract_fails_the_history_rebuild() {
    let storage = MockStorage::new();
    let thread_id = Uuid::new_v4();
    storage
        .append_message(AppendMessage {
            thread_id,
            attachments: vec![serde_json::json!({"kind": "hologram", "url": "x"})],
            turn_id: None,
            role: Role::User,
            content: "look".into(),
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();

    let error = super::history::rebuild_thread_history_messages(&storage, thread_id)
        .await
        .expect_err("an out-of-contract attachment must not be silently dropped");
    assert!(
        matches!(
            &error,
            GatewayError::Storage(StorageError::Corruption { message }) if message.contains("hologram")
        ),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delivered_delegation_results_survive_the_history_rebuild() {
    let storage = MockStorage::new();
    let thread_id = Uuid::new_v4();

    // The delivery loop's role choice is load-bearing: the rebuild carries
    // Tool-role rows only when they answer a recorded tool call, so a
    // platform delivery on that role would be recorded yet never reach the
    // model. Pin both halves.
    storage
        .append_message(AppendMessage {
            thread_id,
            attachments: Vec::new(),
            turn_id: None,
            role: Role::User,
            content: "Delegated agent researcher completed:\nGathered 3 sources.".into(),
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();
    storage
        .append_message(AppendMessage {
            thread_id,
            attachments: Vec::new(),
            turn_id: None,
            role: Role::Tool,
            content: "orphaned tool row".into(),
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();

    let rebuilt = super::history::rebuild_thread_history_messages(&storage, thread_id)
        .await
        .unwrap();

    assert!(
        rebuilt
            .iter()
            .any(|m| m.text_content().contains("Gathered 3 sources.")),
        "a delivered delegation result must reach the model as history"
    );
    assert!(
        !rebuilt
            .iter()
            .any(|m| m.text_content().contains("orphaned tool row")),
        "a tool row answering no recorded call is dropped by the rebuild — \
         which is why deliveries must not use Role::Tool"
    );
}
