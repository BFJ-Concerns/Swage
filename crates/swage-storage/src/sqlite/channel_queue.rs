use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{ErrorCode, OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    ChannelInboundMessage, ChannelInboundMessageState, NewChannelInboundMessage,
    PendingChannelOrigin, Turn, TurnState,
};
use crate::sqlite::SqliteStorage;
use crate::store::ChannelQueueStore;

#[derive(Debug)]
struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn parse_state(value: String) -> rusqlite::Result<ChannelInboundMessageState> {
    value
        .parse::<ChannelInboundMessageState>()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(ParseError(error)),
            )
        })
}

fn row_to_channel_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelInboundMessage> {
    let id: String = row.get(0)?;
    let state: String = row.get(7)?;
    let turn_id: Option<String> = row.get(9)?;
    let received_at: String = row.get(10)?;
    let presented_at: Option<String> = row.get(11)?;

    Ok(ChannelInboundMessage {
        id: parse_uuid(&id)?,
        integration: row.get(1)?,
        account: row.get(2)?,
        conversation: row.get(3)?,
        external_id: row.get(4)?,
        sender: row.get(5)?,
        content: row.get(6)?,
        state: parse_state(state)?,
        discard_reason: row.get(8)?,
        turn_id: turn_id.as_deref().map(parse_uuid).transpose()?,
        received_at: parse_dt(&received_at)?,
        presented_at: presented_at.as_deref().map(parse_dt).transpose()?,
    })
}

