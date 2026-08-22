use std::{collections::HashMap, sync::Arc};

use uuid::Uuid;

use serde_json::Value as JsonValue;
use swage_core::{GatewayError, Role, StorageError};
use swage_storage::{
    model::{NewSession, NewThread, ThreadBranch, TurnState},
    store::{SpaceStore, Storage},
};

use swage_protocol::client::conversation::TurnState as WireTurnState;
pub use swage_protocol::client::conversation::{
    ConversationBranchResponse, ConversationDetail, ConversationResponse, ConversationSummary,
    ConversationThreadDetail, CreateConversationBranchRequest, CreateConversationRequest,
    MessageView, ToolCallView,
};

/// The built-in agent a session runs under when no definition is selected.
/// It needs no agent-definition row, so selector validation always admits it.
const DEFAULT_AGENT_ID: &str = "gateway";

fn turn_state_to_wire(state: &TurnState) -> WireTurnState {
    match state {
        TurnState::Pending => WireTurnState::Pending,
        TurnState::Processing => WireTurnState::Processing,
        TurnState::Completed => WireTurnState::Completed,
        TurnState::Failed => WireTurnState::Failed,
        TurnState::Interrupted => WireTurnState::Interrupted,
        TurnState::AwaitingApproval => WireTurnState::AwaitingApproval,
        TurnState::Delegating => WireTurnState::Delegating,
    }
}
/// Mediates between HTTP handlers and storage.
///
/// Translates domain errors into `GatewayError` variants and maps storage
/// rows to API-facing response types.
#[derive(Clone)]
pub struct ConversationService {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) space_store: Arc<dyn SpaceStore>,
    config: Option<swage_config::ResolvedConfig>,
}

impl ConversationService {
    pub fn new(storage: Arc<dyn Storage>, space_store: Arc<dyn SpaceStore>) -> Self {
        Self {
            storage,
            space_store,
            config: None,
        }
    }

    pub fn with_config(mut self, config: swage_config::ResolvedConfig) -> Self {
        self.config = Some(config);
        self
    }

    async fn resolved_effort(
        &self,
        space_id: Uuid,
        space_slug: &str,
        agent_id: &str,
        selected_model_id: Option<&str>,
        requested: Option<swage_protocol::client::settings::Effort>,
    ) -> Result<Option<swage_protocol::client::settings::Effort>, GatewayError> {
        if requested.is_some() {
            return Ok(requested);
        }
        let Some(active_config) = self.config.clone() else {
            return Ok(None);
        };
        let config =
            crate::turn::load_session_config_from_active(active_config, space_slug, agent_id)?;
        let config = match self
            .storage
            .resolve_agent_definition(agent_id, space_id)
            .await
        {
            Ok(Some(definition)) => crate::turn::apply_agent_definition(config, &definition),
            Ok(None) => config,
            Err(error) => {
                tracing::warn!(
                    agent_id,
                    %error,
                    "failed to resolve the agent definition while selecting session effort; using file config alone"
                );
                config
            }
        };
        let config = crate::turn::effective_session_config(config, selected_model_id);
        Ok(config
            .models
            .entries
            .get(&config.models.default)
            .and_then(|model| model.default_effort))
    }

    fn effort_metadata(
        effort: Option<swage_protocol::client::settings::Effort>,
    ) -> Option<JsonValue> {
        effort.map(|effort| serde_json::json!({"effort": effort}))
    }

    /// Create a new conversation in the default space.
    ///
    /// Also creates a default thread for the session so that process_turn has
    /// a thread_id to attach turns and messages to.
    pub async fn create_conversation(&self) -> Result<ConversationResponse, GatewayError> {
        self.create_conversation_from_request(CreateConversationRequest::default())
            .await
    }

    pub async fn create_conversation_with_model(
        &self,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_conversation_from_request(CreateConversationRequest {
            selected_model_id,
            agent_id: None,
            effort: None,
        })
        .await
    }

