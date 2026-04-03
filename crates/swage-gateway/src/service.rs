use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use serde_json::Value as JsonValue;
use swage_core::{GatewayError, StorageError};
use swage_storage::{
    model::{
        EdgeEvaluation, NewProject, NewSession, NewTeam, NewThread, NodeExecution, NodeRetry, Team,
        Thread, ThreadBranch, TopologyRun, TurnState,
    },
    store::{ProjectStore, Storage},
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
    pub project_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

/// Compact representation of a conversation, used in list responses.
#[derive(Debug, Serialize)]
pub struct ConversationSummary {
    pub id: Uuid,
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
    pub project_id: Uuid,
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
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Latest turn state observed while building this conversation view.
    ///
    /// Today this value is shared across every returned message in the session,
    /// rather than being attributed per message or per thread.
    pub turn_state: TurnState,
    /// Tool calls that occurred in this message's turn.
    ///
    /// Populated for assistant messages; empty for user messages.
    /// Always present (empty array) so the frontend schema is consistent.
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
    pub(crate) project_store: Arc<dyn ProjectStore>,
}

impl ConversationService {
    pub fn new(storage: Arc<dyn Storage>, project_store: Arc<dyn ProjectStore>) -> Self {
        Self {
            storage,
            project_store,
        }
    }

    /// Create a new conversation in the default project.
    ///
    /// Also creates a default thread for the session so that process_turn has
    /// a thread_id to attach turns and messages to.
    pub async fn create_conversation(&self) -> Result<ConversationResponse, GatewayError> {
        let project = self.project_store.get_or_create_default().await?;
        let session = self
            .storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await?;

        // Create the default thread. The gateway uses one thread per session.
        self.storage
            .create(NewThread {
                session_id: session.id,
                agent_id: "gateway".to_string(),
            })
            .await?;

        Ok(ConversationResponse {
            id: session.id,
            project_id: session.project_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
        })
    }

    /// List all conversations in the default project, ordered by most-recent activity.
    pub async fn list_conversations(&self) -> Result<Vec<ConversationSummary>, GatewayError> {
        let project = self.project_store.get_or_create_default().await?;
        let sessions = self.storage.list_by_project(project.id).await?;

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
        // The gateway uses one thread per session; this handles the general case.
        let threads = self.storage.list_by_session(id).await?;

        let mut all_messages = Vec::new();
        // All tool calls across all turns in all threads.
        let mut all_tool_call_views: Vec<ToolCallView> = Vec::new();
        // State of the last turn across all threads; default to Completed when none.
        let mut last_turn_state = TurnState::Completed;

        for thread in &threads {
            // Messages for this thread, already ordered by position.
            let messages = self.storage.list_messages_by_thread(thread.id).await?;
            all_messages.extend(messages);

            // Turns for this thread — the last turn's state represents the thread's
            // final LLM interaction result.
            let turns = self.storage.list_turns_by_thread(thread.id).await?;
            if let Some(last) = turns.last() {
                last_turn_state = last.state.clone();
            }

            // Collect tool call views for every turn in this thread.
            // Note: in a single-thread/single-turn session this is exact. For
            // multi-turn threads, all tool calls are surfaced on every assistant
            // message — a known limitation until Message gains a turn_id link.
            // TODO Tool calls incorrectly attributed to all assistant messages in multi-turn sessions
            // tags: correctness, review:branch-era-02-delivery
            // `all_tool_call_views` accumulates tool calls from every turn, then the mapping
            // below attaches the entire collection to every assistant message (line ~235).
            // In a multi-turn conversation each assistant message should only carry the tool
            // calls from its own turn. Fix requires a turn_id column on messages (or a
            // per-turn bucket map keyed by position) so tool calls can be scoped correctly.
            // ---
            for turn in &turns {
                let records = self.storage.list_tool_calls_by_turn(turn.id).await?;
                for r in records {
                    let arguments = serde_json::from_str(&r.arguments)
                        .unwrap_or(JsonValue::String(r.arguments));
                    let result = r
                        .result
                        .map(|text| serde_json::from_str(&text).unwrap_or(JsonValue::String(text)));
                    all_tool_call_views.push(ToolCallView {
                        call_id: r.call_id,
                        tool: r.tool,
                        arguments,
                        result,
                        status: r.status,
                        duration_ms: r.duration_ms.map(|ms| ms.max(0) as u64),
                        truncated: r.truncated,
                        total_bytes: r.total_bytes.map(|b| b.max(0) as u64),
                        parent_call_id: r.parent_call_id,
                    });
                }
            }
        }

        // Sort all messages by position (threads use per-thread position, so sort
        // by created_at if positions overlap across threads).
        all_messages.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then(a.created_at.cmp(&b.created_at))
        });

        let message_views = all_messages
            .into_iter()
            .map(|m| {
                // Tool calls are attached to assistant messages only.
                let tool_calls = if m.role == "assistant" {
                    all_tool_call_views.clone()
                } else {
                    vec![]
                };

                MessageView {
                    role: m.role,
                    content: m.content,
                    timestamp: m.created_at,
                    turn_state: last_turn_state.clone(),
                    tool_calls,
                }
            })
            .collect();

        Ok(ConversationDetail {
            id: session.id,
            project_id: session.project_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
            messages: message_views,
        })
    }

    /// List sessions belonging to a specific project.
    ///
    /// Returns `ProjectNotFound` if no project with the given ID exists.
    pub async fn list_sessions_by_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ConversationSummary>, GatewayError> {
        // Verify the project exists before listing — gives a clean 404 rather
        // than silently returning an empty list for a non-existent project.
        self.project_store
            .get(project_id)
            .await?
            .ok_or(GatewayError::ProjectNotFound { id: project_id })?;

        let sessions = self.storage.list_by_project(project_id).await?;

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
                created_at: session.created_at,
                last_activity: session.updated_at,
                message_preview: preview,
                has_interrupted_turn,
            });
        }

        Ok(summaries)
    }

    /// Create a new session in the specified project.
    ///
    /// Returns `ProjectNotFound` if no project with the given ID exists.
    pub async fn create_session_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<ConversationResponse, GatewayError> {
        // Verify the project exists — return 404 rather than a FK constraint error.
        self.project_store
            .get(project_id)
            .await?
            .ok_or(GatewayError::ProjectNotFound { id: project_id })?;

        let session = self
            .storage
            .create_session(NewSession {
                project_id,
                agent_id: "gateway".to_string(),
            })
            .await?;

        // Create the default thread for the new session.
        self.storage
            .create(NewThread {
                session_id: session.id,
                agent_id: "gateway".to_string(),
            })
            .await?;

        Ok(ConversationResponse {
            id: session.id,
            project_id: session.project_id,
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
        let turn_state = turns
            .last()
            .map(|turn| turn.state.clone())
            .unwrap_or(TurnState::Completed);
        let branch = self.storage.get_branch(thread.id).await?;

        Ok(ConversationThreadDetail {
            id: thread.id,
            session_id: thread.session_id,
            agent_id: thread.agent_id,
            state: thread.state.to_string(),
            messages: messages
                .into_iter()
                .map(|message| MessageView {
                    role: message.role,
                    content: message.content,
                    timestamp: message.created_at,
                    turn_state: turn_state.clone(),
                    tool_calls: vec![],
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
}

// ---------------------------------------------------------------------------
// Project service — request/response types and service logic
// ---------------------------------------------------------------------------

/// Request body for creating a project.
#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    /// Optional workspace path. If provided, must exist on disk.
    pub workspace_path: Option<String>,
}

/// Request body for partially updating a project.
#[derive(Debug, Deserialize)]
pub struct UpdateProjectRequest {
    pub name: Option<String>,
    pub workspace_path: Option<String>,
}

/// API response for a single project.
#[derive(Debug, Serialize)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub workspace_path: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// API response for the list-projects endpoint.
#[derive(Debug, Serialize)]
pub struct ProjectListResponse {
    pub projects: Vec<ProjectResponse>,
}

/// Derives a URL-safe slug from a project name.
///
/// - Lowercases the name
/// - Replaces spaces with hyphens
/// - Strips any character that is neither alphanumeric nor a hyphen
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}

/// Returns the base directory for all project data: `~/.swage/projects/`.
fn projects_base_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".swage").join("projects"))
}

/// Mediates between HTTP project handlers and storage.
#[derive(Clone)]
pub struct ProjectService {
    pub(crate) project_store: Arc<dyn ProjectStore>,
}

impl ProjectService {
    pub fn new(project_store: Arc<dyn ProjectStore>) -> Self {
        Self { project_store }
    }

    /// Create a new project.
    ///
    /// Validates the name is non-empty, generates a slug, checks name
    /// uniqueness, optionally validates the workspace path exists on disk,
    /// creates the project directory structure, and stores the project.
    pub async fn create_project(
        &self,
        req: CreateProjectRequest,
    ) -> Result<ProjectResponse, GatewayError> {
        // Validate non-empty name.
        if req.name.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "name must not be empty".into(),
            ));
        }

        // Check workspace_path exists on disk if provided.
        if let Some(ref wp) = req.workspace_path {
            if !std::path::Path::new(wp).exists() {
                return Err(GatewayError::ValidationError(format!(
                    "workspace_path '{wp}' does not exist on disk"
                )));
            }
        }

        // Check name uniqueness.
        if self.project_store.get_by_name(&req.name).await?.is_some() {
            return Err(GatewayError::ProjectConflict(format!(
                "a project named '{}' already exists",
                req.name
            )));
        }

        let slug = slugify(&req.name);

        // Create the project directory: ~/.swage/projects/{slug}/memory/
        if let Some(base) = projects_base_dir() {
            let project_dir = base.join(&slug);
            let memory_dir = project_dir.join("memory");
            // Non-fatal: best-effort directory creation.
            if let Err(error) = std::fs::create_dir_all(&memory_dir) {
                tracing::warn!(
                    slug,
                    path = %memory_dir.display(),
                    %error,
                    "failed to create project memory directory"
                );
            }
        }

        let project = self
            .project_store
            .create(NewProject {
                name: req.name,
                slug,
                workspace_path: req.workspace_path,
            })
            .await?;

        Ok(project_to_response(project))
    }

    /// List all projects.
    pub async fn list_projects(&self) -> Result<ProjectListResponse, GatewayError> {
        let projects = self.project_store.list_all().await?;
        Ok(ProjectListResponse {
            projects: projects.into_iter().map(project_to_response).collect(),
        })
    }

    /// Get a project by ID.
    pub async fn get_project(&self, id: Uuid) -> Result<ProjectResponse, GatewayError> {
        self.project_store
            .get(id)
            .await?
            .map(project_to_response)
            .ok_or(GatewayError::ProjectNotFound { id })
    }

    /// Partially update a project.
    ///
    /// Validates workspace_path if provided. Regenerates slug when name changes.
    pub async fn update_project(
        &self,
        id: Uuid,
        req: UpdateProjectRequest,
    ) -> Result<ProjectResponse, GatewayError> {
        // Validate name if provided.
        if let Some(ref name) = req.name {
            if name.trim().is_empty() {
                return Err(GatewayError::ValidationError(
                    "name must not be empty".into(),
                ));
            }
        }

        // Validate workspace_path if provided.
        if let Some(ref wp) = req.workspace_path {
            if !std::path::Path::new(wp).exists() {
                return Err(GatewayError::ValidationError(format!(
                    "workspace_path '{wp}' does not exist on disk"
                )));
            }
        }

        // Derive the new slug from the new name, if name is being updated.
        let new_slug = req.name.as_deref().map(slugify);

        let project = self
            .project_store
            .update(id, req.name, new_slug, req.workspace_path)
            .await
            .map_err(|e| match e {
                swage_core::StorageError::NotFound { id: ref eid, .. } => {
                    let parsed_id = eid.parse::<Uuid>().unwrap_or(id);
                    GatewayError::ProjectNotFound { id: parsed_id }
                }
                other => GatewayError::Storage(other),
            })?;

        Ok(project_to_response(project))
    }

    /// Delete a project.
    ///
    /// Returns 404 if not found. Returns a conflict error if called on the
    /// default project. The project directory on disk is preserved.
    pub async fn delete_project(&self, id: Uuid) -> Result<(), GatewayError> {
        let found = self.project_store.delete(id).await.map_err(|e| match e {
            swage_core::StorageError::ConstraintViolation(msg) => {
                GatewayError::ProjectConflict(msg)
            }
            other => GatewayError::Storage(other),
        })?;

        if !found {
            return Err(GatewayError::ProjectNotFound { id });
        }

        Ok(())
    }
}

