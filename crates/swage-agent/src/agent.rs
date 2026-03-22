//! Core agent struct and public API.
//!
//! The [`Agent`] is the central runtime component. It owns all dependencies,
//! manages sessions and threads, and processes messages through the LLM with
//! tool dispatch, hooks, leak detection, and context compaction.

use std::sync::Arc;

use uuid::Uuid;

use serde_json::Value;
use swage_core::{
    ChatMessage, CompletionRequest, ContentPart, FinishReason, Role, ToolCompletionRequest,
    ToolResult, build_tool_round_messages,
};
use swage_hook::{HookEvent, HookOutcome, HookRegistry};
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::{
    NewMessage, NewSession, NewThread, NewToolCall, NewTurn, Storage, ThreadState, ThreadStore,
    TurnState,
};
use swage_tool::dispatch::ToolDispatcher;

use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::message::{InboundMessage, OutboundMessage};

// ---------------------------------------------------------------------------
// Agent struct
// ---------------------------------------------------------------------------

/// A single-agent runtime that processes messages through an LLM.
pub struct Agent {
    pub(crate) config: AgentConfig,
    pub(crate) provider: Arc<dyn LlmProvider>,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) tool_dispatcher: Option<ToolDispatcher>,
    pub(crate) hook_registry: Option<Arc<HookRegistry>>,
    pub(crate) leak_detector: Option<Arc<LeakDetector>>,

    /// The currently active session, if any.
    pub(crate) active_session_id: Option<Uuid>,
    /// The currently active thread within the active session.
    pub(crate) active_thread_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Session management
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Message processing — the core execution loop
// ---------------------------------------------------------------------------

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
    /// Fire `SessionStart` — non-blocking fire-and-forget.
    pub(crate) fn fire_session_start(&self, session_id: &str) {
        let Some(registry) = self.hook_registry.clone() else {
            return;
        };
        let session_id = session_id.to_owned();
        tokio::spawn(async move {
            registry
                .dispatch(HookEvent::SessionStart { session_id })
                .await;
        });
    }

    /// Fire `SessionEnd` — non-blocking fire-and-forget.
    pub(crate) fn fire_session_end(&self, session_id: &str) {
        let Some(registry) = self.hook_registry.clone() else {
            return;
        };
        let session_id = session_id.to_owned();
        tokio::spawn(async move {
            registry
                .dispatch(HookEvent::SessionEnd { session_id })
                .await;
        });
    }

    /// Fire `AgentStop` — non-blocking fire-and-forget.
    pub(crate) fn fire_agent_stop(&self, agent_id: &str) {
        let Some(registry) = self.hook_registry.clone() else {
            return;
        };
        let agent_id = agent_id.to_owned();
        tokio::spawn(async move {
            registry
                .dispatch(HookEvent::AgentStop { agent_id })
                .await;
        });
    }

    /// Fire `PreCompact` and return the outcome for the caller to inspect.
    ///
    /// Unlike the other lifecycle hooks, `PreCompact` is blocking — the
    /// outcome can flag content for preservation before the compaction runs.
    /// When no registry is configured, returns `HookOutcome::Continue`.
    pub(crate) async fn fire_pre_compact(
        &self,
        context: &serde_json::Value,
    ) -> HookOutcome {
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
    /// If a hook blocks, the returned error should be converted by the caller
    /// into a tool result string — this is not a turn-level failure.
    /// Returns `Ok(())` immediately when no registry is configured.
    pub(crate) async fn dispatch_pre_tool_use(
        &self,
        tool_name: &str,
        params: &serde_json::Value,
    ) -> Result<(), AgentError> {
        let Some(registry) = &self.hook_registry else {
            return Ok(());
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
            // Continue and ContinueModified both mean "proceed".
            _ => Ok(()),
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
    /// Thread state transitions: Idle → Processing → Idle (or Idle on error).
    /// Turn state transitions: Pending → Processing → Completed (or Failed).
    pub async fn process_message(
        &mut self,
        msg: InboundMessage,
    ) -> Result<OutboundMessage, AgentError> {
        // 1. Resolve target thread — explicit thread_id takes precedence,
        //    then fall back to the agent's active thread.
        let thread_id = msg.thread_id.or(self.active_thread_id).ok_or_else(|| {
            AgentError::InvalidConfig(
                "no active thread — create or resume a session first".into(),
            )
        })?;

        // 2. Create turn in Pending state — records the intent before any work.
        let turn = self
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: self.config.agent_id.clone(),
                state: TurnState::Pending,
            })
            .await?;

        // 3. Persist user message (position from existing count).
        let existing_messages = self.storage.list_messages_by_thread(thread_id).await?;
        let next_position = existing_messages.len() as i32;
        self.storage
            .create_message(NewMessage {
                thread_id,
                role: "user".into(),
                content: msg.message.clone(),
                position: next_position,
                agent_id: self.config.agent_id.clone(),
            })
            .await?;

        // 4. BeforeInbound hook — may block or modify content.
        let _processed_content = match self.dispatch_before_inbound(&msg.message).await {
            Ok(content) => content,
            Err(e) => {
                // Mark turn as Failed on hook block.
                let _ = self
                    .storage
                    .update_turn_state(turn.id, TurnState::Failed)
                    .await;
                return Err(e);
            }
        };

        // 5–6. Transition turn → Processing, thread → Processing.
        self.storage
            .update_turn_state(turn.id, TurnState::Processing)
            .await?;
        self.storage
            .update_state(thread_id, ThreadState::Processing)
            .await?;

        // 7. Build LLM context from thread message history.
        let mut context = self.build_context_from_thread(thread_id).await?;
        let mut current_position = next_position + 1;
        let mut total_tool_calls: u32 = 0;
        let mut total_input_tokens: u32 = 0;
        let mut total_output_tokens: u32 = 0;

        // 8. Call LLM — tool-aware path if dispatcher present, text-only otherwise.
        let final_content = if let Some(ref dispatcher) = self.tool_dispatcher {
            // Tool-aware path: use complete_with_tools in a multi-round loop.
            let tool_defs = dispatcher.tool_definitions();
            let mut round_count: u32 = 0;

            loop {
                let request = ToolCompletionRequest {
                    messages: context.clone(),
                    model: None,
                    max_tokens: None,
                    temperature: None,
                    stop_sequences: None,
                    metadata: None,
                    tool_definitions: tool_defs.clone(),
                };

                let response = match self
                    .call_with_context_recovery(request, turn.id, thread_id, &mut context)
                    .await
                {
                    Ok(resp) => resp,
                    Err(e) => {
                        self.fail_turn_and_idle(turn.id, thread_id).await;
                        return Err(e);
                    }
                };

                total_input_tokens += response.input_tokens;
                total_output_tokens += response.output_tokens;

                // If LLM returned text (no tool calls), we're done.
                if response.finish_reason != FinishReason::ToolUse
                    || response.tool_calls.is_empty()
                {
                    break response.content;
                }

                // Check max rounds before executing this batch of tool calls.
                round_count += 1;
                if round_count > self.config.max_tool_rounds {
                    self.fail_turn_and_idle(turn.id, thread_id).await;
                    return Err(AgentError::ToolRoundsExceeded {
                        max: self.config.max_tool_rounds,
                    });
                }

                // --- Tool dispatch cycle ---
                let tool_results = self
                    .execute_tool_calls(
                        &response.tool_calls,
                        turn.id,
                        thread_id,
                        &mut current_position,
                    )
                    .await?;
                total_tool_calls += response.tool_calls.len() as u32;

                // Build tool round messages and extend context for the next LLM call.
                let round_messages = build_tool_round_messages(
                    &response.tool_calls,
                    &tool_results,
                    &response.content,
                );
                context.extend(round_messages);
            }
        } else {
            // No tool dispatcher — text-only path using simple complete().
            let request = CompletionRequest {
                messages: context,
                model: None,
                max_tokens: None,
                temperature: None,
                stop_sequences: None,
                metadata: None,
            };

            let response = match self.provider.complete(request).await {
                Ok(resp) => resp,
                Err(e) => {
                    self.fail_turn_and_idle(turn.id, thread_id).await;
                    return Err(e.into());
                }
            };

            total_input_tokens += response.input_tokens;
            total_output_tokens += response.output_tokens;
            response.content
        };

        // 9a. AfterResponse hook — may block or modify content.
        let response_content = match self.dispatch_after_response(&final_content).await {
            Ok(content) => content,
            Err(e) => {
                self.fail_turn_and_idle(turn.id, thread_id).await;
                return Err(e);
            }
        };

        // 9b. Leak detection — redact any exposed credentials.
        let safe_content = self.scan_output(&response_content);

        // 9c. Persist assistant message.
        self.storage
            .create_message(NewMessage {
                thread_id,
                role: "assistant".into(),
                content: safe_content.clone(),
                position: current_position,
                agent_id: self.config.agent_id.clone(),
            })
            .await?;

        // 9d. Update turn with response and tokens, mark Completed.
        self.storage
            .update_response(turn.id, &safe_content)
            .await?;
        self.storage
            .update_tokens(turn.id, total_input_tokens.into(), total_output_tokens.into())
            .await?;
        self.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await?;

        // 9e. Thread back to Idle.
        self.storage
            .update_state(thread_id, ThreadState::Idle)
            .await?;

        // 9f. Return the outbound message.
        Ok(OutboundMessage {
            response: safe_content,
            turn_id: turn.id,
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
            tool_calls_count: total_tool_calls,
        })
    }

    // -----------------------------------------------------------------------
    // Private helpers for process_message
    // -----------------------------------------------------------------------

    /// Build ChatMessage context from all messages in the thread.
    async fn build_context_from_thread(
        &self,
        thread_id: Uuid,
    ) -> Result<Vec<ChatMessage>, AgentError> {
        let all_messages = self.storage.list_messages_by_thread(thread_id).await?;
        Ok(all_messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "system" => Role::System,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                ChatMessage {
                    id: m.id,
                    role,
                    content: vec![ContentPart::Text {
                        text: m.content.clone(),
                    }],
                    created_at: m.created_at,
                    name: None,
                    tool_call_id: m.tool_call_id.clone(),
                    tool_calls: None,
                    content_blocks: None,
                }
            })
            .collect())
    }

    /// Execute a batch of tool calls from the LLM, returning ToolResults for
    /// the next LLM round.
    ///
    /// For each tool call:
    /// 1. PreToolUse hook — if blocked, use block reason as result
    /// 2. Dispatch through tool_dispatcher (errors become tool results, not turn failures)
    /// 3. PostToolUse hook (informational)
    /// 4. Persist tool call record
    /// 5. Scan result with leak detector
    /// 6. Persist tool result message (role="tool")
    async fn execute_tool_calls(
        &self,
        tool_calls: &[swage_core::ToolCall],
        turn_id: Uuid,
        thread_id: Uuid,
        current_position: &mut i32,
    ) -> Result<Vec<ToolResult>, AgentError> {
        use crate::context::AgentToolContext;

        let dispatcher = self
            .tool_dispatcher
            .as_ref()
            .expect("execute_tool_calls requires tool_dispatcher");

        let ctx = AgentToolContext::new(
            &self.config.agent_id,
            self.active_session_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            std::env::current_dir().unwrap_or_default(),
        );

        let mut results = Vec::with_capacity(tool_calls.len());

        for (i, tc) in tool_calls.iter().enumerate() {
            // 1. PreToolUse hook — may block the tool call.
            let (result_content, is_error, status, duration_ms) =
                match self.dispatch_pre_tool_use(&tc.name, &tc.arguments).await {
                    Err(AgentError::HookBlocked { reason, .. }) => {
                        // Hook blocked — use block reason as tool result.
                        (reason, true, "blocked".to_string(), None)
                    }
                    Err(e) => {
                        // Unexpected hook error — treat as tool error.
                        (format!("hook error: {e}"), true, "error".to_string(), None)
                    }
                    Ok(()) => {
                        // 2. Dispatch through tool_dispatcher.
                        match dispatcher
                            .dispatch(&tc.name, tc.arguments.clone(), &ctx, &self.config.agent_id)
                            .await
                        {
                            Ok(output) => {
                                let result_str =
                                    serde_json::to_string(&output.result).unwrap_or_default();
                                let dur = Some(output.duration.as_millis() as i64);
                                (result_str, false, "success".to_string(), dur)
                            }
                            Err(tool_err) => {
                                // Tool errors become tool results, not turn failures.
                                (
                                    tool_err.to_string(),
                                    true,
                                    "error".to_string(),
                                    None,
                                )
                            }
                        }
                    }
                };

            // 3. PostToolUse hook (informational — fire and forget).
            let result_value: Value =
                serde_json::from_str(&result_content).unwrap_or(Value::String(result_content.clone()));
            self.dispatch_post_tool_use(&tc.name, &tc.arguments, &result_value)
                .await;

            // 4. Persist tool call record.
            let _ = self
                .storage
                .create_tool_call(NewToolCall {
                    turn_id,
                    agent_id: self.config.agent_id.clone(),
                    call_id: tc.call_id.clone(),
                    tool: tc.name.clone(),
                    arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                    result: Some(result_content.clone()),
                    status: Some(status),
                    duration_ms,
                    truncated: false,
                    total_bytes: None,
                    position: i as i32,
                    parent_call_id: None,
                })
                .await;

            // 5. Scan result with leak detector.
            let safe_result = self.scan_output(&result_content);

            // 6. Persist tool result message.
            self.storage
                .create_message(NewMessage {
                    thread_id,
                    role: "tool".into(),
                    content: safe_result.clone(),
                    position: *current_position,
                    agent_id: self.config.agent_id.clone(),
                })
                .await?;
            *current_position += 1;

            results.push(ToolResult {
                call_id: tc.call_id.clone(),
                content: safe_result,
                is_error,
            });
        }

        Ok(results)
    }

    /// Call the LLM with context-too-long recovery.
    ///
    /// If the provider returns an error whose message contains "context"
    /// (case-insensitive), triggers compaction and retries the call once.
    /// If the retry also fails, returns `AgentError::ContextTooLong`.
    async fn call_with_context_recovery(
        &self,
        request: ToolCompletionRequest,
        turn_id: Uuid,
        thread_id: Uuid,
        context: &mut Vec<ChatMessage>,
    ) -> Result<swage_core::ToolCompletionResponse, AgentError> {
        use crate::compaction;

        match self.provider.complete_with_tools(request).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                // Check if this is a context-too-long error.
                let error_msg = e.to_string().to_lowercase();
                if !error_msg.contains("context") {
                    return Err(e.into());
                }

                // Attempt compaction and retry once.
                tracing::warn!("context-too-long detected, attempting compaction");
                let compacted = compaction::compact(
                    context.clone(),
                    &self.config.compaction_threshold,
                    self.provider.as_ref(),
                    self.storage.as_ref(),
                    thread_id,
                )
                .await?;
                *context = compacted;

                let tool_defs = self
                    .tool_dispatcher
                    .as_ref()
                    .map(|d| d.tool_definitions())
                    .unwrap_or_default();

                let retry_request = ToolCompletionRequest {
                    messages: context.clone(),
                    model: None,
                    max_tokens: None,
                    temperature: None,
                    stop_sequences: None,
                    metadata: None,
                    tool_definitions: tool_defs,
                };

                match self.provider.complete_with_tools(retry_request).await {
                    Ok(resp) => Ok(resp),
                    Err(_) => {
                        self.fail_turn_and_idle(turn_id, thread_id).await;
                        Err(AgentError::ContextTooLong)
                    }
                }
            }
        }
    }

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

    /// Create a new session within a project, with a default Idle thread.
    ///
    /// Sets both `active_session_id` and `active_thread_id` on the agent.
    /// `SessionStart` is fired by `resume_session`, not here — creation is
    /// administrative; resumption is the observable lifecycle event.
    pub async fn create_session(&mut self, project_id: Uuid) -> Result<Uuid, AgentError> {
        // 1. Create the session scoped to this agent.
        let session = self
            .storage
            .create_session(NewSession {
                project_id,
                agent_id: self.config.agent_id.clone(),
            })
            .await?;

        // 2. Create a default thread — storage assigns Idle state.
        let thread = self
            .storage
            .create(NewThread {
                session_id: session.id,
                agent_id: self.config.agent_id.clone(),
            })
            .await?;

        // 3. Activate the new session and thread.
        self.active_session_id = Some(session.id);
        self.active_thread_id = Some(thread.id);

        Ok(session.id)
    }

    /// Resume an existing session from storage.
    ///
    /// Loads the session, verifies it belongs to this agent, sets the most recent
    /// thread as active, and recovers any turns left in Pending/Processing state
    /// by a previous unclean shutdown.
    pub async fn resume_session(&mut self, session_id: Uuid) -> Result<(), AgentError> {
        // Load and validate the session.
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or(AgentError::SessionNotFound(session_id))?;

        // Enforce agent_id ownership — another agent must not resume a foreign session.
        if session.agent_id != self.config.agent_id {
            return Err(AgentError::SessionAccessDenied {
                session_id,
                agent_id: self.config.agent_id.clone(),
            });
        }

        // Load all threads; pick the most recent one as the active thread.
        let mut threads = self.storage.list_by_session(session_id).await?;
        threads.sort_by_key(|t| t.created_at);
        let active_thread_id = threads.last().map(|t| t.id);

        self.active_session_id = Some(session_id);
        self.active_thread_id = active_thread_id;

        // Shutdown recovery: any turn left in Pending or Processing from a previous
        // run is orphaned. Transition to Interrupted — the state itself signals
        // unclean shutdown; no response text is set so callers can test is_none().
        let orphaned = self
            .storage
            .find_in_progress_by_agent(&self.config.agent_id)
            .await?;

        for turn in orphaned {
            self.storage
                .update_turn_state(turn.id, TurnState::Interrupted)
                .await?;
        }

        // Fire SessionStart lifecycle hook (fire-and-forget — does not block).
        if let Some(registry) = &self.hook_registry {
            registry
                .dispatch(HookEvent::SessionStart {
                    session_id: session_id.to_string(),
                })
                .await;
        }

        Ok(())
    }

    /// Create a new thread in the active session.
    ///
    /// The new thread starts in Idle state and becomes the active thread.
    /// Returns the new thread's ID. Fails with `InvalidConfig` if no session
    /// has been set as active.
    pub async fn create_thread(&mut self) -> Result<Uuid, AgentError> {
        let session_id = self.active_session_id.ok_or_else(|| {
            AgentError::InvalidConfig(
                "no active session — call create_session or resume_session first".into(),
            )
        })?;

        let thread = ThreadStore::create(
            self.storage.as_ref(),
            NewThread {
                session_id,
                agent_id: self.config.agent_id.clone(),
            },
        )
        .await?;

        self.active_thread_id = Some(thread.id);
        Ok(thread.id)
    }

    /// Switch to an existing thread within the active session.
    ///
    /// Returns `ThreadNotFound` if the thread does not exist or belongs to a
    /// different session — callers should not be able to probe other sessions'
    /// thread IDs through error discrimination.
    pub async fn switch_thread(&mut self, thread_id: Uuid) -> Result<(), AgentError> {
        let thread = self
            .storage
            .get(thread_id)
            .await?
            .ok_or(AgentError::ThreadNotFound(thread_id))?;

        // Verify the thread belongs to the currently active session.
        // Return ThreadNotFound (not AccessDenied) to avoid leaking session state.
        if Some(thread.session_id) != self.active_session_id {
            return Err(AgentError::ThreadNotFound(thread_id));
        }

        self.active_thread_id = Some(thread.id);
        Ok(())
    }

    /// Shut down the agent, firing lifecycle hooks and clearing active state.
    ///
    /// Hook order: `SessionEnd` (if a session is active) → `AgentStop`.
    /// Both are fire-and-forget — a blocking hook or timeout does not prevent
    /// the agent from completing its shutdown.
    pub async fn shutdown(&mut self) -> Result<(), AgentError> {
        // Fire SessionEnd for the active session before clearing state.
        if let Some(session_id) = self.active_session_id {
            self.fire_session_end(&session_id.to_string());
        }

        // Always fire AgentStop.
        self.fire_agent_stop(&self.config.agent_id.clone());

        // Clear active state regardless of hook outcomes.
        self.active_session_id = None;
        self.active_thread_id = None;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Hook dispatch helpers
    // -----------------------------------------------------------------------

    /// Dispatch a `BeforeInbound` hook, returning the (possibly modified) content.
    ///
    /// When no hook registry is configured, the content passes through unchanged.
    /// A blocking hook produces `AgentError::HookBlocked`.
    pub(crate) async fn dispatch_before_inbound(
        &self,
        content: &str,
    ) -> Result<String, AgentError> {
        let registry = match &self.hook_registry {
            Some(r) => r,
            None => return Ok(content.to_owned()),
        };

        let event = HookEvent::BeforeInbound {
            content: content.to_owned(),
        };

        match registry.dispatch(event).await {
            HookOutcome::Continue => Ok(content.to_owned()),
            HookOutcome::ContinueModified(modified) => {
                // Extract the content from the modified event.
                if let HookEvent::BeforeInbound { content: new_content } = *modified {
                    Ok(new_content)
                } else {
                    // Hook returned a different event variant — treat as unmodified.
                    Ok(content.to_owned())
                }
            }
            HookOutcome::Block(reason) => Err(AgentError::HookBlocked {
                hook: "BeforeInbound".into(),
                reason,
            }),
        }
    }

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
                if let HookEvent::AfterResponse { content: new_content } = *modified {
                    Ok(new_content)
                } else {
                    Ok(content.to_owned())
                }
            }
            HookOutcome::Block(reason) => Err(AgentError::HookBlocked {
                hook: "AfterResponse".into(),
                reason,
            }),
        }
    }

    // -----------------------------------------------------------------------
    // Leak detection
    // -----------------------------------------------------------------------

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
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            detector.scan(&text_owned)
        })) {
            Ok(redacted) => redacted,
            Err(_) => {
                tracing::warn!("leak detector panicked during scan — returning original text");
                text.to_owned()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Hook dispatch unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod hook_inbound_response_tests {
    use super::*;
    use std::pin::Pin;

    use async_trait::async_trait;
    use futures::Stream;
    use swage_hook::{Hook, HookRegistration};

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;

    // -----------------------------------------------------------------------
    // Minimal test doubles
    // -----------------------------------------------------------------------

    /// Stub provider — not exercised by hook dispatch tests.
    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn model_id(&self) -> &str {
            "stub"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            Ok(ToolCompletionResponse {
                content: String::new(),
                tool_calls: vec![],
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    /// Stub storage — not exercised by hook dispatch tests.
    /// Uses SqliteStorage with an in-memory database.
    fn stub_storage() -> Arc<dyn Storage> {
        use std::path::Path;
        let storage = swage_storage::sqlite::SqliteStorage::open(Path::new(":memory:"))
            .expect("in-memory SQLite should open");
        Arc::new(storage) as Arc<dyn Storage>
    }

    /// Build a minimal Agent for hook dispatch testing.
    fn test_agent(hook_registry: Option<Arc<HookRegistry>>) -> Agent {
        Agent {
            config: AgentConfig::new("test-agent"),
            provider: Arc::new(StubProvider),
            storage: stub_storage(),
            tool_dispatcher: None,
            hook_registry,
            leak_detector: None,
            active_session_id: None,
            active_thread_id: None,
        }
    }

    // -----------------------------------------------------------------------
    // Hook test doubles
    // -----------------------------------------------------------------------

    /// A hook that always continues without modification.
    struct ContinueHook;

    #[async_trait]
    impl Hook for ContinueHook {
        fn name(&self) -> &str {
            "continue"
        }

        async fn execute(&self, _event: &HookEvent) -> HookOutcome {
            HookOutcome::Continue
        }
    }

    /// A hook that blocks with the given reason.
    struct BlockingHook {
        reason: String,
    }

    #[async_trait]
    impl Hook for BlockingHook {
        fn name(&self) -> &str {
            "blocker"
        }

        async fn execute(&self, _event: &HookEvent) -> HookOutcome {
            HookOutcome::Block(self.reason.clone())
        }
    }

    /// A hook that modifies the content by appending a suffix.
    struct ModifyingHook {
        suffix: String,
    }

    #[async_trait]
    impl Hook for ModifyingHook {
        fn name(&self) -> &str {
            "modifier"
        }

        async fn execute(&self, event: &HookEvent) -> HookOutcome {
            match event {
                HookEvent::BeforeInbound { content } => {
                    HookOutcome::ContinueModified(Box::new(HookEvent::BeforeInbound {
                        content: format!("{}{}", content, self.suffix),
                    }))
                }
                HookEvent::AfterResponse { content } => {
                    HookOutcome::ContinueModified(Box::new(HookEvent::AfterResponse {
                        content: format!("{}{}", content, self.suffix),
                    }))
                }
                _ => HookOutcome::Continue,
            }
        }
    }

    // -----------------------------------------------------------------------
    // dispatch_before_inbound
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn before_inbound_no_registry_passes_through() {
        let agent = test_agent(None);
        let result = agent.dispatch_before_inbound("hello").await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn before_inbound_continue_returns_original() {
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(ContinueHook)));
        let agent = test_agent(Some(Arc::new(registry)));

        let result = agent.dispatch_before_inbound("original content").await.unwrap();
        assert_eq!(result, "original content");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn before_inbound_block_returns_hook_blocked() {
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(BlockingHook {
            reason: "content policy".into(),
        })));
        let agent = test_agent(Some(Arc::new(registry)));

        let err = agent.dispatch_before_inbound("bad content").await.unwrap_err();
        match err {
            AgentError::HookBlocked { hook, reason } => {
                assert_eq!(hook, "BeforeInbound");
                assert_eq!(reason, "content policy");
            }
            other => panic!("expected HookBlocked, got: {other}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn before_inbound_modified_returns_new_content() {
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(ModifyingHook {
            suffix: " [sanitised]".into(),
        })));
        let agent = test_agent(Some(Arc::new(registry)));

        let result = agent.dispatch_before_inbound("user input").await.unwrap();
        assert_eq!(result, "user input [sanitised]");
    }

    // -----------------------------------------------------------------------
    // dispatch_after_response
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn after_response_no_registry_passes_through() {
        let agent = test_agent(None);
        let result = agent.dispatch_after_response("response text").await.unwrap();
        assert_eq!(result, "response text");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_response_continue_returns_original() {
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(ContinueHook)));
        let agent = test_agent(Some(Arc::new(registry)));

        let result = agent.dispatch_after_response("model output").await.unwrap();
        assert_eq!(result, "model output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_response_block_returns_hook_blocked() {
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(BlockingHook {
            reason: "safety filter".into(),
        })));
        let agent = test_agent(Some(Arc::new(registry)));

        let err = agent.dispatch_after_response("unsafe output").await.unwrap_err();
        match err {
            AgentError::HookBlocked { hook, reason } => {
                assert_eq!(hook, "AfterResponse");
                assert_eq!(reason, "safety filter");
            }
            other => panic!("expected HookBlocked, got: {other}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_response_modified_returns_new_content() {
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(ModifyingHook {
            suffix: " [redacted]".into(),
        })));
        let agent = test_agent(Some(Arc::new(registry)));

        let result = agent.dispatch_after_response("raw output").await.unwrap();
        assert_eq!(result, "raw output [redacted]");
    }

    // -----------------------------------------------------------------------
    // scan_output — leak detection
    // -----------------------------------------------------------------------

    /// Build an Agent with a leak detector configured.
    fn test_agent_with_detector() -> Agent {
        let detector =
            LeakDetector::new(vec![]).expect("default patterns should compile");
        Agent {
            config: AgentConfig::new("test-agent"),
            provider: Arc::new(StubProvider),
            storage: stub_storage(),
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: Some(Arc::new(detector)),
            active_session_id: None,
            active_thread_id: None,
        }
    }

    #[test]
    fn scan_output_no_detector_passes_through() {
        let agent = test_agent(None);
        let result = agent.scan_output("sk-test1234567890abcdef");
        // Without a detector, no redaction occurs.
        assert_eq!(result, "sk-test1234567890abcdef");
    }

    #[test]
    fn scan_output_clean_text_unchanged() {
        let agent = test_agent_with_detector();
        let result = agent.scan_output("just a normal response");
        assert_eq!(result, "just a normal response");
    }

    #[test]
    fn scan_output_redacts_api_key() {
        let agent = test_agent_with_detector();
        // The default LeakDetector patterns match common API key formats.
        let input = "The key is sk-proj-abc123def456ghi789jklmnopqrstuvwxyz1234567890ABCDEFGHIJ";
        let result = agent.scan_output(input);
        // The redacted output should not contain the original key.
        assert_ne!(result, input, "expected redaction to modify the text");
        assert!(
            result.contains("[REDACTED"),
            "expected a [REDACTED...] placeholder in: {result}"
        );
    }

    #[test]
    fn scan_output_empty_text_unchanged() {
        let agent = test_agent_with_detector();
        assert_eq!(agent.scan_output(""), "");
    }
}

// ---------------------------------------------------------------------------
// Unit tests for PreToolUse / PostToolUse hook dispatch
// ---------------------------------------------------------------------------

#[cfg(test)]
mod hook_dispatch_tests {
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::Stream;
    use serde_json::json;

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
    use swage_provider::LlmProvider;
    use swage_storage::sqlite::SqliteStorage;

    use crate::config::AgentConfig;
    use crate::error::AgentError;

    use super::Agent;

    // Minimal LLM provider that is never called in these tests.
    struct NullProvider;

    #[async_trait]
    impl LlmProvider for NullProvider {
        fn model_id(&self) -> &str {
            "null"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _req: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            Ok(ToolCompletionResponse {
                content: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
                tool_calls: vec![],
            })
        }

        fn stream(
            &self,
            _req: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    /// Build a bare Agent with only the hook registry wired in.
    fn agent_with_registry(registry: Option<HookRegistry>) -> Agent {
        // SqliteStorage::in_memory() needs multi-thread runtime — tests use that flavor.
        let storage = Arc::new(
            SqliteStorage::in_memory().expect("in-memory storage for hook dispatch tests"),
        );
        Agent {
            config: AgentConfig::new("test-agent"),
            provider: Arc::new(NullProvider),
            storage,
            tool_dispatcher: None,
            hook_registry: registry.map(|r| Arc::new(r)),
            leak_detector: None,
            active_session_id: None,
            active_thread_id: None,
        }
    }

    // A hook that records which events it saw.
    struct RecordingHook(Arc<Mutex<Vec<String>>>);

    #[async_trait]
    impl Hook for RecordingHook {
        fn name(&self) -> &str {
            "recorder"
        }

        async fn execute(&self, event: &HookEvent) -> HookOutcome {
            self.0.lock().unwrap().push(event.event_name().to_owned());
            HookOutcome::Continue
        }
    }

    // A hook that blocks PreToolUse unconditionally.
    struct BlockPreToolUse;

    #[async_trait]
    impl Hook for BlockPreToolUse {
        fn name(&self) -> &str {
            "block-pre-tool"
        }

        async fn execute(&self, event: &HookEvent) -> HookOutcome {
            if matches!(event, HookEvent::PreToolUse { .. }) {
                HookOutcome::Block("tool not permitted".into())
            } else {
                HookOutcome::Continue
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pre_tool_use_no_registry_returns_ok() {
        let agent = agent_with_registry(None);
        let result = agent
            .dispatch_pre_tool_use("my_tool", &json!({"x": 1}))
            .await;
        assert!(result.is_ok(), "no registry should be a no-op returning Ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pre_tool_use_hook_blocks_returns_hook_blocked_error() {
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(BlockPreToolUse)));

        let agent = agent_with_registry(Some(registry));
        let result = agent
            .dispatch_pre_tool_use("restricted_tool", &json!({}))
            .await;

        assert!(
            matches!(result, Err(AgentError::HookBlocked { ref hook, .. }) if hook == "PreToolUse"),
            "blocking hook should produce HookBlocked error, got {:?}",
            result,
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn post_tool_use_fires_without_blocking() {
        // PostToolUse is informational — even with a recording hook, the call
        // completes normally and we verify the event was dispatched.
        let fired = Arc::new(Mutex::new(Vec::new()));
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(RecordingHook(fired.clone()))));

        let agent = agent_with_registry(Some(registry));
        agent
            .dispatch_post_tool_use("echo", &json!({"text": "hi"}), &json!({"result": "hi"}))
            .await;

        let events = fired.lock().unwrap().clone();
        assert_eq!(events, vec!["PostToolUse"], "PostToolUse hook should have fired");
    }
}

#[cfg(test)]
mod session_tests {
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;

    use swage_core::{
        CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_storage::store::{ProjectStore, SessionStore, ThreadStore};
    use swage_storage::{NewThread, SqliteStorage, ThreadState};
    use swage_storage::model::NewProject;
    use uuid::Uuid;

    use super::*;
    use crate::builder::AgentBuilder;
    use crate::config::{AgentConfig, CompactionThreshold};

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn model_id(&self) -> &str {
            "stub"
        }
        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("stub: complete not used in session tests")
        }
        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("stub: complete_with_tools not used in session tests")
        }
        fn stream(
            &self,
            _: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unimplemented!("stub: stream not used in session tests")
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Create an in-memory database, insert a project, return both.
    async fn setup_storage() -> (Arc<SqliteStorage>, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory storage"));
        let project = ProjectStore::create(
            storage.as_ref(),
            NewProject {
                name: "test-project".into(),
                slug: "test-project".into(),
                workspace_path: None,
            },
        )
        .await
        .expect("create test project");
        (storage, project.id)
    }

    fn make_agent(storage: Arc<SqliteStorage>) -> Agent {
        AgentBuilder::new()
            .config(AgentConfig {
                agent_id: "test-agent".into(),
                max_tool_rounds: 10,
                compaction_threshold: CompactionThreshold::MessageCount(100),
                metadata: HashMap::new(),
            })
            .provider(Arc::new(StubProvider))
            .storage(storage)
            .build()
            .expect("build agent")
    }

    // -----------------------------------------------------------------------
    // create_session tests [mmlb]
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_stores_session_with_correct_agent_id() {
        let (storage, project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());

        let session_id = agent.create_session(project_id).await.expect("create_session");

        let session = SessionStore::get_session(storage.as_ref(), session_id)
            .await
            .expect("get_session")
            .expect("session should exist");
        assert_eq!(session.agent_id, "test-agent");
        assert_eq!(session.project_id, project_id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_creates_default_thread_in_idle_state() {
        let (storage, project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());

        let session_id = agent.create_session(project_id).await.expect("create_session");

        let threads = ThreadStore::list_by_session(storage.as_ref(), session_id)
            .await
            .expect("list_by_session");
        assert_eq!(threads.len(), 1, "exactly one default thread should be created");
        assert_eq!(threads[0].state, ThreadState::Idle);
        assert_eq!(threads[0].agent_id, "test-agent");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_sets_active_session_and_thread() {
        let (storage, project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());

        let session_id = agent.create_session(project_id).await.expect("create_session");

        assert_eq!(agent.active_session_id, Some(session_id));

        // Verify the active_thread_id matches the thread that was created.
        let threads = ThreadStore::list_by_session(storage.as_ref(), session_id)
            .await
            .expect("list_by_session");
        assert_eq!(agent.active_thread_id, Some(threads[0].id));
    }

    // -----------------------------------------------------------------------
    // switch_thread tests [16t4]
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn switch_thread_updates_active_thread_id() {
        let (storage, project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());

        // Set up a session with a default thread, then create a second thread.
        let session_id = agent.create_session(project_id).await.expect("create_session");
        let second_thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id,
                agent_id: "test-agent".into(),
            },
        )
        .await
        .expect("create second thread");

        agent.switch_thread(second_thread.id).await.expect("switch_thread");

        assert_eq!(agent.active_thread_id, Some(second_thread.id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn switch_thread_returns_not_found_for_nonexistent_thread() {
        let (storage, project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());
        agent.create_session(project_id).await.expect("create_session");

        let bogus_id = Uuid::new_v4();
        let err = agent.switch_thread(bogus_id).await.expect_err("should fail");

        assert!(
            matches!(err, AgentError::ThreadNotFound(id) if id == bogus_id),
            "expected ThreadNotFound({bogus_id}), got {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn switch_thread_returns_not_found_for_thread_in_different_session() {
        let (storage, project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());

        // Create two sessions; each gets a default thread.
        agent.create_session(project_id).await.expect("create first session");
        let second_session = SessionStore::create_session(
            storage.as_ref(),
            swage_storage::NewSession {
                project_id,
                agent_id: "test-agent".into(),
            },
        )
        .await
        .expect("create second session");
        let thread_in_other_session = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: second_session.id,
                agent_id: "test-agent".into(),
            },
        )
        .await
        .expect("create thread in second session");

        // The agent's active session is the first one; this thread belongs to the second.
        let err = agent
            .switch_thread(thread_in_other_session.id)
            .await
            .expect_err("should fail");

        assert!(
            matches!(err, AgentError::ThreadNotFound(_)),
            "expected ThreadNotFound, got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // shutdown tests [z1gy]
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_clears_active_session_and_thread() {
        let (storage, project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());
        agent.create_session(project_id).await.expect("create_session");

        // Both should be set after session creation.
        assert!(agent.active_session_id.is_some());
        assert!(agent.active_thread_id.is_some());

        agent.shutdown().await.expect("shutdown");

        assert!(agent.active_session_id.is_none(), "active_session_id should be cleared");
        assert!(agent.active_thread_id.is_none(), "active_thread_id should be cleared");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_without_active_session_clears_state() {
        let (storage, _project_id) = setup_storage().await;
        let mut agent = make_agent(storage.clone());

        // No session created — shutdown should still succeed.
        agent.shutdown().await.expect("shutdown with no active session");

        assert!(agent.active_session_id.is_none());
        assert!(agent.active_thread_id.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_with_session_fires_session_end_and_agent_stop() {
        use std::sync::{Arc as StdArc, Mutex};
        use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};

        // A recording hook that captures event names.
        struct Recorder(StdArc<Mutex<Vec<String>>>);

        #[async_trait::async_trait]
        impl Hook for Recorder {
            fn name(&self) -> &str {
                "recorder"
            }
            async fn execute(&self, event: &HookEvent) -> HookOutcome {
                self.0.lock().unwrap().push(event.event_name().to_owned());
                HookOutcome::Continue
            }
        }

        let fired = StdArc::new(Mutex::new(Vec::<String>::new()));
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(StdArc::new(Recorder(fired.clone()))));
        let registry = Arc::new(registry);

        let (storage, project_id) = setup_storage().await;
        let mut agent = AgentBuilder::new()
            .config(AgentConfig {
                agent_id: "test-agent".into(),
                max_tool_rounds: 10,
                compaction_threshold: CompactionThreshold::MessageCount(100),
                metadata: HashMap::new(),
            })
            .provider(Arc::new(StubProvider))
            .storage(storage)
            .hook_registry(registry)
            .build()
            .expect("build agent");

        agent.create_session(project_id).await.expect("create_session");
        agent.shutdown().await.expect("shutdown");

        // Give the spawned hook tasks a moment to complete.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = fired.lock().unwrap().clone();
        // SessionEnd + AgentStop fire during shutdown (SessionStart fires on resume, not create).
        assert!(events.contains(&"SessionEnd".to_owned()), "SessionEnd not fired: {events:?}");
        assert!(events.contains(&"AgentStop".to_owned()), "AgentStop not fired: {events:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_without_session_fires_only_agent_stop() {
        use std::sync::{Arc as StdArc, Mutex};
        use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};

        struct Recorder(StdArc<Mutex<Vec<String>>>);

        #[async_trait::async_trait]
        impl Hook for Recorder {
            fn name(&self) -> &str {
                "recorder"
            }
            async fn execute(&self, event: &HookEvent) -> HookOutcome {
                self.0.lock().unwrap().push(event.event_name().to_owned());
                HookOutcome::Continue
            }
        }

        let fired = StdArc::new(Mutex::new(Vec::<String>::new()));
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(StdArc::new(Recorder(fired.clone()))));
        let registry = Arc::new(registry);

        let (storage, _project_id) = setup_storage().await;
        let mut agent = AgentBuilder::new()
            .config(AgentConfig {
                agent_id: "test-agent".into(),
                max_tool_rounds: 10,
                compaction_threshold: CompactionThreshold::MessageCount(100),
                metadata: HashMap::new(),
            })
            .provider(Arc::new(StubProvider))
            .storage(storage)
            .hook_registry(registry)
            .build()
            .expect("build agent");

        // No active session — shutdown should fire AgentStop but not SessionEnd.
        agent.shutdown().await.expect("shutdown");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = fired.lock().unwrap().clone();
        assert!(!events.contains(&"SessionEnd".to_owned()), "SessionEnd should not fire: {events:?}");
        assert!(events.contains(&"AgentStop".to_owned()), "AgentStop not fired: {events:?}");
    }

    // -----------------------------------------------------------------------
    // process_message tests [iwpu]
    // -----------------------------------------------------------------------

    /// A mock provider that returns a canned text response.
    struct TextProvider {
        response: String,
        input_tokens: u32,
        output_tokens: u32,
    }

    impl TextProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.into(),
                input_tokens: 10,
                output_tokens: 20,
            }
        }
    }

    #[async_trait]
    impl LlmProvider for TextProvider {
        fn model_id(&self) -> &str {
            "text-mock"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: self.response.clone(),
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                finish_reason: swage_core::FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            Ok(swage_core::ToolCompletionResponse {
                content: self.response.clone(),
                tool_calls: vec![],
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                finish_reason: swage_core::FinishReason::Stop,
                cost: None,
            })
        }

        fn stream(
            &self,
            _: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    fn make_agent_with_provider(
        storage: Arc<SqliteStorage>,
        provider: Arc<dyn LlmProvider>,
    ) -> Agent {
        AgentBuilder::new()
            .config(AgentConfig {
                agent_id: "test-agent".into(),
                max_tool_rounds: 10,
                compaction_threshold: CompactionThreshold::MessageCount(100),
                metadata: HashMap::new(),
            })
            .provider(provider)
            .storage(storage)
            .build()
            .expect("build agent")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn process_message_returns_response_with_correct_content() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(TextProvider::new("Hello from the LLM!"));
        let mut agent = make_agent_with_provider(storage, provider);

        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let result = agent
            .process_message(InboundMessage {
                message: "Hi there".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message");

        assert_eq!(result.response, "Hello from the LLM!");
        assert_eq!(result.input_tokens, 10);
        assert_eq!(result.output_tokens, 20);
        assert_eq!(result.tool_calls_count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn process_message_persists_turn_in_completed_state() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(TextProvider::new("Done."));
        let mut agent = make_agent_with_provider(storage.clone(), provider);

        agent.create_session(project_id).await.expect("create session");
        let session_id = agent.active_session_id.unwrap();

        let result = agent
            .process_message(InboundMessage {
                message: "Do something".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message");

        // Verify turn state
        use swage_storage::TurnStore;
        let turn = TurnStore::get_turn(storage.as_ref(), result.turn_id)
            .await
            .expect("get_turn")
            .expect("turn should exist");

        assert_eq!(turn.state, swage_storage::TurnState::Completed);
        assert_eq!(turn.assistant_response.as_deref(), Some("Done."));
        assert_eq!(turn.input_tokens, Some(10));
        assert_eq!(turn.output_tokens, Some(20));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn process_message_persists_user_and_assistant_messages() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(TextProvider::new("Response text"));
        let mut agent = make_agent_with_provider(storage.clone(), provider);

        agent.create_session(project_id).await.expect("create session");
        let session_id = agent.active_session_id.unwrap();
        let thread_id = agent.active_thread_id.unwrap();

        agent
            .process_message(InboundMessage {
                message: "User message".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message");

        // Verify messages in thread history
        use swage_storage::MessageStore;
        let messages = MessageStore::list_messages_by_thread(storage.as_ref(), thread_id)
            .await
            .expect("list messages");

        assert_eq!(messages.len(), 2, "should have user + assistant messages");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "User message");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "Response text");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn process_message_thread_transitions_idle_to_processing_to_idle() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(TextProvider::new("ok"));
        let mut agent = make_agent_with_provider(storage.clone(), provider);

        agent.create_session(project_id).await.expect("create session");
        let session_id = agent.active_session_id.unwrap();
        let thread_id = agent.active_thread_id.unwrap();

        // Before processing, thread should be Idle.
        let thread_before = ThreadStore::get(storage.as_ref(), thread_id)
            .await
            .expect("get thread")
            .expect("thread exists");
        assert_eq!(thread_before.state, ThreadState::Idle);

        agent
            .process_message(InboundMessage {
                message: "check thread state".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message");

        // After processing, thread should be back to Idle.
        let thread_after = ThreadStore::get(storage.as_ref(), thread_id)
            .await
            .expect("get thread")
            .expect("thread exists");
        assert_eq!(thread_after.state, ThreadState::Idle);
    }
}

// ---------------------------------------------------------------------------
// resume_session tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod resume_session_tests {
    use super::*;
    use std::pin::Pin;

    use async_trait::async_trait;
    use futures::Stream;

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_storage::{
        NewProject, NewSession, NewThread, NewTurn, ProjectStore, SessionStore, ThreadStore,
        TurnState, TurnStore, sqlite::SqliteStorage,
    };

    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn model_id(&self) -> &str { "stub" }
        async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse { content: String::new(), input_tokens: 0, output_tokens: 0,
                finish_reason: FinishReason::Stop, cost: None })
        }
        async fn complete_with_tools(&self, _: ToolCompletionRequest) -> Result<ToolCompletionResponse, ProviderError> {
            Ok(ToolCompletionResponse { content: String::new(), tool_calls: vec![], input_tokens: 0,
                output_tokens: 0, finish_reason: FinishReason::Stop, cost: None })
        }
        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    fn test_storage() -> Arc<SqliteStorage> {
        Arc::new(SqliteStorage::in_memory().expect("in-memory storage"))
    }

    fn test_agent_with_storage(agent_id: &str, storage: Arc<SqliteStorage>) -> Agent {
        Agent {
            config: AgentConfig::new(agent_id),
            provider: Arc::new(StubProvider),
            storage: storage as Arc<dyn swage_storage::Storage>,
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: None,
            active_session_id: None,
            active_thread_id: None,
        }
    }

    /// Create a session and a default thread for it, returning the session_id.
    async fn make_session(storage: &Arc<SqliteStorage>, agent_id: &str) -> uuid::Uuid {
        // Must create a project first (foreign key constraint).
        let project = ProjectStore::create(storage.as_ref(), NewProject {
            name: "test-project".into(),
            slug: format!("test-{}", uuid::Uuid::new_v4()),
            workspace_path: None,
        }).await.expect("create project");

        let session = SessionStore::create_session(storage.as_ref(), NewSession {
            project_id: project.id,
            agent_id: agent_id.into(),
        }).await.expect("create session");

        ThreadStore::create(storage.as_ref(), NewThread {
            session_id: session.id,
            agent_id: agent_id.into(),
        }).await.expect("create thread");

        session.id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_existing_session_sets_active_ids() {
        let storage = test_storage();
        let session_id = make_session(&storage, "agent-x").await;
        let mut agent = test_agent_with_storage("agent-x", storage);

        agent.resume_session(session_id).await.expect("resume should succeed");

        assert_eq!(agent.active_session_id, Some(session_id));
        assert!(agent.active_thread_id.is_some(), "active_thread_id should be set");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_nonexistent_session_returns_not_found() {
        let storage = test_storage();
        let mut agent = test_agent_with_storage("agent-x", storage);
        let missing_id = uuid::Uuid::new_v4();

        let err = agent.resume_session(missing_id).await.unwrap_err();
        assert!(matches!(err, AgentError::SessionNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_session_wrong_agent_returns_access_denied() {
        let storage = test_storage();
        // Session belongs to "agent-a"
        let session_id = make_session(&storage, "agent-a").await;
        // But we resume with "agent-b"
        let mut agent = test_agent_with_storage("agent-b", storage);

        let err = agent.resume_session(session_id).await.unwrap_err();
        assert!(matches!(err, AgentError::SessionAccessDenied { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_with_orphaned_turn_transitions_to_interrupted() {
        let storage = test_storage();
        let session_id = make_session(&storage, "agent-r").await;

        // Get the thread created by make_session
        let threads = ThreadStore::list_by_session(storage.as_ref(), session_id)
            .await.expect("list threads");
        let thread_id = threads[0].id;

        // Create an orphaned Processing turn for this agent
        let orphaned = TurnStore::create_turn(storage.as_ref(), NewTurn {
            thread_id,
            agent_id: "agent-r".into(),
            state: TurnState::Processing,
        }).await.expect("create orphaned turn");

        let mut agent = test_agent_with_storage("agent-r", storage.clone());
        agent.resume_session(session_id).await.expect("resume should succeed");

        // Verify the orphaned turn is now Interrupted
        let turn = TurnStore::get_turn(storage.as_ref(), orphaned.id)
            .await.expect("get turn").expect("turn exists");
        assert_eq!(turn.state, TurnState::Interrupted, "orphaned turn should be Interrupted");
    }
}

// ---------------------------------------------------------------------------
// Tests for lifecycle hook dispatch (SessionStart, SessionEnd, AgentStop,
// PreCompact) and agent shutdown.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod lifecycle_hook_tests {
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::Stream;

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
    use swage_provider::LlmProvider;
    use swage_storage::sqlite::SqliteStorage;
    use swage_storage::store::ProjectStore;
    use swage_storage::model::NewProject;

    use crate::builder::AgentBuilder;
    use crate::config::{AgentConfig, CompactionThreshold};

    use super::Agent;

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn model_id(&self) -> &str { "stub" }
        async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }
        async fn complete_with_tools(&self, _: ToolCompletionRequest) -> Result<ToolCompletionResponse, ProviderError> {
            Ok(ToolCompletionResponse {
                content: String::new(),
                tool_calls: vec![],
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }
        fn stream(&self, _: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    /// Records the name of every event it receives.
    struct RecordingHook(Arc<Mutex<Vec<String>>>);

    #[async_trait]
    impl Hook for RecordingHook {
        fn name(&self) -> &str { "recorder" }
        async fn execute(&self, event: &HookEvent) -> HookOutcome {
            self.0.lock().unwrap().push(event.event_name().to_owned());
            HookOutcome::Continue
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    async fn setup() -> (Arc<SqliteStorage>, uuid::Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory storage"));
        let project = ProjectStore::create(
            storage.as_ref(),
            NewProject {
                name: "test".into(),
                slug: "test".into(),
                workspace_path: None,
            },
        )
        .await
        .expect("create project");
        (storage, project.id)
    }

    fn make_agent(storage: Arc<SqliteStorage>, registry: Option<HookRegistry>) -> Agent {
        AgentBuilder::new()
            .config(AgentConfig {
                agent_id: "test-agent".into(),
                max_tool_rounds: 10,
                compaction_threshold: CompactionThreshold::MessageCount(100),
                metadata: HashMap::new(),
            })
            .provider(Arc::new(StubProvider))
            .storage(storage)
            .build()
            .map(|mut agent| {
                if let Some(r) = registry {
                    agent.hook_registry = Some(Arc::new(r));
                }
                agent
            })
            .expect("build agent")
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_does_not_fire_session_start_hook() {
        // SessionStart is a resume-only lifecycle event — create_session is
        // administrative and must not fire it. If it did, a test that calls
        // create_session and then process_message would see SessionStart race
        // into the hook log and corrupt ordered-event assertions.
        let (storage, project_id) = setup().await;

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(RecordingHook(events.clone()))));

        let mut agent = make_agent(storage, Some(registry));
        agent.create_session(project_id).await.expect("create_session");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let recorded = events.lock().unwrap().clone();
        assert!(
            !recorded.contains(&"SessionStart".to_owned()),
            "SessionStart must not fire from create_session, got: {recorded:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_fires_session_end_and_agent_stop_hooks() {
        let (storage, project_id) = setup().await;

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let mut registry = HookRegistry::new();
        registry.register(HookRegistration::new(Arc::new(RecordingHook(events.clone()))));

        let mut agent = make_agent(storage, Some(registry));
        agent.create_session(project_id).await.expect("create_session");
        agent.shutdown().await.expect("shutdown");

        // shutdown fires hooks via tokio::spawn — give tasks time to complete.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let recorded = events.lock().unwrap().clone();
        assert!(
            recorded.contains(&"SessionEnd".to_owned()),
            "expected SessionEnd event, got: {recorded:?}"
        );
        assert!(
            recorded.contains(&"AgentStop".to_owned()),
            "expected AgentStop event, got: {recorded:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_hook_registry_shutdown_succeeds_without_error() {
        let (storage, project_id) = setup().await;
        // Agent without a hook registry — lifecycle hooks should be no-ops.
        let mut agent = make_agent(storage, None);
        agent.create_session(project_id).await.expect("create_session");
        agent.shutdown().await.expect("shutdown with no registry should not error");
        assert!(agent.active_session_id.is_none(), "active session should be cleared after shutdown");
        assert!(agent.active_thread_id.is_none(), "active thread should be cleared after shutdown");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fire_pre_compact_returns_continue_when_no_registry() {
        let (storage, _) = setup().await;
        let agent = make_agent(storage, None);
        let outcome = agent.fire_pre_compact(&serde_json::json!({"key": "value"})).await;
        assert!(
            matches!(outcome, HookOutcome::Continue),
            "expected Continue when no registry configured"
        );
    }
}

// ---------------------------------------------------------------------------
// Unit tests for create_thread
// ---------------------------------------------------------------------------

#[cfg(test)]
mod create_thread_tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_storage::{
        sqlite::SqliteStorage, NewSession, ProjectStore, SessionStore, ThreadStore,
    };

    use crate::config::AgentConfig;
    use crate::error::AgentError;

    use super::Agent;

    struct NullProvider;

    #[async_trait]
    impl LlmProvider for NullProvider {
        fn model_id(&self) -> &str {
            "null"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _req: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            Ok(ToolCompletionResponse {
                content: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                finish_reason: FinishReason::Stop,
                cost: None,
                tool_calls: vec![],
            })
        }

        fn stream(
            &self,
            _req: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    /// Creates storage, a project, and a session; returns an Agent with
    /// active_session_id set but no active_thread_id.
    async fn agent_with_session() -> (Agent, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory storage"));
        let project = ProjectStore::get_or_create_default(&*storage)
            .await
            .expect("default project");
        let session = SessionStore::create_session(
            &*storage,
            NewSession {
                project_id: project.id,
                agent_id: "test-agent".into(),
            },
        )
        .await
        .expect("session");
        let agent = Agent {
            config: AgentConfig::new("test-agent"),
            provider: Arc::new(NullProvider),
            storage: storage.clone(),
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: None,
            active_session_id: Some(session.id),
            active_thread_id: None,
        };
        (agent, storage)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_thread_returns_id_and_sets_active_thread() {
        let (mut agent, _storage) = agent_with_session().await;
        assert!(
            agent.active_thread_id.is_none(),
            "active_thread_id should start as None"
        );

        let thread_id = agent.create_thread().await.expect("create_thread");

        assert_eq!(
            agent.active_thread_id,
            Some(thread_id),
            "active_thread_id should be set to the new thread"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_thread_exists_in_storage_under_active_session() {
        let (mut agent, storage) = agent_with_session().await;
        let session_id = agent.active_session_id.unwrap();

        let thread_id = agent.create_thread().await.expect("create_thread");

        let threads = ThreadStore::list_by_session(&*storage, session_id)
            .await
            .expect("list threads");
        assert_eq!(threads.len(), 1, "one thread should exist in the session");
        assert_eq!(threads[0].id, thread_id, "stored thread ID should match");
        assert_eq!(
            threads[0].session_id, session_id,
            "thread must belong to the active session"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_thread_without_active_session_returns_error() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let mut agent = Agent {
            config: AgentConfig::new("test-agent"),
            provider: Arc::new(NullProvider),
            storage,
            tool_dispatcher: None,
            hook_registry: None,
            leak_detector: None,
            active_session_id: None, // intentionally absent
            active_thread_id: None,
        };

        let result = agent.create_thread().await;
        assert!(
            matches!(result, Err(AgentError::InvalidConfig(_))),
            "should return InvalidConfig when no active session, got {:?}",
            result,
        );
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch cycle tests [15v6]
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tool_dispatch_cycle_tests {
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;
    use serde_json::{json, Value};

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCall, ToolCompletionRequest, ToolCompletionResponse, ToolError,
    };
    use swage_provider::LlmProvider;
    use swage_storage::model::NewProject;
    use swage_storage::store::ProjectStore;
    use swage_storage::{SqliteStorage, ToolCallStore};
    use swage_tool::context::ToolContext;
    use swage_tool::dispatch::ToolDispatcher;
    use swage_tool::tool::Tool;
    use uuid::Uuid;

    use super::*;
    use crate::builder::AgentBuilder;
    use crate::config::{AgentConfig, CompactionThreshold};

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    /// A tool that returns a canned JSON value.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes its input."
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        async fn execute(
            &self,
            params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            Ok(json!({ "echoed": params["text"] }))
        }
    }

    /// A tool that always fails with an execution error.
    struct FailingTool;

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "failing_tool"
        }
        fn description(&self) -> &str {
            "Always fails."
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        async fn execute(
            &self,
            _params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            Err(ToolError::ExecutionFailed {
                name: "failing_tool".into(),
                message: "something went wrong".into(),
            })
        }
    }

    /// Mock provider: first complete_with_tools returns tool calls,
    /// second returns text.
    struct ToolCallProvider {
        call_count: AtomicUsize,
        tool_calls: Vec<ToolCall>,
        final_response: String,
    }

    impl ToolCallProvider {
        fn new(tool_calls: Vec<ToolCall>, final_response: &str) -> Self {
            Self {
                call_count: AtomicUsize::new(0),
                tool_calls,
                final_response: final_response.into(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for ToolCallProvider {
        fn model_id(&self) -> &str {
            "tool-mock"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: self.final_response.clone(),
                input_tokens: 10,
                output_tokens: 20,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(ToolCompletionResponse {
                    content: String::new(),
                    tool_calls: self.tool_calls.clone(),
                    input_tokens: 5,
                    output_tokens: 15,
                    finish_reason: FinishReason::ToolUse,
                    cost: None,
                })
            } else {
                Ok(ToolCompletionResponse {
                    content: self.final_response.clone(),
                    tool_calls: vec![],
                    input_tokens: 10,
                    output_tokens: 20,
                    finish_reason: FinishReason::Stop,
                    cost: None,
                })
            }
        }

        fn stream(
            &self,
            _: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    async fn setup_storage() -> (Arc<SqliteStorage>, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory storage"));
        let project = ProjectStore::create(
            storage.as_ref(),
            NewProject {
                name: "test-project".into(),
                slug: "test-project".into(),
                workspace_path: None,
            },
        )
        .await
        .expect("create test project");
        (storage, project.id)
    }

    fn echo_dispatcher() -> ToolDispatcher {
        ToolDispatcher::allow_all(vec![Arc::new(EchoTool) as Arc<dyn Tool>])
    }

    fn failing_dispatcher() -> ToolDispatcher {
        ToolDispatcher::allow_all(vec![Arc::new(FailingTool) as Arc<dyn Tool>])
    }

    fn make_agent_with_tools(
        storage: Arc<SqliteStorage>,
        provider: Arc<dyn LlmProvider>,
        dispatcher: ToolDispatcher,
    ) -> Agent {
        AgentBuilder::new()
            .config(AgentConfig {
                agent_id: "test-agent".into(),
                max_tool_rounds: 10,
                compaction_threshold: CompactionThreshold::MessageCount(100),
                metadata: HashMap::new(),
            })
            .provider(provider)
            .storage(storage)
            .tool_dispatcher(dispatcher)
            .build()
            .expect("build agent")
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_call_executes_and_returns_final_response() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(ToolCallProvider::new(
            vec![ToolCall {
                call_id: "call-1".into(),
                name: "echo".into(),
                arguments: json!({"text": "hello"}),
            }],
            "Here is the echoed result.",
        ));
        let mut agent = make_agent_with_tools(storage, provider, echo_dispatcher());
        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let result = agent
            .process_message(InboundMessage {
                message: "Echo hello".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message");

        assert_eq!(result.response, "Here is the echoed result.");
        assert!(result.tool_calls_count >= 1, "should have at least 1 tool call");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_call_record_persisted_with_correct_fields() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(ToolCallProvider::new(
            vec![ToolCall {
                call_id: "call-persist".into(),
                name: "echo".into(),
                arguments: json!({"text": "persist me"}),
            }],
            "Done.",
        ));
        let mut agent = make_agent_with_tools(storage.clone(), provider, echo_dispatcher());
        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let result = agent
            .process_message(InboundMessage {
                message: "Persist test".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message");

        let records = storage.list_tool_calls_by_turn(result.turn_id)
            .await
            .expect("list tool calls");
        assert_eq!(records.len(), 1, "should have exactly 1 tool call record");
        assert_eq!(records[0].tool, "echo");
        assert_eq!(records[0].call_id, "call-persist");
        assert!(records[0].result.is_some(), "result should be persisted");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_execution_error_becomes_tool_result_turn_completes() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(ToolCallProvider::new(
            vec![ToolCall {
                call_id: "call-fail".into(),
                name: "failing_tool".into(),
                arguments: json!({}),
            }],
            "I handled the error.",
        ));
        let mut agent = make_agent_with_tools(storage.clone(), provider, failing_dispatcher());
        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let result = agent
            .process_message(InboundMessage {
                message: "Try the failing tool".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message should succeed even with tool error");

        assert_eq!(result.response, "I handled the error.");

        let records = storage.list_tool_calls_by_turn(result.turn_id)
            .await
            .expect("list tool calls");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status.as_deref(), Some("error"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn not_found_tool_sends_error_as_result_turn_completes() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(ToolCallProvider::new(
            vec![ToolCall {
                call_id: "call-denied".into(),
                name: "nonexistent_tool".into(),
                arguments: json!({}),
            }],
            "I noted the denial.",
        ));
        let mut agent = make_agent_with_tools(storage.clone(), provider, echo_dispatcher());
        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let result = agent
            .process_message(InboundMessage {
                message: "Call denied tool".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message should succeed even with denied tool");

        assert_eq!(result.response, "I noted the denial.");
    }
}

// ---------------------------------------------------------------------------
// Multi-round tool calling tests [ghtn]
// ---------------------------------------------------------------------------

#[cfg(test)]
mod multi_round_tests {
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;
    use serde_json::{json, Value};

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
        ToolCall, ToolCompletionRequest, ToolCompletionResponse, ToolError,
    };
    use swage_provider::LlmProvider;
    use swage_storage::model::NewProject;
    use swage_storage::store::ProjectStore;
    use swage_storage::SqliteStorage;
    use swage_tool::context::ToolContext;
    use swage_tool::dispatch::ToolDispatcher;
    use swage_tool::tool::Tool;
    use uuid::Uuid;

    use super::*;
    use crate::builder::AgentBuilder;
    use crate::config::{AgentConfig, CompactionThreshold};

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes its input."
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        async fn execute(
            &self,
            params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            Ok(json!({ "echoed": params["text"] }))
        }
    }

    /// Mock provider that returns tool calls for `rounds` rounds, then text.
    struct MultiRoundProvider {
        call_count: AtomicUsize,
        /// Number of tool-call rounds before returning text.
        rounds: usize,
        final_response: String,
    }

    impl MultiRoundProvider {
        fn new(rounds: usize, final_response: &str) -> Self {
            Self {
                call_count: AtomicUsize::new(0),
                rounds,
                final_response: final_response.into(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MultiRoundProvider {
        fn model_id(&self) -> &str {
            "multi-round-mock"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: self.final_response.clone(),
                input_tokens: 10,
                output_tokens: 20,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n < self.rounds {
                // Return a tool call for this round.
                Ok(ToolCompletionResponse {
                    content: format!("Round {} thinking", n + 1),
                    tool_calls: vec![ToolCall {
                        call_id: format!("call-round-{}", n + 1),
                        name: "echo".into(),
                        arguments: json!({"text": format!("round {}", n + 1)}),
                    }],
                    input_tokens: 5,
                    output_tokens: 15,
                    finish_reason: FinishReason::ToolUse,
                    cost: None,
                })
            } else {
                // Final text response.
                Ok(ToolCompletionResponse {
                    content: self.final_response.clone(),
                    tool_calls: vec![],
                    input_tokens: 10,
                    output_tokens: 20,
                    finish_reason: FinishReason::Stop,
                    cost: None,
                })
            }
        }

        fn stream(
            &self,
            _: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    /// Mock provider that returns a context-too-long error on first call,
    /// then succeeds on retry after compaction.
    struct ContextTooLongProvider {
        call_count: AtomicUsize,
    }

    #[async_trait]
    impl LlmProvider for ContextTooLongProvider {
        fn model_id(&self) -> &str {
            "ctx-too-long-mock"
        }

        async fn complete(
            &self,
            _: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                content: "compacted response".into(),
                input_tokens: 5,
                output_tokens: 10,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First call: simulate context-too-long error.
                Err(ProviderError::ModelError {
                    model: "test-model".into(),
                    message: "context window exceeded".into(),
                })
            } else {
                // After compaction: succeed.
                Ok(ToolCompletionResponse {
                    content: "compacted response".into(),
                    tool_calls: vec![],
                    input_tokens: 5,
                    output_tokens: 10,
                    finish_reason: FinishReason::Stop,
                    cost: None,
                })
            }
        }

        fn stream(
            &self,
            _: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    async fn setup_storage() -> (Arc<SqliteStorage>, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("in-memory storage"));
        let project = ProjectStore::create(
            storage.as_ref(),
            NewProject {
                name: "test-project".into(),
                slug: "test-project".into(),
                workspace_path: None,
            },
        )
        .await
        .expect("create test project");
        (storage, project.id)
    }

    fn echo_dispatcher() -> ToolDispatcher {
        ToolDispatcher::allow_all(vec![Arc::new(EchoTool) as Arc<dyn Tool>])
    }

    fn make_agent_with_config(
        storage: Arc<SqliteStorage>,
        provider: Arc<dyn LlmProvider>,
        dispatcher: ToolDispatcher,
        max_tool_rounds: u32,
    ) -> Agent {
        AgentBuilder::new()
            .config(AgentConfig {
                agent_id: "test-agent".into(),
                max_tool_rounds,
                compaction_threshold: CompactionThreshold::MessageCount(100),
                metadata: HashMap::new(),
            })
            .provider(provider)
            .storage(storage)
            .tool_dispatcher(dispatcher)
            .build()
            .expect("build agent")
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn three_rounds_of_tool_calls_then_text_succeeds() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(MultiRoundProvider::new(3, "All done after 3 rounds."));
        let mut agent = make_agent_with_config(storage, provider, echo_dispatcher(), 10);
        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let result = agent
            .process_message(InboundMessage {
                message: "Do three rounds".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message");

        assert_eq!(result.response, "All done after 3 rounds.");
        assert_eq!(result.tool_calls_count, 3, "should have 3 tool call rounds");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn max_tool_rounds_exceeded_returns_error_and_fails_turn() {
        let (storage, project_id) = setup_storage().await;
        // Provider returns 2 rounds of tool calls, but max is 1.
        let provider = Arc::new(MultiRoundProvider::new(2, "unreachable"));
        let mut agent = make_agent_with_config(storage.clone(), provider, echo_dispatcher(), 1);
        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let err = agent
            .process_message(InboundMessage {
                message: "Exceed rounds".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect_err("should fail with ToolRoundsExceeded");

        assert!(
            matches!(err, AgentError::ToolRoundsExceeded { max: 1 }),
            "expected ToolRoundsExceeded {{ max: 1 }}, got {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn context_too_long_triggers_compaction_and_recovers() {
        let (storage, project_id) = setup_storage().await;
        let provider = Arc::new(ContextTooLongProvider {
            call_count: AtomicUsize::new(0),
        });
        let mut agent = make_agent_with_config(storage, provider, echo_dispatcher(), 10);
        agent.create_session(project_id).await.expect("create session");

        let session_id = agent.active_session_id.unwrap();
        let result = agent
            .process_message(InboundMessage {
                message: "Trigger context recovery".into(),
                session_id,
                thread_id: None,
            })
            .await
            .expect("process_message should recover after compaction");

        assert_eq!(result.response, "compacted response");
    }
}
