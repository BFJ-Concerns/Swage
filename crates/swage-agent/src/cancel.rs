//! Cancel and Shutdown message handling for inter-agent communication.
//!
//! Cancel interrupts a specific delegation (by correlation_id).
//! Shutdown terminates all active work under a target agent.

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use swage_core::MessageKind;
use swage_storage::model::AgentMessage;
use swage_storage::store::{ThreadStore, TurnStore};
use swage_storage::{Storage, ThreadState, TurnState};

use crate::delegation::ActiveDelegations;
use crate::error::AgentError;

/// Handle a Cancel message by interrupting the delegated turn.
///
/// When `active_delegations` is provided, the handler uses a three-path
/// fallback to scope the interruption as narrowly as possible:
///
/// 1. **Active delegation entry** — looks up the exact delegation by message
///    ID, triggers its cancellation token (stopping the in-memory execution),
///    and interrupts only the turns in that delegation's thread.
/// 2. **Thread-ID annotation** — if no live entry exists (e.g. execution
///    already completed) but the Delegate message payload carries a
///    `_thread_id` annotation (injected by `DelegationExecutor`), interrupts
///    only the turns in that specific thread.
/// 3. **Last resort** — falls back to `find_in_progress_by_agent`, which
///    interrupts all in-progress turns for the target agent. This is the
///    broadest scope and only fires when neither of the above is available.
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
        // Extract the delegation data we need, then release the lock before
        // any async I/O — holding a tokio RwLock across .await can starve
        // pending writers.
        let delegation_data = {
            let active = registry.read().await;
            active.get(&correlation_id).map(|d| {
                (d.thread_id, d.cancel_token.clone())
            })
        };
        // Guard dropped here — safe to await storage calls.

        if let Some((thread_id, cancel_token)) = delegation_data {
            // Trigger cooperative cancellation — the agent's turn loop checks
            // this token between rounds and tool calls.
            cancel_token.cancel();

            // Interrupt only the turns in this delegation's thread.
            // The active-delegation path uses a narrower set of interruptible
            // states (no AwaitingApproval) since the live cancel token handles
            // in-flight approval waits directly.
            interrupt_turns_in_thread(
                storage,
                thread_id,
                &[TurnState::Pending, TurnState::Processing],
            )
            .await?;
            return Ok(());
        }
    }

    // Fallback: no active delegation entry (execution may have already
    // finished or active_delegations wasn't provided). Scope to the specific
    // thread if the delegate payload carries a _thread_id annotation;
    // otherwise fall back to the broad agent-level query.
    //
    // These paths also reset the thread to Idle — no live agent loop exists
    // to do cleanup, so the thread would remain stuck in Processing.
    if let Some(thread_id_str) = delegate.payload.get("_thread_id").and_then(|v| v.as_str()) {
        if let Ok(thread_id) = thread_id_str.parse::<Uuid>() {
            interrupt_turns_in_thread(
                storage,
                thread_id,
                &[TurnState::Pending, TurnState::Processing, TurnState::AwaitingApproval],
            )
            .await?;
            return Ok(());
        }
    }

    // Last resort: no thread annotation — interrupt all in-progress turns.
    // See BACKLOG.md [vpbv] for the scoping limitation.
    let active_turns = storage
        .find_in_progress_by_agent(&delegate.recipient_agent_id)
        .await?;

    let thread_ids: HashSet<Uuid> = active_turns.iter().map(|t| t.thread_id).collect();

    for turn in active_turns {
        if let Err(e) = storage.update_turn_state(turn.id, TurnState::Interrupted).await {
            tracing::warn!(
                turn_id = %turn.id,
                error = %e,
                "fallback cancel: failed to interrupt turn — thread may remain stuck"
            );
        }
    }

    for thread_id in thread_ids {
        if let Err(e) = storage.update_state(thread_id, ThreadState::Idle).await {
            tracing::warn!(
                thread_id = %thread_id,
                error = %e,
                "fallback cancel: failed to reset thread to Idle"
            );
        }
    }

    Ok(())
}

/// Interrupt turns in a thread that match any of the given states, then reset
/// the thread to Idle.
///
/// Used by the cancel fallback paths where no live cancellation token exists —
/// interruption is communicated solely via storage writes. Logs warnings on
/// storage errors so stuck threads are diagnosable.
async fn interrupt_turns_in_thread(
    storage: &Arc<dyn Storage>,
    thread_id: Uuid,
    interruptible_states: &[TurnState],
) -> Result<(), AgentError> {
    let turns = storage.list_turns_by_thread(thread_id).await?;
    for turn in turns {
        if interruptible_states.contains(&turn.state) {
            if let Err(e) = storage.update_turn_state(turn.id, TurnState::Interrupted).await {
                tracing::warn!(
                    turn_id = %turn.id,
                    thread_id = %thread_id,
                    error = %e,
                    "failed to interrupt turn in fallback cancel — thread may remain stuck"
                );
            }
        }
    }
    if let Err(e) = storage.update_state(thread_id, ThreadState::Idle).await {
        tracing::warn!(
            thread_id = %thread_id,
            error = %e,
            "failed to reset thread to Idle after fallback cancel"
        );
    }
    Ok(())
}

