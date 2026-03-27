//! Cancel and Shutdown message handling for inter-agent communication.
//!
//! Cancel interrupts a specific delegation (by correlation_id).
//! Shutdown terminates all active work under a target agent.

use std::sync::Arc;

use swage_core::MessageKind;
use swage_storage::model::AgentMessage;
use swage_storage::store::TurnStore;
use swage_storage::{Storage, TurnState};

use crate::error::AgentError;

/// Handle a Cancel message by interrupting the delegated turn.
///
/// Looks up the delegation chain via correlation_id, finds the target turn,
/// and transitions it to Interrupted. The cancel message itself is already
/// claimed (delivered) by the mailbox.
///
/// If the target turn is not found or already in a terminal state, the cancel
/// is a no-op — "first terminal state wins" (ADR-0002).
pub async fn handle_cancel(
    storage: &Arc<dyn Storage>,
    cancel_msg: &AgentMessage,
) -> Result<(), AgentError> {
    debug_assert_eq!(cancel_msg.kind, MessageKind::Cancel);

    let Some(correlation_id) = cancel_msg.correlation_id else {
        return Err(AgentError::InvalidConfig(
            "Cancel message missing correlation_id".into(),
        ));
    };

    // Find the original Delegate in the correlation chain, then locate the
    // turn created for that delegation. Since we don't have a direct
    // message→turn link, we look for in-progress turns for the target agent.
    let correlated = storage.list_by_correlation(correlation_id).await?;
    let delegate_msg = correlated.iter().find(|m| m.kind == MessageKind::Delegate);

    if let Some(delegate) = delegate_msg {
        // Find active turns for the recipient of the original delegation.
        let active_turns = storage
            .find_in_progress_by_agent(&delegate.recipient_agent_id)
            .await?;

        // Interrupt the first active turn — in a per-task lifecycle, there's
        // at most one active turn per agent instantiation.
        for turn in active_turns {
            let _ = storage
                .update_turn_state(turn.id, TurnState::Interrupted)
                .await;
        }
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

    // Find all active turns for the target agent.
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
    use swage_storage::store::{ProjectStore, SessionStore, ThreadStore};
    use swage_storage::SqliteStorage;
    use uuid::Uuid;

    async fn setup_with_turn(agent_id: &str) -> (Arc<dyn Storage>, Uuid) {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
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

        handle_cancel(&storage, &cancel).await.unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);
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

        handle_shutdown(&storage, &shutdown).await.unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);
    }
}