    pub async fn create_conversation_from_request(
        &self,
        request: CreateConversationRequest,
    ) -> Result<ConversationResponse, GatewayError> {
        let space = self.space_store.get_or_create_default().await?;
        let agent_id = self.validated_agent_id(space.id, request.agent_id).await?;
        let selected_model_id = normalise_selected_model_id(request.selected_model_id);
        let effort = self
            .resolved_effort(
                space.id,
                &space.slug,
                &agent_id,
                selected_model_id.as_deref(),
                request.effort,
            )
            .await?;
        let active_workspace_attachment_id = self.default_workspace_for_space(space.id).await?;
        let mut session = self
            .storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: agent_id.clone(),
                metadata: Self::effort_metadata(effort),
                active_workspace_attachment_id,
            })
            .await?;
        if let Some(selected_model_id) = selected_model_id {
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
                agent_id,
            })
            .await?;

        Ok(ConversationResponse {
            id: session.id,
            space_id: session.space_id,
            selected_model_id: session.selected_model_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
        })
    }

    /// Resolve a client-supplied agent selector to the agent id a new session
    /// runs under.
    ///
    /// An absent selector means the default agent. A named selector must be
    /// the default agent or an agent definition visible to the space
    /// (space-scoped shadowing global) — anything else is a validation error,
    /// never a session that silently runs under configuration the client did
    /// not choose. Trust-carrying paths that name their agent directly
    /// (schedule runs) use `create_session_in_space_for_agent` and are not
    /// validated here.
    async fn validated_agent_id(
        &self,
        space_id: Uuid,
        requested: Option<String>,
    ) -> Result<String, GatewayError> {
        let Some(agent_id) = requested else {
            return Ok(DEFAULT_AGENT_ID.to_string());
        };
        let agent_id = agent_id.trim().to_string();
        if agent_id.is_empty() {
            return Err(GatewayError::ValidationError(
                "agent_id must not be empty".to_string(),
            ));
        }
        if agent_id != DEFAULT_AGENT_ID
            && self
                .storage
                .resolve_agent_definition(&agent_id, space_id)
                .await?
                .is_none()
        {
            return Err(GatewayError::ValidationError(format!(
                "no agent definition named '{agent_id}' is visible to this space"
            )));
        }
        Ok(agent_id)
    }

    /// List all conversations in the default space, ordered by most-recent activity.
    pub async fn list_conversations(&self) -> Result<Vec<ConversationSummary>, GatewayError> {
        let space = self.space_store.get_or_create_default().await?;
        let sessions = self.storage.list_by_space(space.id).await?;

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
                    .and_then(|message_turn_id| turn_state_by_turn.get(&message_turn_id))
                    .map(turn_state_to_wire);
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
            space_id: session.space_id,
            selected_model_id: session.selected_model_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
            messages: message_views,
        })
    }

    /// List sessions belonging to a specific space.
    ///
    /// Returns `SpaceNotFound` if no space with the given ID exists.
    pub async fn list_sessions_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<ConversationSummary>, GatewayError> {
        // Verify the space exists before listing — gives a clean 404 rather
        // than silently returning an empty list for a non-existent space.
        self.space_store
            .get(space_id)
            .await?
            .ok_or(GatewayError::SpaceNotFound { id: space_id })?;

        let sessions = self.storage.list_by_space(space_id).await?;

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

    /// Create a new session in the specified space.
    ///
    /// Returns `SpaceNotFound` if no space with the given ID exists.
    pub async fn create_session_in_space(
        &self,
        space_id: Uuid,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_space_with_model(space_id, None)
            .await
    }

    pub async fn create_session_in_space_with_model(
        &self,
        space_id: Uuid,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_space_for_agent(space_id, DEFAULT_AGENT_ID, selected_model_id)
            .await
    }

    /// Create a session in a space from a client request, resolving and
    /// validating its agent selector.
    ///
    /// The space is checked first so a missing space reports 404 rather than
    /// an unknown-agent validation error against a space that does not exist.
    pub async fn create_session_in_space_from_request(
        &self,
        space_id: Uuid,
        request: CreateConversationRequest,
    ) -> Result<ConversationResponse, GatewayError> {
        let space = self
            .space_store
            .get(space_id)
            .await?
            .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
        let agent_id = self.validated_agent_id(space_id, request.agent_id).await?;
        let selected_model_id = normalise_selected_model_id(request.selected_model_id);
        let effort = self
            .resolved_effort(
                space_id,
                &space.slug,
                &agent_id,
                selected_model_id.as_deref(),
                request.effort,
            )
            .await?;
        self.create_session_in_space_for_agent_with_effort(
            space_id,
            &agent_id,
            selected_model_id,
            effort,
        )
        .await
    }

    /// Create a new session in the specified space, owned by a specific agent.
    ///
    /// The general session-creation path: interactive conversations use the
    /// default `"gateway"` agent via the wrappers above, while fresh-policy
    /// schedule runs create sessions for their schedule's target agent.
    pub async fn create_session_in_space_for_agent(
        &self,
        space_id: Uuid,
        agent_id: &str,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_space_for_agent_with_effort(
            space_id,
            agent_id,
            selected_model_id,
            None,
        )
        .await
    }

    async fn create_session_in_space_for_agent_with_effort(
        &self,
        space_id: Uuid,
        agent_id: &str,
        selected_model_id: Option<String>,
        effort: Option<swage_protocol::client::settings::Effort>,
    ) -> Result<ConversationResponse, GatewayError> {
        // Verify the space exists — return 404 rather than a FK constraint error.
        self.space_store
            .get(space_id)
            .await?
            .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
        let active_workspace_attachment_id = self.default_workspace_for_space(space_id).await?;

        let mut session = self
            .storage
            .create_session(NewSession {
                space_id,
                agent_id: agent_id.to_string(),
                metadata: Self::effort_metadata(effort),
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
            space_id: session.space_id,
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
                            .and_then(|turn_id| turn_state_by_turn.get(&turn_id))
                            .map(turn_state_to_wire),
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

    async fn default_workspace_for_space(
        &self,
        space_id: Uuid,
    ) -> Result<Option<Uuid>, GatewayError> {
        Ok(self
            .storage
            .list_workspace_attachments_by_space(space_id)
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
