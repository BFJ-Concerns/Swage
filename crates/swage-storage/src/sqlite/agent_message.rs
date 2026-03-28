//! SQLite implementation of [`AgentMessageStore`].
//!
//! Follows the same `with_conn` + row-mapping pattern as the approval store.
//! Status transitions use optimistic concurrency: `WHERE status = 'pending'`
//! ensures only one transition succeeds when concurrent deliveries race.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use swage_core::{MessageStatus, StorageError};

use crate::model::{AgentMessage, NewAgentMessage};
use crate::sqlite::SqliteStorage;
use crate::store::AgentMessageStore;

// ---------------------------------------------------------------------------
// Parse helpers (private, following per-module duplication pattern)
// ---------------------------------------------------------------------------

/// A simple wrapper to shuttle parse-error strings through rusqlite's error type.
#[derive(Debug)]
struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(ParseError(e.to_string())))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(ParseError(e.to_string())))
        })
}

fn parse_json(text: &str, col: usize) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(ParseError(e.to_string())))
    })
}

fn parse_error(col: usize, msg: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(ParseError(msg)))
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// Column order: id, sender_agent_id, recipient_agent_id, kind, correlation_id,
/// payload, status, failure_reason, created_at, delivered_at
fn row_to_agent_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessage> {
    let id: String = row.get(0)?;
    let sender_agent_id: String = row.get(1)?;
    let recipient_agent_id: String = row.get(2)?;
    let kind_str: String = row.get(3)?;
    let correlation_id_str: Option<String> = row.get(4)?;
    let payload_str: String = row.get(5)?;
    let status_str: String = row.get(6)?;
    let failure_reason: Option<String> = row.get(7)?;
    let created_at_str: String = row.get(8)?;
    let delivered_at_str: Option<String> = row.get(9)?;

    let id = parse_uuid(&id)?;
    let correlation_id = correlation_id_str
        .as_deref()
        .map(parse_uuid)
        .transpose()?;
    let payload = parse_json(&payload_str, 5)?;
    let kind = kind_str
        .parse()
        .map_err(|e: String| parse_error(3, e))?;
    let status = status_str
        .parse()
        .map_err(|e: String| parse_error(6, e))?;
    let created_at = parse_dt(&created_at_str)?;
    let delivered_at = delivered_at_str
        .as_deref()
        .map(parse_dt)
        .transpose()?;

    Ok(AgentMessage {
        id,
        sender_agent_id,
        recipient_agent_id,
        kind,
        correlation_id,
        payload,
        status,
        failure_reason,
        created_at,
        delivered_at,
    })
}