#[async_trait]
impl ChannelQueueStore for SqliteStorage {
    async fn ingest_channel_inbound(
        &self,
        messages: Vec<NewChannelInboundMessage>,
        cursor_integration: &str,
        cursor_account: &str,
        cursor: &str,
    ) -> Result<usize, StorageError> {
        let cursor_integration = cursor_integration.to_string();
        let cursor_account = cursor_account.to_string();
        let cursor = cursor.to_string();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut inserted = 0usize;
            let now = Utc::now().to_rfc3339();

            for message in messages {
                let affected = tx.execute(
                    "INSERT OR IGNORE INTO channel_inbound_messages
                     (id, integration, account, conversation, external_id, sender, content, state, received_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
                    params![
                        Uuid::new_v4().to_string(),
                        message.integration,
                        message.account,
                        message.conversation,
                        message.external_id,
                        message.sender,
                        message.content,
                        &now,
                    ],
                )?;
                inserted += affected;
            }

            tx.execute(
                "INSERT INTO channel_cursors (integration, account, cursor, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(integration, account) DO UPDATE SET
                    cursor = excluded.cursor,
                    updated_at = excluded.updated_at
                 WHERE CAST(excluded.cursor AS INTEGER) > CAST(channel_cursors.cursor AS INTEGER)",
                params![cursor_integration, cursor_account, cursor, &now],
            )?;
            tx.commit()?;
            Ok(inserted)
        })
        .await
    }

    async fn get_channel_cursor(
        &self,
        integration: &str,
        account: &str,
    ) -> Result<Option<String>, StorageError> {
        let integration = integration.to_string();
        let account = account.to_string();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT cursor FROM channel_cursors
                 WHERE integration = ?1 AND account = ?2",
                params![integration, account],
                |row| row.get(0),
            )
            .optional()
        })
        .await
    }

    async fn list_pending_channel_origins(
        &self,
    ) -> Result<Vec<PendingChannelOrigin>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT integration, account, conversation
                 FROM channel_inbound_messages
                 WHERE state = 'pending'
                 GROUP BY integration, account, conversation
                 ORDER BY MIN(received_at) ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(PendingChannelOrigin {
                    integration: row.get(0)?,
                    account: row.get(1)?,
                    conversation: row.get(2)?,
                })
            })?;
            rows.collect()
        })
        .await
    }

    async fn list_pending_by_origin(
        &self,
        origin: PendingChannelOrigin,
    ) -> Result<Vec<ChannelInboundMessage>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, integration, account, conversation, external_id, sender, content,
                        state, discard_reason, turn_id, received_at, presented_at
                 FROM channel_inbound_messages
                 WHERE state = 'pending'
                   AND integration = ?1
                   AND account = ?2
                   AND conversation = ?3
                 ORDER BY received_at ASC, rowid ASC",
            )?;
            let rows = stmt.query_map(
                params![origin.integration, origin.account, origin.conversation],
                row_to_channel_message,
            )?;
            rows.collect()
        })
        .await
    }

    async fn begin_channel_turn(
        &self,
        thread_id: Uuid,
        agent_id: &str,
        composed_input: &str,
        message_ids: Vec<Uuid>,
    ) -> Result<Turn, StorageError> {
        let agent_id = agent_id.to_string();
        let composed_input = composed_input.to_string();
        self.with_conn(move |conn| {
            if message_ids.is_empty() {
                return Err(rusqlite::Error::InvalidParameterName(
                    "message_ids must not be empty".to_string(),
                ));
            }

            let tx = conn.unchecked_transaction()?;
            let turn_id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let state = TurnState::Pending;
            let state_str = state.to_string();

            tx.execute(
                "INSERT INTO turns (id, thread_id, agent_id, user_input, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    turn_id.to_string(),
                    thread_id.to_string(),
                    &agent_id,
                    &composed_input,
                    &state_str,
                    &now_str,
                ],
            )?;

            let mut claimed = 0usize;
            for id in &message_ids {
                claimed += tx.execute(
                    "UPDATE channel_inbound_messages
                     SET state = 'presented',
                         turn_id = ?2,
                         presented_at = ?3
                     WHERE id = ?1
                       AND state = 'pending'",
                    params![id.to_string(), turn_id.to_string(), &now_str],
                )?;
            }
            if claimed != message_ids.len() {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: ErrorCode::ConstraintViolation,
                        extended_code: 0,
                    },
                    Some("channel messages were claimed by another drain".to_string()),
                ));
            }

            tx.commit()?;
            Ok(Turn {
                id: turn_id,
                thread_id,
                agent_id,
                state,
                user_input: Some(composed_input),
                inbound_audit: None,
                assistant_response: None,
                input_tokens: None,
                output_tokens: None,
                cost_microcents: None,
                structured_output: None,
                parent_turn_id: None,
                created_at: now,
                completed_at: None,
                delegated_input_tokens: None,
                delegated_output_tokens: None,
            })
        })
        .await
    }

    async fn discard_channel_messages(
        &self,
        message_ids: Vec<Uuid>,
        reason: &str,
    ) -> Result<u64, StorageError> {
        let reason = reason.to_string();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut affected = 0u64;
            for id in message_ids {
                affected += tx.execute(
                    "UPDATE channel_inbound_messages
                     SET state = 'discarded',
                         discard_reason = ?2
                     WHERE id = ?1
                       AND state = 'pending'",
                    params![id.to_string(), &reason],
                )? as u64;
            }
            tx.commit()?;
            Ok(affected)
        })
        .await
    }

    async fn requeue_presented_for_inflight_turns(&self) -> Result<u64, StorageError> {
        self.with_conn(|conn| {
            let affected = conn.execute(
                "UPDATE channel_inbound_messages
                 SET state = 'pending',
                     turn_id = NULL,
                     presented_at = NULL,
                     discard_reason = NULL
                 WHERE state = 'presented'
                   AND turn_id IN (
                       SELECT id FROM turns
                       WHERE state IN ('pending', 'processing', 'awaiting_approval', 'delegating')
                   )",
                [],
            )?;
            Ok(affected as u64)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewSession, NewSpace, NewThread};
    use crate::store::{ChannelQueueStore, SessionStore, SpaceStore, ThreadStore, TurnStore};

    fn inbound(external_id: &str, content: &str) -> NewChannelInboundMessage {
        NewChannelInboundMessage {
            integration: "telegram".into(),
            account: "bot-1".into(),
            conversation: "chat-1".into(),
            external_id: external_id.into(),
            sender: "sender-1".into(),
            content: content.into(),
        }
    }

    async fn thread(storage: &SqliteStorage) -> (Uuid, String) {
        let space = SpaceStore::create(
            storage,
            NewSpace {
                name: "queue space".into(),
                slug: "queue-space".into(),
            },
        )
        .await
        .unwrap();
        let agent_id = "gateway".to_string();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: agent_id.clone(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            storage,
            NewThread {
                session_id: session.id,
                agent_id: agent_id.clone(),
            },
        )
        .await
        .unwrap();
        (thread.id, agent_id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ingest_deduplicates_and_advances_cursor_atomically() {
        let storage = SqliteStorage::in_memory().unwrap();

        let inserted = storage
            .ingest_channel_inbound(
                vec![inbound("10", "first"), inbound("10", "duplicate")],
                "telegram",
                "bot-1",
                "10",
            )
            .await
            .unwrap();
        assert_eq!(inserted, 1);
        assert_eq!(
            storage
                .get_channel_cursor("telegram", "bot-1")
                .await
                .unwrap()
                .as_deref(),
            Some("10")
        );

        let origins = storage.list_pending_channel_origins().await.unwrap();
        assert_eq!(origins.len(), 1);
        let rows = storage
            .list_pending_by_origin(origins.into_iter().next().unwrap())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "first");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursor_only_moves_forward() {
        let storage = SqliteStorage::in_memory().unwrap();

        storage
            .ingest_channel_inbound(vec![inbound("20", "newer")], "telegram", "bot-1", "20")
            .await
            .unwrap();
        storage
            .ingest_channel_inbound(vec![inbound("19", "older")], "telegram", "bot-1", "19")
            .await
            .unwrap();

        assert_eq!(
            storage
                .get_channel_cursor("telegram", "bot-1")
                .await
                .unwrap()
                .as_deref(),
            Some("20")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn begin_channel_turn_claims_only_named_rows() {
        let storage = SqliteStorage::in_memory().unwrap();
        let (thread_id, agent_id) = thread(&storage).await;
        storage
            .ingest_channel_inbound(
                vec![inbound("11", "one"), inbound("12", "two")],
                "telegram",
                "bot-1",
                "12",
            )
            .await
            .unwrap();
        let origin = storage
            .list_pending_channel_origins()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let rows = storage
            .list_pending_by_origin(origin.clone())
            .await
            .unwrap();
        let turn = storage
            .begin_channel_turn(thread_id, &agent_id, "one", vec![rows[0].id])
            .await
            .unwrap();
        assert_eq!(turn.user_input.as_deref(), Some("one"));

        let remaining = storage.list_pending_by_origin(origin).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].content, "two");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn begin_channel_turn_rolls_back_when_claim_conflicts() {
        let storage = SqliteStorage::in_memory().unwrap();
        let (thread_id, agent_id) = thread(&storage).await;
        storage
            .ingest_channel_inbound(
                vec![inbound("21", "one"), inbound("22", "two")],
                "telegram",
                "bot-1",
                "22",
            )
            .await
            .unwrap();
        let origin = storage
            .list_pending_channel_origins()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let rows = storage
            .list_pending_by_origin(origin.clone())
            .await
            .unwrap();

        let result = storage
            .begin_channel_turn(
                thread_id,
                &agent_id,
                "one",
                vec![rows[0].id, Uuid::new_v4()],
            )
            .await;

        assert!(matches!(result, Err(StorageError::ConstraintViolation(_))));
        let pending = storage.list_pending_by_origin(origin).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            storage.list_turns_by_thread(thread_id).await.unwrap().len(),
            0,
            "the failed claim must roll back the placeholder turn"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requeue_presented_messages_for_pending_turns() {
        let storage = SqliteStorage::in_memory().unwrap();
        let (thread_id, agent_id) = thread(&storage).await;
        storage
            .ingest_channel_inbound(vec![inbound("13", "stuck")], "telegram", "bot-1", "13")
            .await
            .unwrap();
        let origin = storage
            .list_pending_channel_origins()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let rows = storage
            .list_pending_by_origin(origin.clone())
            .await
            .unwrap();
        storage
            .begin_channel_turn(thread_id, &agent_id, "stuck", vec![rows[0].id])
            .await
            .unwrap();

        let requeued = storage
            .requeue_presented_for_inflight_turns()
            .await
            .unwrap();
        assert_eq!(requeued, 1);
        let rows = storage.list_pending_by_origin(origin).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "stuck");
    }
}
