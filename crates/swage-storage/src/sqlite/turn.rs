use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{InboundAudit, NewTurn, Turn, TurnIsolation, TurnState};
use crate::sqlite::SqliteStorage;
use crate::store::TurnStore;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Wraps a plain-string parse error so it satisfies `Box<dyn Error + Send + Sync>`.
#[derive(Debug)]
struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

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
// Row mapping
// ---------------------------------------------------------------------------

/// Column order: id, thread_id, agent_id, user_input, inbound_audit,
/// assistant_response, input_tokens, output_tokens, state, created_at,
/// completed_at, cost_microcents, structured_output, parent_turn_id,
/// delegated_input_tokens, delegated_output_tokens, isolation_kind,
/// isolation_detail
fn row_to_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    let id: String = row.get(0)?;
    let thread_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let inbound_audit_text: Option<String> = row.get(4)?;
    let inbound_audit = inbound_audit_text
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(ParseError(e.to_string())),
            )
        })?;
    let state_str: String = row.get(8)?;
    let created: String = row.get(9)?;
    let completed: Option<String> = row.get(10)?;
    // structured_output is stored as a JSON text string; deserialise on read.
    let structured_output_text: Option<String> = row.get(12)?;
    let structured_output = structured_output_text
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                Box::new(ParseError(e.to_string())),
            )
        })?;
    let parent_turn_id_str: Option<String> = row.get(13)?;
    let parent_turn_id = parent_turn_id_str.as_deref().map(parse_uuid).transpose()?;

    Ok(Turn {
        id: parse_uuid(&id)?,
        thread_id: parse_uuid(&thread_id)?,
        agent_id,
        state: state_str.parse::<TurnState>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(ParseError(e)),
            )
        })?,
        user_input: row.get(3)?,
        inbound_audit,
        assistant_response: row.get(5)?,
        input_tokens: row.get(6)?,
        output_tokens: row.get(7)?,
        cost_microcents: row.get(11)?,
        structured_output,
        parent_turn_id,
        delegated_input_tokens: row.get(14)?,
        delegated_output_tokens: row.get(15)?,
        execution_isolation: {
            let kind: Option<String> = row.get(16)?;
            let detail: Option<String> = row.get(17)?;
            kind.map(|kind| TurnIsolation::from_columns(&kind, detail))
                .transpose()
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        16,
                        rusqlite::types::Type::Text,
                        Box::new(ParseError(e)),
                    )
                })?
        },
        created_at: parse_dt(&created)?,
        completed_at: completed.as_deref().map(parse_dt).transpose()?,
    })
}

// ---------------------------------------------------------------------------
// TurnStore
// ---------------------------------------------------------------------------

#[async_trait]
impl TurnStore for SqliteStorage {
    async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let state_str = turn.state.to_string();

            let parent_str = turn.parent_turn_id.map(|p| p.to_string());

