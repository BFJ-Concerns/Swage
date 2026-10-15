//! Cancel and Shutdown message handling for inter-agent communication.
//!
//! Cancel interrupts a specific delegation (by correlation_id).
//! Shutdown terminates all active work under a target agent.

use std::collections::HashSet;

use async_trait::async_trait;
use uuid::Uuid;

use swage_core::MessageKind;
use swage_storage::model::AgentMessage;
use swage_storage::store::{AgentMessageStore, ThreadStore, TurnStore};
use swage_storage::{ThreadState, TurnState};

use crate::delegation::ActiveDelegations;
use crate::error::AgentError;

/// Narrow storage surface used by cancel and shutdown handling.
///
/// These paths only need agent-message lookup plus thread and turn state
/// updates. Keeping the bound narrow makes the degraded-state behaviour
/// testable without constructing a full storage implementation.
#[async_trait]
pub trait CancellationStorage: AgentMessageStore + ThreadStore + TurnStore + Send + Sync {}

impl<T> CancellationStorage for T where T: AgentMessageStore + ThreadStore + TurnStore + Send + Sync {}

/// Handle a Cancel message by interrupting the delegated turn.
///
/// When `active_delegations` is provided, the handler uses a three-path
/// fallback to scope the interruption as narrowly as possible:
///
/// 1. **Active delegation entry** — looks up the exact delegation by message
///    ID, triggers its cancellation token (stopping the in-memory execution),
///    and cancels only the turns in that delegation's thread.
/// 2. **Thread-ID annotation** — if no live entry exists (e.g. execution
///    already completed) but the Delegate message payload carries a
///    `_thread_id` annotation (injected by `DelegationExecutor`), cancels
///    only the turns in that specific thread.
/// 3. **Last resort** — falls back to `find_in_progress_by_agent`, which
///    cancels all in-progress turns for the target agent. This is the
///    broadest scope and only fires when neither of the above is available.
///
/// The cancel message itself is already claimed (delivered) by the mailbox.
/// If the target turn is not found or already in a terminal state, the cancel
/// is a no-op — "first terminal state wins" (ADR-0002).
pub async fn handle_cancel(
    storage: &dyn CancellationStorage,
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

    let Some(delegate) = delegate else {
        return Ok(());
    };
    if delegate.kind != MessageKind::Delegate {
        return Ok(());
    }

    // Try the active-delegations registry first — this gives us the exact
    // cancellation token and thread ID for scoped interruption.
    if let Some(registry) = active_delegations
        && let Some((thread_id, cancel_token)) = registry.cancellation_target(correlation_id).await
    {
        // Trigger cooperative cancellation — the agent's turn loop checks
        // this token between rounds and tool calls.
        cancel_token.cancel();

        // Cancel only the turns in this delegation's thread.
        // The active-delegation path uses a narrower set of interruptible
        // states (no AwaitingApproval) since the live cancel token handles
        // in-flight approval waits directly.
        cancel_turns_in_thread(
            storage,
            thread_id,
            &[TurnState::Pending, TurnState::Processing],
        )
        .await?;
        return Ok(());
    }

    // Fallback: no active delegation entry (execution may have already
    // finished or active_delegations wasn't provided). Scope to the specific
    // thread if the delegate payload carries a _thread_id annotation;
    // otherwise fall back to the broad agent-level query.
    //
    // These paths also reset the thread to Idle — no live agent loop exists
    // to do cleanup, so the thread would remain stuck in Processing.
    if let Some(thread_id_str) = delegate.payload.get("_thread_id").and_then(|v| v.as_str())
        && let Ok(thread_id) = thread_id_str.parse::<Uuid>()
    {
        cancel_turns_in_thread(
            storage,
            thread_id,
            &[
                TurnState::Pending,
                TurnState::Processing,
                TurnState::AwaitingApproval,
            ],
        )
        .await?;
        return Ok(());
    }

    // Last resort: no thread annotation — cancel all in-progress turns.
    // This is deliberately broad until delegated-turn cancellation can target
    // the exact recipient thread.
    let active_turns = storage
        .find_in_progress_by_agent(&delegate.recipient_agent_id)
        .await?;

    let thread_ids: HashSet<Uuid> = active_turns.iter().map(|t| t.thread_id).collect();

    for turn in active_turns {
        storage
            .update_turn_state(turn.id, TurnState::Cancelled)
            .await?;
    }

    for thread_id in thread_ids {
        reset_thread_to_idle(storage, thread_id).await?;
    }

    Ok(())
}

