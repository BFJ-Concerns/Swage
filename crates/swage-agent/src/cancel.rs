//! Cancel and Shutdown message handling for inter-agent communication.
//!
//! Cancel interrupts a specific delegation (by correlation_id).
//! Shutdown terminates all active work under a target agent.

use std::sync::Arc;

use swage_core::MessageKind;
use swage_storage::model::AgentMessage;
use swage_storage::store::{AgentMessageStore, TurnStore};
use swage_storage::{Storage, TurnState};

use crate::delegation::ActiveDelegations;
use crate::error::AgentError;

/// Handle a Cancel message by interrupting the delegated turn.
///
/// When `active_delegations` is provided, the handler looks up the exact
/// delegation by message ID — triggering its cancellation token (stopping
/// the in-memory execution) and interrupting only the turns in that
/// delegation's thread. Without an active delegation entry (e.g. when the
/// execution already completed), falls back to `find_in_progress_by_agent`.
///
/// The cancel message itself is already claimed (delivered) by the mailbox.
/// If the target turn is not found or already in a terminal state, the cancel
/// is a no-op — "first terminal state wins" (ADR-0002).
pub async fn handle_cancel(
    storage: &Arc<dyn Storage>,
    cancel_msg: &AgentMessage,
    active_delegations: Option<&ActiveDelegations>,
) -> Result<(), AgentError> {
    debug_assert_eq!(cancel_msg.kind, MessageKind::Cancel);

    let Some(correlation_id) = cancel_msg.correlation_id else {
        return Err(AgentError::InvalidConfig(
            "Cancel message missing correlation_id".into(),
        ));
    };

    // The correlation_id on a Cancel references the original Delegate's message ID.
    let delegate = storage.get_agent_message(correlation_id).await?;

    let Some(delegate) = delegate else { return Ok(()) };
    if delegate.kind != MessageKind::Delegate {
        return Ok(());
    }

    // Try the active-delegations registry first — this gives us the exact
    // cancellation token and thread ID for scoped interruption.
    if let Some(registry) = active_delegations {
        let active = registry.read().await;
        if let Some(delegation) = active.get(&correlation_id) {
            // Trigger cooperative cancellation — the agent's turn loop checks
            // this token between rounds and tool calls.
            delegation.cancel_token.cancel();

            // Interrupt only the turns in this delegation's thread.
            let turns = storage.list_turns_by_thread(delegation.thread_id).await?;
            for turn in turns {
                if matches!(turn.state, TurnState::Pending | TurnState::Processing) {
                    let _ = storage
                        .update_turn_state(turn.id, TurnState::Interrupted)
                        .await;
                }
            }
            return Ok(());
        }
    }

    // TODO Cancel fallback interrupts all active turns for the target agent
    // priority: high | tags: review:branch-inter-agent-communication
    // When `active_delegations` is None or the registry has no entry for this
    // correlation_id, `find_in_progress_by_agent` returns every in-progress turn
    // for the recipient — not just the one from this delegation. In a scenario
    // where agent-b is running two concurrent delegations from different senders,
    // cancelling one will also interrupt the other. Scope the fallback by storing
    // the thread_id in the `AgentMessage` payload (or the delegate record) so the
    // fallback can filter to the correct thread even without the in-memory registry.
    // ---

    // Fallback: no active delegation entry (execution may have already
    // finished or active_delegations wasn't provided). Use the broad
    // agent-level query.
    let active_turns = storage
        .find_in_progress_by_agent(&delegate.recipient_agent_id)
        .await?;

    for turn in active_turns {
        let _ = storage
            .update_turn_state(turn.id, TurnState::Interrupted)
            .await;
    }

    Ok(())
}

