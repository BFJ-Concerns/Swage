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

fn row_to_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let state_str: String = row.get(2)?;
    let created: String = row.get(3)?;
    let completed: Option<String> = row.get(4)?;

    Ok(Turn {
        id: parse_uuid(&id)?,
        session_id: parse_uuid(&session_id)?,
        state: state_str
            .parse::<TurnState>()
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(ParseError(e))))?,
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
                "INSERT INTO turns (id, session_id, state, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), turn.session_id.to_string(), &state_str, &now_str],
            )?;

            Ok(Turn {
                id,
                session_id: turn.session_id,
                state: turn.state,
                created_at: now,
                completed_at: None,
            })
        })
        .await
    }

    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, session_id, state, created_at, completed_at
                 FROM turns WHERE id = ?1",
                params![id.to_string()],
                row_to_turn,
            )
            .optional()
        })
        .await
    }

    async fn list_turns_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Turn>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, state, created_at, completed_at
                 FROM turns WHERE session_id = ?1
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_turn)?;
            rows.collect()
        })
        .await
    }

    async fn update_turn_state(
        &self,
        id: Uuid,
        new_state: TurnState,
    ) -> Result<(), StorageError> {
        let current = self
            .get_turn(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "turn".into(),
                id: id.to_string(),
            })?;

        if !current.state.can_transition_to(&new_state) {
            return Err(StorageError::ConstraintViolation(format!(
                "cannot transition turn {} from {} to {}",
                id, current.state, new_state
            )));
        }

        let is_terminal = !matches!(new_state, TurnState::InProgress);

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

    async fn find_in_progress_turns(&self) -> Result<Vec<Turn>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, state, created_at, completed_at
                 FROM turns WHERE state = 'in_progress'",
            )?;
            let rows = stmt.query_map([], row_to_turn)?;
            rows.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewProject, NewSession};
    use crate::store::{ProjectStore, SessionStore};

    use super::*;

    async fn make_session() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject { name: "p".into(), slug: "p".into(), workspace_path: None })
            .await
            .unwrap();
        let session = storage.create_session(NewSession { project_id: project.id }).await.unwrap();
        (storage, session.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_turn() {
        let (storage, session_id) = make_session().await;
        let turn = storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();

        assert_eq!(turn.session_id, session_id);
        assert_eq!(turn.state, TurnState::InProgress);
        assert!(turn.completed_at.is_none());

        let fetched = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, turn.id);
        assert_eq!(fetched.state, TurnState::InProgress);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_turns_ordered_by_created_at_asc() {
        let (storage, session_id) = make_session().await;

        let t1 = storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();
        let t2 = storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();

        let turns = storage.list_turns_by_session(session_id).await.unwrap();
        assert_eq!(turns.len(), 2);
        // Older turn first.
        assert!(turns[0].created_at <= turns[1].created_at);
        assert_eq!(turns[0].id, t1.id);
        assert_eq!(turns[1].id, t2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_valid_transition() {
        let (storage, session_id) = make_session().await;
        let turn = storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();

        storage.update_turn_state(turn.id, TurnState::Completed).await.unwrap();

        let updated = storage.get_turn(turn.id).await.unwrap().unwrap();
        assert_eq!(updated.state, TurnState::Completed);
        // completed_at should be set for terminal states.
        assert!(updated.completed_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_state_invalid_transition_rejected() {
        let (storage, session_id) = make_session().await;
        let turn = storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();
        storage.update_turn_state(turn.id, TurnState::Completed).await.unwrap();

        // Terminal→terminal transitions must be rejected.
        let err = storage
            .update_turn_state(turn.id, TurnState::Failed)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_in_progress_turns_only_returns_in_progress() {
        let (storage, session_id) = make_session().await;

        let t1 = storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();
        let t2 = storage
            .create_turn(NewTurn { session_id, state: TurnState::InProgress })
            .await
            .unwrap();
        storage.update_turn_state(t2.id, TurnState::Completed).await.unwrap();

        let in_progress = storage.find_in_progress_turns().await.unwrap();
        assert_eq!(in_progress.len(), 1);
        assert_eq!(in_progress[0].id, t1.id);
    }
}
