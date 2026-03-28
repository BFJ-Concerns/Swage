use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;

use crate::{
    error::ApiError,
    service::{
        ConversationDetail, ConversationResponse, ConversationService, ConversationSummary,
        CreateProjectRequest, ProjectListResponse, ProjectResponse, ProjectService,
        UpdateProjectRequest,
    },
    state::AppState,
};

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

/// `GET /api/health` — lightweight startup and readiness check.
///
/// Returns `{"status": "ok"}` with no database queries or provider calls.
/// Used by process managers (systemd, wrapper scripts) to verify the gateway
/// is up and accepting connections before routing traffic to it.
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

// ---------------------------------------------------------------------------
// Wrapper type for list endpoint
// ---------------------------------------------------------------------------

/// Top-level response body for the list-conversations endpoint.
#[derive(Debug, Serialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationSummary>,
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `POST /api/conversations` — create a new conversation.
///
/// Returns 201 Created with the new conversation's metadata.
pub async fn create_conversation(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let resp: ConversationResponse = svc.create_conversation().await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/conversations` — list all conversations.
///
/// Returns 200 with `{ conversations: [...] }`.
pub async fn list_conversations(
    State(state): State<AppState>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let conversations = svc.list_conversations().await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// `GET /api/conversations/:id` — get a conversation with full message history.
///
/// Returns 200 with the conversation detail, or 404 if not found.
pub async fn get_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversationDetail>, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let detail = svc.get_conversation(id).await?;
    Ok(Json(detail))
}

/// `GET /api/projects/:project_id/sessions` — list sessions for a project.
///
/// Returns 200 with `{ conversations: [...] }`, or 404 if the project is not found.
pub async fn list_project_sessions(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let conversations = svc.list_sessions_by_project(project_id).await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// `POST /api/projects/:project_id/sessions` — create a session in a project.
///
/// Returns 201 Created with the new session's metadata, or 404 if the project is not found.
pub async fn create_project_session(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let resp = svc.create_session_in_project(project_id).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `DELETE /api/conversations/:id` — delete a conversation.
///
/// Returns 204 No Content on success, or 404 if not found.
pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    svc.delete_conversation(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Project handlers
// ---------------------------------------------------------------------------

/// `POST /api/projects` — create a new project.
///
/// Returns 201 Created with the project's metadata, or 409/422 on error.
pub async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.create_project(req).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/projects` — list all projects.
///
/// Returns 200 with `{ projects: [...] }`.
pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<ProjectListResponse>, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.list_projects().await?;
    Ok(Json(resp))
}

/// `GET /api/projects/:id` — get a project by ID.
///
/// Returns 200 with the project, or 404 if not found.
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ProjectResponse>, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.get_project(id).await?;
    Ok(Json(resp))
}

/// `PUT /api/projects/:id` — partially update a project.
///
/// Returns 200 with the updated project, 404 if not found, or 422/409 on error.
pub async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateProjectRequest>,
) -> Result<Json<ProjectResponse>, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.update_project(id, req).await?;
    Ok(Json(resp))
}

