use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewTurn, Turn, TurnState};
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
    s.parse::<Uuid>()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// Column order: id, thread_id, agent_id, user_input, assistant_response,
/// input_tokens, output_tokens, state, created_at, completed_at, cost_microcents,
/// structured_output
fn row_to_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    let id: String = row.get(0)?;
    let thread_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let state_str: String = row.get(7)?;
    let created: String = row.get(8)?;
    let completed: Option<String> = row.get(9)?;
    // structured_output is stored as a JSON text string; deserialise on read.
    let structured_output_text: Option<String> = row.get(11)?;
    let structured_output = structured_output_text
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(ParseError(e.to_string())),
        ))?;

    Ok(Turn {
        id: parse_uuid(&id)?,
        thread_id: parse_uuid(&thread_id)?,
        agent_id,
        state: state_str
            .parse::<TurnState>()
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(ParseError(e))))?,
        user_input: row.get(3)?,
        assistant_response: row.get(4)?,
        input_tokens: row.get(5)?,
        output_tokens: row.get(6)?,
        cost_microcents: row.get(10)?,
        structured_output,
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

            conn.execute(
                "INSERT INTO turns (id, thread_id, agent_id, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id.to_string(),
                    turn.thread_id.to_string(),
                    &turn.agent_id,
                    &state_str,
                    &now_str
                ],
            )?;

            Ok(Turn {
                id,
                thread_id: turn.thread_id,
                agent_id: turn.agent_id,
                state: turn.state,
                user_input: None,
                assistant_response: None,
                input_tokens: None,
                output_tokens: None,
                cost_microcents: None,
                structured_output: None,
                created_at: now,
                completed_at: None,
            })
        })
        .await
    }

    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, thread_id, agent_id, user_input, assistant_response,
                        input_tokens, output_tokens, state, created_at, completed_at, cost_microcents,
                        structured_output
                 FROM turns WHERE id = ?1",
                params![id.to_string()],
                row_to_turn,
            )
            .optional()
        })
        .await
    }

    async fn list_turns_by_thread(
        &self,
        thread_id: Uuid,
    ) -> Result<Vec<Turn>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, thread_id, agent_id, user_input, assistant_response,
                        input_tokens, output_tokens, state, created_at, completed_at, cost_microcents,
                        structured_output
                 FROM turns WHERE thread_id = ?1
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![thread_id.to_string()], row_to_turn)?;
            rows.collect()
        })
        .await
    }

    async fn update_turn_state(
        &self,
        id: Uuid,
        new_state: TurnState,
    ) -> Result<(), StorageError> {
        // Verify the turn exists before updating.
        self.get_turn(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "turn".into(),
                id: id.to_string(),
            })?;

        // State transitions are consumer-driven; no machine enforcement here.
        // Completed, Failed, and Interrupted are terminal — set completed_at.
        let is_terminal = matches!(new_state, TurnState::Completed | TurnState::Failed | TurnState::Interrupted);

        self.with_conn(move |conn| {
            let state_str = new_state.to_string();
            if is_terminal {
                let now_str = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE turns SET state = ?1, completed_at = ?2 WHERE id = ?3",
                    params![&state_str, &now_str, id.to_string()],
                )?;
            } else {
                conn.execute(
                    "UPDATE turns SET state = ?1 WHERE id = ?2",
                    params![&state_str, id.to_string()],
                )?;
            }
            Ok(())
        })
        .await
    }

    async fn find_in_progress_by_agent(&self, agent_id: &str) -> Result<Vec<Turn>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, thread_id, agent_id, user_input, assistant_response,
                        input_tokens, output_tokens, state, created_at, completed_at, cost_microcents,
                        structured_output
                 FROM turns WHERE state IN ('pending', 'processing') AND agent_id = ?1",
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

    async fn update_structured_output(
        &self,
        id: Uuid,
        output: &serde_json::Value,
    ) -> Result<(), StorageError> {
        // Serialise the JSON value to a text string for storage. This mirrors
        // how cost_microcents stores a scalar — we store structured_output as
        // a TEXT column containing compact JSON.
        let json_text = serde_json::to_string(output)
            .map_err(|e| StorageError::Serialisation { message: e.to_string() })?;
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

    use crate::model::{NewProject, NewSession};
    use crate::store::{ProjectStore, SessionStore};

    use super::*;

    // Create a project → session → thread chain.
    //
    // Threads are inserted via raw SQL because the ThreadStore SQLite
    // implementation lives in a separate task and may not be wired yet.
    async fn make_thread() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject { name: "p".into(), slug: "p".into(), workspace_path: None })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession { project_id: project.id, agent_id: "test-agent".to_string() })
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

        (storage, thread_id)
    }

    fn new_turn(thread_id: Uuid) -> NewTurn {
        NewTurn { thread_id, agent_id: "test-agent".to_string(), state: TurnState::Pending }
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

        storage.update_turn_state(turn.id, TurnState::Completed).await.unwrap();

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
        storage.update_turn_state(turn.id, TurnState::Processing).await.unwrap();
        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.state, TurnState::Processing);
        assert!(updated.completed_at.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_in_progress_by_agent_filters_by_agent_id() {
        let (storage, thread_id) = make_thread().await;

        // Create turns for two different agents.
        let t1 = storage
            .create_turn(NewTurn { thread_id, agent_id: "agent-a".to_string(), state: TurnState::Pending })
            .await
            .unwrap();
        let t2 = storage
            .create_turn(NewTurn { thread_id, agent_id: "agent-b".to_string(), state: TurnState::Pending })
            .await
            .unwrap();
        // Advance t2 to Processing — still in-progress for agent-b.
        storage.update_turn_state(t2.id, TurnState::Processing).await.unwrap();

        // Complete t3 for agent-a — must NOT appear in results.
        let t3 = storage
            .create_turn(NewTurn { thread_id, agent_id: "agent-a".to_string(), state: TurnState::Pending })
            .await
            .unwrap();
        storage.update_turn_state(t3.id, TurnState::Completed).await.unwrap();

        // agent-a has one in-progress turn (t1 is pending).
        let a_results = storage.find_in_progress_by_agent("agent-a").await.unwrap();
        assert_eq!(a_results.len(), 1);
        assert_eq!(a_results[0].id, t1.id);

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
        assert!(storage.get_turn(turn.id).await.unwrap().unwrap().assistant_response.is_none());

        storage.update_response(turn.id, "The assistant replied.").await.unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.assistant_response.as_deref(), Some("The assistant replied."));
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
    async fn update_structured_output_persists_and_is_retrievable() {
        let (storage, thread_id) = make_thread().await;
        let turn = storage.create_turn(new_turn(thread_id)).await.unwrap();

        let output = serde_json::json!({"x": 1, "label": "ok"});
        storage.update_structured_output(turn.id, &output).await.unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.structured_output, Some(output));
    }
}
