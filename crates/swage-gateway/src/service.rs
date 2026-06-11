use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use serde_json::Value as JsonValue;
use swage_core::{GatewayError, Role, StorageError};
use swage_storage::{
    model::{
        BenchSlug, EdgeEvaluation, NewBench, NewSession, NewTeam, NewThread,
        NewWorkspaceAttachment, NodeExecution, NodeRetry, Team, TeamAgentDefinition,
        TeamTaskDefinition, ThreadBranch, TopologyRun, TurnState, WorkspaceAttachment,
    },
    store::{BenchStore, Storage},
};

use crate::{
    state::AppState,
    topology::{TopologyExecutionRequest, TopologyExecutor},
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

        // Create the default thread for the new session.
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

// ---------------------------------------------------------------------------
// Bench service — request/response types and service logic
// ---------------------------------------------------------------------------

/// Request body for creating a bench.
#[derive(Debug, Deserialize)]
pub struct CreateBenchRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Request body for partially updating a bench.
#[derive(Debug, Deserialize)]
pub struct UpdateBenchRequest {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// API response for a single bench.
#[derive(Debug, Serialize)]
pub struct BenchResponse {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}

/// API response for the list-benches endpoint.
#[derive(Debug, Serialize)]
pub struct BenchListResponse {
    pub benches: Vec<BenchResponse>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceAttachmentRequest {
    pub path: String,
    pub label: Option<String>,
    #[serde(default)]
    pub set_as_default: bool,
    #[serde(default)]
    pub active_session_id: Option<Uuid>,
    #[serde(default = "default_workspace_permissions")]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub is_trusted: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceTrustRequest {
    pub is_trusted: bool,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceAttachmentResponse {
    pub id: Uuid,
    pub bench_id: Uuid,
    pub label: String,
    pub path: String,
    pub canonical_path: String,
    pub is_missing: bool,
    pub is_default: bool,
    pub permissions: Vec<String>,
    pub is_trusted: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceAttachmentListResponse {
    pub workspaces: Vec<WorkspaceAttachmentResponse>,
}

fn default_workspace_permissions() -> Vec<String> {
    vec!["read".to_string()]
}

/// Derives a URL-safe slug from a bench name.
///
/// - Lowercases the name
/// - Replaces spaces with hyphens
/// - Strips any character that is neither ASCII alphanumeric nor a hyphen
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

fn validate_bench_slug(slug: &str) -> Result<BenchSlug, GatewayError> {
    BenchSlug::new(slug).map_err(|_| {
        GatewayError::ValidationError("name must contain at least one ASCII letter or digit".into())
    })
}

/// Returns the base directory for all bench data under the canonical Swage root.
fn benches_base_dir() -> Result<PathBuf, GatewayError> {
    swage_config::discovery::resolve_root()
        .map(|root| root.join("benches"))
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))
}

/// Mediates between HTTP bench handlers and storage.
#[derive(Clone)]
pub struct BenchService {
    pub(crate) bench_store: Arc<dyn BenchStore>,
}

impl BenchService {
    pub fn new(bench_store: Arc<dyn BenchStore>) -> Self {
        Self { bench_store }
    }

    /// Create a new bench.
    ///
    /// Validates the name is non-empty, generates a slug, checks name
    /// uniqueness, creates the bench directory structure, and stores the bench.
    pub async fn create_bench(
        &self,
        req: CreateBenchRequest,
    ) -> Result<BenchResponse, GatewayError> {
        // Validate non-empty name.
        if req.name.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "name must not be empty".into(),
            ));
        }

        // Check name uniqueness.
        if self.bench_store.get_by_name(&req.name).await?.is_some() {
            return Err(GatewayError::BenchConflict(format!(
                "a bench named '{}' already exists",
                req.name
            )));
        }

        let slug = validate_bench_slug(&slugify(&req.name))?;

        // Create the bench directory under the canonical Swage root.
        let base = benches_base_dir()?;
        let bench_dir = base.join(&slug);
        let memory_dir = bench_dir.join("memory");
        // Non-fatal: best-effort directory creation.
        if let Err(error) = std::fs::create_dir_all(&memory_dir) {
            tracing::warn!(
            %slug,
                path = %memory_dir.display(),
                %error,
                "failed to create bench memory directory"
            );
        }

        let bench = self
            .bench_store
            .create(NewBench {
                name: req.name,
                slug,
            })
            .await?;
        if !req.description.trim().is_empty() {
            return Ok(bench_to_response(
                self.bench_store
                    .update(
                        bench.id,
                        None,
                        Some(req.description.trim().to_string()),
                        None,
                    )
                    .await?,
            ));
        }

        Ok(bench_to_response(bench))
    }

    /// List all benches.
    pub async fn list_benches(&self) -> Result<BenchListResponse, GatewayError> {
        let benches = self.bench_store.list_all().await?;
        Ok(BenchListResponse {
            benches: benches.into_iter().map(bench_to_response).collect(),
        })
    }

    /// Get a bench by ID.
    pub async fn get_bench(&self, id: Uuid) -> Result<BenchResponse, GatewayError> {
        self.bench_store
            .get(id)
            .await?
            .map(bench_to_response)
            .ok_or(GatewayError::BenchNotFound { id })
    }

    /// Partially update a bench.
    ///
    /// Regenerates slug when name changes.
    pub async fn update_bench(
        &self,
        id: Uuid,
        req: UpdateBenchRequest,
    ) -> Result<BenchResponse, GatewayError> {
        // Validate name if provided.
        if let Some(ref name) = req.name
            && name.trim().is_empty()
        {
            return Err(GatewayError::ValidationError(
                "name must not be empty".into(),
            ));
        }

        // Derive the new slug from the new name, if name is being updated.
        let new_slug = match req.name.as_deref() {
            Some(name) => {
                let slug = slugify(name);
                Some(validate_bench_slug(&slug)?)
            }
            None => None,
        };

        let bench = self
            .bench_store
            .update(id, req.name, req.description, new_slug)
            .await
            .map_err(|e| match e {
                swage_core::StorageError::NotFound { id: ref eid, .. } => {
                    let parsed_id = eid.parse::<Uuid>().unwrap_or(id);
                    GatewayError::BenchNotFound { id: parsed_id }
                }
                other => GatewayError::Storage(other),
            })?;

        Ok(bench_to_response(bench))
    }

    /// Delete a bench.
    ///
    /// Returns 404 if not found. Returns a conflict error if called on the
    /// default bench. The bench directory on disk is preserved.
    pub async fn delete_bench(&self, id: Uuid) -> Result<(), GatewayError> {
        let found = self.bench_store.delete(id).await.map_err(|e| match e {
            swage_core::StorageError::ConstraintViolation(msg) => GatewayError::BenchConflict(msg),
            other => GatewayError::Storage(other),
        })?;

        if !found {
            return Err(GatewayError::BenchNotFound { id });
        }

        Ok(())
    }
}