/// Handle a Shutdown message by interrupting all active turns for the target.
///
/// The shutdown message itself is already claimed by the mailbox. This handler
/// interrupts any in-progress turns for the target agent.
pub async fn handle_shutdown(
    storage: &Arc<dyn Storage>,
    shutdown_msg: &AgentMessage,
) -> Result<(), AgentError> {
    debug_assert_eq!(shutdown_msg.kind, MessageKind::Shutdown);

    // TODO Shutdown only marks turn state — live executions keep running
    // priority: high | tags: review:branch-inter-agent-communication
    // `find_in_progress_by_agent` finds turns in storage and marks them
    // `Interrupted`, but agent turn loops do not poll turn state — they stop
    // only when their `CancellationToken` is cancelled. A delegated agent that
    // is mid-execution when shutdown arrives will continue running, execute
    // further tools, and may still emit a `Result` message. To actually
    // interrupt live work, `handle_shutdown` needs access to the
    // `ActiveDelegations` registry (like `handle_cancel`) so it can call
    // `cancel_token.cancel()` on every active delegation for the target agent.
    // ---

    // Find all active turns for the target agent (including AwaitingApproval).
    let active_turns = storage
        .find_in_progress_by_agent(&shutdown_msg.recipient_agent_id)
        .await?;

    for turn in active_turns {
        let _ = storage
            .update_turn_state(turn.id, TurnState::Interrupted)
            .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::{MessageKind, MessageStatus};
    use swage_storage::model::{NewAgentMessage, NewSession, NewThread, NewTurn};
    use swage_storage::store::{AgentMessageStore, ProjectStore, SessionStore, ThreadStore};
    use swage_storage::SqliteStorage;
    use uuid::Uuid;

    async fn setup_with_turn(agent_id: &str) -> (Arc<SqliteStorage>, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let project = ProjectStore::get_or_create_default(storage.as_ref())
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: agent_id.to_string(),
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: agent_id.to_string(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: agent_id.to_string(),
                state: TurnState::Processing,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        (storage, turn.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_interrupts_active_turn() {
        let (storage, turn_id) = setup_with_turn("agent-b").await;

        // Create a delegate message to establish the correlation chain.
        let delegate = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        // Create a cancel correlated to the delegate.
        let cancel = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Cancel,
            correlation_id: Some(delegate.id),
            payload: serde_json::json!({}),
            status: MessageStatus::Delivered,
            failure_reason: None,
            created_at: chrono::Utc::now(),
            delivered_at: Some(chrono::Utc::now()),
        };

        // Store the delegate in the correlation chain.
        storage
            .update_agent_message_status(delegate.id, MessageStatus::Delivered, None)
            .await
            .unwrap();

        // Fallback path — no active delegations provided.
        handle_cancel(&(storage.clone() as Arc<dyn Storage>), &cancel, None).await.unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_with_active_delegations_scopes_to_thread() {
        // Set up TWO agents with the same ID in different sessions/threads
        // to verify that scoped cancel only interrupts the targeted delegation.
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let project = ProjectStore::get_or_create_default(storage.as_ref())
            .await
            .unwrap();

        // First delegation — the one we'll cancel.
        let session_a = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "agent-b".into(),
            })
            .await
            .unwrap();
        let thread_a = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session_a.id,
                agent_id: "agent-b".into(),
            },
        )
        .await
        .unwrap();
        let turn_a = storage
            .create_turn(NewTurn {
                thread_id: thread_a.id,
                agent_id: "agent-b".into(),
                state: TurnState::Processing,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        // Second delegation — should NOT be interrupted.
        let session_b = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "agent-b".into(),
            })
            .await
            .unwrap();
        let thread_b = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session_b.id,
                agent_id: "agent-b".into(),
            },
        )
        .await
        .unwrap();
        let turn_b = storage
            .create_turn(NewTurn {
                thread_id: thread_b.id,
                agent_id: "agent-b".into(),
                state: TurnState::Processing,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        // Create the delegate message for the first delegation.
        let delegate = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        // Register only the first delegation in the active registry.
        let registry = crate::delegation::active_delegations();
        {
            let mut active = registry.write().await;
            active.insert(delegate.id, crate::delegation::ActiveDelegation {
                session_id: session_a.id,
                thread_id: thread_a.id,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            });
        }

        let cancel = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Cancel,
            correlation_id: Some(delegate.id),
            payload: serde_json::json!({}),
            status: MessageStatus::Delivered,
            failure_reason: None,
            created_at: chrono::Utc::now(),
            delivered_at: Some(chrono::Utc::now()),
        };

        handle_cancel(
            &(storage.clone() as Arc<dyn Storage>),
            &cancel,
            Some(&registry),
        )
        .await
        .unwrap();

        // First delegation's turn interrupted.
        let ta = storage.get_turn(turn_a.id).await.unwrap().unwrap();
        assert_eq!(ta.state, TurnState::Interrupted, "targeted turn should be interrupted");

        // Second delegation's turn still processing — not affected.
        let tb = storage.get_turn(turn_b.id).await.unwrap().unwrap();
        assert_eq!(tb.state, TurnState::Processing, "other turn should be unaffected");

        // Cancellation token should have been triggered.
        let active = registry.read().await;
        let delegation = active.get(&delegate.id).unwrap();
        assert!(delegation.cancel_token.is_cancelled(), "cancel token should be triggered");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_interrupts_all_active_turns() {
        let (storage, turn_id) = setup_with_turn("agent-b").await;

        let shutdown = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Shutdown,
            correlation_id: None,
            payload: serde_json::json!({}),
            status: MessageStatus::Delivered,
            failure_reason: None,
            created_at: chrono::Utc::now(),
            delivered_at: Some(chrono::Utc::now()),
        };

        handle_shutdown(&(storage.clone() as Arc<dyn Storage>), &shutdown).await.unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);
    }
}
