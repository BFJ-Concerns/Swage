//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use crate::sqlite::secret::{
    MANAGED_SECRET_KEY_ENV, ManagedSecretEncryptionDiagnostics,
    managed_secret_encryption_diagnostics,
};
pub use model::{
    ActiveSkillRecord, AgentMessage, AppLens, ApprovalKind, ApprovalRequest, ApprovalState,
    Attachment, AttachmentKind, AttachmentScopeKind, AttachmentStatus, AuditEvent, BackgroundJob,
    BackgroundJobKind, BackgroundJobState, ChannelLink, ConfigurationVersion,
    ContextSnapshotRecord, DeliveryRecord, EdgeEvaluation, Heartbeat, HeartbeatRun,
    HeartbeatRunState, HeartbeatStatus, InboundAudit, LensPreference, LensRecord, LensSourceKind,
    Material, MaterialSourceKind, MaterialStatus, Message, NewAgentMessage, NewApprovalRequest,
    NewAttachment, NewAuditEvent, NewBackgroundJob, NewChannelLink, NewConfigurationVersion,
    NewDeliveryRecord, NewEdgeEvaluation, NewHeartbeat, NewHeartbeatRun, NewLensRecord,
    NewMaterial, NewMessage, NewNodeExecution, NewNodeRetry, NewProject, NewScheduledJob,
    NewScheduledRun, NewSecretRecord, NewSession, NewSharedReferenceEvent, NewTeam, NewThread,
    NewToolCall, NewTopologyRun, NewTurn, NodeExecution, NodeExecutionState, NodeRetry, Project,
    ScheduledJob, ScheduledJobKind, ScheduledJobStatus, ScheduledRun, ScheduledRunState,
    ScheduledRunTrigger, SecretRecord, Session, SharedReferenceEvent, SharedReferenceKind, Team,
    Thread, ThreadBranch, ThreadState, ToolCallRecord, TopologyRun, TopologyRunState, Turn,
    TurnState, UpdateHeartbeat, UpdateScheduledJob,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMessageStore, ApprovalStore, AttachmentStore, AuditStore, BackgroundJobStore,
    ChannelLinkStore, ConfigurationVersionStore, ContextSnapshotStore, DeliveryStore,
    HeartbeatStore, LensStore, MaterialStore, MessageStore, ProjectStore, ScheduleStore,
    SecretStore, SessionStore, SharedReferenceStore, SkillActivationStore, Storage, TeamStore,
    ThreadStore, ToolCallStore, TopologyTraceStore, TurnStore,
};
