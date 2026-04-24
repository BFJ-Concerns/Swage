//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use crate::sqlite::secret::{
    MANAGED_SECRET_KEY_ENV, ManagedSecretEncryptionDiagnostics,
    managed_secret_encryption_diagnostics,
};
pub use model::{
    ActiveSkillRecord, AgentMemoryRecord, AgentMessage, AppLens, ApprovalKind, ApprovalRequest,
    ApprovalState, Attachment, AttachmentKind, AttachmentScopeKind, AttachmentStatus, AuditEvent,
    BackgroundJob, BackgroundJobKind, BackgroundJobState, Bench, ChannelLink, ConfigurationVersion,
    ContextSnapshotRecord, DeliveryRecord, EdgeEvaluation, Heartbeat, HeartbeatRun,
    HeartbeatRunState, HeartbeatStatus, InboundAudit, LensPreference, LensRecord, LensSourceKind,
    Material, MaterialSourceKind, MaterialStatus, Message, NewAgentMemoryRecord, NewAgentMessage,
    NewApprovalRequest, NewAttachment, NewAuditEvent, NewBackgroundJob, NewBench, NewChannelLink,
    NewConfigurationVersion, NewDeliveryRecord, NewEdgeEvaluation, NewHeartbeat, NewHeartbeatRun,
    NewLensRecord, NewMaterial, NewMessage, NewNodeExecution, NewNodeRetry, NewScheduledJob,
    NewScheduledRun, NewSecretRecord, NewSession, NewSharedReferenceEvent, NewTeam, NewThread,
    NewToolCall, NewTopologyRun, NewTurn, NewWorkspaceAttachment, NodeExecution,
    NodeExecutionState, NodeRetry, ScheduledJob, ScheduledJobKind, ScheduledJobStatus,
    ScheduledRun, ScheduledRunState, ScheduledRunTrigger, SecretRecord, Session,
    SharedReferenceEvent, SharedReferenceKind, Team, Thread, ThreadBranch, ThreadState,
    ToolCallRecord, TopologyRun, TopologyRunState, Turn, TurnState, UpdateHeartbeat,
    UpdateScheduledJob, WorkspaceAttachment,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMemoryStore, AgentMessageStore, ApprovalStore, AttachmentStore, AuditStore,
    BackgroundJobStore, BenchStore, ChannelLinkStore, ConfigurationVersionStore,
    ContextSnapshotStore, DeliveryStore, HeartbeatStore, LensStore, MaterialStore, MessageStore,
    ScheduleStore, SecretStore, SessionStore, SharedReferenceStore, SkillActivationStore, Storage,
    TeamStore, ThreadStore, ToolCallStore, TopologyTraceStore, TurnStore, WorkspaceAttachmentStore,
};
