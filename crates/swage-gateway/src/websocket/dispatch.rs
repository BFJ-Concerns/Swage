use std::{str::FromStr, sync::Arc};

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use swage_agent::approval::ApprovalDecisionValue;

use crate::{state::AppState, subscription::SubscriptionState, turn};

use super::{ClientEvent, ServerEvent, error_codes};

#[cfg(test)]
pub(crate) static TEST_SKILL_ROOTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<Uuid, std::path::PathBuf>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Discover skills available to a session.
///
/// Resolves the Swage root, looks up the session's space slug and agent id,
/// builds the config hierarchy, and delegates to [`crate::skill::SkillManager`].
/// Infrastructure failures are returned to the caller so slash-command and
/// explicit activation flows can surface the real problem instead of treating
/// the outage as an empty skill catalogue.
pub(crate) async fn discover_available_for_session(
    state: &AppState,
    session_id: Uuid,
) -> Result<Vec<swage_config::skill::SkillDefinition>, String> {
    // Fetch session to get space_id and agent_id.
    let session = match state.storage.get_session(session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err(format!(
                "skill discovery failed: session {session_id} was not found"
            ));
        }
        Err(e) => {
            return Err(format!(
                "skill discovery failed while loading session {session_id}: {e}"
            ));
        }
    };

    let root = skill_root_for_session(session_id).unwrap_or_else(|| state.config_root.clone());
    crate::skill::discover_available_for_space(
        state,
        session.space_id,
        Some(&session.agent_id),
        &root,
    )
    .await
    .map_err(|error| format!("skill discovery failed for session {session_id}: {error}"))
}

#[cfg(test)]
fn skill_root_for_session(session_id: Uuid) -> Option<std::path::PathBuf> {
    TEST_SKILL_ROOTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&session_id)
        .cloned()
}

#[cfg(not(test))]
fn skill_root_for_session(_session_id: Uuid) -> Option<std::path::PathBuf> {
    None
}

#[cfg(test)]
pub(crate) struct SessionSkillRootGuard {
    session_id: Uuid,
    _root: Option<tempfile::TempDir>,
}

#[cfg(test)]
impl SessionSkillRootGuard {
    pub(crate) fn at(session_id: Uuid, root: &std::path::Path) -> Self {
        TEST_SKILL_ROOTS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id, root.to_path_buf());
        Self {
            session_id,
            _root: None,
        }
    }

    pub(crate) fn empty(session_id: Uuid) -> Self {
        let root = tempfile::TempDir::new().expect("temp skill root");
        TEST_SKILL_ROOTS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id, root.path().to_path_buf());
        Self {
            session_id,
            _root: Some(root),
        }
    }
}

#[cfg(test)]
impl Drop for SessionSkillRootGuard {
    fn drop(&mut self) {
        TEST_SKILL_ROOTS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.session_id);
    }
}

