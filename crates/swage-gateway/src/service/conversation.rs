use std::{collections::HashMap, path::PathBuf, sync::Arc};
use swage_core::ScopeLocation;
use tokio::sync::broadcast;

use uuid::Uuid;

use serde_json::Value as JsonValue;
use swage_core::{GatewayError, Role, StorageError};
use swage_storage::{
    SessionActivity, SessionListing,
    model::{
        NewAuditEvent, NewSession, NewThread, SessionOrigin, ThreadBranch, ThreadState, TurnState,
    },
    store::{Storage, TopicStore},
};

use swage_protocol::client::conversation::TurnState as WireTurnState;
pub use swage_protocol::client::conversation::{
    ConversationBranchListResponse, ConversationBranchResponse, ConversationDetail,
    ConversationResponse, ConversationSummary, ConversationThreadDetail,
    CreateConversationBranchRequest, CreateConversationRequest, MessageView, ToolCallView,
};

/// How much of the branch-point message a branch listing shows.
const BRANCH_PREVIEW_CHARS: usize = 80;

/// The built-in agent a session runs under when no definition is selected.
/// It needs no agent-definition row, so selector validation always admits it.
const DEFAULT_AGENT_ID: &str = "gateway";

/// The wire view of a stored message: every field the record holds, the
/// attachments parsed against the contract (a value outside it is refused as
/// corruption), the turn state and tool calls the caller resolved. Every
/// projection and every live announcement maps through here, so the view a
/// client receives live is byte-for-byte the view it would load.
pub(crate) fn message_view(
    message: swage_storage::model::Message,
    turn_state: Option<WireTurnState>,
    tool_calls: Vec<ToolCallView>,
) -> Result<MessageView, GatewayError> {
    Ok(MessageView {
        id: message.id,
        thread_id: message.thread_id,
        turn_id: message.turn_id,
        role: message.role.to_string(),
        origin: message.origin,
        content: message.content,
        attachments: crate::turn::message_attachments(&message.attachments)?,
        timestamp: message.created_at,
        turn_state,
        tool_calls,
    })
}

