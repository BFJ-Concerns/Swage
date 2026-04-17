//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use crate::sqlite::secret::{
    MANAGED_SECRET_KEY_ENV, ManagedSecretEncryptionDiagnostics,
    managed_secret_encryption_diagnostics,
};
pub use model::{
    ActiveSkillRecord, AgentMessage, ApprovalKind, ApprovalRequest, ApprovalState, AuditEvent,
    ChannelLink, DeliveryRecord, EdgeEvaluation, Heartbeat, HeartbeatRun, HeartbeatRunState,
    HeartbeatStatus, InboundAudit, Message, NewAgentMessage, NewApprovalRequest, NewAuditEvent,
    NewChannelLink, NewDeliveryRecord, NewEdgeEvaluation, NewHeartbeat, NewHeartbeatRun,
    NewMessage, NewNodeExecution, NewNodeRetry, NewProject, NewScheduledJob, NewScheduledRun,
    NewSecretRecord, NewSession, NewTeam, NewThread, NewToolCall, NewTopologyRun, NewTurn,
    NodeExecution, NodeExecutionState, NodeRetry, Project, ScheduledJob, ScheduledJobKind,
    ScheduledJobStatus, ScheduledRun, ScheduledRunState, ScheduledRunTrigger, SecretRecord,
    Session, Team, Thread, ThreadBranch, ThreadState, ToolCallRecord, TopologyRun,
    TopologyRunState, Turn, TurnState, UpdateHeartbeat, UpdateScheduledJob,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMessageStore, ApprovalStore, AuditStore, ChannelLinkStore, ContextSnapshotStore,
    DeliveryStore, HeartbeatStore, MessageStore, ProjectStore, ScheduleStore, SecretStore,
    SessionStore, SkillActivationStore, Storage, TeamStore, ThreadStore, ToolCallStore,
    TopologyTraceStore, TurnStore,
};
