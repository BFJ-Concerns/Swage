use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_core::{
    ChatMessage, CompletionRequest, GatewayError, Role, SandboxError, StreamChunk,
    ToolCall, ToolResult, build_tool_round_messages,
};
use swage_storage::model::{NewMessage, NewToolCall, NewTurn, TurnState};
use swage_storage::store::ContextSnapshotStore;

use swage_context::element::ContextElement;
use swage_context::history::HistoryElement;
use swage_context::prompts::PromptsElement;
use swage_context::tokeniser::TiktokenTokeniser;
use swage_config::prompt_rule::PromptRule;

use swage_tool::context::MockToolContext;

use crate::{
    state::AppState,
    subscription::SubscriptionState,
    websocket::ServerEvent,
};
use crate::tool::python_execute::{PythonExecuteContext, PYTHON_CONTEXT};

/// Build the context assembly elements for a turn.
///
/// Constructs a `PromptsElement` from the resolved instruction string and
/// a `HistoryElement` from the thread's message history. Returns a boxed
/// element vec suitable for passing to `swage_context::assembly::assemble()`.
///
/// When instructions are empty the `PromptsElement` produces no messages,
/// matching era 0.1 behaviour where an empty instruction string produced no
/// system message.
fn build_assembly_elements(
    instructions: &str,
    history: Vec<ChatMessage>,
) -> Vec<Box<dyn ContextElement>> {
    // Construct a single PromptRule from the resolved instructions string.
    // The config resolver has already merged hierarchy levels and assembled
    // the text — we wrap it as a single Global-level rule so the PromptsElement
    // produces the expected system message.
    let rules = if instructions.is_empty() {
        vec![]
    } else {
        vec![PromptRule {
            priority: 0,
            content: instructions.to_string(),
            source_level: swage_config::HierarchyLevel::Global,
            file_path: std::path::PathBuf::from("resolved-instructions"),
        }]
    };

    vec![
        Box::new(PromptsElement::new(rules)),
        Box::new(HistoryElement::new(history)),
    ]
}

