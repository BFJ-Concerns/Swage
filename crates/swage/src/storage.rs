//! Storage abstraction for persisting agent sessions and conversation history.
//!
//! Defines the `Storage` trait for managing sessions, containers, and messages.
//! Implementations handle persistence details like database connections, file
//! formats, and transaction management.
//!
//! # Storage Responsibilities
//!
//! - **Session management**: Create, retrieve, update, and delete agent sessions
//! - **Container management**: Organise sessions into logical containers
//! - **Message persistence**: Store conversation history within sessions
//! - **State management**: Track arbitrary session state via JSON values
//! - **Branching support**: Optional branch_id parameter for future multi-branch sessions
//!
//! # Core Concepts
//!
//! - **Session**: Represents a single agent conversation with message history and state
//! - **Container**: A logical grouping of related sessions (e.g., project, user, workspace)
//! - **Message**: A single turn in the conversation with role and content
//! - **State**: Arbitrary JSON data attached to a session for custom metadata
//!
//! # Examples
//!
//! ```rust,ignore
//! use swage::storage::{Storage, Session, Message, Role};
//!
//! async fn create_conversation<S: Storage>(storage: &S) -> Result<(), StorageError> {
//!     // Create a container for the project
//!     let container = Container {
//!         id: "project-123".to_string(),
//!         container_type: "project".to_string(),
//!         name: "My Project".to_string(),
//!         metadata: serde_json::json!({"owner": "alice"}),
//!     };
//!     storage.create_container(&container).await?;
//!
//!     // Create a session within the container
//!     let session = Session {
//!         id: "session-456".to_string(),
//!         container_id: Some("project-123".to_string()),
//!         messages: vec![
//!             Message {
//!                 role: Role::User,
//!                 content: "Hello!".to_string(),
//!             }
//!         ],
//!         state: serde_json::json!({}),
//!         branch_id: None,
//!         created_at: chrono::Utc::now(),
//!         updated_at: chrono::Utc::now(),
//!     };
//!     storage.create_session(&session).await?;
//!
//!     Ok(())
//! }
//! ```

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;

// Re-export message types for backwards compatibility
pub use crate::message::{Message, Role};

/// Async trait for storage backend implementations.
///
/// Provides operations for managing sessions, containers, and conversation history.
/// All methods are async to support various backend types (databases, remote APIs, etc.).
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent access across async tasks.
///
/// # Transaction Semantics
///
/// Implementations should provide atomic operations where possible. If a create or update
/// operation fails partway through, the storage should remain in a consistent state.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Creates a new session.
    ///
    /// The session ID must be unique. If a session with the same ID already exists,
    /// implementations should return an appropriate error.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if:
    /// - A session with this ID already exists
    /// - The container_id references a non-existent container
    /// - Serialisation of the state or messages fails
    /// - An I/O error occurs during persistence
    async fn create_session(&self, session: &Session) -> Result<(), StorageError>;

    /// Retrieves a session by ID.
    ///
    /// # Parameters
    ///
    /// - `id`: The session identifier
    /// - `branch_id`: Optional branch identifier for future branching support
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NotFound` if the session doesn't exist.
    /// Returns other `StorageError` variants for I/O or deserialisation failures.
    async fn get_session(&self, id: &str, branch_id: Option<&str>) -> Result<Session, StorageError>;

    /// Updates an existing session.
    ///
    /// Replaces the session data with the provided session. The session ID must exist.
    /// Implementations should update the `updated_at` timestamp automatically.
    ///
    /// # Parameters
    ///
    /// - `session`: The updated session data
    /// - `branch_id`: Optional branch identifier for future branching support
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NotFound` if the session doesn't exist.
    /// Returns other `StorageError` variants for serialisation or I/O failures.
    async fn update_session(&self, session: &Session, branch_id: Option<&str>) -> Result<(), StorageError>;

    /// Deletes a session by ID.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NotFound` if the session doesn't exist.
    /// Returns other `StorageError` variants for I/O failures.
    async fn delete_session(&self, id: &str) -> Result<(), StorageError>;

    /// Creates a new container.
    ///
    /// The container ID must be unique. If a container with the same ID already exists,
    /// implementations should return an appropriate error.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if:
    /// - A container with this ID already exists
    /// - Serialisation of the metadata fails
    /// - An I/O error occurs during persistence
    async fn create_container(&self, container: &Container) -> Result<(), StorageError>;

    /// Retrieves a container by ID.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NotFound` if the container doesn't exist.
    /// Returns other `StorageError` variants for I/O or deserialisation failures.
    async fn get_container(&self, id: &str) -> Result<Container, StorageError>;

    /// Lists all sessions within a container.
    ///
    /// Returns an empty vector if the container has no sessions. Does not return an error
    /// if the container doesn't exist—implementations should treat non-existent containers
    /// as empty.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` for I/O or deserialisation failures.
    async fn list_sessions(&self, container_id: &str) -> Result<Vec<Session>, StorageError>;
}

/// A conversation session with message history and state.
///
/// Sessions represent a single agent conversation. They contain the full message history,
/// arbitrary state data, and metadata about creation and modification times.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    /// Unique identifier for this session.
    pub id: String,

    /// Optional container this session belongs to.
    ///
    /// Sessions can be organised into containers for logical grouping.
    /// A `None` value indicates an orphaned or top-level session.
    pub container_id: Option<String>,

    /// The conversation history.
    ///
    /// Messages are ordered chronologically. New messages should be appended to the end.
    pub messages: Vec<Message>,

    /// Arbitrary state data for this session.
    ///
    /// Can store custom metadata, checkpoints, or application-specific data.
    /// Implementations should preserve this value exactly as provided.
    pub state: serde_json::Value,

    /// Optional branch identifier for multi-branch session support.
    ///
    /// Currently unused but reserved for future branching functionality where
    /// sessions can fork into multiple conversation paths.
    pub branch_id: Option<String>,

    /// Timestamp when this session was created.
    pub created_at: DateTime<Utc>,

    /// Timestamp when this session was last updated.
    ///
    /// Implementations should update this automatically on `update_session` calls.
    pub updated_at: DateTime<Utc>,
}

/// A logical grouping of related sessions.
///
/// Containers provide organisation for sessions, allowing them to be grouped by
/// project, user, workspace, or any other logical boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Container {
    /// Unique identifier for this container.
    pub id: String,

    /// The type of container (e.g., "project", "user", "workspace").
    ///
    /// This is a free-form string that applications can use to distinguish
    /// different container categories.
    pub container_type: String,

    /// Human-readable name for this container.
    pub name: String,

    /// Arbitrary metadata for this container.
    ///
    /// Can store ownership information, configuration, or application-specific data.
    /// Implementations should preserve this value exactly as provided.
    pub metadata: serde_json::Value,
}

