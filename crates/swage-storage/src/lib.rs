//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use crate::sqlite::secret::{
    MANAGED_SECRET_KEY_ENV, ManagedSecretEncryptionDiagnostics,
    managed_secret_encryption_diagnostics,
};
pub use model::{
    ActiveSkillRecord, AgentMemoryRecord, AgentMessage, ApprovalKind, ApprovalRequest,
    ApprovalState, Attachment, AttachmentKind, AttachmentScopeKind, AttachmentStatus, AuditEvent,
    BackgroundJob, BackgroundJobKind, BackgroundJobState, ChannelInboundMessage,
    ChannelInboundMessageState, ChannelLink, ConfigurationVersion, ContextSnapshotRecord,
    DeliveryRecord, GateConfig, GateDecision, GateVerdict, InboundAudit,
    Material, MaterialSourceKind, MaterialStatus,
    Message, NewAgentMemoryRecord, NewAgentMessage, NewApprovalRequest, NewAttachment,
    NewAuditEvent, NewBackgroundJob, NewChannelInboundMessage, NewChannelLink,
    NewConfigurationVersion, NewDeliveryRecord, NewMaterial,
    NewMessage, NewSchedule, NewScheduleRun, NewSecretRecord,
    NewSession, NewSpace, NewThread, NewToolCall,
    NewTurn, NewWorkflowJournalIntent, NewWorkflowRun, NewWorkflowScript, NewWorkspaceAttachment,
    PendingChannelOrigin, Schedule, ScheduleCreator,
    ScheduleKind, ScheduleRun, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate,
    ScheduleStatus, SecretRecord, Session, SessionPolicy, SkipReason, Space, SpaceSlug,
    Thread, ThreadBranch, ThreadState, ToolCallRecord, Turn, TurnState, UpdateSchedule,
    WorkflowJournalRecord, WorkflowRunRecord, WorkflowScriptRecord, WorkflowScriptVersion,
    WorkspaceAttachment,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMemoryStore, AgentMessageStore, ApprovalStore, AttachmentStore, AuditStore,
    BackgroundJobStore, ChannelLinkStore, ChannelQueueStore, ConfigurationVersionStore,
    ContextSnapshotStore, DeliveryStore, MaterialStore, MessageStore, ScheduleStore,
    SecretStore, SessionStore, SkillActivationStore, SpaceStore, Storage,
    ThreadStore, ToolCallStore, TurnStore, WorkflowStore,
    WorkspaceAttachmentStore,
};
