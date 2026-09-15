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
    NewKnowledgeCitation, NewMaterial, NewMessage, NewOrientationCard, NewPage, NewPageAnnotation,
    NewPageModule, NewPageTemplate, NewPageTemplateModule, NewPageTemplateModuleInput, NewSchedule,
    NewScheduleRun, NewSecretRecord, NewSession, NewSourceObservation, NewTaskDefinition,
    NewThread, NewToolCall, NewTopic, NewTurn, NewWorkflowJournalIntent, NewWorkflowRun,
    NewWorkflowRunEvent, NewWorkflowRunHold, NewWorkflowRunHoldRejection, NewWorkflowScript,
    NewWorkspaceAttachment, ObservationOrigin, OrientationCard, OrientationCardLifecycle,
    OrientationCardLifecycleKind, Page, PageAnnotation, PageModule, PageTemplate,
    PageTemplateModule, PendingChannelOrigin, ProviderRoundRecord, Schedule, ScheduleCreator,
    ScheduleKind, ScheduleRun, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate,
    ScheduleStatus, SecretRecord, Session, SessionPolicy, SessionVariable, SkipReason,
    SourceCapture, SourceObservation, TaskDefinition, Thread, ThreadBranch, ThreadState,
    ToolCallRecord, Topic, TopicSlug, Turn, TurnState, UpdateSchedule, WorkflowHoldSettlement,
    WorkflowJournalRecord, WorkflowRunEventRecord, WorkflowRunHoldAnswer, WorkflowRunHoldRecord,
    WorkflowRunHoldRejectionRecord, WorkflowRunRecord, WorkflowRunWorkerPlacement,
    WorkflowScriptRecord, WorkflowScriptVersion, WorkspaceAttachment,
};
pub use sqlite::SqliteStorage;
pub use store::{
    AgentDefinitionStore, AgentMemoryStore, AgentMessageStore, ApprovalStore, AttachmentStore,
    AuditStore, BackgroundJobStore, ChannelLinkStore, ChannelQueueStore, CitationStore,
    ContextSnapshotStore, CurationAssertionStore, DeliveryStore, MaterialStore, MessageStore,
    ObservationStore, OrientationCardStore, PageAnnotationStore, PageStore, ProviderRoundStore,
    ScheduleStore, SecretStore, SessionStore, SessionVariableStore, SkillActivationStore, Storage,
    TaskDefinitionStore, ThreadStore, ToolCallStore, TopicStore, TurnStore, WorkflowStore,
    WorkspaceAttachmentStore,
};
