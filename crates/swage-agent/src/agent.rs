//! Core agent struct and public API.
//!
//! The [`Agent`] is the central runtime component. It owns all dependencies,
//! manages sessions and threads, and processes messages through the LLM with
//! tool dispatch, hooks, leak detection, and context compaction.

use std::future::Future;
use std::sync::Arc;

use uuid::Uuid;

use serde_json::Value;
use swage_core::{
    ChatMessage, CompletionRequest, Role, StreamChunk, ToolCall, ToolReplayReasoning, ToolResult,
    build_tool_round_messages_with_reasoning, compact_tool_arguments_for_model,
    compact_tool_result_for_model_with_reference, extract_tool_result_metadata,
    tool_result_reference,
};
use swage_hook::{HookEvent, HookOutcome, HookRegistry};
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::{NewToolCall, NewTurn, Storage, ThreadState, TurnState};
use swage_tool::dispatch::ToolDispatcher;
use swage_tool::output::ToolOutput;

use crate::approval::{ApprovalChannelRegistrar, PreToolUseOutcome};
use crate::config::AgentConfig;
use crate::context::{AgentToolContext, TurnExecutionContext};
use crate::error::AgentError;
use crate::event::{AgentEvent, ProviderRoundOutcome, SchemaValidationError};
use crate::extraction::extract_json_values;
use crate::message::{OutboundMessage, TurnRequest};

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use swage_core::{PermissionDeniedReason, ToolError};

    use super::{compact_tool_error_detail, format_tool_error_for_model};

    #[test]
    fn format_tool_error_for_model_keeps_invalid_parameters_compact() {
        let error = ToolError::InvalidParameters {
            name: "file_edit".to_string(),
            message:
                "missing required field `edits`\n\nschema dump: {\"properties\":{\"edits\":[]}}"
                    .to_string(),
        };

        let formatted = format_tool_error_for_model(&error);

        assert_eq!(
            formatted,
            "Tool error: invalid parameters for `file_edit`: missing required field `edits`. Correct the arguments and retry."
        );
        assert!(!formatted.contains("schema dump"));
    }

    #[test]
    fn format_tool_error_for_model_labels_permission_denial() {
        let error = ToolError::PermissionDenied {
            name: "shell".to_string(),
            reason: PermissionDeniedReason::ExplicitlyDenied,
        };

        let formatted = format_tool_error_for_model(&error);

        assert_eq!(
            formatted,
            "Tool error: `shell` is not permitted for this turn (explicitly denied). Choose an allowed tool or ask for access."
        );
    }

    #[test]
    fn format_tool_error_for_model_labels_timeout() {
        let error = ToolError::Timeout {
            name: "web_search".to_string(),
            duration: Duration::from_secs(30),
        };

        let formatted = format_tool_error_for_model(&error);

        assert_eq!(
            formatted,
            "Tool error: `web_search` timed out after 30s. Retry with a smaller or simpler request."
        );
    }

    #[test]
    fn compact_tool_error_detail_collapses_whitespace_and_truncates() {
        let detail = compact_tool_error_detail(&format!("{}\n{}", "x ".repeat(400), "ignored"));

        assert!(detail.ends_with("..."));
        assert!(detail.len() <= 320);
        assert!(!detail.contains('\n'));
        assert!(!detail.contains("ignored"));
    }

    #[test]
    fn compact_tool_error_detail_keeps_traceback_exception_tail() {
        let detail = compact_tool_error_detail(
            "Traceback (most recent call last):\n  File \"x.py\", line 1\nZeroDivisionError: division by zero",
        );

        assert!(detail.contains("Traceback (most recent call last):"));
        assert!(detail.contains("ZeroDivisionError: division by zero"));
    }
}

#[cfg(test)]
mod approval_tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use swage_storage::model::{ApprovalKind, NewSession, NewThread, NewTurn};
    use swage_storage::store::{SessionStore, ThreadStore, TopicStore, TurnStore};
    use swage_storage::{SqliteStorage, Storage, TurnState};

    use super::*;

    async fn approval_fixture(
        registrar: ApprovalChannelRegistrar,
    ) -> (Agent, TurnExecutionContext) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory storage"));
        let topic = storage.general_scope().await.expect("default topic");
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "agent".into(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        let thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: "agent".into(),
            },
        )
        .await
        .expect("thread");
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent".into(),
                state: TurnState::Pending,
                user_input: Some("test approval".into()),
                parent_turn_id: None,
            })
            .await
            .expect("turn");
        let agent = Agent {
            config: AgentConfig::new("agent").with_workspace("/work", vec!["/work".into()]),
            provider: Arc::new(swage_provider::UnsupportedLlmProvider::new("not called")),
            storage: Arc::clone(&storage) as Arc<dyn Storage>,
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: None,
            approval_registrar: Some(registrar),
            delegation_handler: None,
            context_transfer_config: swage_config::ContextTransferConfig::default(),
        };
        let execution = TurnExecutionContext {
            session_id: session.id,
            thread_id: thread.id,
            turn_id: turn.id,
            topic_id: Some(topic.id),
        };
        (agent, execution)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn immediate_rejection_after_approval_notification_keeps_its_sender() {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let event_rx = Arc::new(Mutex::new(event_rx));
        let registrar_event_rx = Arc::clone(&event_rx);
        let notification_preceded_registration = Arc::new(AtomicBool::new(false));
        let registrar_observation = Arc::clone(&notification_preceded_registration);
        let registered_approval_id = Arc::new(Mutex::new(None));
        let registrar_approval_id = Arc::clone(&registered_approval_id);
        let registrar: ApprovalChannelRegistrar = Arc::new(move |approval_id, sender| {
            if registrar_event_rx
                .lock()
                .expect("approval events")
                .try_recv()
                .is_ok()
            {
                registrar_observation.store(true, Ordering::SeqCst);
            }
            *registrar_approval_id
                .lock()
                .expect("registered approval id") = Some(approval_id);
            sender
                .send(crate::approval::ApprovalDecision {
                    decision: crate::approval::ApprovalDecisionValue::Rejected,
                    scope: crate::approval::ApprovalDecisionScope::Once,
                    reason: Some("immediate rejection".into()),
                })
                .expect("registered sender accepts an immediate rejection");
        });
        let (agent, execution) = approval_fixture(registrar).await;
        let outcome = agent
            .handle_approval_gate(
                &execution,
                ApprovalKind::Tool,
                Some("shell".into()),
                None,
                None,
                "Allow shell?".into(),
                None,
                std::time::Duration::from_secs(5),
                swage_core::ApprovalTimeoutPolicy::Reject,
                &event_tx,
                &tokio_util::sync::CancellationToken::new(),
                &mut TurnApprovalScope::default(),
            )
            .await
            .expect("approval gate");

        assert!(
            !notification_preceded_registration.load(Ordering::SeqCst),
            "ApprovalRequired became observable before its decision sender was registered"
        );
        let approval_id = match event_rx
            .lock()
            .expect("approval events")
            .try_recv()
            .expect("approval event after registration")
        {
            AgentEvent::ApprovalRequired { approval_id, .. } => approval_id,
            event => panic!("expected approval event, got {event:?}"),
        };
        assert_eq!(
            *registered_approval_id
                .lock()
                .expect("registered approval id"),
            Some(approval_id)
        );
        assert!(matches!(
            outcome,
            crate::approval::ApprovalGateOutcome::Rejected { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_tool_approval_emits_settlement_log() {
        use tracing_subscriber::{Registry, layer::SubscriberExt};

        type RecordedMessages = Arc<Mutex<Vec<(Option<String>, Option<String>)>>>;

        #[derive(Default)]
        struct MessageRecorder(RecordedMessages);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for MessageRecorder {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _context: tracing_subscriber::layer::Context<'_, S>,
            ) {
                #[derive(Default)]
                struct MessageVisitor {
                    message: Option<String>,
                    approval_id: Option<String>,
                }

                impl tracing::field::Visit for MessageVisitor {
                    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                        if field.name() == "message" {
                            self.message = Some(value.to_owned());
                        } else if field.name() == "approval_id" {
                            self.approval_id = Some(value.to_owned());
                        }
                    }

                    fn record_debug(
                        &mut self,
                        field: &tracing::field::Field,
                        value: &dyn std::fmt::Debug,
                    ) {
                        if field.name() == "message" {
                            self.message = Some(format!("{value:?}"));
                        } else if field.name() == "approval_id" {
                            self.approval_id = Some(format!("{value:?}"));
                        }
                    }
                }

                let mut visitor = MessageVisitor::default();
                event.record(&mut visitor);
                if visitor.message.is_some() {
                    self.0
                        .lock()
                        .expect("recorded log messages")
                        .push((visitor.message, visitor.approval_id));
                }
            }
        }

        let recorder = MessageRecorder::default();
        let messages = Arc::clone(&recorder.0);
        let subscriber = Registry::default().with(recorder);
        tracing::subscriber::set_global_default(subscriber)
            .expect("approval log recorder must be the crate test process subscriber");

        let registrar: ApprovalChannelRegistrar = Arc::new(|_approval_id, sender| {
            sender
                .send(crate::approval::ApprovalDecision {
                    decision: crate::approval::ApprovalDecisionValue::Rejected,
                    scope: crate::approval::ApprovalDecisionScope::Once,
                    reason: Some("not allowed".into()),
                })
                .expect("registered sender accepts rejection");
        });
        let (agent, execution) = approval_fixture(registrar).await;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = agent
            .handle_approval_gate(
                &execution,
                ApprovalKind::Tool,
                Some("shell".into()),
                None,
                None,
                "Allow shell?".into(),
                None,
                std::time::Duration::from_secs(5),
                swage_core::ApprovalTimeoutPolicy::Reject,
                &event_tx,
                &tokio_util::sync::CancellationToken::new(),
                &mut TurnApprovalScope::default(),
            )
            .await
            .expect("approval gate");
        assert!(matches!(
            outcome,
            crate::approval::ApprovalGateOutcome::Rejected { .. }
        ));
        let approval_id = match event_rx.try_recv().expect("approval event") {
            AgentEvent::ApprovalRequired { approval_id, .. } => approval_id.to_string(),
            event => panic!("expected approval event, got {event:?}"),
        };

        assert!(
            messages.lock().expect("recorded log messages").iter().any(
                |(message, recorded_approval_id)| {
                    message.as_deref() == Some("tool approval rejected and settled")
                        && recorded_approval_id.as_deref() == Some(approval_id.as_str())
                }
            ),
            "human rejection must emit its settlement log"
        );
    }
}

#[cfg(test)]
mod compaction_caller_tests {
    use std::{pin::Pin, sync::Arc};

    use async_trait::async_trait;
    use futures::{Stream, stream};
    use swage_core::{
        CompletionResponse, FinishReason, ProviderError, ToolCompletionRequest,
        ToolCompletionResponse,
    };
    use swage_storage::model::{NewSession, NewThread};
    use swage_storage::store::{SessionStore, ThreadStore, TopicStore};
    use swage_storage::{SqliteStorage, Storage};

    use super::*;
    use crate::config::CompactionThreshold;

    struct NoNativeCountingProvider;

    #[async_trait]
    impl LlmProvider for NoNativeCountingProvider {
        fn model_id(&self) -> &str {
            "no-native-counting"
        }

        async fn count_tokens(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Option<u64>, ProviderError> {
            Ok(None)
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: "brief".into(),
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
            unreachable!("the test agent has no tools")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(stream::iter([
                StreamChunk::Delta {
                    content: "completed".into(),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop),
                },
            ]))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_without_native_counting_can_still_compact() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory storage"));
        let topic = storage.general_scope().await.expect("default topic");
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "agent".into(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        let thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: "agent".into(),
            },
        )
        .await
        .expect("thread");
        let mut config = AgentConfig::new("agent").with_workspace("/work", vec!["/work".into()]);
        config.compaction_threshold = CompactionThreshold::TokenCount(25);
        let agent = Agent {
            config,
            provider: Arc::new(NoNativeCountingProvider),
            storage: Arc::clone(&storage) as Arc<dyn Storage>,
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: None,
            approval_registrar: None,
            delegation_handler: None,
            context_transfer_config: swage_config::ContextTransferConfig::default(),
        };
        let messages = ["a", "b", "c", "d"]
            .into_iter()
            .map(|prefix| ChatMessage::new(Role::User, format!("{prefix}{}", "x".repeat(39))))
            .collect();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        let outbound = agent
            .process_turn_streaming(
                TurnRequest {
                    execution: TurnExecutionContext {
                        session_id: session.id,
                        thread_id: thread.id,
                        turn_id: Uuid::new_v4(),
                        topic_id: Some(topic.id),
                    },
                    user_message: "continue".into(),
                    assembled_context: messages,
                    compaction_focus: None,
                    parent_turn_id: None,
                    existing_turn_id: None,
                    effort: None,
                    cancellation_token: tokio_util::sync::CancellationToken::new(),
                },
                event_tx,
            )
            .await
            .expect("estimate-only threshold breach must compact successfully");

        assert_eq!(outbound.response, "completed");
        assert!(
            std::iter::from_fn(|| event_rx.try_recv().ok())
                .any(|event| matches!(event, AgentEvent::CompactionApplied { .. })),
            "the production caller must take the compaction path"
        );
    }
}
// Agent struct
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct TurnApprovalScope {
    approves_remaining_gates: bool,
}

impl TurnApprovalScope {
    fn approve_remaining_gates(&mut self) {
        self.approves_remaining_gates = true;
    }

    fn should_skip_gate(&self) -> bool {
        self.approves_remaining_gates
    }
}

/// A single-agent runtime that processes messages through an LLM.
pub struct Agent {
    pub(crate) config: AgentConfig,
    pub(crate) provider: Arc<dyn LlmProvider>,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) tool_dispatcher: Option<ToolDispatcher>,
    pub(crate) hook_registry: Option<Arc<HookRegistry>>,
    pub(crate) leak_detector: Option<Arc<LeakDetector>>,

    /// Callback to register a oneshot channel for HITL approval decisions.
    /// When set, the agent can pause tool execution and wait for human review.
    pub(crate) approval_registrar: Option<ApprovalChannelRegistrar>,

    /// Handler for tool-as-delegation — called when the LLM invokes the
    /// "delegate" tool. When `None`, delegation tool calls return an error.
    pub(crate) delegation_handler: Option<Arc<dyn crate::delegation::DelegationHandler>>,

    /// Context transfer policy applied when this agent delegates work.
    pub(crate) context_transfer_config: swage_config::ContextTransferConfig,
}

