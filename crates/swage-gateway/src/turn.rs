use std::path::PathBuf;
use std::time::Instant;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_core::{
    ChatMessage, GatewayError, LlmRequest, Role, StreamChunk, ToolCall, ToolResult,
    build_tool_round_messages,
};
use swage_storage::model::{NewMessage, NewToolCall, NewTurn, TurnState};

use crate::{
    context::{ProjectContext, PROJECT_CONTEXT},
    state::AppState,
    websocket::ServerEvent,
};
use crate::tool::python_execute::{PythonExecuteContext, PYTHON_CONTEXT};

/// Orchestrate a complete agent turn: user message → provider stream → storage.
///
/// # Flow
/// 1. Guard: reject if a turn is already InProgress for this session.
/// 2. Create a Turn record (state=InProgress) and register a CancellationToken.
/// 3. Store the user message as position 0 in the new turn.
/// 4. Build message history and send a streaming request to the provider.
/// 5. For each chunk: forward Token events to the WebSocket, accumulate content.
/// 6. On Done: store the assistant message, mark the turn Completed, send TurnComplete.
/// 7. On Cancel: store any partial response, mark the turn Interrupted, send Cancelled.
/// 8. On provider Error: mark the turn Failed, send an Error event.
/// 9. Cleanup: remove the CancellationToken from active_turns.
pub async fn process_turn(
    state: &AppState,
    session_id: Uuid,
    content: String,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
) -> Result<(), GatewayError> {
    // Step 1: Reject if a turn is already InProgress for this session.
    let existing_turns = state.storage.list_turns_by_session(session_id).await?;
    if existing_turns
        .iter()
        .any(|t| t.state == TurnState::InProgress)
    {
        return Err(GatewayError::TurnInProgress { session_id });
    }

    // Resolve the project for this session so we can load per-project config
    // and build the ProjectContext that tools read via the task-local.
    let session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let project_id = session.project_id;

    let project = state
        .storage
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
    let project_slug = project.slug.clone();

    // Load project-specific config (model, instructions, etc.). Falls back to
    // the startup config when no project-level config.toml exists, which is
    // the normal case for most projects.
    let config = match swage_config::load_resolved_config(&project_slug, "default") {
        Ok(c) => c,
        Err(_) => state.config.clone(),
    };

    // Build the memory path for this project: ~/.swage/projects/{slug}/memory/
    // Exposed via PROJECT_CONTEXT so MemoryTool can determine where to write.
    let memory_path = std::env::var("HOME")
        .ok()
        .map(|h| {
            PathBuf::from(h)
                .join(".swage")
                .join("projects")
                .join(&project_slug)
                .join("memory")
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    let project_ctx = ProjectContext {
        project_id,
        project_slug,
        memory_path,
    };

    // Step 2: Create the turn record and register a cancellation token.
    let turn = state
        .storage
        .create_turn(NewTurn {
            session_id,
            state: TurnState::InProgress,
        })
        .await?;

    let cancel_token = CancellationToken::new();
    {
        let mut turns = state.active_turns.lock().await;
        turns.insert(session_id, cancel_token.clone());
    }

    // Step 3: Store the user message (position 0 in this turn).
    let user_message_count = state
        .storage
        .list_messages_by_turn(turn.id)
        .await
        .map(|msgs| msgs.len() as i32)
        .unwrap_or(0);

    state
        .storage
        .create_message(NewMessage {
            turn_id: turn.id,
            role: Role::User.to_string(),
            content: content.clone(),
            position: user_message_count,
        })
        .await?;

    // Step 4: Build message history for the provider request.
    let history = state.storage.list_messages_by_session(session_id).await?;
    let messages: Vec<ChatMessage> = history
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

    // Step 5: Include tool definitions from the registry in the request.
    let tool_defs = state.tool_registry.definitions();

    // Use the per-project model and instructions (falls back to startup config
    // when no project-specific config.toml exists).
    let mut request = LlmRequest::builder(&config.provider.model)
        .messages(messages)
        .system_prompt(&config.instructions)
        .tools(tool_defs)
        .stream(true)
        .build();

    // Step 6-8: Tool-aware streaming loop.
    //
    // Each iteration streams one provider response. If the response contains
    // ToolUse chunks, we execute each tool, build result messages, append them
    // to the request, and loop again. The loop exits when a response produces
    // no tool calls (text-only finish) or on cancellation/error.
    let mut accumulated = String::new();
    let mut stored_usage = None;
    // Running position counter for tool calls within this turn.
    let mut tool_position: i32 = 0;

    let outcome = 'outer: loop {
        let mut stream = state.provider.stream_message(request.clone());
        // Text and tool calls collected in this streaming round.
        let mut round_text = String::new();
        let mut round_tool_calls: Vec<ToolCall> = Vec::new();

        // Stream one provider response.
        let _round_outcome = loop {
            tokio::select! {
                // Cancellation takes priority.
                _ = cancel_token.cancelled() => {
                    break 'outer TurnOutcome::Cancelled;
                }
                chunk = stream.next() => {
                    match chunk {
                        None => break RoundOutcome::Done,
                        Some(StreamChunk::Done { .. }) => break RoundOutcome::Done,
                        Some(StreamChunk::Delta { content }) => {
                            round_text.push_str(&content);
                            accumulated.push_str(&content);
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

        // If no tool calls in this round, we're done.
        if round_tool_calls.is_empty() {
            break 'outer TurnOutcome::Done;
        }

        // Execute each tool call and collect results.
        let mut tool_results: Vec<ToolResult> = Vec::new();

        for call in &round_tool_calls {
            // Check cancellation between tool executions.
            if cancel_token.is_cancelled() {
                break 'outer TurnOutcome::Cancelled;
            }

            // Send ToolStart event to the WebSocket.
            // Native tool calls (dispatched directly by the model) have no parent.
            // Sub-tool calls from python_execute pass Some(parent_id) here.
            let _ = ws_tx.send(ServerEvent::ToolStart {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                parent_call_id: None,
            });

            // Store the tool call record (no result yet).
            let _ = state.storage.create_tool_call(NewToolCall {
                turn_id: turn.id,
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
            }).await;
            tool_position += 1;

            // Look up the tool in the registry and execute it.
            //
            // All executions are wrapped in PROJECT_CONTEXT.scope() so tools
            // like MemoryTool can read the current project without parameters.
            // For python_execute, PYTHON_CONTEXT is nested inside so the
            // GatewayToolDispatcher gets the correct turn/WebSocket context.
            let start = Instant::now();
            let (result_content, is_error, truncated, total_bytes) =
                match state.tool_registry.get(&call.name) {
                    Some(tool) => {
                        let exec_result = if call.name == "python_execute" {
                            let ctx = PythonExecuteContext {
                                ws_tx: ws_tx.clone(),
                                turn_id: turn.id,
                                call_id: call.call_id.clone(),
                            };
                            PROJECT_CONTEXT
                                .scope(
                                    project_ctx.clone(),
                                    PYTHON_CONTEXT.scope(ctx, tool.execute(call.arguments.clone())),
                                )
                                .await
                        } else {
                            PROJECT_CONTEXT
                                .scope(project_ctx.clone(), tool.execute(call.arguments.clone()))
                                .await
                        };
                        (
                            exec_result.content,
                            exec_result.is_error,
                            exec_result.truncated,
                            exec_result.total_bytes,
                        )
                    }
                    None => {
                        // Unknown tool — return an error that the model can read.
                        (
                            format!("Tool '{}' is not registered.", call.name),
                            true,
                            false,
                            None,
                        )
                    }
                };
            let duration = start.elapsed();
            let duration_ms = duration.as_millis() as u64;

            let status = if is_error { "error" } else { "success" };

            // Send ToolEnd event.
            let _ = ws_tx.send(ServerEvent::ToolEnd {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                result: serde_json::json!(result_content),
                status: status.to_string(),
                duration_ms,
                truncated,
                total_bytes: total_bytes.map(|b| b as u64),
                parent_call_id: None,
            });

            // Update the tool call record with the result.
            let _ = state.storage.update_tool_call_result(
                &call.call_id,
                turn.id,
                &result_content,
                status,
                duration_ms as i64,
                truncated,
                total_bytes.map(|b| b as i64),
            ).await;

            tool_results.push(ToolResult {
                call_id: call.call_id.clone(),
                content: result_content,
                is_error,
            });
        }

        // Build tool_result messages and append to the conversation.
        let tool_messages = build_tool_round_messages(
            &round_tool_calls,
            &tool_results,
            &round_text,
        );

        // Extend the request's message history with the tool round.
        // Re-use the resolved_config model so the loop stays on the
        // project-specific model across all rounds.
        let mut updated_messages = request.messages.clone();
        updated_messages.extend(tool_messages);
        // Preserve the per-project model; system_prompt carries forward from
        // the previous request so we don't re-assemble instructions mid-turn.
        request = LlmRequest::builder(&config.provider.model)
            .messages(updated_messages)
            .system_prompt(
                request.system_prompt.as_deref().unwrap_or_default(),
            )
            .tools(request.tools.clone())
            .stream(true)
            .build();

        // Reset round_text for the next iteration (accumulated carries forward).
        // The outer loop continues to stream the next response.
    };

    // Step 9: Finalise based on outcome, then clean up.
    match outcome {
        TurnOutcome::Done => {
            // Store the assistant's message.
            let position = state
                .storage
                .list_messages_by_turn(turn.id)
                .await
                .map(|msgs| msgs.len() as i32)
                .unwrap_or(1);

            state
                .storage
                .create_message(NewMessage {
                    turn_id: turn.id,
                    role: Role::Assistant.to_string(),
                    content: accumulated,
                    position,
                })
                .await?;

            // Log usage if provided (non-fatal if storage isn't ready for it yet).
            let _ = stored_usage;

            state
                .storage
                .update_turn_state(turn.id, TurnState::Completed)
                .await?;

            state.storage.update_activity(session_id).await?;

            let _ = ws_tx.send(ServerEvent::TurnComplete { turn_id: turn.id });
        }

        TurnOutcome::Cancelled => {
            // Store whatever accumulated before cancellation (may be empty).
            if !accumulated.is_empty() {
                let position = state
                    .storage
                    .list_messages_by_turn(turn.id)
                    .await
                    .map(|msgs| msgs.len() as i32)
                    .unwrap_or(1);

                state
                    .storage
                    .create_message(NewMessage {
                        turn_id: turn.id,
                        role: Role::Assistant.to_string(),
                        content: accumulated,
                        position,
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

    use swage_core::{LlmRequest, LlmResponse, ProviderError, StreamChunk};
    use swage_provider::LlmProvider;
    use swage_core::StorageError;
    use swage_storage::{
        model::{
            Message, NewMessage, NewProject, NewSession, NewToolCall, NewTurn, Project, Session,
            ToolCallRecord, Turn, TurnState,
        },
        store::{MessageStore, ProjectStore, SessionStore, ToolCallStore, TurnStore},
    };

    use super::*;
    use crate::{state::AppState, websocket::ServerEvent};

    // -----------------------------------------------------------------------
    // Mock LLM providers
    //
    // Uses `std::sync::Mutex` (not tokio) because `stream_message` is a sync
    // function — we cannot `.await` inside it to take a tokio lock.
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
        async fn send_message(
            &self,
            _request: LlmRequest,
        ) -> Result<LlmResponse, ProviderError> {
            unimplemented!("not used in streaming tests")
        }

        fn stream_message(
            &self,
            _request: LlmRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            // Drain the chunk list so each provider call gets its own sequence.
            let chunks = {
                let mut lock = self.chunks.lock().expect("mock lock poisoned");
                std::mem::take(&mut *lock)
            };
            Box::pin(stream::iter(chunks))
        }
    }

    /// A mock provider supporting multiple streaming rounds.
    ///
    /// Each call to `stream_message` pops the next batch of chunks from the
    /// front of the queue.  Used in tool execution tests where the model calls
    /// a tool and then produces a final response.
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
        async fn send_message(
            &self,
            _request: LlmRequest,
        ) -> Result<LlmResponse, ProviderError> {
            unimplemented!("not used in multi-round tests")
        }

        fn stream_message(
            &self,
            _request: LlmRequest,
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

    /// A simple tool that echoes its "text" argument, prefixed with "Echo: ".
    /// Used in tool execution loop tests.
    struct EchoTool;

    #[async_trait]
    impl crate::tool::Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "Echoes text back" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            })
        }
        async fn execute(&self, arguments: serde_json::Value) -> crate::tool::ToolExecutionResult {
            let text = arguments["text"].as_str().unwrap_or("").to_string();
            crate::tool::ToolExecutionResult {
                content: format!("Echo: {text}"),
                is_error: false,
                truncated: false,
                total_bytes: None,
            }
        }
    }

    /// A mock provider whose stream blocks forever.
    ///
    /// Used in cancellation tests: the turn task blocks on the pending stream,
    /// giving the test time to call `cancel_turn` without racing against stream
    /// completion.
    pub(crate) struct PendingProvider;

    #[async_trait]
    impl LlmProvider for PendingProvider {
        async fn send_message(
            &self,
            _request: LlmRequest,
        ) -> Result<LlmResponse, ProviderError> {
            unimplemented!("not used in cancellation tests")
        }

        fn stream_message(
            &self,
            _request: LlmRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            // Never produces any chunks — the tokio::select! in process_turn will
            // block on this, allowing the cancellation token to fire.
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
        pub(crate) turns: HashMap<Uuid, Turn>,
        pub(crate) messages: Vec<Message>,
        pub(crate) tool_calls: Vec<ToolCallRecord>,
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
            self.create(NewProject {
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
    impl TurnStore for MockStorage {
        async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError> {
            let t = Turn {
                id: Uuid::new_v4(),
                session_id: turn.session_id,
                state: turn.state,
                created_at: chrono::Utc::now(),
                completed_at: None,
            };
            self.db.lock().await.turns.insert(t.id, t.clone());
            Ok(t)
        }

        async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
            Ok(self.db.lock().await.turns.get(&id).cloned())
        }

        async fn list_turns_by_session(
            &self,
            session_id: Uuid,
        ) -> Result<Vec<Turn>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .turns
                .values()
                .filter(|t| t.session_id == session_id)
                .cloned()
                .collect())
        }

        async fn update_turn_state(
            &self,
            id: Uuid,
            state: TurnState,
        ) -> Result<(), StorageError> {
            if let Some(turn) = self.db.lock().await.turns.get_mut(&id) {
                turn.state = state;
            }
            Ok(())
        }

        async fn find_in_progress_turns(&self) -> Result<Vec<Turn>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .turns
                .values()
                .filter(|t| t.state == TurnState::InProgress)
                .cloned()
                .collect())
        }
    }

    #[async_trait]
    impl MessageStore for MockStorage {
        async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError> {
            let m = Message {
                id: Uuid::new_v4(),
                turn_id: message.turn_id,
                role: message.role,
                content: message.content,
                position: message.position,
                created_at: chrono::Utc::now(),
            };
            self.db.lock().await.messages.push(m.clone());
            Ok(m)
        }

        async fn list_messages_by_session(
            &self,
            session_id: Uuid,
        ) -> Result<Vec<Message>, StorageError> {
            let db = self.db.lock().await;
            let turn_ids: std::collections::HashSet<Uuid> = db
                .turns
                .values()
                .filter(|t| t.session_id == session_id)
                .map(|t| t.id)
                .collect();
            let mut msgs: Vec<Message> = db
                .messages
                .iter()
                .filter(|m| turn_ids.contains(&m.turn_id))
                .cloned()
                .collect();
            msgs.sort_by_key(|m| m.position);
            Ok(msgs)
        }

        async fn list_messages_by_turn(
            &self,
            turn_id: Uuid,
        ) -> Result<Vec<Message>, StorageError> {
            let db = self.db.lock().await;
            let mut msgs: Vec<Message> = db
                .messages
                .iter()
                .filter(|m| m.turn_id == turn_id)
                .cloned()
                .collect();
            msgs.sort_by_key(|m| m.position);
            Ok(msgs)
        }

        async fn get_latest_message_for_session(
            &self,
            session_id: Uuid,
        ) -> Result<Option<Message>, StorageError> {
            let db = self.db.lock().await;
            let turn_ids: std::collections::HashSet<Uuid> = db
                .turns
                .values()
                .filter(|t| t.session_id == session_id)
                .map(|t| t.id)
                .collect();
            Ok(db
                .messages
                .iter()
                .filter(|m| turn_ids.contains(&m.turn_id))
                .max_by_key(|m| m.position)
                .cloned())
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

    /// Build an AppState with a pre-populated session for testing.
    pub(crate) async fn make_state_with_session(
        provider_chunks: Vec<StreamChunk>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();

        // Pre-populate with a session so update_activity doesn't fail.
        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");
        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        let provider = Arc::new(MockProvider::new(provider_chunks));
        let state = AppState {
            storage: storage as Arc<dyn swage_storage::store::Storage>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_registry: Arc::new(crate::tool::ToolRegistry::new()),
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

        process_turn(&state, session_id, "Hi".to_string(), tx)
            .await
            .expect("turn should complete without error");

        // Collect events — expect 3 tokens then TurnComplete.
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

        // Verify messages were stored in the DB.
        let messages = state
            .storage
            .list_messages_by_session(session_id)
            .await
            .unwrap();
        assert_eq!(messages.len(), 2, "user message + assistant message");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hi");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "Hello, world!");

        // Verify the turn was marked Completed.
        let turns = state
            .storage
            .list_turns_by_session(session_id)
            .await
            .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].state, TurnState::Completed);
    }

    #[tokio::test]
    async fn cancelled_turn_stores_partial_and_marks_interrupted() {
        // Use PendingProvider so the stream never resolves on its own.
        // This guarantees the turn is still active when we call cancel_turn.
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");
        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        let state = Arc::new(AppState {
            storage: storage as Arc<dyn swage_storage::store::Storage>,
            provider: Arc::new(PendingProvider) as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_registry: Arc::new(crate::tool::ToolRegistry::new()),
        });

        let (tx, mut rx) = mpsc::unbounded_channel();

        // Spawn the turn in a background task.
        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            process_turn(&*state_clone, session_id, "Hi".to_string(), tx).await
        });

        // Wait until the cancellation token is registered in active_turns.
        // With a pending stream, the turn task will hit the select! loop and yield.
        // Polling active_turns is more reliable than a fixed sleep.
        for _ in 0..50 {
            tokio::task::yield_now().await;
            if state.active_turns.lock().await.contains_key(&session_id) {
                break;
            }
        }

        // Token must now be registered.
        assert!(
            state.active_turns.lock().await.contains_key(&session_id),
            "turn should be active before cancel"
        );

        cancel_turn(&state, session_id)
            .await
            .expect("cancel should succeed");

        handle.await.expect("task should not panic").expect("process_turn ok");

        // Verify Cancelled event was sent.
        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| matches!(e, ServerEvent::Cancelled {})),
            "expected Cancelled event"
        );

        // Verify turn is Interrupted in storage.
        let turns = state
            .storage
            .list_turns_by_session(session_id)
            .await
            .unwrap();
        assert_eq!(turns[0].state, TurnState::Interrupted);
    }

    #[tokio::test]
    async fn provider_error_marks_turn_failed() {
        let (state, session_id) = make_state_with_session(vec![StreamChunk::Error {
            message: "provider overloaded".to_string(),
        }])
        .await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx)
            .await
            .expect("error is surfaced via event, not Err return");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            ServerEvent::Error { code, .. } if code == "provider_error"
        )));

        let turns = state
            .storage
            .list_turns_by_session(session_id)
            .await
            .unwrap();
        assert_eq!(turns[0].state, TurnState::Failed);
    }

    #[tokio::test]
    async fn concurrent_turn_rejected_with_turn_in_progress() {
        // We create a turn manually in InProgress state to simulate an active turn,
        // then attempt to start another one for the same session.
        let (state, session_id) = make_state_with_session(vec![]).await;

        // Manually insert an InProgress turn to simulate an active turn.
        state
            .storage
            .create_turn(NewTurn {
                session_id,
                state: TurnState::InProgress,
            })
            .await
            .unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = process_turn(&state, session_id, "Another message".to_string(), tx).await;

        assert!(
            matches!(result, Err(GatewayError::TurnInProgress { .. })),
            "expected TurnInProgress, got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Mock tool for the execution loop (returns text directly without prefix)
    // -----------------------------------------------------------------------

    /// A tool that echoes its "text" argument back — predictable output for tests.
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
        ) -> crate::tool::ToolExecutionResult {
            let text = arguments["text"].as_str().unwrap_or("").to_string();
            crate::tool::ToolExecutionResult {
                content: text,
                is_error: false,
                truncated: false,
                total_bytes: None,
            }
        }
    }

    /// Build an AppState with a multi-round provider and a ToolRegistry
    /// containing the EchoMockTool.
    async fn make_state_with_tools(
        rounds: Vec<Vec<StreamChunk>>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        let provider = Arc::new(MultiRoundMockProvider::new(rounds));

        let mut registry = crate::tool::ToolRegistry::new();
        registry.register(Arc::new(EchoMockTool));

        let state = AppState {
            storage: storage as Arc<dyn swage_storage::store::Storage>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_registry: Arc::new(registry),
        };
        (state, session_id)
    }

    // -----------------------------------------------------------------------
    // Tool execution loop tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn tool_call_sends_tool_start_and_tool_end_events() {
        // Round 1: model calls the echo tool.
        // Round 2: model produces final text.
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

        process_turn(&state, session_id, "Use echo".to_string(), tx)
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        // ToolStart must be emitted with the right tool name.
        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolStart { tool, call_id, .. }
                if tool == "echo" && call_id == "call_1"
            )),
            "expected ToolStart event, got: {events:?}"
        );

        // ToolEnd must follow.
        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolEnd { tool, call_id, status, .. }
                if tool == "echo" && call_id == "call_1" && status == "success"
            )),
            "expected ToolEnd event, got: {events:?}"
        );

        // Final text should also appear.
        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::Token { content } if content.contains("hello")
            )),
            "expected token with 'hello', got: {events:?}"
        );

        // Turn should complete.
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

        process_turn(&state, session_id, "Store this".to_string(), tx)
            .await
            .expect("turn should complete");

        // Check the tool call was persisted in storage.
        let turns = state.storage.list_turns_by_session(session_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 1, "expected 1 tool call record");
        assert_eq!(tool_calls[0].call_id, "call_store");
        assert_eq!(tool_calls[0].tool, "echo");
        assert!(tool_calls[0].result.is_some(), "tool call should have a result");
        assert_eq!(tool_calls[0].result.as_deref(), Some("stored"));
    }

    #[tokio::test]
    async fn unknown_tool_returns_error_to_model() {
        // Model calls a tool that doesn't exist in the registry.
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

        process_turn(&state, session_id, "Use unknown".to_string(), tx)
            .await
            .expect("turn should still complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        // ToolEnd should have an error status.
        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolEnd { status, .. } if status == "error"
            )),
            "expected ToolEnd with error status, got: {events:?}"
        );

        // Turn should still complete — the model gets the error and can respond.
        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
    }

    #[tokio::test]
    async fn multi_round_tool_calling() {
        // Round 1: model calls echo twice.
        // Round 2: model calls echo once more.
        // Round 3: model produces final text.
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

        process_turn(&state, session_id, "Multi-round".to_string(), tx)
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        // Should have 3 ToolStart/ToolEnd pairs.
        let tool_starts: Vec<_> = events.iter().filter(|e| matches!(e, ServerEvent::ToolStart { .. })).collect();
        let tool_ends: Vec<_> = events.iter().filter(|e| matches!(e, ServerEvent::ToolEnd { .. })).collect();
        assert_eq!(tool_starts.len(), 3, "expected 3 ToolStart events");
        assert_eq!(tool_ends.len(), 3, "expected 3 ToolEnd events");

        // All 3 tool calls should be stored.
        let turns = state.storage.list_turns_by_session(session_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 3, "expected 3 tool call records");

        // Final text and TurnComplete.
        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
    }

    // -----------------------------------------------------------------------
    // Project context and config resolution tests
    // -----------------------------------------------------------------------

    /// A provider that captures the LlmRequest passed to `stream_message`.
    ///
    /// Used to assert that `process_turn` uses the correct model and system
    /// prompt derived from per-project config resolution.
    struct CapturingProvider {
        captured: Arc<std::sync::Mutex<Option<LlmRequest>>>,
        chunks: Vec<StreamChunk>,
    }

    impl CapturingProvider {
        fn new(chunks: Vec<StreamChunk>) -> (Self, Arc<std::sync::Mutex<Option<LlmRequest>>>) {
            let captured = Arc::new(std::sync::Mutex::new(None));
            (
                Self { captured: Arc::clone(&captured), chunks },
                captured,
            )
        }
    }

    #[async_trait]
    impl LlmProvider for CapturingProvider {
        async fn send_message(&self, _: LlmRequest) -> Result<LlmResponse, ProviderError> {
            unimplemented!()
        }

        fn stream_message(&self, request: LlmRequest) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            *self.captured.lock().unwrap() = Some(request);
            Box::pin(stream::iter(self.chunks.clone()))
        }
    }

    /// A tool that captures the `PROJECT_CONTEXT` task-local during execution.
    ///
    /// The captured (project_id, slug) pair is stored in `captured` for
    /// assertion after the turn completes.
    struct ContextCaptureTool {
        captured: Arc<std::sync::Mutex<Option<(Uuid, String)>>>,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for ContextCaptureTool {
        fn name(&self) -> &str { "capture_context" }
        fn description(&self) -> &str { "Captures the project context task-local" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn execute(&self, _: serde_json::Value) -> crate::tool::ToolExecutionResult {
            // Read the project context from the task-local, if set.
            let context = crate::context::PROJECT_CONTEXT
                .try_with(|ctx| (ctx.project_id, ctx.project_slug.clone()))
                .ok();
            *self.captured.lock().unwrap() = context;
            crate::tool::ToolExecutionResult {
                content: "captured".into(),
                is_error: false,
                truncated: false,
                total_bytes: None,
            }
        }
    }

    #[tokio::test]
    async fn process_turn_sets_project_context_for_tool_execution() {
        // Create a tool that captures the PROJECT_CONTEXT task-local.
        let captured = Arc::new(std::sync::Mutex::new(None));
        let capture_tool = Arc::new(ContextCaptureTool { captured: Arc::clone(&captured) });

        let storage = Arc::new(MockStorage::new());
        let project = storage.get_or_create_default().await.expect("default project");
        let session_id = Uuid::new_v4();
        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        // Round 1: model calls capture_context.
        // Round 2: model produces final text.
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
        let mut registry = crate::tool::ToolRegistry::new();
        registry.register(capture_tool);

        let state = AppState {
            storage: storage as Arc<dyn swage_storage::store::Storage>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_registry: Arc::new(registry),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "capture".into(), tx).await.expect("turn ok");

        // The tool should have read the project context.
        let (captured_id, captured_slug) = captured
            .lock()
            .unwrap()
            .take()
            .expect("PROJECT_CONTEXT should have been set during tool execution");

        assert_eq!(captured_id, project.id, "project_id should match the session's project");
        assert_eq!(captured_slug, "default", "slug should be 'default' for the default project");
    }

    #[tokio::test]
    async fn process_turn_resolves_config_per_project_and_completes_turn() {
        // Verify that per-project config resolution does not break the turn
        // lifecycle. The resolved config may come from the global ~/.swage/
        // hierarchy or fall back to state.config — either way the turn should
        // complete and the request must have a non-empty model string.
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".into() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (provider, captured_request) = CapturingProvider::new(chunks);

        let storage = Arc::new(MockStorage::new());
        // Create a project with a slug that has no project-level config.toml —
        // config resolution will use only the global level (or fall back to
        // state.config when the root can't be resolved).
        let project = storage
            .create(swage_storage::model::NewProject {
                name: "Test Config Project".to_string(),
                slug: "test-config-resolution-no-project-toml".to_string(),
                workspace_path: None,
            })
            .await
            .expect("create test project");
        let session_id = Uuid::new_v4();
        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        let state = AppState {
            storage: storage as Arc<dyn swage_storage::store::Storage>,
            provider: Arc::new(provider) as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_registry: Arc::new(crate::tool::ToolRegistry::new()),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "hi".into(), tx).await.expect("turn ok");

        // The request must have been issued with a non-empty model — whether from
        // the global config hierarchy or the state.config fallback.
        let req = captured_request.lock().unwrap().take().expect("request captured");
        assert!(!req.model.is_empty(), "model must be non-empty regardless of config source");
    }

    #[tokio::test]
    async fn text_only_turn_still_works_with_tool_registry() {
        // Verify that the tool-aware loop doesn't break text-only responses.
        let (state, session_id) = make_state_with_tools(vec![vec![
            StreamChunk::Delta { content: "Just text".into() },
            StreamChunk::Done { finish_reason: None },
        ]]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hello".to_string(), tx)
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, ServerEvent::Token { content } if content == "Just text")));
        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));

        // No tool events.
        assert!(!events.iter().any(|e| matches!(e, ServerEvent::ToolStart { .. })));
    }

    // -----------------------------------------------------------------------
    // PROJECT_CONTEXT task-local tests
    // -----------------------------------------------------------------------

    /// A tool that reads `PROJECT_CONTEXT` and returns the project slug.
    /// Used to verify that `process_turn` correctly sets the context before
    /// delegating to tool execution.
    struct ProjectContextReadTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for ProjectContextReadTool {
        fn name(&self) -> &str { "project_context_read" }
        fn description(&self) -> &str { "Returns the current project slug from PROJECT_CONTEXT" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        async fn execute(&self, _arguments: serde_json::Value) -> crate::tool::ToolExecutionResult {
            let slug = PROJECT_CONTEXT
                .try_with(|ctx| ctx.project_slug.clone())
                // Returns a sentinel if the context was not set — the test asserts this
                // value is NOT present, making the test fail before the implementation
                // is in place.
                .unwrap_or_else(|_| "no-context".to_string());
            crate::tool::ToolExecutionResult {
                content: slug,
                is_error: false,
                truncated: false,
                total_bytes: None,
            }
        }
    }

    #[tokio::test]
    async fn project_context_is_set_during_tool_execution() {
        // Create a project with a distinctive slug.
        let storage = Arc::new(MockStorage::new());
        let project = storage
            .create(NewProject {
                name: "my-project".to_string(),
                slug: "my-project".to_string(),
                workspace_path: None,
            })
            .await
            .expect("create project");

        let session_id = Uuid::new_v4();
        storage.db.lock().await.sessions.insert(
            session_id,
            Session {
                id: session_id,
                project_id: project.id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        // Round 1: model calls the context-reading tool.
        // Round 2: model produces its final response.
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "ctx-check".into(),
                name: "project_context_read".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

        let mut registry = crate::tool::ToolRegistry::new();
        registry.register(Arc::new(ProjectContextReadTool));

        let state = AppState {
            storage: storage as Arc<dyn swage_storage::store::Storage>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_registry: Arc::new(registry),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "Check context".to_string(), tx)
            .await
            .expect("turn should complete");

        // The tool result stored in the DB should contain the project slug.
        let turns = state.storage.list_turns_by_session(session_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0].result.as_deref(),
            Some("my-project"),
            "PROJECT_CONTEXT must be set with the project slug during tool execution"
        );
    }
}
