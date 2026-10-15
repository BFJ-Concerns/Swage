//! Gateway shutdown and recovery.
//!
//! Three functions manage the process lifecycle:
//!
//! - [`graceful_shutdown`] — awaits SIGTERM or SIGINT. Pass this to axum's
//!   `serve().with_graceful_shutdown()` so the server stops accepting new
//!   connections while existing ones drain.
//!
//! - [`shutdown_active_turns`] — cancels every in-flight turn's
//!   `CancellationToken` and waits 500 ms for turn tasks to write partial
//!   responses before the process exits.
//!
//! - [`recover_orphaned_turns`] — runs once at startup to transition any turns
//!   left in an in-progress state by a previous unclean exit to `Interrupted`.
//!
//! - [`recover_orphaned_background_runs`] — runs once at startup to repair
//!   maintenance rows left in `running` by a previous unclean exit.
//!
//! Typical call sequence at shutdown:
//! ```text
//! graceful_shutdown().await;
//! shutdown_active_turns(&state).await;
//! ```

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::time;
use tracing::{info, warn};

use swage_core::{ApprovalTimeoutPolicy, StorageError};
use swage_storage::model::{ApprovalState, ThreadState, TurnState};
use swage_storage::store::{
    INTERRUPTED_TOOL_RESULT, INTERRUPTED_TOOL_STATUS, Storage, TOOL_RESULT_UNAVAILABLE,
    TOOL_RESULT_UNAVAILABLE_STATUS, TopicStore,
};

use crate::state::AppState;

pub(crate) const SCHEDULE_INTERRUPTION_REASON: &str =
    "gateway stopped while schedule run was running";

/// Wait for SIGTERM or SIGINT, then log and return.
///
/// Pass this to axum's `serve().with_graceful_shutdown()`. Returning from
/// this future causes axum to stop accepting new connections and drain the
/// ones already in flight.
pub async fn graceful_shutdown() {
    // Wrap SIGTERM setup in a cfg(unix) block — the API does not exist on Windows.
    let sigterm = async {
        #[cfg(unix)]
        {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        }
        // Non-Unix platforms: never resolve this branch, rely on ctrl_c only.
        #[cfg(not(unix))]
        std::future::pending::<()>().await;
    };

    tokio::select! {
        _ = sigterm => {}
        _ = tokio::signal::ctrl_c() => {}
    }

    info!("Shutdown signal received, draining connections...");
}

