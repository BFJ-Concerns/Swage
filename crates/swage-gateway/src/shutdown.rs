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

use swage_agent::{AgentError, MessageRouter};
use swage_core::{ApprovalTimeoutPolicy, MessageKind, MessageStatus, Role, StorageError};
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
/// child turns that run under their own agent IDs. The router must use the
/// gateway's storage and configured lifecycle hooks for reconstructed Results.
///
/// Returns the number of turns recovered.
pub async fn recover_orphaned_turns(
    storage: &dyn Storage,
    topic_store: &dyn TopicStore,
    router: &MessageRouter,
) -> Result<usize, StorageError> {
    let mut recovered_turn_ids = HashSet::new();
    let mut seen_turn_ids = HashSet::new();
    // Parents cut off while waiting on a delegated child: interrupted like
    // any in-flight turn here, then resumed once every child's fate is
    // settled by this same pass (resume_delegating_parent).
    let mut delegating_parents: Vec<(Session, Turn, TurnOwner)> = Vec::new();

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
                        let owner = turn_owner(storage, &turn).await?;
                        delegating_parents.push((session.clone(), turn.clone(), owner));
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

    for (session, parent, owner) in delegating_parents {
        resume_delegating_parent(storage, router, &session, &parent, owner).await?;
    }

    Ok(recovered_turn_ids.len())
}

/// Whose turn a delegating parent was, by the origin of the input that
/// started it: a person's (or a channel's, or a delegation notice's), or
/// an automation's — a schedule's run or a workflow's agent call — whose
/// own recovery settles the run and redispatches the call, and which must
/// not be continued a second way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnOwner {
    Person,
    Automation,
}

/// Who the turn answers to, read from the inputs that started it: a turn
/// started by a schedule or a workflow with no person's input beside it
/// is theirs to recover; a turn with a person's input among its starters
/// — a message batched with a scheduled brief — is the person's, and
/// resumes. A recovered continuation among those starters retains that
/// ownership. Rows appended later — a delivered child's result, a notice —
/// say nothing about who started it.
async fn turn_owner(storage: &dyn Storage, turn: &Turn) -> Result<TurnOwner, StorageError> {
    let messages: Vec<_> = storage
        .list_messages_by_thread(turn.thread_id)
        .await?
        .into_iter()
        .filter(|message| message.turn_id == Some(turn.id) && message.role == Role::User)
        .collect();
    // The drain records the starter rows first and joins their content into
    // user_input with blank lines. Stop at the end of that input: later
    // mailbox deliveries can share the Delegation origin, but do not own it.
    let mut remaining = turn.user_input.as_deref();
    let mut starter_count = 0;
    for message in &messages {
        let Some(rest) = remaining.and_then(|input| input.strip_prefix(message.content.as_str()))
        else {
            break;
        };
        remaining = if rest.is_empty() {
            None
        } else if let Some(next) = rest.strip_prefix("\n\n") {
            Some(next)
        } else {
            break;
        };
        starter_count += 1;
    }
    let a_persons = messages.iter().enumerate().any(|(index, message)| {
        message.origin == MessageOrigin::User
            || (index < starter_count && message.origin == MessageOrigin::Delegation)
    });
    let an_automations = messages.iter().any(|message| {
        matches!(
            message.origin,
            MessageOrigin::Schedule | MessageOrigin::Workflow
        )
    });
    let automated = an_automations && !a_persons;
    Ok(if automated {
        TurnOwner::Automation
    } else {
        TurnOwner::Person
    })
}

/// Whether the parent's own tool call for a delegation — the call the
/// delegate message names — settled with the delegation's own result in
/// the parent's record. A delegate that names no call, a call with no
/// result, and a call recovery itself settled with its placeholder for a
/// cut-off or unavailable result (startup settles every started call
/// before the parents resume) did not: only a result the delegation
/// wrote counts.
async fn delegate_call_settled(
    storage: &dyn Storage,
    parent: &Turn,
    delegate_payload: &serde_json::Value,
) -> Result<bool, StorageError> {
    let Some(call_id) = delegate_payload.get("_call_id").and_then(|v| v.as_str()) else {
        return Ok(false);
    };
    Ok(storage
        .get_tool_call_by_turn_and_call_id(parent.id, call_id)
        .await?
        .is_some_and(|call| {
            call.result.is_some()
                && !matches!(
                    call.status.as_deref(),
                    Some(INTERRUPTED_TOOL_STATUS) | Some(TOOL_RESULT_UNAVAILABLE_STATUS)
                )
        }))
}

/// A child's own Result readdressed to the parent that waits for it: the
/// session and thread the mailbox delivery puts it into, and the turn it
/// answers, so no other session or thread of the same agent takes it.
fn readdressed(payload: &serde_json::Value, session: &Session, parent: &Turn) -> serde_json::Value {
    let mut payload = payload.clone();
    payload["parent_session_id"] = serde_json::json!(session.id);
    payload["parent_thread_id"] = serde_json::json!(parent.thread_id);
    payload["parent_turn_id"] = serde_json::json!(parent.id);
    payload
}

/// Why an incomplete child stopped, as its own record says: the account
/// the runtime left on its thread when the response was cut short, or the
/// turn's failure message, or that it stopped short.
async fn incomplete_account(storage: &dyn Storage, child: &Turn) -> Result<String, StorageError> {
    let account = storage
        .list_messages_by_thread(child.thread_id)
        .await?
        .into_iter()
        .rev()
        .find(|message| {
            message.turn_id == Some(child.id)
                && message.origin == MessageOrigin::Platform
                && message.content.starts_with("Response incomplete:")
        })
        .map(|message| message.content);
    Ok(account
        .or_else(|| child.failure_message.clone())
        .unwrap_or_else(|| "the turn stopped short of completing".to_owned()))
}

