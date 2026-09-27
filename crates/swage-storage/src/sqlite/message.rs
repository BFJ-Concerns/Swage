use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;
use uuid::Uuid;

use swage_core::{Role, StorageError};

use crate::model::{AppendMessage, Message, NewMessage};
use crate::sqlite::SqliteStorage;
use crate::store::MessageStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// Column order: id, thread_id, turn_id, role, content, position, agent_id,
/// tool_call_id, tool_name, created_at, attachments (11 columns, matching
/// every SELECT).
fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let id: String = row.get(0)?;
    let thread_id: String = row.get(1)?;
    let turn_id: Option<String> = row.get(2)?;
    let created: String = row.get(9)?;
    let attachments: String = row.get(10)?;

    let role: String = row.get(3)?;

    Ok(Message {
        id: parse_uuid(&id)?,
        thread_id: parse_uuid(&thread_id)?,
        turn_id: turn_id.as_deref().map(parse_uuid).transpose()?,
        role: Role::from_str(&role).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(err))
        })?,
        content: row.get(4)?,
        attachments: parse_attachments(&attachments)?,
        position: row.get(5)?,
        agent_id: row.get(6)?,
        tool_call_id: row.get(7)?,
        tool_name: row.get(8)?,
        created_at: parse_dt(&created)?,
    })
}