fn project_to_response(p: swage_storage::model::Project) -> ProjectResponse {
    ProjectResponse {
        id: p.id,
        name: p.name,
        slug: p.slug,
        workspace_path: p.workspace_path,
        created_at: p.created_at,
    }
}

// ---------------------------------------------------------------------------
// Team service — request/response types and service logic
// ---------------------------------------------------------------------------

/// Request body for creating a team within a project.
#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub entry_agent_id: String,
    #[serde(default)]
    pub members: Vec<String>,
}

/// Request body for partially updating a team.
#[derive(Debug, Deserialize)]
pub struct UpdateTeamRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub entry_agent_id: Option<String>,
    pub members: Option<Vec<String>>,
}

/// API response for a single team.
#[derive(Debug, Serialize)]
pub struct TeamResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    pub entry_agent_id: String,
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
    pub(crate) project_store: Arc<dyn ProjectStore>,
}

impl TeamService {
    pub fn new(storage: Arc<dyn Storage>, project_store: Arc<dyn ProjectStore>) -> Self {
        Self {
            storage,
            project_store,
        }
    }

    pub async fn create_team(
        &self,
        project_id: Uuid,
        req: CreateTeamRequest,
    ) -> Result<TeamResponse, GatewayError> {
        self.ensure_project_exists(project_id).await?;
        validate_team_name(&req.name)?;
        validate_agent_id(&req.entry_agent_id, "entry_agent_id")?;

        if self
            .storage
            .get_team_by_name(project_id, &req.name)
            .await?
            .is_some()
        {
            return Err(GatewayError::Storage(StorageError::ConstraintViolation(
                format!(
                    "a team named '{}' already exists in project {project_id}",
                    req.name
                ),
            )));
        }

        let members = normalise_members(req.members, &req.entry_agent_id)?;
        let team = self
            .storage
            .create_team(NewTeam {
                project_id,
                name: req.name,
                description: req.description,
                entry_agent_id: req.entry_agent_id,
                members,
            })
            .await?;

        Ok(team_to_response(team))
    }

