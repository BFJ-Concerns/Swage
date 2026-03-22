use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{Message, NewMessage};
use crate::sqlite::SqliteStorage;
use crate::store::MessageStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let id: String = row.get(0)?;
    let turn_id: String = row.get(1)?;
    let created: String = row.get(5)?;

    Ok(Message {
        id: parse_uuid(&id)?,
        turn_id: parse_uuid(&turn_id)?,
        role: row.get(2)?,
        content: row.get(3)?,
        position: row.get(4)?,
        created_at: parse_dt(&created)?,
    })
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
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
                "INSERT INTO messages (id, turn_id, role, content, position, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.to_string(),
                    message.turn_id.to_string(),
                    &message.role,
                    &message.content,
                    message.position,
                    &now_str
                ],
            )?;

            Ok(Message {
                id,
                turn_id: message.turn_id,
                role: message.role,
                content: message.content,
                position: message.position,
                created_at: now,
            })
        })
        .await
    }

    async fn list_messages_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Message>, StorageError> {
        // Join through turns to get messages ordered by turn age then position.
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.turn_id, m.role, m.content, m.position, m.created_at
                 FROM messages m
                 JOIN turns t ON m.turn_id = t.id
                 WHERE t.session_id = ?1
                 ORDER BY t.created_at ASC, m.position ASC",
            )?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_message)?;
            rows.collect()
        })
        .await
    }

    async fn list_messages_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<Message>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, turn_id, role, content, position, created_at
                 FROM messages WHERE turn_id = ?1
                 ORDER BY position ASC",
            )?;
            let rows = stmt.query_map(params![turn_id.to_string()], row_to_message)?;
            rows.collect()
        })
        .await
    }

    async fn get_latest_message_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Message>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT m.id, m.turn_id, m.role, m.content, m.position, m.created_at
                 FROM messages m
                 JOIN turns t ON m.turn_id = t.id
                 WHERE t.session_id = ?1
                 ORDER BY t.created_at DESC, m.position DESC
                 LIMIT 1",
                params![session_id.to_string()],
                row_to_message,
            )
            .optional()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewProject, NewSession, NewTurn, TurnState};
    use crate::store::{ProjectStore, SessionStore, TurnStore};

    use super::*;

    async fn make_turn() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject { name: "p".into(), slug: "p".into(), workspace_path: None })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession { project_id: project.id })
            .await
            .unwrap();
        let turn = storage
            .create_turn(NewTurn { session_id: session.id, state: TurnState::InProgress })
            .await
            .unwrap();
        (storage, session.id, turn.id)
    }

    fn new_msg(turn_id: Uuid, role: &str, content: &str, position: i32) -> NewMessage {
        NewMessage {
            turn_id,
            role: role.into(),
            content: content.into(),
            position,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_list_by_turn() {
        let (storage, _, turn_id) = make_turn().await;

        storage.create_message(new_msg(turn_id, "user", "hello", 0)).await.unwrap();
        storage.create_message(new_msg(turn_id, "assistant", "hi", 1)).await.unwrap();

        let msgs = storage.list_messages_by_turn(turn_id).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].position, 0);
        assert_eq!(msgs[1].position, 1);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_session_orders_by_turn_then_position() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject { name: "p".into(), slug: "p".into(), workspace_path: None })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession { project_id: project.id })
            .await
            .unwrap();

        let turn1 = storage
            .create_turn(NewTurn { session_id: session.id, state: TurnState::InProgress })
            .await
            .unwrap();
        let turn2 = storage
            .create_turn(NewTurn { session_id: session.id, state: TurnState::InProgress })
            .await
            .unwrap();

        // Messages in turn1 then turn2, with positions out-of-order relative to insertion.
        storage.create_message(new_msg(turn1.id, "user", "t1-0", 0)).await.unwrap();
        storage.create_message(new_msg(turn2.id, "user", "t2-0", 0)).await.unwrap();
        storage.create_message(new_msg(turn1.id, "assistant", "t1-1", 1)).await.unwrap();

        let msgs = storage.list_messages_by_session(session.id).await.unwrap();
        assert_eq!(msgs.len(), 3);
        // turn1 messages come first (turn1 created before turn2).
        assert_eq!(msgs[0].content, "t1-0");
        assert_eq!(msgs[1].content, "t1-1");
        assert_eq!(msgs[2].content, "t2-0");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_latest_for_session_returns_last_message() {
        let (storage, session_id, turn_id) = make_turn().await;

        storage.create_message(new_msg(turn_id, "user", "first", 0)).await.unwrap();
        storage.create_message(new_msg(turn_id, "assistant", "last", 1)).await.unwrap();

        let latest = storage
            .get_latest_message_for_session(session_id)
            .await
            .unwrap()
            .expect("should exist");

        assert_eq!(latest.content, "last");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_latest_for_empty_session_returns_none() {
        let (storage, session_id, _) = make_turn().await;
        let result = storage.get_latest_message_for_session(session_id).await.unwrap();
        assert!(result.is_none());
    }
}
