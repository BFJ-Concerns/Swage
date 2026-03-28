//! Mailbox delivery — pending message retrieval on agent instantiation.
//!
//! When an agent is instantiated, it checks for pending messages in storage.
//! Cancel and Shutdown messages are processed before starting work; pending
//! Delegate messages are queued for processing.

use std::sync::Arc;

use uuid::Uuid;

use swage_core::{MessageKind, MessageStatus};
use swage_storage::model::AgentMessage;
use swage_storage::store::AgentMessageStore;
use swage_storage::Storage;

use crate::error::AgentError;

/// Categorised pending messages retrieved from the mailbox on instantiation.
#[derive(Debug, Default)]
pub struct MailboxContents {
    /// If true, a Shutdown message was found — the agent should terminate
    /// immediately without processing any other work.
    pub should_shutdown: bool,
    /// Correlation IDs of delegations that were cancelled before the agent
    /// started. The agent should not begin work for these delegations.
    pub cancelled_delegations: Vec<Uuid>,
    /// Pending Delegate messages queued for processing.
    pub pending_delegates: Vec<AgentMessage>,
    /// Pending Result messages from completed sub-delegations.
    pub pending_results: Vec<AgentMessage>,
}

/// Check the mailbox for an agent, claiming all pending messages atomically.
///
/// Messages are transitioned to Delivered as they're claimed. This prevents
/// concurrent instantiations from delivering the same message twice — the
/// `update_agent_message_status` call only succeeds for the first claimer.
pub async fn check_mailbox(
    storage: &Arc<dyn Storage>,
    agent_id: &str,
) -> Result<MailboxContents, AgentError> {
    let pending = storage.list_pending_for_agent(agent_id).await?;

    let mut contents = MailboxContents::default();

    for msg in pending {
        // Claim the message atomically — if this returns false, another
        // instantiation already claimed it (race condition handled).
        let claimed = storage
            .update_agent_message_status(msg.id, MessageStatus::Delivered, None)
            .await?;
        if !claimed {
            continue;
        }

        match msg.kind {
            MessageKind::Shutdown => {
                // TODO Shutdown claiming consumes sibling messages that will never be processed
                // priority: high | tags: review:branch-inter-agent-communication
                // All pending messages are claimed as Delivered before the caller inspects
                // `should_shutdown`. When the caller sees `true` and terminates, any
                // Delegate/Result messages already claimed here are permanently lost —
                // future instantiations cannot reclaim them. Either stop the loop and mark
                // remaining unclaimed messages as Failed("shutdown received"), or skip
                // claiming non-Shutdown messages once Shutdown is seen.
                // ---
                contents.should_shutdown = true;
            }
            MessageKind::Cancel => {
                if let Some(correlation_id) = msg.correlation_id {
                    contents.cancelled_delegations.push(correlation_id);
                }
            }
            MessageKind::Delegate => {
                contents.pending_delegates.push(msg);
            }
            MessageKind::Result => {
                contents.pending_results.push(msg);
            }
            MessageKind::Notify => {
                // Notify messages shouldn't be in storage (they're ephemeral),
                // but handle gracefully if they appear.
            }
        }
    }

    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_storage::model::NewAgentMessage;
    use swage_storage::SqliteStorage;

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_mailbox() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let contents = check_mailbox(&storage, "agent-a").await.unwrap();
        assert!(!contents.should_shutdown);
        assert!(contents.cancelled_delegations.is_empty());
        assert!(contents.pending_delegates.is_empty());
        assert!(contents.pending_results.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mailbox_categorises_messages() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());

        // Queue a Delegate.
        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "sender".into(),
                recipient_agent_id: "agent-a".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({"task": "work"}),
            })
            .await
            .unwrap();

        // Queue a Shutdown.
        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "sender".into(),
                recipient_agent_id: "agent-a".into(),
                kind: MessageKind::Shutdown,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        let contents = check_mailbox(&storage, "agent-a").await.unwrap();
        assert!(contents.should_shutdown);
        assert_eq!(contents.pending_delegates.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn claimed_messages_not_pending() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());

        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "sender".into(),
                recipient_agent_id: "agent-a".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        // First check claims the message.
        let first = check_mailbox(&storage, "agent-a").await.unwrap();
        assert_eq!(first.pending_delegates.len(), 1);

        // Second check finds nothing — message was already delivered.
        let second = check_mailbox(&storage, "agent-a").await.unwrap();
        assert!(second.pending_delegates.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_collects_correlation_ids() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let correlation = Uuid::new_v4();

        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "sender".into(),
                recipient_agent_id: "agent-a".into(),
                kind: MessageKind::Cancel,
                correlation_id: Some(correlation),
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        let contents = check_mailbox(&storage, "agent-a").await.unwrap();
        assert_eq!(contents.cancelled_delegations, vec![correlation]);
    }
}
