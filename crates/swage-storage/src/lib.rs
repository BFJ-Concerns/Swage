//! Storage layer with SQLite persistence.

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures;
pub mod model;
pub mod sqlite;
pub mod store;

pub use crate::sqlite::secret::{
    MANAGED_SECRET_KEY_ENV, ManagedSecretEncryptionDiagnostics,
    managed_secret_encryption_diagnostics,
};
pub use model::{
    ActiveSkillRecord, AgentDefinition, AgentMessage, ApprovalKind, ApprovalRequest, ApprovalState,
    Attachment, AttachmentKind, AttachmentScopeKind, AttachmentStatus, AuditEvent, BackgroundJob,
    BackgroundJobKind, BackgroundJobState, ChannelInboundMessage, ChannelInboundMessageState,
    ChannelLink, ContextSnapshotRecord, CurationAssertion, CurationAssertionKind, DeliveryRecord,
    ExtractionDerivation, GateConfig, GateDecision, GateVerdict, InboundAudit, KnowledgeCitation,
    KnowledgeSearchHit, LibraryEntry, LibraryEntryModule, LibrarySeeding, Material,
    MaterialSourceKind, MaterialStatus, Message, MessageOrigin, ModuleSourceReplacement,
    NewAgentDefinition, NewAgentMessage, NewApprovalRequest, NewAttachment, NewAuditEvent,
    NewBackgroundJob, NewChannelInboundMessage, NewChannelLink, NewCurationAssertion,
    NewDeliveryRecord, NewKnowledgeCitation, NewLibraryEntry, NewLibraryEntryModule, NewMaterial,
    NewMessage, NewOrientationCard, NewSchedule, NewScheduleRun, NewSecretRecord, NewSession,
    NewSourceObservation, NewSurface, NewSurfaceAnnotation, NewSurfaceModule, NewTaskDefinition,
    NewThread, NewToolCall, NewTopic, NewTurn, NewWorkflowJournalIntent, NewWorkflowRun,
    NewWorkflowRunEvent, NewWorkflowRunHold, NewWorkflowRunHoldRejection, NewWorkflowScript,
    NewWorkspaceAttachment, ObservationOrigin, OrientationCard, OrientationCardLifecycle,
    OrientationCardLifecycleKind, PendingChannelOrigin, ProviderRoundRecord, RemovedSurfaceModule,
    Schedule, ScheduleCreator, ScheduleKind, ScheduleRun, ScheduleRunState, ScheduleRunTrigger,
    ScheduleRunUpdate, ScheduleStatus, SecretRecord, Session, SessionActivity, SessionListing,
    SessionOrigin, SessionPolicy, SessionVariable, SkipReason, SourceCapture, SourceObservation,
    Surface, SurfaceAnnotation, SurfaceModule, TaskDefinition, Thread, ThreadBranch, ThreadState,
    ToolCallRecord, Topic, TopicSlug, Turn, TurnIsolation, TurnState, UpdateSchedule,
    WorkflowHoldSettlement, WorkflowJournalRecord, WorkflowRunEventRecord, WorkflowRunHoldAnswer,
    WorkflowRunHoldRecord, WorkflowRunHoldRejectionRecord, WorkflowRunRecord,
    WorkflowRunWorkerPlacement, WorkflowScriptRecord, WorkflowScriptVersion, WorkspaceAttachment,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentDefinitionStore, AgentMessageStore, ApprovalStore, AttachmentStore, AuditStore,
    BackgroundJobStore, ChannelLinkStore, ChannelQueueStore, CitationStore, ContextSnapshotStore,
    CurationAssertionStore, DeliveryStore, MaterialStore, MessageStore, ObservationStore,
    OrientationCardStore, ProviderRoundStore, ScheduleStore, SecretStore, SessionStore,
    SessionVariableStore, SkillActivationStore, Storage, SurfaceAnnotationStore, SurfaceStore,
    TaskDefinitionStore, ThreadStore, ToolCallStore, TopicStore, TurnStore, WorkflowStore,
    WorkspaceAttachmentStore,
};

// The turn queue: messages held behind a running turn.
pub use model::{NewQueuedTurnInput, QueuedTurnInput};
pub use store::TurnQueueStore;