            conn.execute(
                "INSERT INTO turns (id, thread_id, agent_id, user_input, state, created_at, parent_turn_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id.to_string(),
                    turn.thread_id.to_string(),
                    &turn.agent_id,
                    &turn.user_input,
                    &state_str,
                    &now_str,
                    &parent_str,
                ],
            )?;

            Ok(Turn {
                id,
                thread_id: turn.thread_id,
                agent_id: turn.agent_id,
                state: turn.state,
                user_input: turn.user_input,
                inbound_audit: None,
                assistant_response: None,
                input_tokens: None,
                output_tokens: None,
                cost_microcents: None,
                structured_output: None,
                parent_turn_id: turn.parent_turn_id,
                created_at: now,
                completed_at: None,
                delegated_input_tokens: None,
                delegated_output_tokens: None,
                execution_isolation: None,
            })
        })
        .await
    }

    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, thread_id, agent_id, user_input, inbound_audit, assistant_response,
                        input_tokens, output_tokens, state, created_at, completed_at, cost_microcents,
                        structured_output, parent_turn_id,
                        delegated_input_tokens, delegated_output_tokens,
                        isolation_kind, isolation_detail
                 FROM turns WHERE id = ?1",
                params![id.to_string()],
                row_to_turn,
            )
            .optional()
        })
        .await
    }

    async fn list_turns_by_thread(&self, thread_id: Uuid) -> Result<Vec<Turn>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, thread_id, agent_id, user_input, inbound_audit, assistant_response,
                        input_tokens, output_tokens, state, created_at, completed_at, cost_microcents,
                        structured_output, parent_turn_id,
                        delegated_input_tokens, delegated_output_tokens,
                        isolation_kind, isolation_detail
                 FROM turns WHERE thread_id = ?1
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![thread_id.to_string()], row_to_turn)?;
            rows.collect()
        })
        .await
    }

    async fn update_turn_state(&self, id: Uuid, new_state: TurnState) -> Result<(), StorageError> {
        // Verify the turn exists before updating.
        let _current = self
            .get_turn(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "turn".into(),
                id: id.to_string(),
            })?;

        // Terminal-state guard is enforced atomically in the SQL WHERE clause
        // below — "first terminal state wins" without a read-then-write race.
        let is_terminal = new_state.is_terminal();

        self.with_conn(move |conn| {
            let state_str = new_state.to_string();
            // The WHERE excludes terminal states so a concurrent Interrupted
            // write can't be overwritten by a late Completed transition.
            if is_terminal {
                let now_str = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE turns SET state = ?1, completed_at = ?2
                     WHERE id = ?3 AND state NOT IN ('completed', 'refused', 'failed', 'interrupted')",
                    params![&state_str, &now_str, id.to_string()],
                )?;
            } else {
                conn.execute(
                    "UPDATE turns SET state = ?1
                     WHERE id = ?2 AND state NOT IN ('completed', 'refused', 'failed', 'interrupted')",
                    params![&state_str, id.to_string()],
                )?;
            }
            Ok(())
        })
        .await
    }

    async fn force_turn_failure(&self, id: Uuid) -> Result<(), StorageError> {
        let _current = self
            .get_turn(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "turn".into(),
                id: id.to_string(),
            })?;

        // Deliberately no terminal guard: the record durability escalation
        // must be able to fail a turn the agent already marked Completed —
        // a known-incomplete record must not stand as a completed turn.
        self.with_conn(move |conn| {
            let now_str = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE turns SET state = 'failed', completed_at = COALESCE(completed_at, ?1)
                 WHERE id = ?2",
                params![&now_str, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn find_in_progress_by_agent(&self, agent_id: &str) -> Result<Vec<Turn>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, thread_id, agent_id, user_input, inbound_audit, assistant_response,
                        input_tokens, output_tokens, state, created_at, completed_at, cost_microcents,
                        structured_output, parent_turn_id,
                        delegated_input_tokens, delegated_output_tokens,
                        isolation_kind, isolation_detail
                 FROM turns WHERE state IN ('pending', 'processing', 'awaiting_approval', 'delegating') AND agent_id = ?1",
            )?;
            let rows = stmt.query_map(params![&agent_id], row_to_turn)?;
            rows.collect()
        })
        .await
    }

    async fn update_response(&self, id: Uuid, response: &str) -> Result<(), StorageError> {
        let response = response.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE turns SET assistant_response = ?1 WHERE id = ?2",
                params![&response, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn update_inbound_audit(
        &self,
        id: Uuid,
        inbound_audit: &InboundAudit,
    ) -> Result<(), StorageError> {
        let inbound_audit_json =
            serde_json::to_string(inbound_audit).map_err(|error| StorageError::Serialisation {
                message: format!("failed to serialise inbound_audit: {error}"),
            })?;
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE turns SET inbound_audit = ?1 WHERE id = ?2",
                params![inbound_audit_json, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn update_tokens(&self, id: Uuid, input: i64, output: i64) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE turns SET input_tokens = ?1, output_tokens = ?2 WHERE id = ?3",
                params![input, output, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn add_delegated_tokens(
        &self,
        id: Uuid,
        input: i64,
        output: i64,
    ) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE turns
                 SET delegated_input_tokens = COALESCE(delegated_input_tokens, 0) + ?1,
                     delegated_output_tokens = COALESCE(delegated_output_tokens, 0) + ?2
                 WHERE id = ?3",
                params![input, output, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn update_cost(&self, id: Uuid, cost: i64) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE turns SET cost_microcents = ?1 WHERE id = ?2",
                params![cost, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn record_turn_isolation(
        &self,
        id: Uuid,
        isolation: &TurnIsolation,
    ) -> Result<(), StorageError> {
        let kind = isolation.kind().to_string();
        let detail = isolation.detail().map(str::to_string);
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE turns SET isolation_kind = ?1, isolation_detail = ?2 WHERE id = ?3",
                params![kind, detail, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn update_structured_output(
        &self,
        id: Uuid,
        output: &serde_json::Value,
    ) -> Result<(), StorageError> {
        // Serialise the JSON value to a text string for storage. This mirrors
        // how cost_microcents stores a scalar — we store structured_output as
        // a TEXT column containing compact JSON.
        let json_text = serde_json::to_string(output).map_err(|e| StorageError::Serialisation {
            message: e.to_string(),
        })?;
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE turns SET structured_output = ?1 WHERE id = ?2",
                params![json_text, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rusqlite::params;
    use uuid::Uuid;

    use crate::model::{NewSession, NewTopic};
    use crate::store::{SessionStore, TopicStore};

    use super::*;

    // Create a topic → session → thread chain.
    //
    // Threads are inserted via raw SQL because the ThreadStore SQLite
    // implementation lives in a separate task and may not be wired yet.
    async fn make_thread() -> (SqliteStorage, Uuid) {
        let (storage, _session_id, thread_id) = make_session_thread().await;
        (storage, thread_id)
    }

    async fn make_session_thread() -> (SqliteStorage, Uuid, Uuid) {
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

    async fn insert_thread(storage: &SqliteStorage, session_id: Uuid, agent_id: &str) -> Uuid {
        let thread_id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        let agent_id = agent_id.to_string();
        storage
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![thread_id.to_string(), session_id.to_string(), agent_id, now],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        thread_id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn force_turn_failure_overrides_a_completed_state() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();
        // The ordinary write's first-terminal-wins guard makes this a no-op…
        storage
            .update_turn_state(turn.id, TurnState::Failed)
            .await
            .unwrap();
        let unchanged = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(unchanged.state, TurnState::Completed);

        // …which is exactly why the durability escalation has its own write.
        storage.force_turn_failure(turn.id).await.unwrap();
        let forced = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(forced.state, TurnState::Failed);
        assert!(forced.completed_at.is_some());
    }

    fn new_turn(thread_id: Uuid) -> NewTurn {
        NewTurn {
            thread_id,
            agent_id: "test-agent".to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_turn() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        assert_eq!(turn.thread_id, thread_id);
        assert_eq!(turn.agent_id, "test-agent");
        assert_eq!(turn.state, TurnState::Pending);
        assert!(turn.completed_at.is_none());
        // Optional fields start as None on creation.
        assert!(turn.user_input.is_none());
        assert!(turn.assistant_response.is_none());
        assert!(turn.input_tokens.is_none());
        assert!(turn.output_tokens.is_none());

        let fetched = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, turn.id);
        assert_eq!(fetched.thread_id, thread_id);
        assert_eq!(fetched.agent_id, "test-agent");
        assert_eq!(fetched.state, TurnState::Pending);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_turns_ordered_by_created_at_asc() {
        let (storage, thread_id) = make_thread().await;

        let t1 = storage.create_turn(new_turn(thread_id)).await.unwrap();
        storage
            .update_turn_state(t1.id, TurnState::Completed)
            .await
            .unwrap();
        let t2 = storage.create_turn(new_turn(thread_id)).await.unwrap();

        let turns = storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns.len(), 2);
        // Older turn first.
        assert!(turns[0].created_at <= turns[1].created_at);
        assert_eq!(turns[0].id, t1.id);
        assert_eq!(turns[1].id, t2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_to_terminal_sets_completed_at() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.state, TurnState::Completed);
        // completed_at should be set for terminal states.
        assert!(updated.completed_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_non_terminal_does_not_set_completed_at() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        // Transitioning to Processing (non-terminal) must leave completed_at unset.
        storage
            .update_turn_state(turn.id, TurnState::Processing)
            .await
            .unwrap();
        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.state, TurnState::Processing);
        assert!(updated.completed_at.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_in_progress_by_agent_filters_by_agent_id() {
        let (storage, session_id, thread_id) = make_session_thread().await;
        let agent_b_thread_id = insert_thread(&storage, session_id, "agent-b").await;
        let completed_agent_a_thread_id = insert_thread(&storage, session_id, "agent-a").await;
        let approval_agent_a_thread_id = insert_thread(&storage, session_id, "agent-a").await;

        // Create turns for two different agents.
        let t1 = storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "agent-a".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let t2 = storage
            .create_turn(NewTurn {
                thread_id: agent_b_thread_id,
                agent_id: "agent-b".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        // Advance t2 to Processing — still in-progress for agent-b.
        storage
            .update_turn_state(t2.id, TurnState::Processing)
            .await
            .unwrap();

        // Complete t3 for agent-a — must NOT appear in results.
        let t3 = storage
            .create_turn(NewTurn {
                thread_id: completed_agent_a_thread_id,
                agent_id: "agent-a".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        storage
            .update_turn_state(t3.id, TurnState::Completed)
            .await
            .unwrap();

        // t4 for agent-a in AwaitingApproval — still active, must appear.
        let t4 = storage
            .create_turn(NewTurn {
                thread_id: approval_agent_a_thread_id,
                agent_id: "agent-a".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        storage
            .update_turn_state(t4.id, TurnState::AwaitingApproval)
            .await
            .unwrap();

        // agent-a has two active turns (t1 pending, t4 awaiting approval).
        let a_results = storage.find_in_progress_by_agent("agent-a").await.unwrap();
        assert_eq!(a_results.len(), 2);
        let a_ids: Vec<_> = a_results.iter().map(|t| t.id).collect();
        assert!(a_ids.contains(&t1.id));
        assert!(a_ids.contains(&t4.id));

        // agent-b has one in-progress turn (t2 is processing).
        let b_results = storage.find_in_progress_by_agent("agent-b").await.unwrap();
        assert_eq!(b_results.len(), 1);
        assert_eq!(b_results[0].id, t2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_response_persists_text() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        // assistant_response is None on creation.
        assert!(
            storage
                .get_turn(turn.id)
                .await
                .unwrap()
                .unwrap()
                .assistant_response
                .is_none()
        );

        storage
            .update_response(turn.id, "The assistant replied.")
            .await
            .unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(
            updated.assistant_response.as_deref(),
            Some("The assistant replied.")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_turn_rejects_second_in_flight_turn_for_thread() {
        let (storage, thread_id) = make_thread().await;

        storage.create_turn(new_turn(thread_id)).await.unwrap();
        let result = storage.create_turn(new_turn(thread_id)).await;

        assert!(
            matches!(result, Err(StorageError::ConstraintViolation(_))),
            "expected unique in-flight turn constraint, got: {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_turn_allows_new_in_flight_after_terminal_turn() {
        let (storage, thread_id) = make_thread().await;

        let completed = storage.create_turn(new_turn(thread_id)).await.unwrap();
        storage
            .update_turn_state(completed.id, TurnState::Completed)
            .await
            .unwrap();

        let next = storage.create_turn(new_turn(thread_id)).await.unwrap();
        assert_eq!(next.thread_id, thread_id);
        assert_eq!(next.state, TurnState::Pending);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_tokens_persists_counts() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        // Token counts are None on creation.
        let fresh = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert!(fresh.input_tokens.is_none());
        assert!(fresh.output_tokens.is_none());

        storage.update_tokens(turn.id, 1024, 256).await.unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.input_tokens, Some(1024));
        assert_eq!(updated.output_tokens, Some(256));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cost_microcents_is_none_on_creation() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();
        assert!(turn.cost_microcents.is_none());

        let fetched = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert!(fetched.cost_microcents.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_cost_persists_value() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        storage.update_cost(turn.id, 1_050).await.unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.cost_microcents, Some(1_050));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn structured_output_is_none_on_creation() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();
        // Freshly created turns have no structured output.
        assert!(turn.structured_output.is_none());

        let fetched = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert!(fetched.structured_output.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_reads_back_the_boundary_its_dispatcher_settled_at() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();
        assert_eq!(
            turn.execution_isolation, None,
            "a turn that has not settled its dispatcher names no boundary yet"
        );

        for isolation in [
            TurnIsolation::Isolated,
            TurnIsolation::Degraded {
                reason: "bubblewrap_unavailable".into(),
            },
            TurnIsolation::Declared {
                boundary: "container".into(),
            },
        ] {
            storage
                .record_turn_isolation(turn.id, &isolation)
                .await
                .unwrap();
            let read = storage.get_turn(turn.id).await.unwrap().unwrap();
            assert_eq!(read.execution_isolation, Some(isolation));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_structured_output_persists_and_is_retrievable() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        let output = serde_json::json!({"x": 1, "label": "ok"});
        storage
            .update_structured_output(turn.id, &output)
            .await
            .unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.structured_output, Some(output));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn terminal_state_guard_prevents_overwrite() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        // Transition to Interrupted (terminal).
        storage
            .update_turn_state(turn.id, TurnState::Interrupted)
            .await
            .unwrap();
        let interrupted = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(interrupted.state, TurnState::Interrupted);

        // Attempting to transition to Completed should be a silent no-op.
        storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();
        let still_interrupted = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(
            still_interrupted.state,
            TurnState::Interrupted,
            "terminal state should not be overwritten"
        );
    }

    /// The delegated aggregate accumulates separately from the turn's own
    /// tokens: billing sums both, the context measure reads own alone, and
    /// which-child-spent-what stays on the child rows.
    #[tokio::test(flavor = "multi_thread")]
    async fn delegated_tokens_accumulate_beside_the_turns_own() {
        let (storage, thread_id) = make_thread().await;
        let parent = storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "test-agent".into(),
                state: TurnState::Processing,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        assert!(parent.delegated_input_tokens.is_none(), "starts unset");

        storage.update_tokens(parent.id, 100, 40).await.unwrap();
        storage
            .add_delegated_tokens(parent.id, 500, 200)
            .await
            .unwrap();
        storage
            .add_delegated_tokens(parent.id, 30, 10)
            .await
            .unwrap();

        let read = storage.get_turn(parent.id).await.unwrap().unwrap();
        // The turn's own figures — the context measure's inputs — are
        // untouched by the children's spend.
        assert_eq!(read.input_tokens, Some(100));
        assert_eq!(read.output_tokens, Some(40));
        // The billing aggregate accumulated both children.
        assert_eq!(read.delegated_input_tokens, Some(530));
        assert_eq!(read.delegated_output_tokens, Some(210));
    }
}
