use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use swage_core::{
    StorageError, compact_tool_result_for_model_with_reference, tool_result_reference,
};

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

fn copy_tool_calls_for_branch(
    conn: &rusqlite::Connection,
    source_turn_id: &str,
    child_turn_id: &str,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, call_id, tool, arguments, model_arguments, result, status,
                duration_ms, truncated, total_bytes, provider_round, position, created_at,
                parent_call_id, display_summary
         FROM tool_calls
         WHERE turn_id = ?1
         ORDER BY provider_round ASC, position ASC",
    )?;
    let rows = stmt.query_map(params![source_turn_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, i32>(10)?,
            row.get::<_, i32>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<String>>(14)?,
        ))
    })?;
    let calls = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for (
        agent_id,
        call_id,
        tool,
        arguments,
        model_arguments,
        result,
        status,
        duration_ms,
        truncated,
        total_bytes,
        provider_round,
        position,
        created_at,
        parent_call_id,
        display_summary,
    ) in calls
    {
        let model_result = result.as_deref().map(|result| {
            compact_tool_result_for_model_with_reference(
                result,
                status.as_deref() != Some("success"),
                tool_result_reference(child_turn_id, &call_id),
            )
        });

        conn.execute(
            "INSERT INTO tool_calls
	             (id, turn_id, agent_id, call_id, tool, arguments, model_arguments, result,
	              model_result, status, duration_ms, truncated, total_bytes, provider_round,
	              position, created_at, parent_call_id, display_summary)
	             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                Uuid::new_v4().to_string(),
                child_turn_id,
                agent_id,
                call_id,
                tool,
                arguments,
                model_arguments,
                result,
                model_result,
                status,
                duration_ms,
                truncated,
                total_bytes,
                provider_round,
                position,
                created_at,
                parent_call_id,
                display_summary,
            ],
        )?;
    }

    Ok(())
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

    async fn get_codex_thread_id(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT codex_thread_id FROM threads WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.flatten())
        })
        .await
    }

    async fn set_codex_thread_id(
        &self,
        id: Uuid,
        codex_thread_id: Option<&str>,
    ) -> Result<(), StorageError> {
        if self.get(id).await?.is_none() {
            return Err(StorageError::NotFound {
                entity: "thread".to_string(),
                id: id.to_string(),
            });
        }
        let codex_thread_id = codex_thread_id.map(ToString::to_string);
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE threads SET codex_thread_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.to_string(), codex_thread_id, Utc::now().to_rfc3339()],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_codex_tools_json(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT codex_tools_json FROM threads WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.flatten())
        })
        .await
    }

    async fn set_codex_tools_json(
        &self,
        id: Uuid,
        tools_json: Option<&str>,
    ) -> Result<(), StorageError> {
        let tools_json = tools_json.map(ToString::to_string);
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE threads SET codex_tools_json = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.to_string(), tools_json, Utc::now().to_rfc3339()],
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
            .ok_or_else(|| StorageError::NotFound {
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
            .ok_or_else(|| StorageError::NotFound {
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
                "SELECT turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at, attachments
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
                        row.get::<_, String>(8)?,
                    ))
                },
            )?;
            let messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);

            let turn_ids_with_assistant_messages: HashSet<&str> = messages
                .iter()
                .filter_map(|(turn_id, role, ..)| {
                    (role == "assistant").then_some(turn_id.as_deref()).flatten()
                })
                .collect();
            let mut turn_id_map: HashMap<String, String> = HashMap::new();
            for original_turn_id in messages
                .iter()
                .filter_map(|(turn_id, ..)| turn_id.as_ref())
            {
                if turn_id_map.contains_key(original_turn_id) {
                    continue;
                }

                let child_turn_id = Uuid::new_v4().to_string();
                let turn = conn
                    .query_row(
                        "SELECT agent_id, user_input, inbound_audit, assistant_response,
                                input_tokens, output_tokens, state, created_at, completed_at,
                                cost_microcents, structured_output, parent_turn_id
                         FROM turns WHERE id = ?1",
                        params![original_turn_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, Option<i64>>(4)?,
                                row.get::<_, Option<i64>>(5)?,
                                row.get::<_, String>(6)?,
                                row.get::<_, String>(7)?,
                                row.get::<_, Option<String>>(8)?,
                                row.get::<_, Option<i64>>(9)?,
                                row.get::<_, Option<String>>(10)?,
                                row.get::<_, Option<String>>(11)?,
                            ))
                        },
                    )
                    .optional()?;

                if let Some((
                    agent_id,
                    user_input,
                    inbound_audit,
                    assistant_response,
                    input_tokens,
                    output_tokens,
                    state,
                    created_at,
                    completed_at,
                    cost_microcents,
                    structured_output,
                    parent_turn_id,
                )) = turn
                {
                    conn.execute(
                        "INSERT INTO turns
                         (id, thread_id, agent_id, user_input, inbound_audit, assistant_response,
                          input_tokens, output_tokens, state, created_at, completed_at,
                          cost_microcents, structured_output, parent_turn_id)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                        params![
                            &child_turn_id,
                            child_thread_id.to_string(),
                            agent_id,
                            user_input,
                            inbound_audit,
                            assistant_response,
                            input_tokens,
                            output_tokens,
                            state,
                            created_at,
                            completed_at,
                            cost_microcents,
                            structured_output,
                            parent_turn_id,
                        ],
                    )?;

                    if turn_ids_with_assistant_messages.contains(original_turn_id.as_str()) {
                        copy_tool_calls_for_branch(conn, original_turn_id, &child_turn_id)?;
                    }
                    turn_id_map.insert(original_turn_id.clone(), child_turn_id);
                }
            }

            for row in messages {
                let (
                    turn_id,
                    role,
                    content,
                    position,
                    agent_id,
                    tool_call_id,
                    tool_name,
                    created_at,
                    attachments,
                ) = row;
                let child_turn_id = turn_id
                    .as_ref()
                    .and_then(|turn_id| turn_id_map.get(turn_id))
                    .cloned();
                conn.execute(
                    "INSERT INTO messages (id, thread_id, turn_id, role, content, position, agent_id, tool_call_id, tool_name, created_at, attachments)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        Uuid::new_v4().to_string(),
                        child_thread_id.to_string(),
                        child_turn_id,
                        role,
                        content,
                        position,
                        agent_id,
                        tool_call_id,
                        tool_name,
                        created_at,
                        attachments,
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
    use crate::model::{NewMessage, NewSession, NewSpace, NewToolCall, NewTurn, TurnState};
    use crate::store::{MessageStore, SessionStore, SpaceStore, ToolCallStore, TurnStore};
    use swage_core::{
        Role, compact_tool_arguments_for_model, compact_tool_result_for_model,
        compact_tool_result_for_model_with_reference, tool_result_reference,
    };

    use super::*;

    // Create a storage instance and return a valid (space_id, session_id) pair.
    async fn make_session() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        // Explicit trait qualification: both SpaceStore and ThreadStore define `create`.
        let space = SpaceStore::create(
            &storage,
            NewSpace {
                name: "proj".into(),
                slug: "proj".into(),
            },
        )
        .await
        .unwrap();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "agent-1".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        (storage, space.id, session.id)
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
        let space = SpaceStore::create(
            &storage,
            NewSpace {
                name: "p".into(),
                slug: "p".into(),
            },
        )
        .await
        .unwrap();
        let s1 = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "a".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let s2 = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "a".into(),
                metadata: None,
                active_workspace_attachment_id: None,
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
    async fn create_branch_copies_turns_and_compact_tool_replay() {
        let (storage, _, session_id) = make_session().await;
        let parent = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        let parent_turn = storage
            .create_turn(NewTurn {
                thread_id: parent.id,
                agent_id: "agent-1".into(),
                state: TurnState::Completed,
                user_input: Some("use a tool".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        storage
            .create_message(NewMessage {
                thread_id: parent.id,
                attachments: Vec::new(),
                turn_id: Some(parent_turn.id),
                role: Role::User,
                content: "use a tool".into(),
                position: 0,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();
        let assistant_message = storage
            .create_message(NewMessage {
                thread_id: parent.id,
                attachments: Vec::new(),
                turn_id: Some(parent_turn.id),
                role: Role::Assistant,
                content: "done".into(),
                position: 1,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();
        let expected_model_arguments =
            compact_tool_arguments_for_model(&serde_json::json!({"cmd": "pwd"}));
        storage
            .create_tool_call(NewToolCall {
                turn_id: parent_turn.id,
                agent_id: "agent-1".into(),
                call_id: "call_1".into(),
                tool: "shell".into(),
                arguments: r#"{"cmd":"pwd"}"#.into(),
                model_arguments: expected_model_arguments.clone(),
                result: Some("workspace".into()),
                model_result: Some(compact_tool_result_for_model("workspace", false)),
                status: Some("success".into()),
                duration_ms: Some(12),
                truncated: false,
                total_bytes: Some(9),
                provider_round: 2,
                position: 0,
                parent_call_id: None,
                display_summary: None,
            })
            .await
            .unwrap();

        let branch = storage
            .create_branch(parent.id, assistant_message.id)
            .await
            .unwrap();
        let child_turns = storage
            .list_turns_by_thread(branch.child_thread_id)
            .await
            .unwrap();
        assert_eq!(child_turns.len(), 1);
        assert_ne!(child_turns[0].id, parent_turn.id);
        assert_eq!(child_turns[0].user_input.as_deref(), Some("use a tool"));

        let child_messages = storage
            .list_messages_by_thread(branch.child_thread_id)
            .await
            .unwrap();
        assert_eq!(child_messages.len(), 2);
        assert_eq!(child_messages[0].turn_id, Some(child_turns[0].id));
        assert_eq!(child_messages[1].turn_id, Some(child_turns[0].id));

        let child_tool_calls = storage
            .list_tool_calls_by_turn(child_turns[0].id)
            .await
            .unwrap();
        assert_eq!(child_tool_calls.len(), 1);
        let expected_model_result = compact_tool_result_for_model_with_reference(
            "workspace",
            false,
            tool_result_reference(&child_turns[0].id.to_string(), "call_1"),
        );
        assert_eq!(child_tool_calls[0].call_id, "call_1");
        assert_eq!(
            child_tool_calls[0].model_arguments,
            expected_model_arguments
        );
        assert_eq!(
            child_tool_calls[0].model_result.as_deref(),
            Some(expected_model_result.as_str())
        );
        assert_eq!(child_tool_calls[0].provider_round, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_branch_from_user_message_does_not_copy_future_tool_replay() {
        let (storage, _, session_id) = make_session().await;
        let parent = ThreadStore::create(&storage, new_thread(session_id))
            .await
            .unwrap();
        let parent_turn = storage
            .create_turn(NewTurn {
                thread_id: parent.id,
                agent_id: "agent-1".into(),
                state: TurnState::Completed,
                user_input: Some("use a tool".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let user_message = storage
            .create_message(NewMessage {
                thread_id: parent.id,
                attachments: Vec::new(),
                turn_id: Some(parent_turn.id),
                role: Role::User,
                content: "use a tool".into(),
                position: 0,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();
        storage
            .create_message(NewMessage {
                thread_id: parent.id,
                attachments: Vec::new(),
                turn_id: Some(parent_turn.id),
                role: Role::Assistant,
                content: "done".into(),
                position: 1,
                agent_id: "agent-1".into(),
            })
            .await
            .unwrap();
        storage
            .create_tool_call(NewToolCall {
                turn_id: parent_turn.id,
                agent_id: "agent-1".into(),
                call_id: "call_future".into(),
                tool: "shell".into(),
                arguments: r#"{"cmd":"pwd"}"#.into(),
                model_arguments: compact_tool_arguments_for_model(&serde_json::json!({
                    "cmd": "pwd"
                })),
                result: Some("workspace".into()),
                model_result: Some(compact_tool_result_for_model("workspace", false)),
                status: Some("success".into()),
                duration_ms: Some(12),
                truncated: false,
                total_bytes: Some(9),
                provider_round: 1,
                position: 0,
                parent_call_id: None,
                display_summary: None,
            })
            .await
            .unwrap();

        let branch = storage
            .create_branch(parent.id, user_message.id)
            .await
            .unwrap();
        let child_turns = storage
            .list_turns_by_thread(branch.child_thread_id)
            .await
            .unwrap();
        assert_eq!(child_turns.len(), 1);

        let child_messages = storage
            .list_messages_by_thread(branch.child_thread_id)
            .await
            .unwrap();
        assert_eq!(child_messages.len(), 1);
        assert_eq!(child_messages[0].turn_id, Some(child_turns[0].id));

        let child_tool_calls = storage
            .list_tool_calls_by_turn(child_turns[0].id)
            .await
            .unwrap();
        assert!(
            child_tool_calls.is_empty(),
            "branching before the assistant response must not copy future tool replay"
        );
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
