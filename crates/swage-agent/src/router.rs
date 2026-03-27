//! Message routing service for inter-agent communication.
//!
//! Central service that validates, routes, persists, and delivers messages
//! between agents, with hook dispatch at each lifecycle stage.

use std::sync::Arc;

use uuid::Uuid;

use swage_core::{MessageKind, MessageStatus};
use swage_hook::{HookEvent, HookOutcome, HookRegistry};
use swage_storage::model::{AgentMessage, NewAgentMessage};
use swage_storage::store::AgentMessageStore;
use swage_storage::Storage;

use crate::error::AgentError;

/// Routes inter-agent messages through validation, hooks, and persistence.
///
/// The router does NOT instantiate agents — that's the DelegationExecutor's job.
/// It handles message lifecycle: create, persist, deliver, fail.
pub struct MessageRouter {
    storage: Arc<dyn Storage>,
    hook_registry: Option<Arc<HookRegistry>>,
}

impl MessageRouter {
    pub fn new(storage: Arc<dyn Storage>, hook_registry: Option<Arc<HookRegistry>>) -> Self {
        Self {
            storage,
            hook_registry,
        }
    }

    /// Send a new inter-agent message.
    ///
    /// 1. Validates correlation_id requirements (required on Result/Cancel).
    /// 2. Dispatches MessageSend hook — if blocked, returns error.
    /// 3. For Notify: not persisted (at-most-once semantics).
    /// 4. For durable kinds: persists via AgentMessageStore.
    pub async fn send_message(
        &self,
        msg: NewAgentMessage,
    ) -> Result<AgentMessage, AgentError> {
        // Validate: correlation_id is required on Result and Cancel messages.
        match msg.kind {
            MessageKind::Result | MessageKind::Cancel => {
                if msg.correlation_id.is_none() {
                    return Err(AgentError::InvalidConfig(format!(
                        "{} message requires a correlation_id",
                        msg.kind,
                    )));
                }
            }
            _ => {}
        }

        // Dispatch MessageSend hook — may block message creation.
        if let Some(ref registry) = self.hook_registry {
            let event = HookEvent::MessageSend {
                sender_agent_id: msg.sender_agent_id.clone(),
                recipient_agent_id: msg.recipient_agent_id.clone(),
                kind: msg.kind.to_string(),
                payload: msg.payload.clone(),
            };
            match registry.dispatch(event).await {
                HookOutcome::Block(reason) => {
                    return Err(AgentError::HookBlocked {
                        hook: "MessageSend".into(),
                        reason,
                    });
                }
                // Fail-open: hook errors don't prevent message creation.
                _ => {}
            }
        }

        // Notify is ephemeral — not persisted. Return a synthetic message.
        if msg.kind == MessageKind::Notify {
            let now = chrono::Utc::now();
            return Ok(AgentMessage {
                id: Uuid::new_v4(),
                sender_agent_id: msg.sender_agent_id,
                recipient_agent_id: msg.recipient_agent_id,
                kind: msg.kind,
                correlation_id: msg.correlation_id,
                payload: msg.payload,
                status: MessageStatus::Delivered,
                failure_reason: None,
                created_at: now,
                delivered_at: Some(now),
            });
        }

        // Durable kinds: persist with pending status.
        let stored = self.storage.create_agent_message(msg).await?;
        Ok(stored)
    }

