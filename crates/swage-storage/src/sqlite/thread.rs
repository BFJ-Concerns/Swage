use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewThread, Thread, ThreadBranch, ThreadState};
use crate::sqlite::SqliteStorage;
use crate::store::ThreadStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let state_str: String = row.get(3)?;
    let created: String = row.get(4)?;
    let updated: String = row.get(5)?;

    // ThreadState::from_str returns String on error — wrap it so we can use ? here.
    let state = state_str.parse::<ThreadState>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(StrError(e)),
        )
    })?;

    Ok(Thread {
        id: parse_uuid(&id)?,
        session_id: parse_uuid(&session_id)?,
        agent_id,
        state,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn row_to_branch(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadBranch> {
    let child_thread_id: String = row.get(0)?;
    let parent_thread_id: String = row.get(1)?;
    let branched_from_message_id: String = row.get(2)?;
    let created: String = row.get(3)?;

    Ok(ThreadBranch {
        child_thread_id: parse_uuid(&child_thread_id)?,
        parent_thread_id: parse_uuid(&parent_thread_id)?,
        branched_from_message_id: parse_uuid(&branched_from_message_id)?,
        created_at: parse_dt(&created)?,
    })
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

/// Wraps a bare `String` as a `std::error::Error` for row-mapping conversions.
///
/// `ThreadState::from_str` produces a `String` on error, which cannot be
/// placed directly into `Box<dyn Error>` since `String` doesn't implement
/// `std::error::Error`. This newtype bridges that gap.
#[derive(Debug)]
struct StrError(String);

impl std::fmt::Display for StrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StrError {}

// ---------------------------------------------------------------------------
// ThreadStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ThreadStore for SqliteStorage {
    async fn create(&self, thread: NewThread) -> Result<Thread, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            // New threads always start idle; callers transition state explicitly.
            let state = ThreadState::Idle;

            conn.execute(
                "INSERT INTO threads (id, session_id, agent_id, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    id.to_string(),
                    thread.session_id.to_string(),
                    &thread.agent_id,
                    state.to_string(),
                    &now_str,
                ],
            )?;

            Ok(Thread {
                id,
                session_id: thread.session_id,
                agent_id: thread.agent_id,
                state,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get(&self, id: Uuid) -> Result<Option<Thread>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, session_id, agent_id, state, created_at, updated_at
                 FROM threads WHERE id = ?1",
                params![id.to_string()],
                row_to_thread,
            )
            .optional()
        })
        .await
    }

    async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<Thread>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, state, created_at, updated_at
                 FROM threads WHERE session_id = ?1
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_thread)?;
            rows.collect()
        })
        .await
    }

    async fn list_threads_by_agent_id(&self, agent_id: &str) -> Result<Vec<Thread>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, state, created_at, updated_at
                 FROM threads WHERE agent_id = ?1
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(params![&agent_id], row_to_thread)?;
            rows.collect()
        })
        .await
    }

    async fn update_state(&self, id: Uuid, state: ThreadState) -> Result<(), StorageError> {
        // Pre-flight: fetch current state to validate the transition.
        let current = match self.get(id).await? {
            None => {
                return Err(StorageError::NotFound {
                    entity: "Thread".into(),
                    id: id.to_string(),
                });
            }
            Some(t) => t,
        };

        if !current.state.can_transition_to(&state) {
            return Err(StorageError::ConstraintViolation(format!(
                "invalid thread state transition: {} → {}",
                current.state, state
            )));
        }

        self.with_conn(|conn| {
            let now_str = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE threads SET state = ?1, updated_at = ?2 WHERE id = ?3",
                params![state.to_string(), &now_str, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(|conn| {
            let affected =
                conn.execute("DELETE FROM threads WHERE id = ?1", params![id.to_string()])?;
            Ok(affected > 0)
        })
        .await
    }

    async fn create_branch(
        &self,
        parent_thread_id: Uuid,
        branched_from_message_id: Uuid,
    ) -> Result<ThreadBranch, StorageError> {
        let parent = self
            .get(parent_thread_id)
            .await?
            .ok_or(StorageError::NotFound {
                entity: "Thread".into(),
                id: parent_thread_id.to_string(),
            })?;

        let source_position = self
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT position FROM messages WHERE id = ?1 AND thread_id = ?2",
                    params![
                        branched_from_message_id.to_string(),
                        parent_thread_id.to_string()
                    ],
                    |row| row.get::<_, i32>(0),
                )
                .optional()
            })
            .await?
            .ok_or(StorageError::NotFound {
                entity: "Message".into(),
                id: branched_from_message_id.to_string(),
            })?;

        self.with_conn(move |conn| {
            let child_thread_id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO threads (id, session_id, agent_id, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    child_thread_id.to_string(),
                    parent.session_id.to_string(),
                    &parent.agent_id,
                    ThreadState::Idle.to_string(),
                    &now_str,
                ],
            )?;

            let mut stmt = conn.prepare(
                "SELECT turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at
                 FROM messages
                 WHERE thread_id = ?1 AND position <= ?2
                 ORDER BY position ASC",
            )?;
            let rows = stmt.query_map(
                params![parent_thread_id.to_string(), source_position],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i32>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )?;

            for row in rows {
                let (turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at) = row?;
                conn.execute(
                    "INSERT INTO messages (id, thread_id, turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        Uuid::new_v4().to_string(),
                        child_thread_id.to_string(),
                        turn_id,
                        role,
                        content,
                        position,
                        agent_id,
                        tool_call_id,
                        tool_name,
                        created_at,
                    ],
                )?;
            }

            conn.execute(
                "INSERT INTO thread_branches (child_thread_id, parent_thread_id, branched_from_message_id, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    child_thread_id.to_string(),
                    parent_thread_id.to_string(),
                    branched_from_message_id.to_string(),
                    &now_str,
                ],
            )?;

            Ok(ThreadBranch {
                child_thread_id,
                parent_thread_id,
                branched_from_message_id,
                created_at: now,
            })
        })
        .await
    }

    async fn list_branches_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ThreadBranch>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT tb.child_thread_id, tb.parent_thread_id, tb.branched_from_message_id, tb.created_at
                 FROM thread_branches tb
                 INNER JOIN threads child ON child.id = tb.child_thread_id
                 WHERE child.session_id = ?1
                 ORDER BY tb.created_at ASC",
            )?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_branch)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn get_branch(
        &self,
        child_thread_id: Uuid,
    ) -> Result<Option<ThreadBranch>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT child_thread_id, parent_thread_id, branched_from_message_id, created_at
                 FROM thread_branches WHERE child_thread_id = ?1",
                params![child_thread_id.to_string()],
                row_to_branch,
            )
            .optional()
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::model::NewMessage;
    use crate::model::{NewProject, NewSession};
    use crate::store::{MessageStore, ProjectStore, SessionStore};
    use swage_core::Role;

    use super::*;

    // Create a storage instance and return a valid (project_id, session_id) pair.
    async fn make_session() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        // Explicit trait qualification: both ProjectStore and ThreadStore define `create`.
        let project = ProjectStore::create(
            &storage,
            NewProject {
                name: "proj".into(),
                slug: "proj".into(),
                workspace_path: None,
            },
        )
        .await
        .unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();
        (storage, project.id, session.id)
    }

    fn new_thread(session_id: Uuid) -> NewThread {
        NewThread {
            session_id,
            agent_id: "agent-1".into(),
        }
    }

    // --- CRUD ---

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_thread() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        assert_eq!(thread.session_id, session_id);
        assert_eq!(thread.agent_id, "agent-1");
        assert_eq!(thread.state, ThreadState::Idle);

        let fetched = ThreadStore::get(&storage, thread.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.id, thread.id);
        assert_eq!(fetched.session_id, session_id);
        assert_eq!(fetched.agent_id, "agent-1");
        assert_eq!(fetched.state, ThreadState::Idle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_defaults_to_idle_state() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        assert_eq!(
            thread.state,
            ThreadState::Idle,
            "new thread should start idle"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_returns_none_for_unknown_id() {
        let (storage, _, _) = make_session().await;
        let result = ThreadStore::get(&storage, Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_returns_true_then_false() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        assert!(
            ThreadStore::delete(&storage, thread.id).await.unwrap(),
            "first delete should succeed"
        );
        assert!(
            !ThreadStore::delete(&storage, thread.id).await.unwrap(),
            "second delete should return false"
        );
    }

    // --- Listing order ---

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_session_ordered_by_created_at_asc() {
        let (storage, _, session_id) = make_session().await;
        // Create two threads — ordering must be stable.
        let t1 = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        let t2 = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        let threads = storage.list_by_session(session_id).await.unwrap();
        assert_eq!(threads.len(), 2);
        assert!(
            threads[0].created_at <= threads[1].created_at,
            "list_by_session must be ordered by created_at ASC"
        );
        assert_eq!(threads[0].id, t1.id);
        assert_eq!(threads[1].id, t2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_agent_id_ordered_by_created_at_desc() {
        let (storage, _, session_id) = make_session().await;
        let t1 = ThreadStore::create(
            &storage,
            NewThread {
                session_id,
                agent_id: "agent-x".into(),
            },
        )
        .await
        .unwrap();
        let t2 = ThreadStore::create(
            &storage,
            NewThread {
                session_id,
                agent_id: "agent-x".into(),
            },
        )
        .await
        .unwrap();

        let threads = ThreadStore::list_threads_by_agent_id(&storage, "agent-x")
            .await
            .unwrap();
        assert_eq!(threads.len(), 2);
        // Most-recently created appears first (DESC).
        assert!(
            threads[0].created_at >= threads[1].created_at,
            "list_threads_by_agent_id must be ordered by created_at DESC"
        );
        // t2 was created last so it should be first.
        assert_eq!(threads[0].id, t2.id);
        assert_eq!(threads[1].id, t1.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_by_session_excludes_other_sessions() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = ProjectStore::create(
            &storage,
            NewProject {
                name: "p".into(),
                slug: "p".into(),
                workspace_path: None,
            },
        )
        .await
        .unwrap();
        let s1 = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "a".into(),
            })
            .await
            .unwrap();
        let s2 = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "a".into(),
            })
            .await
            .unwrap();

        ThreadStore::create(
            &storage,
            NewThread {
                session_id: s1.id,
                agent_id: "a".into(),
            },
        )
        .await
        .unwrap();
        ThreadStore::create(
            &storage,
            NewThread {
                session_id: s2.id,
                agent_id: "a".into(),
            },
        )
        .await
        .unwrap();

        let threads = storage.list_by_session(s1.id).await.unwrap();
        assert_eq!(
            threads.len(),
            1,
            "list_by_session should only return threads for that session"
        );
        assert_eq!(threads[0].session_id, s1.id);
    }

    // --- State transitions ---

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_valid_transition() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        // Idle → Processing is a valid transition.
        storage
            .update_state(thread.id, ThreadState::Processing)
            .await
            .unwrap();

        let fetched = ThreadStore::get(&storage, thread.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.state, ThreadState::Processing);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_updates_updated_at() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        let original_updated = thread.updated_at;

        // Small sleep to ensure a measurable time difference.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        storage
            .update_state(thread.id, ThreadState::Processing)
            .await
            .unwrap();

        let fetched = ThreadStore::get(&storage, thread.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            fetched.updated_at > original_updated,
            "update_state should bump updated_at"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_invalid_transition_returns_constraint_violation() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        // Idle → Interrupted is invalid per the state machine.
        let result = storage
            .update_state(thread.id, ThreadState::Interrupted)
            .await;
        assert!(
            matches!(result, Err(StorageError::ConstraintViolation(_))),
            "invalid transition should return ConstraintViolation, got: {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_completed_is_terminal() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        // Idle → Completed.
        storage
            .update_state(thread.id, ThreadState::Completed)
            .await
            .unwrap();

        // Completed → anything must fail.
        for state in [
            ThreadState::Idle,
            ThreadState::Processing,
            ThreadState::Interrupted,
            ThreadState::Completed,
        ] {
            let result = storage.update_state(thread.id, state.clone()).await;
            assert!(
                matches!(result, Err(StorageError::ConstraintViolation(_))),
                "Completed → {state:?} should be rejected"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_not_found_returns_not_found() {
        let (storage, _, _) = make_session().await;
        let result = storage
            .update_state(Uuid::new_v4(), ThreadState::Processing)
            .await;
        assert!(
            matches!(result, Err(StorageError::NotFound { .. })),
            "update on missing thread should return NotFound, got: {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_branch_copies_messages_and_records_lineage() {
        let (storage, _, session_id) = make_session().await;
        let parent = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        let first = storage
            .create_message(NewMessage {
                thread_id: parent.id,
                turn_id: None,
                role: Role::User,
                content: "hello".into(),
                position: 0,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();
        storage
            .create_message(NewMessage {
                thread_id: parent.id,
                turn_id: None,
                role: Role::Assistant,
                content: "world".into(),
                position: 1,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();

        let branch = storage.create_branch(parent.id, first.id).await.unwrap();
        let copied = storage
            .list_messages_by_thread(branch.child_thread_id)
            .await
            .unwrap();
        assert_eq!(copied.len(), 1);
        assert_eq!(copied[0].content, "hello");

        let fetched = storage
            .get_branch(branch.child_thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.parent_thread_id, parent.id);
        assert_eq!(fetched.branched_from_message_id, first.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_branches_by_session_returns_session_branches_only() {
        let (storage, _, session_id) = make_session().await;
        let parent_a = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        let parent_b = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();

        let msg_a = storage
            .create_message(NewMessage {
                thread_id: parent_a.id,
                turn_id: None,
                role: Role::User,
                content: "a".into(),
                position: 0,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();
        let msg_b = storage
            .create_message(NewMessage {
                thread_id: parent_b.id,
                turn_id: None,
                role: Role::User,
                content: "b".into(),
                position: 0,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();

        let branch_a = storage.create_branch(parent_a.id, msg_a.id).await.unwrap();
        let branch_b = storage.create_branch(parent_b.id, msg_b.id).await.unwrap();

        let branches = storage.list_branches_by_session(session_id).await.unwrap();
        assert_eq!(branches.len(), 2);
        assert!(
            branches
                .iter()
                .any(|branch| branch.child_thread_id == branch_a.child_thread_id)
        );
        assert!(
            branches
                .iter()
                .any(|branch| branch.child_thread_id == branch_b.child_thread_id)
        );
    }

    // --- Cascade delete ---

    #[tokio::test(flavor = "multi_thread")]
    async fn cascade_delete_removes_turns() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        let now = Utc::now().to_rfc3339();

        // Insert a turn directly using the new schema (TurnStore not yet updated).
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO turns (id, thread_id, agent_id, state, created_at)
                     VALUES (?1, ?2, 'agent-1', 'pending', ?3)",
                    rusqlite::params![Uuid::new_v4().to_string(), thread.id.to_string(), &now],
                )
            })
            .await
            .unwrap();

        ThreadStore::delete(&storage, thread.id).await.unwrap();

        let turn_count: i64 = storage
            .with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0)))
            .await
            .unwrap();
        assert_eq!(
            turn_count, 0,
            "turns should be cascade-deleted when thread is deleted"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cascade_delete_removes_messages() {
        let (storage, _, session_id) = make_session().await;
        let thread = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        let now = Utc::now().to_rfc3339();

        // Insert a message directly using the new schema (MessageStore not yet updated).
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO messages (id, thread_id, role, content, position, agent_id, created_at)
                     VALUES (?1, ?2, 'user', 'hello', 0, 'agent-1', ?3)",
                    rusqlite::params![Uuid::new_v4().to_string(), thread.id.to_string(), &now],
                )
            })
            .await
            .unwrap();

        ThreadStore::delete(&storage, thread.id).await.unwrap();

        let msg_count: i64 = storage
            .with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)))
            .await
            .unwrap();
        assert_eq!(
            msg_count, 0,
            "messages should be cascade-deleted when thread is deleted"
        );
    }
}