/// The stored attachment array. Storage holds the parts as opaque JSON; a
/// column that is not a JSON array is corruption, reported as such rather
/// than read as "no attachments".
fn parse_attachments(stored: &str) -> rusqlite::Result<Vec<serde_json::Value>> {
    serde_json::from_str(stored).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn encode_attachments(attachments: &[serde_json::Value]) -> rusqlite::Result<String> {
    serde_json::to_string(attachments)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

// ---------------------------------------------------------------------------
// MessageStore
// ---------------------------------------------------------------------------

#[async_trait]
impl MessageStore for SqliteStorage {
    async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO messages
                     (id, thread_id, turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at, attachments)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id.to_string(),
                    message.thread_id.to_string(),
                    message.turn_id.map(|turn_id| turn_id.to_string()),
                    message.role.to_string(),
                    &message.content,
                    message.position,
                    &message.agent_id,
                    Option::<&str>::None, // tool_call_id — not in NewMessage; callers update separately
                    Option::<&str>::None, // tool_name — same
                    &now_str,
                    encode_attachments(&message.attachments)?,
                ],
            )?;

            Ok(Message {
                id,
                thread_id: message.thread_id,
                attachments: message.attachments,
                turn_id: message.turn_id,
                role: message.role,
                content: message.content,
                position: message.position,
                agent_id: message.agent_id,
                tool_call_id: None,
                tool_name: None,
                created_at: now,
            })
        })
        .await
    }

    async fn append_message(&self, message: AppendMessage) -> Result<Message, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let position = conn.query_row(
                "SELECT COALESCE(MAX(position) + 1, 0)
                 FROM messages
                 WHERE thread_id = ?1",
                params![message.thread_id.to_string()],
                |row| row.get::<_, i32>(0),
            )?;

            conn.execute(
                "INSERT INTO messages
                     (id, thread_id, turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at, attachments)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id.to_string(),
                    message.thread_id.to_string(),
                    message.turn_id.map(|turn_id| turn_id.to_string()),
                    message.role.to_string(),
                    &message.content,
                    position,
                    &message.agent_id,
                    Option::<&str>::None,
                    Option::<&str>::None,
                    &now_str,
                    encode_attachments(&message.attachments)?,
                ],
            )?;

            Ok(Message {
                id,
                thread_id: message.thread_id,
                attachments: message.attachments,
                turn_id: message.turn_id,
                role: message.role,
                content: message.content,
                position,
                agent_id: message.agent_id,
                tool_call_id: None,
                tool_name: None,
                created_at: now,
            })
        })
        .await
    }

    async fn list_messages_by_thread(&self, thread_id: Uuid) -> Result<Vec<Message>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, thread_id, turn_id, role, content, position, agent_id,
                        tool_call_id, tool_name, created_at, attachments
                 FROM messages
                 WHERE thread_id = ?1
                 ORDER BY position ASC",
            )?;
            let rows = stmt.query_map(params![thread_id.to_string()], row_to_message)?;
            rows.collect()
        })
        .await
    }

    async fn get_latest_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Message>, StorageError> {
        // Join through threads to locate the session's messages, then take the
        // most recently created one (ties broken by descending position).
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT m.id, m.thread_id, m.turn_id, m.role, m.content, m.position, m.agent_id,
                        m.tool_call_id, m.tool_name, m.created_at, m.attachments
                 FROM messages m
                 JOIN threads t ON m.thread_id = t.id
                 WHERE t.session_id = ?1
                 ORDER BY m.created_at DESC, m.position DESC
                 LIMIT 1",
                params![session_id.to_string()],
                row_to_message,
            )
            .optional()
        })
        .await
    }

    async fn get_message(&self, id: Uuid) -> Result<Option<Message>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, thread_id, turn_id, role, content, position, agent_id,
                        tool_call_id, tool_name, created_at, attachments
                 FROM messages
                 WHERE id = ?1",
                params![id.to_string()],
                row_to_message,
            )
            .optional()
        })
        .await
    }

    async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM messages WHERE thread_id = ?1",
                params![thread_id.to_string()],
            )?;
            Ok(())
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rusqlite::params;
    use swage_core::Role;
    use uuid::Uuid;

    use crate::model::{NewMessage, NewSession, NewTopic};
    use crate::store::{MessageStore, SessionStore, TopicStore};

    use super::*;

    // Create a topic → session → thread chain.
    //
    // Threads are inserted via raw SQL because the ThreadStore SQLite
    // implementation lives in a separate task and may not be wired yet.
    async fn make_thread() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "p".into(),
                slug: "p".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "test-agent".to_string(),
                ..crate::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();

        let thread_id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
                     VALUES (?1, ?2, 'test-agent', ?3, ?3)",
                    params![thread_id.to_string(), session.id.to_string(), &now],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        (storage, session.id, thread_id)
    }

    fn new_msg(thread_id: Uuid, role: Role, content: &str, position: i32) -> NewMessage {
        NewMessage {
            thread_id,
            attachments: Vec::new(),
            turn_id: None,
            role,
            content: content.into(),
            position,
            agent_id: "test-agent".to_string(),
        }
    }

    fn append_msg(thread_id: Uuid, role: Role, content: &str) -> AppendMessage {
        AppendMessage {
            thread_id,
            attachments: Vec::new(),
            turn_id: None,
            role,
            content: content.into(),
            agent_id: "test-agent".to_string(),
        }
    }

    // --- attachments ---

    #[tokio::test(flavor = "multi_thread")]
    async fn append_stores_attachment_values_opaquely_and_reads_them_back() {
        let (storage, _, thread_id) = make_thread().await;
        let mut message = append_msg(thread_id, Role::User, "what is this?");
        message.attachments =
            vec![serde_json::json!({"kind": "image_url", "url": "https://example.test/cat.png"})];

        let appended = storage.append_message(message).await.unwrap();
        assert_eq!(appended.attachments.len(), 1);

        let listed = storage.list_messages_by_thread(thread_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].attachments,
            vec![serde_json::json!({"kind": "image_url", "url": "https://example.test/cat.png"})]
        );
        let fetched = storage.get_message(appended.id).await.unwrap().unwrap();
        assert_eq!(fetched.attachments, listed[0].attachments);

        let plain = storage
            .append_message(append_msg(thread_id, Role::Assistant, "a cat"))
            .await
            .unwrap();
        assert!(plain.attachments.is_empty());
    }

    // --- create_message and list_by_thread ---

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_list_by_thread() {
        let (storage, _, thread_id) = make_thread().await;

        storage
            .create_message(new_msg(thread_id, Role::User, "hello", 0))
            .await
            .unwrap();
        storage
            .create_message(new_msg(thread_id, Role::Assistant, "hi", 1))
            .await
            .unwrap();

        let msgs = storage.list_messages_by_thread(thread_id).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].position, 0);
        assert_eq!(msgs[1].position, 1);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_thread_returns_empty_for_unknown_thread() {
        let (storage, _, _) = make_thread().await;
        let msgs = storage
            .list_messages_by_thread(Uuid::new_v4())
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_thread_orders_by_position_ascending() {
        let (storage, _, thread_id) = make_thread().await;

        // Insert out of position order to verify the ORDER BY clause works.
        storage
            .create_message(new_msg(thread_id, Role::Assistant, "second", 1))
            .await
            .unwrap();
        storage
            .create_message(new_msg(thread_id, Role::User, "first", 0))
            .await
            .unwrap();

        let msgs = storage.list_messages_by_thread(thread_id).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "second");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_message_assigns_monotonic_positions() {
        let (storage, _, thread_id) = make_thread().await;

        let first = storage
            .append_message(append_msg(thread_id, Role::User, "first"))
            .await
            .unwrap();
        let second = storage
            .append_message(append_msg(thread_id, Role::Assistant, "second"))
            .await
            .unwrap();

        assert_eq!(first.position, 0);
        assert_eq!(second.position, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_message_continues_after_existing_highest_position() {
        let (storage, _, thread_id) = make_thread().await;

        storage
            .create_message(new_msg(thread_id, Role::User, "seed", 7))
            .await
            .unwrap();

        let appended = storage
            .append_message(append_msg(thread_id, Role::Assistant, "next"))
            .await
            .unwrap();

        assert_eq!(appended.position, 8);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_message_position_in_thread_is_rejected() {
        let (storage, _, thread_id) = make_thread().await;

        storage
            .create_message(new_msg(thread_id, Role::User, "first", 0))
            .await
            .unwrap();

        let err = storage
            .create_message(new_msg(thread_id, Role::Assistant, "collision", 0))
            .await
            .unwrap_err();

        match err {
            StorageError::ConstraintViolation(message) => {
                assert!(
                    message.contains("UNIQUE") || message.contains("idx_messages_thread_position"),
                    "unexpected constraint message: {message}"
                );
            }
            other => panic!("expected constraint violation, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_append_message_assigns_unique_positions() {
        let (storage, _, thread_id) = make_thread().await;
        let storage = std::sync::Arc::new(storage);

        let mut tasks = Vec::new();
        for i in 0..12 {
            let storage = storage.clone();
            tasks.push(tokio::spawn(async move {
                storage
                    .append_message(append_msg(thread_id, Role::Assistant, &format!("m{i}")))
                    .await
                    .expect("append should succeed")
                    .position
            }));
        }

        let mut positions = Vec::new();
        for task in tasks {
            positions.push(task.await.unwrap());
        }
        positions.sort_unstable();

        assert_eq!(positions, (0..12).collect::<Vec<_>>());
    }

    // --- get_message ---

    #[tokio::test(flavor = "multi_thread")]
    async fn get_message_by_id_returns_message() {
        let (storage, _, thread_id) = make_thread().await;

        let created = storage
            .create_message(new_msg(thread_id, Role::User, "hi", 0))
            .await
            .unwrap();
        let fetched = storage
            .get_message(created.id)
            .await
            .unwrap()
            .expect("should exist");

        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.thread_id, thread_id);
        assert_eq!(fetched.role, Role::User);
        assert_eq!(fetched.content, "hi");
        assert_eq!(fetched.agent_id, "test-agent");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_message_by_id_returns_none_for_unknown() {
        let (storage, _, _) = make_thread().await;
        let result = storage.get_message(Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
    }

    // --- delete_by_thread ---

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_by_thread_removes_all_messages() {
        let (storage, _, thread_id) = make_thread().await;

        storage
            .create_message(new_msg(thread_id, Role::User, "a", 0))
            .await
            .unwrap();
        storage
            .create_message(new_msg(thread_id, Role::Assistant, "b", 1))
            .await
            .unwrap();

        storage.delete_by_thread(thread_id).await.unwrap();

        let msgs = storage.list_messages_by_thread(thread_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "all messages should be gone after delete_by_thread"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_by_thread_is_scoped_to_thread() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "p".into(),
                slug: "p".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "a".to_string(),
                ..crate::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();

        // Create two threads in the same session.
        let now = Utc::now().to_rfc3339();
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
                     VALUES (?1, ?2, 'a', ?3, ?3), (?4, ?2, 'a', ?3, ?3)",
                    params![t1.to_string(), session.id.to_string(), &now, t2.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        storage
            .create_message(new_msg(t1, Role::User, "in t1", 0))
            .await
            .unwrap();
        storage
            .create_message(new_msg(t2, Role::User, "in t2", 0))
            .await
            .unwrap();

        storage.delete_by_thread(t1).await.unwrap();

        assert!(
            storage
                .list_messages_by_thread(t1)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(storage.list_messages_by_thread(t2).await.unwrap().len(), 1);
    }

    // --- get_latest_for_session ---

    #[tokio::test(flavor = "multi_thread")]
    async fn get_latest_for_session_returns_last_message() {
        let (storage, session_id, thread_id) = make_thread().await;

        storage
            .create_message(new_msg(thread_id, Role::User, "first", 0))
            .await
            .unwrap();
        storage
            .create_message(new_msg(thread_id, Role::Assistant, "last", 1))
            .await
            .unwrap();

        let latest = storage
            .get_latest_for_session(session_id)
            .await
            .unwrap()
            .expect("should exist");

        assert_eq!(latest.content, "last");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_latest_for_empty_session_returns_none() {
        let (storage, session_id, _) = make_thread().await;
        let result = storage.get_latest_for_session(session_id).await.unwrap();
        assert!(result.is_none());
    }

    // --- tool_call_id and tool_name round-trip ---

    #[tokio::test(flavor = "multi_thread")]
    async fn message_tool_fields_are_null_on_create() {
        // NewMessage has no tool fields; they default to NULL.
        let (storage, _, thread_id) = make_thread().await;
        let msg = storage
            .create_message(new_msg(thread_id, Role::User, "plain", 0))
            .await
            .unwrap();

        assert!(msg.tool_call_id.is_none(), "tool_call_id should be None");
        assert!(msg.tool_name.is_none(), "tool_name should be None");

        let fetched = storage.get_message(msg.id).await.unwrap().unwrap();
        assert!(fetched.tool_call_id.is_none());
        assert!(fetched.tool_name.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_stored_role_is_rejected_by_schema() {
        let (storage, _, thread_id) = make_thread().await;
        let now = Utc::now().to_rfc3339();
        let error = storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO messages (id, thread_id, turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at)
                     VALUES (?1, ?2, NULL, 'bogus', 'oops', 0, 'test-agent', NULL, NULL, ?3)",
                    params![Uuid::new_v4().to_string(), thread_id.to_string(), now],
                )?;
                Ok(())
            })
            .await
            .expect_err("invalid stored role should fail closed");

        assert!(
            error
                .to_string()
                .contains("messages.role must be one of system, user, assistant, tool")
        );
    }
}
