//! The inactivity archive: a periodic sweep that hides sessions nobody has
//! spoken in for the configured number of days.
//!
//! Archiving is visibility only. The session's threads, surfaces and queued
//! continuations are untouched, a scheduled or parked continuation still
//! fires, and the next turn on the session returns it to the recent list
//! (`turn.rs` clears `archived_at` as the turn starts). The only sessions the
//! sweep leaves alone are those with a turn executing right now: a parked
//! workflow, an unanswered hold or a future timer is not execution and does
//! not keep a session visible.

use chrono::{DateTime, Duration, Utc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::NewAuditEvent;

use crate::state::AppState;

/// How often the sweep runs. The first pass runs as the gateway starts, so a
/// restart is also a sweep.
pub const ARCHIVE_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Archive every session inactive for longer than the configured delay,
/// except those with a turn executing. Returns the ids archived.
pub async fn archive_inactive_sessions(
    state: &AppState,
    now: DateTime<Utc>,
) -> Result<Vec<Uuid>, GatewayError> {
    let days = state.active_config().sessions.archive_after_days;
    // A delay past the bound archives nothing rather than panicking the
    // maintenance loop; validation keeps configured values well inside it.
    let Some(delay) = i64::try_from(days).ok().and_then(Duration::try_days) else {
        tracing::warn!(
            days,
            "session archive delay is out of range; nothing archived"
        );
        return Ok(Vec::new());
    };
    let inactive_before = now - delay;
    let executing: Vec<Uuid> = state.active_turns.lock().await.keys().copied().collect();
    let archived = state
        .storage
        .archive_inactive_sessions(inactive_before, &executing)
        .await?;
    for session_id in &archived {
        state
            .storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id: Some(*session_id),
                entity_kind: "session".into(),
                entity_id: session_id.to_string(),
                event_kind: "archived".into(),
                summary: format!("session archived after {days} days without a message"),
                metadata: serde_json::json!({
                    "inactive_before": inactive_before,
                    "archive_after_days": days,
                }),
            })
            .await?;
    }
    Ok(archived)
}

pub fn spawn_archive_loop_until_cancelled(
    state: AppState,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    crate::background_loop::spawn_periodic_loop(
        ARCHIVE_SWEEP_INTERVAL,
        shutdown,
        "session archive sweep failed",
        move || {
            let state = state.clone();
            async move {
                let archived = archive_inactive_sessions(&state, Utc::now()).await?;
                if !archived.is_empty() {
                    tracing::info!(count = archived.len(), "archived inactive sessions");
                }
                Ok(())
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use swage_core::Role;
    use swage_storage::{
        AuditStore, MessageStore, NewMessage, NewSession, NewThread, SessionListing, SessionStore,
        SqliteStorage, ThreadStore, TopicStore,
    };

    use super::*;

    fn make_state(archive_after_days: u64) -> (AppState, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let mut config = swage_config::ResolvedConfig::default();
        config.sessions.archive_after_days = archive_after_days;
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        (state, storage)
    }

    /// A session whose last word was `days_ago` days ago.
    async fn session_spoken(storage: &SqliteStorage, days_ago: i64) -> Uuid {
        let general = storage.general_scope().await.expect("general scope");
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(general.id)
            })
            .await
            .expect("session");
        let thread = ThreadStore::create(
            storage,
            NewThread {
                session_id: session.id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .expect("thread");
        let message = storage
            .create_message(NewMessage {
                thread_id: thread.id,
                turn_id: None,
                role: Role::Assistant,
                content: "…".to_string(),
                attachments: Vec::new(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("message");
        let stamp = (Utc::now() - Duration::days(days_ago)).to_rfc3339();
        storage
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                    rusqlite::params![stamp, message.id.to_string()],
                )
            })
            .await
            .expect("backdate");
        session.id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sweep_archives_by_the_configured_delay_and_spares_executing_work() {
        let (state, storage) = make_state(3);
        let stale = session_spoken(&storage, 5).await;
        let busy = session_spoken(&storage, 5).await;
        let recent = session_spoken(&storage, 1).await;
        state
            .active_turns
            .lock()
            .await
            .insert(busy, CancellationToken::new());

        let archived = archive_inactive_sessions(&state, Utc::now())
            .await
            .expect("sweep");

        assert_eq!(archived, vec![stale]);
        let live: Vec<Uuid> = storage
            .list_recent_sessions(SessionListing::default())
            .await
            .expect("listing")
            .into_iter()
            .map(|activity| activity.session.id)
            .collect();
        assert!(live.contains(&busy) && live.contains(&recent) && !live.contains(&stale));
        let events = storage
            .list_audit_events_by_session(stale)
            .await
            .expect("audit");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, "archived");
        assert_eq!(events[0].entity_id, stale.to_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_loop_sweeps_on_start_and_stops_on_cancel() {
        let (state, storage) = make_state(1);
        let stale = session_spoken(&storage, 2).await;
        let shutdown = CancellationToken::new();

        let handle = spawn_archive_loop_until_cancelled(state, shutdown.clone());
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let hidden = storage
                .get_session(stale)
                .await
                .expect("session")
                .expect("exists")
                .archived_at
                .is_some();
            if hidden {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the first sweep never ran"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        shutdown.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("the loop should stop once cancelled")
            .expect("the loop should not panic");
    }
}