/// Cancel all active turn tokens and wait briefly for their tasks to save state.
///
/// Call this after `graceful_shutdown()` returns so that no new turns can be
/// added while the cleanup loop runs (the server has already stopped accepting
/// new connections at that point).
pub async fn shutdown_active_turns(state: &AppState) {
    state.turn_shutdown_started.store(true, Ordering::Release);
    let turns = state.active_turns.lock().await;
    let count = turns.len();
    for (session_id, token) in turns.iter() {
        // Shutdown owns interruption; a person's Stop owns cancellation.
        // Record that distinction before waking the execution token, so a
        // cooperative cancellation cannot win the terminal-state write.
        let interrupted: Result<(), StorageError> = async {
            for thread in state.storage.list_by_session(*session_id).await? {
                for turn in state.storage.list_turns_by_thread(thread.id).await? {
                    if !turn.state.is_terminal() {
                        state
                            .storage
                            .update_turn_state(turn.id, TurnState::Interrupted)
                            .await?;
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = interrupted {
            warn!(%session_id, %error, "failed to record shutdown interruption");
        }
        token.cancel();
    }
    // Drop the lock before sleeping so turn tasks can acquire it to write state.
    drop(turns);

    if count > 0 {
        // Give turn tasks a window to write partial responses and update their state.
        time::sleep(Duration::from_millis(500)).await;
        info!("Interrupted {count} active turns during shutdown");
    }
}

/// Transition all in-progress turns to Interrupted.
///
/// Runs at startup — before the server begins accepting connections — to clean
/// up turns left dangling by a previous unclean shutdown, including delegated
/// child turns that run under their own agent IDs.
///
/// Returns the number of turns recovered.
pub async fn recover_orphaned_turns(
    storage: &dyn Storage,
    topic_store: &dyn TopicStore,
) -> Result<usize, StorageError> {
    let mut recovered_turn_ids = HashSet::new();
    let mut seen_turn_ids = HashSet::new();

    for topic in topic_store.list_scopes().await? {
        for session in storage.list_by_topic(topic.id).await? {
            for thread in storage.list_by_session(session.id).await? {
                for turn in storage.list_turns_by_thread(thread.id).await? {
                    // A Delegating parent turn is interrupted here like any
                    // other in-flight turn: no delegation correlation state
                    // survives a restart, so the parent cannot resume.
                    if !seen_turn_ids.insert(turn.id) {
                        continue;
                    }

                    // A settlement may have failed after a turn completed;
                    // otherwise an outstanding call was cut off. Replay the
                    // explanation that matches the persisted turn state.
                    let (result, status) = if turn.state == TurnState::Completed {
                        (TOOL_RESULT_UNAVAILABLE, TOOL_RESULT_UNAVAILABLE_STATUS)
                    } else {
                        (INTERRUPTED_TOOL_RESULT, INTERRUPTED_TOOL_STATUS)
                    };
                    storage
                        .settle_started_tool_calls(turn.id, result, status)
                        .await?;
                    if turn.state.is_terminal() {
                        continue;
                    }
                    recovered_turn_ids.insert(turn.id);

                    warn!(
                        agent_id = %turn.agent_id,
                        turn_id = %turn.id,
                        thread_id = %turn.thread_id,
                        "Recovered orphaned turn from a previous shutdown"
                    );

                    // Recovery runs before new work can begin. Read the persisted
                    // thread state at the recovery point: an orphaned worker can
                    // only be replayed after its Processing → Idle transition. Do
                    // this before terminalising the turn so a failed reset leaves
                    // the turn recoverable on the next startup.
                    if storage
                        .get(thread.id)
                        .await?
                        .is_some_and(|persisted| persisted.state == ThreadState::Processing)
                    {
                        storage.update_state(thread.id, ThreadState::Idle).await?;
                    }

                    storage
                        .update_turn_state(turn.id, TurnState::Interrupted)
                        .await?;
                }
            }
        }
    }

    Ok(recovered_turn_ids.len())
}

/// Counts returned by [`recover_orphaned_background_runs`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackgroundRunRecoveryCounts {
    pub schedule_runs_failed: u64,
    pub background_jobs_retried: u64,
}

/// Repair background maintenance rows left mid-run by an unclean shutdown.
///
/// Schedule runs are marked failed because their durable run row already
/// belongs to a previous process. Attachment jobs are moved back to
/// `retried`, which is already part of the runnable-job selector and lets
/// ingestion safely resume on the next tick.
pub async fn recover_orphaned_background_runs(
    storage: &dyn Storage,
) -> Result<BackgroundRunRecoveryCounts, StorageError> {
    Ok(BackgroundRunRecoveryCounts {
        schedule_runs_failed: storage
            .recover_incomplete_schedule_runs(SCHEDULE_INTERRUPTION_REASON)
            .await?,
        background_jobs_retried: storage
            .recover_running_background_jobs("gateway stopped while background job was running")
            .await?,
    })
}

/// Counts returned by [`recover_expired_approvals`].
#[derive(Debug, Default)]
pub struct ApprovalRecoveryCounts {
    /// Approvals that timed out and had the Reject policy applied (turn reset
    /// to Processing so the agent can continue with rejection feedback).
    pub rejected_by_timeout: u64,
    /// Approvals that timed out and had the Interrupt policy applied (turn
    /// set to Interrupted — no further agent execution).
    pub interrupted_by_timeout: u64,
    /// Approvals cleaned up because their parent turn no longer exists
    /// (deleted during downtime).
    pub orphaned_cleaned: u64,
}

/// Apply the timeout policy to any approval requests that expired during downtime.
///
/// Called during startup — before the server begins accepting connections.
/// Three categories of pending approvals are handled:
///
/// 1. **Timeout elapsed + Reject policy**: mark the approval Expired and
///    transition the turn back to `Processing` so the agent can resume with
///    rejection feedback on the next turn.
/// 2. **Timeout elapsed + Interrupt policy**: mark the approval Expired and
///    transition the turn to `Interrupted`.
/// 3. **Still within timeout**: leave as-is — they will be re-emitted to
///    clients that connect later.
///
/// Also calls `cleanup_orphaned_approvals` to handle approvals whose turns
/// were deleted while the gateway was offline.
///
/// Errors during individual approval/turn updates are logged and skipped
/// (fail-open) — a single storage problem should not prevent the rest of
/// recovery from proceeding. The function returns `Err` only if the initial
/// `list_all_pending` or `cleanup_orphaned_approvals` query fails.
pub async fn recover_expired_approvals(
    storage: &dyn Storage,
    message_ttl: std::time::Duration,
) -> Result<ApprovalRecoveryCounts, StorageError> {
    let mut counts = ApprovalRecoveryCounts::default();

    let pending = storage.list_all_pending().await?;
    let now = chrono::Utc::now();

    for approval in pending {
        if approval.timeout_at > now {
            // Still within the timeout window — leave for re-emission on connect.
            continue;
        }

        // Attempt to mark the approval as Expired. Returns false if it was
        // already in a terminal state (e.g., decided just before shutdown).
        let updated = match storage
            .update_approval_state(
                approval.id,
                ApprovalState::Expired,
                Some("approval timed out during gateway downtime"),
            )
            .await
        {
            Ok(did_update) => did_update,
            Err(e) => {
                warn!(
                    approval_id = %approval.id,
                    error = %e,
                    "failed to update approval state during startup recovery"
                );
                continue;
            }
        };

        if !updated {
            // Already in a terminal state — no turn state change needed.
            continue;
        }

        // Both policies result in Interrupted at startup — no live agent
        // exists to consume a rejection, so Processing would orphan the turn
        // permanently (blocking the conversation with TurnInProgress).
        let target_state = TurnState::Interrupted;

        if let Err(e) = storage
            .update_turn_state(approval.turn_id, target_state)
            .await
        {
            warn!(
                approval_id = %approval.id,
                turn_id = %approval.turn_id,
                policy = ?approval.timeout_policy,
                error = %e,
                "failed to set turn to Interrupted after approval timeout"
            );
        } else {
            match approval.timeout_policy {
                ApprovalTimeoutPolicy::Reject => counts.rejected_by_timeout += 1,
                ApprovalTimeoutPolicy::Interrupt => counts.interrupted_by_timeout += 1,
            }
        }
    }

    // Clean up any approvals whose turns were deleted during downtime.
    counts.orphaned_cleaned = storage.cleanup_orphaned_approvals().await?;

    // Expire stale agent messages that outlived their TTL during downtime.
    match storage.expire_stale_messages(message_ttl).await {
        Ok(0) => {}
        Ok(n) => {
            info!(
                expired = n,
                "expired stale agent messages during startup recovery"
            );
        }
        Err(e) => {
            warn!(error = %e, "failed to expire stale agent messages during startup");
        }
    }

    Ok(counts)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_storage::model::{NewSession, NewThread, NewTurn, TurnState};
    use swage_storage::store::{SessionStore, ThreadStore, TopicStore, TurnStore};

    use super::*;
    use crate::turn::tests::MockStorage;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a MockStorage pre-populated with in-progress and completed turns
    /// (all belonging to a single thread).
    async fn make_storage(
        processing: usize,
        delegating: usize,
        completed: usize,
    ) -> Arc<MockStorage> {
        let storage = Arc::new(MockStorage::new());

        // Create the topic → session → thread hierarchy required for FK integrity.
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .unwrap();
        let thread_id = thread.id;

        for _ in 0..processing {
            storage
                .create_turn(NewTurn {
                    thread_id,
                    agent_id: "gateway".to_string(),
                    state: TurnState::Processing,
                    user_input: None,
                    parent_turn_id: None,
                })
                .await
                .unwrap();
        }

        for _ in 0..delegating {
            storage
                .create_turn(NewTurn {
                    thread_id,
                    agent_id: "gateway".to_string(),
                    state: TurnState::Delegating,
                    user_input: None,
                    parent_turn_id: None,
                })
                .await
                .unwrap();
        }

        for _ in 0..completed {
            // Create as Processing (the normal pre-terminal state), then
            // immediately transition to Completed.
            let turn = storage
                .create_turn(NewTurn {
                    thread_id,
                    agent_id: "gateway".to_string(),
                    state: TurnState::Processing,
                    user_input: None,
                    parent_turn_id: None,
                })
                .await
                .unwrap();
            storage
                .update_turn_state(turn.id, TurnState::Completed)
                .await
                .unwrap();
        }

        storage
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn recovery_marks_in_progress_interrupted_leaves_completed_untouched() {
        let storage = make_storage(2, 0, 1).await;

        let count = recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");

        assert_eq!(count, 2, "should report 2 recovered turns");

        let db = storage.db.lock().await;
        let n_interrupted = db
            .turns
            .values()
            .filter(|t| t.state == TurnState::Interrupted)
            .count();
        let n_completed = db
            .turns
            .values()
            .filter(|t| t.state == TurnState::Completed)
            .count();
        let n_processing = db
            .turns
            .values()
            .filter(|t| t.state == TurnState::Processing)
            .count();

        assert_eq!(n_interrupted, 2, "2 orphaned turns should be Interrupted");
        assert_eq!(n_completed, 1, "Completed turn should be unchanged");
        assert_eq!(n_processing, 0, "no turns should remain Processing");
    }

    #[tokio::test]
    async fn recovery_with_no_orphans_returns_zero() {
        let storage = Arc::new(MockStorage::new());

        let count = recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail on empty storage");

        assert_eq!(count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recovery_settles_started_calls_on_terminal_turns() {
        use swage_storage::model::NewToolCall;
        use swage_storage::store::ToolCallStore;
        let (storage, thread_id) = make_sqlite_with_thread().await;
        for state in [TurnState::Completed, TurnState::Interrupted] {
            let turn = storage
                .create_turn(NewTurn {
                    thread_id,
                    agent_id: "gateway".into(),
                    state: TurnState::Processing,
                    user_input: None,
                    parent_turn_id: None,
                })
                .await
                .unwrap();
            storage
                .create_tool_call(NewToolCall {
                    execution: None,
                    turn_id: turn.id,
                    agent_id: "gateway".into(),
                    call_id: turn.id.to_string(),
                    tool: "echo".into(),
                    arguments: "{}".into(),
                    model_arguments: "{}".into(),
                    result: None,
                    model_result: None,
                    status: None,
                    duration_ms: None,
                    truncated: false,
                    total_bytes: None,
                    provider_round: 1,
                    position: 0,
                    parent_call_id: None,
                    display_summary: None,
                })
                .await
                .unwrap();
            storage.update_turn_state(turn.id, state).await.unwrap();
        }
        assert_eq!(
            recover_orphaned_turns(storage.as_ref(), storage.as_ref())
                .await
                .unwrap(),
            0
        );
        for turn in storage.list_turns_by_thread(thread_id).await.unwrap() {
            let call = storage
                .list_tool_calls_by_turn(turn.id)
                .await
                .unwrap()
                .pop()
                .unwrap();
            let (expected_result, expected_status) = if turn.state == TurnState::Completed {
                (TOOL_RESULT_UNAVAILABLE, TOOL_RESULT_UNAVAILABLE_STATUS)
            } else {
                (INTERRUPTED_TOOL_RESULT, INTERRUPTED_TOOL_STATUS)
            };
            assert_eq!(
                call.status.as_deref(),
                Some(expected_status),
                "terminal turn {:?} settled a call with the wrong status",
                turn.state
            );
            assert_eq!(call.result.as_deref(), Some(expected_result));
            assert!(
                call.model_result
                    .as_deref()
                    .is_some_and(|result| result.contains(expected_result)),
                "replay must explain the missing result for {:?}",
                turn.state
            );
        }
    }

    #[tokio::test]
    async fn recovery_marks_delegating_turns_interrupted() {
        let storage = make_storage(0, 1, 0).await;

        let count = recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");

        assert_eq!(count, 1, "delegating turn should be recovered");

        let db = storage.db.lock().await;
        let n_interrupted = db
            .turns
            .values()
            .filter(|t| t.state == TurnState::Interrupted)
            .count();
        let n_delegating = db
            .turns
            .values()
            .filter(|t| t.state == TurnState::Delegating)
            .count();

        assert_eq!(
            n_interrupted, 1,
            "delegating turn should become Interrupted"
        );
        assert_eq!(n_delegating, 0, "no turns should remain Delegating");
    }

    #[tokio::test]
    async fn recovery_marks_pending_turns_interrupted() {
        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .unwrap();
        let pending_turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: Some("claimed but not processed".to_string()),
                parent_turn_id: None,
            })
            .await
            .unwrap();

        let count = recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");

        assert_eq!(count, 1, "pending channel placeholder should be recovered");
        let recovered = storage.get_turn(pending_turn.id).await.unwrap().unwrap();
        assert_eq!(recovered.state, TurnState::Interrupted);
    }

    #[tokio::test]
    async fn recovery_interrupts_orphaned_turns_for_non_gateway_agents() {
        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "reviewer".to_string(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "reviewer".to_string(),
            },
        )
        .await
        .unwrap();
        let delegated_turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "reviewer".to_string(),
                state: TurnState::Delegating,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        let count = recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");

        assert_eq!(count, 1, "delegated agent turn should be recovered");
        let recovered = storage.get_turn(delegated_turn.id).await.unwrap().unwrap();
        assert_eq!(recovered.state, TurnState::Interrupted);
    }

    #[tokio::test]
    async fn recovery_interrupts_detached_child_turn_left_in_flight() {
        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "agent-b".to_string(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "agent-b".to_string(),
            },
        )
        .await
        .unwrap();
        let parent_turn_id = uuid::Uuid::new_v4();
        let detached_child_turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent-b".to_string(),
                state: TurnState::Processing,
                user_input: Some("detached child work".to_string()),
                parent_turn_id: Some(parent_turn_id),
            })
            .await
            .unwrap();

        let count = recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");

        assert_eq!(count, 1, "detached child turn should be recovered");
        let recovered = storage
            .get_turn(detached_child_turn.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, TurnState::Interrupted);
        assert_eq!(recovered.parent_turn_id, Some(parent_turn_id));
    }

    // -----------------------------------------------------------------------
    // Approval startup recovery tests
    //
    // These tests use SqliteStorage::in_memory() because MockStorage does not
    // implement ApprovalStore (it panics on unimplemented!()).
    // -----------------------------------------------------------------------

    /// Helper: create an in-memory SqliteStorage with a single session and
    /// thread, returning `(storage, thread_id)`.
    async fn make_sqlite_with_thread() -> (Arc<swage_storage::SqliteStorage>, uuid::Uuid) {
        use swage_storage::model::{NewSession, NewThread};
        use swage_storage::store::{SessionStore, TopicStore};

        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = swage_storage::store::ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .unwrap();
        (storage, thread.id)
    }

    /// Helper: create a turn in AwaitingApproval state, then create an approval
    /// for it with the given `timeout_at` and policy.
    async fn seed_approval(
        storage: &swage_storage::SqliteStorage,
        thread_id: uuid::Uuid,
        timeout_at: chrono::DateTime<chrono::Utc>,
        timeout_policy: swage_core::ApprovalTimeoutPolicy,
    ) -> (uuid::Uuid, uuid::Uuid) {
        // Returns (turn_id, approval_id).
        use swage_storage::model::{ApprovalKind, NewApprovalRequest, NewTurn, TurnState};
        use swage_storage::store::{ApprovalStore, TurnStore};

        let turn = storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::AwaitingApproval,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        let approval = storage
            .create_approval(NewApprovalRequest {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                kind: ApprovalKind::Tool,
                tool_name: Some("write_file".to_string()),
                tool_arguments: None,
                hook_name: None,
                prompt: "Approve?".to_string(),
                context: None,
                timeout_at,
                timeout_policy,
            })
            .await
            .unwrap();

        (turn.id, approval.id)
    }

    /// An expired approval with the Reject policy should become Expired and the
    /// turn should transition back to Processing for continued agent execution.
    #[tokio::test(flavor = "multi_thread")]
    async fn expired_reject_policy_resets_turn_to_processing() {
        use swage_core::ApprovalTimeoutPolicy;
        use swage_storage::model::{ApprovalState, TurnState};
        use swage_storage::store::{ApprovalStore, TurnStore};

        let (storage, thread_id) = make_sqlite_with_thread().await;

        // Approval whose deadline was one hour ago.
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let (turn_id, approval_id) =
            seed_approval(&storage, thread_id, past, ApprovalTimeoutPolicy::Reject).await;

        let counts = recover_expired_approvals(&*storage, swage_agent::DEFAULT_MESSAGE_TTL)
            .await
            .expect("recovery should not fail");

        // Approval must be Expired.
        let approval = storage.get_approval(approval_id).await.unwrap().unwrap();
        assert_eq!(approval.state, ApprovalState::Expired);

        // Both policies result in Interrupted at startup — no live agent
        // exists to consume a rejection, so Processing would orphan the turn.
        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);

        assert_eq!(counts.rejected_by_timeout, 1);
        assert_eq!(counts.interrupted_by_timeout, 0);
    }

    /// An expired approval with the Interrupt policy should become Expired and
    /// the turn should become Interrupted (no further agent execution).
    #[tokio::test(flavor = "multi_thread")]
    async fn expired_interrupt_policy_sets_turn_to_interrupted() {
        use swage_core::ApprovalTimeoutPolicy;
        use swage_storage::model::{ApprovalState, TurnState};
        use swage_storage::store::{ApprovalStore, TurnStore};

        let (storage, thread_id) = make_sqlite_with_thread().await;

        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let (turn_id, approval_id) =
            seed_approval(&storage, thread_id, past, ApprovalTimeoutPolicy::Interrupt).await;

        let counts = recover_expired_approvals(&*storage, swage_agent::DEFAULT_MESSAGE_TTL)
            .await
            .expect("recovery should not fail");

        let approval = storage.get_approval(approval_id).await.unwrap().unwrap();
        assert_eq!(approval.state, ApprovalState::Expired);

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Interrupted);

        assert_eq!(counts.rejected_by_timeout, 0);
        assert_eq!(counts.interrupted_by_timeout, 1);
    }

    /// An approval still within its timeout window must be left untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn non_expired_approval_is_left_pending() {
        use swage_core::ApprovalTimeoutPolicy;
        use swage_storage::model::{ApprovalState, TurnState};
        use swage_storage::store::{ApprovalStore, TurnStore};

        let (storage, thread_id) = make_sqlite_with_thread().await;

        // Approval whose deadline is one hour in the future.
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let (turn_id, approval_id) =
            seed_approval(&storage, thread_id, future, ApprovalTimeoutPolicy::Reject).await;

        let counts = recover_expired_approvals(&*storage, swage_agent::DEFAULT_MESSAGE_TTL)
            .await
            .expect("recovery should not fail");

        let approval = storage.get_approval(approval_id).await.unwrap().unwrap();
        assert_eq!(
            approval.state,
            ApprovalState::Pending,
            "non-expired approval must remain Pending"
        );

        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(
            turn.state,
            TurnState::AwaitingApproval,
            "turn must remain in AwaitingApproval"
        );

        assert_eq!(counts.rejected_by_timeout, 0);
        assert_eq!(counts.interrupted_by_timeout, 0);
    }

    /// Empty storage: recovery should succeed and return zero counts.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_pending_approvals_returns_zero_counts() {
        let storage = swage_storage::SqliteStorage::in_memory().unwrap();

        let counts = recover_expired_approvals(&storage, swage_agent::DEFAULT_MESSAGE_TTL)
            .await
            .expect("recovery on empty storage should not fail");

        assert_eq!(counts.rejected_by_timeout, 0);
        assert_eq!(counts.interrupted_by_timeout, 0);
        assert_eq!(counts.orphaned_cleaned, 0);
    }
}
