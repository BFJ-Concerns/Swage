use async_trait::async_trait;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    Message, NewMessage, NewProject, NewSession, NewToolCall, NewTurn, Project, Session, Turn,
    ToolCallRecord, TurnState,
};

// ---------------------------------------------------------------------------
// Store sub-traits
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ProjectStore: Send + Sync {
    async fn create(&self, project: NewProject) -> Result<Project, StorageError>;
    async fn get(&self, id: Uuid) -> Result<Option<Project>, StorageError>;
    async fn get_by_name(&self, name: &str) -> Result<Option<Project>, StorageError>;
    /// Get the default project, creating it if it does not exist.
    async fn get_or_create_default(&self) -> Result<Project, StorageError>;
    /// Return all projects ordered by created_at ascending.
    async fn list_all(&self) -> Result<Vec<Project>, StorageError>;
    /// Look up a project by its slug.
    async fn get_by_slug(&self, slug: &str) -> Result<Option<Project>, StorageError>;
    /// Partially update a project. Only `Some` fields are changed.
    ///
    /// Returns the updated project, or `StorageError::NotFound` if `id` is unknown.
    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        slug: Option<String>,
        workspace_path: Option<String>,
    ) -> Result<Project, StorageError>;
    /// Delete a project and cascade its sessions.
    ///
    /// Returns `true` if the project existed and was removed, `false` if not found.
    /// Returns `StorageError::ConstraintViolation` if called on the default project.
    async fn delete(&self, id: Uuid) -> Result<bool, StorageError>;
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError>;
    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError>;
    /// List sessions in the project ordered by updated_at DESC.
    async fn list_by_project(&self, project_id: Uuid) -> Result<Vec<Session>, StorageError>;
    /// Delete a session. Returns true if it existed and was removed.
    async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError>;
    /// Bump the session's updated_at to now.
    async fn update_activity(&self, id: Uuid) -> Result<(), StorageError>;
}

#[async_trait]
pub trait TurnStore: Send + Sync {
    async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError>;
    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError>;
    /// List turns for a session ordered by created_at ascending.
    async fn list_turns_by_session(&self, session_id: Uuid) -> Result<Vec<Turn>, StorageError>;
    /// Transition a turn to a new state. Validates via `can_transition_to`.
    async fn update_turn_state(&self, id: Uuid, state: TurnState) -> Result<(), StorageError>;
    /// Find all in-progress turns (used by startup recovery).
    async fn find_in_progress_turns(&self) -> Result<Vec<Turn>, StorageError>;
}

#[async_trait]
pub trait MessageStore: Send + Sync {
    async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError>;
    /// List all messages for a session, ordered by turn created_at then position.
    async fn list_messages_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Message>, StorageError>;
    async fn list_messages_by_turn(&self, turn_id: Uuid) -> Result<Vec<Message>, StorageError>;
    async fn get_latest_message_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Message>, StorageError>;
}

#[async_trait]
pub trait ToolCallStore: Send + Sync {
    /// Insert a new tool call record. Returns the persisted record with its assigned id.
    async fn create_tool_call(
        &self,
        tool_call: NewToolCall,
    ) -> Result<ToolCallRecord, StorageError>;

    /// Write the completed result back onto an in-progress tool call.
    ///
    /// Matched by `call_id` scoped to `turn_id` to avoid cross-turn collisions.
    async fn update_tool_call_result(
        &self,
        call_id: &str,
        turn_id: Uuid,
        result: &str,
        status: &str,
        duration_ms: i64,
        truncated: bool,
        total_bytes: Option<i64>,
    ) -> Result<(), StorageError>;

    /// Fetch all tool calls for a turn, ordered by position ascending.
    async fn list_tool_calls_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ToolCallRecord>, StorageError>;
}

// ---------------------------------------------------------------------------
// Storage supertrait
// ---------------------------------------------------------------------------

/// Combined storage interface implemented by `SqliteStorage`.
pub trait Storage:
    ProjectStore + SessionStore + TurnStore + MessageStore + ToolCallStore + Send + Sync
{
}

/// Blanket implementation so any type satisfying all sub-traits gets Storage for free.
impl<T> Storage for T where
    T: ProjectStore + SessionStore + TurnStore + MessageStore + ToolCallStore + Send + Sync
{
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewToolCall, ToolCallRecord};
    use std::future::Future;

    /// Compile-time check: a type implementing all sub-traits satisfies Storage.
    ///
    /// This struct exists only to verify the trait bounds compile correctly.
    /// It panics if any method is actually called.
    struct StubStorage;

    #[async_trait]
    impl ProjectStore for StubStorage {
        async fn create(&self, _: NewProject) -> Result<Project, StorageError> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<Project>, StorageError> {
            unimplemented!()
        }
        async fn get_by_name(&self, _: &str) -> Result<Option<Project>, StorageError> {
            unimplemented!()
        }
        async fn get_or_create_default(&self) -> Result<Project, StorageError> {
            unimplemented!()
        }
        async fn list_all(&self) -> Result<Vec<Project>, StorageError> {
            unimplemented!()
        }
        async fn get_by_slug(&self, _: &str) -> Result<Option<Project>, StorageError> {
            unimplemented!()
        }
        async fn update(&self, _: Uuid, _: Option<String>, _: Option<String>, _: Option<String>) -> Result<Project, StorageError> {
            unimplemented!()
        }
        async fn delete(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl SessionStore for StubStorage {
        async fn create_session(&self, _: NewSession) -> Result<Session, StorageError> {
            unimplemented!()
        }
        async fn get_session(&self, _: Uuid) -> Result<Option<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_by_project(&self, _: Uuid) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn delete_session(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_activity(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TurnStore for StubStorage {
        async fn create_turn(&self, _: NewTurn) -> Result<Turn, StorageError> {
            unimplemented!()
        }
        async fn get_turn(&self, _: Uuid) -> Result<Option<Turn>, StorageError> {
            unimplemented!()
        }
        async fn list_turns_by_session(&self, _: Uuid) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_turn_state(&self, _: Uuid, _: TurnState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn find_in_progress_turns(&self) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl MessageStore for StubStorage {
        async fn create_message(&self, _: NewMessage) -> Result<Message, StorageError> {
            unimplemented!()
        }
        async fn list_messages_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<Message>, StorageError> {
            unimplemented!()
        }
        async fn list_messages_by_turn(&self, _: Uuid) -> Result<Vec<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_latest_message_for_session(
            &self,
            _: Uuid,
        ) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ToolCallStore for StubStorage {
        async fn create_tool_call(
            &self,
            _: NewToolCall,
        ) -> Result<ToolCallRecord, StorageError> {
            unimplemented!()
        }
        async fn update_tool_call_result(
            &self,
            _call_id: &str,
            _turn_id: Uuid,
            _result: &str,
            _status: &str,
            _duration_ms: i64,
            _truncated: bool,
            _total_bytes: Option<i64>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_tool_calls_by_turn(
            &self,
            _: Uuid,
        ) -> Result<Vec<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
    }

    /// Verify that a type implementing all sub-traits satisfies the Storage supertrait.
    #[test]
    fn storage_supertrait_includes_tool_call_store() {
        // This is a compile-time check: if Storage doesn't require ToolCallStore,
        // the blanket impl and this constraint would be inconsistent.
        fn _assert_is_storage<T: Storage>(_: &T) {}
        let stub = StubStorage;
        _assert_is_storage(&stub);
    }
}