    pub async fn list_teams(&self, project_id: Uuid) -> Result<TeamListResponse, GatewayError> {
        self.ensure_project_exists(project_id).await?;
        let teams = self.storage.list_teams_by_project(project_id).await?;
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
                .get_team_by_name(existing.project_id, name)
                .await?
            {
                if conflict.id != id {
                    return Err(GatewayError::Storage(StorageError::ConstraintViolation(
                        format!(
                            "a team named '{}' already exists in project {}",
                            name, existing.project_id
                        ),
                    )));
                }
            }
        }

        if let Some(ref entry_agent_id) = entry_agent_id {
            validate_agent_id(entry_agent_id, "entry_agent_id")?;
        }

        let resolved_entry_agent_id = entry_agent_id
            .clone()
            .unwrap_or_else(|| existing.entry_agent_id.clone());
        let members = match members {
            Some(members) => Some(normalise_members(members, &resolved_entry_agent_id)?),
            None if resolved_entry_agent_id != existing.entry_agent_id
                && !existing
                    .members
                    .iter()
                    .any(|member| member == &resolved_entry_agent_id) =>
            {
                Some(normalise_members(
                    existing.members.clone(),
                    &resolved_entry_agent_id,
                )?)
            }
            None => None,
        };

        let team = self
            .storage
            .update_team(id, name, description, entry_agent_id, members)
            .await?;

