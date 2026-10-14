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

const COLUMNS: &str = "id, session_id, thread_id, seq, origin, content, attachments, created_at, schedule_run_id, attended, client_id";

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
        attended: row.get(9)?,
        client_id: row
            .get::<_, Option<String>>(10)?
            .as_deref()
            .map(parse_uuid)
            .transpose()?,
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
        schedule_run_id: row
            .get::<_, Option<String>>(8)?
            .as_deref()
            .map(parse_uuid)
            .transpose()?,
        channel_message_ids: Vec::new(),
    })
}

fn channel_message_ids(
    conn: &rusqlite::Connection,
    queued_id: Uuid,
) -> rusqlite::Result<Vec<Uuid>> {
    let mut stmt =
        conn.prepare("SELECT message_id FROM turn_queue_channel_messages WHERE queued_id = ?1")?;
    let rows = stmt.query_map(params![queued_id.to_string()], |row| {
        parse_uuid(&row.get::<_, String>(0)?)
    })?;
    rows.collect()
}

#[async_trait]
impl TurnQueueStore for SqliteStorage {
    async fn enqueue_turn_input(
        &self,
        input: NewQueuedTurnInput,
    ) -> Result<QueuedTurnInput, StorageError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let id = Uuid::new_v4();
            let now = Utc::now();
            let attachments = serde_json::to_string(&input.attachments)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            // The sequence is allocated inside the connection's lock, so two
            // arrivals never share a place in the queue.
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq) + 1, 0) FROM turn_queue WHERE session_id = ?1",
                params![input.session_id.to_string()],
                |row| row.get(0),
            )?;
            tx.execute(
                &format!(
                    "INSERT INTO turn_queue ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
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
                    input.schedule_run_id.map(|id| id.to_string()),
                    input.attended,
                    input.client_id.map(|id| id.to_string()),
                ],
            )?;
            for message_id in &input.channel_message_ids {
                let claimed = tx.execute(
                    "INSERT INTO turn_queue_channel_messages (queued_id, message_id)
                     SELECT ?1, id FROM channel_inbound_messages WHERE id = ?2 AND state = 'pending'",
                    params![id.to_string(), message_id.to_string()],
                )?;
                if claimed != 1 {
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }
            }
            tx.commit()?;
            Ok(QueuedTurnInput {
                attended: input.attended,
                client_id: input.client_id,
                id,
                session_id: input.session_id,
                thread_id: input.thread_id,
                seq,
                origin: input.origin,
                content: input.content,
                attachments: input.attachments,
                created_at: now,
                schedule_run_id: input.schedule_run_id,
                channel_message_ids: input.channel_message_ids,
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
            let mut queued = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            for item in &mut queued {
                item.channel_message_ids = channel_message_ids(conn, item.id)?;
            }
            Ok(queued)
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
            let mut queued = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLUMNS} FROM turn_queue WHERE session_id = ?1 ORDER BY seq ASC"
                ))?;
                let rows = stmt.query_map(params![session_id.to_string()], row_to_queued)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for item in &mut queued {
                item.channel_message_ids = channel_message_ids(&tx, item.id)?;
            }
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
            for (index, item) in queued.iter().enumerate() {
                let recorded_at = now + chrono::Duration::nanoseconds(index as i64);
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
                        recorded_at.to_rfc3339(),
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
                    created_at: recorded_at,
                });
            }
            let mut schedule_run_ids = Vec::new();
            let mut channel_origins = Vec::new();
            for item in &queued {
                if let Some(run_id) = item.schedule_run_id {
                    tx.execute("UPDATE schedule_runs SET state = 'running', turn_id = ?2, started_at = ?3 WHERE id = ?1 AND state = 'pending'",
                        params![run_id.to_string(), turn_id.to_string(), &now_str])?;
                    schedule_run_ids.push(run_id);
                }
                for message_id in &item.channel_message_ids {
                    let origin = tx.query_row("SELECT integration, account, conversation FROM channel_inbound_messages WHERE id = ?1",
                        params![message_id.to_string()], |row| Ok(crate::model::PendingChannelOrigin {
                            integration: row.get(0)?, account: row.get(1)?, conversation: row.get(2)?,
                        }))?;
                    if !channel_origins.iter().any(|existing: &crate::model::PendingChannelOrigin|
                        existing.integration == origin.integration && existing.account == origin.account && existing.conversation == origin.conversation) {
                        channel_origins.push(origin);
                    }
                    tx.execute("UPDATE channel_inbound_messages SET state = 'presented', turn_id = ?2, presented_at = ?3 WHERE id = ?1 AND state = 'pending'",
                        params![message_id.to_string(), turn_id.to_string(), &now_str])?;
                }
            }
            tx.execute(
                "DELETE FROM turn_queue WHERE session_id = ?1",
                params![session_id.to_string()],
            )?;
            tx.commit()?;
            let queued_ids = queued.iter().map(|item| item.id).collect();
            Ok(Some(DrainedTurnQueue {
                attended: queued.iter().any(|input| input.attended),
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
                    failure_message: None,
                },
                messages,
                queued_ids,
                schedule_run_ids,
                channel_origins,
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
        session_and_thread_in(SqliteStorage::in_memory().unwrap()).await
    }

    async fn session_and_thread_in(storage: SqliteStorage) -> (SqliteStorage, Uuid, Uuid) {
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
            attended: true,
            client_id: None,
            session_id,
            thread_id,
            origin: MessageOrigin::User,
            content: content.into(),
            attachments: Vec::new(),
            schedule_run_id: None,
            channel_message_ids: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queue_attendance_and_client_identity_survive_reopening() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("queue.db");
        let (storage, session_id, thread_id) =
            session_and_thread_in(SqliteStorage::open(&database).unwrap()).await;
        let held = storage
            .enqueue_turn_input(NewQueuedTurnInput {
                attended: false,
                client_id: Some(Uuid::new_v4()),
                ..queued(session_id, thread_id, "unattended send")
            })
            .await
            .unwrap();
        drop(storage);
        let reopened = SqliteStorage::open(&database).unwrap();
        let listed = reopened.list_queued_turn_inputs(session_id).await.unwrap();
        let drained = reopened
            .drain_queued_turn_inputs(session_id, "gateway")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (listed, drained.attended),
            (vec![held], false),
            "a reopened queue must retain its client's identity and source attendance through the drain"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_schedule_cancels_its_queued_brief_only() {
        use crate::model::{
            NewSchedule, NewScheduleRun, ScheduleCreator, ScheduleKind, ScheduleRunTrigger,
            ScheduleStatus, SessionPolicy,
        };
        use crate::store::{ScheduleStore, SessionStore};
        let (storage, session_id, thread_id) = session_and_thread().await;
        let topic_id = storage
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap()
            .topic_id;
        let schedule = storage
            .create_schedule(NewSchedule {
                id: Uuid::new_v4(),
                message: "scheduled brief".into(),
                status: ScheduleStatus::Active,
                kind: ScheduleKind::Interval,
                expression: "every 5 seconds".into(),
                session_policy: SessionPolicy::Fresh {
                    topic_id: Some(topic_id),
                    agent_id: "gateway".into(),
                },
                creator: ScheduleCreator::User,
                delivery_routing: serde_json::json!({}),
                gate: None,
                next_run_at: None,
                last_run_at: None,
            })
            .await
            .unwrap();
        let run = storage
            .create_schedule_run(NewScheduleRun::pending(
                schedule.id,
                ScheduleRunTrigger::Tick,
                None,
                "the brief",
            ))
            .await
            .unwrap();
        storage
            .enqueue_turn_input(NewQueuedTurnInput {
                schedule_run_id: Some(run.id),
                origin: MessageOrigin::Schedule,
                ..queued(session_id, thread_id, "scheduled brief")
            })
            .await
            .unwrap();
        storage
            .enqueue_turn_input(queued(session_id, thread_id, "person message"))
            .await
            .unwrap();
        let deleted = storage.delete_schedule(schedule.id).await;
        let remaining = storage.list_queued_turn_inputs(session_id).await.unwrap();
        assert_eq!(
            (
                deleted.is_ok(),
                storage.get_schedule_run(run.id).await.unwrap().is_none(),
                remaining
                    .iter()
                    .map(|input| input.content.as_str())
                    .collect::<Vec<_>>()
            ),
            (true, true, vec!["person message"]),
            "schedule deletion must succeed, cancelling its run and brief while retaining a person's queued input: {deleted:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drained_rows_have_ordered_instants_in_storage_and_the_announcement() {
        use crate::store::MessageStore;

        let (storage, session_id, thread_id) = session_and_thread().await;
        for content in ["first", "second", "third"] {
            storage
                .enqueue_turn_input(queued(session_id, thread_id, content))
                .await
                .unwrap();
        }
        let drained = storage
            .drain_queued_turn_inputs(session_id, "gateway")
            .await
            .unwrap()
            .unwrap();
        assert!(
            drained
                .messages
                .windows(2)
                .all(|pair| pair[0].created_at < pair[1].created_at),
            "each drained input must have its own ordered instant"
        );
        let recorded = MessageStore::list_messages_by_thread(&storage, thread_id)
            .await
            .unwrap();
        assert_eq!(
            recorded
                .iter()
                .map(|row| (row.id, row.created_at))
                .collect::<Vec<_>>(),
            drained
                .messages
                .iter()
                .map(|row| (row.id, row.created_at))
                .collect::<Vec<_>>(),
            "announced instants must be the instants committed in the transaction"
        );
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