fn bench_to_response(p: swage_storage::model::Bench) -> BenchResponse {
    BenchResponse {
        id: p.id,
        name: p.name,
        description: p.description,
        is_default: p.is_default,
        slug: p.slug.to_string(),
        created_at: p.created_at,
    }
}

#[derive(Clone)]
pub struct WorkspaceAttachmentService {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) bench_store: Arc<dyn BenchStore>,
}

impl WorkspaceAttachmentService {
    pub fn new(storage: Arc<dyn Storage>, bench_store: Arc<dyn BenchStore>) -> Self {
        Self {
            storage,
            bench_store,
        }
    }

    pub async fn list_workspaces(
        &self,
        bench_id: Uuid,
    ) -> Result<WorkspaceAttachmentListResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        let workspaces = self
            .storage
            .list_workspace_attachments_by_bench(bench_id)
            .await?;
        let mut refreshed = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            refreshed.push(self.refresh_workspace_missing_state(workspace).await?);
        }
        Ok(WorkspaceAttachmentListResponse {
            workspaces: refreshed.into_iter().map(workspace_to_response).collect(),
        })
    }

    pub async fn attach_workspace(
        &self,
        bench_id: Uuid,
        req: CreateWorkspaceAttachmentRequest,
    ) -> Result<WorkspaceAttachmentResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        let path = req.path.trim();
        if path.is_empty() {
            return Err(GatewayError::ValidationError(
                "workspace path must not be empty".into(),
            ));
        }
        if !Path::new(path).exists() {
            return Err(GatewayError::ValidationError(format!(
                "workspace path '{path}' does not exist on disk"
            )));
        }
        let canonical_path = std::fs::canonicalize(path).map_err(|error| {
            GatewayError::ValidationError(format!(
                "workspace path '{path}' could not be resolved: {error}"
            ))
        })?;
        let canonical_path = canonical_path.to_string_lossy().into_owned();

        let label = req
            .label
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                Path::new(&canonical_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("workspace")
                    .to_string()
            });
        let permissions = if req.permissions.is_empty() {
            default_workspace_permissions()
        } else {
            req.permissions
        };

        let workspace = self
            .storage
            .create_workspace_attachment(NewWorkspaceAttachment {
                bench_id,
                label,
                path: canonical_path,
                permissions,
                is_trusted: req.is_trusted,
            })
            .await?;
        if req.set_as_default {
            self.storage
                .set_bench_default_workspace(bench_id, Some(workspace.id))
                .await?;
        }
        if let Some(session_id) = req.active_session_id {
            self.ensure_session_belongs_to_bench(bench_id, session_id)
                .await?;
            self.storage
                .set_session_active_workspace(session_id, Some(workspace.id))
                .await?;
        }
        let workspace = self
            .storage
            .get_workspace_attachment(workspace.id)
            .await?
            .ok_or_else(|| GatewayError::ValidationError("created workspace disappeared".into()))?;
        Ok(workspace_to_response(workspace))
    }

    pub async fn set_trust(
        &self,
        bench_id: Uuid,
        workspace_id: Uuid,
        req: UpdateWorkspaceTrustRequest,
    ) -> Result<WorkspaceAttachmentResponse, GatewayError> {
        self.ensure_workspace_belongs_to_bench(bench_id, workspace_id)
            .await?;
        let workspace = self
            .storage
            .set_workspace_trust(workspace_id, req.is_trusted)
            .await?;
        Ok(workspace_to_response(workspace))
    }

    pub async fn detach_workspace(
        &self,
        bench_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<(), GatewayError> {
        self.ensure_workspace_belongs_to_bench(bench_id, workspace_id)
            .await?;
        self.storage
            .delete_workspace_attachment(workspace_id)
            .await?;
        Ok(())
    }

    async fn ensure_bench_exists(&self, bench_id: Uuid) -> Result<(), GatewayError> {
        self.bench_store
            .get(bench_id)
            .await?
            .map(|_| ())
            .ok_or(GatewayError::BenchNotFound { id: bench_id })
    }

    async fn ensure_workspace_belongs_to_bench(
        &self,
        bench_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<(), GatewayError> {
        let workspace = self
            .storage
            .get_workspace_attachment(workspace_id)
            .await?
            .ok_or_else(|| {
                GatewayError::ValidationError(format!(
                    "workspace attachment {workspace_id} not found"
                ))
            })?;
        if workspace.bench_id != bench_id {
            return Err(GatewayError::ValidationError(format!(
                "workspace attachment {workspace_id} does not belong to bench {bench_id}"
            )));
        }
        Ok(())
    }

    async fn ensure_session_belongs_to_bench(
        &self,
        bench_id: Uuid,
        session_id: Uuid,
    ) -> Result<(), GatewayError> {
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
        if session.bench_id != bench_id {
            return Err(GatewayError::ValidationError(format!(
                "session {session_id} does not belong to bench {bench_id}"
            )));
        }
        Ok(())
    }

    async fn refresh_workspace_missing_state(
        &self,
        workspace: WorkspaceAttachment,
    ) -> Result<WorkspaceAttachment, GatewayError> {
        let check_path = if workspace.canonical_path.is_empty() {
            &workspace.path
        } else {
            &workspace.canonical_path
        };
        let is_missing = !Path::new(check_path).exists();
        if workspace.is_missing == is_missing {
            return Ok(workspace);
        }
        Ok(self
            .storage
            .set_workspace_missing_state(workspace.id, is_missing)
            .await?)
    }
}

fn workspace_to_response(workspace: WorkspaceAttachment) -> WorkspaceAttachmentResponse {
    WorkspaceAttachmentResponse {
        id: workspace.id,
        bench_id: workspace.bench_id,
        label: workspace.label,
        path: workspace.path,
        canonical_path: workspace.canonical_path,
        is_missing: workspace.is_missing,
        is_default: workspace.is_default,
        permissions: workspace.permissions,
        is_trusted: workspace.is_trusted,
        created_at: workspace.created_at,
    }
}

#[cfg(test)]
mod bench_slug_tests {
    use super::{GatewayError, slugify, validate_bench_slug};

    #[test]
    fn punctuation_only_name_is_rejected_after_slugify() {
        let err = validate_bench_slug(&slugify("!!! /// ___"))
            .expect_err("punctuation-only names should be rejected");
        assert!(matches!(err, GatewayError::ValidationError(_)));
        assert!(
            err.to_string()
                .contains("at least one ASCII letter or digit")
        );
    }
}

// ---------------------------------------------------------------------------
// Team service — request/response types and service logic
// ---------------------------------------------------------------------------

/// Request body for creating a team within a bench.
#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub entry_agent_id: String,
    #[serde(default)]
    pub agents: Vec<TeamAgentPayload>,
    #[serde(default)]
    pub tasks: Vec<TeamTaskPayload>,
}