        Ok(team_to_response(team))
    }

    pub async fn delete_team(&self, id: Uuid) -> Result<(), GatewayError> {
        if !self.storage.delete_team(id).await? {
            return Err(team_not_found(id));
        }

        Ok(())
    }

    async fn ensure_project_exists(&self, project_id: Uuid) -> Result<(), GatewayError> {
        self.project_store
            .get(project_id)
            .await?
            .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
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

fn normalise_members(
    members: Vec<String>,
    entry_agent_id: &str,
) -> Result<Vec<String>, GatewayError> {
    let mut normalised = Vec::new();

    let entry_agent_id = entry_agent_id.trim();
    if entry_agent_id.is_empty() {
        return Err(GatewayError::ValidationError(
            "entry_agent_id must not be empty".into(),
        ));
    }

    normalised.push(entry_agent_id.to_string());
    for member in members {
        let member = member.trim();
        if member.is_empty() {
            return Err(GatewayError::ValidationError(
                "members must not contain empty agent IDs".into(),
            ));
        }
        if !normalised.iter().any(|existing| existing == member) {
            normalised.push(member.to_string());
        }
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
        project_id: team.project_id,
        name: team.name,
        description: team.description,
        entry_agent_id: team.entry_agent_id,
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
    pub topology_source: String,
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
        if req.topology_source.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "topology_source must not be empty".into(),
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
            team.members.iter().cloned().collect();
        known_agents.insert(team.entry_agent_id.clone());
        let catalog = swage_topology::TopologyEntityCatalog {
            known_agents,
            known_tasks: Default::default(),
            known_handlers: ["identity", "collect-results"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        };

        let graph = swage_python::topology::execute_topology_source(
            &req.topology_source,
            &format!("team-run:{team_id}"),
            std::time::Duration::from_secs(self.state.config.context.timeout),
            catalog,
        )
        .map_err(|error| GatewayError::ValidationError(error.to_string()))?;

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
                crate::topology::TopologyEngineError::TeamProjectMissing { project_id, .. } => {
                    GatewayError::ProjectNotFound { id: project_id }
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
        node_kind: node.node_kind,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use swage_storage::{
        model::{NewMessage, NewToolCall, NewTurn, TurnState},
        store::{SessionStore, TurnStore},
    };

    use super::*;
    use crate::turn::tests::MockStorage;

    fn make_service() -> ConversationService {
        let storage = Arc::new(MockStorage::new());
        ConversationService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn ProjectStore>,
        )
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
        assert_ne!(resp.project_id, Uuid::nil());
    }

    #[tokio::test]
    async fn create_conversation_multiple_times_yields_distinct_ids() {
        let svc = make_service();

        let a = svc.create_conversation().await.unwrap();
        let b = svc.create_conversation().await.unwrap();

        // Both conversations should belong to the same default project.
        assert_eq!(a.project_id, b.project_id);
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
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                role: "user".to_string(),
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
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                role: "user".to_string(),
                content: "Hi".to_string(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();
        svc.storage
            .create_message(NewMessage {
                thread_id,
                role: "assistant".to_string(),
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
        assert_eq!(detail.messages[0].turn_state, TurnState::Completed);
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
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                role: "user".into(),
                content: "list files".into(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                role: "assistant".into(),
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
                result: Some("file1\nfile2".into()),
                status: Some("success".into()),
                duration_ms: Some(10),
                truncated: false,
                total_bytes: Some(12),
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
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                role: "assistant".into(),
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
                result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
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
                result: Some("hi".into()),
                status: Some("success".into()),
                duration_ms: Some(5),
                truncated: false,
                total_bytes: Some(2),
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
                parent_turn_id: None,
            })
            .await
            .unwrap();
        svc.storage
            .create_message(NewMessage {
                thread_id,
                role: "assistant".into(),
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
