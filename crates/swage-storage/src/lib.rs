//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use model::{
    ActiveSkillRecord, AgentMessage, ApprovalKind, ApprovalRequest, ApprovalState, Message,
    EdgeEvaluation, NewAgentMessage, NewApprovalRequest, NewEdgeEvaluation,
    NewMessage, NewNodeExecution, NewNodeRetry, NewProject, NewSession, NewTeam,
    NewThread, NewToolCall, NewTopologyRun, NewTurn, NodeExecution, NodeExecutionState,
    NodeRetry, Project, Session, Team, Thread, ThreadState, ToolCallRecord, TopologyRun,
    TopologyRunState, Turn, TurnState,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMessageStore, ApprovalStore, ContextSnapshotStore, MessageStore, ProjectStore,
    SessionStore, SkillActivationStore, Storage, TeamStore, ThreadStore,
    ToolCallStore, TopologyTraceStore, TurnStore,
};