/// `DELETE /api/projects/:id` — delete a project.
///
/// Returns 204 No Content on success, 404 if not found, or 409 if refusing
/// to delete the default project.
pub async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = ProjectService::new(state.project_store);
    svc.delete_project(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Skills listing
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/skills`.
#[derive(Debug, Deserialize)]
pub struct SkillsQueryParams {
    /// ID of the project to list skills for (required).
    pub project_id: Uuid,
    /// Agent ID to include agent-level skills. When provided, the agent's
    /// directory is searched for a `skills/` folder.
    pub agent_id: Option<String>,
}

/// A single skill entry in the `GET /api/skills` response.
#[derive(Debug, Serialize)]
pub struct SkillItem {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    /// Hierarchy level where this skill was resolved: "agent", "project",
    /// "team", "lens", or "global".
    pub level: String,
    /// Tool names the skill requests access to.
    pub tools: Vec<String>,
    /// True for agent-level skills — they are always active and cannot be
    /// deactivated by the user.
    pub always_active: bool,
}

/// Response body for `GET /api/skills`.
#[derive(Debug, Serialize)]
pub struct SkillListResponse {
    pub skills: Vec<SkillItem>,
}

/// `GET /api/skills` — list skills available for a project/agent combination.
///
/// Resolves skills from the Swage config hierarchy (global → project → agent),
/// applying override rules when the same skill name appears at multiple levels.
/// Returns 200 with `{ skills: [...] }`, sorted agent-first then alphabetically.
///
/// Returns 404 if `project_id` does not correspond to a known project.
/// Returns 400 if `project_id` is missing or not a valid UUID (axum `Query`
/// extractor enforces this automatically).
///
/// When the Swage config root directory does not exist (e.g. on a fresh
/// install or in a test environment without `SWAGE_HOME`), returns an empty
/// skills list rather than an error.
pub async fn list_skills(
    State(state): State<AppState>,
    Query(params): Query<SkillsQueryParams>,
) -> Result<Json<SkillListResponse>, ApiError> {
    use swage_config::{HierarchyLevel, resolve_root};
    use swage_config::skill::discover_skills;
    use swage_storage::store::ProjectStore;

    // Verify the project exists — return 404 for unknown IDs.
    let project = state
        .project_store
        .get(params.project_id)
        .await
        .map_err(|e| ApiError(GatewayError::Storage(e)))?
        .ok_or_else(|| ApiError(GatewayError::ProjectNotFound { id: params.project_id }))?;

    // Resolve the config root. When it doesn't exist (no `~/.swage/` directory
    // and `SWAGE_HOME` unset or pointing at a missing path), degrade gracefully
    // by returning an empty list — skill discovery is a best-effort operation.
    let root = match resolve_root() {
        Ok(r) => r,
        Err(_) => return Ok(Json(SkillListResponse { skills: vec![] })),
    };

    // Build the hierarchy: global always included; project and agent directories
    // are included only when they exist on disk.
    let project_dir = root.join("projects").join(&project.slug);
    let mut levels: Vec<(HierarchyLevel, std::path::PathBuf)> = vec![
        (HierarchyLevel::Global, root.clone()),
    ];

    if project_dir.is_dir() {
        levels.push((HierarchyLevel::Project, project_dir.clone()));

        if let Some(ref agent_id) = params.agent_id {
            let agent_dir = project_dir.join("agents").join(agent_id);
            if agent_dir.is_dir() {
                levels.push((HierarchyLevel::Agent, agent_dir));
            }
        }
    }

    // discover_skills takes borrowed (Level, &Path) pairs — build the refs here.
    let level_refs: Vec<(HierarchyLevel, &std::path::Path)> = levels
        .iter()
        .map(|(l, p)| (l.clone(), p.as_path()))
        .collect();

    let (skills, _warnings) = discover_skills(&level_refs);

    let skill_items: Vec<SkillItem> = skills
        .into_iter()
        .map(|s| {
            let always_active = s.level == HierarchyLevel::Agent;
            let level = match s.level {
                HierarchyLevel::Agent   => "agent",
                HierarchyLevel::Team    => "team",
                HierarchyLevel::Project => "project",
                HierarchyLevel::Lens    => "lens",
                HierarchyLevel::Global  => "global",
            };
            SkillItem {
                name: s.name,
                display_name: s.display_name,
                description: s.description,
                level: level.to_string(),
                tools: s.tools,
                always_active,
            }
        })
        .collect();

    Ok(Json(SkillListResponse { skills: skill_items }))
}

/// `GET /api/turns/:id/context` — fetch the context assembly report for a completed turn.
///
/// Returns 200 with `{ turn_id, report }` if a snapshot is found. The `report`
/// field has the same shape as the `ContextAssembled` WebSocket event's report
/// field (a serialised `AssemblyReport`).
///
/// Returns 404 when no snapshot exists for the given turn — either the turn ID
/// is unknown or the turn completed before context assembly was enabled.
pub async fn get_turn_context(
    State(state): State<AppState>,
    Path(turn_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    use swage_storage::store::ContextSnapshotStore;

    let snapshot = state
        .storage
        .get_snapshot(turn_id)
        .await
        .map_err(|e| ApiError(GatewayError::Storage(e)))?;

    match snapshot {
        None => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "code": "not_found",
                "message": format!("no context snapshot found for turn {turn_id}"),
            })),
        )
            .into_response()),
        Some(report_json) => {
            // Parse back to Value so the HTTP response is clean JSON, not a
            // double-encoded string.
            let report: serde_json::Value = serde_json::from_str(&report_json)
                .map_err(|e| ApiError(GatewayError::ConfigError(e.to_string())))?;
            Ok(Json(serde_json::json!({
                "turn_id": turn_id,
                "report": report,
            }))
            .into_response())
        }
    }
}

/// `POST /api/shutdown` — initiate a graceful server shutdown.
///
/// # Localhost restriction
///
/// Only requests from `127.0.0.1` or `::1` are accepted. All other source
/// addresses receive `403 Forbidden`. When `ConnectInfo` is absent (e.g. in
/// unit tests that use `Router::oneshot` without a real TCP connection) the
/// restriction is bypassed — this is intentional test-environment behaviour.
///
/// # Behaviour
///
/// 1. Broadcasts `ServerEvent::Shutdown` to all connected WebSocket clients
///    so they can close gracefully before the connection is dropped.
/// 2. Cancels `AppState::shutdown_signal`, which the server's graceful-shutdown
///    future awaits alongside SIGTERM and the idle monitor.
/// 3. Returns `200 OK` with `{ status: "shutting_down", message }`.
pub async fn shutdown(
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    use crate::websocket::ServerEvent;

    // Enforce localhost-only access when a real TCP connection provides the
    // peer address. Without ConnectInfo (unit tests / same-process calls),
    // the restriction is not enforced.
    if let Some(ConnectInfo(addr)) = connect_info {
        if !addr.ip().is_loopback() {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "code": "forbidden",
                    "message": "shutdown is only accessible from localhost",
                })),
            )
                .into_response();
        }
    }

    let reason = "admin shutdown request".to_string();

    // Notify all connected WebSocket clients. Errors here are non-fatal:
    // if no clients are subscribed the send returns Err(SendError), which we
    // discard — the server is shutting down anyway.
    let _ = state.broadcast.send(ServerEvent::Shutdown { reason: reason.clone() });

    // Signal the graceful-shutdown future so axum drains in-flight requests
    // before the process exits.
    state.shutdown_signal.cancel();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "shutting_down",
            "message": format!("Gateway is shutting down: {reason}"),
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Programmatic interface — REST endpoints for async turn submission,
// approval decisions, agent messaging, and turn listing.
// ---------------------------------------------------------------------------

/// Request body for async turn submission.
#[derive(Debug, Deserialize)]
pub struct SubmitTurnRequest {
    /// The conversation (session) to submit the turn in.
    pub conversation_id: Uuid,
    /// The user message to process.
    pub message: String,
    /// Optional output schema that overrides the agent's configured schema
    /// for this single turn.
    pub output_schema: Option<serde_json::Value>,
}

/// Response for async turn submission.
#[derive(Debug, Serialize)]
pub struct SubmitTurnResponse {
    pub turn_id: Uuid,
    pub status: String,
}

/// Response for turn polling.
#[derive(Debug, Serialize)]
pub struct TurnPollResponse {
    pub turn_id: Uuid,
    pub state: String,
    pub assistant_response: Option<String>,
    pub structured_output: Option<serde_json::Value>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub parent_turn_id: Option<Uuid>,
}

/// `POST /api/turns` — submit a turn for async processing.
///
/// Returns 202 Accepted with the turn_id. The client polls GET /api/turns/:id
/// for the result. The turn is dispatched to the agent runtime in the background —
/// no WebSocket connection required.
pub async fn submit_turn(
    State(state): State<AppState>,
    Json(req): Json<SubmitTurnRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use swage_storage::model::NewTurn;
    use swage_storage::store::{SessionStore, ThreadStore, TurnStore};

    // Verify the session exists and find its thread.
    let _session = state
        .storage
        .get_session(req.conversation_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("conversation", req.conversation_id))?;

    let threads = state
        .storage
        .list_by_session(req.conversation_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let thread = threads
        .first()
        .ok_or_else(|| ApiError::internal("no thread found for session".into()))?;

    // Create a placeholder turn in Pending state — visible to the poller
    // immediately. The agent will create its own internal turn record during
    // processing; after completion we mirror the result onto this placeholder
    // so the REST client's poll resolves correctly.
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".into(),
            state: swage_storage::TurnState::Pending,
            parent_turn_id: None,
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let turn_id = turn.id;
    let thread_id = thread.id;

    // Spawn background turn processing — the REST client polls for result.
    let bg_state = state.clone();
    let message = req.message;
    let output_schema = req.output_schema;
    let session_id = req.conversation_id;
    tokio::spawn(async move {
        use crate::subscription::SubscriptionState;
        use swage_storage::store::TurnStore;
        use tokio::sync::{mpsc, Mutex};
        use std::sync::Arc;

        // Create a dummy WebSocket sender — events are discarded but the
        // turn processing infrastructure requires it.
        let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
        let subscription = Arc::new(Mutex::new(SubscriptionState::new(false)));

        // Drain events in background so the channel never fills.
        tokio::spawn(async move {
            while ws_rx.recv().await.is_some() {}
        });

        let result = crate::turn::process_turn(
            &bg_state,
            session_id,
            message,
            ws_tx,
            subscription,
            output_schema,
        )
        .await;

        // Mirror the agent's real turn result onto the placeholder so the
        // REST poller sees completion. The agent creates its own turn record
        // internally — find it and copy the result data across.
        match result {
            Ok(()) => {
                match bg_state.storage.list_turns_by_thread(thread_id).await {
                    Ok(turns) => {
                        // The real turn is the latest terminal turn that isn't
                        // the placeholder (different ID, created by the agent).
                        // Includes Interrupted so cooperatively cancelled turns
                        // propagate the correct state to the REST poller.
                        let real_turn = turns.iter().rev().find(|t| {
                            t.id != turn_id
                                && matches!(
                                    t.state,
                                    swage_storage::TurnState::Completed
                                        | swage_storage::TurnState::Failed
                                        | swage_storage::TurnState::Interrupted
                                )
                        });
                        if let Some(real) = real_turn {
                            let _ = bg_state.storage.update_turn_state(turn_id, real.state.clone()).await;
                            if let Some(ref resp) = real.assistant_response {
                                let _ = bg_state.storage.update_response(turn_id, resp).await;
                            }
                            if let (Some(input), Some(output)) = (real.input_tokens, real.output_tokens)
                            {
                                let _ = bg_state.storage.update_tokens(turn_id, input, output).await;
                            }
                            if let Some(ref so) = real.structured_output {
                                let _ = bg_state.storage.update_structured_output(turn_id, so).await;
                            }
                            if let Some(cost) = real.cost_microcents {
                                let _ = bg_state.storage.update_cost(turn_id, cost).await;
                            }
                        } else {
                            // No real turn found in storage — the agent likely failed
                            // before persisting. Mark as Failed so REST pollers see the
                            // failure rather than a misleading Completed status.
                            tracing::warn!(
                                turn_id = %turn_id,
                                "process_turn succeeded but no agent turn found; marking placeholder as failed"
                            );
                            let _ = bg_state
                                .storage
                                .update_turn_state(turn_id, swage_storage::TurnState::Failed)
                                .await;
                        }
                    }
                    Err(e) => {
                        // Storage error listing turns — mark the placeholder as
                        // failed so the REST poller isn't left hanging indefinitely.
                        tracing::warn!(
                            turn_id = %turn_id,
                            error = %e,
                            "failed to list turns for result mirroring; marking placeholder failed"
                        );
                        let _ = bg_state
                            .storage
                            .update_turn_state(turn_id, swage_storage::TurnState::Failed)
                            .await;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(turn_id = %turn_id, error = %e, "async turn processing failed");
                let _ = bg_state
                    .storage
                    .update_turn_state(turn_id, swage_storage::TurnState::Failed)
                    .await;
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitTurnResponse {
            turn_id,
            status: "pending".into(),
        }),
    ))
}

/// `GET /api/turns/:id` — poll a turn for status and result.
///
/// Returns the turn's current state. When completed, includes the assistant
/// response, structured output, and token counts.
pub async fn get_turn(
    State(state): State<AppState>,
    Path(turn_id): Path<Uuid>,
) -> Result<Json<TurnPollResponse>, ApiError> {
    use swage_storage::store::TurnStore;

    let turn = state
        .storage
        .get_turn(turn_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("turn", turn_id))?;

    Ok(Json(TurnPollResponse {
        turn_id: turn.id,
        state: turn.state.to_string(),
        assistant_response: turn.assistant_response,
        structured_output: turn.structured_output,
        input_tokens: turn.input_tokens,
        output_tokens: turn.output_tokens,
        parent_turn_id: turn.parent_turn_id,
    }))
}

/// Request body for sending an inter-agent message.
#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub sender_agent_id: String,
    pub recipient_agent_id: String,
    pub kind: String,
    pub correlation_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

/// Response for message operations.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: Uuid,
    pub status: String,
}

/// `POST /api/messages` — send a typed message to an agent.
///
/// Creates a durable message (Delegate, Result, Cancel, Shutdown) or an
/// ephemeral Notify. Returns the message ID and initial status.
pub async fn send_agent_message(
    State(state): State<AppState>,
    Json(req): Json<SendMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use swage_agent::MessageRouter;
    use swage_core::MessageKind;
    use swage_storage::model::NewAgentMessage;

    let kind: MessageKind = req
        .kind
        .parse()
        .map_err(|e: String| ApiError::validation(e))?;

    // Store-only path — live dispatch deferred. See BACKLOG.md [fvxd].
    let router = MessageRouter::new(state.storage.clone(), None);
    let msg = router
        .send_message(NewAgentMessage {
            sender_agent_id: req.sender_agent_id,
            recipient_agent_id: req.recipient_agent_id,
            kind,
            correlation_id: req.correlation_id,
            payload: req.payload,
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(MessageResponse {
            id: msg.id,
            status: msg.status.to_string(),
        }),
    ))
}

/// `GET /api/messages/:id` — query message delivery status.
pub async fn get_message_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MessageResponse>, ApiError> {
    use swage_storage::store::AgentMessageStore;

    let msg = state
        .storage
        .get_agent_message(id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("message", id))?;

    Ok(Json(MessageResponse {
        id: msg.id,
        status: msg.status.to_string(),
    }))
}

/// Approval listing response.
#[derive(Debug, Serialize)]
pub struct ApprovalListResponse {
    pub approvals: Vec<ApprovalSummary>,
}

/// Summary of a pending approval request.
#[derive(Debug, Serialize)]
pub struct ApprovalSummary {
    pub id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub kind: String,
    pub tool_name: Option<String>,
    pub prompt: String,
    pub timeout_at: Option<String>,
}

/// Query parameters for approval listing.
#[derive(Debug, Deserialize)]
pub struct ApprovalListQuery {
    pub agent_id: Option<String>,
}

// TODO list_approvals and list_turns return unbounded result sets — add pagination
// priority: normal | tags: review:inter-agent-comms
// Both endpoints fetch all matching records in a single query with no limit or
// cursor. As approval queues and conversation history grow this causes unbounded
// memory use, slow responses, and potential timeouts. Add a `limit`/`offset` or
// cursor query parameter and pass it through to the storage layer.
// ---
/// `GET /api/approvals` — list pending approval requests.
///
/// Optionally filtered by agent_id. Returns all pending approvals visible
/// to the caller.
pub async fn list_approvals(
    State(state): State<AppState>,
    Query(query): Query<ApprovalListQuery>,
) -> Result<Json<ApprovalListResponse>, ApiError> {
    use swage_storage::store::ApprovalStore;

    let approvals = if let Some(ref agent_id) = query.agent_id {
        state.storage.list_pending_by_agent(agent_id).await
    } else {
        state.storage.list_all_pending().await
    }
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let summaries = approvals
        .into_iter()
        .map(|a| ApprovalSummary {
            id: a.id,
            turn_id: a.turn_id,
            agent_id: a.agent_id,
            kind: a.kind.to_string(),
            tool_name: a.tool_name,
            prompt: a.prompt,
            timeout_at: Some(a.timeout_at.to_rfc3339()),
        })
        .collect();

    Ok(Json(ApprovalListResponse { approvals: summaries }))
}

/// The two valid approval outcomes.
///
/// Used for both REST and WebSocket decision paths. Parsed from the request
/// string early so invalid values surface as 422 before any side-effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApprovalOutcome {
    Approved,
    Rejected,
}

impl ApprovalOutcome {
    /// Parse a decision string, returning `None` for unrecognised values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

/// Request body for an approval decision.
#[derive(Debug, Deserialize)]
pub struct ApprovalDecisionRequest {
    /// "approved" or "rejected"
    pub decision: String,
    pub reason: Option<String>,
}

/// `POST /api/approvals/:id` — decide on a pending approval.
///
/// Routes through the same approval channel mechanism as WebSocket decisions.
/// Returns 200 on success, 404 if the approval doesn't exist, 409 if already decided.
pub async fn decide_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use crate::state::ApprovalDecision;

    // Validate the decision string early — before any side effects.
    let outcome = ApprovalOutcome::parse(&req.decision).ok_or_else(|| {
        ApiError::validation(format!(
            "invalid decision '{}': expected 'approved' or 'rejected'",
            req.decision,
        ))
    })?;

    // Try to fire the oneshot channel for this approval.
    let sender = state
        .approval_channels
        .lock()
        .await
        .remove(&approval_id);

    // Map the validated outcome to storage state once, used by all paths.
    let new_state = match outcome {
        ApprovalOutcome::Approved => swage_storage::model::ApprovalState::Approved,
        ApprovalOutcome::Rejected => swage_storage::model::ApprovalState::Rejected,
    };

    match sender {
        Some(tx) => {
            let decision = ApprovalDecision {
                decision: outcome.as_str().to_string(),
                reason: req.reason.clone(),
            };

            if tx.send(decision).is_ok() {
                // Agent received the decision via the live channel.
                Ok((
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "approval_id": approval_id,
                        "decision": outcome.as_str(),
                    })),
                ))
            } else {
                // Agent timed out or cancelled just before we sent —
                // persist the decision to storage so it isn't lost.
                use swage_storage::store::ApprovalStore;

                match state
                    .storage
                    .update_approval_state(approval_id, new_state, req.reason.as_deref())
                    .await
                {
                    Ok(true) => Ok((
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "approval_id": approval_id,
                            "decision": outcome.as_str(),
                            "note": "agent timed out; decision persisted to storage",
                        })),
                    )),
                    Ok(false) => Err(ApiError::conflict(format!(
                        "approval {} already resolved",
                        approval_id,
                    ))),
                    Err(e) => Err(ApiError::internal(e.to_string())),
                }
            }
        }
        None => {
            // No in-memory channel — either already resolved, or the agent
            // timed out / restarted. Persist the decision to storage directly
            // so recovered approvals are resolvable via the REST API.
            use swage_storage::store::ApprovalStore;

            let existing = state
                .storage
                .get_approval(approval_id)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;

            match existing {
                None => Err(ApiError::not_found("approval", approval_id)),
                Some(a) if a.state != swage_storage::model::ApprovalState::Pending => {
                    Err(ApiError::conflict(format!(
                        "approval {} already {}",
                        approval_id, a.state
                    )))
                }
                Some(_) => {
                    // Pending in storage with no live channel — persist the
                    // decision so the approval record reflects the outcome.
                    match state
                        .storage
                        .update_approval_state(approval_id, new_state, req.reason.as_deref())
                        .await
                    {
                        Ok(true) => Ok((
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "approval_id": approval_id,
                                "decision": outcome.as_str(),
                                "note": "no live channel; decision persisted to storage",
                            })),
                        )),
                        Ok(false) => Err(ApiError::conflict(format!(
                            "approval {} already resolved",
                            approval_id,
                        ))),
                        Err(e) => Err(ApiError::internal(e.to_string())),
                    }
                }
            }
        }
    }
}

/// Turn listing response for a conversation.
#[derive(Debug, Serialize)]
pub struct TurnListResponse {
    pub turns: Vec<TurnPollResponse>,
}

/// Query parameters for turn listing.
#[derive(Debug, Deserialize)]
pub struct TurnListQuery {
    pub state: Option<String>,
    pub agent_id: Option<String>,
}

/// `GET /api/conversations/:id/turns` — list turns in a conversation.
///
/// Returns turns for the default thread, optionally filtered by state and agent_id.
pub async fn list_turns(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<TurnListQuery>,
) -> Result<Json<TurnListResponse>, ApiError> {
    use swage_storage::store::{SessionStore, ThreadStore, TurnStore};

    // Verify session exists.
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("conversation", session_id))?;

    // Get the default thread.
    let threads = state
        .storage
        .list_by_session(session.id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let thread = threads
        .first()
        .ok_or_else(|| ApiError::internal("no thread found for session".into()))?;

    // List all turns in the thread.
    let mut turns = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Apply optional filters.
    if let Some(ref state_filter) = query.state {
        turns.retain(|t| t.state.to_string() == *state_filter);
    }
    if let Some(ref agent_filter) = query.agent_id {
        turns.retain(|t| t.agent_id == *agent_filter);
    }

    let responses = turns
        .into_iter()
        .map(|t| TurnPollResponse {
            turn_id: t.id,
            state: t.state.to_string(),
            assistant_response: t.assistant_response,
            structured_output: t.structured_output,
            input_tokens: t.input_tokens,
            output_tokens: t.output_tokens,
            parent_turn_id: t.parent_turn_id,
        })
        .collect();

    Ok(Json(TurnListResponse { turns: responses }))
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::SocketAddr, sync::Arc};

    use axum::{
        body::to_bytes,
        extract::ConnectInfo,
        http::Request,
        Router,
    };
    use tokio::sync::{broadcast, Mutex};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    use swage_storage::store::{ProjectStore, SessionStore, Storage};

    use super::*;
    use crate::{
        server::build_router,
        state::AppState,
        turn::tests::{MockProvider, MockStorage},
        websocket::ServerEvent,
    };

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build an axum `Router` backed by an empty `MockStorage`.
    ///
    /// Returns the router and the storage handle so tests can inspect state.
    fn make_router() -> (Router, Arc<MockStorage>) {
        let storage = Arc::new(MockStorage::new());
        let provider = Arc::new(MockProvider::new(vec![]));
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn Storage>,
            project_store: Arc::clone(&storage) as Arc<dyn ProjectStore>,
            provider: provider as Arc<dyn swage_provider::LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };
        (build_router(state), storage)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    // -----------------------------------------------------------------------
    // POST /api/conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_conversations_returns_201_with_json() {
        let (router, _storage) = make_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string(), "response should have a string id");
        assert!(body["project_id"].is_string());
        assert!(body["created_at"].is_string());
        assert!(body["last_activity"].is_string());
    }

    // -----------------------------------------------------------------------
    // GET /api/conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversations_returns_200_with_array() {
        let (router, _) = make_router();

        // Create two conversations first.
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let list = body["conversations"].as_array().unwrap();
        assert_eq!(list.len(), 2);
    }

    // -----------------------------------------------------------------------
    // GET /api/conversations/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversation_by_id_returns_200_with_messages() {
        let (router, _) = make_router();

        // Create a conversation.
        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_body = body_json(create_resp).await;
        let id = create_body["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/conversations/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["id"].as_str().unwrap(), id);
        assert!(body["messages"].is_array());
    }

    #[tokio::test]
    async fn get_non_existent_conversation_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/conversations/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // DELETE /api/conversations/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_conversation_returns_204() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_body = body_json(create_resp).await;
        let id = create_body["id"].as_str().unwrap();

        let del_resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/conversations/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
    }

    // -----------------------------------------------------------------------
    // POST /api/projects
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_projects_returns_201_with_json() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"name":"My Project"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string(), "response should have a string id");
        assert!(body["slug"].is_string(), "response should have a slug");
        assert_eq!(body["name"].as_str().unwrap(), "My Project");
    }

    #[tokio::test]
    async fn post_projects_empty_name_returns_422() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_projects_duplicate_name_returns_409() {
        let (router, _) = make_router();

        // First creation succeeds.
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Dup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Second with same name must conflict.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Dup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/projects
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_projects_returns_200_with_list() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/projects")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // At least the seeded default project.
        assert!(body["projects"].is_array());
    }

    // -----------------------------------------------------------------------
    // GET /api/projects/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_project_by_id_returns_200() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Fetched"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["id"].as_str().unwrap(), id);
    }

    #[tokio::test]
    async fn get_project_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // PUT /api/projects/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_project_returns_200_with_updated_fields() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Original"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/projects/{id}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Renamed"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["name"].as_str().unwrap(), "Renamed");
    }

    #[tokio::test]
    async fn put_project_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/projects/{missing}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"X"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // DELETE /api/projects/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_project_returns_204() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Deletable"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_default_project_returns_409() {
        use crate::service::ProjectService;

        let (router, storage) = make_router();
        // Get the default project's ID.
        let svc = ProjectService::new(Arc::clone(&storage) as Arc<dyn ProjectStore>);
        let default_project = svc.project_store.get_or_create_default().await.unwrap();
        let id = default_project.id;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/projects/:project_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_project_sessions_returns_200_with_array() {
        let (router, storage) = make_router();

        // Create a named project directly in storage.
        let project = storage
            .create(swage_storage::model::NewProject {
                name: "my-project".into(),
                slug: "my-project".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        // Add two sessions to that project.
        storage
            .create_session(swage_storage::model::NewSession { project_id: project.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession { project_id: project.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{}/sessions", project.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Gateway wraps sessions in a "conversations" key (frontend-compatible).
        let list = body["conversations"].as_array().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn get_project_sessions_scopes_to_project() {
        let (router, storage) = make_router();

        let project_a = storage
            .create(swage_storage::model::NewProject {
                name: "project-a".into(),
                slug: "project-a".into(),
                workspace_path: None,
            })
            .await
            .unwrap();
        let project_b = storage
            .create(swage_storage::model::NewProject {
                name: "project-b".into(),
                slug: "project-b".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        // One session in A, two in B.
        storage
            .create_session(swage_storage::model::NewSession { project_id: project_a.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession { project_id: project_b.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession { project_id: project_b.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{}/sessions", project_a.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp).await;
        let list = body["conversations"].as_array().unwrap();
        // Only sessions belonging to project A.
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn get_project_sessions_returns_404_for_unknown_project() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{missing}/sessions"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // POST /api/projects/:project_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_project_sessions_returns_201_with_json() {
        let (router, storage) = make_router();

        let project = storage
            .create(swage_storage::model::NewProject {
                name: "my-project".into(),
                slug: "my-project".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{}/sessions", project.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string());
        // Session should be associated with the specified project.
        assert_eq!(body["project_id"].as_str().unwrap(), project.id.to_string());
    }

    #[tokio::test]
    async fn post_project_sessions_returns_404_for_unknown_project() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{missing}/sessions"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    #[tokio::test]
    async fn delete_non_existent_conversation_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/conversations/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // GET /api/turns/:id/context
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_turn_context_returns_404_for_unknown_turn() {
        let (router, _) = make_router();
        let unknown_id = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/turns/{unknown_id}/context"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // Helpers specific to shutdown tests
    // -----------------------------------------------------------------------

    /// Variant of `make_router` that also returns the shutdown signal and a
    /// broadcast receiver so shutdown tests can observe both side-effects.
    fn make_shutdown_router() -> (
        Router,
        Arc<MockStorage>,
        CancellationToken,
        broadcast::Receiver<ServerEvent>,
    ) {
        let storage = Arc::new(MockStorage::new());
        let provider = Arc::new(MockProvider::new(vec![]));
        let (broadcast_tx, broadcast_rx) = broadcast::channel(16);
        let shutdown_signal = CancellationToken::new();
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn Storage>,
            project_store: Arc::clone(&storage) as Arc<dyn ProjectStore>,
            provider: provider as Arc<dyn swage_provider::LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: broadcast_tx,
            shutdown_signal: shutdown_signal.clone(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };
        (build_router(state), storage, shutdown_signal, broadcast_rx)
    }

    // -----------------------------------------------------------------------
    // POST /api/shutdown
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn shutdown_from_localhost_returns_200() {
        let (router, _, _, _) = make_shutdown_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"].as_str().unwrap(), "shutting_down");
        assert!(body["message"].is_string());
    }

    #[tokio::test]
    async fn shutdown_from_non_localhost_returns_403() {
        let (router, _, _, _) = make_shutdown_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    // Simulate a request from a remote IP.
                    .extension(ConnectInfo(SocketAddr::from(([192, 168, 1, 100], 54321))))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "forbidden");
    }

    #[tokio::test]
    async fn shutdown_cancels_the_shutdown_signal() {
        let (router, _, shutdown_signal, _) = make_shutdown_router();

        assert!(
            !shutdown_signal.is_cancelled(),
            "signal should not be cancelled before the request"
        );

        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            shutdown_signal.is_cancelled(),
            "signal should be cancelled after the shutdown request"
        );
    }

    #[tokio::test]
    async fn shutdown_broadcasts_shutdown_event() {
        let (router, _, _, mut broadcast_rx) = make_shutdown_router();

        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Handler should have sent exactly one Shutdown event to the channel.
        let event = broadcast_rx
            .try_recv()
            .expect("broadcast channel should have received a Shutdown event");
        assert!(
            matches!(event, ServerEvent::Shutdown { .. }),
            "expected a Shutdown event, got: {event:?}"
        );
    }

    #[tokio::test]
    async fn get_turn_context_returns_200_with_report() {
        use swage_storage::store::ContextSnapshotStore;

        let (router, storage) = make_router();
        let turn_id = Uuid::new_v4();
        let report_json = r#"{"total_tokens":1024,"elements":[]}"#;

        // Store a snapshot directly — simulates what the context engine writes.
        storage.store_snapshot(turn_id, report_json).await.unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/turns/{turn_id}/context"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["turn_id"].as_str().unwrap(), turn_id.to_string());
        assert_eq!(body["report"]["total_tokens"].as_i64().unwrap(), 1024);
    }
}