    /// Deliver a message to its target agent.
    ///
    /// Dispatches MessageDeliver and MessageReceive hooks. If either blocks,
    /// the message transitions to Failed. Otherwise, transitions to Delivered.
    pub async fn deliver_message(&self, msg: &AgentMessage) -> Result<(), AgentError> {
        if let Some(ref registry) = self.hook_registry {
            let event = HookEvent::MessageDeliver {
                message_id: msg.id.to_string(),
                sender_agent_id: msg.sender_agent_id.clone(),
                recipient_agent_id: msg.recipient_agent_id.clone(),
                kind: msg.kind.to_string(),
                payload: msg.payload.clone(),
            };
            if let HookOutcome::Block(reason) = registry.dispatch(event).await {
                self.fail_message(msg.id, &reason).await?;
                return Err(AgentError::HookBlocked {
                    hook: "MessageDeliver".into(),
                    reason,
                });
            }
        }

        if let Some(ref registry) = self.hook_registry {
            let event = HookEvent::MessageReceive {
                message_id: msg.id.to_string(),
                sender_agent_id: msg.sender_agent_id.clone(),
                recipient_agent_id: msg.recipient_agent_id.clone(),
                kind: msg.kind.to_string(),
                payload: msg.payload.clone(),
            };
            if let HookOutcome::Block(reason) = registry.dispatch(event).await {
                self.fail_message(msg.id, &reason).await?;
                return Err(AgentError::HookBlocked {
                    hook: "MessageReceive".into(),
                    reason,
                });
            }
        }

        // Transition to Delivered.
        self.storage
            .update_agent_message_status(msg.id, MessageStatus::Delivered, None)
            .await?;

        Ok(())
    }

    /// Transition a message to Failed with the given reason.
    pub async fn fail_message(&self, id: Uuid, reason: &str) -> Result<(), AgentError> {
        self.storage
            .update_agent_message_status(id, MessageStatus::Failed, Some(reason))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_storage::SqliteStorage;

    async fn make_router() -> MessageRouter {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        MessageRouter::new(storage, None)
    }

    fn delegate_msg() -> NewAgentMessage {
        NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "summarise"}),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_delegate_persists() {
        let router = make_router().await;
        let msg = router.send_message(delegate_msg()).await.unwrap();
        assert_eq!(msg.kind, MessageKind::Delegate);
        assert_eq!(msg.status, MessageStatus::Pending);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_notify_not_persisted() {
        let router = make_router().await;
        let msg = router
            .send_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Notify,
                correlation_id: None,
                payload: serde_json::json!({"progress": 50}),
            })
            .await
            .unwrap();

        assert_eq!(msg.status, MessageStatus::Delivered);
        let fetched = router.storage.get_agent_message(msg.id).await.unwrap();
        assert!(fetched.is_none(), "Notify should not be persisted");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn result_without_correlation_fails() {
        let router = make_router().await;
        let result = router
            .send_message(NewAgentMessage {
                sender_agent_id: "agent-b".into(),
                recipient_agent_id: "agent-a".into(),
                kind: MessageKind::Result,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deliver_transitions_to_delivered() {
        let router = make_router().await;
        let msg = router.send_message(delegate_msg()).await.unwrap();
        router.deliver_message(&msg).await.unwrap();

        let updated = router.storage.get_agent_message(msg.id).await.unwrap().unwrap();
        assert_eq!(updated.status, MessageStatus::Delivered);
        assert!(updated.delivered_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fail_message_transitions_to_failed() {
        let router = make_router().await;
        let msg = router.send_message(delegate_msg()).await.unwrap();
        router.fail_message(msg.id, "agent not found").await.unwrap();

        let updated = router.storage.get_agent_message(msg.id).await.unwrap().unwrap();
        assert_eq!(updated.status, MessageStatus::Failed);
        assert_eq!(updated.failure_reason.as_deref(), Some("agent not found"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hook_blocks_message_send() {
        use swage_hook::Hook;

        struct BlockHook;

        #[async_trait::async_trait]
        impl Hook for BlockHook {
            fn event_filter(&self) -> Option<&str> {
                Some("MessageSend")
            }

            async fn execute(&self, _event: &HookEvent) -> HookOutcome {
                HookOutcome::Block("policy: no delegation".into())
            }
        }

        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let registry = Arc::new(HookRegistry::new());
        registry.register(Arc::new(BlockHook));
        let router = MessageRouter::new(storage.clone(), Some(registry));

        let result = router.send_message(delegate_msg()).await;
        assert!(result.is_err());

        // No message should have been persisted.
        let pending = storage.list_pending_for_agent("agent-b").await.unwrap();
        assert!(pending.is_empty());
    }
}
