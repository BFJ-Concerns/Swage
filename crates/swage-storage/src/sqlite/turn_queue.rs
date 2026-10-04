//! The turn queue: messages accepted while a session's turn was running,
//! held durably in arrival order until the next turn takes them as its input.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    DrainedTurnQueue, Message, MessageOrigin, NewQueuedTurnInput, QueuedTurnInput, Turn, TurnState,
};
use crate::sqlite::SqliteStorage;
use crate::sqlite::message::encode_attachments;
use crate::store::TurnQueueStore;
use swage_core::Role;

const COLUMNS: &str = "id, session_id, thread_id, seq, origin, content, attachments, created_at";

fn parse_uuid(value: &str) -> rusqlite::Result<Uuid> {
    value.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_dt(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn row_to_queued(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedTurnInput> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let thread_id: String = row.get(2)?;
    let origin: String = row.get(4)?;
    let attachments: String = row.get(6)?;
    let created_at: String = row.get(7)?;
    Ok(QueuedTurnInput {
        id: parse_uuid(&id)?,
        session_id: parse_uuid(&session_id)?,
        thread_id: parse_uuid(&thread_id)?,
        seq: row.get(3)?,
        origin: origin.parse::<MessageOrigin>().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, error.into())
        })?,
        content: row.get(5)?,
        attachments: serde_json::from_str(&attachments).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        created_at: parse_dt(&created_at)?,
    })
}

