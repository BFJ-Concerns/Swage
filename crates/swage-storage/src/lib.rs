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
    BackgroundJob, BackgroundJobKind, BackgroundJobState, ChannelInboundMessage,
    ChannelInboundMessageState, ChannelLink, ConfigurationVersion, ContextSnapshotRecord,
    DeliveryRecord, EdgeEvaluation, GateConfig, GateDecision, GateVerdict, InboundAudit,
    LensPreference, LensRecord, LensSourceKind, Material, MaterialSourceKind, MaterialStatus,
    Message, NewAgentMemoryRecord, NewAgentMessage, NewApprovalRequest, NewAttachment,
    NewAuditEvent, NewBackgroundJob, NewChannelInboundMessage, NewChannelLink,
    NewConfigurationVersion, NewDeliveryRecord, NewEdgeEvaluation, NewLensRecord, NewMaterial,
    NewMessage, NewNodeExecution, NewNodeRetry, NewSchedule, NewScheduleRun, NewSecretRecord,
    NewSession, NewSharedReferenceEvent, NewSpace, NewTeam, NewThread, NewToolCall, NewTopologyRun,
    NewTurn, NewWorkflowJournalIntent, NewWorkflowRun, NewWorkflowScript, NewWorkspaceAttachment,
    NodeExecution, NodeExecutionState, NodeRetry, PendingChannelOrigin, Schedule, ScheduleCreator,
    ScheduleKind, ScheduleRun, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate,
    ScheduleStatus, SecretRecord, Session, SessionPolicy, SharedReferenceEvent,
    SharedReferenceKind, SkipReason, Space, SpaceSlug, Team, Thread, ThreadBranch, ThreadState,
    ToolCallRecord, TopologyRun, TopologyRunState, Turn, TurnState, UpdateSchedule,
    WorkflowJournalRecord, WorkflowRunRecord, WorkflowScriptRecord, WorkflowScriptVersion,
    WorkspaceAttachment,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentMemoryStore, AgentMessageStore, ApprovalStore, AttachmentStore, AuditStore,
    BackgroundJobStore, ChannelLinkStore, ChannelQueueStore, ConfigurationVersionStore,
    ContextSnapshotStore, DeliveryStore, LensStore, MaterialStore, MessageStore, ScheduleStore,
    SecretStore, SessionStore, SharedReferenceStore, SkillActivationStore, SpaceStore, Storage,
    TeamStore, ThreadStore, ToolCallStore, TopologyTraceStore, TurnStore, WorkflowStore,
    WorkspaceAttachmentStore,
};
