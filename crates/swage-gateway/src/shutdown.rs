//! Gateway shutdown and recovery.
//!
//! Four functions manage the process lifecycle:
//!
//! - [`idle_monitor`] — polls [`crate::lifecycle::ActivityTracker`] and resolves
//!   when the gateway has been idle for `idle_timeout_secs`. Disabled (never
//!   resolves) when the timeout is zero. Defers shutdown if active turns are
//!   in flight, waiting up to one further `idle_timeout_secs` grace period
//!   before proceeding anyway.
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
//!   left in `Processing` state by a previous unclean exit to `Interrupted`.
//!
//! Typical call sequence at shutdown:
//! ```text
//! tokio::select! {
//!     _ = graceful_shutdown() => {}
//!     _ = idle_monitor(state, timeout) => {}
//! }
//! shutdown_active_turns(&state).await;
//! ```

use std::time::Duration;

use tokio::time;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use swage_core::StorageError;
use swage_storage::model::TurnState;
use swage_storage::store::Storage;

use crate::state::AppState;

/// Monitor for idle-triggered gateway shutdown.
///
/// Resolves when the gateway should shut down due to inactivity, allowing the
/// caller to race it against signal-based shutdown with `tokio::select!`.
///
/// # Behaviour
///
/// - **`idle_timeout_secs == 0`**: idle shutdown is disabled — this future
///   never resolves, leaving shutdown entirely to signals.
/// - **Idle detected, no active turns**: resolves immediately.
/// - **Idle detected, active turns present**: defers and logs. A grace period
///   equal to `idle_timeout_secs` begins; if turns are still active after the
///   grace period the monitor resolves anyway (the outer shutdown sequence
///   calls [`shutdown_active_turns`] to cancel them).
///
/// The caller is responsible for calling [`shutdown_active_turns`] once this
/// future resolves.
pub async fn idle_monitor(state: AppState, idle_timeout_secs: u64) {
    if idle_timeout_secs == 0 {
        // Idle shutdown disabled — yield indefinitely; only signals can stop us.
        std::future::pending::<()>().await;
        return;
    }

    let grace_period = Duration::from_secs(idle_timeout_secs);

    // Check every second so short test timeouts (1s) work without a long wait.
    let mut interval = time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // When Some, records the instant we first noticed the idle threshold was exceeded.
    let mut idle_grace_started: Option<std::time::Instant> = None;

    loop {
        interval.tick().await;

        let idle_secs = state.activity_tracker.seconds_since_last_activity();
        if idle_secs < idle_timeout_secs {
            // Still within the active window — clear any grace-period tracking.
            idle_grace_started = None;
            continue;
        }

        // Idle threshold exceeded — check for in-flight turns.
        let active_count = state.active_turns.lock().await.len();
        if active_count == 0 {
            // Clean idle shutdown.
            info!(
                idle_secs,
                idle_timeout_secs,
                "Idle for {idle_secs}s (threshold {idle_timeout_secs}s) — initiating shutdown"
            );
            return;
        }

        // Turns are still running. Start (or check) the grace period.
        let grace_elapsed = {
            let start = *idle_grace_started.get_or_insert_with(std::time::Instant::now);
            start.elapsed()
        };

        if grace_elapsed >= grace_period {
            warn!(
                active_count,
                grace_secs = grace_elapsed.as_secs(),
                "Grace period expired with {active_count} active turn(s) — \
                 proceeding with shutdown"
            );
            return; // Outer shutdown sequence cancels turns via shutdown_active_turns.
        }

        let remaining = grace_period.saturating_sub(grace_elapsed);
        info!(
            active_count,
            grace_remaining_secs = remaining.as_secs(),
            "Idle timeout reached but {active_count} turn(s) active, deferring \
             ({}/{} grace period elapsed)",
            grace_elapsed.as_secs(),
            grace_period.as_secs(),
        );
    }
}

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

/// Transition all Processing turns for the gateway agent to Interrupted.
///
/// Runs at startup — before the server begins accepting connections — to clean
/// up turns left dangling by a previous unclean shutdown.
///
/// Returns the number of turns recovered.
pub async fn recover_orphaned_turns(storage: &dyn Storage) -> Result<usize, StorageError> {
    // "gateway" is the agent_id used by the embedded single-agent mode.
    let orphans = storage.find_in_progress_by_agent("gateway").await?;
    let count = orphans.len();

    for turn in &orphans {
        warn!(
            "Recovered orphaned turn {} for thread {}",
            turn.id, turn.thread_id,
        );
        storage.update_turn_state(turn.id, TurnState::Interrupted).await?;
    }

    Ok(count)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use swage_storage::model::{NewSession, NewThread, NewTurn, TurnState};
    use swage_storage::store::{ProjectStore, SessionStore, ThreadStore, TurnStore};

    use super::*;
    use crate::turn::tests::MockStorage;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a MockStorage pre-populated with `processing` Processing turns
    /// and `completed` Completed turns (all belonging to a single thread).
    async fn make_storage(processing: usize, completed: usize) -> Arc<MockStorage> {
        let storage = Arc::new(MockStorage::new());

        // Create the project → session → thread hierarchy required for FK integrity.
        let project = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
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
        let storage = make_storage(2, 1).await;

        let count = recover_orphaned_turns(&*storage)
            .await
            .expect("recovery should not fail");

        assert_eq!(count, 2, "should report 2 recovered turns");

        let db = storage.db.lock().await;
        let n_interrupted = db.turns.values().filter(|t| t.state == TurnState::Interrupted).count();
        let n_completed = db.turns.values().filter(|t| t.state == TurnState::Completed).count();
        let n_processing = db.turns.values().filter(|t| t.state == TurnState::Processing).count();

        assert_eq!(n_interrupted, 2, "2 orphaned turns should be Interrupted");
        assert_eq!(n_completed, 1, "Completed turn should be unchanged");
        assert_eq!(n_processing, 0, "no turns should remain Processing");
    }

    #[tokio::test]
    async fn recovery_with_no_orphans_returns_zero() {
        let storage = Arc::new(MockStorage::new());

        let count = recover_orphaned_turns(&*storage)
            .await
            .expect("recovery should not fail on empty storage");

        assert_eq!(count, 0);
    }
}
