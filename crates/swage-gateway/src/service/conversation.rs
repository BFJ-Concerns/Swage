use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use serde_json::Value as JsonValue;
use swage_core::{GatewayError, Role, StorageError};
use swage_storage::{
    model::{NewSession, NewThread, ThreadBranch, TurnState},
    store::{BenchStore, Storage},
};

// ---------------------------------------------------------------------------
// Response types — serialised to JSON for API consumers
// ---------------------------------------------------------------------------

/// Response body for a newly created (or fetched) conversation.
#[derive(Debug, Serialize)]
pub struct ConversationResponse {
    pub id: Uuid,
    pub bench_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

/// Compact representation of a conversation, used in list responses.
#[derive(Debug, Serialize)]
pub struct ConversationSummary {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    /// Content of the most recent message in the conversation, if any.
    pub message_preview: Option<String>,
    /// True if any turn in the conversation was interrupted (e.g. by a crash).
    pub has_interrupted_turn: bool,
}

/// Full conversation including all messages with turn-level state information.
#[derive(Debug, Serialize)]
pub struct ConversationDetail {
    pub id: Uuid,
    pub bench_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<MessageView>,
}

/// Branch metadata for a child thread within a conversation.
#[derive(Debug, Serialize)]
pub struct ConversationBranchResponse {
    pub child_thread_id: Uuid,
    pub parent_thread_id: Uuid,
    pub branched_from_message_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a branch from a thread/message pair.
#[derive(Debug, Deserialize)]
pub struct CreateConversationBranchRequest {
    pub thread_id: Uuid,
    pub from_message_id: Uuid,
}

/// Detailed view of a single thread within a conversation.
#[derive(Debug, Serialize)]
pub struct ConversationThreadDetail {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub state: String,
    pub messages: Vec<MessageView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<ConversationBranchResponse>,
}

// TODO Move API view/DTO types into the swage-protocol crate
// The view types in this file (ToolCallView, MessageView, the conversation/
// bench/workspace responses) are the HTTP wire contract, mirrored by hand in
// frontend/src/lib/types.ts with no enforcement. Move them to swage-protocol
// so the frontend types are generated rather than maintained in parallel. See
// the wire-contract section in CLAUDE.md and the swage-protocol mote in
// .motes/TODO.md.
// ---

/// A tool call that occurred within a turn, as surfaced by the conversation API.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallView {
    pub call_id: String,
    pub tool: String,
    /// Tool arguments, parsed from the stored JSON string.
    pub arguments: JsonValue,
    /// Tool result, if available. Parsed from the stored string when valid JSON;
    /// otherwise wrapped as a plain string value.
    pub result: Option<JsonValue>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub truncated: bool,
    pub total_bytes: Option<u64>,
    /// call_id of the enclosing python_execute (or similar) call, if this is a
    /// sub-tool invocation. None for top-level native calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
}

/// A single message in a conversation, annotated with its turn's lifecycle state.
#[derive(Debug, Serialize)]
pub struct MessageView {
    pub thread_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Lifecycle state of the turn that produced this message, when known.
    pub turn_state: Option<TurnState>,
    /// Tool calls that occurred in this message's turn.
    ///
    /// Populated for assistant messages when the originating turn is known.
    pub tool_calls: Vec<ToolCallView>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Mediates between HTTP handlers and storage.
///
/// Translates domain errors into `GatewayError` variants and maps storage
/// rows to API-facing response types.
#[derive(Clone)]
pub struct ConversationService {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) bench_store: Arc<dyn BenchStore>,
}

impl ConversationService {
    pub fn new(storage: Arc<dyn Storage>, bench_store: Arc<dyn BenchStore>) -> Self {
        Self {
            storage,
            bench_store,
        }
    }

    /// Create a new conversation in the default bench.
    ///
    /// Also creates a default thread for the session so that process_turn has
    /// a thread_id to attach turns and messages to.
    pub async fn create_conversation(&self) -> Result<ConversationResponse, GatewayError> {
        self.create_conversation_with_model(None).await
    }