pub(crate) fn turn_state_to_wire(state: &TurnState) -> WireTurnState {
    match state {
        TurnState::Pending => WireTurnState::Pending,
        TurnState::Processing => WireTurnState::Processing,
        TurnState::Completed => WireTurnState::Completed,
        TurnState::Refused => WireTurnState::Refused,
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
    pub(crate) topic_store: Arc<dyn TopicStore>,
    config: Option<(swage_config::ResolvedConfig, PathBuf)>,
    broadcast: broadcast::Sender<swage_protocol::client::websocket::ServerEvent>,
}

impl ConversationService {
    pub fn new(storage: Arc<dyn Storage>, topic_store: Arc<dyn TopicStore>) -> Self {
        let (broadcast, _) = broadcast::channel(16);
        Self {
            storage,
            topic_store,
            config: None,
            broadcast,
        }
    }

    pub fn with_broadcast(
        mut self,
        broadcast: broadcast::Sender<swage_protocol::client::websocket::ServerEvent>,
    ) -> Self {
        self.broadcast = broadcast;
        self
    }

    pub fn with_config(
        mut self,
        config: swage_config::ResolvedConfig,
        config_root: PathBuf,
    ) -> Self {
        self.config = Some((config, config_root));
        self
    }

    async fn resolved_effort(
        &self,
        topic_id: Uuid,
        scope: &ScopeLocation,
        agent_id: &str,
        selected_model_id: Option<&str>,
        requested: Option<swage_protocol::client::settings::Effort>,
    ) -> Result<Option<swage_protocol::client::settings::Effort>, GatewayError> {
        if requested.is_some() {
            return Ok(requested);
        }
        let Some((active_config, config_root)) = &self.config else {
            return Ok(None);
        };
        let config = crate::turn::load_session_config_from_root(
            config_root,
            active_config.clone(),
            scope,
            agent_id,
        )?;
        let config = match crate::service::resolve_effective_agent_definition(
            self.storage.as_ref(),
            agent_id,
            topic_id,
        )
        .await
        {
            Ok(Some(definition)) => {
                crate::turn::apply_agent_definition(config, &definition.definition)
            }
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
        let selected_model_id = selected_model_id.unwrap_or(&config.models.default);
        Ok(
            crate::turn::resolve_model_catalogue_entry(&config.models, selected_model_id)
                .and_then(|(_, model)| model.default_effort),
        )
    }

    fn effort_metadata(
        effort: Option<swage_protocol::client::settings::Effort>,
    ) -> Option<JsonValue> {
        effort.map(|effort| serde_json::json!({"effort": effort}))
    }

    /// Create a new conversation without a topic, in the general scope.
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
        let topic = self.topic_store.general_scope().await?;
        let agent_id = self.validated_agent_id(topic.id, request.agent_id).await?;
        let selected_model_id = normalise_selected_model_id(request.selected_model_id);
        let effort = self
            .resolved_effort(
                topic.id,
                &topic.location(),
                &agent_id,
                selected_model_id.as_deref(),
                request.effort,
            )
            .await?;
        let active_workspace_attachment_id = self.default_workspace_for_topic(topic.id).await?;
        let mut session = self
            .storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: agent_id.clone(),
                origin: swage_storage::SessionOrigin::Direct,
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
        self.open_default_surfaces(session.id).await?;

        Ok(ConversationResponse {
            id: session.id,
            topic_id: topic.client_topic_id(),
            agent_id: session.agent_id.clone(),
            selected_model_id: session.selected_model_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
        })
    }

    /// Resolve a client-supplied agent selector to the agent id a new session
    /// runs under.
    ///
    /// An absent selector means the default agent. A named selector must be
    /// the default agent or an agent definition visible to the topic
    /// (topic-scoped shadowing global) — anything else is a validation error,
    /// never a session that silently runs under configuration the client did
    /// not choose. Trust-carrying paths that name their agent directly
    /// (schedule runs) use `create_session_in_topic_for_agent` and are not
    /// validated here.
    async fn validated_agent_id(
        &self,
        topic_id: Uuid,
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
        match crate::service::resolve_effective_agent_definition(
            self.storage.as_ref(),
            &agent_id,
            topic_id,
        )
        .await?
        {
            None => Err(GatewayError::ValidationError(format!(
                "no agent definition named '{agent_id}' is visible to this topic"
            ))),
            Some(definition) if !definition.selectable => {
                Err(GatewayError::ValidationError(format!(
                    "'{agent_id}' is the platform's own agent and cannot be chosen for a session"
                )))
            }
            Some(_) => Ok(agent_id),
        }
    }

    /// List the conversations of the general scope, newest session update first.
    pub async fn list_conversations(&self) -> Result<Vec<ConversationSummary>, GatewayError> {
        let topic = self.topic_store.general_scope().await?;
        self.list_conversation_summaries(topic.id).await
    }

    /// Get a conversation with its full message history.
    pub async fn get_conversation(&self, id: Uuid) -> Result<ConversationDetail, GatewayError> {
        let session = self
            .storage
            .get_session(id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id })?;

        // The detail is the root thread's view: a branch carries copies of
        // these messages up to its branch point and diverges after it, so
        // its messages are read from the thread route, never merged here.
        let root_thread =
            crate::thread_selection::resolve_thread_for_session(self.storage.as_ref(), id, None)
                .await?;

        let mut all_messages = self.storage.list_messages_by_thread(root_thread.id).await?;
        let mut tool_calls_by_turn: HashMap<Uuid, Vec<ToolCallView>> = HashMap::new();
        let mut turn_state_by_turn: HashMap<Uuid, TurnState> = HashMap::new();

        for turn in self.storage.list_turns_by_thread(root_thread.id).await? {
            let turn_id = turn.id;
            turn_state_by_turn.insert(turn_id, turn.state);
            let records = self.storage.list_tool_calls_by_turn(turn_id).await?;
            tool_calls_by_turn.insert(
                turn_id,
                records.into_iter().map(tool_call_record_to_view).collect(),
            );
        }

        all_messages.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.position.cmp(&b.position))
        });

        let message_views = all_messages
            .into_iter()
            .map(|message| -> Result<MessageView, GatewayError> {
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

                message_view(message, turn_state, tool_calls)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let topic =
            self.topic_store
                .get(session.topic_id)
                .await?
                .ok_or(GatewayError::TopicNotFound {
                    id: session.topic_id,
                })?;
        let queued = self
            .storage
            .list_queued_turn_inputs(session.id)
            .await?
            .iter()
            .map(crate::turn_queue::queued_message_view)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ConversationDetail {
            queued,
            id: session.id,
            topic_id: topic.client_topic_id(),
            agent_id: session.agent_id.clone(),
            selected_model_id: session.selected_model_id,
            created_at: session.created_at,
            last_activity: session.updated_at,
            root_thread_id: root_thread.id,
            messages: message_views,
        })
    }

    /// List sessions belonging to a specific topic.
    ///
    /// Returns `TopicNotFound` if no topic with the given ID exists.
    pub async fn list_sessions_by_topic(
        &self,
        topic_id: Uuid,
    ) -> Result<Vec<ConversationSummary>, GatewayError> {
        // Verify the topic exists before listing — gives a clean 404 rather
        // than silently returning an empty list for a non-existent topic.
        self.topic_store
            .get(topic_id)
            .await?
            .ok_or(GatewayError::TopicNotFound { id: topic_id })?;

        self.list_conversation_summaries(topic_id).await
    }

    /// Create a new session in the specified topic.
    ///
    /// Returns `TopicNotFound` if no topic with the given ID exists.
    pub async fn create_session_in_topic(
        &self,
        topic_id: Uuid,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_topic_with_model(topic_id, None)
            .await
    }

    pub async fn create_session_in_topic_with_model(
        &self,
        topic_id: Uuid,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_topic_for_agent(
            topic_id,
            DEFAULT_AGENT_ID,
            SessionOrigin::Direct,
            selected_model_id,
        )
        .await
    }

    /// Create a session in a topic from a client request, resolving and
    /// validating its agent selector.
    ///
    /// The topic is checked first so a missing topic reports 404 rather than
    /// an unknown-agent validation error against a topic that does not exist.
    pub async fn create_session_in_topic_from_request(
        &self,
        topic_id: Uuid,
        request: CreateConversationRequest,
    ) -> Result<ConversationResponse, GatewayError> {
        let topic = self
            .topic_store
            .get(topic_id)
            .await?
            .ok_or(GatewayError::TopicNotFound { id: topic_id })?;
        let agent_id = self.validated_agent_id(topic_id, request.agent_id).await?;
        let selected_model_id = normalise_selected_model_id(request.selected_model_id);
        let effort = self
            .resolved_effort(
                topic_id,
                &topic.location(),
                &agent_id,
                selected_model_id.as_deref(),
                request.effort,
            )
            .await?;
        self.create_session_in_topic_for_agent_with_effort(
            topic_id,
            &agent_id,
            SessionOrigin::Direct,
            selected_model_id,
            effort,
        )
        .await
    }

    /// Create a new session in the specified topic, owned by a specific agent.
    ///
    /// The general session-creation path: interactive conversations use the
    /// default `"gateway"` agent via the wrappers above, while fresh-policy
    /// schedule runs create sessions for their schedule's target agent and
    /// record the schedule as the session's origin.
    pub async fn create_session_in_topic_for_agent(
        &self,
        topic_id: Uuid,
        agent_id: &str,
        origin: SessionOrigin,
        selected_model_id: Option<String>,
    ) -> Result<ConversationResponse, GatewayError> {
        self.create_session_in_topic_for_agent_with_effort(
            topic_id,
            agent_id,
            origin,
            selected_model_id,
            None,
        )
        .await
    }

    async fn create_session_in_topic_for_agent_with_effort(
        &self,
        topic_id: Uuid,
        agent_id: &str,
        origin: SessionOrigin,
        selected_model_id: Option<String>,
        effort: Option<swage_protocol::client::settings::Effort>,
    ) -> Result<ConversationResponse, GatewayError> {
        let person_opened = matches!(origin, SessionOrigin::Direct);
        // Verify the topic exists — return 404 rather than a FK constraint error.
        let topic = self
            .topic_store
            .get(topic_id)
            .await?
            .ok_or(GatewayError::TopicNotFound { id: topic_id })?;
        let active_workspace_attachment_id = self.default_workspace_for_topic(topic_id).await?;

        let mut session = self
            .storage
            .create_session(NewSession {
                topic_id,
                agent_id: agent_id.to_string(),
                origin,
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
        if person_opened {
            self.open_default_surfaces(session.id).await?;
        }

        Ok(ConversationResponse {
            id: session.id,
            topic_id: topic.client_topic_id(),
            agent_id: session.agent_id.clone(),
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

        // The lineage is audit-trail material: a reader reconstructs where a
        // branch came from without opening the thread.
        self.storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id: Some(session_id),
                entity_kind: "thread".to_owned(),
                entity_id: branch.child_thread_id.to_string(),
                event_kind: "thread_branched".to_owned(),
                summary: format!(
                    "Branched thread {} from thread {} at message {}",
                    branch.child_thread_id,
                    branch.parent_thread_id,
                    branch.branched_from_message_id
                ),
                metadata: serde_json::json!({
                    "child_thread_id": branch.child_thread_id,
                    "parent_thread_id": branch.parent_thread_id,
                    "branched_from_message_id": branch.branched_from_message_id,
                }),
            })
            .await?;

        self.branch_response(branch).await
    }

    /// List branches created within a conversation, each with its thread's
    /// state and the opening of the message it was taken from.
    pub async fn list_branches(
        &self,
        session_id: Uuid,
    ) -> Result<ConversationBranchListResponse, GatewayError> {
        self.storage
            .get_session(session_id)
            .await?
            .ok_or(GatewayError::ConversationNotFound { id: session_id })?;

        let mut branches = Vec::new();
        for branch in self.storage.list_branches_by_session(session_id).await? {
            branches.push(self.branch_response(branch).await?);
        }
        Ok(ConversationBranchListResponse { branches })
    }

    /// Abandon a branch: the thread keeps its history and lineage and takes
    /// no further turns. The root thread is the conversation itself and is
    /// never abandoned this way; a thread mid-turn is cancelled first.
    pub async fn abandon_thread(
        &self,
        thread_id: Uuid,
    ) -> Result<ConversationThreadDetail, GatewayError> {
        let thread = self.storage.get(thread_id).await?.ok_or_else(|| {
            GatewayError::Storage(StorageError::NotFound {
                entity: "thread".into(),
                id: thread_id.to_string(),
            })
        })?;
        let branch = self.storage.get_branch(thread.id).await?.ok_or_else(|| {
            GatewayError::ValidationError(format!(
                "thread {thread_id} is the conversation's root thread, not a branch"
            ))
        })?;

        self.storage
            .update_state(thread.id, ThreadState::Abandoned)
            .await?;
        self.storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id: Some(thread.session_id),
                entity_kind: "thread".to_owned(),
                entity_id: thread.id.to_string(),
                event_kind: "thread_abandoned".to_owned(),
                summary: format!(
                    "Abandoned branch thread {} (branched from thread {} at message {})",
                    thread.id, branch.parent_thread_id, branch.branched_from_message_id
                ),
                metadata: serde_json::json!({
                    "child_thread_id": branch.child_thread_id,
                    "parent_thread_id": branch.parent_thread_id,
                    "branched_from_message_id": branch.branched_from_message_id,
                }),
            })
            .await?;

        self.get_thread_detail(thread.id).await
    }

    /// Project a lineage row with what a person needs to recognise it: the
    /// branch thread's state and the opening of the branch-point message.
    async fn branch_response(
        &self,
        branch: ThreadBranch,
    ) -> Result<ConversationBranchResponse, GatewayError> {
        let state = self
            .storage
            .get(branch.child_thread_id)
            .await?
            .map(|thread| thread.state.to_string())
            .ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "thread".into(),
                    id: branch.child_thread_id.to_string(),
                })
            })?;
        let branched_from_preview = self
            .storage
            .get_message(branch.branched_from_message_id)
            .await?
            .map(|message| message_preview(&message.content));
        Ok(ConversationBranchResponse {
            child_thread_id: branch.child_thread_id,
            parent_thread_id: branch.parent_thread_id,
            branched_from_message_id: branch.branched_from_message_id,
            branched_from_preview,
            state,
            created_at: branch.created_at,
        })
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
        let branch = match self.storage.get_branch(thread.id).await? {
            Some(branch) => Some(self.branch_response(branch).await?),
            None => None,
        };

        Ok(ConversationThreadDetail {
            id: thread.id,
            session_id: thread.session_id,
            agent_id: thread.agent_id,
            state: thread.state.to_string(),
            messages: messages
                .into_iter()
                .map(|message| -> Result<MessageView, GatewayError> {
                    let is_assistant = message.role == Role::Assistant;
                    let turn_state = message
                        .turn_id
                        .and_then(|turn_id| turn_state_by_turn.get(&turn_id))
                        .map(turn_state_to_wire);
                    let tool_calls = if is_assistant {
                        message
                            .turn_id
                            .and_then(|turn_id| tool_calls_by_turn.get(&turn_id).cloned())
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    message_view(message, turn_state, tool_calls)
                })
                .collect::<Result<Vec<_>, _>>()?,
            branch,
        })
    }

    // ---------------------------------------------------------------------------
    // Private helpers
    // ---------------------------------------------------------------------------

    async fn list_conversation_summaries(
        &self,
        topic_id: Uuid,
    ) -> Result<Vec<ConversationSummary>, GatewayError> {
        let mut activities = self
            .storage
            .list_recent_sessions(SessionListing {
                topic_id: Some(topic_id),
                include_archived: true,
                ..SessionListing::default()
            })
            .await?;
        activities.sort_by_key(|activity| std::cmp::Reverse(activity.session.updated_at));
        Ok(activities.into_iter().map(conversation_summary).collect())
    }

    /// A new session opens with its topic's default surfaces; the rule is
    /// the surface module's, shared with every other add.
    async fn open_default_surfaces(&self, session_id: Uuid) -> Result<(), GatewayError> {
        crate::surface::open_default_surfaces(self.storage.as_ref(), &self.broadcast, session_id)
            .await
    }

    async fn default_workspace_for_topic(
        &self,
        topic_id: Uuid,
    ) -> Result<Option<Uuid>, GatewayError> {
        Ok(self
            .storage
            .list_workspace_attachments_by_topic(topic_id)
            .await?
            .into_iter()
            .find(|workspace| workspace.is_default && !workspace.is_missing)
            .map(|workspace| workspace.id))
    }
}

