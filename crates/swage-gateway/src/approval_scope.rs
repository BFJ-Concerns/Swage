use swage_agent::approval::{ApprovalDecisionScope, ApprovalDecisionValue};
use swage_storage::model::{ApprovalRequest, NewWorkspaceAttachment, Topic};

use crate::{error::ApiError, state::AppState};

/// Validate and claim a waiting call in one map operation. A claimed slot
/// remains present until the agent settles its row, so a competing decision
/// cannot mistake an in-flight decision for an abandoned gate.
pub(crate) async fn claim_approval_waiter(
    state: &AppState,
    approval: &ApprovalRequest,
) -> Result<tokio::sync::oneshot::Sender<swage_agent::ApprovalDecision>, ApiError> {
    use swage_storage::{ApprovalState, TurnState};

    if approval.state != ApprovalState::Pending {
        return Err(ApiError::conflict(format!(
            "approval request is {}; retry the operation",
            approval.state
        )));
    }
    let turn = state
        .storage
        .get_turn(approval.turn_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let has_waiter = {
        let mut channels = state
            .approval_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let slot = channels.get_mut(&approval.id);
        if slot.as_ref().is_some_and(|slot| slot.sender.is_none()) {
            return Err(ApiError::conflict(
                "approval request is already decided".to_string(),
            ));
        }
        let has_waiter = slot.as_ref().is_some_and(|slot| {
            slot.sender
                .as_ref()
                .is_some_and(|sender| !sender.is_closed())
        });
        if approval.timeout_at > chrono::Utc::now()
            && has_waiter
            && turn
                .as_ref()
                .is_some_and(|turn| turn.state == TurnState::AwaitingApproval)
        {
            return Ok(slot
                .and_then(|slot| slot.sender.take())
                .expect("live waiter"));
        }
        // Dropping an unclaimed sender wakes its gate through the timeout path.
        channels.remove(&approval.id);
        has_waiter
    };
    expire_approval(state, approval, if has_waiter { None } else { turn }).await?;
    Err(ApiError::conflict(
        "approval request expired; retry the operation".to_string(),
    ))
}

/// Failed delivery and genuinely abandoned gates may expire only a pending row.
pub(crate) async fn expire_approval(
    state: &AppState,
    approval: &ApprovalRequest,
    abandoned_turn: Option<swage_storage::Turn>,
) -> Result<(), ApiError> {
    use swage_storage::{ApprovalState, TurnState};
    let expired = state
        .storage
        .update_approval_state(
            approval.id,
            ApprovalState::Expired,
            Some("approval timed out"),
        )
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if !expired {
        let current = state
            .storage
            .get_approval(approval.id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
            .ok_or_else(|| ApiError::not_found("approval", approval.id))?;
        return Err(ApiError::conflict(format!(
            "approval request is {}; retry the operation",
            current.state
        )));
    }
    if let Some(turn) = abandoned_turn
        && turn.state == TurnState::AwaitingApproval
    {
        state
            .storage
            .update_turn_state(turn.id, TurnState::Interrupted)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        if state
            .storage
            .get(turn.thread_id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
            .is_some_and(|thread| thread.state != swage_storage::ThreadState::Idle)
        {
            state
                .storage
                .update_state(turn.thread_id, swage_storage::ThreadState::Idle)
                .await
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
    }
    Ok(())
}

pub(crate) async fn apply_approval_scope(
    state: &AppState,
    approval: &ApprovalRequest,
    decision: ApprovalDecisionValue,
    scope: ApprovalDecisionScope,
) -> Result<(), ApiError> {
    if decision != ApprovalDecisionValue::Approved {
        return Ok(());
    }

    match scope {
        ApprovalDecisionScope::Once => Ok(()),
        ApprovalDecisionScope::Session => {
            let session_id = approval_session_id(state, approval).await?;
            let grant = if approval.kind == swage_storage::model::ApprovalKind::Location {
                let (path, access) = location_context(approval)?;
                crate::state::ApprovalGrantKey::location(Some(session_id), None, path, access)
            } else {
                let Some(grant) =
                    crate::state::ApprovalGrantKey::from_approval(approval, Some(session_id))
                else {
                    return Err(ApiError::validation(
                        "session approval requires a permission target".to_string(),
                    ));
                };
                grant
            };
            state
                .session_approval_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(grant);
            Ok(())
        }
        ApprovalDecisionScope::Permanent | ApprovalDecisionScope::PermanentGlobal => {
            if approval.kind == swage_storage::model::ApprovalKind::Location {
                let (path, access) = location_context(approval)?;
                return match scope {
                    ApprovalDecisionScope::Permanent => {
                        let topic = approval_topic(state, approval).await?;
                        state
                            .session_approval_grants
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(crate::state::ApprovalGrantKey::location(
                                None,
                                Some(topic.id),
                                path.clone(),
                                access.clone(),
                            ));
                        persist_location_grant_for_topic(state, &topic, &path, &access).await
                    }
                    ApprovalDecisionScope::PermanentGlobal => {
                        state
                            .session_approval_grants
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(crate::state::ApprovalGrantKey::location(
                                None,
                                None,
                                path.clone(),
                                access.clone(),
                            ));
                        persist_location_grant_globally(state, &path, &access).await
                    }
                    ApprovalDecisionScope::Once | ApprovalDecisionScope::Session => unreachable!(),
                };
            }

            let Some(tool_name) = approval.tool_name.as_deref() else {
                return Err(ApiError::validation(
                    "permanent approval can only be applied to tool permissions".to_string(),
                ));
            };

            match scope {
                ApprovalDecisionScope::Permanent => {
                    let topic = approval_topic(state, approval).await?;
                    let config = crate::turn::load_session_config(
                        state,
                        &topic.location(),
                        &approval.agent_id,
                    )
                    .map_err(ApiError::from)?;
                    state
                        .session_approval_grants
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(crate::state::ApprovalGrantKey {
                            session_id: None,
                            topic_id: Some(topic.id),
                            kind: swage_storage::model::ApprovalKind::Tool,
                            name: tool_name.to_string(),
                            access: None,
                        });
                    crate::settings::persist_permanent_tool_approval_for_topic(
                        &state.config_root,
                        &config,
                        &topic,
                        tool_name,
                    )
                    .await
                }
                ApprovalDecisionScope::PermanentGlobal => {
                    state
                        .session_approval_grants
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(crate::state::ApprovalGrantKey {
                            session_id: None,
                            topic_id: None,
                            kind: swage_storage::model::ApprovalKind::Tool,
                            name: tool_name.to_string(),
                            access: None,
                        });
                    crate::settings::persist_permanent_tool_approval_globally(state, tool_name)
                        .await
                }
                ApprovalDecisionScope::Once | ApprovalDecisionScope::Session => unreachable!(),
            }
        }
    }
}

fn location_context(approval: &ApprovalRequest) -> Result<(String, String), ApiError> {
    let context = approval
        .context
        .as_ref()
        .ok_or_else(|| ApiError::validation("location approval is missing context".to_string()))?;
    let path = context
        .get("path")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::validation("location approval is missing path".to_string()))?;
    let access = context
        .get("access")
        .and_then(|value| value.as_str())
        .filter(|value| matches!(*value, "read" | "write"))
        .ok_or_else(|| {
            ApiError::validation("location approval access must be read or write".to_string())
        })?;
    Ok((path.to_string(), access.to_string()))
}

async fn persist_location_grant_for_topic(
    state: &AppState,
    topic: &Topic,
    path: &str,
    access: &str,
) -> Result<(), ApiError> {
    let existing = state
        .storage
        .list_workspace_attachments_by_topic(topic.id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .into_iter()
        .find(|workspace| workspace.canonical_path == path || workspace.path == path);

    if let Some(workspace) = existing {
        if !workspace
            .permissions
            .iter()
            .any(|permission| permission == access)
        {
            return Err(ApiError::validation(format!(
                "workspace attachment already exists without {access} permission"
            )));
        }
        state
            .storage
            .set_workspace_trust(workspace.id, true)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        return Ok(());
    }

    state
        .storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            topic_id: topic.id,
            label: format!("Approved {}", access),
            path: path.to_string(),
            permissions: vec![access.to_string()],
            is_trusted: true,
        })
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(())
}

async fn persist_location_grant_globally(
    state: &AppState,
    path: &str,
    access: &str,
) -> Result<(), ApiError> {
    // A global grant reaches every scope, the general scope included.
    let scopes = state
        .topic_store
        .list_scopes()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    for topic in scopes {
        persist_location_grant_for_topic(state, &topic, path, access).await?;
    }
    Ok(())
}

async fn approval_topic(state: &AppState, approval: &ApprovalRequest) -> Result<Topic, ApiError> {
    let session_id = approval_session_id(state, approval).await?;
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("session", session_id))?;

    state
        .topic_store
        .get(session.topic_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("topic", session.topic_id))
}

async fn approval_session_id(
    state: &AppState,
    approval: &ApprovalRequest,
) -> Result<uuid::Uuid, ApiError> {
    let turn = state
        .storage
        .get_turn(approval.turn_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("turn", approval.turn_id))?;
    let thread = state
        .storage
        .get(turn.thread_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("thread", turn.thread_id))?;
    Ok(thread.session_id)
}