/// Parse and dispatch a raw text frame from the client.
///
/// Subscribe/Unsubscribe update per-connection [`SubscriptionState`].
/// SendMessage intercepts slash commands before spawning a turn.
/// ActivateSkill and DeactivateSkill delegate to [`crate::skill::SkillManager`].
pub(super) async fn dispatch_client_message(
    text: &str,
    state: &AppState,
    session_id: Uuid,
    tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
) {
    use crate::skill::{SkillActivationResult, SkillError, SkillManager, SlashCommandResult};
    use swage_context::tokeniser::TiktokenTokeniser;

    let event = match serde_json::from_str::<ClientEvent>(text) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(%session_id, error = %e, raw = text, "unparseable client event");
            let _ = tx.send(ServerEvent::Error {
                code: error_codes::INTERNAL_ERROR.to_string(),
                message: format!("invalid event: {e}"),
            });
            return;
        }
    };

    match event {
        ClientEvent::Subscribe { channels } => {
            // Update subscription state — unknown channel names are silently ignored.
            subscription.lock().await.subscribe(&channels);
        }
        ClientEvent::Unsubscribe { channels } => {
            subscription.lock().await.unsubscribe(&channels);
        }

        ClientEvent::SendMessage {
            content,
            thread_id,
            selected_model_id,
        } => {
            if let Some(selected_model_id) = normalise_selected_model_id(selected_model_id)
                && let Err(error) = state
                    .storage
                    .set_session_selected_model(session_id, Some(selected_model_id))
                    .await
            {
                let _ = tx.send(ServerEvent::Error {
                    code: error_codes::INTERNAL_ERROR.to_string(),
                    message: error.to_string(),
                });
                return;
            }

            // Check for a slash command before deciding whether to spawn a turn.
            match crate::skill::parse_slash_command(&content) {
                SlashCommandResult::SkillActivation {
                    skill_name,
                    remaining_text,
                } => {
                    let available = match discover_available_for_session(state, session_id).await {
                        Ok(available) => available,
                        Err(message) => {
                            tracing::warn!(%session_id, %message, "skill discovery failed");
                            let _ = tx.send(ServerEvent::Error {
                                code: error_codes::INTERNAL_ERROR.to_string(),
                                message,
                            });
                            return;
                        }
                    };
                    let is_known = available.iter().any(|s| s.name == skill_name);

                    if is_known {
                        // Activate the skill and forward any trailing text as a turn.
                        let config = state.active_config();
                        let tok = TiktokenTokeniser::new(config.default_model_id());
                        match SkillManager::activate(
                            session_id,
                            &skill_name,
                            &available,
                            state.storage.as_ref(),
                            &tok,
                        )
                        .await
                        {
                            Ok(SkillActivationResult::Activated {
                                name,
                                display_name,
                                description,
                                token_count,
                            }) => {
                                let _ = tx.send(ServerEvent::SkillActivated {
                                    name,
                                    display_name,
                                    description,
                                    token_count,
                                });
                            }
                            Ok(SkillActivationResult::AlreadyActive) => {
                                let _ = tx.send(ServerEvent::SkillAlreadyActive {
                                    name: skill_name.clone(),
                                });
                            }
                            Err(SkillError::NotFound(n)) => {
                                tracing::warn!(%session_id, skill = %skill_name, "slash command skill not found");
                                let _ = tx.send(ServerEvent::Error {
                                    code: error_codes::SKILL_NOT_FOUND.to_string(),
                                    message: format!("skill '{n}' not found"),
                                });
                            }
                            Err(e) => {
                                tracing::warn!(%session_id, skill = %skill_name, error = ?e, "slash command skill activation failed");
                                let _ = tx.send(ServerEvent::Error {
                                    code: error_codes::INTERNAL_ERROR.to_string(),
                                    message: "skill activation failed".to_string(),
                                });
                            }
                        }

                        // If the slash command had trailing text, forward it as a turn message.
                        if let Some(text) = remaining_text {
                            let state = state.clone();
                            let sub = Arc::clone(&subscription);
                            tokio::spawn(async move {
                                if let Err(e) = turn::process_turn(
                                    &state,
                                    session_id,
                                    thread_id,
                                    text,
                                    tx.clone(),
                                    sub,
                                    None,
                                )
                                .await
                                {
                                    let (code, message) = classify_gateway_error(&e);
                                    let _ = tx.send(ServerEvent::Error { code, message });
                                }
                            });
                        }
                        // Bare `/skill-name` (no trailing text) completes with activation only.
                    } else {
                        // Unknown skill — pass the original message through unchanged.
                        let state = state.clone();
                        let sub = Arc::clone(&subscription);
                        tokio::spawn(async move {
                            if let Err(e) = turn::process_turn(
                                &state,
                                session_id,
                                thread_id,
                                content,
                                tx.clone(),
                                sub,
                                None,
                            )
                            .await
                            {
                                let (code, message) = classify_gateway_error(&e);
                                let _ = tx.send(ServerEvent::Error { code, message });
                            }
                        });
                    }
                }
                SlashCommandResult::NotACommand => {
                    // Regular message — spawn the turn as normal.
                    let state = state.clone();
                    let sub = Arc::clone(&subscription);
                    tokio::spawn(async move {
                        if let Err(e) = turn::process_turn(
                            &state,
                            session_id,
                            thread_id,
                            content,
                            tx.clone(),
                            sub,
                            None,
                        )
                        .await
                        {
                            let (code, message) = classify_gateway_error(&e);
                            let _ = tx.send(ServerEvent::Error { code, message });
                        }
                    });
                }
            }
        }

        ClientEvent::Cancel {} => {
            if let Err(e) = turn::cancel_turn(state, session_id).await {
                tracing::warn!(%session_id, error = %e, "cancel_turn failed");
                let _ = tx.send(ServerEvent::Error {
                    code: error_codes::INTERNAL_ERROR.to_string(),
                    message: e.to_string(),
                });
            }
        }

        ClientEvent::ActivateSkill { name } => {
            let available = match discover_available_for_session(state, session_id).await {
                Ok(available) => available,
                Err(message) => {
                    tracing::warn!(%session_id, skill = %name, %message, "skill discovery failed");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message,
                    });
                    return;
                }
            };
            let config = state.active_config();
            let tok = TiktokenTokeniser::new(config.default_model_id());

            match SkillManager::activate(
                session_id,
                &name,
                &available,
                state.storage.as_ref(),
                &tok,
            )
            .await
            {
                Ok(SkillActivationResult::Activated {
                    name,
                    display_name,
                    description,
                    token_count,
                }) => {
                    let _ = tx.send(ServerEvent::SkillActivated {
                        name,
                        display_name,
                        description,
                        token_count,
                    });
                }
                Ok(SkillActivationResult::AlreadyActive) => {
                    let _ = tx.send(ServerEvent::SkillAlreadyActive { name });
                }
                Err(SkillError::NotFound(n)) => {
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::SKILL_NOT_FOUND.to_string(),
                        message: format!("skill '{n}' not found"),
                    });
                }
                Err(e) => {
                    tracing::error!(%session_id, skill = %name, error = ?e, "skill activation error");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "skill activation failed".to_string(),
                    });
                }
            }
        }

        ClientEvent::DeactivateSkill { name } => {
            let available = match discover_available_for_session(state, session_id).await {
                Ok(available) => available,
                Err(message) => {
                    tracing::warn!(%session_id, skill = %name, %message, "skill discovery failed");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message,
                    });
                    return;
                }
            };

            match SkillManager::deactivate(session_id, &name, &available, state.storage.as_ref())
                .await
            {
                Ok(()) => {
                    let _ = tx.send(ServerEvent::SkillDeactivated { name });
                }
                Err(SkillError::AgentLevelCannotDeactivate(n)) => {
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: format!("Cannot deactivate agent-level skill '{n}'"),
                    });
                }
                Err(e) => {
                    tracing::error!(%session_id, skill = %name, error = ?e, "skill deactivation error");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "skill deactivation failed".to_string(),
                    });
                }
            }
        }

        ClientEvent::ApprovalDecision {
            approval_id,
            decision,
            scope,
            reason,
        } => {
            // The shared wire contract deserialises the identifier as a UUID,
            // so malformed values are rejected before dispatch.
            let approval_uuid = approval_id;

            // Step 2: Validate that the decision string is one of the two legal values.
            if decision != "approved" && decision != "rejected" {
                let _ = tx.send(ServerEvent::ApprovalDecisionError {
                    approval_id: approval_uuid,
                    error: "invalid_decision".to_string(),
                    message: format!(
                        "decision must be \"approved\" or \"rejected\", got \"{decision}\"."
                    ),
                    current_state: None,
                });
                return;
            }

            // Step 3: Look up the approval record in storage.
            let approval = match state.storage.get_approval(approval_uuid).await {
                Ok(Some(a)) => a,
                Ok(None) => {
                    let _ = tx.send(ServerEvent::ApprovalDecisionError {
                        approval_id: approval_uuid,
                        error: "not_found".to_string(),
                        message: "No approval request found with that ID.".to_string(),
                        current_state: None,
                    });
                    return;
                }
                Err(e) => {
                    tracing::error!(%session_id, %approval_uuid, err = %e, "storage error looking up approval");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "Failed to look up approval request.".to_string(),
                    });
                    return;
                }
            };

            // Step 4: Atomically transition state from Pending → decided.
            //
            // `update_approval_state` uses an optimistic WHERE state='pending'
            // guard and returns false if the record was already resolved —
            // either by a concurrent client or by the timeout mechanism.
            let new_state = if decision == "approved" {
                swage_storage::ApprovalState::Approved
            } else {
                swage_storage::ApprovalState::Rejected
            };
            let Ok(decision_value) = ApprovalDecisionValue::from_str(&decision) else {
                tracing::error!(%session_id, %approval_uuid, decision = %decision, "invalid approval decision after validation");
                let _ = tx.send(ServerEvent::Error {
                    code: error_codes::INTERNAL_ERROR.to_string(),
                    message: "Failed to record approval decision.".to_string(),
                });
                return;
            };
            match state
                .storage
                .update_approval_state(approval_uuid, new_state, reason.as_deref())
                .await
            {
                Ok(true) => {} // Recorded successfully.
                Ok(false) => {
                    // Concurrent decision or expiry beat us to it.
                    let _ = tx.send(ServerEvent::ApprovalDecisionError {
                        approval_id: approval_uuid,
                        error: "already_decided".to_string(),
                        message: "This approval request has already been resolved.".to_string(),
                        current_state: Some(approval.state.to_string()),
                    });
                    return;
                }
                Err(e) => {
                    tracing::error!(%session_id, %approval_uuid, err = %e, "storage error updating approval state");
                    let _ = tx.send(ServerEvent::Error {
                        code: error_codes::INTERNAL_ERROR.to_string(),
                        message: "Failed to record approval decision.".to_string(),
                    });
                    return;
                }
            }

            // Step 5: Notify the waiting agent task via the in-process oneshot.
            //
            // Remove (not just read) the sender so only one decision fires through.
            // If no sender is present the agent either timed out or never registered —
            // proceed to emit the confirmed event anyway for UI consistency.
            {
                let mut channels = state
                    .approval_channels
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(sender) = channels.remove(&approval_uuid) {
                    let _ = sender.send(swage_agent::ApprovalDecision {
                        decision: decision_value,
                        scope,
                        reason: reason.clone(),
                    });
                }
            }

            if let Err(error) =
                crate::approval_scope::apply_approval_scope(state, &approval, decision_value, scope)
                    .await
            {
                tracing::error!(%session_id, %approval_uuid, err = %error.0, "approval scope update failed");
                let _ = tx.send(ServerEvent::Error {
                    code: error_codes::INTERNAL_ERROR.to_string(),
                    message: "Failed to save approval scope.".to_string(),
                });
                return;
            }

            // Step 6: Broadcast confirmation to the client.
            let _ = tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval_uuid,
                decision,
                reason,
            });
        }
    }
}

/// Map a `GatewayError` to a (code, message) pair suitable for a client Error event.
pub(super) fn classify_gateway_error(err: &swage_core::GatewayError) -> (String, String) {
    use swage_core::GatewayError;
    match err {
        GatewayError::TurnInProgress { .. } => {
            (error_codes::TURN_IN_PROGRESS.to_string(), err.to_string())
        }
        GatewayError::Provider(_) => (error_codes::PROVIDER_ERROR.to_string(), err.to_string()),
        GatewayError::ConfigError(_) => (error_codes::INVALID_CONFIG.to_string(), err.to_string()),
        _ => (error_codes::INTERNAL_ERROR.to_string(), err.to_string()),
    }
}

pub(super) fn normalise_selected_model_id(selected_model_id: Option<String>) -> Option<String> {
    selected_model_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
