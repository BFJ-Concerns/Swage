//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use model::{
    ActiveSkillRecord, AgentMessage, ApprovalKind, ApprovalRequest, ApprovalState, Message,
    NewAgentMessage, NewApprovalRequest, NewMessage, NewProject, NewSession, NewTeam,
    NewThread, NewToolCall, NewTurn, Project, Session, Team, Thread, ThreadState, Turn,
    ToolCallRecord, TurnState,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMessageStore, ApprovalStore, ContextSnapshotStore, MessageStore, ProjectStore,
    SessionStore, SkillActivationStore, Storage, TeamStore, ThreadStore, ToolCallStore,
    TurnStore,
};