    pub async fn create_conversation_with_model(
        &self,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        let bench = self.bench_store.get_or_create_default().await?;
        let active_workspace_attachment_id = self.default_workspace_for_bench(bench.id).await?;
        let mut session = self
            .storage
            .create_session(NewSession {
                bench_id: bench.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id,
            })
            .await?;
        if let Some(selected_model_id) = normalise_selected_model_id(selected_model_id) {
            session = self
                .storage
                .set_session_selected_model(session.id, Some(selected_model_id))
                .await?;
        }

        // Create the conversation's root thread. Additional branch threads may
        // be added later under the same session.
        self.storage
            .create(NewThread {
                session_id: session.id,
                agent_id: "gateway".to_string(),
            })
            .await?;

        Ok(ConversationResponse {
            id: session.id,
            bench_id: session.bench_id,
            selected_model_id: session.selected_model_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
        })
    }

    /// List all conversations in the default bench, ordered by most-recent activity.
    pub async fn list_conversations(&self) -> Result<Vec<ConversationSummary>, GatewayError> {
        let bench = self.bench_store.get_or_create_default().await?;
        let sessions = self.storage.list_by_bench(bench.id).await?;

        let mut summaries = Vec::with_capacity(sessions.len());

        for session in sessions {
            let preview = self
                .storage
                .get_latest_for_session(session.id)
                .await?
                .map(|m| m.content);

            // Check whether any thread has an Interrupted turn.
            let has_interrupted_turn = self.has_interrupted_turn_in_session(session.id).await?;

            summaries.push(ConversationSummary {
                id: session.id,
                selected_model_id: session.selected_model_id,
                created_at: session.created_at,
                last_activity: session.updated_at,
                message_preview: preview,
                has_interrupted_turn,
            });
        }

        Ok(summaries)
    }

    /// Get a conversation with its full message history.
    pub async fn get_conversation(&self, id: Uuid) -> Result<ConversationDetail, GatewayError> {
        let session = self
            .storage
            .get_session(id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id })?;

        // Collect messages and tool calls across all threads for this session.
        let threads = self.storage.list_by_session(id).await?;

        let mut all_messages = Vec::new();
        let mut tool_calls_by_turn: HashMap<Uuid, Vec<ToolCallView>> = HashMap::new();
        let mut turn_state_by_turn: HashMap<Uuid, TurnState> = HashMap::new();

        for thread in &threads {
            let messages = self.storage.list_messages_by_thread(thread.id).await?;
            all_messages.extend(messages);

            let turns = self.storage.list_turns_by_thread(thread.id).await?;
            for turn in turns {
                let turn_id = turn.id;
                turn_state_by_turn.insert(turn_id, turn.state);
                let records = self.storage.list_tool_calls_by_turn(turn_id).await?;
                tool_calls_by_turn.insert(
                    turn_id,
                    records.into_iter().map(tool_call_record_to_view).collect(),
                );
            }
        }

