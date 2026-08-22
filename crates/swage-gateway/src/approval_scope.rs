use swage_agent::approval::{ApprovalDecisionScope, ApprovalDecisionValue};
use swage_storage::model::{ApprovalRequest, NewWorkspaceAttachment, Space};

use crate::{error::ApiError, state::AppState};

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
            let grant = if approval.kind == swage_storage::model::ApprovalKind::Location {
                let (path, access) = location_context(approval)?;
                crate::state::ApprovalGrantKey::location(None, path, access)
            } else {
                let Some(grant) = crate::state::ApprovalGrantKey::from_approval(approval) else {
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
                        let space = approval_space(state, approval).await?;
                        state
                            .session_approval_grants
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(crate::state::ApprovalGrantKey::location(
                                Some(space.id),
                                path.clone(),
                                access.clone(),
                            ));
                        persist_location_grant_for_space(state, &space, &path, &access).await
                    }
                    ApprovalDecisionScope::PermanentGlobal => {
                        state
                            .session_approval_grants
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(crate::state::ApprovalGrantKey::location(
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
                    let space = approval_space(state, approval).await?;
                    let config =
                        crate::turn::load_session_config(state, &space.slug, &approval.agent_id)
                            .map_err(ApiError::from)?;
                    state
                        .session_approval_grants
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(crate::state::ApprovalGrantKey {
                            space_id: Some(space.id),
                            kind: swage_storage::model::ApprovalKind::Tool,
                            name: tool_name.to_string(),
                            access: None,
                        });
                    crate::settings::persist_permanent_tool_approval_for_space(
                        &state.config_root,
                        &config,
                        &space,
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
                            space_id: None,
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

async fn persist_location_grant_for_space(
    state: &AppState,
    space: &Space,
    path: &str,
    access: &str,
) -> Result<(), ApiError> {
    let existing = state
        .storage
        .list_workspace_attachments_by_space(space.id)
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
            space_id: space.id,
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
    let spaces = state
        .space_store
        .list_all()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    for space in spaces {
        persist_location_grant_for_space(state, &space, path, access).await?;
    }
    Ok(())
}

async fn approval_space(state: &AppState, approval: &ApprovalRequest) -> Result<Space, ApiError> {
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
    let session = state
        .storage
        .get_session(thread.session_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("session", thread.session_id))?;

    state
        .space_store
        .get(session.space_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("space", session.space_id))
}