/// Orchestrate a complete agent turn: user message → provider stream → storage.
///
/// # Flow
/// 1. Look up the session's default thread (created alongside the session).
/// 2. Guard: reject if a turn is already Processing for this thread.
/// 3. Resolve the project for the session; load per-project config and build ToolContext.
/// 4. Create a Turn record (state=Pending), immediately advance to Processing.
/// 5. Store the user message with thread_id and position.
/// 6. Build message history from the thread and send a streaming request to the provider.
/// 7. For each chunk: forward Token events to the WebSocket, accumulate content.
/// 8. On Done: store the assistant message, mark the turn Completed, send TurnComplete.
/// 9. On Cancel: store any partial response, mark the turn Interrupted, send Cancelled.
/// 10. On provider Error: mark the turn Failed, send an Error event.
/// 11. Cleanup: remove the CancellationToken from active_turns.
// Because process_turn is spawned as a tokio task, it won't inherit the
// HTTP request span. The instrument macro creates a self-contained "turn"
// span so all log entries within the turn share the same turn_id and
// session_id. turn_id is Empty at span creation and recorded once the
// Turn record exists.
#[tracing::instrument(
    name = "turn",
    skip(state, content, ws_tx, subscription),
    fields(turn_id = tracing::field::Empty),
)]
pub async fn process_turn(
    state: &AppState,
    session_id: Uuid,
    content: String,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
) -> Result<(), GatewayError> {
    // Step 1: Resolve the session's thread. The gateway creates one thread per
    // session in create_conversation / create_session_in_project.
    let threads = state.storage.list_by_session(session_id).await?;
    let thread = threads
        .into_iter()
        .next()
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let thread_id = thread.id;

    // Step 2: Guard — reject if a turn is already Processing for this thread.
    let existing_turns = state.storage.list_turns_by_thread(thread_id).await?;
    if existing_turns
        .iter()
        .any(|t| t.state == TurnState::Processing)
    {
        return Err(GatewayError::TurnInProgress { session_id });
    }

    // Step 3: Resolve the project for this session so we can load per-project config
    // and build the ToolContext passed to each tool invocation.
    let session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let project_id = session.project_id;

    let project = state
        .project_store
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
    let project_slug = project.slug.clone();

    // Load project-specific config (model, instructions, etc.). Falls back to
    // the startup config when no project-level config.toml exists, which is
    // the normal case for most projects.
    let config = match swage_config::load_resolved_config(&project_slug, "default") {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("failed to load project config for '{project_slug}', using startup config: {e}");
            state.config.clone()
        }
    };

    // Build the ToolContext passed to each tool invocation.
    let working_dir = crate::tool::workspace::resolve_workspace_root(&config);
    let tool_ctx = MockToolContext {
        agent_id: "gateway".to_string(),
        session_id: session_id.to_string(),
        working_directory: working_dir,
        allowed_paths: vec![],
        user_info: None,
    };

    // Step 4: Create the turn record (Pending), then advance to Processing.
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
        })
        .await?;

    state.storage.update_turn_state(turn.id, TurnState::Processing).await?;

    // Record the turn_id now that we have the Turn record. session_id was
    // already recorded by the #[instrument] macro from the function parameter.
    tracing::Span::current().record("turn_id", turn.id.to_string());
    tracing::debug!("processing turn");

    let cancel_token = CancellationToken::new();
    {
        let mut turns = state.active_turns.lock().await;
        turns.insert(session_id, cancel_token.clone());
    }

    // Step 5: Store the user message.
    // Position is the current count of messages in the thread.
    let user_position = state
        .storage
        .list_messages_by_thread(thread_id)
        .await
        .map_err(|e| {
            // Log before discarding: a transient read error here would silently
            // assign position 0 to all subsequent messages, corrupting order.
            tracing::warn!(error = %e, "failed to read message count; defaulting position to 0");
            e
        })
        .map(|msgs| msgs.len() as i32)
        .unwrap_or_else(|e| {
            tracing::warn!("failed to read message count for position, defaulting to 0: {e}");
            0
        });

    state
        .storage
        .create_message(NewMessage {
            thread_id,
            role: Role::User.to_string(),
            content: content.clone(),
            position: user_position,
            agent_id: "gateway".to_string(),
        })
        .await?;

    // Steps 6–7: Assemble context via the swage-context assembly engine.
    // Loads thread history, constructs typed elements (prompts + history),
    // and runs the assembly with token budgeting and compaction support.
    let stored_history = state.storage.list_messages_by_thread(thread_id).await?;
    let history_messages: Vec<ChatMessage> = stored_history
        .into_iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            };
            ChatMessage::new(role, m.content)
        })
        .collect();

    // Run context assembly in a scoped block so the non-Send element and
    // tokeniser values are dropped before the streaming loop's await points.
    let (assembled_messages, assembly_report) = {
        let elements = build_assembly_elements(&config.instructions, history_messages);
        let tokeniser = TiktokenTokeniser::new(&config.provider.model);

        // Use default_assembly directly — it's synchronous and avoids Send
        // issues from holding trait object references across the async timeout
        // boundary in assemble(). In era 0.1 only the "default" assembly is
        // registered, so the fail-open wrapper would dispatch here anyway.
        let output = swage_context::assembly::default_assembly(
            &elements, &config.context, &tokeniser,
        );

        (output.messages, output.report)
    };

    tracing::debug!(
        total_tokens = assembly_report.total_tokens,
        elements = assembly_report.elements.len(),
        "context assembled"
    );

    // Serialise the assembly report and emit ContextAssembled before streaming.
    // Persistence always happens; WebSocket delivery is gated by subscription
    // (with an error-bypass: reports containing errors reach all clients).
    let report_json = serde_json::to_value(&assembly_report)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to serialise assembly report");
            serde_json::json!({ "error": "serialisation failed" })
        });

    // Persist the snapshot for the past-turn REST endpoint.
    if let Err(e) = state
        .storage
        .store_snapshot(turn.id, &report_json.to_string())
        .await
    {
        tracing::warn!(error = %e, "failed to persist context snapshot");
    }

    // Deliver the ContextAssembled event (subscription-gated).
    let assembled_event = ServerEvent::ContextAssembled {
        turn_id: turn.id,
        report: report_json,
    };
    if subscription.lock().await.should_deliver(&assembled_event) {
        let _ = ws_tx.send(assembled_event);
    }

    let mut request = CompletionRequest {
        messages: assembled_messages,
        model: Some(config.provider.model.clone()),
        max_tokens: Some(4096),
        temperature: None,
        stop_sequences: None,
        metadata: None,
    };

    // Step 8-10: Tool-aware streaming loop.
    let mut accumulated = String::new();
    let mut stored_usage = None;
    let mut tool_position: i32 = 0;

    // Hard cap on tool-call rounds to prevent infinite loops driven by a
    // misbehaving model or circular tool dependencies.
    const MAX_TOOL_ROUNDS: usize = 25;
    let mut round = 0usize;

    let outcome = 'outer: loop {
        if round >= MAX_TOOL_ROUNDS {
            let msg = format!(
                "\n\n[Tool-call round limit ({MAX_TOOL_ROUNDS}) reached — stopping to prevent an infinite loop.]"
            );
            accumulated.push_str(&msg);
            let _ = ws_tx.send(ServerEvent::Token { content: msg });
            break TurnOutcome::Done;
        }
        round += 1;

        let mut stream = state.provider.stream(request.clone());
        let mut round_text = String::new();
        let mut round_tool_calls: Vec<ToolCall> = Vec::new();

        let _round_outcome = loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break 'outer TurnOutcome::Cancelled;
                }
                chunk = stream.next() => {
                    match chunk {
                        None => break RoundOutcome::Done,
                        Some(StreamChunk::Done { .. }) => break RoundOutcome::Done,
                        Some(StreamChunk::Delta { content }) => {
                            // Each streaming token counts as activity so a long response
                            // doesn't trigger the idle monitor mid-stream.
                            state.activity_tracker.record_activity();
                            round_text.push_str(&content);
                            accumulated.push_str(&content);
                            // Each token is a sign of life — reset the idle timer so
                            // a slow but active stream doesn't trigger idle shutdown.
                            state.activity_tracker.record_activity();
                            let _ = ws_tx.send(ServerEvent::Token { content });
                        }
                        Some(StreamChunk::Usage(usage)) => {
                            stored_usage = Some(usage);
                        }
                        Some(StreamChunk::Error { message }) => {
                            break 'outer TurnOutcome::ProviderError(message);
                        }
                        Some(StreamChunk::ToolUse { call_id, name, arguments }) => {
                            round_tool_calls.push(ToolCall {
                                call_id,
                                name,
                                arguments,
                            });
                        }
                    }
                }
            }
        };

        if round_tool_calls.is_empty() {
            break 'outer TurnOutcome::Done;
        }

        let mut tool_results: Vec<ToolResult> = Vec::new();

        for call in &round_tool_calls {
            if cancel_token.is_cancelled() {
                break 'outer TurnOutcome::Cancelled;
            }

            // Deliver ToolStart only to connections subscribed to the "tools"
            // channel. Storage recording (create_tool_call below) is unconditional
            // so the turn history is always complete regardless of subscription.
            let tool_start = ServerEvent::ToolStart {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                parent_call_id: None,
            };
            if subscription.lock().await.should_deliver(&tool_start) {
                let _ = ws_tx.send(tool_start);
            }

            if let Err(e) = state.storage.create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                arguments: serde_json::to_string(&call.arguments).unwrap_or_default(),
                result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                position: tool_position,
                parent_call_id: None,
            }).await {
                tracing::warn!("failed to record tool call {}: {e}", call.call_id);
            }
            tool_position += 1;

            // A running tool is a form of activity — reset the idle timer so
            // long-running tool calls don't trigger an idle shutdown mid-turn.
            state.activity_tracker.record_activity();
            let start = Instant::now();
            let (result_content, is_error) = if let Some(sandbox) = state.sandbox.as_ref() {
                // Route through the sandboxed agent process — credentials and
                // filesystem access are mediated by the sandbox; tool code never
                // runs directly in the gateway process.
                match sandbox.execute_tool(&call.name, call.arguments.clone()).await {
                    Ok(result) => match result.error {
                        // The sandboxed tool itself reported a failure.
                        Some(err_msg) => (err_msg, true),
                        None => {
                            // Prefer a plain string output; fall back to JSON serialisation.
                            let content = result.output.as_str()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| result.output.to_string());
                            (content, false)
                        }
                    },
                    Err(SandboxError::IpcError { message }) => {
                        (format!("sandbox communication error: {message}"), true)
                    }
                    Err(e) => (e.to_string(), true),
                }
            } else {
                // Direct dispatch: sandbox is disabled or unavailable.
                let dispatch_result = if call.name == "python_execute" {
                    let py_ctx = PythonExecuteContext {
                        ws_tx: ws_tx.clone(),
                        turn_id: turn.id,
                        call_id: call.call_id.clone(),
                    };
                    PYTHON_CONTEXT
                        .scope(
                            py_ctx,
                            state.tool_dispatcher.dispatch(
                                &call.name,
                                call.arguments.clone(),
                                &tool_ctx,
                                "gateway",
                            ),
                        )
                        .await
                } else {
                    state
                        .tool_dispatcher
                        .dispatch(&call.name, call.arguments.clone(), &tool_ctx, "gateway")
                        .await
                };
                match dispatch_result {
                    Ok(output) => (
                        output.result["output"].as_str().unwrap_or("").to_string(),
                        false,
                    ),
                    Err(e) => (e.to_string(), true),
                }
            };
            let duration = start.elapsed();
            let duration_ms = duration.as_millis() as u64;

            let status = if is_error { "error" } else { "success" };

            // ToolEnd is gated the same way as ToolStart.
            let tool_end = ServerEvent::ToolEnd {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                result: serde_json::json!(result_content),
                status: status.to_string(),
                duration_ms,
                truncated: false,
                total_bytes: None,
                parent_call_id: None,
            };
            if subscription.lock().await.should_deliver(&tool_end) {
                let _ = ws_tx.send(tool_end);
            }

            if let Err(e) = state.storage.update_tool_call_result(
                &call.call_id,
                turn.id,
                &result_content,
                status,
                duration_ms as i64,
                false,
                None,
            ).await {
                tracing::warn!("failed to update tool call result {}: {e}", call.call_id);
            }

            tool_results.push(ToolResult {
                call_id: call.call_id.clone(),
                content: result_content,
                is_error,
            });
        }

        let tool_messages = build_tool_round_messages(
            &round_tool_calls,
            &tool_results,
            &round_text,
        );

        let mut updated_messages = request.messages.clone();
        updated_messages.extend(tool_messages);
        request = CompletionRequest {
            messages: updated_messages,
            ..request.clone()
        };
    };

    // Step 11: Finalise based on outcome, then clean up.
    match outcome {
        TurnOutcome::Done => {
            // Position = current message count in thread.
            let position = state
                .storage
                .list_messages_by_thread(thread_id)
                .await
                .map(|msgs| msgs.len() as i32)
                .unwrap_or(1);

            state
                .storage
                .create_message(NewMessage {
                    thread_id,
                    role: Role::Assistant.to_string(),
                    content: accumulated,
                    position,
                    agent_id: "gateway".to_string(),
                })
                .await?;

            // Persist token usage from the provider if available.
            if let Some(usage) = &stored_usage {
                if let Err(e) = state.storage.update_tokens(
                    turn.id,
                    usage.input_tokens as i64,
                    usage.output_tokens as i64,
                ).await {
                    tracing::warn!(error = %e, "failed to persist token usage");
                }
            }

            // Emit a structured log event with token usage and derived cost.
            // These fields extend the turn span's correlation data for operators
            // monitoring per-turn costs from JSON log output.
            // input_tokens / output_tokens are None when the provider did not
            // report usage (e.g. streamed without a Usage chunk). cost_microcents
            // is None when no pricing entry is configured for the model.
            {
                let input_tokens = stored_usage.as_ref().map(|u| u.input_tokens as i64);
                let output_tokens = stored_usage.as_ref().map(|u| u.output_tokens as i64);
                let cost_microcents = stored_usage.as_ref().and_then(|u| {
                    state.config.pricing.compute_cost(
                        &config.provider.model,
                        u.input_tokens,
                        u.output_tokens,
                    )
                });
                tracing::info!(
                    input_tokens = ?input_tokens,
                    output_tokens = ?output_tokens,
                    cost_microcents = ?cost_microcents,
                    "turn_complete",
                );
            }

            state
                .storage
                .update_turn_state(turn.id, TurnState::Completed)
                .await?;

            state.storage.update_activity(session_id).await?;

            // Populate usage fields only for context-subscribed connections.
            let (input_tokens, output_tokens, cost_microcents) =
                if subscription.lock().await.is_subscribed(
                    crate::subscription::ObservabilityChannel::Context,
                ) {
                    (
                        stored_usage.as_ref().map(|u| u.input_tokens),
                        stored_usage.as_ref().map(|u| u.output_tokens),
                        None, // Cost derivation deferred to pricing config integration.
                    )
                } else {
                    (None, None, None)
                };

            let _ = ws_tx.send(ServerEvent::TurnComplete {
                turn_id: turn.id,
                input_tokens,
                output_tokens,
                cost_microcents,
            });
        }

        TurnOutcome::Cancelled => {
            if !accumulated.is_empty() {
                let position = state
                    .storage
                    .list_messages_by_thread(thread_id)
                    .await
                    .map(|msgs| msgs.len() as i32)
                    .unwrap_or(1);

                state
                    .storage
                    .create_message(NewMessage {
                        thread_id,
                        role: Role::Assistant.to_string(),
                        content: accumulated,
                        position,
                        agent_id: "gateway".to_string(),
                    })
                    .await?;
            }

            state
                .storage
                .update_turn_state(turn.id, TurnState::Interrupted)
                .await?;

            let _ = ws_tx.send(ServerEvent::Cancelled {});
        }

        TurnOutcome::ProviderError(message) => {
            state
                .storage
                .update_turn_state(turn.id, TurnState::Failed)
                .await?;

            let _ = ws_tx.send(ServerEvent::Error {
                code: "provider_error".to_string(),
                message,
            });
        }
    }

    // Cleanup: remove the cancellation token regardless of outcome.
    {
        let mut turns = state.active_turns.lock().await;
        turns.remove(&session_id);
    }

    Ok(())
}

