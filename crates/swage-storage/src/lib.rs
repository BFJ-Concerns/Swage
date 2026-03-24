//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use model::{
    ActiveSkillRecord, ApprovalKind, ApprovalRequest, ApprovalState, Message, NewApprovalRequest,
    NewMessage, NewProject, NewSession, NewThread, NewToolCall, NewTurn, Project, Session, Thread,
    ThreadState, Turn, ToolCallRecord, TurnState,
};
pub use sqlite::SqliteStorage;
pub use store::{
    ApprovalStore, ContextSnapshotStore, MessageStore, ProjectStore, SessionStore,
    SkillActivationStore, Storage, ThreadStore, ToolCallStore, TurnStore,
};
