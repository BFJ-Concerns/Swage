//! Storage layer with SQLite persistence.

pub mod model;
pub mod sqlite;
pub mod store;

pub use crate::sqlite::secret::{
    MANAGED_SECRET_KEY_ENV, ManagedSecretEncryptionDiagnostics,
    managed_secret_encryption_diagnostics,
};
pub use model::{
    ActiveSkillRecord, AgentDefinition, AgentMemoryRecord, AgentMessage, ApprovalKind,
    ApprovalRequest, ApprovalState, Attachment, AttachmentKind, AttachmentScopeKind,
    AttachmentStatus, AuditEvent, BackgroundJob, BackgroundJobKind, BackgroundJobState,
    ChannelInboundMessage, ChannelInboundMessageState, ChannelLink, ContextSnapshotRecord,
    CurationAssertion, CurationAssertionKind, DeliveryRecord, ExtractionDerivation, GateConfig,
    GateDecision, GateVerdict, InboundAudit, KnowledgeCitation, KnowledgeSearchHit, Material,
    MaterialSourceKind, MaterialStatus, Message, NewAgentDefinition, NewAgentMemoryRecord,
    NewAgentMessage, NewApprovalRequest, NewAttachment, NewAuditEvent, NewBackgroundJob,
    NewChannelInboundMessage, NewChannelLink, NewCurationAssertion, NewDeliveryRecord,
    NewKnowledgeCitation, NewMaterial, NewMessage, NewOrientationCard, NewSchedule, NewScheduleRun,
    NewSecretRecord, NewSession, NewSourceObservation, NewSpace, NewThread, NewToolCall, NewTurn,
    NewWorkflowJournalIntent, NewWorkflowRun, NewWorkflowRunEvent, NewWorkflowScript,
    NewWorkspaceAttachment, ObservationOrigin, OrientationCard, OrientationCardLifecycle,
    OrientationCardLifecycleKind, PendingChannelOrigin, Schedule, ScheduleCreator, ScheduleKind,
    ScheduleRun, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate, ScheduleStatus,
    SecretRecord, Session, SessionPolicy, SkipReason, SourceObservation, Space, SpaceSlug, Thread,
    ThreadBranch, ThreadState, ToolCallRecord, Turn, TurnState, UpdateSchedule,
    WorkflowJournalRecord, WorkflowRunEventRecord, WorkflowRunRecord, WorkflowScriptRecord,
    WorkflowScriptVersion, WorkspaceAttachment,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentDefinitionStore, AgentMemoryStore, AgentMessageStore, ApprovalStore, AttachmentStore,
    AuditStore, BackgroundJobStore, ChannelLinkStore, ChannelQueueStore, CitationStore,
    ContextSnapshotStore, CurationAssertionStore, DeliveryStore, MaterialStore, MessageStore,
    ObservationStore, OrientationCardStore, ScheduleStore, SecretStore, SessionStore,
    SkillActivationStore, SpaceStore, Storage, ThreadStore, ToolCallStore, TurnStore,
    WorkflowStore, WorkspaceAttachmentStore,
};
