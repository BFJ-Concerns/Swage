use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    ChannelInboundMessage, ChannelInboundMessageState, NewChannelInboundMessage,
    PendingChannelOrigin,
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

    async fn list_pending_channel_messages(
        &self,
    ) -> Result<Vec<ChannelInboundMessage>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, integration, account, conversation, external_id, sender, content,
                        state, discard_reason, turn_id, received_at, presented_at
                 FROM channel_inbound_messages
                 WHERE state = 'pending'
                   AND NOT EXISTS (SELECT 1 FROM turn_queue_channel_messages q WHERE q.message_id = channel_inbound_messages.id)
                 ORDER BY received_at ASC, rowid ASC",
            )?;
            stmt.query_map([], row_to_channel_message)?.collect()
        }).await
    }

    async fn list_pending_channel_origins(
        &self,
    ) -> Result<Vec<PendingChannelOrigin>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT integration, account, conversation
                 FROM channel_inbound_messages
                 WHERE state = 'pending'
                   AND NOT EXISTS (SELECT 1 FROM turn_queue_channel_messages q WHERE q.message_id = channel_inbound_messages.id)
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
                   AND NOT EXISTS (SELECT 1 FROM turn_queue_channel_messages q WHERE q.message_id = channel_inbound_messages.id)
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
                       AND state = 'pending'
                       AND NOT EXISTS (SELECT 1 FROM turn_queue_channel_messages q WHERE q.message_id = channel_inbound_messages.id)",
                    params![id.to_string(), &reason],
                )? as u64;
            }
            tx.commit()?;
            Ok(affected)
        })
        .await
    }

    async fn list_channel_messages_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ChannelInboundMessage>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, integration, account, conversation, external_id, sender, content,
                        state, discard_reason, turn_id, received_at, presented_at
                 FROM channel_inbound_messages
                 WHERE turn_id = ?1
                 ORDER BY received_at ASC, rowid ASC",
            )?;
            stmt.query_map(params![turn_id.to_string()], row_to_channel_message)?
                .collect()
        })
        .await
    }

    async fn requeue_presented_for_inflight_turns(&self) -> Result<u64, StorageError> {
        self.with_conn(|conn| {
            // A delegating turn is not in this set: recovery resumes it
            // with its presented rows carried on the continuation.
            let affected = conn.execute(
                "UPDATE channel_inbound_messages
                 SET state = 'pending',
                     turn_id = NULL,
                     presented_at = NULL,
                     discard_reason = NULL
                 WHERE state = 'presented'
                   AND turn_id IN (
                       SELECT id FROM turns
                       WHERE state IN ('pending', 'processing', 'awaiting_approval')
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
    use crate::model::{
        MessageOrigin, NewQueuedTurnInput, NewSession, NewThread, NewTopic, Turn, TurnState,
    };
    use crate::store::{
        ChannelQueueStore, SessionStore, ThreadStore, TopicStore, TurnQueueStore, TurnStore,
    };

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
        let topic = TopicStore::create(
            storage,
            NewTopic {
                name: "queue topic".into(),
                slug: "queue-topic".into(),
            },
        )
        .await
        .unwrap();
        let agent_id = "gateway".to_string();
        let session = storage
            .create_session(NewSession {
                agent_id: agent_id.clone(),
                ..crate::fixtures::new_session(topic.id)
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

    async fn present_channel_rows(
        storage: &SqliteStorage,
        thread_id: Uuid,
        agent_id: &str,
        rows: &[ChannelInboundMessage],
    ) -> Turn {
        let session_id = ThreadStore::get(storage, thread_id)
            .await
            .unwrap()
            .unwrap()
            .session_id;
        storage
            .enqueue_turn_inputs(
                rows.iter()
                    .map(|row| NewQueuedTurnInput {
                        session_id,
                        thread_id,
                        origin: MessageOrigin::User,
                        content: row.content.clone(),
                        attended: false,
                        delivery_targets: Vec::new(),
                        client_id: None,
                        attachments: Vec::new(),
                        schedule_run_id: None,
                        channel_message_ids: vec![row.id],
                    })
                    .collect(),
            )
            .await
            .unwrap();
        storage
            .drain_queued_turn_inputs(session_id, agent_id)
            .await
            .unwrap()
            .unwrap()
            .turn
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
        present_channel_rows(&storage, thread_id, &agent_id, &rows).await;

        let requeued = storage
            .requeue_presented_for_inflight_turns()
            .await
            .unwrap();
        assert_eq!(requeued, 1);
        let rows = storage.list_pending_by_origin(origin).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "stuck");
    }

    /// A turn cut off while delegating keeps its presented rows: recovery
    /// resumes that turn with them, so re-presenting them would run the
    /// chat's message a second time beside the resumption.
    #[tokio::test(flavor = "multi_thread")]
    async fn requeue_leaves_a_delegating_turns_messages_presented() {
        let storage = SqliteStorage::in_memory().unwrap();
        let (thread_id, agent_id) = thread(&storage).await;
        storage
            .ingest_channel_inbound(vec![inbound("14", "delegated")], "telegram", "bot-1", "14")
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
        let turn = present_channel_rows(&storage, thread_id, &agent_id, &rows).await;
        storage
            .update_turn_state(turn.id, TurnState::Delegating)
            .await
            .unwrap();

        let requeued = storage
            .requeue_presented_for_inflight_turns()
            .await
            .unwrap();
        assert_eq!(requeued, 0);
        assert!(
            storage
                .list_pending_by_origin(origin)
                .await
                .unwrap()
                .is_empty()
        );
        let presented = storage
            .list_channel_messages_by_turn(turn.id)
            .await
            .unwrap();
        assert_eq!(presented.len(), 1);
        assert_eq!(presented[0].id, rows[0].id);
        assert_eq!(presented[0].turn_id, Some(turn.id));
    }

    /// The rows a turn presented are read back by the turn, in arrival
    /// order, and a turn no channel started has none.
    #[tokio::test(flavor = "multi_thread")]
    async fn channel_messages_list_by_the_turn_that_presented_them() {
        let storage = SqliteStorage::in_memory().unwrap();
        let (thread_id, agent_id) = thread(&storage).await;
        storage
            .ingest_channel_inbound(
                vec![inbound("15", "first"), inbound("16", "second")],
                "telegram",
                "bot-1",
                "16",
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
        let rows = storage.list_pending_by_origin(origin).await.unwrap();
        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        let turn = present_channel_rows(&storage, thread_id, &agent_id, &rows).await;
        let other = storage
            .create_turn(crate::model::NewTurn {
                thread_id,
                agent_id: agent_id.clone(),
                state: TurnState::Completed,
                user_input: Some("typed".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();

        let presented = storage
            .list_channel_messages_by_turn(turn.id)
            .await
            .unwrap();
        assert_eq!(presented.iter().map(|row| row.id).collect::<Vec<_>>(), ids);
        assert!(
            storage
                .list_channel_messages_by_turn(other.id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