/// Request body for partially updating a team.
#[derive(Debug, Deserialize)]
pub struct UpdateTeamRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub entry_agent_id: Option<String>,
    pub agents: Option<Vec<TeamAgentPayload>>,
    pub tasks: Option<Vec<TeamTaskPayload>>,
    #[serde(default)]
    pub members: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeamAgentPayload {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeamTaskPayload {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub output_schema: Option<JsonValue>,
    #[serde(default)]
    pub max_output_retries: Option<u32>,
    #[serde(default)]
    pub tool_allow: Option<Vec<String>>,
    #[serde(default)]
    pub tool_deny: Option<Vec<String>>,
}

/// API response for a single team.
#[derive(Debug, Serialize, Deserialize)]
pub struct TeamResponse {
    pub id: Uuid,
    pub bench_id: Uuid,
    pub name: String,
    pub description: String,
    pub entry_agent_id: String,
    pub agents: Vec<TeamAgentPayload>,
    pub tasks: Vec<TeamTaskPayload>,
    pub members: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// API response for the list-teams endpoint.
#[derive(Debug, Serialize)]
pub struct TeamListResponse {
    pub teams: Vec<TeamResponse>,
}

/// Mediates between HTTP team handlers and storage.
#[derive(Clone)]
pub struct TeamService {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) bench_store: Arc<dyn BenchStore>,
}

impl TeamService {
    pub fn new(storage: Arc<dyn Storage>, bench_store: Arc<dyn BenchStore>) -> Self {
        Self {
            storage,
            bench_store,
        }
    }

    pub async fn create_team(
        &self,
        bench_id: Uuid,
        req: CreateTeamRequest,
    ) -> Result<TeamResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        validate_team_name(&req.name)?;
        validate_agent_id(&req.entry_agent_id, "entry_agent_id")?;

        if self
            .storage
            .get_team_by_name(bench_id, &req.name)
            .await?
            .is_some()
        {
            return Err(GatewayError::Storage(StorageError::ConstraintViolation(
                format!(
                    "a team named '{}' already exists in bench {bench_id}",
                    req.name
                ),
            )));
        }

        let agents = normalise_agents(req.agents, &req.entry_agent_id)?;
        let tasks = normalise_tasks(req.tasks)?;
        let team = self
            .storage
            .create_team(NewTeam {
                bench_id,
                name: req.name,
                description: req.description,
                entry_agent_id: req.entry_agent_id,
                agents,
                tasks,
            })
            .await?;

        Ok(team_to_response(team))
    }

    pub async fn list_teams(&self, bench_id: Uuid) -> Result<TeamListResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        let teams = self.storage.list_teams_by_bench(bench_id).await?;
        Ok(TeamListResponse {
            teams: teams.into_iter().map(team_to_response).collect(),
        })
    }

    pub async fn get_team(&self, id: Uuid) -> Result<TeamResponse, GatewayError> {
        self.storage
            .get_team(id)
            .await?
            .map(team_to_response)
            .ok_or_else(|| team_not_found(id))
    }

    pub async fn update_team(
        &self,
        id: Uuid,
        req: UpdateTeamRequest,
    ) -> Result<TeamResponse, GatewayError> {
        let UpdateTeamRequest {
            name,
            description,
            entry_agent_id,
            agents,
            tasks,
            members,
        } = req;

        let existing = self
            .storage
            .get_team(id)
            .await?
            .ok_or_else(|| team_not_found(id))?;

        if let Some(ref name) = name {
            validate_team_name(name)?;
            if let Some(conflict) = self
                .storage
                .get_team_by_name(existing.bench_id, name)
                .await?
                && conflict.id != id
            {
                return Err(GatewayError::Storage(StorageError::ConstraintViolation(
                    format!(
                        "a team named '{}' already exists in bench {}",
                        name, existing.bench_id
                    ),
                )));
            }
        }

        if let Some(ref entry_agent_id) = entry_agent_id {
            validate_agent_id(entry_agent_id, "entry_agent_id")?;
        }

        let resolved_entry_agent_id = entry_agent_id
            .clone()
            .unwrap_or_else(|| existing.entry_agent_id.clone());
        let agents = match (agents, members) {
            (Some(_agents), Some(_)) => {
                return Err(GatewayError::ValidationError(
                    "provide agents or members when updating a team, not both".into(),
                ));
            }
            (Some(agents), None) => Some(normalise_agents(agents, &resolved_entry_agent_id)?),
            (None, Some(member_ids)) => Some(expand_members_into_agents(
                member_ids,
                &resolved_entry_agent_id,
                &existing.agents,
            )?),
            (None, None) if resolved_entry_agent_id != existing.entry_agent_id => Some(
                ensure_entry_agent_present(existing.agents.clone(), &resolved_entry_agent_id)?,
            ),
            (None, None) => None,
        };
        let tasks = tasks.map(normalise_tasks).transpose()?;

        let team = self
            .storage
            .update_team(id, name, description, entry_agent_id, agents, tasks)
            .await?;

        Ok(team_to_response(team))
    }

    pub async fn delete_team(&self, id: Uuid) -> Result<(), GatewayError> {
        if !self.storage.delete_team(id).await? {
            return Err(team_not_found(id));
        }

        Ok(())
    }

    async fn ensure_bench_exists(&self, bench_id: Uuid) -> Result<(), GatewayError> {
        self.bench_store
            .get(bench_id)
            .await?
            .ok_or(GatewayError::BenchNotFound { id: bench_id })?;
        Ok(())
    }
}