#[async_trait]
impl TurnQueueStore for SqliteStorage {
    async fn enqueue_turn_input(
        &self,
        input: NewQueuedTurnInput,
    ) -> Result<QueuedTurnInput, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let attachments = serde_json::to_string(&input.attachments)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            // The sequence is allocated inside the connection's lock, so two
            // arrivals never share a place in the queue.
            let seq: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq) + 1, 0) FROM turn_queue WHERE session_id = ?1",
                params![input.session_id.to_string()],
                |row| row.get(0),
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO turn_queue ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                ),
                params![
                    id.to_string(),
                    input.session_id.to_string(),
                    input.thread_id.to_string(),
                    seq,
                    input.origin.as_str(),
                    &input.content,
                    attachments,
                    now.to_rfc3339(),
                ],
            )?;
            Ok(QueuedTurnInput {
                id,
                session_id: input.session_id,
                thread_id: input.thread_id,
                seq,
                origin: input.origin,
                content: input.content,
                attachments: input.attachments,
                created_at: now,
            })
        })
        .await
    }

    async fn list_queued_turn_inputs(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<QueuedTurnInput>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM turn_queue WHERE session_id = ?1 ORDER BY seq ASC"
            ))?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_queued)?;
            rows.collect()
        })
        .await
    }

    async fn drain_queued_turn_inputs(
        &self,
        session_id: Uuid,
        agent_id: &str,
    ) -> Result<Option<DrainedTurnQueue>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let queued = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLUMNS} FROM turn_queue WHERE session_id = ?1 ORDER BY seq ASC"
                ))?;
                let rows = stmt.query_map(params![session_id.to_string()], row_to_queued)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let Some(first) = queued.first() else {
                return Ok(None);
            };
            let thread_id = first.thread_id;
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            // The claim: the thread's one-live-turn index refuses a second
            // live turn, in which case nothing here happened.
            let turn_id = Uuid::new_v4();
            let user_input = queued
                .iter()
                .map(|item| item.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            match tx.execute(
                "INSERT INTO turns (id, thread_id, agent_id, user_input, state, created_at, parent_turn_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                params![
                    turn_id.to_string(),
                    thread_id.to_string(),
                    &agent_id,
                    &user_input,
                    TurnState::Pending.to_string(),
                    &now_str,
                ],
            ) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(failure, _))
                    if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }

            // One recorded message per queued input, in arrival order, each
            // with the origin it arrived with.
            let mut messages = Vec::with_capacity(queued.len());
            for item in &queued {
                let position: i32 = tx.query_row(
                    "SELECT COALESCE(MAX(position) + 1, 0) FROM messages WHERE thread_id = ?1",
                    params![thread_id.to_string()],
                    |row| row.get(0),
                )?;
                let message_id = Uuid::new_v4();
                tx.execute(
                    "INSERT INTO messages
                         (id, thread_id, turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at, attachments, origin)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?10)",
                    params![
                        message_id.to_string(),
                        thread_id.to_string(),
                        turn_id.to_string(),
                        Role::User.to_string(),
                        &item.content,
                        position,
                        &agent_id,
                        &now_str,
                        encode_attachments(&item.attachments)?,
                        item.origin.as_str(),
                    ],
                )?;
                messages.push(Message {
                    id: message_id,
                    thread_id,
                    attachments: item.attachments.clone(),
                    turn_id: Some(turn_id),
                    role: Role::User,
                    origin: item.origin,
                    content: item.content.clone(),
                    position,
                    agent_id: agent_id.clone(),
                    tool_call_id: None,
                    tool_name: None,
                    created_at: now,
                });
            }
            tx.execute(
                "DELETE FROM turn_queue WHERE session_id = ?1",
                params![session_id.to_string()],
            )?;
            tx.commit()?;
            let queued_ids = queued.iter().map(|item| item.id).collect();
            Ok(Some(DrainedTurnQueue {
                turn: Turn {
                    id: turn_id,
                    thread_id,
                    agent_id,
                    state: TurnState::Pending,
                    user_input: Some(user_input),
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
                    execution_isolation: None,
                },
                messages,
                queued_ids,
            }))
        })
        .await
    }

    async fn sessions_with_queued_turn_inputs(&self) -> Result<Vec<Uuid>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT DISTINCT session_id FROM turn_queue ORDER BY session_id")?;
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                parse_uuid(&id)
            })?;
            rows.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::new_session;
    use crate::model::{NewThread, NewTopic};
    use crate::store::{SessionStore, ThreadStore, TopicStore};

    async fn session_and_thread() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = TopicStore::create(
            &storage,
            NewTopic {
                name: "t".into(),
                slug: "t".into(),
            },
        )
        .await
        .unwrap();
        let session = storage.create_session(new_session(topic.id)).await.unwrap();
        let thread = ThreadStore::create(
            &storage,
            NewThread {
                session_id: session.id,
                agent_id: "gateway".into(),
            },
        )
        .await
        .unwrap();
        (storage, session.id, thread.id)
    }

    fn queued(session_id: Uuid, thread_id: Uuid, content: &str) -> NewQueuedTurnInput {
        NewQueuedTurnInput {
            session_id,
            thread_id,
            origin: MessageOrigin::User,
            content: content.into(),
            attachments: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn arrivals_keep_their_order_and_a_take_empties_the_queue_once() {
        let (storage, session_id, thread_id) = session_and_thread().await;
        let first = storage
            .enqueue_turn_input(queued(session_id, thread_id, "first"))
            .await
            .unwrap();
        let second = storage
            .enqueue_turn_input(NewQueuedTurnInput {
                origin: MessageOrigin::Schedule,
                ..queued(session_id, thread_id, "second")
            })
            .await
            .unwrap();
        assert!(first.seq < second.seq);
        assert_eq!(second.origin, MessageOrigin::Schedule);

        let listed = storage.list_queued_turn_inputs(session_id).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|q| q.content.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(
            storage.sessions_with_queued_turn_inputs().await.unwrap(),
            vec![session_id]
        );

        let drained = storage
            .drain_queued_turn_inputs(session_id, "gateway")
            .await
            .unwrap()
            .expect("two queued inputs drain");
        assert_eq!(drained.turn.user_input.as_deref(), Some("first\n\nsecond"));
        assert_eq!(
            drained
                .messages
                .iter()
                .map(|message| (message.content.as_str(), message.origin, message.turn_id))
                .collect::<Vec<_>>(),
            vec![
                ("first", MessageOrigin::User, Some(drained.turn.id)),
                ("second", MessageOrigin::Schedule, Some(drained.turn.id)),
            ],
            "one recorded message per input, in order, with its origin"
        );
        assert!(
            storage
                .list_queued_turn_inputs(session_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            storage
                .drain_queued_turn_inputs(session_id, "gateway")
                .await
                .unwrap()
                .is_none(),
            "a second drain finds nothing: the queue hands over once"
        );
        assert!(
            storage
                .sessions_with_queued_turn_inputs()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_drain_that_loses_the_claim_leaves_the_queue_exactly_as_it_was() {
        use crate::model::NewTurn;
        use crate::store::TurnStore;

        let (storage, session_id, thread_id) = session_and_thread().await;
        storage
            .enqueue_turn_input(queued(session_id, thread_id, "waiting"))
            .await
            .unwrap();
        // A live turn holds the thread: the one-live-turn index refuses the
        // drain's claim and the transaction rolls back whole.
        storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".into(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        assert!(
            storage
                .drain_queued_turn_inputs(session_id, "gateway")
                .await
                .unwrap()
                .is_none()
        );
        let still_queued = storage.list_queued_turn_inputs(session_id).await.unwrap();
        assert_eq!(still_queued.len(), 1);
        assert_eq!(still_queued[0].content, "waiting");
        assert_eq!(still_queued[0].seq, 0, "the same row, not a re-insertion");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_queued_message_keeps_its_attachments_opaquely() {
        let (storage, session_id, thread_id) = session_and_thread().await;
        let image = serde_json::json!({"kind": "image_url", "url": "https://example.test/x.png"});
        storage
            .enqueue_turn_input(NewQueuedTurnInput {
                attachments: vec![image.clone()],
                ..queued(session_id, thread_id, "see this")
            })
            .await
            .unwrap();
        let listed = storage.list_queued_turn_inputs(session_id).await.unwrap();
        assert_eq!(listed[0].attachments, vec![image]);
    }
}
