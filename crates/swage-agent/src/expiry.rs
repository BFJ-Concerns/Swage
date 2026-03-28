//! Delivery expiry processing for inter-agent messages.
//!
//! Durable messages have a configurable TTL. If the target agent is not
//! instantiated within the TTL, the message transitions to Failed with
//! "delivery timeout". Checked on gateway startup and periodically.

use std::sync::Arc;
use std::time::Duration;

use swage_storage::store::AgentMessageStore;
use swage_storage::Storage;

/// Default message TTL: 24 hours.
pub const DEFAULT_MESSAGE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Default expiry check cadence: every 60 seconds.
pub const DEFAULT_EXPIRY_INTERVAL: Duration = Duration::from_secs(60);

/// Expire pending messages older than `ttl`.
///
/// Delegates to `AgentMessageStore::expire_stale_messages`. Returns the count
/// of messages transitioned to Failed.
pub async fn expire_stale_messages(
    storage: &Arc<dyn Storage>,
    ttl: Duration,
) -> Result<u64, swage_core::StorageError> {
    storage.expire_stale_messages(ttl).await
}

/// Spawn a background task that periodically expires stale messages.
///
/// Runs once immediately on spawn, then every `interval`. Errors are logged
/// but do not stop the task — the next tick retries.
///
/// Returns a `JoinHandle` that can be used to abort the task on shutdown.
pub fn spawn_expiry_task(
    storage: Arc<dyn Storage>,
    ttl: Duration,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip missed ticks rather than bursting — a background cleanup task
        // doesn't need to catch up; the next tick covers all expired messages.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;

            match expire_stale_messages(&storage, ttl).await {
                Ok(0) => {} // Nothing to expire — common case, no log noise.
                Ok(count) => {
                    tracing::info!(count, "expired stale agent messages");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to expire stale agent messages");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::MessageKind;
    use swage_storage::model::NewAgentMessage;
    use swage_storage::SqliteStorage;

    #[tokio::test(flavor = "multi_thread")]
    async fn expire_with_zero_ttl() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());

        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "a".into(),
                recipient_agent_id: "b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        let count = expire_stale_messages(&storage, Duration::from_secs(0))
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expire_with_long_ttl_skips() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());

        storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "a".into(),
                recipient_agent_id: "b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        // 24-hour TTL — message was just created, should not expire.
        let count = expire_stale_messages(&storage, DEFAULT_MESSAGE_TTL)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