fn validate_team_name(name: &str) -> Result<(), GatewayError> {
    if name.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "name must not be empty".into(),
        ));
    }
    Ok(())
}

fn validate_agent_id(agent_id: &str, field_name: &str) -> Result<(), GatewayError> {
    if agent_id.trim().is_empty() {
        return Err(GatewayError::ValidationError(format!(
            "{field_name} must not be empty"
        )));
    }
    Ok(())
}

fn normalise_agents(
    agents: Vec<TeamAgentPayload>,
    entry_agent_id: &str,
) -> Result<Vec<TeamAgentDefinition>, GatewayError> {
    let mut normalised = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let entry_agent_id = entry_agent_id.trim();
    if entry_agent_id.is_empty() {
        return Err(GatewayError::ValidationError(
            "entry_agent_id must not be empty".into(),
        ));
    }

    for agent in agents {
        validate_agent_id(&agent.id, "agents[].id")?;
        let trimmed_id = agent.id.trim().to_string();
        if !seen.insert(trimmed_id.clone()) {
            return Err(GatewayError::ValidationError(format!(
                "agents must not contain duplicate IDs: {trimmed_id}"
            )));
        }
        normalised.push(TeamAgentDefinition {
            id: trimmed_id,
            display_name: agent.display_name.trim().to_string(),
            description: agent.description.trim().to_string(),
            instructions: agent.instructions.trim().to_string(),
            model: agent.model.as_ref().map(|model| model.trim().to_string()),
        });
    }

    if !normalised.iter().any(|agent| agent.id == entry_agent_id) {
        return Err(GatewayError::ValidationError(format!(
            "entry_agent_id '{entry_agent_id}' must match one of the defined team agents"
        )));
    }

    Ok(normalised)
}