fn conversation_summary(activity: SessionActivity) -> ConversationSummary {
    let session = activity.session;
    ConversationSummary {
        id: session.id,
        agent_id: session.agent_id,
        selected_model_id: session.selected_model_id,
        created_at: session.created_at,
        last_activity: session.updated_at,
        message_preview: activity.latest_message,
        has_interrupted_turn: activity.has_interrupted_turn,
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
        summary: record.display_summary,
    }
}

/// The opening of a message, on one line, for telling branches apart.
fn message_preview(content: &str) -> String {
    let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= BRANCH_PREVIEW_CHARS {
        return single_line;
    }
    let mut preview = single_line
        .chars()
        .take(BRANCH_PREVIEW_CHARS)
        .collect::<String>();
    preview.push('…');
    preview
}

fn normalise_selected_model_id(selected_model_id: Option<String>) -> Option<String> {
    selected_model_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod branch_tests {
    use std::sync::Arc;

    use swage_core::Role;
    use swage_storage::model::NewMessage;
    use swage_storage::sqlite::SqliteStorage;
    use swage_storage::store::{AuditStore, MessageStore, Storage, ThreadStore, TopicStore};
    use uuid::Uuid;

    use super::*;

    async fn service_with_conversation() -> (ConversationService, Arc<SqliteStorage>, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite"));
        let service = ConversationService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
        );
        let conversation = service.create_conversation().await.expect("conversation");
        (service, storage, conversation.id)
    }

    async fn say(
        storage: &SqliteStorage,
        thread_id: Uuid,
        role: Role,
        content: &str,
        position: i32,
    ) -> Uuid {
        storage
            .create_message(NewMessage {
                thread_id,
                attachments: Vec::new(),
                turn_id: None,
                role,
                origin: swage_storage::MessageOrigin::User,
                content: content.into(),
                position,
                agent_id: "gateway".into(),
            })
            .await
            .expect("message")
            .id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_view_carries_each_message_s_recorded_origin() {
        // Both projections — the conversation and a thread's detail — map
        // through one view, so the origin a client labels by is the recorded
        // one on either route.
        let (service, storage, session_id) = service_with_conversation().await;
        let root = service.get_conversation(session_id).await.expect("detail");
        say(&storage, root.root_thread_id, Role::User, "typed", 0).await;
        storage
            .create_message(NewMessage {
                thread_id: root.root_thread_id,
                attachments: Vec::new(),
                turn_id: None,
                role: Role::User,
                origin: swage_storage::MessageOrigin::Schedule,
                content: "a brief".into(),
                position: 1,
                agent_id: "gateway".into(),
            })
            .await
            .expect("brief");

        let detail = service.get_conversation(session_id).await.expect("detail");
        let origins: Vec<_> = detail
            .messages
            .iter()
            .map(|message| (message.role.as_str(), message.origin))
            .collect();
        assert_eq!(
            origins,
            vec![
                ("user", swage_storage::MessageOrigin::User),
                ("user", swage_storage::MessageOrigin::Schedule),
            ]
        );

        let thread = service
            .get_thread_detail(root.root_thread_id)
            .await
            .expect("thread detail");
        assert_eq!(
            thread.messages[1].origin,
            swage_storage::MessageOrigin::Schedule
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_branch_carries_its_lineage_state_and_preview_and_lands_in_the_audit_trail() {
        let (service, storage, session_id) = service_with_conversation().await;
        let root = service.get_conversation(session_id).await.expect("detail");
        let first = say(
            &storage,
            root.root_thread_id,
            Role::User,
            "  What about\nthe other  approach?",
            0,
        )
        .await;
        say(
            &storage,
            root.root_thread_id,
            Role::Assistant,
            "It also works.",
            1,
        )
        .await;

        let branch = service
            .create_branch(
                session_id,
                CreateConversationBranchRequest {
                    thread_id: root.root_thread_id,
                    from_message_id: first,
                },
            )
            .await
            .expect("branch");
        assert_eq!(branch.parent_thread_id, root.root_thread_id);
        assert_eq!(branch.branched_from_message_id, first);
        assert_eq!(branch.state, "idle");
        assert_eq!(
            branch.branched_from_preview.as_deref(),
            Some("What about the other approach?")
        );

        let listed = service.list_branches(session_id).await.expect("list");
        assert_eq!(listed.branches.len(), 1);
        assert_eq!(listed.branches[0].child_thread_id, branch.child_thread_id);

        // The branch carries the ancestor message; the root view is untouched.
        let thread = service
            .get_thread_detail(branch.child_thread_id)
            .await
            .expect("thread");
        assert_eq!(thread.messages.len(), 1);
        assert_eq!(
            thread.messages[0].content,
            "  What about\nthe other  approach?"
        );
        assert_eq!(
            thread
                .branch
                .as_ref()
                .map(|lineage| lineage.parent_thread_id),
            Some(root.root_thread_id)
        );
        let root_again = service.get_conversation(session_id).await.expect("detail");
        assert_eq!(root_again.messages.len(), 2);
        assert!(
            root_again
                .messages
                .iter()
                .all(|message| message.thread_id == root.root_thread_id)
        );
        assert_eq!(root_again.messages[0].id, first);

        let events = storage
            .list_audit_events_by_session(session_id)
            .await
            .expect("audit");
        let branched = events
            .iter()
            .find(|event| event.event_kind == "thread_branched")
            .expect("thread_branched event");
        assert_eq!(branched.entity_id, branch.child_thread_id.to_string());
        assert_eq!(
            branched.metadata["parent_thread_id"],
            serde_json::json!(root.root_thread_id)
        );
        assert_eq!(
            branched.metadata["branched_from_message_id"],
            serde_json::json!(first)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn abandoning_a_branch_is_terminal_recorded_and_refused_for_the_root() {
        let (service, storage, session_id) = service_with_conversation().await;
        let root = service.get_conversation(session_id).await.expect("detail");
        let first = say(&storage, root.root_thread_id, Role::User, "hello", 0).await;
        let branch = service
            .create_branch(
                session_id,
                CreateConversationBranchRequest {
                    thread_id: root.root_thread_id,
                    from_message_id: first,
                },
            )
            .await
            .expect("branch");

        let abandoned = service
            .abandon_thread(branch.child_thread_id)
            .await
            .expect("abandon");
        assert_eq!(abandoned.state, "abandoned");
        assert_eq!(
            abandoned
                .branch
                .as_ref()
                .map(|lineage| lineage.state.as_str()),
            Some("abandoned")
        );
        let listed = service.list_branches(session_id).await.expect("list");
        assert_eq!(listed.branches[0].state, "abandoned");
        assert_eq!(
            ThreadStore::get(storage.as_ref(), branch.child_thread_id)
                .await
                .expect("thread")
                .expect("exists")
                .state,
            ThreadState::Abandoned
        );

        let events = storage
            .list_audit_events_by_session(session_id)
            .await
            .expect("audit");
        assert!(
            events
                .iter()
                .any(|event| event.event_kind == "thread_abandoned"
                    && event.entity_id == branch.child_thread_id.to_string())
        );

        // Abandoning again is a conflict (terminal state), and the root is
        // not a branch at all.
        assert!(matches!(
            service.abandon_thread(branch.child_thread_id).await,
            Err(GatewayError::Storage(StorageError::ConstraintViolation(_)))
        ));
        assert!(matches!(
            service.abandon_thread(root.root_thread_id).await,
            Err(GatewayError::ValidationError(_))
        ));

        // An abandoned branch takes no further turns.
        let refused = crate::thread_selection::resolve_thread_for_session(
            storage.as_ref(),
            session_id,
            Some(branch.child_thread_id),
        )
        .await;
        assert!(
            matches!(refused, Err(GatewayError::ValidationError(message)) if message.contains("abandoned"))
        );
    }
}
