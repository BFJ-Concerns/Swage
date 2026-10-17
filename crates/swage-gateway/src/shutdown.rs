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

use swage_core::{ApprovalTimeoutPolicy, MessageKind, StorageError};
use swage_storage::MessageOrigin;
use swage_storage::model::{
    ApprovalState, NewAgentMessage, NewQueuedTurnInput, Session, ThreadState, Turn, TurnState,
};
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
    topic_store: &dyn TopicStore,
) -> Result<usize, StorageError> {
    let mut recovered_turn_ids = HashSet::new();
    let mut seen_turn_ids = HashSet::new();
    // Parents cut off while waiting on a delegated child: interrupted like
    // any in-flight turn here, then resumed once every child's fate is
    // settled by this same pass (resume_delegating_parent).
    let mut delegating_parents: Vec<(Session, Turn)> = Vec::new();

    for topic in topic_store.list_scopes().await? {
        for session in storage.list_by_topic(topic.id).await? {
            for thread in storage.list_by_session(session.id).await? {
                for turn in storage.list_turns_by_thread(thread.id).await? {
                    if !seen_turn_ids.insert(turn.id) {
                        continue;
                    }
                    // A delegating turn that is itself a child — a middle tier
                    // of a nested delegation — is its own parent's to return
                    // to: that parent is told it was cut off, and resuming it
                    // on its own would run beside the parent's return to it.
                    let delegating =
                        turn.state == TurnState::Delegating && turn.parent_turn_id.is_none();
                    if delegating {
                        delegating_parents.push((session.clone(), turn.clone()));
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

                    // A delegating parent keeps its state until its
                    // resumption is on the record (below): `Delegating` is
                    // the one durable mark that selects it for resumption,
                    // so a failure before the hand-off leaves it for the
                    // next startup rather than consuming the mark first.
                    if !delegating {
                        storage
                            .update_turn_state(turn.id, TurnState::Interrupted)
                            .await?;
                    }
                }
            }
        }
    }

    for (session, parent) in delegating_parents {
        resume_delegating_parent(storage, &session, &parent).await?;
        storage
            .update_turn_state(parent.id, TurnState::Interrupted)
            .await?;
    }

    Ok(recovered_turn_ids.len())
}

/// Resume a parent cut off while waiting on a delegated child: the child's
/// fate goes to the parent agent's mailbox as a pending Result under the
/// delegate that started it — the child's own result when it completed
/// before the restart, its failure when it failed, or a cut-off account
/// with the child's session and thread when it was in flight — naming the
/// parent's session so that session's next turn, and no other session of
/// the same agent, delivers it; and a continuation input is queued on the
/// parent's session, so the startup drain starts the parent's next turn
/// with the Result delivered into its thread first. The parent stays
/// `Delegating` until this returns, so a write that fails here leaves it
/// for the next startup (which may then repeat a Result or continuation
/// already written: at least once, each naming the same outcome). A child
/// whose delegate message cannot be found is a gap in the record: recorded
/// as such, the notice still naming the child.
async fn resume_delegating_parent(
    storage: &dyn Storage,
    session: &Session,
    parent: &Turn,
) -> Result<(), StorageError> {
    let mut notices = Vec::new();
    for child in storage.list_child_turns(parent.id).await? {
        let child_session_id = storage
            .get(child.thread_id)
            .await?
            .map(|thread| thread.session_id);
        // The child's fate from its own row — what the record says it did,
        // never an outcome invented for it: a turn that ended on its own
        // keeps its ending; only a turn still in flight, or already cut
        // off, was cut off by a restart.
        let (mut payload, fate) = match child.state {
            TurnState::Completed => (
                serde_json::json!({
                    "response": child.assistant_response.clone().unwrap_or_default(),
                    "structured_output": child.structured_output,
                }),
                "completed before the restart; its result is delivered above",
            ),
            TurnState::Incomplete => (
                serde_json::json!({
                    "response": child.assistant_response.clone().unwrap_or_default(),
                    "incomplete_reason": child
                        .failure_message
                        .clone()
                        .unwrap_or_else(|| "the turn stopped short of completing".to_owned()),
                    "structured_output": child.structured_output,
                }),
                "stopped short before the restart; what it produced is delivered above",
            ),
            TurnState::Refused => (
                serde_json::json!({
                    "error": "the delegated agent refused the task",
                    "response": child.assistant_response,
                }),
                "refused the task before the restart",
            ),
            TurnState::Failed => (
                serde_json::json!({
                    "error": child
                        .failure_message
                        .clone()
                        .unwrap_or_else(|| "the delegated turn failed".to_owned()),
                }),
                "failed before the restart; its failure is delivered above",
            ),
            TurnState::Interrupted
            | TurnState::Pending
            | TurnState::Processing
            | TurnState::AwaitingApproval
            | TurnState::Delegating => (
                serde_json::json!({
                    "error": "cut off by a gateway restart before completing; its work so far is on its thread",
                }),
                "was cut off by the restart; its work so far is on its thread",
            ),
        };
        payload["child_session_id"] = serde_json::json!(child_session_id);
        payload["child_thread_id"] = serde_json::json!(child.thread_id);
        payload["parent_session_id"] = serde_json::json!(session.id);
        payload["parent_turn_id"] = serde_json::json!(parent.id);
        notices.push(format!(
            "Delegated agent {} (session {}, thread {}) {fate}.",
            child.agent_id,
            child_session_id.map_or_else(|| "unknown".to_owned(), |id| id.to_string()),
            child.thread_id
        ));
        match storage
            .find_delegate_for_thread(child.thread_id, child.created_at)
            .await?
        {
            Some(delegate) => {
                // A Result the child itself wrote and nothing has claimed
                // yet — a detached child's, or an attended one's cut between
                // its write and its claim — is already the record of its
                // fate and will be delivered; a second would read twice.
                let own_result_waiting = storage
                    .list_by_correlation(delegate.id)
                    .await?
                    .iter()
                    .any(|message| {
                        message.kind == MessageKind::Result
                            && message.status == swage_core::MessageStatus::Pending
                    });
                if !own_result_waiting {
                    storage
                        .create_agent_message(NewAgentMessage {
                            sender_agent_id: child.agent_id.clone(),
                            recipient_agent_id: parent.agent_id.clone(),
                            kind: MessageKind::Result,
                            correlation_id: Some(delegate.id),
                            payload,
                        })
                        .await?;
                }
            }
            None => {
                crate::record_durability::record_reconstruction_gap(
                    storage,
                    Some(session.id),
                    parent.id,
                    "delegation result after a restart",
                    &format!(
                        "no delegate message names the child thread {}; the child's outcome is in the continuation notice alone",
                        child.thread_id
                    ),
                )
                .await
                .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
            }
        }
    }
    let content = if notices.is_empty() {
        "The gateway restarted while this turn was waiting on a delegated agent, and no child turn of it is on the record. Continue from here.".to_owned()
    } else {
        format!(
            "The gateway restarted while this turn was waiting on a delegated agent. {} Continue from here.",
            notices.join(" ")
        )
    };
    let attended = continuation_is_attended(session);
    storage
        .enqueue_turn_input(NewQueuedTurnInput {
            delivery_targets: Vec::new(),
            client_id: None,
            attended,
            session_id: session.id,
            thread_id: parent.thread_id,
            origin: MessageOrigin::Delegation,
            content,
            attachments: Vec::new(),
            schedule_run_id: None,
            channel_message_ids: Vec::new(),
        })
        .await?;
    warn!(
        parent_turn_id = %parent.id,
        session_id = %session.id,
        attended,
        "Queued the resumption of a turn interrupted while delegating"
    );
    Ok(())
}

/// Whether a resumed turn runs attended: only a person's own conversation
/// is. A dispatched worker carries its dispatching ancestry in its session
/// metadata, and a channel-linked session its channel origin; both run
/// their turns unattended, as their own inputs do.
fn continuation_is_attended(session: &Session) -> bool {
    !session.metadata.as_ref().is_some_and(|metadata| {
        metadata
            .get(crate::turn::DISPATCH_BOUND_METADATA_KEY)
            .is_some()
            || metadata.get("channel_origin").is_some()
    })
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
    use uuid::Uuid;

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

    /// A parent cut off while delegating resumes: each child's fate reaches
    /// the parent agent's mailbox as a pending Result under the delegate that
    /// started it — the child's own result when it completed, a cut-off
    /// account when it was in flight — and a continuation input waits on the
    /// parent's session for the startup drain.
    #[tokio::test]
    async fn recovery_resumes_a_delegating_parent_with_its_childrens_fates() {
        use swage_core::{MessageKind, MessageStatus};
        use swage_storage::model::NewAgentMessage;
        use swage_storage::store::{AgentMessageStore, TurnQueueStore};

        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "lead".to_string(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "lead".to_string(),
            },
        )
        .await
        .unwrap();
        let parent = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "lead".to_string(),
                state: TurnState::Delegating,
                user_input: Some("delegate the survey and the review".to_string()),
                parent_turn_id: None,
            })
            .await
            .unwrap();

        // Four children: one completed before the restart, one that stopped
        // short, one cut off by the restart, and a detached one whose own
        // Result still waits in the mailbox.
        let mut delegates = Vec::new();
        for (agent, state, response) in [
            (
                "surveyor",
                TurnState::Completed,
                Some("The survey: three findings."),
            ),
            ("drafter", TurnState::Incomplete, Some("Half the draft.")),
            ("reviewer", TurnState::Processing, None),
            ("scout", TurnState::Completed, Some("Scouted.")),
        ] {
            let child_session = storage
                .create_session(NewSession {
                    agent_id: agent.to_string(),
                    ..swage_storage::fixtures::new_session(topic.id)
                })
                .await
                .unwrap();
            let child_thread = ThreadStore::create(
                &*storage,
                NewThread {
                    session_id: child_session.id,
                    agent_id: agent.to_string(),
                },
            )
            .await
            .unwrap();
            // The delegate starts the child: it is on the record before the
            // child's turn, as the executor writes them.
            let delegate = storage
                .create_agent_message(NewAgentMessage {
                    sender_agent_id: "lead".to_string(),
                    recipient_agent_id: agent.to_string(),
                    kind: MessageKind::Delegate,
                    correlation_id: None,
                    payload: serde_json::json!({
                        "task": "the task",
                        "_thread_id": child_thread.id.to_string(),
                    }),
                })
                .await
                .unwrap();
            let child = storage
                .create_turn(NewTurn {
                    thread_id: child_thread.id,
                    agent_id: agent.to_string(),
                    state,
                    user_input: Some("the task".to_string()),
                    parent_turn_id: Some(parent.id),
                })
                .await
                .unwrap();
            if let Some(response) = response {
                storage
                    .db
                    .lock()
                    .await
                    .turns
                    .get_mut(&child.id)
                    .unwrap()
                    .assistant_response = Some(response.to_string());
            }
            if agent == "scout" {
                // The detached child's own Result, written by its executor and
                // not yet claimed: the record of its fate already.
                storage
                    .create_agent_message(NewAgentMessage {
                        sender_agent_id: "scout".to_string(),
                        recipient_agent_id: "lead".to_string(),
                        kind: MessageKind::Result,
                        correlation_id: Some(delegate.id),
                        payload: serde_json::json!({ "response": "Scouted." }),
                    })
                    .await
                    .unwrap();
            }
            delegates.push((agent, delegate.id, child_session.id, child_thread.id));
        }

        let recovered = recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");
        assert_eq!(recovered, 2, "the parent and the in-flight child");
        assert_eq!(
            storage.get_turn(parent.id).await.unwrap().unwrap().state,
            TurnState::Interrupted
        );

        // Each child's fate waits in the lead's mailbox under its delegate.
        let pending = storage.list_pending_for_agent("lead").await.unwrap();
        let results: Vec<_> = pending
            .iter()
            .filter(|m| m.kind == MessageKind::Result)
            .collect();
        assert_eq!(
            results.len(),
            4,
            "one Result per child, the scout's its own: {pending:?}"
        );
        for (agent, delegate_id, child_session_id, child_thread_id) in &delegates {
            let under: Vec<_> = results
                .iter()
                .filter(|m| m.correlation_id == Some(*delegate_id))
                .collect();
            assert_eq!(
                under.len(),
                1,
                "exactly one Result under {agent}'s delegate"
            );
            let result = under[0];
            if *agent == "scout" {
                assert!(
                    result.payload.get("parent_session_id").is_none(),
                    "the scout's own Result stands; recovery wrote no second"
                );
                continue;
            }
            assert_eq!(result.sender_agent_id, *agent);
            assert_eq!(result.status, MessageStatus::Pending);
            assert_eq!(
                result.payload["child_session_id"],
                serde_json::json!(child_session_id)
            );
            assert_eq!(
                result.payload["child_thread_id"],
                serde_json::json!(child_thread_id)
            );
            assert_eq!(
                result.payload["parent_session_id"],
                serde_json::json!(session.id),
                "the Result names the parent's session, so only that session's turn delivers it"
            );
            match *agent {
                "surveyor" => {
                    assert_eq!(result.payload["response"], "The survey: three findings.");
                }
                "drafter" => {
                    assert_eq!(result.payload["response"], "Half the draft.");
                    assert!(result.payload["incomplete_reason"].is_string());
                }
                _ => assert!(
                    result.payload["error"]
                        .as_str()
                        .is_some_and(|e| e.contains("cut off by a gateway restart")),
                    "{:?}",
                    result.payload
                ),
            }
        }

        // The continuation waits on the parent's session for the startup
        // drain, attended: this is a person's session, not a worker's.
        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].thread_id, thread.id);
        assert_eq!(queued[0].origin, swage_storage::MessageOrigin::Delegation);
        assert!(queued[0].attended);
        assert!(
            queued[0]
                .content
                .contains("The gateway restarted while this turn was waiting")
        );
        assert!(queued[0].content.contains("surveyor"));
        assert!(queued[0].content.contains("drafter"));
        assert!(queued[0].content.contains("stopped short"));
        assert!(queued[0].content.contains("reviewer"));
    }

    /// A middle tier of a nested delegation — a delegating turn that is
    /// itself a child — is not resumed on its own: its parent is told it
    /// was cut off and returns to it; only the top-level parent resumes.
    #[tokio::test]
    async fn recovery_resumes_only_the_top_of_a_nested_delegation() {
        use swage_core::MessageKind;
        use swage_storage::model::NewAgentMessage;
        use swage_storage::store::{AgentMessageStore, TurnQueueStore};

        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let mut tiers: Vec<(&str, Uuid, Uuid)> = Vec::new();
        let mut parent_turn_id = None;
        for (agent, state) in [
            ("lead", TurnState::Delegating),
            ("middle", TurnState::Delegating),
            ("leaf", TurnState::Processing),
        ] {
            let session = storage
                .create_session(NewSession {
                    agent_id: agent.to_string(),
                    ..swage_storage::fixtures::new_session(topic.id)
                })
                .await
                .unwrap();
            let thread = ThreadStore::create(
                &*storage,
                NewThread {
                    session_id: session.id,
                    agent_id: agent.to_string(),
                },
            )
            .await
            .unwrap();
            if let Some((parent_agent, _, _)) = tiers.last() {
                storage
                    .create_agent_message(NewAgentMessage {
                        sender_agent_id: parent_agent.to_string(),
                        recipient_agent_id: agent.to_string(),
                        kind: MessageKind::Delegate,
                        correlation_id: None,
                        payload: serde_json::json!({
                            "task": "work",
                            "_thread_id": thread.id.to_string(),
                        }),
                    })
                    .await
                    .unwrap();
            }
            let turn = storage
                .create_turn(NewTurn {
                    thread_id: thread.id,
                    agent_id: agent.to_string(),
                    state,
                    user_input: Some("work".to_string()),
                    parent_turn_id,
                })
                .await
                .unwrap();
            tiers.push((agent, session.id, turn.id));
            parent_turn_id = Some(turn.id);
        }

        recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");

        let (_, lead_session, _) = tiers[0];
        let (_, middle_session, middle_turn) = tiers[1];
        assert_eq!(
            storage
                .list_queued_turn_inputs(lead_session)
                .await
                .unwrap()
                .len(),
            1,
            "the lead resumes"
        );
        assert!(
            storage
                .list_queued_turn_inputs(middle_session)
                .await
                .unwrap()
                .is_empty(),
            "the middle tier is its parent's to return to, not resumed on its own"
        );
        assert_eq!(
            storage.get_turn(middle_turn).await.unwrap().unwrap().state,
            TurnState::Interrupted
        );
        let lead_results: Vec<_> = storage
            .list_pending_for_agent("lead")
            .await
            .unwrap()
            .into_iter()
            .filter(|m| m.kind == MessageKind::Result)
            .collect();
        assert_eq!(lead_results.len(), 1);
        assert!(
            lead_results[0].payload["error"]
                .as_str()
                .is_some_and(|e| e.contains("cut off")),
            "the lead is told its child was cut off"
        );
        assert!(
            storage
                .list_pending_for_agent("middle")
                .await
                .unwrap()
                .iter()
                .all(|m| m.kind != MessageKind::Result),
            "no Result is written for the middle tier's own child"
        );
    }

    /// A child whose delegate message cannot be found is a gap in the record:
    /// recorded as a degraded turn record, the continuation still naming the
    /// child, so the parent resumes with what the record holds.
    #[tokio::test]
    async fn recovery_records_a_gap_for_a_child_without_its_delegate_and_still_resumes() {
        use swage_storage::store::TurnQueueStore;

        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "lead".to_string(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "lead".to_string(),
            },
        )
        .await
        .unwrap();
        let parent = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "lead".to_string(),
                state: TurnState::Delegating,
                user_input: Some("delegate".to_string()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let child_thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "worker".to_string(),
            },
        )
        .await
        .unwrap();
        storage
            .create_turn(NewTurn {
                thread_id: child_thread.id,
                agent_id: "worker".to_string(),
                state: TurnState::Processing,
                user_input: Some("the task".to_string()),
                parent_turn_id: Some(parent.id),
            })
            .await
            .unwrap();

        recover_orphaned_turns(&*storage, &*storage)
            .await
            .expect("recovery should not fail");

        let gaps: Vec<_> = storage
            .db
            .lock()
            .await
            .audit_events
            .iter()
            .filter(|event| event.event_kind == "turn_record_degraded")
            .cloned()
            .collect();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].entity_id, parent.id.to_string());
        assert_eq!(
            gaps[0].metadata["record"],
            "delegation result after a restart"
        );
        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].content.contains("worker"));
    }

    /// Only a person's own conversation resumes attended: a dispatched
    /// worker's and a channel-linked session's turns run unattended, as
    /// their own inputs do.
    #[test]
    fn a_continuation_is_attended_only_for_a_persons_session() {
        let session = |metadata: Option<serde_json::Value>| Session {
            metadata,
            ..swage_storage::fixtures::session(Uuid::new_v4())
        };
        assert!(continuation_is_attended(&session(None)));
        assert!(continuation_is_attended(&session(Some(
            serde_json::json!({ "other": 1 })
        ))));
        assert!(!continuation_is_attended(&session(Some(
            serde_json::json!({
                crate::turn::DISPATCH_BOUND_METADATA_KEY: ["lead"]
            })
        ))));
        assert!(!continuation_is_attended(&session(Some(
            serde_json::json!({
                "channel_origin": { "integration": "telegram" }
            })
        ))));
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