        all_messages.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.thread_id.cmp(&b.thread_id))
                .then(a.position.cmp(&b.position))
        });

        let message_views = all_messages
            .into_iter()
            .map(|message| {
                let turn_id = message.turn_id;
                let turn_state = turn_id
                    .and_then(|message_turn_id| turn_state_by_turn.get(&message_turn_id).cloned());
                let tool_calls = if message.role == Role::Assistant {
                    turn_id
                        .and_then(|message_turn_id| {
                            tool_calls_by_turn.get(&message_turn_id).cloned()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                MessageView {
                    thread_id: message.thread_id,
                    turn_id,
                    role: message.role.to_string(),
                    content: message.content,
                    timestamp: message.created_at,
                    turn_state,
                    tool_calls,
                }
            })
            .collect();

        Ok(ConversationDetail {
            id: session.id,
            bench_id: session.bench_id,
            selected_model_id: session.selected_model_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
            messages: message_views,
        })
    }

    /// List sessions belonging to a specific bench.
    ///
    /// Returns `BenchNotFound` if no bench with the given ID exists.
    pub async fn list_sessions_by_bench(
        &self,
        bench_id: Uuid,
    ) -> Result<Vec<ConversationSummary>, GatewayError> {
        // Verify the bench exists before listing — gives a clean 404 rather
        // than silently returning an empty list for a non-existent bench.
        self.bench_store
            .get(bench_id)
            .await?
            .ok_or(GatewayError::BenchNotFound { id: bench_id })?;

        let sessions = self.storage.list_by_bench(bench_id).await?;

        let mut summaries = Vec::with_capacity(sessions.len());
        for session in sessions {
            let preview = self
                .storage
                .get_latest_for_session(session.id)
                .await?
                .map(|m| m.content);

            let has_interrupted_turn = self.has_interrupted_turn_in_session(session.id).await?;

            summaries.push(ConversationSummary {
                id: session.id,
                selected_model_id: session.selected_model_id,
                created_at: session.created_at,
                last_activity: session.updated_at,
                message_preview: preview,
                has_interrupted_turn,
            });
        }

        Ok(summaries)
    }

    /// Create a new session in the specified bench.
    ///
    /// Returns `BenchNotFound` if no bench with the given ID exists.
    pub async fn create_session_in_bench(
        &self,
        bench_id: Uuid,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_bench_with_model(bench_id, None)
            .await
    }

    pub async fn create_session_in_bench_with_model(
        &self,
        bench_id: Uuid,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_bench_for_agent(bench_id, "gateway", selected_model_id)
            .await
    }

    /// Create a new session in the specified bench, owned by a specific agent.
    ///
    /// The general session-creation path: interactive conversations use the
    /// default `"gateway"` agent via the wrappers above, while fresh-policy
    /// schedule runs create sessions for their schedule's target agent.
    pub async fn create_session_in_bench_for_agent(
        &self,
        bench_id: Uuid,
        agent_id: &str,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        // Verify the bench exists — return 404 rather than a FK constraint error.
        self.bench_store
            .get(bench_id)
            .await?
            .ok_or(GatewayError::BenchNotFound { id: bench_id })?;
        let active_workspace_attachment_id = self.default_workspace_for_bench(bench_id).await?;

        let mut session = self
            .storage
            .create_session(NewSession {
                bench_id,
                agent_id: agent_id.to_string(),
                metadata: None,
                active_workspace_attachment_id,
            })
            .await?;
        if let Some(selected_model_id) = normalise_selected_model_id(selected_model_id) {
            session = self
                .storage
                .set_session_selected_model(session.id, Some(selected_model_id))
                .await?;
        }

        // Create the default thread for the new session.
        self.storage
            .create(NewThread {
                session_id: session.id,
                agent_id: agent_id.to_string(),
            })
            .await?;

        Ok(ConversationResponse {
            id: session.id,
            bench_id: session.bench_id,
            selected_model_id: session.selected_model_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
        })
    }

    /// Delete a conversation and all its turns and messages (cascade).
    pub async fn delete_conversation(&self, id: Uuid) -> Result<(), GatewayError> {
        // Verify it exists before attempting deletion so callers get a clear 404.
        self.storage
            .get_session(id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id })?;

        self.storage.delete_session(id).await?;
        Ok(())
    }

    /// Create a branch thread from a message within an existing conversation.
    pub async fn create_branch(
        &self,
        session_id: Uuid,
        req: CreateConversationBranchRequest,
    ) -> Result<ConversationBranchResponse, GatewayError> {
        self.storage
            .get_session(session_id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id: session_id })?;

        let thread = self.storage.get(req.thread_id).await?.ok_or_else(|| {
            GatewayError::Storage(StorageError::NotFound {
                entity: "thread".into(),
                id: req.thread_id.to_string(),
            })
        })?;
        if thread.session_id != session_id {
            return Err(GatewayError::ValidationError(
                "thread does not belong to this conversation".into(),
            ));
        }

        let branch = self
            .storage
            .create_branch(req.thread_id, req.from_message_id)
            .await?;

        Ok(thread_branch_to_response(branch))
    }

    /// List branches created within a conversation.
    pub async fn list_branches(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ConversationBranchResponse>, GatewayError> {
        self.storage
            .get_session(session_id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id: session_id })?;

        self.storage
            .list_branches_by_session(session_id)
            .await
            .map(|branches| {
                branches
                    .into_iter()
                    .map(thread_branch_to_response)
                    .collect()
            })
            .map_err(GatewayError::Storage)
    }

    /// Fetch a single thread with its copied history and branch metadata.
    pub async fn get_thread_detail(
        &self,
        thread_id: Uuid,
    ) -> Result<ConversationThreadDetail, GatewayError> {
        let thread = self.storage.get(thread_id).await?.ok_or_else(|| {
            GatewayError::Storage(StorageError::NotFound {
                entity: "thread".into(),
                id: thread_id.to_string(),
            })
        })?;

        let messages = self.storage.list_messages_by_thread(thread.id).await?;
        let turns = self.storage.list_turns_by_thread(thread.id).await?;
        let mut turn_state_by_turn = HashMap::new();
        let mut tool_calls_by_turn = HashMap::new();
        for turn in turns {
            let turn_id = turn.id;
            turn_state_by_turn.insert(turn_id, turn.state);
            let records = self.storage.list_tool_calls_by_turn(turn_id).await?;
            tool_calls_by_turn.insert(
                turn_id,
                records.into_iter().map(tool_call_record_to_view).collect(),
            );
        }
        let branch = self.storage.get_branch(thread.id).await?;

        Ok(ConversationThreadDetail {
            id: thread.id,
            session_id: thread.session_id,
            agent_id: thread.agent_id,
            state: thread.state.to_string(),
            messages: messages
                .into_iter()
                .map(|message| {
                    let is_assistant = message.role == Role::Assistant;
                    MessageView {
                        thread_id: message.thread_id,
                        turn_id: message.turn_id,
                        role: message.role.to_string(),
                        content: message.content,
                        timestamp: message.created_at,
                        turn_state: message
                            .turn_id
                            .and_then(|turn_id| turn_state_by_turn.get(&turn_id).cloned()),
                        tool_calls: if is_assistant {
                            message
                                .turn_id
                                .and_then(|turn_id| tool_calls_by_turn.get(&turn_id).cloned())
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        },
                    }
                })
                .collect(),
            branch: branch.map(thread_branch_to_response),
        })
    }

    // ---------------------------------------------------------------------------
    // Private helpers
    // ---------------------------------------------------------------------------

    /// Returns true if any thread in the session has an Interrupted turn.
    async fn has_interrupted_turn_in_session(
        &self,
        session_id: Uuid,
    ) -> Result<bool, GatewayError> {
        let threads = self.storage.list_by_session(session_id).await?;
        for thread in &threads {
            let turns = self.storage.list_turns_by_thread(thread.id).await?;
            if turns.iter().any(|t| t.state == TurnState::Interrupted) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn default_workspace_for_bench(
        &self,
        bench_id: Uuid,
    ) -> Result<Option<Uuid>, GatewayError> {
        Ok(self
            .storage
            .list_workspace_attachments_by_bench(bench_id)
            .await?
            .into_iter()
            .find(|workspace| workspace.is_default && !workspace.is_missing)
            .map(|workspace| workspace.id))
    }
}

fn tool_call_record_to_view(record: swage_storage::model::ToolCallRecord) -> ToolCallView {
    let arguments =
        serde_json::from_str(&record.arguments).unwrap_or(JsonValue::String(record.arguments));
    let result = record
        .result
        .map(|text| serde_json::from_str(&text).unwrap_or(JsonValue::String(text)));

    ToolCallView {
        call_id: record.call_id,
        tool: record.tool,
        arguments,
        result,
        status: record.status,
        duration_ms: record.duration_ms.map(|ms| ms.max(0) as u64),
        truncated: record.truncated,
        total_bytes: record.total_bytes.map(|bytes| bytes.max(0) as u64),
        parent_call_id: record.parent_call_id,
    }
}

fn thread_branch_to_response(branch: ThreadBranch) -> ConversationBranchResponse {
    ConversationBranchResponse {
        child_thread_id: branch.child_thread_id,
        parent_thread_id: branch.parent_thread_id,
        branched_from_message_id: branch.branched_from_message_id,
        created_at: branch.created_at,
    }
}

fn normalise_selected_model_id(selected_model_id: Option<String>) -> Option<String> {
    selected_model_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
