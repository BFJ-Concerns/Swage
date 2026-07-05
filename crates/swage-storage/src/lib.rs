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
    BackgroundJob, BackgroundJobKind, BackgroundJobState, Bench, BenchSlug, ChannelLink,
    ConfigurationVersion, ContextSnapshotRecord, DeliveryRecord, EdgeEvaluation, GateConfig,
    GateDecision, GateVerdict, InboundAudit, LensPreference, LensRecord, LensSourceKind, Material,
    MaterialSourceKind, MaterialStatus, Message, NewAgentMemoryRecord, NewAgentMessage,
    NewApprovalRequest, NewAttachment, NewAuditEvent, NewBackgroundJob, NewBench, NewChannelLink,
    NewConfigurationVersion, NewDeliveryRecord, NewEdgeEvaluation, NewLensRecord, NewMaterial,
    NewMessage, NewNodeExecution, NewNodeRetry, NewSchedule, NewScheduleRun, NewSecretRecord,
    NewSession, NewSharedReferenceEvent, NewTeam, NewThread, NewToolCall, NewTopologyRun, NewTurn,
    NewWorkspaceAttachment, NodeExecution, NodeExecutionState, NodeRetry, Schedule, ScheduleKind,
    ScheduleRun, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate, ScheduleStatus,
    SecretRecord, Session, SessionPolicy, SharedReferenceEvent, SharedReferenceKind, SkipReason,
    Team, Thread, ThreadBranch, ThreadState, ToolCallRecord, TopologyRun, TopologyRunState, Turn,
    TurnState, UpdateSchedule, WorkspaceAttachment,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMemoryStore, AgentMessageStore, ApprovalStore, AttachmentStore, AuditStore,
    BackgroundJobStore, BenchStore, ChannelLinkStore, ConfigurationVersionStore,
    ContextSnapshotStore, DeliveryStore, LensStore, MaterialStore, MessageStore, ScheduleStore,
    SecretStore, SessionStore, SharedReferenceStore, SkillActivationStore, Storage, TeamStore,
    ThreadStore, ToolCallStore, TopologyTraceStore, TurnStore, WorkspaceAttachmentStore,
};
