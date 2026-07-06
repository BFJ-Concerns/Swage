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
use std::time::Duration;

use tokio::time;
use tracing::{info, warn};

use swage_core::{ApprovalTimeoutPolicy, StorageError};
use swage_storage::model::{ApprovalState, TurnState};
use swage_storage::store::{BenchStore, Storage};

use crate::state::AppState;

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
    let turns = state.active_turns.lock().await;
    let count = turns.len();
    for token in turns.values() {
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
    bench_store: &dyn BenchStore,
) -> Result<usize, StorageError> {
    let mut recovered_turn_ids = HashSet::new();

    for bench in bench_store.list_all().await? {
        for session in storage.list_by_bench(bench.id).await? {
            for thread in storage.list_by_session(session.id).await? {
                for turn in storage.list_turns_by_thread(thread.id).await? {
                    // TODO Preserve delegated parent turns across restart
                    // Delegated parent turns should suspend/resume durably;
                    // this startup recovery path currently
                    // interrupts Delegating turns. Persist enough delegation
                    // correlation state to resume the parent turn or expose a
                    // recoverable suspended state after a gateway restart.
                    // ---
                    if !matches!(
                        turn.state,
                        TurnState::Pending
                            | TurnState::Processing
                            | TurnState::AwaitingApproval
                            | TurnState::Delegating
                    ) {
                        continue;
                    }
                    if !recovered_turn_ids.insert(turn.id) {
                        continue;
                    }

                    warn!(
                        agent_id = %turn.agent_id,
                        turn_id = %turn.id,
                        thread_id = %turn.thread_id,
                        "Recovered orphaned turn from a previous shutdown"
                    );
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
            .recover_incomplete_schedule_runs("gateway stopped while schedule run was running")
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
    use swage_storage::store::{BenchStore, SessionStore, ThreadStore, TurnStore};

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

        // Create the bench → session → thread hierarchy required for FK integrity.
        let bench = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                bench_id: bench.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
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
        let bench = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                bench_id: bench.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
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
        let bench = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                bench_id: bench.id,
                agent_id: "reviewer".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
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
        let bench = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                bench_id: bench.id,
                agent_id: "agent-b".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
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
    // Approval startup recovery tests [sfb5]
    //
    // These tests use SqliteStorage::in_memory() because MockStorage does not
    // implement ApprovalStore (it panics on unimplemented!()).
    // -----------------------------------------------------------------------

    /// Helper: create an in-memory SqliteStorage with a single session and
    /// thread, returning `(storage, thread_id)`.
    async fn make_sqlite_with_thread() -> (Arc<swage_storage::SqliteStorage>, uuid::Uuid) {
        use swage_storage::model::{NewSession, NewThread};
        use swage_storage::store::{BenchStore, SessionStore};

        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        let bench = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                bench_id: bench.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
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