fn ensure_entry_agent_present(
    agents: Vec<TeamAgentDefinition>,
    entry_agent_id: &str,
) -> Result<Vec<TeamAgentDefinition>, GatewayError> {
    if agents.iter().any(|agent| agent.id == entry_agent_id) {
        Ok(agents)
    } else {
        Err(GatewayError::ValidationError(format!(
            "entry_agent_id '{entry_agent_id}' must match one of the defined team agents"
        )))
    }
}

fn expand_members_into_agents(
    members: Vec<String>,
    entry_agent_id: &str,
    existing_agents: &[TeamAgentDefinition],
) -> Result<Vec<TeamAgentDefinition>, GatewayError> {
    let mut normalised = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let entry_agent_id = entry_agent_id.trim();
    if entry_agent_id.is_empty() {
        return Err(GatewayError::ValidationError(
            "entry_agent_id must not be empty".into(),
        ));
    }

    if let Some(existing) = existing_agents
        .iter()
        .find(|agent| agent.id == entry_agent_id)
    {
        normalised.push(existing.clone());
        seen.insert(existing.id.clone());
    } else {
        normalised.push(TeamAgentDefinition {
            id: entry_agent_id.to_string(),
            display_name: entry_agent_id.to_string(),
            description: String::new(),
            instructions: String::new(),
            model: None,
        });
        seen.insert(entry_agent_id.to_string());
    }

    for member in members {
        let member = member.trim();
        if member.is_empty() {
            return Err(GatewayError::ValidationError(
                "members must not contain empty agent IDs".into(),
            ));
        }
        if seen.insert(member.to_string()) {
            let existing = existing_agents.iter().find(|agent| agent.id == member);
            normalised.push(existing.cloned().unwrap_or_else(|| TeamAgentDefinition {
                id: member.to_string(),
                display_name: member.to_string(),
                description: String::new(),
                instructions: String::new(),
                model: None,
            }));
        }
    }

    Ok(normalised)
}

fn normalise_tasks(tasks: Vec<TeamTaskPayload>) -> Result<Vec<TeamTaskDefinition>, GatewayError> {
    let mut normalised = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for task in tasks {
        let name = task.name.trim();
        if name.is_empty() {
            return Err(GatewayError::ValidationError(
                "tasks must not contain empty names".into(),
            ));
        }
        if !seen.insert(name.to_string()) {
            return Err(GatewayError::ValidationError(format!(
                "tasks must not contain duplicate names: {name}"
            )));
        }
        normalised.push(TeamTaskDefinition {
            name: name.to_string(),
            description: task.description.trim().to_string(),
            instructions: task.instructions.trim().to_string(),
            model: task.model.as_ref().map(|model| model.trim().to_string()),
            output_schema: task.output_schema,
            max_output_retries: task.max_output_retries,
            tool_allow: task.tool_allow,
            tool_deny: task.tool_deny,
        });
    }

    Ok(normalised)
}

fn team_not_found(id: Uuid) -> GatewayError {
    GatewayError::Storage(StorageError::NotFound {
        entity: "team".into(),
        id: id.to_string(),
    })
}

fn team_to_response(team: Team) -> TeamResponse {
    TeamResponse {
        id: team.id,
        bench_id: team.bench_id,
        name: team.name,
        description: team.description,
        entry_agent_id: team.entry_agent_id,
        agents: team
            .agents
            .into_iter()
            .map(|agent| TeamAgentPayload {
                id: agent.id,
                display_name: agent.display_name,
                description: agent.description,
                instructions: agent.instructions,
                model: agent.model,
            })
            .collect(),
        tasks: team
            .tasks
            .into_iter()
            .map(|task| TeamTaskPayload {
                name: task.name,
                description: task.description,
                instructions: task.instructions,
                model: task.model,
                output_schema: task.output_schema,
                max_output_retries: task.max_output_retries,
                tool_allow: task.tool_allow,
                tool_deny: task.tool_deny,
            })
            .collect(),
        members: team.members,
        created_at: team.created_at,
        updated_at: team.updated_at,
    }
}