// TODO handle_shutdown leaves threads in Processing state after interrupting turns
// priority: normal | tags: review:inter-agent-comms, review:branch-inter-agent-communication
// handle_cancel resets the thread to Idle after interrupting its turn, but
// handle_shutdown does not. Any thread whose turn is interrupted by a shutdown
// message stays in Processing indefinitely, causing subsequent turn creation to
// fail with TurnInProgress until manual intervention. Add thread-to-Idle
// resets after the for-loop over active_turns (mirroring the logic at the
// end of handle_cancel).
// ---
/// Handle a Shutdown message by interrupting all active turns for the target.
///
/// The shutdown message itself is already claimed by the mailbox. This handler
/// interrupts any in-progress turns for the target agent.
pub async fn handle_shutdown(
    storage: &Arc<dyn Storage>,
    shutdown_msg: &AgentMessage,
    active_delegations: Option<&ActiveDelegations>,
) -> Result<(), AgentError> {
    debug_assert_eq!(shutdown_msg.kind, MessageKind::Shutdown);

    // Cancel live execution tokens for all delegations targeting this agent.
    // Without this, agent turn loops continue running — they stop on
    // CancellationToken, not turn state changes in storage.
    if let Some(registry) = active_delegations {
        let active = registry.read().await;
        for (_id, delegation) in active.iter() {
            if delegation.recipient_agent_id == shutdown_msg.recipient_agent_id {
                delegation.cancel_token.cancel();
            }
        }
    }

    // Mark any in-progress turns as Interrupted in storage — covers turns
    // whose tokens aren't in the registry (already finished, or registry
    // wasn't provided).
    let active_turns = storage
        .find_in_progress_by_agent(&shutdown_msg.recipient_agent_id)
        .await?;

    for turn in active_turns {
        if let Err(e) = storage.update_turn_state(turn.id, TurnState::Interrupted).await {
            tracing::warn!(
                turn_id = %turn.id,
                error = %e,
                "shutdown: failed to interrupt turn"
            );
        }
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

        // Verify the thread was also reset to Idle — without this, the thread
        // remains stuck in Processing and future turns fail with TurnInProgress.
        let thread = ThreadStore::get(storage.as_ref(), turn.thread_id).await.unwrap().unwrap();
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

        let result = handle_cancel(&(storage.clone() as Arc<dyn Storage>), &cancel, None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AgentError::InvalidConfig(msg) => {
                assert!(msg.contains("correlation_id"), "error should mention correlation_id: {msg}");
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_thread_id_annotation_scopes_to_annotated_thread() {
        // Set up two threads for agent-b — the annotation should scope
        // interruption to only the annotated thread.
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let project = ProjectStore::get_or_create_default(storage.as_ref())
            .await
            .unwrap();

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "agent-b".into(),
            })
            .await
            .unwrap();

        // Thread A — the annotated target.
        let thread_a = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
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

        // Thread B — should NOT be interrupted.
        let thread_b = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
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
        handle_cancel(&(storage.clone() as Arc<dyn Storage>), &cancel, None)
            .await
            .unwrap();

        // Thread A's turn should be interrupted.
        let ta = storage.get_turn(turn_a.id).await.unwrap().unwrap();
        assert_eq!(ta.state, TurnState::Interrupted, "annotated thread's turn should be interrupted");

        // Thread A should be reset to Idle.
        let thread_a_state = ThreadStore::get(storage.as_ref(), thread_a.id).await.unwrap().unwrap();
        assert_eq!(thread_a_state.state, ThreadState::Idle, "annotated thread should be reset to Idle");

        // Thread B's turn should be unaffected.
        let tb = storage.get_turn(turn_b.id).await.unwrap().unwrap();
        assert_eq!(tb.state, TurnState::Processing, "other thread's turn should be unaffected");
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
        handle_cancel(&(storage.clone() as Arc<dyn Storage>), &cancel, None)
            .await
            .unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted, "should still interrupt via broad fallback");
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
                recipient_agent_id: "agent-b".into(),
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

        handle_shutdown(&(storage.clone() as Arc<dyn Storage>), &shutdown, None).await.unwrap();

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_cancels_active_delegation_tokens() {
        let (storage, _turn_id) = setup_with_turn("agent-b").await;

        // Register a delegation targeting agent-b.
        let registry = crate::delegation::active_delegations();
        let cancel_token = tokio_util::sync::CancellationToken::new();
        {
            let mut active = registry.write().await;
            active.insert(Uuid::new_v4(), crate::delegation::ActiveDelegation {
                session_id: Uuid::new_v4(),
                thread_id: Uuid::new_v4(),
                recipient_agent_id: "agent-b".into(),
                cancel_token: cancel_token.clone(),
            });
        }

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

        handle_shutdown(
            &(storage.clone() as Arc<dyn Storage>),
            &shutdown,
            Some(&registry),
        )
        .await
        .unwrap();

        // Token should have been cancelled — live execution would stop.
        assert!(cancel_token.is_cancelled(), "shutdown must cancel live execution tokens");
    }
}