// ---------------------------------------------------------------------------
// Session management
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Message processing — the core execution loop
// ---------------------------------------------------------------------------

impl Agent {
    /// Process a turn through the streaming execution path.
    ///
    /// This method:
    /// - Takes pre-assembled context rather than building it from thread history
    /// - Uses `provider.stream()` for real-time token emission
    /// - Emits [`AgentEvent`]s over `event_tx` for gateway/client forwarding
    /// - Supports cooperative cancellation via `request.cancellation_token`
    ///
    /// Turn state transitions: Pending → Processing → Completed (or Interrupted on
    /// cancellation, Failed on error).
    pub async fn process_turn_streaming(
        &self,
        request: TurnRequest,
        event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<OutboundMessage, AgentError> {
        let execution = request.execution;
        let thread_id = execution.thread_id;
        let user_message = request.user_message;
        let parent_turn_id = request.parent_turn_id;
        let existing_turn_id = request.existing_turn_id;
        let cancel_token = request.cancellation_token;
        let compaction_focus = request.compaction_focus;

        // 1. Create a Pending turn (or reuse a pre-created placeholder) before
        // any work begins so the lifecycle is recorded durably.
        let turn = if let Some(existing_turn_id) = existing_turn_id {
            let turn = self
                .storage
                .get_turn(existing_turn_id)
                .await?
                .ok_or_else(|| {
                    AgentError::InvalidConfig(format!(
                        "existing turn {existing_turn_id} was not found"
                    ))
                })?;

            if turn.thread_id != thread_id {
                return Err(AgentError::InvalidConfig(format!(
                    "existing turn {} belongs to thread {}, expected {}",
                    turn.id, turn.thread_id, thread_id
                )));
            }
            if turn.agent_id != self.config.agent_id {
                return Err(AgentError::InvalidConfig(format!(
                    "existing turn {} belongs to agent {}, expected {}",
                    turn.id, turn.agent_id, self.config.agent_id
                )));
            }
            if turn.state != TurnState::Pending {
                return Err(AgentError::InvalidConfig(format!(
                    "existing turn {} must be pending, found {}",
                    turn.id, turn.state
                )));
            }

            turn
        } else {
            self.storage
                .create_turn(NewTurn {
                    thread_id,
                    agent_id: self.config.agent_id.clone(),
                    state: TurnState::Pending,
                    user_input: Some(user_message.clone()),
                    parent_turn_id,
                })
                .await?
        };

        // 2. Emit TurnStarted — gateway can show "processing…" to the client.
        let _ = event_tx.send(AgentEvent::TurnStarted { turn_id: turn.id });

        let execution = TurnExecutionContext {
            turn_id: turn.id,
            ..execution
        };

        // 3. Transition turn → Processing, thread → Processing.
        self.storage
            .update_turn_state(turn.id, TurnState::Processing)
            .await?;
        self.storage
            .update_state(thread_id, ThreadState::Processing)
            .await?;

        let mut approval_scope = TurnApprovalScope::default();
        // Permissions disabled at the gateway, or an unattended caller:
        // pre-approve every gate this turn. Each gate (tool, location, hook)
        // consults `should_skip_gate` and falls through to `dispatch_approved`,
        // so this quiets prompts without changing dispatcher grant decisions.
        if self.config.skip_permissions || !self.config.approval_prompts_enabled {
            approval_scope.approve_remaining_gates();
        }

        let threshold_evaluation = crate::compaction::exceeds_threshold_with_model(
            &request.assembled_context,
            &self.config.compaction_threshold,
            self.provider.as_ref(),
            self.config.model_id.as_deref(),
            false,
        )
        .await;
        let assembled_context = if threshold_evaluation.exceeds {
            match crate::compaction::compact_with_model(
                request.assembled_context,
                &self.config.compaction_threshold,
                self.provider.as_ref(),
                self.storage.as_ref(),
                thread_id,
                compaction_focus.as_deref(),
                self.config.model_id.as_deref(),
                threshold_evaluation.native_breach,
            )
            .await
            {
                Ok((context, record)) => {
                    if let Some(record) = record {
                        let _ = event_tx.send(AgentEvent::CompactionApplied {
                            turn_id: turn.id,
                            record,
                        });
                    }
                    context
                }
                Err(error) => {
                    self.fail_turn_and_idle(turn.id, thread_id).await;
                    let _ = event_tx.send(AgentEvent::Error {
                        message: error.to_string(),
                        provider_failure: false,
                    });
                    return Err(error);
                }
            }
        } else {
            request.assembled_context
        };

        // 4. Build initial CompletionRequest from the assembled context.
        let output_requirements = self.config.output_schema.as_ref().map(|schema| {
            format!(
                "## Output Requirements\n\nReturn ONLY a JSON value that conforms to this JSON Schema. The schema is authoritative, including required properties and whether additional properties are allowed.\n\n{}",
                serde_json::to_string_pretty(schema)
                    .expect("a validated JSON Schema value must serialise")
            )
        });
        let mut assembled_context = assembled_context;
        if let Some(requirements) = &output_requirements {
            assembled_context.push(ChatMessage::new(Role::System, requirements));
        }
        let mut llm_request = CompletionRequest {
            messages: assembled_context,
            model: self.config.model_id.clone(),
            effort: request.effort.clone(),
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            stop_sequences: self.config.stop_sequences.clone(),
            metadata: self.config.generation_metadata.clone(),
            tool_definitions: self
                .tool_dispatcher
                .as_ref()
                .map(ToolDispatcher::tool_definitions)
                .unwrap_or_default(),
        };

        // 5–6. Streaming LLM loop with tool dispatch.
        let mut accumulated = String::new();
        let mut total_input_tokens: u32 = 0;
        let mut total_output_tokens: u32 = 0;
        // Whether any provider round reported usage at all. A provider that
        // reports none leaves the turn's token columns unset rather than at
        // zero, so a reader can tell "spent nothing" from "cannot say".
        let mut usage_reported = false;
        let mut total_tool_calls: u32 = 0;
        let mut tool_position: i32 = 0;
        let mut round = 0usize;

        'outer: loop {
            // Check cancellation eagerly before starting each round. This
            // catches pre-cancelled tokens reliably — tokio::select! inside
            // the stream loop is non-deterministic when both branches are
            // immediately ready.
            if cancel_token.is_cancelled() {
                self.persist_cancelled_partial(thread_id, turn.id, &accumulated)
                    .await;
                let _ = self
                    .storage
                    .update_turn_state(turn.id, TurnState::Interrupted)
                    .await;
                let _ = self
                    .storage
                    .update_state(thread_id, ThreadState::Idle)
                    .await;
                return Err(AgentError::Cancelled);
            }

            round += 1;

            let round_outcome = self
                .consume_provider_stream(
                    llm_request.clone(),
                    &execution,
                    &event_tx,
                    &cancel_token,
                    &accumulated,
                    &mut total_input_tokens,
                    &mut total_output_tokens,
                    &mut usage_reported,
                    true,
                    round,
                )
                .await?;
            let round_text = round_outcome.text;
            let round_tool_calls = round_outcome.tool_calls;
            let round_reasoning = round_outcome.reasoning;
            accumulated.push_str(&round_text);

            // No tool calls → LLM produced a final text response for this round.
            if round_tool_calls.is_empty() {
                break 'outer;
            }

            // --- Tool dispatch cycle ---
            let tool_dispatch_result: Result<Vec<ToolResult>, AgentError> = async {
                let mut tool_results = Vec::with_capacity(round_tool_calls.len());

                for call in &round_tool_calls {
                    // Re-check cancellation before each tool execution.
                    if cancel_token.is_cancelled() {
                        let _ = self
                            .storage
                            .update_turn_state(turn.id, TurnState::Interrupted)
                            .await;
                        let _ = self
                            .storage
                            .update_state(thread_id, ThreadState::Idle)
                            .await;
                        return Err(AgentError::Cancelled);
                    }

                    // The call's record opens when the model selects it and is
                    // settled when execution finishes — both writes
                    // constitutive (record_durability.rs, class 1), so a turn
                    // killed mid-execution leaves a started row that recovery
                    // settles as interrupted rather than no row at all. A
                    // failed open refuses the dispatch: a call the record
                    // cannot show must not run.
                    if let Err(error) = self
                        .storage
                        .create_tool_call(NewToolCall {
                            turn_id: turn.id,
                            agent_id: self.config.agent_id.clone(),
                            call_id: call.call_id.clone(),
                            tool: call.name.clone(),
                            arguments: serde_json::to_string(&call.arguments).unwrap_or_default(),
                            model_arguments: compact_tool_arguments_for_model(&call.arguments),
                            result: None,
                            model_result: None,
                            status: None,
                            duration_ms: None,
                            truncated: false,
                            total_bytes: None,
                            provider_round: round as i32,
                            position: tool_position,
                            parent_call_id: None,
                            display_summary: self.tool_display_summary(call),
                        })
                        .await
                    {
                        let refusal =
                            format!("tool call refused: its record could not be written: {error}");
                        let _ = event_tx.send(AgentEvent::ToolEnd {
                            call_id: call.call_id.clone(),
                            tool_name: call.name.clone(),
                            result: Value::String(refusal.clone()),
                            status: "error".to_string(),
                            duration_ms: Some(0),
                        });
                        tool_position += 1;
                        total_tool_calls += 1;
                        tool_results.push(ToolResult {
                            call_id: call.call_id.clone(),
                            content: refusal,
                            is_error: true,
                        });
                        continue;
                    }

                    let start = std::time::Instant::now();

                    // PreToolUse hook — may block, request approval, or proceed.
                    let (result_content, mut is_error, status) = match self
                        .dispatch_pre_tool_use(&call.name, &call.arguments)
                        .await
                    {
                        Err(AgentError::HookBlocked { reason, .. }) => {
                            (reason, true, "blocked".to_string())
                        }
                        Err(e) => (format!("hook error: {e}"), true, "error".to_string()),
                        Ok(PreToolUseOutcome::RequestApproval {
                            prompt,
                            context,
                            timeout,
                        }) => {
                            if approval_scope.should_skip_gate() {
                                // A broader approval decision covers later gates
                                // within this still-running turn.
                                self.dispatch_tool_call(
                                    call,
                                    &execution,
                                    &llm_request.messages,
                                    &event_tx,
                                    &cancel_token,
                                    &mut approval_scope,
                                )
                                .await?
                            } else {
                                // Hook-driven approval gate.
                                use crate::approval::{
                                    ApprovalGateOutcome, DEFAULT_APPROVAL_TIMEOUT_SECS,
                                };
                                let gate_timeout = timeout.unwrap_or(
                                    std::time::Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
                                );
                                match self
                                    .handle_approval_gate(
                                        &execution,
                                        swage_storage::model::ApprovalKind::Hook,
                                        Some(call.name.clone()),
                                        Some(call.arguments.clone()),
                                        None,
                                        prompt,
                                        context,
                                        gate_timeout,
                                        swage_core::ApprovalTimeoutPolicy::default(),
                                        &event_tx,
                                        &cancel_token,
                                        &mut approval_scope,
                                    )
                                    .await?
                                {
                                    ApprovalGateOutcome::Approved => {
                                        // Approved — proceed with normal dispatch.
                                        self.dispatch_tool_call(
                                            call,
                                            &execution,
                                            &llm_request.messages,
                                            &event_tx,
                                            &cancel_token,
                                            &mut approval_scope,
                                        )
                                        .await?
                                    }
                                    ApprovalGateOutcome::Rejected { feedback } => {
                                        (feedback, true, "rejected".to_string())
                                    }
                                    ApprovalGateOutcome::Interrupted => {
                                        return Err(AgentError::Cancelled);
                                    }
                                }
                            }
                        }
                        Ok(PreToolUseOutcome::Proceed) => {
                            self.dispatch_tool_call(
                                call,
                                &execution,
                                &llm_request.messages,
                                &event_tx,
                                &cancel_token,
                                &mut approval_scope,
                            )
                            .await?
                        }
                    };

                    let duration_ms = start.elapsed().as_millis() as u64;

                    let result_value: Value = serde_json::from_str(&result_content)
                        .unwrap_or_else(|_| Value::String(result_content.clone()));
                    let result_metadata = extract_tool_result_metadata(&result_value);

                    // PostToolUse hook — informational, fire-and-forget.
                    self.dispatch_post_tool_use(&call.name, &call.arguments, &result_value)
                        .await;

                    // Leak detection on tool result before feeding back to the model.
                    let safe_result = self.scan_output(&result_content);
                    let model_result = compact_tool_result_for_model_with_reference(
                        &safe_result,
                        is_error,
                        tool_result_reference(&turn.id.to_string(), &call.call_id),
                    );

                    // Settle the started row. The raw result stays in the
                    // store; the scanned model-facing text is what history
                    // replays. A settlement that fails fails the call with the
                    // record failure named, never a result the trail omits —
                    // for the model and the client alike, so ToolEnd follows
                    // the settlement and carries its outcome.
                    let mut safe_result = safe_result;
                    let mut end_result = result_value;
                    let mut end_status = status.clone();
                    if let Err(error) = self
                        .storage
                        .update_tool_call_result(
                            &call.call_id,
                            turn.id,
                            &result_content,
                            &model_result,
                            &status,
                            duration_ms as i64,
                            result_metadata.truncated,
                            result_metadata.total_bytes,
                        )
                        .await
                    {
                        safe_result = format!(
                            "tool '{}' ran but its record could not be settled: {error}",
                            call.name
                        );
                        is_error = true;
                        end_result = Value::String(safe_result.clone());
                        end_status = "error".to_string();
                    }
                    let _ = event_tx.send(AgentEvent::ToolEnd {
                        call_id: call.call_id.clone(),
                        tool_name: call.name.clone(),
                        result: end_result,
                        status: end_status,
                        duration_ms: Some(duration_ms),
                    });
                    tool_position += 1;
                    total_tool_calls += 1;

                    tool_results.push(ToolResult {
                        call_id: call.call_id.clone(),
                        content: safe_result,
                        is_error,
                    });
                }

                Ok(tool_results)
            }
            .await;
            let tool_results = match tool_dispatch_result {
                Ok(tool_results) => tool_results,
                Err(AgentError::Cancelled) => {
                    // This function owns the accumulated partial: no cancellation
                    // from tool dispatch may leave without persisting it here.
                    self.persist_cancelled_partial(thread_id, turn.id, &accumulated)
                        .await;
                    // Settle every started row without a recorded result so
                    // history replay can explain the unknown outcome.
                    if let Err(error) = self
                        .storage
                        .settle_started_tool_calls(
                            turn.id,
                            swage_storage::store::INTERRUPTED_TOOL_RESULT,
                            swage_storage::store::INTERRUPTED_TOOL_STATUS,
                        )
                        .await
                    {
                        tracing::error!(turn_id = %turn.id, %error,
                            "cancelled tool call's record could not be settled");
                        let reason =
                            format!("cancelled tool call's record could not be settled: {error}");
                        let _ = event_tx.send(AgentEvent::RecordIntegrityLost {
                            turn_id: turn.id,
                            reason: reason.clone(),
                        });
                        let _ = event_tx.send(AgentEvent::Error {
                            message: reason,
                            provider_failure: false,
                        });
                    }
                    return Err(AgentError::Cancelled);
                }
                Err(error) => {
                    self.storage
                        .settle_started_tool_calls(
                            turn.id,
                            swage_storage::store::INTERRUPTED_TOOL_RESULT,
                            swage_storage::store::INTERRUPTED_TOOL_STATUS,
                        )
                        .await?;
                    return Err(error);
                }
            };

            // Build tool round messages and extend context for the next LLM call.
            let reasoning_replay = if self.config.replay_reasoning {
                round_reasoning.as_slice()
            } else {
                &[]
            };
            let tool_messages = build_tool_round_messages_with_reasoning(
                &round_tool_calls,
                &tool_results,
                &round_text,
                reasoning_replay,
            );
            let mut updated_messages = llm_request.messages.clone();
            updated_messages.extend(tool_messages);
            llm_request = CompletionRequest {
                messages: updated_messages,
                ..llm_request
            };
        }

        // 6b. Output schema validation — retry loop.
        //
        // When an output schema is configured, extract JSON from the accumulated
        // response, validate it, and retry the LLM call if validation fails. Each
        // retry streams tokens independently. Without a schema, this block is skipped.
        let structured_output = if let Some(ref schema_value) = self.config.output_schema {
            // Compile once — AgentConfig::validate() already proved this succeeds.
            let validator = swage_core::json_schema_validator(schema_value)
                .expect("schema was validated at config time");

            let max_retries = self.config.max_output_retries;
            let mut attempt: u32 = 0;

            loop {
                attempt += 1;

                // Step 1/2: validate every JSON value in source order. A model
                // may reason through provisional values before delivering its
                // result, so the final schema-valid value is authoritative.
                let mut last_valid = None;
                let mut last_errors = None;
                for extracted in extract_json_values(&accumulated) {
                    let errors: Vec<SchemaValidationError> = validator
                        .iter_errors(&extracted)
                        .map(|e| SchemaValidationError {
                            path: e.instance_path().to_string(),
                            message: e.to_string(),
                        })
                        .collect();
                    if errors.is_empty() {
                        last_valid = Some(extracted);
                    } else {
                        last_errors = Some(errors);
                    }
                }
                if let Some(extracted) = last_valid {
                    // Step 3: validation passed — carry the final valid value.
                    break Some(extracted);
                }
                let validation_errors = last_errors.unwrap_or_else(|| {
                    // No JSON found — treat as a single validation error.
                    vec![SchemaValidationError {
                        path: String::new(),
                        message: "no JSON found in response".into(),
                    }]
                });

                // Step 4/5: validation failed.
                if attempt > max_retries {
                    // Step 5: all retries exhausted.
                    let _ = event_tx.send(AgentEvent::OutputSchemaFailed {
                        turn_id: turn.id,
                        attempts: attempt,
                        errors: validation_errors,
                    });

                    // Persist the failed response for debugging.
                    let _ = self
                        .storage
                        .append_message(swage_storage::model::AppendMessage {
                            thread_id,
                            attachments: Vec::new(),
                            turn_id: Some(turn.id),
                            role: Role::Assistant,
                            content: accumulated.clone(),
                            agent_id: self.config.agent_id.clone(),
                        })
                        .await;

                    // Mark turn Failed, thread Idle.
                    self.fail_turn_and_idle(turn.id, thread_id).await;

                    return Err(AgentError::OutputSchemaFailed { attempts: attempt });
                }

                // Step 4: retries remain — emit retry event and loop.
                let _ = event_tx.send(AgentEvent::OutputSchemaRetry {
                    turn_id: turn.id,
                    attempt,
                    max_retries,
                    errors: validation_errors.clone(),
                });

                // Build ephemeral feedback: the assistant's failed response followed by
                // a user message describing the validation errors. Neither is persisted.
                let error_description = validation_errors
                    .iter()
                    .map(|e| {
                        if e.path.is_empty() {
                            format!("- {}", e.message)
                        } else {
                            format!("- {}: {}", e.path, e.message)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let output_requirements = output_requirements
                    .as_ref()
                    .expect("schema validation retries require output requirements");
                let feedback = format!(
                    "Your response did not match the required JSON schema. \
                     Please try again, producing ONLY valid JSON that conforms \
                     to the schema.\n\nValidation errors:\n{error_description}\n\n{output_requirements}"
                );

                let assistant_msg = ChatMessage::new(Role::Assistant, &accumulated);
                let feedback_msg = ChatMessage::new(Role::User, feedback);
                let mut updated_messages = llm_request.messages.clone();
                updated_messages.push(assistant_msg);
                updated_messages.push(feedback_msg);
                llm_request = CompletionRequest {
                    messages: updated_messages,
                    ..llm_request.clone()
                };

                // Clear accumulated text and re-enter the streaming loop for a fresh response.
                accumulated.clear();

                let retry_outcome = self
                    .consume_provider_stream(
                        llm_request.clone(),
                        &execution,
                        &event_tx,
                        &cancel_token,
                        &accumulated,
                        &mut total_input_tokens,
                        &mut total_output_tokens,
                        &mut usage_reported,
                        false,
                        round + attempt as usize,
                    )
                    .await?;
                accumulated = retry_outcome.text;
            }
        } else {
            // No output schema configured — skip validation entirely.
            None
        };

        // 8. AfterResponse hook — may modify or block the final accumulated response.
        let response_content = match self.dispatch_after_response(&accumulated).await {
            Ok(content) => content,
            Err(e) => {
                self.fail_turn_and_idle(turn.id, thread_id).await;
                let _ = event_tx.send(AgentEvent::Error {
                    message: e.to_string(),
                    provider_failure: false,
                });
                return Err(e);
            }
        };

        // 9. Leak detection — redact credentials from the assistant response.
        let safe_content = self.scan_output(&response_content);

        // 10. Persist assistant message.
        self.storage
            .append_message(swage_storage::model::AppendMessage {
                thread_id,
                attachments: Vec::new(),
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: safe_content.clone(),
                agent_id: self.config.agent_id.clone(),
            })
            .await?;

        // 11. Update turn: response text, token counts, state → Completed.
        self.storage.update_response(turn.id, &safe_content).await?;
        if usage_reported {
            self.storage
                .update_tokens(
                    turn.id,
                    total_input_tokens.into(),
                    total_output_tokens.into(),
                )
                .await?;
        }
        self.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await?;

        // 12. Thread → Idle.
        self.storage
            .update_state(thread_id, ThreadState::Idle)
            .await?;

        // 13. Emit TurnCompleted — final event, signals the turn is done.
        let _ = event_tx.send(AgentEvent::TurnCompleted {
            turn_id: turn.id,
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
            tool_calls_count: total_tool_calls,
            structured_output: structured_output.clone(),
        });

        // 14. Return outbound message.
        Ok(OutboundMessage {
            response: safe_content,
            turn_id: turn.id,
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
            tool_calls_count: total_tool_calls,
            structured_output,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn consume_provider_stream(
        &self,
        request: CompletionRequest,
        execution: &TurnExecutionContext,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        cancel_token: &tokio_util::sync::CancellationToken,
        accumulated_prefix: &str,
        total_input_tokens: &mut u32,
        total_output_tokens: &mut u32,
        usage_reported: &mut bool,
        collect_tool_calls: bool,
        round: usize,
    ) -> Result<StreamRoundOutcome, AgentError> {
        use futures::StreamExt;

        let request_snapshot = self.provider.provider_request_snapshot(&request, true);
        let _ = event_tx.send(AgentEvent::ProviderRequestPrepared {
            turn_id: execution.turn_id,
            round,
            request: request_snapshot,
        });

        let mut stream = self.provider.stream(request);
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut reasoning = Vec::new();

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    let accumulated_partial = format!("{accumulated_prefix}{text}");
                    self.persist_cancelled_partial(
                        execution.thread_id,
                        execution.turn_id,
                        &accumulated_partial,
                    ).await;
                    let _ = self.storage.update_turn_state(execution.turn_id, TurnState::Interrupted).await;
                    let _ = self.storage.update_state(execution.thread_id, ThreadState::Idle).await;
                    let _ = event_tx.send(AgentEvent::ProviderRoundSettled {
                        turn_id: execution.turn_id,
                        round,
                        outcome: ProviderRoundOutcome::Interrupted,
                    });
                    return Err(AgentError::Cancelled);
                }
                chunk = stream.next() => {
                    match chunk {
                        None | Some(StreamChunk::Done { .. }) => break,
                        Some(StreamChunk::Delta { content }) => {
                            text.push_str(&content);
                            let _ = event_tx.send(AgentEvent::Token { content });
                        }
                        Some(StreamChunk::Reasoning { content, details }) => {
                            let _ = event_tx.send(AgentEvent::ProviderReasoningCaptured {
                                turn_id: execution.turn_id,
                                round,
                                content: content.clone(),
                                details: details.clone(),
                            });
                            reasoning.push(ToolReplayReasoning { content, details });
                        }
                        Some(StreamChunk::Usage(usage)) => {
                            *usage_reported = true;
                            *total_input_tokens += usage.input_tokens;
                            *total_output_tokens += usage.output_tokens;
                        }
                        Some(StreamChunk::ProviderResponseWitness(witness)) => {
                            let _ = event_tx.send(AgentEvent::ProviderResponseCaptured {
                                turn_id: execution.turn_id,
                                round,
                                witness,
                            });
                        }
                        Some(StreamChunk::ProviderRetryStarted { attempt, max_retries, provider, wait_ms }) => {
                            let _ = event_tx.send(AgentEvent::ProviderRetryStarted {
                                turn_id: execution.turn_id,
                                attempt,
                                max_retries,
                                provider,
                                wait_ms,
                            });
                        }
                        Some(StreamChunk::ProviderRetrySettled { attempt, max_retries, provider, success }) => {
                            let _ = event_tx.send(AgentEvent::ProviderRetrySettled {
                                turn_id: execution.turn_id,
                                attempt,
                                max_retries,
                                provider,
                                success,
                            });
                        }
                        Some(StreamChunk::Error { message }) => {
                            self.fail_turn_and_idle(execution.turn_id, execution.thread_id).await;
                            let _ = event_tx.send(AgentEvent::ProviderRoundSettled {
                                turn_id: execution.turn_id,
                                round,
                                outcome: ProviderRoundOutcome::Failed { message: message.clone() },
                            });
                            let _ = event_tx.send(AgentEvent::Error {
                                message: message.clone(),
                                provider_failure: true,
                            });
                            return Err(AgentError::ProviderFailed(
                                swage_core::ProviderError::StreamError(message),
                            ));
                        }
                        Some(StreamChunk::ProviderAnomaly { message }) => {
                            self.fail_turn_and_idle(execution.turn_id, execution.thread_id).await;
                            let _ = event_tx.send(AgentEvent::ProviderRoundSettled {
                                turn_id: execution.turn_id,
                                round,
                                outcome: ProviderRoundOutcome::Failed { message: message.clone() },
                            });
                            let _ = event_tx.send(AgentEvent::Error {
                                message: message.clone(),
                                provider_failure: true,
                            });
                            return Err(AgentError::ProviderFailed(
                                swage_core::ProviderError::ProviderAnomaly(message),
                            ));
                        }
                        Some(StreamChunk::ToolUse { call_id, name, arguments }) if collect_tool_calls => {
                            tool_calls.push(ToolCall { call_id, name, arguments });
                        }
                        Some(StreamChunk::ToolUse { .. }) => {}
                    }
                }
            }
        }

        let _ = event_tx.send(AgentEvent::ProviderRoundSettled {
            turn_id: execution.turn_id,
            round,
            outcome: ProviderRoundOutcome::Responded {
                tool_calls: tool_calls.len() as u32,
            },
        });

        Ok(StreamRoundOutcome {
            text,
            tool_calls,
            reasoning,
        })
    }
}

struct StreamRoundOutcome {
    text: String,
    tool_calls: Vec<ToolCall>,
    reasoning: Vec<ToolReplayReasoning>,
}

// ---------------------------------------------------------------------------
// Hook lifecycle
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Leak detection
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Lifecycle hook helpers
// ---------------------------------------------------------------------------

impl Agent {
    /// Spawn a fire-and-forget hook dispatch.
    ///
    /// Common plumbing for all non-blocking lifecycle hooks: clone the registry,
    /// short-circuit when none is configured, then dispatch on a background task.
    fn fire_hook(&self, event: HookEvent) {
        let Some(registry) = self.hook_registry.clone() else {
            return;
        };
        tokio::spawn(async move {
            registry.dispatch(event).await;
        });
    }

    /// Fire `SessionStart` — non-blocking fire-and-forget.
    #[allow(dead_code)] // Part of hook lifecycle API; wired when gateway fires session events
    pub(crate) fn fire_session_start(&self, session_id: &str) {
        self.fire_hook(HookEvent::SessionStart {
            session_id: session_id.to_owned(),
        });
    }

    /// Fire `PreCompact` and return the outcome for the caller to inspect.
    ///
    /// Unlike the other lifecycle hooks, `PreCompact` is blocking — the
    /// outcome can flag content for preservation before the compaction runs.
    /// When no registry is configured, returns `HookOutcome::Continue`.
    #[allow(dead_code)] // Part of hook lifecycle API; wired when compaction invokes hooks
    pub(crate) async fn fire_pre_compact(&self, context: &serde_json::Value) -> HookOutcome {
        let Some(registry) = &self.hook_registry else {
            return HookOutcome::Continue;
        };
        registry
            .dispatch(HookEvent::PreCompact {
                context: context.clone(),
            })
            .await
    }
}

// ---------------------------------------------------------------------------
// Pre/post tool hook helpers
// ---------------------------------------------------------------------------

impl Agent {
    /// Dispatch `PreToolUse` to the hook registry.
    ///
    /// Returns [`PreToolUseOutcome::Proceed`] when no hook intervenes (or no
    /// registry is configured), [`PreToolUseOutcome::RequestApproval`] when a
    /// hook requests human review, or `Err(HookBlocked)` when a hook blocks
    /// the tool call outright.
    pub(crate) async fn dispatch_pre_tool_use(
        &self,
        tool_name: &str,
        params: &serde_json::Value,
    ) -> Result<PreToolUseOutcome, AgentError> {
        let Some(registry) = &self.hook_registry else {
            return Ok(PreToolUseOutcome::Proceed);
        };
        let event = swage_hook::HookEvent::PreToolUse {
            tool_name: tool_name.to_owned(),
            params: params.clone(),
        };
        match registry.dispatch(event).await {
            swage_hook::HookOutcome::Block(reason) => Err(AgentError::HookBlocked {
                hook: "PreToolUse".into(),
                reason,
            }),
            swage_hook::HookOutcome::RequestApproval {
                prompt,
                context,
                timeout,
            } => Ok(PreToolUseOutcome::RequestApproval {
                prompt,
                context,
                timeout,
            }),
            // Continue and ContinueModified both mean "proceed".
            _ => Ok(PreToolUseOutcome::Proceed),
        }
    }

    /// Dispatch `PostToolUse` to the hook registry.
    ///
    /// Informational only — the outcome is ignored. Returns immediately when
    /// no registry is configured.
    pub(crate) async fn dispatch_post_tool_use(
        &self,
        tool_name: &str,
        params: &serde_json::Value,
        result: &serde_json::Value,
    ) {
        let Some(registry) = &self.hook_registry else {
            return;
        };
        let event = swage_hook::HookEvent::PostToolUse {
            tool_name: tool_name.to_owned(),
            params: params.clone(),
            result: result.clone(),
        };
        // Fire and discard — PostToolUse cannot block execution.
        registry.dispatch(event).await;
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch helper — used by process_turn_streaming
// ---------------------------------------------------------------------------

fn tool_dispatch_result(
    result: Result<ToolOutput, swage_core::ToolError>,
) -> Result<(String, bool, String), AgentError> {
    match result {
        Ok(output) => {
            let result_str = serde_json::to_string(&output.result).unwrap_or_default();
            Ok((result_str, false, "success".to_string()))
        }
        Err(error) => Ok((
            format_tool_error_for_model(&error),
            true,
            "error".to_string(),
        )),
    }
}

/// Render a tool failure exactly as it will appear in the model's next message.
///
/// Tool implementations may use this boundary to verify that actionable error
/// detail survives the agent's compaction policy.
pub fn format_tool_error_for_model(error: &swage_core::ToolError) -> String {
    use swage_core::ToolError;

    match error {
        ToolError::NotFound { name } => format!(
            "Tool error: `{name}` is not available. Use one of the advertised tools or continue without it."
        ),
        ToolError::InvalidParameters { name, message } => format!(
            "Tool error: invalid parameters for `{name}`: {}. Correct the arguments and retry.",
            compact_tool_error_detail(message)
        ),
        ToolError::ExecutionFailed { name, message } => format!(
            "Tool error: `{name}` failed: {}. Use the error detail to adjust the request or retry if appropriate.",
            compact_tool_error_detail(message)
        ),
        ToolError::Timeout { name, duration } => format!(
            "Tool error: `{name}` timed out after {duration:?}. Retry with a smaller or simpler request."
        ),
        ToolError::PermissionDenied { name, reason } => format!(
            "Tool error: `{name}` is not permitted for this turn ({reason}). Choose an allowed tool or ask for access."
        ),
        ToolError::RequiresApproval { name, .. } => {
            format!("Tool error: `{name}` requires approval before it can run.")
        }
        ToolError::RequiresLocationApproval {
            name, path, access, ..
        } => format!(
            "Tool error: `{name}` requires {access} approval for `{path}` before it can run."
        ),
        ToolError::RequiresDelegation { request } => format!(
            "Tool error: `{}` requires delegation to `{}`, but delegation was not completed.",
            request.name, request.target_agent_id
        ),
    }
}

/// Maximum detail retained in a tool error sent to the model.
///
/// Producers that put structured guidance in errors must fit it within this
/// budget themselves, so compaction never silently changes its meaning.
pub const MAX_MODEL_TOOL_ERROR_DETAIL_CHARS: usize = 320;

fn compact_tool_error_detail(message: &str) -> String {
    let lines = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let first_line = lines.first().copied().unwrap_or("no further detail");
    let detail = if first_line.starts_with("Traceback")
        && let Some(last_line) = lines.last()
        && *last_line != first_line
    {
        format!("{first_line} {last_line}")
    } else {
        first_line.to_string()
    };
    let compact = detail.split_whitespace().collect::<Vec<_>>().join(" ");

    if compact.chars().count() <= MAX_MODEL_TOOL_ERROR_DETAIL_CHARS {
        return compact;
    }

    let mut shortened = compact
        .chars()
        .take(MAX_MODEL_TOOL_ERROR_DETAIL_CHARS.saturating_sub(3))
        .collect::<String>();
    shortened.push_str("...");
    shortened
}

impl Agent {
    async fn race_tool_dispatch<F>(
        &self,
        dispatch_future: F,
        call: &ToolCall,
        execution: &TurnExecutionContext,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        cancel_token: &tokio_util::sync::CancellationToken,
        dispatch_started_at: std::time::Instant,
    ) -> Result<Result<ToolOutput, swage_core::ToolError>, AgentError>
    where
        F: Future<Output = Result<ToolOutput, swage_core::ToolError>>,
    {
        tokio::pin!(dispatch_future);

        tokio::select! {
            result = &mut dispatch_future => Ok(result),
            _ = cancel_token.cancelled() => {
                let _ = self
                    .storage
                    .update_turn_state(execution.turn_id, TurnState::Interrupted)
                    .await;
                let _ = self
                    .storage
                    .update_state(execution.thread_id, ThreadState::Idle)
                    .await;
                let _ = event_tx.send(AgentEvent::ToolEnd {
                    call_id: call.call_id.clone(),
                    tool_name: call.name.clone(),
                    result: Value::Null,
                    status: "cancelled".to_string(),
                    duration_ms: Some(dispatch_started_at.elapsed().as_millis() as u64),
                });
                Err(AgentError::Cancelled)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_location_approval_and_dispatch(
        &self,
        dispatcher: &ToolDispatcher,
        call: &ToolCall,
        execution: &TurnExecutionContext,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        cancel_token: &tokio_util::sync::CancellationToken,
        approval_scope: &mut TurnApprovalScope,
        ctx: &AgentToolContext,
        path: String,
        access: String,
        timeout_secs: Option<u64>,
        timeout_policy: Option<swage_core::ApprovalTimeoutPolicy>,
        skip_tool_permission_gate: bool,
        dispatch_started_at: std::time::Instant,
    ) -> Result<(String, bool, String), AgentError> {
        use crate::approval::{ApprovalGateOutcome, DEFAULT_APPROVAL_TIMEOUT_SECS};

        let gate_timeout =
            std::time::Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_APPROVAL_TIMEOUT_SECS));
        let policy = timeout_policy.unwrap_or_default();
        let prompt = format!("Approval required to {access} location: {path}");
        let context = serde_json::json!({
            "path": path.clone(),
            "access": access.clone(),
            "tool_name": call.name.clone(),
        });
        let mut approved_ctx = ctx.clone();
        approved_ctx
            .allowed_paths
            .push(std::path::PathBuf::from(&path));

        if approval_scope.should_skip_gate() {
            let result = if skip_tool_permission_gate {
                self.race_tool_dispatch(
                    dispatcher.dispatch_approved(
                        &call.name,
                        call.arguments.clone(),
                        &approved_ctx,
                        &self.config.agent_id,
                    ),
                    call,
                    execution,
                    event_tx,
                    cancel_token,
                    dispatch_started_at,
                )
                .await?
            } else {
                self.race_tool_dispatch(
                    dispatcher.dispatch(
                        &call.name,
                        call.arguments.clone(),
                        &approved_ctx,
                        &self.config.agent_id,
                    ),
                    call,
                    execution,
                    event_tx,
                    cancel_token,
                    dispatch_started_at,
                )
                .await?
            };
            return tool_dispatch_result(result);
        }

        match self
            .handle_approval_gate(
                execution,
                swage_storage::model::ApprovalKind::Location,
                Some(call.name.clone()),
                Some(call.arguments.clone()),
                None,
                prompt,
                Some(context),
                gate_timeout,
                policy,
                event_tx,
                cancel_token,
                approval_scope,
            )
            .await?
        {
            ApprovalGateOutcome::Approved => {
                let result = if skip_tool_permission_gate {
                    self.race_tool_dispatch(
                        dispatcher.dispatch_approved(
                            &call.name,
                            call.arguments.clone(),
                            &approved_ctx,
                            &self.config.agent_id,
                        ),
                        call,
                        execution,
                        event_tx,
                        cancel_token,
                        dispatch_started_at,
                    )
                    .await?
                } else {
                    self.race_tool_dispatch(
                        dispatcher.dispatch(
                            &call.name,
                            call.arguments.clone(),
                            &approved_ctx,
                            &self.config.agent_id,
                        ),
                        call,
                        execution,
                        event_tx,
                        cancel_token,
                        dispatch_started_at,
                    )
                    .await?
                };
                tool_dispatch_result(result)
            }
            ApprovalGateOutcome::Rejected { feedback } => {
                Ok((feedback, true, "rejected".to_string()))
            }
            ApprovalGateOutcome::Interrupted => Err(AgentError::Cancelled),
        }
    }

    /// Dispatch a single tool call through the dispatcher, handling the
    /// config-driven approval gate when `RequiresApproval` is returned.
    ///
    /// Returns `Ok((result_content, is_error, status))` on normal completion
    /// (including rejections), or `Err` when the turn must be interrupted
    /// (timeout with interrupt policy, or storage errors).
    /// One-line display headline for a call, from the tool's declared
    /// display hints (`ToolDisplayHints`). None without a dispatcher, a
    /// hint, or a scalar value at the hinted parameter — consumers then use
    /// their generic rendering.
    fn tool_display_summary(&self, call: &ToolCall) -> Option<String> {
        self.tool_dispatcher
            .as_ref()?
            .display_summary(&call.name, &call.arguments)
    }

    async fn dispatch_tool_call(
        &self,
        call: &ToolCall,
        execution: &TurnExecutionContext,
        current_context: &[ChatMessage],
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        cancel_token: &tokio_util::sync::CancellationToken,
        approval_scope: &mut TurnApprovalScope,
    ) -> Result<(String, bool, String), AgentError> {
        // Emit ToolStart — client can render a "running…" indicator.
        let _ = event_tx.send(AgentEvent::ToolStart {
            call_id: call.call_id.clone(),
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
            display_summary: self.tool_display_summary(call),
        });

        let Some(ref dispatcher) = self.tool_dispatcher else {
            return Ok((
                "no tool dispatcher configured".to_string(),
                true,
                "error".to_string(),
            ));
        };

        let ctx = AgentToolContext::new(
            &self.config.agent_id,
            execution.session_id.to_string(),
            self.config.working_directory.clone(),
        )
        .with_call_id(call.call_id.clone())
        .with_turn_id(execution.turn_id)
        .with_thread_id(execution.thread_id);
        let ctx = AgentToolContext {
            allowed_paths: self.config.allowed_paths.clone(),
            ..ctx
        };
        let dispatch_started_at = std::time::Instant::now();
        match self
            .race_tool_dispatch(
                dispatcher.dispatch(
                    &call.name,
                    call.arguments.clone(),
                    &ctx,
                    &self.config.agent_id,
                ),
                call,
                execution,
                event_tx,
                cancel_token,
                dispatch_started_at,
            )
            .await?
        {
            Ok(output) => {
                let result_str = serde_json::to_string(&output.result).unwrap_or_default();
                Ok((result_str, false, "success".to_string()))
            }
            Err(swage_core::ToolError::RequiresApproval {
                name: _,
                timeout_secs,
                timeout_policy,
            }) => {
                // Config-driven approval gate.
                use crate::approval::{ApprovalGateOutcome, DEFAULT_APPROVAL_TIMEOUT_SECS};
                let gate_timeout = std::time::Duration::from_secs(
                    timeout_secs.unwrap_or(DEFAULT_APPROVAL_TIMEOUT_SECS),
                );
                let policy = timeout_policy.unwrap_or_default();
                let prompt = format!("Approval required to execute tool: {}", call.name);

                if approval_scope.should_skip_gate() {
                    match self
                        .race_tool_dispatch(
                            dispatcher.dispatch_approved(
                                &call.name,
                                call.arguments.clone(),
                                &ctx,
                                &self.config.agent_id,
                            ),
                            call,
                            execution,
                            event_tx,
                            cancel_token,
                            dispatch_started_at,
                        )
                        .await?
                    {
                        Ok(output) => {
                            let result_str =
                                serde_json::to_string(&output.result).unwrap_or_default();
                            return Ok((result_str, false, "success".to_string()));
                        }
                        Err(swage_core::ToolError::RequiresLocationApproval {
                            path,
                            access,
                            timeout_secs,
                            timeout_policy,
                            ..
                        }) => {
                            return self
                                .handle_location_approval_and_dispatch(
                                    dispatcher,
                                    call,
                                    execution,
                                    event_tx,
                                    cancel_token,
                                    approval_scope,
                                    &ctx,
                                    path,
                                    access,
                                    timeout_secs,
                                    timeout_policy,
                                    true,
                                    dispatch_started_at,
                                )
                                .await;
                        }
                        Err(e) => {
                            return Ok((
                                format_tool_error_for_model(&e),
                                true,
                                "error".to_string(),
                            ));
                        }
                    }
                }

                match self
                    .handle_approval_gate(
                        execution,
                        swage_storage::model::ApprovalKind::Tool,
                        Some(call.name.clone()),
                        Some(call.arguments.clone()),
                        None,
                        prompt,
                        None,
                        gate_timeout,
                        policy,
                        event_tx,
                        cancel_token,
                        &mut *approval_scope,
                    )
                    .await?
                {
                    ApprovalGateOutcome::Approved => {
                        // Re-dispatch skipping the permission check.
                        match self
                            .race_tool_dispatch(
                                dispatcher.dispatch_approved(
                                    &call.name,
                                    call.arguments.clone(),
                                    &ctx,
                                    &self.config.agent_id,
                                ),
                                call,
                                execution,
                                event_tx,
                                cancel_token,
                                dispatch_started_at,
                            )
                            .await?
                        {
                            Ok(output) => {
                                let result_str =
                                    serde_json::to_string(&output.result).unwrap_or_default();
                                Ok((result_str, false, "success".to_string()))
                            }
                            Err(swage_core::ToolError::RequiresLocationApproval {
                                path,
                                access,
                                timeout_secs,
                                timeout_policy,
                                ..
                            }) => {
                                self.handle_location_approval_and_dispatch(
                                    dispatcher,
                                    call,
                                    execution,
                                    event_tx,
                                    cancel_token,
                                    approval_scope,
                                    &ctx,
                                    path,
                                    access,
                                    timeout_secs,
                                    timeout_policy,
                                    true,
                                    dispatch_started_at,
                                )
                                .await
                            }
                            Err(e) => {
                                Ok((format_tool_error_for_model(&e), true, "error".to_string()))
                            }
                        }
                    }
                    ApprovalGateOutcome::Rejected { feedback } => {
                        Ok((feedback, true, "rejected".to_string()))
                    }
                    ApprovalGateOutcome::Interrupted => {
                        // State transitions already done by handle_approval_gate.
                        Err(AgentError::Cancelled)
                    }
                }
            }
            Err(swage_core::ToolError::RequiresLocationApproval {
                path,
                access,
                timeout_secs,
                timeout_policy,
                ..
            }) => {
                self.handle_location_approval_and_dispatch(
                    dispatcher,
                    call,
                    execution,
                    event_tx,
                    cancel_token,
                    approval_scope,
                    &ctx,
                    path,
                    access,
                    timeout_secs,
                    timeout_policy,
                    false,
                    dispatch_started_at,
                )
                .await
            }
            // --- Tool-as-delegation: the LLM invoked the delegate tool ---
            Err(swage_core::ToolError::RequiresDelegation { request }) => {
                let swage_core::DelegationRequest {
                    name: _,
                    target_agent_id,
                    task,
                    task_definition,
                    context,
                    cwd,
                    isolation,
                    skills,
                    child,
                    composition,
                    completion_mode,
                } = *request;
                let handler = match &self.delegation_handler {
                    Some(h) => h.clone(),
                    None => {
                        return Ok((
                            "delegation is not available — no delegation handler configured"
                                .to_string(),
                            true,
                            "error".to_string(),
                        ));
                    }
                };

                let Some(topic_id) = execution.topic_id else {
                    return Ok((
                        "delegation is not available — topic context is missing".to_string(),
                        true,
                        "error".to_string(),
                    ));
                };

                let transferred_context = match context {
                    Some(context) => Some(context),
                    None => {
                        let transferred = crate::context_transfer::transfer_context_async(
                            &self.context_transfer_config,
                            &target_agent_id,
                            current_context,
                            Some(self.provider.as_ref()),
                        )
                        .await;

                        if transferred.messages.is_empty() {
                            None
                        } else {
                            serde_json::to_value(&transferred.messages).ok()
                        }
                    }
                };

                // Suspend the parent turn — durable state so restart recovery
                // can find it and transition to Interrupted.
                let _ = self
                    .storage
                    .update_turn_state(execution.turn_id, TurnState::Delegating)
                    .await;

                // Emit DelegationStarted event so clients can track progress.
                let _ = event_tx.send(AgentEvent::DelegationStarted {
                    target_agent_id: target_agent_id.clone(),
                    task: task.clone(),
                });

                // Run the delegation — this may take significant time.
                let delegation_result = handler
                    .handle_delegation(
                        &target_agent_id,
                        &task,
                        task_definition.as_deref(),
                        transferred_context,
                        cwd,
                        isolation,
                        skills,
                        child.as_deref(),
                        *composition,
                        &call.call_id,
                        &self.config.agent_id,
                        execution.turn_id,
                        topic_id,
                        cancel_token.clone(),
                        completion_mode,
                    )
                    .await;

                // Resume the parent turn — back to Processing regardless of
                // outcome. The tool call result (success or error string) is
                // what the LLM sees.
                let _ = self
                    .storage
                    .update_turn_state(execution.turn_id, TurnState::Processing)
                    .await;

                match delegation_result {
                    Ok(crate::delegation::DelegationOutcome::Completed(result)) => {
                        let _ = event_tx.send(AgentEvent::DelegationCompleted {
                            target_agent_id: target_agent_id.clone(),
                            success: true,
                        });
                        let result_str = serde_json::to_string(&result).unwrap_or_default();
                        Ok((result_str, false, "success".to_string()))
                    }
                    Ok(crate::delegation::DelegationOutcome::Detached(detached)) => {
                        // The delegate has started, not completed. The actual
                        // child completion is represented by the durable Result
                        // message correlated to delegate_message_id.
                        let result_str =
                            serde_json::to_string(&detached.tool_output()).unwrap_or_default();
                        Ok((result_str, false, "success".to_string()))
                    }
                    Err(e) => {
                        let _ = event_tx.send(AgentEvent::DelegationCompleted {
                            target_agent_id: target_agent_id.clone(),
                            success: false,
                        });
                        Ok((format!("delegation failed: {e}"), true, "error".to_string()))
                    }
                }
            }
            Err(e) => Ok((format_tool_error_for_model(&e), true, "error".to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Approval gate — human-in-the-loop tool approval
// ---------------------------------------------------------------------------

impl Agent {
    async fn resume_after_approval_decision(
        &self,
        approval_id: Uuid,
        turn_id: Uuid,
        state: swage_storage::model::ApprovalState,
        reason: Option<&str>,
    ) -> Result<(), AgentError> {
        self.storage
            .update_approval_state(approval_id, state, reason)
            .await?;
        self.storage
            .update_turn_state(turn_id, TurnState::Processing)
            .await?;
        Ok(())
    }

    async fn interrupt_approval_gate(
        &self,
        approval_id: Uuid,
        turn_id: Uuid,
        thread_id: Uuid,
        reason: &str,
    ) -> Result<crate::approval::ApprovalGateOutcome, AgentError> {
        self.storage
            .update_approval_state(
                approval_id,
                swage_storage::model::ApprovalState::Expired,
                Some(reason),
            )
            .await?;
        self.storage
            .update_turn_state(turn_id, TurnState::Interrupted)
            .await?;
        if self
            .storage
            .get(thread_id)
            .await?
            .is_some_and(|thread| thread.state != ThreadState::Idle)
        {
            self.storage
                .update_state(thread_id, ThreadState::Idle)
                .await?;
        }
        Ok(crate::approval::ApprovalGateOutcome::Interrupted)
    }

    /// Execute the approval gate: create an approval request, transition the
    /// turn to `AwaitingApproval`, emit an `ApprovalRequired` event, then wait
    /// for a human decision on a oneshot channel (racing against a timeout).
    ///
    /// Both approval sources (config-driven `RequiresApproval` and hook-driven
    /// `RequestApproval`) funnel through this single method.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn handle_approval_gate(
        &self,
        execution: &TurnExecutionContext,
        kind: swage_storage::model::ApprovalKind,
        tool_name: Option<String>,
        tool_arguments: Option<serde_json::Value>,
        hook_name: Option<String>,
        prompt: String,
        context: Option<serde_json::Value>,
        timeout: std::time::Duration,
        timeout_policy: swage_core::ApprovalTimeoutPolicy,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        cancel_token: &tokio_util::sync::CancellationToken,
        approval_scope: &mut TurnApprovalScope,
    ) -> Result<crate::approval::ApprovalGateOutcome, AgentError> {
        use crate::approval::{ApprovalGateOutcome, DEFAULT_APPROVAL_TIMEOUT_SECS};
        use swage_storage::model::{ApprovalState, NewApprovalRequest};

        let timeout_at = chrono::Utc::now()
            + chrono::Duration::from_std(timeout).unwrap_or_else(|_| {
                chrono::Duration::seconds(DEFAULT_APPROVAL_TIMEOUT_SECS as i64)
            });

        // 1. Create the approval request record in storage.
        let approval = self
            .storage
            .create_approval(NewApprovalRequest {
                turn_id: execution.turn_id,
                agent_id: self.config.agent_id.clone(),
                kind: kind.clone(),
                tool_name: tool_name.clone(),
                tool_arguments: tool_arguments.clone(),
                hook_name: hook_name.clone(),
                prompt: prompt.clone(),
                context: context.clone(),
                timeout_at,
                timeout_policy: timeout_policy.clone(),
            })
            .await?;

        // 2. Transition turn to AwaitingApproval.
        self.storage
            .update_turn_state(execution.turn_id, TurnState::AwaitingApproval)
            .await?;

        // 3. Register the oneshot channel before exposing the approval. A client may
        // reject immediately after receiving ApprovalRequired.
        let Some(ref registrar) = self.approval_registrar else {
            // No approval channel infrastructure — treat as rejection.
            let _ = self
                .storage
                .update_approval_state(
                    approval.id,
                    ApprovalState::Rejected,
                    Some("no approval channel configured"),
                )
                .await;
            let _ = self
                .storage
                .update_turn_state(execution.turn_id, TurnState::Processing)
                .await;
            return Ok(ApprovalGateOutcome::Rejected {
                feedback: "Tool requires approval but no approval channel is configured".into(),
            });
        };

        let (tx, rx) = tokio::sync::oneshot::channel::<crate::approval::ApprovalDecision>();
        registrar(approval.id, tx);

        // 4. Emit ApprovalRequired only once a decision can be received.
        let _ = event_tx.send(AgentEvent::ApprovalRequired {
            approval_id: approval.id,
            turn_id: execution.turn_id,
            agent_id: self.config.agent_id.clone(),
            kind: kind.to_string(),
            tool_name: tool_name.clone(),
            tool_arguments: tool_arguments.clone(),
            hook_name: hook_name.clone(),
            prompt: prompt.clone(),
            context: context.clone(),
            timeout_at: timeout_at.to_rfc3339(),
            timeout_policy: timeout_policy.clone(),
        });

        // 5. Race the oneshot receiver against the timeout and cancellation.
        // Three outcomes: human decision arrives, timeout fires, or the turn
        // is cooperatively cancelled (e.g. inter-agent Cancel message).
        enum GateResult {
            Decision(crate::approval::ApprovalDecision),
            Timeout,
            Cancelled,
        }

        let gate_result = tokio::select! {
            result = rx => {
                match result {
                    Ok(d) => GateResult::Decision(d),
                    // Sender dropped — treat as timeout (request abandoned).
                    Err(_) => GateResult::Timeout,
                }
            }
            _ = tokio::time::sleep(timeout) => GateResult::Timeout,
            _ = cancel_token.cancelled() => GateResult::Cancelled,
        };

        match gate_result {
            GateResult::Cancelled => {
                // Cooperative cancellation — interrupt immediately regardless
                // of timeout policy. The approval becomes stale.
                self.interrupt_approval_gate(
                    approval.id,
                    execution.turn_id,
                    execution.thread_id,
                    "cancelled by inter-agent message",
                )
                .await
            }
            GateResult::Decision(d) if d.is_approved() => {
                if matches!(
                    d.scope,
                    crate::approval::ApprovalDecisionScope::Session
                        | crate::approval::ApprovalDecisionScope::Permanent
                        | crate::approval::ApprovalDecisionScope::PermanentGlobal
                ) {
                    approval_scope.approve_remaining_gates();
                }

                self.resume_after_approval_decision(
                    approval.id,
                    execution.turn_id,
                    ApprovalState::Approved,
                    d.reason.as_deref(),
                )
                .await?;
                let _ = event_tx.send(AgentEvent::ApprovalConfirmed {
                    approval_id: approval.id,
                    decision: crate::approval::ApprovalDecisionValue::Approved,
                    reason: d.reason,
                });
                Ok(ApprovalGateOutcome::Approved)
            }
            GateResult::Decision(d) => {
                // Rejected by human.
                let reason_text = d
                    .reason
                    .clone()
                    .unwrap_or_else(|| "rejected by reviewer".into());
                self.resume_after_approval_decision(
                    approval.id,
                    execution.turn_id,
                    ApprovalState::Rejected,
                    d.reason.as_deref(),
                )
                .await?;
                let _ = event_tx.send(AgentEvent::ApprovalConfirmed {
                    approval_id: approval.id,
                    decision: crate::approval::ApprovalDecisionValue::Rejected,
                    reason: d.reason,
                });
                tracing::info!(
                    approval_id = %approval.id,
                    turn_id = %execution.turn_id,
                    reason = %reason_text,
                    "tool approval rejected and settled"
                );
                Ok(ApprovalGateOutcome::Rejected {
                    feedback: format!("Tool call rejected: {reason_text}"),
                })
            }
            GateResult::Timeout => {
                // Timeout or channel dropped — apply policy.
                let _ = event_tx.send(AgentEvent::ApprovalExpired {
                    approval_id: approval.id,
                    timeout_policy: timeout_policy.clone(),
                });

                match timeout_policy {
                    swage_core::ApprovalTimeoutPolicy::Reject => {
                        // Treat timeout as rejection — turn continues.
                        self.resume_after_approval_decision(
                            approval.id,
                            execution.turn_id,
                            ApprovalState::Expired,
                            Some("approval timed out"),
                        )
                        .await?;
                        Ok(ApprovalGateOutcome::Rejected {
                            feedback: "Tool call rejected: approval timed out".into(),
                        })
                    }
                    swage_core::ApprovalTimeoutPolicy::Interrupt => {
                        // Turn is interrupted — no further processing.
                        self.interrupt_approval_gate(
                            approval.id,
                            execution.turn_id,
                            execution.thread_id,
                            "approval timed out",
                        )
                        .await
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Message processing and session management
// ---------------------------------------------------------------------------

impl Agent {
    /// Process a user message through the single-turn execution path.
    ///
    /// Resolves the target thread, creates a turn, persists the user message,
    /// runs BeforeInbound hooks, calls the LLM, runs AfterResponse hooks,
    /// scans for leaked credentials, persists the assistant response, and
    /// returns an [`OutboundMessage`] with token counts.
    ///
    /// Mark turn as Failed and thread as Idle — used on error recovery paths.
    async fn fail_turn_and_idle(&self, turn_id: Uuid, thread_id: Uuid) {
        let _ = self
            .storage
            .update_turn_state(turn_id, TurnState::Failed)
            .await;
        let _ = self
            .storage
            .update_state(thread_id, ThreadState::Idle)
            .await;
    }

    // -----------------------------------------------------------------------
    // Hook dispatch helpers
    // -----------------------------------------------------------------------

    /// Dispatch an `AfterResponse` hook, returning the (possibly modified) content.
    ///
    /// When no hook registry is configured, the content passes through unchanged.
    /// A blocking hook produces `AgentError::HookBlocked`.
    pub(crate) async fn dispatch_after_response(
        &self,
        content: &str,
    ) -> Result<String, AgentError> {
        let registry = match &self.hook_registry {
            Some(r) => r,
            None => return Ok(content.to_owned()),
        };

        let event = HookEvent::AfterResponse {
            content: content.to_owned(),
        };

        match registry.dispatch(event).await {
            HookOutcome::Continue => Ok(content.to_owned()),
            HookOutcome::ContinueModified(modified) => {
                if let HookEvent::AfterResponse {
                    content: new_content,
                } = *modified
                {
                    Ok(new_content)
                } else {
                    Ok(content.to_owned())
                }
            }
            HookOutcome::Block(reason) => Err(AgentError::HookBlocked {
                hook: "AfterResponse".into(),
                reason,
            }),
            // RequestApproval is only meaningful on PreToolUse hooks.
            // Silently continue for hook contexts where approval has no effect.
            HookOutcome::RequestApproval { .. } => {
                tracing::warn!(
                    "RequestApproval returned from AfterResponse hook — not applicable; continuing"
                );
                Ok(content.to_owned())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Leak detection
    // -----------------------------------------------------------------------

    async fn persist_cancelled_partial(&self, thread_id: Uuid, turn_id: Uuid, text: &str) {
        if text.is_empty() {
            return;
        }

        let safe_content = self.scan_output(text);
        if let Err(error) = self
            .storage
            .append_message(swage_storage::model::AppendMessage {
                thread_id,
                attachments: Vec::new(),
                turn_id: Some(turn_id),
                role: Role::Assistant,
                content: safe_content.clone(),
                agent_id: self.config.agent_id.clone(),
            })
            .await
        {
            tracing::warn!(
                %turn_id,
                %error,
                "failed to persist cancelled turn's partial assistant message"
            );
        }
        if let Err(error) = self.storage.update_response(turn_id, &safe_content).await {
            tracing::warn!(
                %turn_id,
                %error,
                "failed to persist cancelled turn's partial response"
            );
        }
    }

    /// Scan text for credential leaks and return the redacted version.
    ///
    /// When no leak detector is configured, the text passes through unchanged.
    /// If the detector panics (shouldn't happen, but defensive), we fail-open:
    /// log a warning and return the original text.
    pub(crate) fn scan_output(&self, text: &str) -> String {
        let detector = match &self.leak_detector {
            Some(d) => d.clone(),
            None => return text.to_owned(),
        };

        // Fail-open: catch any panic from the scanning pipeline.
        let text_owned = text.to_owned();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detector.scan(&text_owned)))
        {
            Ok(redacted) => redacted,
            Err(_) => {
                tracing::warn!("leak detector panicked during scan — returning original text");
                text.to_owned()
            }
        }
    }
}

// ---------------------------------------------------------------------------
