//! Mailbox delivery — pending message retrieval on agent instantiation.
//!
//! When an agent is instantiated, it checks for pending messages in storage.
//! Cancel and Shutdown messages are processed before starting work; pending
//! Delegate messages are queued for processing.

use std::sync::Arc;

use uuid::Uuid;

use swage_core::{MessageKind, MessageStatus};
use swage_storage::Storage;
use swage_storage::model::AgentMessage;

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

    // Pre-scan for Shutdown. When present, non-Shutdown messages must not be
    // claimed as Delivered — the caller will terminate immediately and those
    // messages would be permanently lost. Mark them Failed instead.
    let has_shutdown = pending.iter().any(|m| m.kind == MessageKind::Shutdown);

    for msg in pending {
        if has_shutdown && msg.kind != MessageKind::Shutdown {
            // Fail non-Shutdown messages so they aren't silently abandoned.
            // If this write fails the message stays Pending and may be
            // re-delivered on the next instantiation — log so it's diagnosable.
            if let Err(e) = storage
                .update_agent_message_status(
                    msg.id,
                    MessageStatus::Failed,
                    Some("shutdown received"),
                )
                .await
            {
                tracing::warn!(
                    message_id = %msg.id,
                    error = %e,
                    "failed to mark pre-shutdown message as Failed — may be re-delivered"
                );
            }
            // Still note Cancel correlation IDs — useful for cleanup metadata
            // even during shutdown.
            if msg.kind == MessageKind::Cancel {
                if let Some(correlation_id) = msg.correlation_id {
                    contents.cancelled_delegations.push(correlation_id);
                }
            }
            continue;
        }

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
    use swage_storage::SqliteStorage;
    use swage_storage::model::NewAgentMessage;

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
    async fn shutdown_fails_sibling_messages() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());

        // Queue a Delegate — this should be Failed, not claimed as Delivered.
        let delegate = storage
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
        // Delegate is NOT collected — it was Failed to prevent silent loss.
        assert!(contents.pending_delegates.is_empty());

        // Verify the delegate was marked Failed with the shutdown reason.
        let msg = storage
            .get_agent_message(delegate.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.status, MessageStatus::Failed);
        assert_eq!(msg.failure_reason.as_deref(), Some("shutdown received"));
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
    async fn cancel_during_shutdown_populates_cancelled_delegations() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let correlation = Uuid::new_v4();

        // Queue a Cancel with a correlation ID.
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

        // Queue a Shutdown — causes the Cancel to be Failed rather than
        // Delivered, but its correlation ID should still be captured.
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
        // The Cancel was Failed (not Delivered), but its correlation ID
        // must still appear so a restarted agent doesn't re-execute the
        // cancelled delegation.
        assert_eq!(contents.cancelled_delegations, vec![correlation]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn result_messages_collected_in_pending_results() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let correlation = Uuid::new_v4();

        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "sub-agent".into(),
                recipient_agent_id: "agent-a".into(),
                kind: MessageKind::Result,
                correlation_id: Some(correlation),
                payload: serde_json::json!({"response": "done"}),
            })
            .await
            .unwrap();

        let contents = check_mailbox(&storage, "agent-a").await.unwrap();
        assert_eq!(contents.pending_results.len(), 1);
        assert_eq!(
            contents.pending_results[0].correlation_id,
            Some(correlation)
        );
        assert_eq!(contents.pending_results[0].payload["response"], "done");
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