// ---------------------------------------------------------------------------
// Trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl AgentMessageStore for SqliteStorage {
    async fn create_agent_message(
        &self,
        msg: NewAgentMessage,
    ) -> Result<AgentMessage, StorageError> {
        // Pre-serialise outside the closure — serde errors are not rusqlite errors.
        let payload_str = serde_json::to_string(&msg.payload)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let correlation_str = msg.correlation_id.map(|c| c.to_string());
            let kind_str = msg.kind.to_string();

            conn.execute(
                "INSERT INTO agent_messages
                    (id, sender_agent_id, recipient_agent_id, kind, correlation_id,
                     payload, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
                params![
                    id.to_string(),
                    &msg.sender_agent_id,
                    &msg.recipient_agent_id,
                    &kind_str,
                    &correlation_str,
                    &payload_str,
                    &now_str,
                ],
            )?;

            Ok(AgentMessage {
                id,
                sender_agent_id: msg.sender_agent_id,
                recipient_agent_id: msg.recipient_agent_id,
                kind: msg.kind,
                correlation_id: msg.correlation_id,
                payload: msg.payload,
                status: MessageStatus::Pending,
                failure_reason: None,
                created_at: now,
                delivered_at: None,
            })
        })
        .await
    }

    async fn get_agent_message(&self, id: Uuid) -> Result<Option<AgentMessage>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, sender_agent_id, recipient_agent_id, kind, correlation_id,
                            payload, status, failure_reason, created_at, delivered_at
                     FROM agent_messages WHERE id = ?1",
                )
                ?;

            let result = stmt
                .query_row(params![id.to_string()], row_to_agent_message)
                .optional()
                ?;

            Ok(result)
        })
        .await
    }

    async fn list_pending_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let agent_id = agent_id.to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, sender_agent_id, recipient_agent_id, kind, correlation_id,
                            payload, status, failure_reason, created_at, delivered_at
                     FROM agent_messages
                     WHERE recipient_agent_id = ?1 AND status = 'pending'
                     ORDER BY created_at ASC",
                )
                ?;

            let rows = stmt
                .query_map(params![&agent_id], row_to_agent_message)
                ?;

            let mut messages = Vec::new();
            for row in rows {
                messages.push(row?);
            }
            Ok(messages)
        })
        .await
    }

    async fn update_agent_message_status(
        &self,
        id: Uuid,
        status: MessageStatus,
        failure_reason: Option<&str>,
    ) -> Result<bool, StorageError> {
        let status_str = status.to_string();
        let failure_reason = failure_reason.map(|s| s.to_string());
        let delivered_at = if status == MessageStatus::Delivered {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        };

        self.with_conn(move |conn| {
            // Only transition from 'pending' — prevents duplicate delivery.
            let rows = conn
                .execute(
                    "UPDATE agent_messages
                     SET status = ?1, failure_reason = ?2, delivered_at = ?3
                     WHERE id = ?4 AND status = 'pending'",
                    params![
                        &status_str,
                        &failure_reason,
                        &delivered_at,
                        id.to_string(),
                    ],
                )
                ?;

            Ok(rows > 0)
        })
        .await
    }

    async fn update_agent_message_payload(
        &self,
        id: Uuid,
        payload: serde_json::Value,
    ) -> Result<(), StorageError> {
        let payload_str = payload.to_string();
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE agent_messages SET payload = ?1 WHERE id = ?2",
                params![&payload_str, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn list_by_correlation(
        &self,
        correlation_id: Uuid,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, sender_agent_id, recipient_agent_id, kind, correlation_id,
                            payload, status, failure_reason, created_at, delivered_at
                     FROM agent_messages
                     WHERE correlation_id = ?1
                     ORDER BY created_at ASC",
                )
                ?;

            let rows = stmt
                .query_map(params![correlation_id.to_string()], row_to_agent_message)
                ?;

            let mut messages = Vec::new();
            for row in rows {
                messages.push(row?);
            }
            Ok(messages)
        })
        .await
    }

    async fn expire_stale_messages(
        &self,
        max_age: std::time::Duration,
    ) -> Result<u64, StorageError> {
        self.with_conn(move |conn| {
            let chrono_age = chrono::Duration::from_std(max_age)
                .unwrap_or(chrono::Duration::max_value());
            let cutoff = Utc::now() - chrono_age;
            let cutoff_str = cutoff.to_rfc3339();

            let rows = conn
                .execute(
                    "UPDATE agent_messages
                     SET status = 'failed', failure_reason = 'delivery timeout'
                     WHERE status = 'pending' AND created_at < ?1",
                    params![&cutoff_str],
                )
                ?;

            Ok(rows as u64)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::MessageKind;

    async fn make_storage() -> SqliteStorage {
        SqliteStorage::in_memory().unwrap()
    }

    fn delegate_msg() -> NewAgentMessage {
        NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "summarise report"}),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get() {
        let storage = make_storage().await;
        let msg = storage.create_agent_message(delegate_msg()).await.unwrap();

        assert_eq!(msg.sender_agent_id, "agent-a");
        assert_eq!(msg.recipient_agent_id, "agent-b");
        assert_eq!(msg.kind, MessageKind::Delegate);
        assert_eq!(msg.status, MessageStatus::Pending);
        assert!(msg.correlation_id.is_none());
        assert!(msg.failure_reason.is_none());
        assert!(msg.delivered_at.is_none());

        let fetched = storage.get_agent_message(msg.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, msg.id);
        assert_eq!(fetched.kind, MessageKind::Delegate);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_nonexistent_returns_none() {
        let storage = make_storage().await;
        let result = storage.get_agent_message(Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_pending_for_agent_filters_correctly() {
        let storage = make_storage().await;

        // Create two messages for agent-b, one for agent-c.
        let m1 = storage.create_agent_message(delegate_msg()).await.unwrap();
        let m2 = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Shutdown,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let _m3 = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-c".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        let pending = storage.list_pending_for_agent("agent-b").await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, m1.id);
        assert_eq!(pending[1].id, m2.id);

        // Mark m1 as delivered — no longer in pending list.
        storage
            .update_agent_message_status(m1.id, MessageStatus::Delivered, None)
            .await
            .unwrap();
        let pending = storage.list_pending_for_agent("agent-b").await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, m2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_status_atomic_transition() {
        let storage = make_storage().await;
        let msg = storage.create_agent_message(delegate_msg()).await.unwrap();

        // First transition succeeds.
        let ok = storage
            .update_agent_message_status(msg.id, MessageStatus::Delivered, None)
            .await
            .unwrap();
        assert!(ok);

        // Verify delivered_at is set.
        let updated = storage.get_agent_message(msg.id).await.unwrap().unwrap();
        assert_eq!(updated.status, MessageStatus::Delivered);
        assert!(updated.delivered_at.is_some());

        // Second transition fails — already terminal.
        let nope = storage
            .update_agent_message_status(msg.id, MessageStatus::Failed, Some("too late"))
            .await
            .unwrap();
        assert!(!nope);

        // Status unchanged.
        let still = storage.get_agent_message(msg.id).await.unwrap().unwrap();
        assert_eq!(still.status, MessageStatus::Delivered);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_status_to_failed_with_reason() {
        let storage = make_storage().await;
        let msg = storage.create_agent_message(delegate_msg()).await.unwrap();

        let ok = storage
            .update_agent_message_status(msg.id, MessageStatus::Failed, Some("agent not found"))
            .await
            .unwrap();
        assert!(ok);

        let updated = storage.get_agent_message(msg.id).await.unwrap().unwrap();
        assert_eq!(updated.status, MessageStatus::Failed);
        assert_eq!(updated.failure_reason.as_deref(), Some("agent not found"));
        assert!(updated.delivered_at.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_correlation_groups_messages() {
        let storage = make_storage().await;
        let correlation = Uuid::new_v4();

        // Delegate (origin).
        let _delegate = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({"task": "work"}),
            })
            .await
            .unwrap();

        // Result correlated to the delegate.
        let _result = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-b".into(),
                recipient_agent_id: "agent-a".into(),
                kind: MessageKind::Result,
                correlation_id: Some(correlation),
                payload: serde_json::json!({"output": "done"}),
            })
            .await
            .unwrap();

        // Cancel also correlated.
        let _cancel = storage
            .create_agent_message(NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Cancel,
                correlation_id: Some(correlation),
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        let correlated = storage.list_by_correlation(correlation).await.unwrap();
        assert_eq!(correlated.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expire_stale_messages_transitions_to_failed() {
        let storage = make_storage().await;
        let _msg = storage.create_agent_message(delegate_msg()).await.unwrap();

        // Zero-duration TTL forces immediate expiry.
        let expired = storage
            .expire_stale_messages(std::time::Duration::from_secs(0))
            .await
            .unwrap();
        assert_eq!(expired, 1);

        let updated = storage.get_agent_message(_msg.id).await.unwrap().unwrap();
        assert_eq!(updated.status, MessageStatus::Failed);
        assert_eq!(updated.failure_reason.as_deref(), Some("delivery timeout"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expire_skips_already_delivered() {
        let storage = make_storage().await;
        let msg = storage.create_agent_message(delegate_msg()).await.unwrap();

        // Mark as delivered first.
        storage
            .update_agent_message_status(msg.id, MessageStatus::Delivered, None)
            .await
            .unwrap();

        // Expiry should not affect delivered messages.
        let expired = storage
            .expire_stale_messages(std::time::Duration::from_secs(0))
            .await
            .unwrap();
        assert_eq!(expired, 0);
    }
}
