use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use serde_json::Value as JsonValue;
use swage_storage::{
    model::{NewProject, NewSession, TurnState},
    store::Storage,
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
    /// State of the turn this message belongs to.
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
}

impl ConversationService {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
    }

    /// Create a new conversation in the default project.
    pub async fn create_conversation(&self) -> Result<ConversationResponse, GatewayError> {
        let project = self.storage.get_or_create_default().await?;
        let session = self
            .storage
            .create_session(NewSession {
                project_id: project.id,
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
        let project = self.storage.get_or_create_default().await?;
        let sessions = self.storage.list_by_project(project.id).await?;

        let mut summaries = Vec::with_capacity(sessions.len());

        for session in sessions {
            let preview = self
                .storage
                .get_latest_message_for_session(session.id)
                .await?
                .map(|m| m.content);

            let turns = self.storage.list_turns_by_session(session.id).await?;
            let has_interrupted_turn = turns.iter().any(|t| t.state == TurnState::Interrupted);

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
    pub async fn get_conversation(
        &self,
        id: Uuid,
    ) -> Result<ConversationDetail, GatewayError> {
        let session = self
            .storage
            .get_session(id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id })?;

        let messages = self.storage.list_messages_by_session(id).await?;
        let turns = self.storage.list_turns_by_session(id).await?;

        // Build a turn_id → TurnState map for O(1) annotation of each message.
        let turn_states: HashMap<Uuid, TurnState> = turns
            .iter()
            .map(|t| (t.id, t.state.clone()))
            .collect();

        // Load tool calls for all turns in this session, indexed by turn_id.
        // Only fetches turns that have at least one message to avoid unnecessary queries.
        let mut tool_calls_by_turn: HashMap<Uuid, Vec<ToolCallView>> = HashMap::new();
        for turn in &turns {
            let records = self.storage.list_tool_calls_by_turn(turn.id).await?;
            if !records.is_empty() {
                let views = records
                    .into_iter()
                    .map(|r| {
                        // Arguments are stored as a JSON string — parse to Value.
                        let arguments = serde_json::from_str(&r.arguments)
                            .unwrap_or(JsonValue::String(r.arguments));

                        // Results may be arbitrary text or JSON; normalise to Value.
                        let result = r.result.map(|text| {
                            serde_json::from_str(&text)
                                .unwrap_or(JsonValue::String(text))
                        });

                        ToolCallView {
                            call_id: r.call_id,
                            tool: r.tool,
                            arguments,
                            result,
                            status: r.status,
                            // i64 → u64: negative values shouldn't occur in practice.
                            duration_ms: r.duration_ms.map(|ms| ms.max(0) as u64),
                            truncated: r.truncated,
                            total_bytes: r.total_bytes.map(|b| b.max(0) as u64),
                            parent_call_id: r.parent_call_id,
                        }
                    })
                    .collect();
                tool_calls_by_turn.insert(turn.id, views);
            }
        }

        let mut message_views = Vec::with_capacity(messages.len());
        for m in messages {
            // Defensive default: if a message's turn was deleted or never recorded,
            // treat it as completed — the message content is still valid.
            let turn_state = turn_states
                .get(&m.turn_id)
                .cloned()
                .unwrap_or(TurnState::Completed);

            // Tool calls are attached to assistant messages only.
            let tool_calls = if m.role == "assistant" {
                tool_calls_by_turn
                    .get(&m.turn_id)
                    .cloned()
                    .unwrap_or_default()
            } else {
                vec![]
            };

            message_views.push(MessageView {
                role: m.role,
                content: m.content,
                timestamp: m.created_at,
                turn_state,
                tool_calls,
            });
        }
        let message_views = message_views;

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
        self.storage
            .get(project_id)
            .await?
            .ok_or(GatewayError::ProjectNotFound { id: project_id })?;

        let sessions = self.storage.list_by_project(project_id).await?;

        let mut summaries = Vec::with_capacity(sessions.len());
        for session in sessions {
            let preview = self
                .storage
                .get_latest_message_for_session(session.id)
                .await?
                .map(|m| m.content);

            let turns = self.storage.list_turns_by_session(session.id).await?;
            let has_interrupted_turn = turns.iter().any(|t| t.state == TurnState::Interrupted);

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
        self.storage
            .get(project_id)
            .await?
            .ok_or(GatewayError::ProjectNotFound { id: project_id })?;

        let session = self
            .storage
            .create_session(NewSession { project_id })
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
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".swage").join("projects"))
}

/// Mediates between HTTP project handlers and storage.
#[derive(Clone)]
pub struct ProjectService {
    pub(crate) storage: Arc<dyn Storage>,
}

impl ProjectService {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
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
            return Err(GatewayError::ValidationError("name must not be empty".into()));
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
        if self.storage.get_by_name(&req.name).await?.is_some() {
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
            let _ = std::fs::create_dir_all(&memory_dir);
        }

        let project = self.storage.create(NewProject {
            name: req.name,
            slug,
            workspace_path: req.workspace_path,
        }).await?;

        Ok(project_to_response(project))
    }

    /// List all projects.
    pub async fn list_projects(&self) -> Result<ProjectListResponse, GatewayError> {
        let projects = self.storage.list_all().await?;
        Ok(ProjectListResponse {
            projects: projects.into_iter().map(project_to_response).collect(),
        })
    }

    /// Get a project by ID.
    pub async fn get_project(&self, id: Uuid) -> Result<ProjectResponse, GatewayError> {
        self.storage
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
                return Err(GatewayError::ValidationError("name must not be empty".into()));
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

        let project = self.storage
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
        let found = self.storage.delete(id).await.map_err(|e| match e {
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use swage_storage::{
        model::{NewMessage, NewToolCall, NewTurn, TurnState},
        store::SessionStore,
    };

    use super::*;
    use crate::turn::tests::MockStorage;

    fn make_service() -> ConversationService {
        let storage = Arc::new(MockStorage::new()) as Arc<dyn Storage>;
        ConversationService::new(storage)
    }

    // -----------------------------------------------------------------------
    // create_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_conversation_returns_valid_response() {
        let svc = make_service();

        let resp = svc.create_conversation().await.expect("create should succeed");

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

        // Insert a turn and a message via the underlying storage.
        let turn = svc
            .storage
            .create_turn(NewTurn {
                session_id,
                state: TurnState::InProgress,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                turn_id: turn.id,
                role: "user".to_string(),
                content: "Hello!".to_string(),
                position: 0,
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

        let turn = svc
            .storage
            .create_turn(NewTurn {
                session_id,
                state: TurnState::InProgress,
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

        let turn = svc
            .storage
            .create_turn(NewTurn {
                session_id,
                state: TurnState::InProgress,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                turn_id: turn.id,
                role: "user".to_string(),
                content: "Hi".to_string(),
                position: 0,
            })
            .await
            .unwrap();
        svc.storage
            .create_message(NewMessage {
                turn_id: turn.id,
                role: "assistant".to_string(),
                content: "Hello!".to_string(),
                position: 1,
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

        svc.delete_conversation(resp.id).await.expect("delete should succeed");

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

        let turn = svc
            .storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                turn_id: turn.id,
                role: "user".into(),
                content: "list files".into(),
                position: 0,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                turn_id: turn.id,
                role: "assistant".into(),
                content: "I'll run ls for you.".into(),
                position: 1,
            })
            .await
            .unwrap();

        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
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
        assert!(user_msg.tool_calls.is_empty(), "user message should have no tool calls");

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

        let turn = svc
            .storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                turn_id: turn.id,
                role: "assistant".into(),
                content: "Ran python".into(),
                position: 0,
            })
            .await
            .unwrap();

        // Native python_execute call.
        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
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
        assert!(native.parent_call_id.is_none(), "native call should have no parent");

        let sub = &tool_calls[1];
        assert_eq!(sub.call_id, "sub_01");
        assert_eq!(sub.parent_call_id.as_deref(), Some("py_01"));
    }

    #[tokio::test]
    async fn get_conversation_tool_calls_empty_when_none_recorded() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let turn = svc
            .storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();
        svc.storage
            .create_message(NewMessage {
                turn_id: turn.id,
                role: "assistant".into(),
                content: "Hello".into(),
                position: 0,
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
