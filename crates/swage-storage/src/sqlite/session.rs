use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewSession, Session, SessionActivity, SessionListing, SessionOrigin};
use crate::sqlite::SqliteStorage;
use crate::store::SessionStore;

/// The session columns every read selects, in `row_to_session` order.
/// Table-qualified so the activity join can add its own `created_at`.
const SESSION_COLUMNS: &str = "sessions.id, sessions.topic_id, sessions.agent_id, \
    sessions.metadata_json, sessions.selected_model_id, \
    sessions.active_workspace_attachment_id, sessions.created_at, sessions.updated_at, \
    sessions.origin_kind, sessions.origin_source_id, sessions.archived_at";

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

pub(crate) fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let id: String = row.get(0)?;
    let topic_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let metadata_json: Option<String> = row.get(3)?;
    let selected_model_id: Option<String> = row.get(4)?;
    let active_workspace_attachment_id: Option<String> = row.get(5)?;
    let created: String = row.get(6)?;
    let updated: String = row.get(7)?;
    let origin_kind: String = row.get(8)?;
    let origin_source_id: Option<String> = row.get(9)?;
    let archived_at: Option<String> = row.get(10)?;

    let origin = SessionOrigin::from_columns(
        &origin_kind,
        origin_source_id.as_deref().map(parse_uuid).transpose()?,
    )
    .map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, error.into())
    })?;

    Ok(Session {
        id: parse_uuid(&id)?,
        topic_id: parse_uuid(&topic_id)?,
        agent_id,
        origin,
        metadata: parse_metadata(metadata_json.as_deref())?,
        selected_model_id,
        active_workspace_attachment_id: active_workspace_attachment_id
            .as_deref()
            .map(parse_uuid)
            .transpose()?,
        archived_at: archived_at.as_deref().map(parse_dt).transpose()?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

/// A session row joined with its latest user-or-assistant message.
fn row_to_session_activity(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionActivity> {
    let session = row_to_session(row)?;
    let last_activity: String = row.get(11)?;
    let latest_message: Option<String> = row.get(12)?;
    let has_interrupted_turn: bool = row.get(13)?;
    Ok(SessionActivity {
        last_activity: parse_dt(&last_activity)?,
        latest_message,
        has_interrupted_turn,
        session,
    })
}

/// The activity columns appended to `SESSION_COLUMNS` for the recent list:
/// the latest user-or-assistant message's time and text (creation time and
/// no text for a session with none), and whether any turn was interrupted.
/// Tool and system messages are excluded so the list reflects what a person
/// or the agent last said, not bookkeeping.
const SESSION_ACTIVITY_COLUMNS: &str = "
    COALESCE(latest.created_at, sessions.created_at) AS last_activity,
    latest.content AS latest_message,
    EXISTS (
        SELECT 1 FROM turns
        JOIN threads ON threads.id = turns.thread_id
        WHERE threads.session_id = sessions.id AND turns.state = 'interrupted'
    ) AS has_interrupted_turn";

const SESSION_ACTIVITY_JOIN: &str = "
    LEFT JOIN (
        SELECT threads.session_id, messages.created_at, messages.content,
               ROW_NUMBER() OVER (
                   PARTITION BY threads.session_id
                   ORDER BY messages.created_at DESC, messages.position DESC
               ) AS recency
        FROM messages
        JOIN threads ON threads.id = messages.thread_id
        WHERE messages.role IN ('user', 'assistant')
    ) AS latest ON latest.session_id = sessions.id AND latest.recency = 1";

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

fn parse_metadata(metadata_json: Option<&str>) -> rusqlite::Result<Option<serde_json::Value>> {
    metadata_json
        .map(|json| {
            serde_json::from_str(json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn serialise_metadata(
    metadata: Option<&serde_json::Value>,
) -> Result<Option<String>, rusqlite::Error> {
    metadata
        .map(|value| {
            serde_json::to_string(value)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
        })
        .transpose()
}

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for SqliteStorage {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError> {
        let metadata_json = serialise_metadata(session.metadata.as_ref())
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO sessions (id, topic_id, agent_id, metadata_json, active_workspace_attachment_id, created_at, updated_at, origin_kind, origin_source_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
                params![
                    id.to_string(),
                    session.topic_id.to_string(),
                    &session.agent_id,
                    &metadata_json,
                    session
                        .active_workspace_attachment_id
                        .map(|id| id.to_string()),
                    &now_str,
                    session.origin.kind(),
                    session.origin.source_id().map(|id| id.to_string()),
                ],
            )?;

            Ok(Session {
                id,
                topic_id: session.topic_id,
                agent_id: session.agent_id,
                origin: session.origin,
                metadata: session.metadata,
                selected_model_id: None,
                active_workspace_attachment_id: session.active_workspace_attachment_id,
                archived_at: None,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
                params![id.to_string()],
                row_to_session,
            )
            .optional()
        })
        .await
    }

    async fn list_by_topic(&self, topic_id: Uuid) -> Result<Vec<Session>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions WHERE topic_id = ?1
                 ORDER BY updated_at DESC"
            ))?;
            let rows = stmt.query_map(params![topic_id.to_string()], row_to_session)?;
            rows.collect()
        })
        .await
    }

    async fn list_sessions_by_agent_id(
        &self,
        agent_id: &str,
    ) -> Result<Vec<Session>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions WHERE agent_id = ?1
                 ORDER BY updated_at DESC"
            ))?;
            let rows = stmt.query_map(params![&agent_id], row_to_session)?;
            rows.collect()
        })
        .await
    }

    async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(|conn| {
            let affected = conn.execute(
                "DELETE FROM sessions WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    async fn update_activity(&self, id: Uuid) -> Result<(), StorageError> {
        self.with_conn(|conn| {
            let now_str = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![&now_str, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn list_recent_sessions(
        &self,
        listing: SessionListing,
    ) -> Result<Vec<SessionActivity>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SESSION_COLUMNS}, {SESSION_ACTIVITY_COLUMNS}
                 FROM sessions {SESSION_ACTIVITY_JOIN}
                 WHERE (?1 IS NULL OR sessions.topic_id = ?1)
                   AND (?3 OR (sessions.archived_at IS NULL) = (?2 = 0))
                 ORDER BY last_activity DESC, sessions.created_at DESC"
            ))?;
            let rows = stmt.query_map(
                params![
                    listing.topic_id.map(|id| id.to_string()),
                    listing.archived,
                    listing.include_archived
                ],
                row_to_session_activity,
            )?;
            rows.collect()
        })
        .await
    }

    async fn get_session_activity(
        &self,
        id: Uuid,
    ) -> Result<Option<SessionActivity>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS}, {SESSION_ACTIVITY_COLUMNS}
                     FROM sessions {SESSION_ACTIVITY_JOIN}
                     WHERE sessions.id = ?1"
                ),
                params![id.to_string()],
                row_to_session_activity,
            )
            .optional()
        })
        .await
    }

    async fn archive_inactive_sessions(
        &self,
        inactive_before: DateTime<Utc>,
        exempt: &[Uuid],
    ) -> Result<Vec<Uuid>, StorageError> {
        let exempt: Vec<String> = exempt.iter().map(Uuid::to_string).collect();
        self.with_conn(move |conn| {
            let exempt_json = serde_json::to_string(&exempt)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let now_str = Utc::now().to_rfc3339();
            let mut stmt = conn.prepare(&format!(
                "UPDATE sessions SET archived_at = ?1
                 WHERE archived_at IS NULL
                   AND id NOT IN (SELECT value FROM json_each(?2))
                   AND id IN (
                       SELECT sessions.id FROM sessions {SESSION_ACTIVITY_JOIN}
                       WHERE COALESCE(latest.created_at, sessions.created_at) < ?3
                   )
                 RETURNING id"
            ))?;
            let rows = stmt.query_map(
                params![now_str, exempt_json, inactive_before.to_rfc3339()],
                |row| {
                    let id: String = row.get(0)?;
                    parse_uuid(&id)
                },
            )?;
            rows.collect()
        })
        .await
    }

    async fn unarchive_session(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "UPDATE sessions SET archived_at = NULL WHERE id = ?1 AND archived_at IS NOT NULL",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    async fn set_session_metadata(
        &self,
        id: Uuid,
        metadata: Option<serde_json::Value>,
    ) -> Result<Session, StorageError> {
        let metadata_json = serialise_metadata(metadata.as_ref())
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        let affected = self
            .with_conn(move |conn| {
                let now_str = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE sessions SET metadata_json = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id.to_string(), metadata_json, now_str],
                )
            })
            .await?;
        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            });
        }
        self.get_session(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })
    }

    async fn set_session_active_workspace(
        &self,
        id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<Session, StorageError> {
        let affected = self
            .with_conn(move |conn| {
                let now_str = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE sessions
                     SET active_workspace_attachment_id = ?2, updated_at = ?3
                     WHERE id = ?1",
                    params![
                        id.to_string(),
                        workspace_id.map(|workspace_id| workspace_id.to_string()),
                        now_str
                    ],
                )
            })
            .await?;
        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            });
        }
        self.get_session(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })
    }

    async fn set_session_selected_model(
        &self,
        id: Uuid,
        selected_model_id: Option<String>,
    ) -> Result<Session, StorageError> {
        let affected = self
            .with_conn(move |conn| {
                let now_str = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE sessions
                     SET selected_model_id = ?2, updated_at = ?3
                     WHERE id = ?1",
                    params![id.to_string(), selected_model_id, now_str],
                )
            })
            .await?;
        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            });
        }
        self.get_session(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })
    }

    async fn find_session_by_channel_origin(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<Session>, StorageError> {
        let integration = integration.to_string();
        let account = account.to_string();
        let conversation = conversation.to_string();
        self.with_conn(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions
                     WHERE json_extract(metadata_json, '$.channel_origin.integration') = ?1
                       AND json_extract(metadata_json, '$.channel_origin.account') = ?2
                       AND json_extract(metadata_json, '$.channel_origin.conversation') = ?3
                     ORDER BY updated_at DESC
                     LIMIT 1"
                ),
                params![integration, account, conversation],
                row_to_session,
            )
            .optional()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::fixtures::new_session;
    use crate::model::{Message, NewMessage, NewThread, NewTopic, NewWorkspaceAttachment};
    use crate::store::{TopicStore, WorkspaceAttachmentStore};
    use swage_core::Role;

    use super::*;

    async fn make_storage_with_topic() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "test".into(),
                slug: "test".into(),
            })
            .await
            .unwrap();
        (storage, topic.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_session() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let session = storage.create_session(new_session(topic_id)).await.unwrap();

        assert_eq!(session.topic_id, topic_id);

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, session.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_sessions_ordered_by_updated_at_desc() {
        let (storage, topic_id) = make_storage_with_topic().await;

        let s1 = storage.create_session(new_session(topic_id)).await.unwrap();
        // Bump s1 so it has a later updated_at.
        storage.update_activity(s1.id).await.unwrap();
        let s2 = storage.create_session(new_session(topic_id)).await.unwrap();
        storage.update_activity(s2.id).await.unwrap();

        let sessions = storage.list_by_topic(topic_id).await.unwrap();
        assert_eq!(sessions.len(), 2);
        // Most-recently-updated appears first.
        assert!(sessions[0].updated_at >= sessions[1].updated_at);
    }

    async fn say(storage: &SqliteStorage, session_id: Uuid, role: Role, content: &str) -> Message {
        use crate::store::{MessageStore, ThreadStore};
        let thread = match ThreadStore::list_by_session(storage, session_id)
            .await
            .unwrap()
            .into_iter()
            .next()
        {
            Some(thread) => thread,
            None => ThreadStore::create(
                storage,
                NewThread {
                    session_id,
                    agent_id: "test-agent".to_string(),
                },
            )
            .await
            .unwrap(),
        };
        let position = storage
            .list_messages_by_thread(thread.id)
            .await
            .unwrap()
            .len() as i32;
        storage
            .create_message(NewMessage {
                thread_id: thread.id,
                turn_id: None,
                role,
                content: content.to_string(),
                attachments: Vec::new(),
                position,
                agent_id: "test-agent".to_string(),
            })
            .await
            .unwrap()
    }

    /// Move a message's timestamp so a test can stage inactivity without
    /// waiting for it.
    async fn backdate_message(
        storage: &SqliteStorage,
        message_id: Uuid,
        days: i64,
    ) -> DateTime<Utc> {
        let created_at = Utc::now() - chrono::Duration::days(days);
        let stamp = created_at.to_rfc3339();
        storage
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                    params![stamp, message_id.to_string()],
                )
            })
            .await
            .unwrap();
        created_at
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_records_what_opened_it() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let schedule_id = Uuid::new_v4();
        let session = storage
            .create_session(NewSession {
                origin: SessionOrigin::ScheduleRun { schedule_id },
                ..new_session(topic_id)
            })
            .await
            .unwrap();

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.origin, SessionOrigin::ScheduleRun { schedule_id });
        assert_eq!(fetched.archived_at, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_recent_list_orders_by_the_latest_user_or_assistant_message() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let quiet = storage.create_session(new_session(topic_id)).await.unwrap();
        let spoken = storage.create_session(new_session(topic_id)).await.unwrap();
        let tooled = storage.create_session(new_session(topic_id)).await.unwrap();
        // The quiet session is newest by creation; the spoken one has a user
        // message; the tooled one only a tool message, which does not count.
        let said = say(&storage, spoken.id, Role::User, "hello").await;
        let said_at = backdate_message(&storage, said.id, 1).await;
        say(&storage, tooled.id, Role::Tool, "{}").await;

        let listing = storage
            .list_recent_sessions(SessionListing::default())
            .await
            .unwrap();
        let ids: Vec<Uuid> = listing.iter().map(|entry| entry.session.id).collect();

        assert_eq!(ids, vec![tooled.id, quiet.id, spoken.id]);
        let spoken_entry = &listing[2];
        assert_eq!(spoken_entry.last_activity, said_at);
        assert_eq!(spoken_entry.latest_message.as_deref(), Some("hello"));
        let tooled_entry = &listing[0];
        assert_eq!(tooled_entry.last_activity, tooled.created_at);
        assert_eq!(tooled_entry.latest_message, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_recent_list_spans_every_scope_and_narrows_to_one() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let general = storage.general_scope().await.unwrap();
        let in_topic = storage.create_session(new_session(topic_id)).await.unwrap();
        let in_general = storage
            .create_session(new_session(general.id))
            .await
            .unwrap();

        let everywhere = storage
            .list_recent_sessions(SessionListing::default())
            .await
            .unwrap();
        let mut ids: Vec<Uuid> = everywhere.iter().map(|entry| entry.session.id).collect();
        ids.sort();
        let mut expected = vec![in_topic.id, in_general.id];
        expected.sort();
        assert_eq!(ids, expected);

        let narrowed = storage
            .list_recent_sessions(SessionListing {
                topic_id: Some(topic_id),
                archived: false,
                include_archived: false,
            })
            .await
            .unwrap();
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].session.id, in_topic.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_activity_listing_can_include_live_and_archived_sessions_together() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let live = storage.create_session(new_session(topic_id)).await.unwrap();
        let archived = storage.create_session(new_session(topic_id)).await.unwrap();
        let archived_id = archived.id;
        storage
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE sessions SET archived_at = created_at WHERE id = ?1",
                    [archived_id.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let listed = storage
            .list_recent_sessions(SessionListing {
                topic_id: Some(topic_id),
                include_archived: true,
                ..SessionListing::default()
            })
            .await
            .unwrap();
        let ids: std::collections::HashSet<Uuid> = listed
            .into_iter()
            .map(|activity| activity.session.id)
            .collect();
        assert_eq!(ids, std::collections::HashSet::from([live.id, archived_id]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_archive_sweep_hides_inactive_sessions_and_spares_the_executing_and_the_recent() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let stale = storage.create_session(new_session(topic_id)).await.unwrap();
        let executing = storage.create_session(new_session(topic_id)).await.unwrap();
        let fresh = storage.create_session(new_session(topic_id)).await.unwrap();
        let old = say(&storage, stale.id, Role::Assistant, "long ago").await;
        backdate_message(&storage, old.id, 10).await;
        let old_but_busy = say(&storage, executing.id, Role::Assistant, "long ago too").await;
        backdate_message(&storage, old_but_busy.id, 10).await;
        say(&storage, fresh.id, Role::User, "just now").await;

        let cutoff = Utc::now() - chrono::Duration::days(7);
        let archived = storage
            .archive_inactive_sessions(cutoff, &[executing.id])
            .await
            .unwrap();
        assert_eq!(archived, vec![stale.id]);

        let live: Vec<Uuid> = storage
            .list_recent_sessions(SessionListing::default())
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.session.id)
            .collect();
        assert!(live.contains(&executing.id) && live.contains(&fresh.id));
        assert!(!live.contains(&stale.id));

        let hidden = storage
            .list_recent_sessions(SessionListing {
                topic_id: None,
                archived: true,
                include_archived: false,
            })
            .await
            .unwrap();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].session.id, stale.id);
        assert!(hidden[0].session.archived_at.is_some());

        // A second sweep finds nothing new; archiving is idempotent.
        assert!(
            storage
                .archive_inactive_sessions(cutoff, &[executing.id])
                .await
                .unwrap()
                .is_empty()
        );
        // Once nothing executes on it, the old session goes too.
        assert_eq!(
            storage
                .archive_inactive_sessions(cutoff, &[])
                .await
                .unwrap(),
            vec![executing.id]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_archived_session_returns_when_unarchived() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let session = storage.create_session(new_session(topic_id)).await.unwrap();
        let old = say(&storage, session.id, Role::User, "long ago").await;
        backdate_message(&storage, old.id, 30).await;
        storage
            .archive_inactive_sessions(Utc::now() - chrono::Duration::days(7), &[])
            .await
            .unwrap();
        assert!(
            storage
                .get_session(session.id)
                .await
                .unwrap()
                .unwrap()
                .archived_at
                .is_some()
        );

        assert!(storage.unarchive_session(session.id).await.unwrap());
        assert!(!storage.unarchive_session(session.id).await.unwrap());
        let fetched = storage
            .get_session_activity(session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.session.archived_at, None);
        assert_eq!(fetched.latest_message.as_deref(), Some("long ago"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_session_returns_true_then_false() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let session = storage.create_session(new_session(topic_id)).await.unwrap();

        assert!(storage.delete_session(session.id).await.unwrap());
        assert!(!storage.delete_session(session.id).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_stores_agent_id() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let session = storage
            .create_session(NewSession {
                agent_id: "my-agent".to_string(),
                ..new_session(topic_id)
            })
            .await
            .unwrap();

        assert_eq!(session.agent_id, "my-agent");

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.agent_id, "my-agent");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_round_trips_metadata() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let metadata = serde_json::json!({
            "host": "embedder",
            "labels": ["demo", "cli"]
        });

        let session = storage
            .create_session(NewSession {
                agent_id: "my-agent".to_string(),
                metadata: Some(metadata.clone()),
                ..new_session(topic_id)
            })
            .await
            .unwrap();

        assert_eq!(session.metadata, Some(metadata.clone()));

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.metadata, Some(metadata));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_active_workspace_round_trips_and_updates() {
        let (storage, topic_id) = make_storage_with_topic().await;
        let workspace = storage
            .create_workspace_attachment(NewWorkspaceAttachment {
                topic_id,
                label: "repo".to_string(),
                path: "/tmp/swage-test-repo".to_string(),
                permissions: vec!["read".to_string(), "write".to_string()],
                is_trusted: true,
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "my-agent".to_string(),
                active_workspace_attachment_id: Some(workspace.id),
                ..new_session(topic_id)
            })
            .await
            .unwrap();

        let fetched = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(fetched.active_workspace_attachment_id, Some(workspace.id));

        let updated = storage
            .set_session_active_workspace(session.id, None)
            .await
            .unwrap();
        assert_eq!(updated.active_workspace_attachment_id, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_agent_id_filters_and_orders() {
        let (storage, topic_id) = make_storage_with_topic().await;

        // Create sessions for two different agents.
        let s1 = storage
            .create_session(NewSession {
                agent_id: "alpha".to_string(),
                ..new_session(topic_id)
            })
            .await
            .unwrap();
        let _s2 = storage
            .create_session(NewSession {
                agent_id: "beta".to_string(),
                ..new_session(topic_id)
            })
            .await
            .unwrap();
        let s3 = storage
            .create_session(NewSession {
                agent_id: "alpha".to_string(),
                ..new_session(topic_id)
            })
            .await
            .unwrap();

        // Bump s1 so it's more recently updated than s3.
        storage.update_activity(s1.id).await.unwrap();

        let alpha_sessions = storage.list_sessions_by_agent_id("alpha").await.unwrap();
        assert_eq!(
            alpha_sessions.len(),
            2,
            "should only return alpha's sessions"
        );
        // Most recently updated first.
        assert_eq!(alpha_sessions[0].id, s1.id);
        assert_eq!(alpha_sessions[1].id, s3.id);

        let beta_sessions = storage.list_sessions_by_agent_id("beta").await.unwrap();
        assert_eq!(beta_sessions.len(), 1);

        let none = storage.list_sessions_by_agent_id("gamma").await.unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cascade_delete_removes_sessions() {
        // Deleting a session should also remove its turns via ON DELETE CASCADE.
        let (storage, topic_id) = make_storage_with_topic().await;
        let session = storage.create_session(new_session(topic_id)).await.unwrap();
        storage.delete_session(session.id).await.unwrap();

        assert!(storage.get_session(session.id).await.unwrap().is_none());
    }
}