/// Cancel turns in a thread that match any of the given states, then reset
/// the thread to Idle.
///
/// Used by the cancel fallback paths where no live cancellation token exists —
/// cancellation is communicated solely via storage writes. Storage failures
/// propagate so incomplete settlement is visible to the caller.
async fn cancel_turns_in_thread(
    storage: &dyn CancellationStorage,
    thread_id: Uuid,
    interruptible_states: &[TurnState],
) -> Result<(), AgentError> {
    let turns = storage.list_turns_by_thread(thread_id).await?;
    for turn in turns {
        if interruptible_states.contains(&turn.state) {
            storage
                .update_turn_state(turn.id, TurnState::Cancelled)
                .await?;
        }
    }
    reset_thread_to_idle(storage, thread_id).await?;
    Ok(())
}

/// Reset a cancelled thread to Idle, accepting cleanup completed by the live
/// agent after this handler signalled its cancellation token.
async fn reset_thread_to_idle(
    storage: &dyn CancellationStorage,
    thread_id: Uuid,
) -> Result<(), AgentError> {
    match storage.update_state(thread_id, ThreadState::Idle).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let already_idle = storage
                .get(thread_id)
                .await?
                .is_some_and(|thread| thread.state == ThreadState::Idle);
            if already_idle {
                Ok(())
            } else {
                Err(error.into())
            }
        }
    }
}