// ---------------------------------------------------------------------------
// Topology run service
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateTopologyRunRequest {
    #[serde(default)]
    pub topology_source: String,
    #[serde(default)]
    pub flow_name: Option<String>,
    pub message: String,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct TopologyRunResponse {
    pub id: Uuid,
    pub team_id: Uuid,
    pub session_id: Uuid,
    pub state: String,
    pub final_output: Option<JsonValue>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct NodeExecutionResponse {
    pub id: Uuid,
    pub topology_run_id: Uuid,
    pub node_id: String,
    pub node_kind: String,
    pub agent_id: Option<String>,
    pub task_name: Option<String>,
    pub state: String,
    pub input_json: Option<JsonValue>,
    pub output_json: Option<JsonValue>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct EdgeEvaluationResponse {
    pub id: Uuid,
    pub topology_run_id: Uuid,
    pub source_node_execution_id: Uuid,
    pub edge_id: String,
    pub target_node_id: String,
    pub condition_expression: Option<String>,
    pub matched: bool,
    pub payload_json: Option<JsonValue>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct NodeRetryResponse {
    pub id: Uuid,
    pub node_execution_id: Uuid,
    pub attempt_number: i32,
    pub failure_kind: String,
    pub error_message: String,
    pub recovery_guidance: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct TopologyRunDetailResponse {
    pub run: TopologyRunResponse,
    pub nodes: Vec<NodeExecutionResponse>,
    pub edges: Vec<EdgeEvaluationResponse>,
    pub retries: Vec<NodeRetryResponse>,
}

#[derive(Clone)]
pub struct TopologyRunService {
    pub(crate) state: AppState,
}

impl TopologyRunService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub async fn create_run(
        &self,
        team_id: Uuid,
        req: CreateTopologyRunRequest,
    ) -> Result<TopologyRunDetailResponse, GatewayError> {
        if req.topology_source.trim().is_empty() && req.flow_name.is_none() {
            return Err(GatewayError::ValidationError(
                "topology_source or flow_name must be provided".into(),
            ));
        }
        if req.message.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "message must not be empty".into(),
            ));
        }

        let team = self.state.storage.get_team(team_id).await?.ok_or_else(|| {
            GatewayError::Storage(StorageError::NotFound {
                entity: "team".into(),
                id: team_id.to_string(),
            })
        })?;

        let mut known_agents: std::collections::HashSet<String> =
            team.agents.iter().map(|agent| agent.id.clone()).collect();
        known_agents.insert(team.entry_agent_id.clone());
        let catalog = swage_topology::TopologyEntityCatalog {
            known_agents,
            known_tasks: team.tasks.iter().map(|task| task.name.clone()).collect(),
            known_handlers: ["identity", "collect-results"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        };

        let graph = if let Some(flow_name) = req.flow_name.as_deref() {
            crate::authoring_definition::load_topology_flow_graph(
                &self.state,
                team.bench_id,
                flow_name,
            )
            .await?
        } else {
            swage_python::topology::execute_topology_source_with_config(
                &self.state.config.tools.python_execute,
                &req.topology_source,
                &format!("team-run:{team_id}"),
                std::time::Duration::from_secs(self.state.config.context.timeout),
                catalog.clone(),
            )
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?
        };
        swage_topology::validate_graph(&graph, &catalog).map_err(|problems| {
            GatewayError::ValidationError(
                problems
                    .into_iter()
                    .map(|problem| problem.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;

        let result = TopologyExecutor::new(self.state.clone())
            .execute(TopologyExecutionRequest {
                team,
                graph,
                initial_input: serde_json::json!({ "message": req.message }),
                session_id: req.session_id,
                source_turn_id: None,
            })
            .await
            .map_err(|error| match error {
                crate::topology::TopologyEngineError::Storage(storage_error) => {
                    GatewayError::Storage(storage_error)
                }
                crate::topology::TopologyEngineError::TeamBenchMissing { bench_id, .. } => {
                    GatewayError::BenchNotFound { id: bench_id }
                }
                crate::topology::TopologyEngineError::SessionMissing { session_id } => {
                    GatewayError::ConversationNotFound { id: session_id }
                }
                other => GatewayError::ValidationError(other.to_string()),
            })?;

        self.get_run(result.topology_run_id).await
    }

    pub async fn get_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<TopologyRunDetailResponse, GatewayError> {
        let run = self
            .state
            .storage
            .get_topology_run(topology_run_id)
            .await?
            .ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "topology_run".into(),
                    id: topology_run_id.to_string(),
                })
            })?;
        let nodes = self
            .state
            .storage
            .list_node_executions_by_run(topology_run_id)
            .await?;
        let edges = self
            .state
            .storage
            .list_edge_evaluations_by_run(topology_run_id)
            .await?;
        let mut retries = Vec::new();
        for node in &nodes {
            retries.extend(self.state.storage.list_node_retries(node.id).await?);
        }

        Ok(TopologyRunDetailResponse {
            run: topology_run_to_response(run),
            nodes: nodes.into_iter().map(node_execution_to_response).collect(),
            edges: edges.into_iter().map(edge_evaluation_to_response).collect(),
            retries: retries.into_iter().map(node_retry_to_response).collect(),
        })
    }

    pub async fn list_runs(&self, team_id: Uuid) -> Result<Vec<TopologyRunResponse>, GatewayError> {
        self.state
            .storage
            .list_topology_runs_by_team(team_id)
            .await
            .map(|runs| runs.into_iter().map(topology_run_to_response).collect())
            .map_err(GatewayError::Storage)
    }
}

fn topology_run_to_response(run: TopologyRun) -> TopologyRunResponse {
    TopologyRunResponse {
        id: run.id,
        team_id: run.team_id,
        session_id: run.session_id,
        state: run.state.to_string(),
        final_output: run.final_output,
        error_message: run.error_message,
        created_at: run.created_at,
        completed_at: run.completed_at,
    }
}

fn node_execution_to_response(node: NodeExecution) -> NodeExecutionResponse {
    NodeExecutionResponse {
        id: node.id,
        topology_run_id: node.topology_run_id,
        node_id: node.node_id,
        node_kind: node.node_kind.to_string(),
        agent_id: node.agent_id,
        task_name: node.task_name,
        state: node.state.to_string(),
        input_json: node.input_json,
        output_json: node.output_json,
        error_message: node.error_message,
        created_at: node.created_at,
        completed_at: node.completed_at,
    }
}

fn edge_evaluation_to_response(edge: EdgeEvaluation) -> EdgeEvaluationResponse {
    EdgeEvaluationResponse {
        id: edge.id,
        topology_run_id: edge.topology_run_id,
        source_node_execution_id: edge.source_node_execution_id,
        edge_id: edge.edge_id,
        target_node_id: edge.target_node_id,
        condition_expression: edge.condition_expression,
        matched: edge.matched,
        payload_json: edge.payload_json,
        created_at: edge.created_at,
    }
}

fn node_retry_to_response(retry: NodeRetry) -> NodeRetryResponse {
    NodeRetryResponse {
        id: retry.id,
        node_execution_id: retry.node_execution_id,
        attempt_number: retry.attempt_number,
        failure_kind: retry.failure_kind,
        error_message: retry.error_message,
        recovery_guidance: retry.recovery_guidance,
        created_at: retry.created_at,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use swage_core::{compact_tool_arguments_for_model, compact_tool_result_for_model};
    use swage_storage::model::{NewMessage, NewToolCall, NewTurn, TurnState};
    use swage_storage::sqlite::SqliteStorage;

    use super::*;
    use crate::turn::tests::MockStorage;

    fn make_service() -> ConversationService {
        let storage = Arc::new(MockStorage::new());
        ConversationService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn BenchStore>,
        )
    }

    fn model_arguments(raw: &str) -> String {
        let value = serde_json::from_str(raw).unwrap_or_else(|_| JsonValue::String(raw.into()));
        compact_tool_arguments_for_model(&value)
    }

    fn model_result(raw: &str) -> String {
        compact_tool_result_for_model(raw, false)
    }

    async fn make_workspace_service() -> (WorkspaceAttachmentService, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite should initialise"));
        let bench = storage
            .get_or_create_default()
            .await
            .expect("default bench should exist");
        let service = WorkspaceAttachmentService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn BenchStore>,
        );
        (service, bench.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_persists_canonical_path() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        std::fs::create_dir(&workspace).expect("workspace dir");
        let non_canonical = workspace.join("..").join("actual");

        let response = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: non_canonical.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");

        assert_eq!(
            response.path,
            workspace
                .canonicalize()
                .expect("canonical workspace")
                .to_string_lossy()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_workspaces_persists_missing_folder_state() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let attached = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");
        assert!(!attached.is_missing);

        std::fs::remove_dir(&workspace).expect("workspace dir should be removable");

        let list = service
            .list_workspaces(bench_id)
            .await
            .expect("workspaces should list");
        assert_eq!(list.workspaces.len(), 1);
        assert!(list.workspaces[0].is_missing);

        let stored = service
            .storage
            .get_workspace_attachment(attached.id)
            .await
            .expect("workspace lookup")
            .expect("workspace should still exist");
        assert!(stored.is_missing);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_can_become_bench_default() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("default");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let response = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: true,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");

        assert!(response.is_default);
        let stored = service
            .storage
            .get_workspace_attachment(response.id)
            .await
            .expect("workspace lookup")
            .expect("workspace should exist");
        assert!(stored.is_default);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_can_become_session_active_workspace() {
        let (service, bench_id) = make_workspace_service().await;
        let session = service
            .storage
            .create_session(NewSession {
                bench_id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session should create");
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("session");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let response = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: Some(session.id),
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");

        let session = service
            .storage
            .get_session(session.id)
            .await
            .expect("session lookup")
            .expect("session should exist");
        assert_eq!(session.active_workspace_attachment_id, Some(response.id));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_rejects_duplicate_canonical_path() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        let symlink = temp.path().join("alias");
        std::fs::create_dir(&workspace).expect("workspace dir");
        std::os::unix::fs::symlink(&workspace, &symlink).expect("workspace symlink");

        service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("first attachment should succeed");

        let error = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: symlink.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect_err("canonical duplicate should be rejected");

        assert!(matches!(
            error,
            GatewayError::Storage(StorageError::ConstraintViolation(_))
        ));
    }

    // -----------------------------------------------------------------------
    // create_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_conversation_returns_valid_response() {
        let svc = make_service();

        let resp = svc
            .create_conversation()
            .await
            .expect("create should succeed");

        // Non-nil UUID expected.
        assert_ne!(resp.id, Uuid::nil());
        assert_ne!(resp.bench_id, Uuid::nil());
    }

    #[tokio::test]
    async fn create_conversation_multiple_times_yields_distinct_ids() {
        let svc = make_service();

        let a = svc.create_conversation().await.unwrap();
        let b = svc.create_conversation().await.unwrap();

        // Both conversations should belong to the same default bench.
        assert_eq!(a.bench_id, b.bench_id);
        // But have distinct session IDs.
        assert_ne!(a.id, b.id);
    }

    // -----------------------------------------------------------------------
    // list_conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_conversations_includes_all_created_sessions() {
        let svc = make_service();
        svc.create_conversation().await.unwrap();
        svc.create_conversation().await.unwrap();

        let list = svc.list_conversations().await.expect("list should succeed");

        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn list_conversations_shows_message_preview_and_interrupted_flag() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        // Get the default thread created alongside the session.
        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        // Insert a turn and a message via the underlying storage.
        let turn = svc
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

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::User,
                content: "Hello!".to_string(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        // Mark the turn as Interrupted so `has_interrupted_turn` is set.
        svc.storage
            .update_turn_state(turn.id, TurnState::Interrupted)
            .await
            .unwrap();

        let list = svc.list_conversations().await.unwrap();
        let summary = list.iter().find(|s| s.id == session_id).unwrap();

        assert_eq!(summary.message_preview.as_deref(), Some("Hello!"));
        assert!(summary.has_interrupted_turn);
    }

    #[tokio::test]
    async fn list_conversations_no_interrupted_turn_flag_when_completed() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
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
        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let list = svc.list_conversations().await.unwrap();
        let summary = list.iter().find(|s| s.id == session_id).unwrap();

        assert!(!summary.has_interrupted_turn);
    }

    // -----------------------------------------------------------------------
    // get_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversation_returns_messages_with_turn_states() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
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

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::User,
                content: "Hi".to_string(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();
        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "Hello!".to_string(),
                position: 1,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();

        assert_eq!(detail.messages.len(), 2);
        assert_eq!(detail.messages[0].role, "user");
        assert_eq!(detail.messages[0].content, "Hi");
        assert_eq!(detail.messages[0].turn_state, Some(TurnState::Completed));
        assert_eq!(detail.messages[1].role, "assistant");
        assert_eq!(detail.messages[1].content, "Hello!");
    }

    #[tokio::test]
    async fn get_conversation_returns_not_found_for_missing_id() {
        let svc = make_service();
        let missing = Uuid::new_v4();

        let err = svc.get_conversation(missing).await.unwrap_err();

        assert!(
            matches!(err, GatewayError::ConversationNotFound { id } if id == missing),
            "expected ConversationNotFound, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // delete_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_conversation_then_get_returns_not_found() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();

        svc.delete_conversation(resp.id)
            .await
            .expect("delete should succeed");

        let err = svc.get_conversation(resp.id).await.unwrap_err();
        assert!(matches!(err, GatewayError::ConversationNotFound { .. }));
    }

    #[tokio::test]
    async fn delete_non_existent_conversation_returns_not_found() {
        let svc = make_service();
        let missing = Uuid::new_v4();

        let err = svc.delete_conversation(missing).await.unwrap_err();
        assert!(matches!(err, GatewayError::ConversationNotFound { .. }));
    }

    // -----------------------------------------------------------------------
    // tool_calls in get_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversation_attaches_tool_calls_to_assistant_messages() {
        use swage_storage::model::NewToolCall;

        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
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

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::User,
                content: "list files".into(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "I'll run ls for you.".into(),
                position: 1,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                call_id: "toolu_01".into(),
                tool: "shell".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
                model_arguments: model_arguments(r#"{"cmd":"ls"}"#),
                result: Some("file1\nfile2".into()),
                model_result: Some(model_result("file1\nfile2")),
                status: Some("success".into()),
                duration_ms: Some(10),
                truncated: false,
                total_bytes: Some(12),
                provider_round: 1,
                position: 0,
                parent_call_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();
        assert_eq!(detail.messages.len(), 2);

        // User message should have no tool calls.
        let user_msg = &detail.messages[0];
        assert_eq!(user_msg.role, "user");
        assert!(
            user_msg.tool_calls.is_empty(),
            "user message should have no tool calls"
        );

        // Assistant message should carry the tool call.
        let asst_msg = &detail.messages[1];
        assert_eq!(asst_msg.role, "assistant");
        assert_eq!(asst_msg.tool_calls.len(), 1);
        let tc = &asst_msg.tool_calls[0];
        assert_eq!(tc.call_id, "toolu_01");
        assert_eq!(tc.tool, "shell");
        assert_eq!(tc.status.as_deref(), Some("success"));
        assert!(!tc.truncated);
    }

    #[tokio::test]
    async fn get_conversation_tool_call_view_carries_parent_call_id() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
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

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "Ran python".into(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        // Native python_execute call.
        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                call_id: "py_01".into(),
                tool: "python_execute".into(),
                arguments: r#"{"code":"1+1"}"#.into(),
                model_arguments: model_arguments(r#"{"code":"1+1"}"#),
                result: None,
                model_result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                provider_round: 1,
                position: 0,
                parent_call_id: None,
            })
            .await
            .unwrap();

        // Sub-tool dispatched from within python_execute.
        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                call_id: "sub_01".into(),
                tool: "shell".into(),
                arguments: r#"{"cmd":"echo hi"}"#.into(),
                model_arguments: model_arguments(r#"{"cmd":"echo hi"}"#),
                result: Some("hi".into()),
                model_result: Some(model_result("hi")),
                status: Some("success".into()),
                duration_ms: Some(5),
                truncated: false,
                total_bytes: Some(2),
                provider_round: 0,
                position: 1,
                parent_call_id: Some("py_01".into()),
            })
            .await
            .unwrap();

        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();
        let tool_calls = &detail.messages[0].tool_calls;
        assert_eq!(tool_calls.len(), 2);

        let native = &tool_calls[0];
        assert_eq!(native.call_id, "py_01");
        assert!(
            native.parent_call_id.is_none(),
            "native call should have no parent"
        );

        let sub = &tool_calls[1];
        assert_eq!(sub.call_id, "sub_01");
        assert_eq!(sub.parent_call_id.as_deref(), Some("py_01"));
    }

    #[tokio::test]
    async fn get_conversation_tool_calls_empty_when_none_recorded() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
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
        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "Hello".into(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();
        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();
        assert_eq!(detail.messages[0].tool_calls.len(), 0);
    }
}