/// Cancel the active turn for a session.
///
/// Returns an error if there is no active turn to cancel.
pub async fn cancel_turn(state: &AppState, session_id: Uuid) -> Result<(), GatewayError> {
    let turns = state.active_turns.lock().await;
    match turns.get(&session_id) {
        Some(token) => {
            token.cancel();
            Ok(())
        }
        None => Err(GatewayError::WebSocketError(format!(
            "no active turn for session {session_id}"
        ))),
    }
}

/// Internal outcome of a streaming turn, used to drive post-stream finalisation.
enum TurnOutcome {
    /// Stream finished normally (possibly after multiple tool rounds).
    Done,
    /// Client or shutdown requested cancellation.
    Cancelled,
    /// The provider sent an error chunk.
    ProviderError(String),
}

/// Outcome of a single streaming round within the tool loop.
enum RoundOutcome {
    /// Stream for this round finished (may or may not have tool calls).
    Done,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        collections::HashMap,
        pin::Pin,
        sync::Arc,
    };

    use async_trait::async_trait;
    use futures::stream;
    use tokio::sync::{Mutex, mpsc};

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason,
        ProviderError, StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_core::StorageError;
    use swage_storage::{
        model::{
            Message, NewMessage, NewProject, NewSession, NewThread, NewToolCall, NewTurn, Project,
            Session, Thread, ThreadState, ToolCallRecord, Turn, TurnState,
        },
        store::{
            ContextSnapshotStore, MessageStore, ProjectStore, SessionStore, ThreadStore,
            ToolCallStore, TurnStore,
        },
    };

    use super::*;
    use crate::{state::AppState, subscription::SubscriptionState, websocket::ServerEvent};

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
        fn model_id(&self) -> &str { "mock-model" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in streaming tests")
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
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

    #[async_trait]
    impl LlmProvider for MultiRoundMockProvider {
        fn model_id(&self) -> &str { "mock-multi-round" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in multi-round tests")
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
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

    /// A mock provider whose stream blocks forever.
    pub(crate) struct PendingProvider;

    #[async_trait]
    impl LlmProvider for PendingProvider {
        fn model_id(&self) -> &str { "pending-model" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in cancellation tests")
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not used in cancellation tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::pending())
        }
    }

    // -----------------------------------------------------------------------
    // Mock storage — in-memory Vec/HashMap backed, usable in tests.
    // -----------------------------------------------------------------------

    /// In-memory storage for testing.
    pub(crate) struct MockStorage {
        pub(crate) db: Mutex<InMemoryDb>,
    }

    #[derive(Default)]
    pub(crate) struct InMemoryDb {
        pub(crate) projects: HashMap<Uuid, Project>,
        pub(crate) sessions: HashMap<Uuid, Session>,
        pub(crate) threads: HashMap<Uuid, Thread>,
        pub(crate) turns: HashMap<Uuid, Turn>,
        pub(crate) messages: Vec<Message>,
        pub(crate) tool_calls: Vec<ToolCallRecord>,
        pub(crate) snapshots: HashMap<Uuid, String>,
    }

    impl MockStorage {
        pub(crate) fn new() -> Self {
            Self {
                db: Mutex::new(InMemoryDb::default()),
            }
        }
    }

    #[async_trait]
    impl ProjectStore for MockStorage {
        async fn create(&self, project: NewProject) -> Result<Project, StorageError> {
            let p = Project {
                id: Uuid::new_v4(),
                name: project.name,
                slug: project.slug,
                workspace_path: project.workspace_path,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.db.lock().await.projects.insert(p.id, p.clone());
            Ok(p)
        }

        async fn get(&self, id: Uuid) -> Result<Option<Project>, StorageError> {
            Ok(self.db.lock().await.projects.get(&id).cloned())
        }

        async fn get_by_name(&self, name: &str) -> Result<Option<Project>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .projects
                .values()
                .find(|p| p.name == name)
                .cloned())
        }

        async fn get_or_create_default(&self) -> Result<Project, StorageError> {
            if let Some(p) = self.get_by_name("default").await? {
                return Ok(p);
            }
            ProjectStore::create(self, NewProject {
                name: "default".to_string(),
                slug: "default".to_string(),
                workspace_path: None,
            })
            .await
        }

        async fn list_all(&self) -> Result<Vec<Project>, StorageError> {
            let mut projects: Vec<Project> =
                self.db.lock().await.projects.values().cloned().collect();
            projects.sort_by_key(|p| p.created_at);
            Ok(projects)
        }

        async fn get_by_slug(&self, slug: &str) -> Result<Option<Project>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .projects
                .values()
                .find(|p| p.slug == slug)
                .cloned())
        }

        async fn update(
            &self,
            id: Uuid,
            name: Option<String>,
            slug: Option<String>,
            workspace_path: Option<String>,
        ) -> Result<Project, StorageError> {
            let mut db = self.db.lock().await;
            match db.projects.get_mut(&id) {
                None => Err(StorageError::NotFound { entity: "Project".into(), id: id.to_string() }),
                Some(p) => {
                    if let Some(n) = name { p.name = n; }
                    if let Some(s) = slug { p.slug = s; }
                    if let Some(wp) = workspace_path { p.workspace_path = Some(wp); }
                    p.updated_at = chrono::Utc::now();
                    Ok(p.clone())
                }
            }
        }

        async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
            let mut db = self.db.lock().await;
            match db.projects.get(&id) {
                None => Ok(false),
                Some(p) if p.name == "default" => Err(StorageError::ConstraintViolation(
                    "cannot delete the default project".into(),
                )),
                _ => Ok(db.projects.remove(&id).is_some()),
            }
        }
    }

    #[async_trait]
    impl SessionStore for MockStorage {
        async fn create_session(&self, session: NewSession) -> Result<Session, StorageError> {
            let s = Session {
                id: Uuid::new_v4(),
                project_id: session.project_id,
                agent_id: session.agent_id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.db.lock().await.sessions.insert(s.id, s.clone());
            Ok(s)
        }

        async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
            Ok(self.db.lock().await.sessions.get(&id).cloned())
        }

        async fn list_by_project(
            &self,
            project_id: Uuid,
        ) -> Result<Vec<Session>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .sessions
                .values()
                .filter(|s| s.project_id == project_id)
                .cloned()
                .collect())
        }

        async fn list_sessions_by_agent_id(&self, agent_id: &str) -> Result<Vec<Session>, StorageError> {
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
            let mut db = self.db.lock().await;
            match db.threads.get_mut(&id) {
                None => Err(StorageError::NotFound { entity: "Thread".into(), id: id.to_string() }),
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

        async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
            Ok(self.db.lock().await.threads.remove(&id).is_some())
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
                user_input: None,
                assistant_response: None,
                input_tokens: None,
                output_tokens: None,
                cost_microcents: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            };
            self.db.lock().await.turns.insert(t.id, t.clone());
            Ok(t)
        }

        async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
            Ok(self.db.lock().await.turns.get(&id).cloned())
        }

        async fn list_turns_by_thread(
            &self,
            thread_id: Uuid,
        ) -> Result<Vec<Turn>, StorageError> {
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

        async fn update_turn_state(
            &self,
            id: Uuid,
            state: TurnState,
        ) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            if let Some(turn) = db.turns.get_mut(&id) {
                if matches!(state, TurnState::Completed | TurnState::Failed | TurnState::Interrupted) {
                    turn.completed_at = Some(chrono::Utc::now());
                }
                turn.state = state;
            }
            Ok(())
        }

        async fn find_in_progress_by_agent(
            &self,
            agent_id: &str,
        ) -> Result<Vec<Turn>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .turns
                .values()
                .filter(|t| {
                    t.agent_id == agent_id
                        && matches!(t.state, TurnState::Pending | TurnState::Processing)
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

        async fn update_tokens(
            &self,
            id: Uuid,
            input: i64,
            output: i64,
        ) -> Result<(), StorageError> {
            if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
                t.input_tokens = Some(input);
                t.output_tokens = Some(output);
            }
            Ok(())
        }

        async fn update_cost(&self, id: Uuid, cost: i64) -> Result<(), StorageError> {
            if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
                t.cost_microcents = Some(cost);
            }
            Ok(())
        }
    }

    #[async_trait]
    impl ContextSnapshotStore for MockStorage {
        async fn store_snapshot(
            &self,
            turn_id: Uuid,
            report_json: &str,
        ) -> Result<(), StorageError> {
            self.db.lock().await.snapshots.insert(turn_id, report_json.to_string());
            Ok(())
        }

        async fn get_snapshot(&self, turn_id: Uuid) -> Result<Option<String>, StorageError> {
            Ok(self.db.lock().await.snapshots.get(&turn_id).cloned())
        }
    }

    #[async_trait]
    impl MessageStore for MockStorage {
        async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError> {
            let m = Message {
                id: Uuid::new_v4(),
                thread_id: message.thread_id,
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

        async fn list_messages_by_thread(
            &self,
            thread_id: Uuid,
        ) -> Result<Vec<Message>, StorageError> {
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
            Ok(self.db.lock().await.messages.iter().find(|m| m.id == id).cloned())
        }

        async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError> {
            self.db.lock().await.messages.retain(|m| m.thread_id != thread_id);
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
                result: tool_call.result,
                status: tool_call.status,
                duration_ms: tool_call.duration_ms,
                truncated: tool_call.truncated,
                total_bytes: tool_call.total_bytes,
                position: tool_call.position,
                created_at: chrono::Utc::now(),
                parent_call_id: tool_call.parent_call_id,
            };
            self.db.lock().await.tool_calls.push(record.clone());
            Ok(record)
        }

        async fn get_tool_call(
            &self,
            id: Uuid,
        ) -> Result<Option<ToolCallRecord>, StorageError> {
            let db = self.db.lock().await;
            Ok(db.tool_calls.iter().find(|tc| tc.id == id).cloned())
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


    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build an AppState with a pre-populated session and its default thread.
    ///
    /// Returns the state and the session_id. The thread is created automatically
    /// so that `process_turn` can look it up via `list_by_session`.
    pub(crate) async fn make_state_with_session(
        provider_chunks: Vec<StreamChunk>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        // Create the default project via ProjectStore.
        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        // Create a session.
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        // Create the default thread for this session.
        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create default thread");

        let provider = Arc::new(MockProvider::new(provider_chunks));
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
        };
        (state, session_id)
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn full_turn_stores_messages_and_completes() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Delta { content: ", world".to_string() },
            StreamChunk::Delta { content: "!".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete without error");

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
        assert!(events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. })));

        // Get thread_id and verify messages stored in the DB.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let messages = state.storage.list_messages_by_thread(thread_id).await.unwrap();
        assert_eq!(messages.len(), 2, "user message + assistant message");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hi");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "Hello, world!");

        // Verify the turn was marked Completed.
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].state, TurnState::Completed);
    }

    #[tokio::test]
    async fn cancelled_turn_stores_partial_and_marks_interrupted() {
        let storage = Arc::new(MockStorage::new());

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create default thread");

        let state = Arc::new(AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: Arc::new(PendingProvider) as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
        });

        let (tx, mut rx) = mpsc::unbounded_channel();

        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            process_turn(&*state_clone, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await
        });

        for _ in 0..50 {
            tokio::task::yield_now().await;
            if state.active_turns.lock().await.contains_key(&session_id) {
                break;
            }
        }

        assert!(
            state.active_turns.lock().await.contains_key(&session_id),
            "turn should be active before cancel"
        );

        cancel_turn(&state, session_id)
            .await
            .expect("cancel should succeed");

        handle.await.expect("task should not panic").expect("process_turn ok");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| matches!(e, ServerEvent::Cancelled {})),
            "expected Cancelled event"
        );

        // Verify turn is Interrupted in storage.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns[0].state, TurnState::Interrupted);
    }

    #[tokio::test]
    async fn provider_error_marks_turn_failed() {
        let (state, session_id) = make_state_with_session(vec![StreamChunk::Error {
            message: "provider overloaded".to_string(),
        }])
        .await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("error is surfaced via event, not Err return");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            ServerEvent::Error { code, .. } if code == "provider_error"
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
            })
            .await
            .unwrap();
        state
            .storage
            .update_turn_state(turn.id, TurnState::Processing)
            .await
            .unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = process_turn(&state, session_id, "Another message".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await;

        assert!(
            matches!(result, Err(GatewayError::TurnInProgress { .. })),
            "expected TurnInProgress, got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Mock tool for the execution loop
    // -----------------------------------------------------------------------

    struct EchoMockTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for EchoMockTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "Echo the input back" }
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

    /// Build an AppState with a multi-round provider and an EchoMockTool.
    async fn make_state_with_tools(
        rounds: Vec<Vec<StreamChunk>>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create default thread");

        let provider = Arc::new(MultiRoundMockProvider::new(rounds));

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![
                Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>,
            ])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
        };
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
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "The tool said: hello".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use echo".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
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

        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
    }

    #[tokio::test]
    async fn tool_call_stores_tool_call_record() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_store".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "stored" }),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Store this".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 1, "expected 1 tool call record");
        assert_eq!(tool_calls[0].call_id, "call_store");
        assert_eq!(tool_calls[0].tool, "echo");
        assert!(tool_calls[0].result.is_some(), "tool call should have a result");
        assert_eq!(tool_calls[0].result.as_deref(), Some("stored"));
    }

    #[tokio::test]
    async fn unknown_tool_returns_error_to_model() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_unknown".into(),
                name: "nonexistent_tool".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "Sorry, that tool doesn't exist.".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use unknown".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
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

        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
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
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::ToolUse {
                call_id: "c3".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "third" }),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_3 = vec![
            StreamChunk::Delta { content: "All done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) =
            make_state_with_tools(vec![round_1, round_2, round_3]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Multi-round".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let tool_starts: Vec<_> = events.iter().filter(|e| matches!(e, ServerEvent::ToolStart { .. })).collect();
        let tool_ends: Vec<_> = events.iter().filter(|e| matches!(e, ServerEvent::ToolEnd { .. })).collect();
        assert_eq!(tool_starts.len(), 3, "expected 3 ToolStart events");
        assert_eq!(tool_ends.len(), 3, "expected 3 ToolEnd events");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 3, "expected 3 tool call records");

        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
    }

    // -----------------------------------------------------------------------
    // Project context and config resolution tests
    // -----------------------------------------------------------------------

    struct CapturingProvider {
        captured: Arc<std::sync::Mutex<Option<CompletionRequest>>>,
        chunks: Vec<StreamChunk>,
    }

    impl CapturingProvider {
        fn new(chunks: Vec<StreamChunk>) -> (Self, Arc<std::sync::Mutex<Option<CompletionRequest>>>) {
            let captured = Arc::new(std::sync::Mutex::new(None));
            (
                Self { captured: Arc::clone(&captured), chunks },
                captured,
            )
        }
    }

    #[async_trait]
    impl LlmProvider for CapturingProvider {
        fn model_id(&self) -> &str { "capturing-model" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!()
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!()
        }

        fn stream(&self, request: CompletionRequest) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            *self.captured.lock().unwrap() = Some(request);
            Box::pin(stream::iter(self.chunks.clone()))
        }
    }

    struct ContextCaptureTool {
        captured: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for ContextCaptureTool {
        fn name(&self) -> &str { "capture_context" }
        fn description(&self) -> &str { "Captures the working_directory from the tool context" }
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
        let storage = Arc::new(MockStorage::new());
        let project = storage.get_or_create_default().await.expect("project");
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("session");
        let session_id = session.id;
        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("thread");
        (storage, session_id)
    }

    #[tokio::test]
    async fn process_turn_passes_tool_context_to_tools() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let capture_tool = Arc::new(ContextCaptureTool { captured: Arc::clone(&captured) });

        let (storage, session_id) = make_storage_with_session().await;

        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "cc_1".into(),
                name: "capture_context".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![StreamChunk::Delta { content: "done".into() }, StreamChunk::Done { finish_reason: None }];

        let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![
                capture_tool as Arc<dyn crate::tool::Tool>,
            ])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "capture".into(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await.expect("turn ok");

        let working_dir = captured
            .lock()
            .unwrap()
            .take()
            .expect("ToolContext should have been passed during tool execution");

        assert!(working_dir.is_absolute(), "working_directory should be absolute: {working_dir:?}");
    }

    #[tokio::test]
    async fn process_turn_resolves_config_per_project_and_completes_turn() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".into() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (provider, captured_request) = CapturingProvider::new(chunks);

        let storage = Arc::new(MockStorage::new());
        let project = ProjectStore::create(&*storage, swage_storage::model::NewProject {
            name: "Test Config Project".to_string(),
            slug: "test-config-resolution-no-project-toml".to_string(),
            workspace_path: None,
        })
        .await
        .expect("create test project");
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("session");
        let session_id = session.id;
        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("thread");

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: Arc::new(provider) as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "hi".into(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await.expect("turn ok");

        let req = captured_request.lock().unwrap().take().expect("request captured");
        let model = req.model.as_deref().unwrap_or("");
        assert!(!model.is_empty(), "model must be non-empty regardless of config source");
    }

    #[tokio::test]
    async fn text_only_turn_still_works_with_tool_registry() {
        let (state, session_id) = make_state_with_tools(vec![vec![
            StreamChunk::Delta { content: "Just text".into() },
            StreamChunk::Done { finish_reason: None },
        ]]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hello".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, ServerEvent::Token { content } if content == "Just text")));
        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));

        assert!(!events.iter().any(|e| matches!(e, ServerEvent::ToolStart { .. })));
    }

    // -----------------------------------------------------------------------
    // ToolContext propagation tests
    // -----------------------------------------------------------------------

    struct WorkingDirReadTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for WorkingDirReadTool {
        fn name(&self) -> &str { "working_dir_read" }
        fn description(&self) -> &str { "Returns the working_directory from the ToolContext" }
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

    #[tokio::test]
    async fn tool_context_working_directory_is_passed_during_tool_execution() {
        let storage = Arc::new(MockStorage::new());
        let project = ProjectStore::create(&*storage, NewProject {
            name: "my-project".to_string(),
            slug: "my-project".to_string(),
            workspace_path: None,
        })
        .await
        .expect("create project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("thread");

        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "ctx-check".into(),
                name: "working_dir_read".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![
                Arc::new(WorkingDirReadTool) as Arc<dyn crate::tool::Tool>,
            ])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "Check context".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 1);
        let result = tool_calls[0].result.as_deref().unwrap_or("");
        assert!(
            std::path::Path::new(result).is_absolute(),
            "working_directory should be an absolute path, got: {result}"
        );
    }

    // -----------------------------------------------------------------------
    // Sandbox dispatch routing tests
    // -----------------------------------------------------------------------

    /// Records calls and returns a predefined result.
    struct MockSandbox {
        calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
        result: std::sync::Mutex<Option<Result<swage_sandbox::ToolCallResult, swage_core::SandboxError>>>,
    }

    impl MockSandbox {
        fn returning(result: Result<swage_sandbox::ToolCallResult, swage_core::SandboxError>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(vec![])),
                result: std::sync::Mutex::new(Some(result)),
            }
        }
    }

    #[async_trait]
    impl swage_sandbox::Sandbox for MockSandbox {
        async fn spawn_agent(&mut self) -> Result<(), swage_core::SandboxError> {
            Ok(())
        }

        async fn execute_tool(
            &self,
            name: &str,
            params: serde_json::Value,
        ) -> Result<swage_sandbox::ToolCallResult, swage_core::SandboxError> {
            self.calls.lock().await.push((name.to_string(), params));
            self.result.lock().unwrap().take().expect("execute_tool called more than once")
        }

        async fn shutdown(&mut self) -> Result<(), swage_core::SandboxError> {
            Ok(())
        }
    }

    /// Build an AppState with an active sandbox and no tools in the direct dispatcher.
    ///
    /// With no tools registered, direct dispatch would always fail — this lets
    /// us verify sandbox routing by checking whether the tool call succeeds.
    async fn make_state_with_sandbox(
        rounds: Vec<Vec<StreamChunk>>,
        sandbox: Arc<dyn swage_sandbox::Sandbox>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        let project = storage.get_or_create_default().await.expect("project");
        let session = storage
            .create_session(NewSession { project_id: project.id, agent_id: "gateway".to_string() })
            .await
            .expect("session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread { session_id, agent_id: "gateway".to_string() })
            .await
            .expect("thread");

        let provider = Arc::new(MultiRoundMockProvider::new(rounds));
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            // Intentionally empty: direct dispatch would fail for any tool name.
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: Some(sandbox),
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
        };
        (state, session_id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sandbox_routes_tool_calls_through_sandbox_when_present() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "sb_call".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "sandbox_hi"}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Ok(
            swage_sandbox::ToolCallResult::success(serde_json::json!("sandbox_output"), 5),
        )));
        let (state, session_id) = make_state_with_sandbox(vec![round_1, round_2], sandbox).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use echo".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        // The empty direct dispatcher would produce status="error". Only the
        // sandbox path can produce status="success" here.
        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolEnd { call_id, status, .. }
                if call_id == "sb_call" && status == "success"
            )),
            "sandbox tool call should succeed; got events: {events:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sandbox_ipc_error_produces_tool_end_error_with_communication_message() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "err_call".into(),
                name: "broken".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Err(
            swage_core::SandboxError::IpcError { message: "pipe broken".to_string() },
        )));
        let (state, session_id) = make_state_with_sandbox(vec![round_1, round_2], sandbox).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use broken".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let tool_end = events
            .iter()
            .find(|e| matches!(e, ServerEvent::ToolEnd { call_id, .. } if call_id == "err_call"))
            .expect("expected ToolEnd for err_call");

        match tool_end {
            ServerEvent::ToolEnd { status, result, .. } => {
                assert_eq!(status, "error", "IPC error must produce error status");
                let msg = result.as_str().unwrap_or("");
                assert!(
                    msg.contains("sandbox communication error"),
                    "result should contain 'sandbox communication error', got: {msg}"
                );
            }
            _ => panic!("expected ToolEnd"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sandbox_tool_error_field_produces_tool_end_error() {
        // ToolCallResult.error = Some(...) means the sandboxed tool itself failed.
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "fail_call".into(),
                name: "fail_tool".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Ok(
            swage_sandbox::ToolCallResult::failed("tool execution failed inside sandbox", 10),
        )));
        let (state, session_id) = make_state_with_sandbox(vec![round_1, round_2], sandbox).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use fail_tool".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let tool_end = events
            .iter()
            .find(|e| matches!(e, ServerEvent::ToolEnd { call_id, .. } if call_id == "fail_call"))
            .expect("expected ToolEnd for fail_call");

        match tool_end {
            ServerEvent::ToolEnd { status, result, .. } => {
                assert_eq!(status, "error");
                let msg = result.as_str().unwrap_or("");
                assert!(
                    msg.contains("tool execution failed"),
                    "result should contain the sandbox error message, got: {msg}"
                );
            }
            _ => panic!("expected ToolEnd"),
        }
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
        use tracing_subscriber::{layer::SubscriberExt, Registry};

        // Accumulates (span_name, [field_names]) for every span created
        // during process_turn. Shared between the subscriber and the assertions.
        let recorded: StdArc<StdMutex<Vec<(String, Vec<String>)>>> =
            StdArc::new(StdMutex::new(Vec::new()));
        let captured = StdArc::clone(&recorded);

        // Minimal Layer that records span names and field names on creation.
        struct SpanFieldRecorder(StdArc<StdMutex<Vec<(String, Vec<String>)>>>);

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
                        StreamChunk::Delta { content: "ok".to_string() },
                        StreamChunk::Done { finish_reason: None },
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

                    process_turn(&state, session_id, "test".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
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
        use tracing_subscriber::{layer::SubscriberExt, Registry};

        // Accumulates (level_str, [field_names]) for every event emitted
        // during process_turn. Shared between the layer and the assertions.
        let recorded: StdArc<StdMutex<Vec<(String, Vec<String>)>>> =
            StdArc::new(StdMutex::new(Vec::new()));
        let captured = StdArc::clone(&recorded);

        struct EventFieldRecorder(StdArc<StdMutex<Vec<(String, Vec<String>)>>>);

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
                        StreamChunk::Delta { content: "ok".to_string() },
                        StreamChunk::Usage(TokenUsage { input_tokens: 100, output_tokens: 50 }),
                        StreamChunk::Done { finish_reason: None },
                    ];
                    let (state, session_id) = make_state_with_session(chunks).await;
                    let (tx, _rx) = mpsc::unbounded_channel();

                    tracing::callsite::rebuild_interest_cache();

                    process_turn(
                        &state,
                        session_id,
                        "test".to_string(),
                        tx,
                        Arc::new(Mutex::new(SubscriptionState::new(true))),
                    )
                    .await
                    .expect("turn should complete");
                });
        });

        let events = recorded.lock().unwrap();

        // Find an INFO event that carries token usage fields.
        let usage_events: Vec<_> = events
            .iter()
            .filter(|(level, fields)| {
                level == "INFO" && fields.contains(&"input_tokens".to_string())
            })
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
        let history = HistoryElement::new(vec![
            ChatMessage::new(Role::User, "Hello"),
        ]);

        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(prompts),
            Box::new(history),
        ];

        let config = swage_config::ContextConfig::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

        let output = swage_context::assembly::default_assembly(
            &elements, &config, &tokeniser,
        );

        // System message from instructions appears first.
        assert_eq!(
            output.messages[0].role, Role::System,
            "first message should be system instructions"
        );
        // User message from history follows.
        assert_eq!(
            output.messages[1].role, Role::User,
            "second message should be user history"
        );

        // Report should have non-zero token counts for both elements.
        let prompts_report = output.report.elements.iter()
            .find(|e| e.name == "prompts")
            .expect("report should include prompts element");
        assert!(
            prompts_report.token_count > 0,
            "prompts element should have non-zero token count"
        );

        let history_report = output.report.elements.iter()
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
        let history = HistoryElement::new(vec![
            ChatMessage::new(Role::User, "Hello"),
        ]);

        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(prompts),
            Box::new(history),
        ];

        let config = swage_config::ContextConfig::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

        let output = swage_context::assembly::default_assembly(
            &elements, &config, &tokeniser,
        );

        // No system message when instructions are empty.
        assert!(
            output.messages.iter().all(|m| m.role != Role::System),
            "expected no system message when prompts are empty"
        );
        assert_eq!(output.messages.len(), 1, "should only contain the user message");
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

        assert_eq!(content.len(), 1, "should produce exactly one system message");
        assert_eq!(content[0].role, flat_system.role);
        // Content should be identical — single rule, no joining needed.
        assert_eq!(content[0].content, flat_system.content);
    }

    /// The build_assembly_elements helper constructs the correct elements for
    /// the turn's context assembly. This function is tested directly to verify
    /// the wiring before it's used in process_turn.
    #[test]
    fn build_assembly_elements_creates_prompts_and_history() {
        let instructions = "Be helpful.";
        let history = vec![
            ChatMessage::new(Role::User, "Hi"),
            ChatMessage::new(Role::Assistant, "Hello!"),
        ];

        let elements = build_assembly_elements(instructions, history);

        // Should produce two elements: prompts (priority 100) and history (priority 0).
        assert_eq!(elements.len(), 2);

        let names: Vec<&str> = elements.iter().map(|e| e.name()).collect();
        assert!(names.contains(&"prompts"), "should include prompts element");
        assert!(names.contains(&"history"), "should include history element");
    }

    /// When instructions are empty, build_assembly_elements still includes the
    /// prompts element (which will produce no system message) and the history.
    #[test]
    fn build_assembly_elements_with_empty_instructions() {
        let elements = build_assembly_elements("", vec![
            ChatMessage::new(Role::User, "Hello"),
        ]);

        assert_eq!(elements.len(), 2);
        // PromptsElement with empty rules produces no messages.
        let prompts = elements.iter().find(|e| e.name() == "prompts").unwrap();
        assert!(prompts.content().is_empty());
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
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Usage(TokenUsage { input_tokens: 42, output_tokens: 17 }),
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns.len(), 1);

        let turn = &turns[0];
        assert_eq!(
            turn.input_tokens, Some(42),
            "input_tokens should be persisted from provider usage"
        );
        assert_eq!(
            turn.output_tokens, Some(17),
            "output_tokens should be persisted from provider usage"
        );
    }

    /// When the provider does not return a Usage chunk, token fields should
    /// remain NULL (None) in the turn record.
    #[tokio::test]
    async fn token_usage_remains_null_when_provider_sends_no_usage() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();

        let turn = &turns[0];
        assert!(
            turn.input_tokens.is_none(),
            "input_tokens should remain NULL when no usage reported"
        );
        assert!(
            turn.output_tokens.is_none(),
            "output_tokens should remain NULL when no usage reported"
        );
    }

    // -----------------------------------------------------------------------
    // ContextAssembled event and snapshot persistence tests
    // -----------------------------------------------------------------------

    /// ContextAssembled must be the first event before any Token events.
    #[tokio::test]
    async fn context_assembled_precedes_first_token() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
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
                    report.get("elements").is_some(),
                    "report should contain elements key"
                );
            }
            other => panic!("expected ContextAssembled, got: {other:?}"),
        }
    }

    /// The assembly report should be persisted as a context snapshot.
    #[tokio::test]
    async fn assembly_report_persisted_as_context_snapshot() {
        use swage_storage::store::ContextSnapshotStore;

        let chunks = vec![
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
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
    }

    /// When subscription is off, ContextAssembled is not delivered but
    /// snapshot persistence still happens.
    #[tokio::test]
    async fn context_assembled_not_sent_when_unsubscribed() {
        use swage_storage::store::ContextSnapshotStore;

        let chunks = vec![
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = false, no channels subscribed.
        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
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
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Usage(TokenUsage { input_tokens: 100, output_tokens: 50 }),
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = true means all channels are subscribed.
        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
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
                assert_eq!(*output_tokens, Some(50), "output_tokens should be populated");
            }
            other => panic!("expected TurnComplete, got: {other:?}"),
        }
    }

    /// When NOT subscribed to context channel, TurnComplete omits usage fields.
    #[tokio::test]
    async fn turn_complete_omits_usage_when_not_subscribed() {
        use swage_core::TokenUsage;

        let chunks = vec![
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Usage(TokenUsage { input_tokens: 100, output_tokens: 50 }),
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = false, no subscriptions.
        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
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
                assert_eq!(*input_tokens, None, "input_tokens should be None when unsubscribed");
                assert_eq!(*output_tokens, None, "output_tokens should be None when unsubscribed");
                assert_eq!(*cost_microcents, None, "cost_microcents should be None when unsubscribed");
            }
            other => panic!("expected TurnComplete, got: {other:?}"),
        }
    }
}
