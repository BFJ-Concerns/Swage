use std::time::Duration;

use tokio::time;
use tracing::{info, warn};

use swage_core::StorageError;
use swage_storage::model::TurnState;
use swage_storage::store::Storage;

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