/// Handle a Shutdown message by interrupting all active turns for the target.
///
/// The shutdown message itself is already claimed by the mailbox. This handler
/// interrupts any in-progress turns for the target agent, then resets their
/// threads to Idle so new turns can be created.
pub async fn handle_shutdown(
    storage: &dyn CancellationStorage,
    shutdown_msg: &AgentMessage,
    active_delegations: Option<&ActiveDelegations>,
) -> Result<(), AgentError> {
    debug_assert_eq!(shutdown_msg.kind, MessageKind::Shutdown);

    // Mark any in-progress turns as Interrupted in storage — covers turns
    // whose tokens aren't in the registry (already finished, or registry
    // wasn't provided).
    let active_turns = storage
        .find_in_progress_by_agent(&shutdown_msg.recipient_agent_id)
        .await?;

    // Collect thread IDs before iterating so we can reset them to Idle.
    let thread_ids: HashSet<Uuid> = active_turns.iter().map(|t| t.thread_id).collect();

    for turn in active_turns {
        storage
            .update_turn_state(turn.id, TurnState::Interrupted)
            .await?;
    }

    // Reset threads to Idle — mirrors handle_cancel's fallback path. Without
    // this, threads stay in Processing and reject subsequent turn creation.
    for thread_id in thread_ids {
        reset_thread_to_idle(storage, thread_id).await?;
    }

    // The interruption is durable before a live loop observes cancellation.
    // Cancel live execution tokens for all delegations targeting this agent.
    // Without this, agent turn loops continue running — they stop on
    // CancellationToken, not turn state changes in storage.
    if let Some(registry) = active_delegations {
        registry
            .cancel_for_agent(&shutdown_msg.recipient_agent_id)
            .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use async_trait::async_trait;
    use swage_core::StorageError;
    use swage_core::{MessageKind, MessageStatus};
    use swage_storage::SqliteStorage;
    use swage_storage::model::{NewAgentMessage, NewSession, NewThread, NewTurn};
    use swage_storage::store::{
        AgentMessageStore, SessionStore, ThreadStore, TopicStore, TurnStore,
    };
    use uuid::Uuid;

    #[derive(Clone, Copy)]
    enum InjectedFailure {
        UpdateTurnState,
        UpdateThreadState,
        ThreadIdleWonRace,
    }

    struct FailingCancellationStorage {
        inner: Arc<SqliteStorage>,
        injected_failure: InjectedFailure,
    }

    impl FailingCancellationStorage {
        fn injected_error(&self) -> StorageError {
            StorageError::QueryFailed("injected cancellation storage failure".into())
        }
    }

    #[async_trait]
    impl AgentMessageStore for FailingCancellationStorage {
        async fn create_agent_message(
            &self,
            _msg: NewAgentMessage,
        ) -> Result<AgentMessage, StorageError> {
            unimplemented!()
        }

        async fn get_agent_message(&self, id: Uuid) -> Result<Option<AgentMessage>, StorageError> {
            self.inner.get_agent_message(id).await
        }

        async fn list_pending_for_agent(
            &self,
            _agent_id: &str,
        ) -> Result<Vec<AgentMessage>, StorageError> {
            unimplemented!()
        }

        async fn update_agent_message_status(
            &self,
            _id: Uuid,
            _status: MessageStatus,
            _failure_reason: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }

        async fn update_agent_message_payload(
            &self,
            _id: Uuid,
            _payload: serde_json::Value,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn list_by_correlation(
            &self,
            _correlation_id: Uuid,
        ) -> Result<Vec<AgentMessage>, StorageError> {
            unimplemented!()
        }

        async fn expire_stale_messages(
            &self,
            _max_age: std::time::Duration,
        ) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TurnStore for FailingCancellationStorage {
        async fn create_turn(&self, _turn: NewTurn) -> Result<swage_storage::Turn, StorageError> {
            unimplemented!()
        }

        async fn get_turn(&self, id: Uuid) -> Result<Option<swage_storage::Turn>, StorageError> {
            self.inner.get_turn(id).await
        }

        async fn list_turns_by_thread(
            &self,
            thread_id: Uuid,
        ) -> Result<Vec<swage_storage::Turn>, StorageError> {
            self.inner.list_turns_by_thread(thread_id).await
        }

        async fn force_turn_failure(&self, id: Uuid) -> Result<(), StorageError> {
            self.inner.force_turn_failure(id).await
        }

        async fn update_turn_state(&self, id: Uuid, state: TurnState) -> Result<(), StorageError> {
            match self.injected_failure {
                InjectedFailure::UpdateTurnState => Err(self.injected_error()),
                InjectedFailure::UpdateThreadState | InjectedFailure::ThreadIdleWonRace => {
                    self.inner.update_turn_state(id, state).await
                }
            }
        }

        async fn find_in_progress_by_agent(
            &self,
            agent_id: &str,
        ) -> Result<Vec<swage_storage::Turn>, StorageError> {
            self.inner.find_in_progress_by_agent(agent_id).await
        }

        async fn update_response(&self, _id: Uuid, _response: &str) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn update_inbound_audit(
            &self,
            _id: Uuid,
            _inbound_audit: &swage_storage::InboundAudit,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn update_tokens(
            &self,
            _id: Uuid,
            _input: i64,
            _output: i64,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn add_delegated_tokens(
            &self,
            _id: Uuid,
            _input: i64,
            _output: i64,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn update_cost(&self, _id: Uuid, _cost: i64) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn record_turn_failure_message(
            &self,
            _id: Uuid,
            _message: &str,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn record_turn_isolation(
            &self,
            _id: Uuid,
            _isolation: &swage_storage::TurnIsolation,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn update_structured_output(
            &self,
            _id: Uuid,
            _output: &serde_json::Value,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ThreadStore for FailingCancellationStorage {
        async fn create(&self, _thread: NewThread) -> Result<swage_storage::Thread, StorageError> {
            unimplemented!()
        }

        async fn get(&self, id: Uuid) -> Result<Option<swage_storage::Thread>, StorageError> {
            ThreadStore::get(self.inner.as_ref(), id).await
        }

        async fn list_by_session(
            &self,
            _session_id: Uuid,
        ) -> Result<Vec<swage_storage::Thread>, StorageError> {
            unimplemented!()
        }

        async fn list_threads_by_agent_id(
            &self,
            _agent_id: &str,
        ) -> Result<Vec<swage_storage::Thread>, StorageError> {
            unimplemented!()
        }

        async fn update_state(&self, id: Uuid, state: ThreadState) -> Result<(), StorageError> {
            match self.injected_failure {
                InjectedFailure::UpdateThreadState => Err(self.injected_error()),
                InjectedFailure::UpdateTurnState => self.inner.update_state(id, state).await,
                InjectedFailure::ThreadIdleWonRace => {
                    self.inner.update_state(id, state.clone()).await?;
                    self.inner.update_state(id, state).await
                }
            }
        }

        async fn delete(&self, _id: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }

        async fn create_branch(
            &self,
            _parent_thread_id: Uuid,
            _branched_from_message_id: Uuid,
        ) -> Result<swage_storage::ThreadBranch, StorageError> {
            unimplemented!()
        }

        async fn list_branches_by_session(
            &self,
            _session_id: Uuid,
        ) -> Result<Vec<swage_storage::ThreadBranch>, StorageError> {
            unimplemented!()
        }

        async fn get_branch(
            &self,
            _child_thread_id: Uuid,
        ) -> Result<Option<swage_storage::ThreadBranch>, StorageError> {
            unimplemented!()
        }
    }

    async fn create_processing_turn(
        storage: &SqliteStorage,
        session_id: Uuid,
        agent_id: &str,
    ) -> (swage_storage::Thread, swage_storage::Turn) {
        let thread = ThreadStore::create(
            storage,
            NewThread {
                session_id,
                agent_id: agent_id.to_string(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: agent_id.to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        storage
            .update_turn_state(turn.id, TurnState::Processing)
            .await
            .unwrap();
        storage
            .update_state(thread.id, ThreadState::Processing)
            .await
            .unwrap();
        let turn = storage.get_turn(turn.id).await.unwrap().unwrap();
        (thread, turn)
    }

    async fn setup_with_turn(agent_id: &str) -> (Arc<SqliteStorage>, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::general_scope(storage.as_ref()).await.unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: agent_id.to_string(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let (_thread, turn) = create_processing_turn(storage.as_ref(), session.id, agent_id).await;
        (storage, turn.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_marks_active_turn_cancelled() {
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
        handle_cancel(storage.as_ref(), &cancel, None)
            .await
            .unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Cancelled);

        // Verify the thread was also reset to Idle — without this, the thread
        // remains stuck in Processing and future turns fail with TurnInProgress.
        let thread = ThreadStore::get(storage.as_ref(), turn.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(thread.state, ThreadState::Idle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_with_no_correlation_id_returns_error() {
        let (storage, _turn_id) = setup_with_turn("agent-b").await;

        let cancel = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Cancel,
            correlation_id: None, // Missing — should fail.
            payload: serde_json::json!({}),
            status: MessageStatus::Delivered,
            failure_reason: None,
            created_at: chrono::Utc::now(),
            delivered_at: Some(chrono::Utc::now()),
        };

        let result = handle_cancel(storage.as_ref(), &cancel, None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AgentError::InvalidConfig(msg) => {
                assert!(
                    msg.contains("correlation_id"),
                    "error should mention correlation_id: {msg}"
                );
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_thread_id_annotation_scopes_to_annotated_thread() {
        // Set up two threads for agent-b — the annotation should scope
        // interruption to only the annotated thread.
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::general_scope(storage.as_ref()).await.unwrap();

        let session = storage
            .create_session(NewSession {
                agent_id: "agent-b".into(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();

        // Thread A — the annotated target.
        let (thread_a, turn_a) =
            create_processing_turn(storage.as_ref(), session.id, "agent-b").await;

        // Thread B — should NOT be interrupted.
        let (_thread_b, turn_b) =
            create_processing_turn(storage.as_ref(), session.id, "agent-b").await;

        // Create a delegate with _thread_id annotation pointing to thread_a.
        let delegate = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({
                    "_thread_id": thread_a.id.to_string(),
                    "task": "something"
                }),
            })
            .await
            .unwrap();
        storage
            .update_agent_message_status(delegate.id, MessageStatus::Delivered, None)
            .await
            .unwrap();

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

        // No active delegations — forces the _thread_id annotation fallback.
        handle_cancel(storage.as_ref(), &cancel, None)
            .await
            .unwrap();

        // Thread A's turn should be interrupted.
        let ta = storage.get_turn(turn_a.id).await.unwrap().unwrap();
        assert_eq!(
            ta.state,
            TurnState::Cancelled,
            "annotated thread's turn should be cancelled"
        );

        // Thread A should be reset to Idle.
        let thread_a_state = ThreadStore::get(storage.as_ref(), thread_a.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            thread_a_state.state,
            ThreadState::Idle,
            "annotated thread should be reset to Idle"
        );

        // Thread B's turn should be unaffected.
        let tb = storage.get_turn(turn_b.id).await.unwrap().unwrap();
        assert_eq!(
            tb.state,
            TurnState::Processing,
            "other thread's turn should be unaffected"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_malformed_thread_id_annotation_falls_through_to_broad_cancel() {
        let (storage, turn_id) = setup_with_turn("agent-b").await;

        // Create a delegate with a malformed _thread_id — not a valid UUID.
        let delegate = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({
                    "_thread_id": "not-a-valid-uuid",
                }),
            })
            .await
            .unwrap();
        storage
            .update_agent_message_status(delegate.id, MessageStatus::Delivered, None)
            .await
            .unwrap();

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

        // Should succeed — malformed UUID is ignored, falls through to
        // the broad agent-level cancel path.
        handle_cancel(storage.as_ref(), &cancel, None)
            .await
            .unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(
            turn.state,
            TurnState::Cancelled,
            "should still cancel via broad fallback"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_with_active_delegations_scopes_to_thread() {
        // Set up TWO agents with the same ID in different sessions/threads
        // to verify that scoped cancel only interrupts the targeted delegation.
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::general_scope(storage.as_ref()).await.unwrap();

        // First delegation — the one we'll cancel.
        let session_a = storage
            .create_session(NewSession {
                agent_id: "agent-b".into(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let (thread_a, turn_a) =
            create_processing_turn(storage.as_ref(), session_a.id, "agent-b").await;

        // Second delegation — should NOT be interrupted.
        let session_b = storage
            .create_session(NewSession {
                agent_id: "agent-b".into(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let (_thread_b, turn_b) =
            create_processing_turn(storage.as_ref(), session_b.id, "agent-b").await;

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
        let cancel_token = tokio_util::sync::CancellationToken::new();
        registry
            .insert(
                delegate.id,
                crate::delegation::ActiveDelegation {
                    session_id: session_a.id,
                    thread_id: thread_a.id,
                    recipient_agent_id: "agent-b".into(),
                    cancel_token: cancel_token.clone(),
                },
            )
            .await;

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

        handle_cancel(storage.as_ref(), &cancel, Some(&registry))
            .await
            .unwrap();

        // First delegation's turn interrupted.
        let ta = storage.get_turn(turn_a.id).await.unwrap().unwrap();
        assert_eq!(
            ta.state,
            TurnState::Cancelled,
            "targeted turn should be cancelled"
        );

        // Second delegation's turn still processing — not affected.
        let tb = storage.get_turn(turn_b.id).await.unwrap().unwrap();
        assert_eq!(
            tb.state,
            TurnState::Processing,
            "other turn should be unaffected"
        );

        // Cancellation token should have been triggered.
        assert!(
            cancel_token.is_cancelled(),
            "cancel token should be triggered"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_succeeds_when_live_agent_wins_thread_idle_race() {
        let (inner, turn_id) = setup_with_turn("agent-b").await;
        let turn = inner.get_turn(turn_id).await.unwrap().unwrap();
        let thread = ThreadStore::get(inner.as_ref(), turn.thread_id)
            .await
            .unwrap()
            .unwrap();
        let delegate = inner
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        let registry = crate::delegation::active_delegations();
        let cancel_token = tokio_util::sync::CancellationToken::new();
        registry
            .insert(
                delegate.id,
                crate::delegation::ActiveDelegation {
                    session_id: thread.session_id,
                    thread_id: thread.id,
                    recipient_agent_id: "agent-b".into(),
                    cancel_token: cancel_token.clone(),
                },
            )
            .await;
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
        let racing_storage = FailingCancellationStorage {
            inner: inner.clone(),
            injected_failure: InjectedFailure::ThreadIdleWonRace,
        };

        handle_cancel(&racing_storage, &cancel, Some(&registry))
            .await
            .expect("already-completed live cleanup should satisfy cancellation");

        assert!(cancel_token.is_cancelled());
        assert_eq!(
            inner.get_turn(turn_id).await.unwrap().unwrap().state,
            TurnState::Cancelled
        );
        assert_eq!(
            ThreadStore::get(inner.as_ref(), thread.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ThreadState::Idle
        );
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

        handle_shutdown(storage.as_ref(), &shutdown, None)
            .await
            .unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_cancels_active_delegation_tokens() {
        let (storage, _turn_id) = setup_with_turn("agent-b").await;

        // Register a delegation targeting agent-b.
        let registry = crate::delegation::active_delegations();
        let cancel_token = tokio_util::sync::CancellationToken::new();
        registry
            .insert(
                Uuid::new_v4(),
                crate::delegation::ActiveDelegation {
                    session_id: Uuid::new_v4(),
                    thread_id: Uuid::new_v4(),
                    recipient_agent_id: "agent-b".into(),
                    cancel_token: cancel_token.clone(),
                },
            )
            .await;

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

        handle_shutdown(storage.as_ref(), &shutdown, Some(&registry))
            .await
            .unwrap();

        // Token should have been cancelled — live execution would stop.
        assert!(
            cancel_token.is_cancelled(),
            "shutdown must cancel live execution tokens"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_fallback_returns_error_when_turn_interrupt_write_fails() {
        let (inner, _turn_id) = setup_with_turn("agent-b").await;

        let delegate = inner
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        inner
            .update_agent_message_status(delegate.id, MessageStatus::Delivered, None)
            .await
            .unwrap();

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

        let failing_storage = Arc::new(FailingCancellationStorage {
            inner,
            injected_failure: InjectedFailure::UpdateTurnState,
        });

        let error = handle_cancel(failing_storage.as_ref(), &cancel, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                AgentError::StorageFailed(StorageError::QueryFailed(ref message))
                if message.contains("injected cancellation storage failure")
            ),
            "expected explicit storage failure, got {error:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_returns_error_when_thread_reset_write_fails() {
        let (inner, _turn_id) = setup_with_turn("agent-b").await;

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

        let failing_storage = Arc::new(FailingCancellationStorage {
            inner,
            injected_failure: InjectedFailure::UpdateThreadState,
        });

        let error = handle_shutdown(failing_storage.as_ref(), &shutdown, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                AgentError::StorageFailed(StorageError::QueryFailed(ref message))
                if message.contains("injected cancellation storage failure")
            ),
            "expected explicit storage failure, got {error:?}"
        );
    }
}