/// Resume a parent cut off while waiting on a delegated child: the child's
/// fate goes to the parent agent's mailbox as a pending Result under the
/// delegate that started it — the child's own result when it completed
/// before the restart, its failure when it failed, or a cut-off account
/// with the child's session and thread when it was in flight — naming the
/// parent's session so that session's next turn, and no other session of
/// the same agent, delivers it; and a continuation input is queued on the
/// parent's session, so the startup drain starts the parent's next turn
/// with the Result delivered into its thread first. Enqueueing consumes the
/// parent's `Delegating` state atomically with its inbox hand-over, so a failed
/// transaction leaves it recoverable and a committed one cannot repeat. A child
/// whose delegate message cannot be found is a gap in the record: recorded
/// as such, the notice still naming the child.
async fn resume_delegating_parent(
    storage: &dyn Storage,
    router: &MessageRouter,
    session: &Session,
    parent: &Turn,
    owner: TurnOwner,
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
        // `fate` names the child's ending; `delivered` names what of it the
        // Result carries into the thread, said only by the branch that
        // actually puts one there. The two are separate so no notice claims
        // a delivery the record does not hold.
        let (mut payload, fate, delivered) = match child.state {
            TurnState::Completed => (
                serde_json::json!({
                    "response": child.assistant_response.clone().unwrap_or_default(),
                }),
                "completed before the restart",
                "its result is delivered above",
            ),
            TurnState::Incomplete => (
                serde_json::json!({
                    "response": child.assistant_response.clone().unwrap_or_default(),
                    "incomplete_reason": incomplete_account(storage, &child).await?,
                }),
                "stopped short before the restart",
                "what it produced is delivered above",
            ),
            TurnState::Refused => (
                serde_json::json!({
                    "error": "the delegated agent refused the task",
                    "refused": true,
                    "response": child.assistant_response,
                }),
                "refused the task before the restart",
                "its refusal is delivered above",
            ),
            TurnState::Failed => (
                serde_json::json!({
                    "error": child
                        .failure_message
                        .clone()
                        .unwrap_or_else(|| "the delegated turn failed".to_owned()),
                }),
                "failed before the restart",
                "its failure is delivered above",
            ),
            TurnState::Cancelled => (
                serde_json::json!({ "error": "the delegated turn was cancelled" }),
                "was cancelled before the restart",
                "its cancellation is noted above",
            ),
            TurnState::Interrupted
            | TurnState::Pending
            | TurnState::Processing
            | TurnState::AwaitingApproval
            | TurnState::Delegating => (
                serde_json::json!({
                    "error": "cut off by a gateway restart before completing; its work so far is on its thread",
                }),
                "was cut off by the restart; its work so far is on its own thread, and delegating to it again continues there",
                "the cut-off is noted above",
            ),
        };
        // Structured output rides the payload only when the child produced
        // some: the renderer shows a block for the key's presence, and a
        // null block would read as output of nothing.
        if let Some(structured) = child.structured_output.clone() {
            payload["structured_output"] = structured;
        }
        payload["child_session_id"] = serde_json::json!(child_session_id);
        payload["child_thread_id"] = serde_json::json!(child.thread_id);
        payload["parent_session_id"] = serde_json::json!(session.id);
        payload["parent_thread_id"] = serde_json::json!(parent.thread_id);
        payload["parent_turn_id"] = serde_json::json!(parent.id);
        match storage
            .find_delegate_for_thread(child.thread_id, child.created_at)
            .await?
        {
            Some(delegate) => {
                // A Result the child itself wrote is already the record of
                // its fate: one delivered into the parent's thread before
                // the restart needs nothing more, and one nothing has
                // claimed yet — a detached child's, or an attended one's cut
                // between its write and its claim — will be delivered; a
                // second of either would read twice.
                let own_results: Vec<_> = storage
                    .list_by_correlation(delegate.id)
                    .await?
                    .into_iter()
                    .filter(|message| message.kind == MessageKind::Result)
                    .collect();
                let own_result_failed = own_results
                    .iter()
                    .find(|message| message.status == MessageStatus::Failed);
                let own_result_delivered = own_results
                    .iter()
                    .find(|message| message.status == MessageStatus::Delivered);
                let own_result_waiting = own_results
                    .iter()
                    .find(|message| message.status == MessageStatus::Pending);
                // The notice names the child's fate in words; the record's
                // ids ride the Result's payload and the audit trail, not
                // the model's text.
                if let Some(failed) = own_result_failed {
                    // A hook blocked the child's result on its way: that is
                    // the result's end, and a fresh copy would carry the
                    // blocked output past the hook.
                    notices.push(format!(
                        "Delegated agent {} {fate}; its result was blocked before delivery ({}).",
                        child.agent_id,
                        failed
                            .failure_reason
                            .as_deref()
                            .unwrap_or("no reason recorded")
                    ));
                    continue;
                }
                if let Some(delivered_result) = own_result_delivered {
                    // Delivered to the parent agent's mailbox; reached the
                    // parent's thread only if the parent's own delegate call
                    // settled with it — a crash between the two leaves the
                    // mailbox saying delivered and the thread without it.
                    if delegate_call_settled(storage, parent, &delegate.payload).await? {
                        notices.push(format!(
                            "Delegated agent {} {fate}; its result reached this thread before the restart.",
                            child.agent_id
                        ));
                    } else {
                        notices.push(format!(
                            "Delegated agent {} {fate}; {delivered}.",
                            child.agent_id
                        ));
                        storage
                            .reissue_agent_message(
                                delivered_result.id,
                                readdressed(&delivered_result.payload, session, parent),
                            )
                            .await?;
                    }
                    continue;
                }
                if let Some(waiting) = own_result_waiting {
                    // The child's own Result, written before the restart and
                    // addressed to the agent alone: readdressed to the parent's
                    // session and thread, so no other session or thread of the
                    // agent takes it, and dated the restart, so the startup
                    // expiry leaves it.
                    storage
                        .reissue_agent_message(
                            waiting.id,
                            readdressed(&waiting.payload, session, parent),
                        )
                        .await?;
                } else {
                    // The live send path checks MessageSend before inserting:
                    // a veto leaves no Result for any mailbox reader to take.
                    match router
                        .send_message(NewAgentMessage {
                            sender_agent_id: child.agent_id.clone(),
                            recipient_agent_id: parent.agent_id.clone(),
                            kind: MessageKind::Result,
                            correlation_id: Some(delegate.id),
                            payload,
                        })
                        .await
                    {
                        Ok(_) => {}
                        Err(AgentError::HookBlocked { reason, .. }) => {
                            warn!(
                                parent_turn_id = %parent.id,
                                child_turn_id = %child.id,
                                delegate_message_id = %delegate.id,
                                %reason,
                                "Reconstructed delegation result refused before sending"
                            );
                            notices.push(format!(
                                "Delegated agent {} {fate}; its result was refused before sending.",
                                child.agent_id
                            ));
                            continue;
                        }
                        Err(AgentError::StorageFailed(error)) => return Err(error),
                        Err(error) => return Err(StorageError::QueryFailed(error.to_string())),
                    }
                }
                notices.push(format!(
                    "Delegated agent {} {fate}; {delivered}.",
                    child.agent_id
                ));
            }
            None => {
                notices.push(format!(
                    "Delegated agent {} {fate}; no record of its delegation names it, so this notice is the only account of it.",
                    child.agent_id
                ));
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
    if owner == TurnOwner::Automation {
        // A schedule's run or a workflow's call: their own recovery fails the
        // run or redispatches the call, and a continuation here would run
        // the turn a second way outside their accounting. The children's
        // fates are on the record for the session's next turn.
        warn!(
            parent_turn_id = %parent.id,
            session_id = %session.id,
            "An automation's turn was interrupted while delegating; its own recovery continues it"
        );
        storage
            .update_turn_state(parent.id, TurnState::Interrupted)
            .await?;
        return Ok(());
    }
    let content = if notices.is_empty() {
        "The gateway restarted while this turn was waiting on a delegated agent, and no child turn of it is on the record. Continue from here.".to_owned()
    } else {
        format!(
            "The gateway restarted while this turn was waiting on a delegated agent. {} Continue from here.",
            notices.join(" ")
        )
    };
    // The continuation runs as the original turn did — attended only if
    // a person attended it, delivered to the channel destinations its
    // inputs named — read from what the drain recorded on the turn. A
    // turn created outside the queue (an API call's) recorded nothing and
    // resumes unattended, delivering to no channel, as it ran.
    let (attended, delivery_targets) = match &parent.attribution {
        Some(attribution) => (attribution.attended, attribution.delivery_targets.clone()),
        None => (false, Vec::new()),
    };
    // A chat's turn names its destination through the inbox rows it
    // presented, not through explicit targets: the drain resolves those
    // rows to the originating chat, so the continuation carries them and
    // its reply goes where the original input's would have. The startup
    // requeue leaves a delegating turn's rows presented for this reason.
    let channel_message_ids = storage
        .list_channel_messages_by_turn(parent.id)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect();
    storage
        .enqueue_delegation_continuation(
            parent.id,
            NewQueuedTurnInput {
                delivery_targets,
                client_id: None,
                attended,
                session_id: session.id,
                thread_id: parent.thread_id,
                origin: MessageOrigin::Delegation,
                content,
                attachments: Vec::new(),
                schedule_run_id: None,
                channel_message_ids,
            },
        )
        .await?;
    warn!(
        parent_turn_id = %parent.id,
        session_id = %session.id,
        attended,
        "Queued the resumption of a turn interrupted while delegating"
    );
    Ok(())
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

        let count = recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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

        let count = recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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
            recover_orphaned_turns(
                storage.as_ref(),
                storage.as_ref(),
                &MessageRouter::new(storage.clone(), None)
            )
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

        let count = recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[(MessageOrigin::User, true, Vec::new())],
        )
        .await;

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
            ("critic", TurnState::Refused, Some("I will not write that.")),
            ("builder", TurnState::Failed, None),
            ("stopped", TurnState::Cancelled, None),
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
            if agent == "builder" {
                storage
                    .db
                    .lock()
                    .await
                    .turns
                    .get_mut(&child.id)
                    .unwrap()
                    .failure_message = Some("the build tool crashed".to_string());
            }
            if agent == "surveyor" {
                storage
                    .db
                    .lock()
                    .await
                    .turns
                    .get_mut(&child.id)
                    .unwrap()
                    .structured_output = Some(serde_json::json!({ "findings": 3 }));
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

        let recovered = recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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
            7,
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
                assert_eq!(
                    result.payload["response"], "Scouted.",
                    "the scout's own Result stands; recovery wrote no second"
                );
                assert_eq!(
                    result.payload["parent_session_id"],
                    serde_json::json!(session.id),
                    "and is readdressed to the parent's session"
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
            assert_eq!(
                result.payload["parent_thread_id"],
                serde_json::json!(thread.id),
                "and the parent's thread, so no sibling thread of the session takes it"
            );
            match *agent {
                "surveyor" => {
                    assert_eq!(result.payload["response"], "The survey: three findings.");
                    assert_eq!(
                        result.payload["structured_output"],
                        serde_json::json!({ "findings": 3 })
                    );
                }
                "drafter" => {
                    assert_eq!(result.payload["response"], "Half the draft.");
                    assert!(
                        result.payload.get("structured_output").is_none(),
                        "a child with no structured output carries no key for it: {}",
                        result.payload
                    );
                    assert!(result.payload["incomplete_reason"].is_string());
                }
                "critic" => {
                    assert_eq!(result.payload["refused"], true);
                    assert_eq!(result.payload["response"], "I will not write that.");
                    assert!(result.payload["error"].is_string());
                }
                "stopped" => {
                    assert_eq!(result.payload["error"], "the delegated turn was cancelled");
                }
                "builder" => {
                    assert_eq!(result.payload["error"], "the build tool crashed");
                    assert!(result.payload.get("refused").is_none());
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
        assert!(queued[0].content.contains("critic refused the task"));
        assert!(queued[0].content.contains("builder failed"));
        assert!(queued[0].content.contains("drafter"));
        assert!(queued[0].content.contains("stopped short"));
        assert!(queued[0].content.contains("reviewer"));
        assert!(
            !queued[0].content.contains(&session.id.to_string())
                && !queued[0].content.contains(&thread.id.to_string()),
            "the notice names fates, not ids: {}",
            queued[0].content
        );
    }

    /// A parent turn started from the queue — as a person's or a channel's
    /// turn is — from the inputs given as (origin, attended, delivery
    /// targets), and left `Delegating` as a restart would find it.
    async fn drained_delegating_parent(
        storage: &MockStorage,
        session_id: Uuid,
        thread_id: Uuid,
        inputs: &[(MessageOrigin, bool, Vec<serde_json::Value>)],
    ) -> Turn {
        use swage_storage::store::TurnQueueStore;
        for (origin, attended, delivery_targets) in inputs {
            storage
                .enqueue_turn_input(NewQueuedTurnInput {
                    delivery_targets: delivery_targets.clone(),
                    client_id: None,
                    attended: *attended,
                    session_id,
                    thread_id,
                    origin: *origin,
                    content: format!("an input of origin {origin:?}"),
                    attachments: Vec::new(),
                    schedule_run_id: None,
                    channel_message_ids: Vec::new(),
                })
                .await
                .unwrap();
        }
        let drained = storage
            .drain_queued_turn_inputs(session_id, "lead")
            .await
            .unwrap()
            .expect("the queued inputs drain into a turn");
        storage
            .update_turn_state(drained.turn.id, TurnState::Delegating)
            .await
            .unwrap();
        storage.get_turn(drained.turn.id).await.unwrap().unwrap()
    }

    /// A child of `parent` on its own session: its delegate message, then
    /// its turn in `state`, as the executor writes them.
    async fn delegated_child(
        storage: &MockStorage,
        topic_id: Uuid,
        parent: &Turn,
        agent: &str,
        state: TurnState,
    ) -> (Turn, swage_storage::model::AgentMessage) {
        use swage_storage::model::NewAgentMessage;
        use swage_storage::store::AgentMessageStore;
        let child_session = storage
            .create_session(NewSession {
                agent_id: agent.to_string(),
                ..swage_storage::fixtures::new_session(topic_id)
            })
            .await
            .unwrap();
        let child_thread = ThreadStore::create(
            storage,
            NewThread {
                session_id: child_session.id,
                agent_id: agent.to_string(),
            },
        )
        .await
        .unwrap();
        let delegate = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: parent.agent_id.clone(),
                recipient_agent_id: agent.to_string(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({
                    "task": "work",
                    "_thread_id": child_thread.id.to_string(),
                    "_call_id": format!("call-{agent}"),
                }),
            })
            .await
            .unwrap();
        let child = storage
            .create_turn(NewTurn {
                thread_id: child_thread.id,
                agent_id: agent.to_string(),
                state,
                user_input: Some("work".to_string()),
                parent_turn_id: Some(parent.id),
            })
            .await
            .unwrap();
        (child, delegate)
    }

    /// The parent's own record of its delegate call, settled with a result
    /// — what the synchronous path writes once the child's result is in
    /// the parent's thread.
    async fn settle_delegate_call(storage: &MockStorage, parent: &Turn, agent: &str) {
        use swage_storage::model::NewToolCall;
        use swage_storage::store::ToolCallStore;
        storage
            .create_tool_call(NewToolCall {
                turn_id: parent.id,
                agent_id: parent.agent_id.clone(),
                call_id: format!("call-{agent}"),
                tool: "delegate".to_string(),
                arguments: "{}".to_string(),
                model_arguments: "{}".to_string(),
                execution: Some(swage_protocol::client::conversation::ToolExecution::succeeded()),
                result: Some("the child's result".to_string()),
                model_result: Some("the child's result".to_string()),
                status: Some("ok".to_string()),
                duration_ms: Some(1),
                truncated: false,
                total_bytes: None,
                provider_round: 1,
                position: 0,
                parent_call_id: None,
                display_summary: None,
            })
            .await
            .unwrap();
    }

    /// A channel's turn resumes where it was going: the continuation
    /// carries the delivery targets the original input named and runs
    /// unattended, as the channel's own turns do.
    #[tokio::test]
    async fn recovery_continues_a_channel_turn_to_its_own_destinations() {
        use swage_storage::store::{ChannelQueueStore, TurnQueueStore};

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
        // The chat's message, presented by the parent's turn: the turn's
        // destination is the chat the row came from, resolved by the drain
        // from the row, so the row is what the continuation has to carry.
        storage
            .ingest_channel_inbound(
                vec![swage_storage::NewChannelInboundMessage {
                    integration: "telegram".into(),
                    account: "ops".into(),
                    conversation: "chat-7".into(),
                    external_id: "update-1".into(),
                    sender: "ben".into(),
                    content: "Delegate the survey.".into(),
                }],
                "telegram",
                "ops",
                "update-1",
            )
            .await
            .unwrap();
        let row = storage
            .list_pending_channel_messages()
            .await
            .unwrap()
            .remove(0);
        // A second input in the same batch with an explicit target, as a
        // schedule's delivery names one.
        let target = serde_json::json!({
            "integration": "telegram",
            "account": "ops",
            "conversation": "chat-9",
        });
        for (channel_message_ids, delivery_targets) in [
            (vec![row.id], Vec::new()),
            (Vec::new(), vec![target.clone()]),
        ] {
            storage
                .enqueue_turn_input(NewQueuedTurnInput {
                    delivery_targets,
                    client_id: None,
                    attended: false,
                    session_id: session.id,
                    thread_id: thread.id,
                    origin: MessageOrigin::User,
                    content: "an input".to_string(),
                    attachments: Vec::new(),
                    schedule_run_id: None,
                    channel_message_ids,
                })
                .await
                .unwrap();
        }
        let drained = storage
            .drain_queued_turn_inputs(session.id, "lead")
            .await
            .unwrap()
            .expect("the queued inputs drain into a turn");
        assert_eq!(
            drained.channel_origins.len(),
            1,
            "the drain resolved the chat"
        );
        storage
            .update_turn_state(drained.turn.id, TurnState::Delegating)
            .await
            .unwrap();
        let parent = storage.get_turn(drained.turn.id).await.unwrap().unwrap();
        delegated_child(&storage, topic.id, &parent, "worker", TurnState::Processing).await;

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert!(!queued[0].attended, "a channel's turn runs unattended");
        assert_eq!(
            queued[0].delivery_targets,
            vec![target],
            "the continuation delivers where the original input's explicit targets did"
        );
        assert_eq!(
            queued[0].channel_message_ids,
            vec![row.id],
            "and carries the chat's row, so the drain resolves the chat as it did for the original"
        );
        let presented = storage
            .list_channel_messages_by_turn(parent.id)
            .await
            .unwrap();
        assert_eq!(
            presented.len(),
            0,
            "the row has been handed to the queued continuation"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sqlite_recovery_resumes_a_chat_parent_with_its_inbox_rows() {
        use swage_storage::store::{AgentMessageStore, ChannelQueueStore, TurnQueueStore};

        let (storage, thread_id) = make_sqlite_with_thread().await;
        let thread = ThreadStore::get(&*storage, thread_id)
            .await
            .unwrap()
            .unwrap();
        let session_id = thread.session_id;
        storage
            .ingest_channel_inbound(
                ["parent-input", "unrelated"]
                    .into_iter()
                    .map(|external_id| swage_storage::NewChannelInboundMessage {
                        integration: "telegram".into(),
                        account: "ops".into(),
                        conversation: "chat-7".into(),
                        external_id: external_id.into(),
                        sender: "ben".into(),
                        content: "Delegate the survey.".into(),
                    })
                    .collect(),
                "telegram",
                "ops",
                "1",
            )
            .await
            .unwrap();
        let rows = storage.list_pending_channel_messages().await.unwrap();
        let input = NewQueuedTurnInput {
            delivery_targets: Vec::new(),
            client_id: None,
            attended: false,
            session_id,
            thread_id,
            origin: MessageOrigin::User,
            content: "Delegate the survey.".into(),
            attachments: Vec::new(),
            schedule_run_id: None,
            channel_message_ids: vec![rows[0].id],
        };
        storage.enqueue_turn_input(input).await.unwrap();
        let original = storage
            .drain_queued_turn_inputs(session_id, "gateway")
            .await
            .unwrap()
            .unwrap();
        storage
            .update_turn_state(original.turn.id, TurnState::Delegating)
            .await
            .unwrap();
        let child_thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "worker".into(),
            },
        )
        .await
        .unwrap();
        storage
            .create_turn(NewTurn {
                thread_id: child_thread.id,
                agent_id: "worker".into(),
                state: TurnState::Processing,
                user_input: Some("survey".into()),
                parent_turn_id: Some(original.turn.id),
            })
            .await
            .unwrap();
        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "gateway".into(),
                recipient_agent_id: "worker".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({"_thread_id": child_thread.id}),
            })
            .await
            .unwrap();
        assert_eq!(
            storage
                .list_channel_messages_by_turn(original.turn.id)
                .await
                .unwrap()
                .len(),
            1
        );

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("SQLite recovery must enqueue the chat continuation");

        let queued = storage.list_queued_turn_inputs(session_id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].origin, MessageOrigin::Delegation);
        assert_eq!(queued[0].thread_id, thread_id);
        assert_eq!(queued[0].channel_message_ids, vec![rows[0].id]);
        assert_eq!(
            serde_json::to_value(storage.list_pending_channel_messages().await.unwrap()).unwrap(),
            serde_json::json!([rows[1]])
        );
        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .unwrap();
        assert_eq!(
            storage.list_queued_turn_inputs(session_id).await.unwrap(),
            queued
        );
        let continued = storage
            .drain_queued_turn_inputs(session_id, "gateway")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            continued.channel_origins, original.channel_origins,
            "the continuation's reply must reach the originating chat"
        );
        let linked = storage
            .list_channel_messages_by_turn(continued.turn.id)
            .await
            .unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].id, rows[0].id);
        assert_eq!(
            serde_json::to_value(storage.list_pending_channel_messages().await.unwrap()).unwrap(),
            serde_json::json!([rows[1]])
        );
    }

    /// A person's message batched with a scheduled brief is the person's
    /// turn: it resumes, rather than being left to a schedule's recovery
    /// that would never answer the person.
    #[tokio::test]
    async fn recovery_resumes_a_persons_turn_batched_with_a_scheduled_brief() {
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
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[
                (MessageOrigin::Schedule, false, Vec::new()),
                (MessageOrigin::User, true, Vec::new()),
            ],
        )
        .await;
        delegated_child(&storage, topic.id, &parent, "worker", TurnState::Processing).await;

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert_eq!(queued.len(), 1, "the person's turn resumes");
        assert!(
            queued[0].attended,
            "and attended, as the person's input was"
        );
    }

    #[tokio::test]
    async fn recovery_resumes_a_continuation_batched_with_a_scheduled_brief() {
        use swage_storage::store::{MessageStore, TurnQueueStore};
        for origin in [
            MessageOrigin::User,
            MessageOrigin::Delegation,
            MessageOrigin::Schedule,
            MessageOrigin::Workflow,
        ] {
            for continuation_first in [false, true] {
                for attended in [false, true] {
                    let storage = Arc::new(MockStorage::new());
                    let topic = storage.general_scope().await.unwrap();
                    let session = storage
                        .create_session(NewSession {
                            agent_id: "lead".into(),
                            ..swage_storage::fixtures::new_session(topic.id)
                        })
                        .await
                        .unwrap();
                    let thread = ThreadStore::create(
                        &*storage,
                        NewThread {
                            session_id: session.id,
                            agent_id: "lead".into(),
                        },
                    )
                    .await
                    .unwrap();
                    let mut inputs = vec![
                        (MessageOrigin::Schedule, false, Vec::new()),
                        (origin, attended, Vec::new()),
                    ];
                    if continuation_first {
                        inputs.reverse();
                    }
                    let parent =
                        drained_delegating_parent(&storage, session.id, thread.id, &inputs).await;
                    // Mailbox output appended after the starter inputs cannot turn
                    // a schedule's or workflow's turn into a person's turn.
                    storage
                        .append_message(swage_storage::model::AppendMessage {
                            thread_id: thread.id,
                            turn_id: Some(parent.id),
                            role: Role::User,
                            origin: MessageOrigin::Delegation,
                            content: "A detached child's result.".into(),
                            attachments: Vec::new(),
                            agent_id: "lead".into(),
                        })
                        .await
                        .unwrap();
                    delegated_child(&storage, topic.id, &parent, "worker", TurnState::Processing)
                        .await;
                    recover_orphaned_turns(
                        &*storage,
                        &*storage,
                        &MessageRouter::new(storage.clone(), None),
                    )
                    .await
                    .unwrap();
                    let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
                    let persons = matches!(origin, MessageOrigin::User | MessageOrigin::Delegation);
                    assert_eq!(
                        queued.len(),
                        usize::from(persons),
                        "a person's continuation must resume beside a scheduled brief: {origin:?}, continuation_first={continuation_first}, attended={attended}"
                    );
                    if persons {
                        assert_eq!(queued[0].attended, attended);
                    }
                }
            }
        }
    }

    /// A child whose own result already reached the parent's thread before
    /// the restart gets no second result: the notice says it was delivered,
    /// and the record keeps the one delivered message.
    #[tokio::test]
    async fn recovery_writes_no_second_result_for_a_child_already_delivered() {
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
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[(MessageOrigin::User, true, Vec::new())],
        )
        .await;
        let (_, delegate) =
            delegated_child(&storage, topic.id, &parent, "scout", TurnState::Completed).await;
        settle_delegate_call(&storage, &parent, "scout").await;
        let delivered = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "scout".to_string(),
                recipient_agent_id: "lead".to_string(),
                kind: MessageKind::Result,
                correlation_id: Some(delegate.id),
                payload: serde_json::json!({ "response": "Scouted." }),
            })
            .await
            .unwrap();
        storage
            .update_agent_message_status(delivered.id, MessageStatus::Delivered, None)
            .await
            .unwrap();

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        let results: Vec<_> = storage
            .list_by_correlation(delegate.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|message| message.kind == MessageKind::Result)
            .collect();
        assert_eq!(results.len(), 1, "no second result: {results:?}");
        assert_eq!(results[0].status, MessageStatus::Delivered);
        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert!(
            queued[0]
                .content
                .contains("its result reached this thread before the restart"),
            "{}",
            queued[0].content
        );
    }

    /// A result the mailbox marked delivered that the parent's own record
    /// never settled — the crash fell between the two — has not reached
    /// the parent's thread: it is reissued to the parent's session, not
    /// skipped.
    #[tokio::test]
    async fn recovery_reissues_a_delivered_result_the_parents_call_never_settled() {
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
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[(MessageOrigin::User, true, Vec::new())],
        )
        .await;
        let (_, delegate) =
            delegated_child(&storage, topic.id, &parent, "scout", TurnState::Completed).await;
        // The parent's delegate call was still started when the gateway
        // stopped: startup settles it with its cut-off placeholder before
        // the parent resumes, and that placeholder is not the delegation's
        // result.
        {
            use swage_storage::model::NewToolCall;
            use swage_storage::store::ToolCallStore;
            storage
                .create_tool_call(NewToolCall {
                    turn_id: parent.id,
                    agent_id: parent.agent_id.clone(),
                    execution: None,
                    call_id: "call-scout".to_string(),
                    tool: "delegate".to_string(),
                    arguments: "{}".to_string(),
                    model_arguments: "{}".to_string(),
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
        }
        let delivered = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "scout".to_string(),
                recipient_agent_id: "lead".to_string(),
                kind: MessageKind::Result,
                correlation_id: Some(delegate.id),
                payload: serde_json::json!({ "response": "Scouted." }),
            })
            .await
            .unwrap();
        storage
            .update_agent_message_status(delivered.id, MessageStatus::Delivered, None)
            .await
            .unwrap();

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        let results: Vec<_> = storage
            .list_by_correlation(delegate.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|message| message.kind == MessageKind::Result)
            .collect();
        assert_eq!(
            results.len(),
            1,
            "the one result is reissued, not copied: {results:?}"
        );
        assert_eq!(results[0].status, MessageStatus::Pending);
        {
            use swage_storage::store::ToolCallStore;
            let call = storage
                .get_tool_call_by_turn_and_call_id(parent.id, "call-scout")
                .await
                .unwrap()
                .expect("the delegate call is on the record");
            assert_eq!(
                call.status.as_deref(),
                Some(INTERRUPTED_TOOL_STATUS),
                "startup settled the started call with its placeholder, which is not the delegation's result"
            );
        }
        assert_eq!(results[0].payload["response"], "Scouted.");
        assert_eq!(
            results[0].payload["parent_session_id"],
            serde_json::json!(session.id)
        );
        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert!(
            queued[0].content.contains("its result is delivered above"),
            "{}",
            queued[0].content
        );
    }

    /// A result a hook blocked on its way is the child's end: recovery
    /// writes no fresh copy that would carry the blocked output past the
    /// hook, and the notice says it was blocked.
    #[tokio::test]
    async fn recovery_writes_no_fresh_copy_of_a_result_a_hook_blocked() {
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
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[(MessageOrigin::User, true, Vec::new())],
        )
        .await;
        let (_, delegate) =
            delegated_child(&storage, topic.id, &parent, "scout", TurnState::Completed).await;
        let blocked = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "scout".to_string(),
                recipient_agent_id: "lead".to_string(),
                kind: MessageKind::Result,
                correlation_id: Some(delegate.id),
                payload: serde_json::json!({ "response": "the blocked text" }),
            })
            .await
            .unwrap();
        storage
            .update_agent_message_status(
                blocked.id,
                MessageStatus::Failed,
                Some("blocked by the receive hook"),
            )
            .await
            .unwrap();

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        let results: Vec<_> = storage
            .list_by_correlation(delegate.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|message| message.kind == MessageKind::Result)
            .collect();
        assert_eq!(results.len(), 1, "{results:?}");
        assert_eq!(results[0].status, MessageStatus::Failed);
        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert!(
            queued[0]
                .content
                .contains("blocked before delivery (blocked by the receive hook)"),
            "{}",
            queued[0].content
        );
    }

    /// A schedule's turn stays the schedule's when a detached child's
    /// result was delivered into it before the restart: the platform's
    /// delegation row says nothing about who started the turn.
    #[tokio::test]
    async fn a_delivered_detached_result_does_not_make_a_schedules_turn_a_persons() {
        use swage_core::Role;
        use swage_storage::model::AppendMessage;
        use swage_storage::store::{MessageStore, TurnQueueStore};

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
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[(MessageOrigin::Schedule, false, Vec::new())],
        )
        .await;
        storage
            .append_message(AppendMessage {
                thread_id: thread.id,
                turn_id: Some(parent.id),
                role: Role::User,
                origin: MessageOrigin::Delegation,
                content: "Delegated agent scout completed:\nearlier".to_string(),
                attachments: Vec::new(),
                agent_id: "lead".to_string(),
            })
            .await
            .unwrap();
        delegated_child(&storage, topic.id, &parent, "worker", TurnState::Processing).await;

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        assert!(
            storage
                .list_queued_turn_inputs(session.id)
                .await
                .unwrap()
                .is_empty(),
            "the schedule's own recovery continues its run"
        );
    }

    /// An incomplete child's reason is the account its runtime left on its
    /// thread, not a generic stand-in.
    #[tokio::test]
    async fn recovery_carries_an_incomplete_childs_recorded_reason() {
        use swage_core::Role;
        use swage_storage::model::AppendMessage;
        use swage_storage::store::{AgentMessageStore, MessageStore};

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
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[(MessageOrigin::User, true, Vec::new())],
        )
        .await;
        let (child, delegate) = delegated_child(
            &storage,
            topic.id,
            &parent,
            "drafter",
            TurnState::Incomplete,
        )
        .await;
        storage
            .append_message(AppendMessage {
                thread_id: child.thread_id,
                turn_id: Some(child.id),
                role: Role::User,
                origin: MessageOrigin::Platform,
                content: "Response incomplete: output token limit reached (max_tokens)."
                    .to_string(),
                attachments: Vec::new(),
                agent_id: "drafter".to_string(),
            })
            .await
            .unwrap();

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        let result = storage
            .list_by_correlation(delegate.id)
            .await
            .unwrap()
            .into_iter()
            .find(|message| message.kind == MessageKind::Result)
            .expect("the child's fate is on the record");
        assert_eq!(
            result.payload["incomplete_reason"],
            "Response incomplete: output token limit reached (max_tokens)."
        );
    }

    /// A turn created outside the queue — an API call's — recorded no
    /// attendance, and resumes as it ran: unattended, delivered nowhere.
    #[tokio::test]
    async fn recovery_resumes_a_turn_created_outside_the_queue_unattended() {
        use swage_core::Role;
        use swage_storage::model::AppendMessage;
        use swage_storage::store::{MessageStore, TurnQueueStore};

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
                user_input: Some("an API call's input".to_string()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        storage
            .append_message(AppendMessage {
                thread_id: thread.id,
                turn_id: Some(parent.id),
                role: Role::User,
                origin: MessageOrigin::User,
                content: "an API call's input".to_string(),
                attachments: Vec::new(),
                agent_id: "lead".to_string(),
            })
            .await
            .unwrap();
        delegated_child(&storage, topic.id, &parent, "worker", TurnState::Processing).await;

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert!(!queued[0].attended);
        assert!(queued[0].delivery_targets.is_empty());
    }

    /// An automation's turn — a schedule's run, a workflow's call — that was
    /// delegating is left to its own recovery: its children's fates reach
    /// the record, and no continuation is queued beside the run's or the
    /// call's own.
    #[tokio::test]
    async fn recovery_queues_no_continuation_for_an_automations_delegating_turn() {
        use swage_core::{MessageKind, Role};
        use swage_storage::model::{AppendMessage, NewAgentMessage};
        use swage_storage::store::{AgentMessageStore, MessageStore, TurnQueueStore};

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
                user_input: Some("the schedule's brief".to_string()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        storage
            .append_message(AppendMessage {
                thread_id: thread.id,
                turn_id: Some(parent.id),
                role: Role::User,
                origin: swage_storage::MessageOrigin::Schedule,
                content: "the schedule's brief".to_string(),
                attachments: Vec::new(),
                agent_id: "lead".to_string(),
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
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "lead".to_string(),
                recipient_agent_id: "worker".to_string(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({ "task": "work", "_thread_id": child_thread.id.to_string() }),
            })
            .await
            .unwrap();
        storage
            .create_turn(NewTurn {
                thread_id: child_thread.id,
                agent_id: "worker".to_string(),
                state: TurnState::Processing,
                user_input: Some("work".to_string()),
                parent_turn_id: Some(parent.id),
            })
            .await
            .unwrap();

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
        .await
        .expect("recovery should not fail");

        assert!(
            storage
                .list_queued_turn_inputs(session.id)
                .await
                .unwrap()
                .is_empty(),
            "the schedule's own recovery continues its run"
        );
        assert_eq!(
            storage
                .list_pending_for_agent("lead")
                .await
                .unwrap()
                .iter()
                .filter(|m| m.kind == MessageKind::Result)
                .count(),
            1,
            "the child's fate is still on the record"
        );
        assert_eq!(
            storage.get_turn(parent.id).await.unwrap().unwrap().state,
            TurnState::Interrupted
        );
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

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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

        recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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
        assert!(
            queued[0].content.contains(
                "no record of its delegation names it, so this notice is the only account of it"
            ),
            "the notice claims no delivery where nothing was written: {}",
            queued[0].content
        );
        assert!(
            !queued[0].content.contains("delivered above"),
            "{}",
            queued[0].content
        );
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

        let count = recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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

        let count = recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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

        let count = recover_orphaned_turns(
            &*storage,
            &*storage,
            &MessageRouter::new(storage.clone(), None),
        )
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
    #[tokio::test]
    async fn recovery_does_not_recreate_a_result_refused_by_the_send_hook() {
        assert_refused_result_not_delivered(false).await;
    }

    #[tokio::test]
    async fn refusal_notice_does_not_echo_rejected_payload() {
        assert_refused_result_not_delivered(true).await;
    }

    async fn assert_refused_result_not_delivered(quote_payload: bool) {
        use swage_agent::{AgentError, MessageRouter};
        use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
        use swage_storage::store::{AgentMessageStore, MessageStore, TurnQueueStore};

        struct RefuseResult(Arc<std::sync::atomic::AtomicUsize>, bool);
        #[async_trait::async_trait]
        impl Hook for RefuseResult {
            fn name(&self) -> &str {
                "refuse-result"
            }
            async fn execute(&self, event: &HookEvent) -> HookOutcome {
                match event {
                    HookEvent::MessageSend { kind, payload, .. } if kind == "result" => {
                        self.0.fetch_add(1, Ordering::SeqCst);
                        HookOutcome::Block(if self.1 {
                            format!("result send refused: {}", payload["response"])
                        } else {
                            "result send refused".into()
                        })
                    }
                    _ => HookOutcome::Continue,
                }
            }
        }
        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id: session.id,
                agent_id: "lead".into(),
            },
        )
        .await
        .unwrap();
        let parent = drained_delegating_parent(
            &storage,
            session.id,
            thread.id,
            &[(MessageOrigin::User, true, Vec::new())],
        )
        .await;
        let (child, delegate) =
            delegated_child(&storage, topic.id, &parent, "worker", TurnState::Completed).await;
        storage
            .update_response(child.id, "refused output")
            .await
            .unwrap();
        let send_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut hooks = HookRegistry::new();
        hooks.register(HookRegistration {
            hook: Arc::new(RefuseResult(send_calls.clone(), quote_payload)),
            event_filter: None,
            matcher: None,
            priority: 0,
            failure_mode: swage_hook::FailureMode::FailOpen,
            timeout: Duration::from_secs(5),
        });
        let router = MessageRouter::new(storage.clone(), Some(Arc::new(hooks)));
        let refused = router
            .send_message(NewAgentMessage {
                sender_agent_id: "worker".into(),
                recipient_agent_id: "lead".into(),
                kind: MessageKind::Result,
                correlation_id: Some(delegate.id),
                payload: serde_json::json!({"response": "refused output"}),
            })
            .await;
        assert!(
            matches!(refused, Err(AgentError::HookBlocked { .. })),
            "live send must be refused"
        );
        assert!(
            storage
                .list_by_correlation(delegate.id)
                .await
                .unwrap()
                .is_empty()
        );
        use tracing::instrument::WithSubscriber;
        struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for LogCapture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || LogCapture(capture.clone()))
            .finish();
        recover_orphaned_turns(&*storage, &*storage, &router)
            .with_subscriber(subscriber)
            .await
            .unwrap();
        let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
        assert!(log.contains("Reconstructed delegation result refused before sending"));
        assert!(log.contains("result send refused"));
        if quote_payload {
            assert!(log.contains("result send refused: \"refused output\""));
        }
        assert!(log.contains(&parent.id.to_string()));
        assert!(log.contains(&child.id.to_string()));
        assert!(log.contains(&delegate.id.to_string()));
        let results = storage.list_by_correlation(delegate.id).await.unwrap();
        // Delivery-only hooks do not block: exactly the bypass detached delivery uses.
        for result in &results {
            router.check_delivery(result).await.unwrap();
        }
        assert!(
            results.is_empty(),
            "recovery must not recreate a Result refused by MessageSend; got {} deliverable Result(s)",
            results.len()
        );
        assert_eq!(
            send_calls.load(Ordering::SeqCst),
            2,
            "live send and recovery use the same hook"
        );
        assert!(
            storage
                .list_pending_for_agent("lead")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            storage.get_turn(parent.id).await.unwrap().unwrap().state,
            TurnState::Interrupted
        );
        let queued = storage.list_queued_turn_inputs(session.id).await.unwrap();
        assert_eq!(queued.len(), 1, "a veto must still resume the parent");
        assert!(
            queued[0]
                .content
                .contains("its result was refused before sending")
        );
        assert!(!queued[0].content.contains("delivered above"));
        assert!(!queued[0].content.contains("refused output"));
        assert!(!queued[0].content.contains("result send refused"));
        assert!(queued[0].content.contains("Delegated agent worker completed before the restart; its result was refused before sending."));
        let resumed = storage
            .drain_queued_turn_inputs(session.id, "lead")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            resumed.turn.user_input.as_deref(),
            Some(queued[0].content.as_str())
        );
        assert!(
            storage
                .list_messages_by_thread(thread.id)
                .await
                .unwrap()
                .iter()
                .all(|message| !message.content.contains("refused output"))
        );
        recover_orphaned_turns(&*storage, &*storage, &router)
            .await
            .unwrap();
        assert_eq!(
            send_calls.load(Ordering::SeqCst),
            2,
            "a resumed parent must not recreate the refused result on another restart"
        );
        assert!(
            storage
                .list_by_correlation(delegate.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            storage
                .list_queued_turn_inputs(session.id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
