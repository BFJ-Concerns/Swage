use async_trait::async_trait;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    ActiveSkillRecord, AgentDefinition, AgentMemoryRecord, AgentMessage, AppendMessage,
    ApprovalRequest,
    ApprovalState, Attachment, AttachmentStatus, AuditEvent, BackgroundJob, ChannelInboundMessage,
    ChannelLink, ConfigurationVersion, ContextSnapshotRecord, DeliveryRecord, DeviceRegistration,
    ExtractionDerivation, InboundAudit, KnowledgeSearchHit, Material, Message,
    NewAgentDefinition, NewAgentMemoryRecord, NewAgentMessage, NewApprovalRequest, NewAttachment,
    NewAuditEvent,
    NewBackgroundJob, NewChannelInboundMessage, NewChannelLink, NewConfigurationVersion,
    NewDeliveryRecord, NewDeviceRegistration, NewMaterial,
    NewMessage, NewSchedule, NewScheduleRun, NewSecretRecord,
    NewSession, NewSourceObservation, NewSpace, NewThread, NewToolCall,
    NewTurn, NewWorkflowJournalIntent, NewWorkflowRun, NewWorkflowScript, NewWorkspaceAttachment,
    PendingChannelOrigin, Schedule, ScheduleRun, ScheduleRunUpdate, SecretRecord, Session,
    SourceObservation, Space,
    SpaceSlug, Thread, ThreadBranch, ThreadState, ToolCallRecord, Turn, TurnState, UpdateSchedule,
    WorkflowJournalRecord, WorkflowRunRecord,
    WorkflowScriptRecord, WorkflowScriptVersion, WorkspaceAttachment,
};

// ---------------------------------------------------------------------------
// Store sub-traits
// ---------------------------------------------------------------------------

#[async_trait]
pub trait SpaceStore: Send + Sync {
    async fn create(&self, space: NewSpace) -> Result<Space, StorageError>;
    async fn get(&self, id: Uuid) -> Result<Option<Space>, StorageError>;
    async fn get_by_name(&self, name: &str) -> Result<Option<Space>, StorageError>;
    /// Get the default space, creating it if it does not exist.
    async fn get_or_create_default(&self) -> Result<Space, StorageError>;
    /// Return all spaces ordered by created_at ascending.
    async fn list_all(&self) -> Result<Vec<Space>, StorageError>;
    /// Look up a space by its slug.
    async fn get_by_slug(&self, slug: &str) -> Result<Option<Space>, StorageError>;
    /// Partially update a space. Only `Some` fields are changed.
    ///
    /// Returns the updated space, or `StorageError::NotFound` if `id` is unknown.
    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        slug: Option<SpaceSlug>,
    ) -> Result<Space, StorageError>;
    /// Delete a space and cascade its sessions.
    ///
    /// Returns `true` if the space existed and was removed, `false` if not found.
    /// Returns `StorageError::ConstraintViolation` if called on the default space.
    async fn delete(&self, id: Uuid) -> Result<bool, StorageError>;
}

#[async_trait]
pub trait WorkspaceAttachmentStore: Send + Sync {
    async fn create_workspace_attachment(
        &self,
        workspace: NewWorkspaceAttachment,
    ) -> Result<WorkspaceAttachment, StorageError>;

    async fn get_workspace_attachment(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkspaceAttachment>, StorageError>;

    async fn list_workspace_attachments_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<WorkspaceAttachment>, StorageError>;

    async fn set_workspace_trust(
        &self,
        id: Uuid,
        is_trusted: bool,
    ) -> Result<WorkspaceAttachment, StorageError>;

    async fn set_workspace_missing_state(
        &self,
        id: Uuid,
        is_missing: bool,
    ) -> Result<WorkspaceAttachment, StorageError>;

    async fn set_space_default_workspace(
        &self,
        space_id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<(), StorageError>;

    async fn delete_workspace_attachment(&self, id: Uuid) -> Result<bool, StorageError>;
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError>;
    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError>;
    /// List sessions in the space ordered by updated_at DESC.
    async fn list_by_space(&self, space_id: Uuid) -> Result<Vec<Session>, StorageError>;
    /// List sessions belonging to an agent, ordered by updated_at DESC.
    async fn list_sessions_by_agent_id(&self, agent_id: &str)
    -> Result<Vec<Session>, StorageError>;
    /// Delete a session. Returns true if it existed and was removed.
    async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError>;
    /// Bump the session's updated_at to now.
    async fn update_activity(&self, id: Uuid) -> Result<(), StorageError>;
    async fn set_session_active_workspace(
        &self,
        id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<Session, StorageError>;
    async fn set_session_selected_model(
        &self,
        id: Uuid,
        selected_model_id: Option<String>,
    ) -> Result<Session, StorageError>;
    /// Find a session whose metadata records the given external channel origin.
    async fn find_session_by_channel_origin(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<Session>, StorageError>;
}

#[async_trait]
pub trait DeviceStore: Send + Sync {
    async fn upsert_device(
        &self,
        host: NewDeviceRegistration,
    ) -> Result<DeviceRegistration, StorageError>;
    async fn list_devices(&self) -> Result<Vec<DeviceRegistration>, StorageError>;
    async fn delete_device(&self, device_id: &str) -> Result<bool, StorageError>;
}

#[async_trait]
pub trait ChannelLinkStore: Send + Sync {
    async fn upsert_channel_link(&self, link: NewChannelLink) -> Result<ChannelLink, StorageError>;
    async fn get_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<ChannelLink>, StorageError>;
    async fn list_channel_links(&self) -> Result<Vec<ChannelLink>, StorageError>;
    async fn delete_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<bool, StorageError>;
}

#[async_trait]
pub trait DeliveryStore: Send + Sync {
    async fn create_delivery_record(
        &self,
        delivery: NewDeliveryRecord,
    ) -> Result<DeliveryRecord, StorageError>;
    async fn list_delivery_records(&self) -> Result<Vec<DeliveryRecord>, StorageError>;
    async fn list_delivery_records_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<DeliveryRecord>, StorageError>;
}

#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn create_audit_event(&self, event: NewAuditEvent) -> Result<AuditEvent, StorageError>;
    async fn list_audit_events(&self) -> Result<Vec<AuditEvent>, StorageError>;
    async fn list_audit_events_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<AuditEvent>, StorageError>;
}

#[async_trait]
pub trait AgentMemoryStore: Send + Sync {
    async fn create_agent_memory(
        &self,
        record: NewAgentMemoryRecord,
    ) -> Result<AgentMemoryRecord, StorageError>;
    async fn get_agent_memory(&self, id: Uuid) -> Result<Option<AgentMemoryRecord>, StorageError>;
    async fn list_agent_memories(
        &self,
        space_id: Uuid,
        agent_id: Option<&str>,
        session_id: Option<Uuid>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AgentMemoryRecord>, StorageError>;
}

/// Store for durable agent definitions.
///
/// Writes are whole-definition upserts because mutations arrive through the
/// recorded configuration-change path, which validates and diffs complete
/// definitions rather than field patches.
#[async_trait]
pub trait AgentDefinitionStore: Send + Sync {
    async fn upsert_agent_definition(
        &self,
        definition: NewAgentDefinition,
    ) -> Result<AgentDefinition, StorageError>;

    /// Resolve the definition a session starting as `agent_id` in `space_id`
    /// should use: the space-scoped definition when one exists, else the
    /// global one, else none (the agent runs on resolved config alone).
    async fn resolve_agent_definition(
        &self,
        agent_id: &str,
        space_id: Uuid,
    ) -> Result<Option<AgentDefinition>, StorageError>;

    async fn list_agent_definitions(
        &self,
        space_id: Option<Uuid>,
    ) -> Result<Vec<AgentDefinition>, StorageError>;

    async fn delete_agent_definition(
        &self,
        agent_id: &str,
        space_id: Option<Uuid>,
    ) -> Result<bool, StorageError>;
}

/// Store for a space's knowledge base: immutable source observations and
/// their versioned text extractions, searched by full text.
///
/// Immutability is this store's contract, not a caller convention: nothing
/// here updates or deletes an observation or derivation, and extensions
/// (curation assertions, citations, orientation cards) must keep that shape
/// — later state is always a new row.
#[async_trait]
pub trait ObservationStore: Send + Sync {
    /// Record a capture. Identical bodies within a space deduplicate: the
    /// existing observation is returned, so evidence identity never forks.
    async fn record_observation(
        &self,
        observation: NewSourceObservation,
    ) -> Result<SourceObservation, StorageError>;

    async fn get_observation(
        &self,
        id: Uuid,
    ) -> Result<Option<SourceObservation>, StorageError>;

    async fn list_observations_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<SourceObservation>, StorageError>;

    /// Add the next extraction version for an observation. The store assigns
    /// the version; existing derivations are never touched.
    async fn add_extraction(
        &self,
        observation_id: Uuid,
        extracted_text: String,
        extractor: String,
    ) -> Result<ExtractionDerivation, StorageError>;

    async fn get_extraction(
        &self,
        observation_id: Uuid,
        version: u32,
    ) -> Result<Option<ExtractionDerivation>, StorageError>;

    /// Full-text search across the space's newest derivation per
    /// observation, best matches first. Each hit carries the derivation
    /// version and snippet offsets a citation pins to.
    async fn search_extractions(
        &self,
        space_id: Uuid,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeSearchHit>, StorageError>;
}

#[async_trait]
pub trait ConfigurationVersionStore: Send + Sync {
    async fn create_configuration_version(
        &self,
        version: NewConfigurationVersion,
    ) -> Result<ConfigurationVersion, StorageError>;
    async fn get_configuration_version(
        &self,
        id: Uuid,
    ) -> Result<Option<ConfigurationVersion>, StorageError>;
    async fn list_configuration_versions(
        &self,
        space_id: Option<Uuid>,
    ) -> Result<Vec<ConfigurationVersion>, StorageError>;
    async fn list_configuration_versions_for_entity(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Vec<ConfigurationVersion>, StorageError>;
    async fn get_latest_configuration_version(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Option<ConfigurationVersion>, StorageError>;
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn create_secret(&self, secret: NewSecretRecord) -> Result<SecretRecord, StorageError>;
    async fn list_secrets(&self) -> Result<Vec<SecretRecord>, StorageError>;
    async fn get_secret(&self, id: Uuid) -> Result<Option<SecretRecord>, StorageError>;
    async fn update_secret_value(
        &self,
        id: Uuid,
        value: Option<String>,
        external_reference: Option<String>,
        metadata: serde_json::Value,
    ) -> Result<SecretRecord, StorageError>;
    async fn revoke_secret(&self, id: Uuid) -> Result<SecretRecord, StorageError>;
    async fn restore_secret(&self, id: Uuid) -> Result<SecretRecord, StorageError>;
    async fn get_managed_secret_value(&self, id: Uuid) -> Result<Option<String>, StorageError>;
}

#[async_trait]
pub trait AttachmentStore: Send + Sync {
    async fn queue_attachment_ingestion(
        &self,
        attachment: NewAttachment,
        job: NewBackgroundJob,
    ) -> Result<(Attachment, BackgroundJob), StorageError>;
    async fn get_attachment(&self, id: Uuid) -> Result<Option<Attachment>, StorageError>;
    async fn list_attachments_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError>;
    async fn list_attachments_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError>;
    async fn update_attachment_ingestion(
        &self,
        id: Uuid,
        status: AttachmentStatus,
    ) -> Result<Attachment, StorageError>;
}

#[async_trait]
pub trait BackgroundJobStore: Send + Sync {
    async fn get_background_job(&self, id: Uuid) -> Result<Option<BackgroundJob>, StorageError>;
    async fn list_runnable_background_jobs(&self) -> Result<Vec<BackgroundJob>, StorageError>;
    async fn mark_background_job_running(&self, id: Uuid) -> Result<BackgroundJob, StorageError>;
    async fn complete_background_job(
        &self,
        id: Uuid,
        result: Option<&serde_json::Value>,
    ) -> Result<BackgroundJob, StorageError>;
    async fn fail_background_job(
        &self,
        id: Uuid,
        error_message: &str,
    ) -> Result<BackgroundJob, StorageError>;
    async fn retry_background_job(&self, id: Uuid) -> Result<BackgroundJob, StorageError>;
    async fn list_background_jobs_by_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Vec<BackgroundJob>, StorageError>;
    async fn recover_running_background_jobs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError>;
}

#[async_trait]
pub trait MaterialStore: Send + Sync {
    async fn create_material(&self, material: NewMaterial) -> Result<Material, StorageError>;
    async fn get_material(&self, id: Uuid) -> Result<Option<Material>, StorageError>;
    async fn get_material_by_source_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Option<Material>, StorageError>;
    async fn list_materials_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Material>, StorageError>;
    async fn list_materials_by_space(&self, space_id: Uuid) -> Result<Vec<Material>, StorageError>;
    async fn update_material_content(
        &self,
        id: Uuid,
        content_text: &str,
        mime_type: &str,
    ) -> Result<Material, StorageError>;
    async fn archive_material(&self, id: Uuid) -> Result<Material, StorageError>;
    async fn restore_material(&self, id: Uuid) -> Result<Material, StorageError>;
    async fn promote_material(
        &self,
        id: Uuid,
        scope_kind: crate::model::AttachmentScopeKind,
        session_id: Option<Uuid>,
        space_id: Option<Uuid>,
        relative_path: &str,
    ) -> Result<Material, StorageError>;
}

#[async_trait]
pub trait TurnStore: Send + Sync {
    async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError>;
    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError>;
    /// List all turns for a thread ordered by created_at ascending.
    async fn list_turns_by_thread(&self, thread_id: Uuid) -> Result<Vec<Turn>, StorageError>;
    /// Update a turn's state.
    ///
    /// State is consumer-driven — the storage layer records whatever state is
    /// provided. No state-machine enforcement is applied here.
    async fn update_turn_state(&self, id: Uuid, state: TurnState) -> Result<(), StorageError>;
    /// Find all active turns for a given agent (Pending, Processing, or AwaitingApproval).
    ///
    /// Used by startup recovery, shutdown, and cancel to identify in-flight turns.
    async fn find_in_progress_by_agent(&self, agent_id: &str) -> Result<Vec<Turn>, StorageError>;
    /// Persist the assistant's response text onto a completed turn.
    async fn update_response(&self, id: Uuid, response: &str) -> Result<(), StorageError>;
    /// Persist inbound canonicalisation audit metadata for a turn.
    async fn update_inbound_audit(
        &self,
        id: Uuid,
        inbound_audit: &InboundAudit,
    ) -> Result<(), StorageError>;
    /// Persist token counts after the LLM call completes.
    async fn update_tokens(&self, id: Uuid, input: i64, output: i64) -> Result<(), StorageError>;
    /// Persist the computed cost after pricing is applied.
    ///
    /// `cost` is in microcents (see `PricingConfig::compute_cost`). Only called
    /// when pricing was configured for the model; absent turns remain NULL.
    async fn update_cost(&self, id: Uuid, cost: i64) -> Result<(), StorageError>;
    /// Persist the validated structured output for a completed turn.
    ///
    /// Called when output schema validation succeeds. The `output` value is
    /// serialised as JSON text for storage. Subsequent `get_turn` calls will
    /// return it deserialised in `Turn::structured_output`.
    async fn update_structured_output(
        &self,
        id: Uuid,
        output: &serde_json::Value,
    ) -> Result<(), StorageError>;
}

/// Store for message CRUD operations.
///
/// Messages belong to threads and may optionally reference the turn that
/// produced them. `position` is monotonically increasing within a thread and
/// is used to reconstruct conversation order.
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Persist a new message. Returns the stored record with its assigned id.
    async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError>;

    /// Persist a new message at the end of the thread.
    ///
    /// The storage layer allocates the next monotonic `position` atomically so
    /// concurrent writers cannot race on client-side `len()` calculations.
    async fn append_message(&self, message: AppendMessage) -> Result<Message, StorageError>;

    /// Fetch all messages in a thread ordered by position ascending.
    async fn list_messages_by_thread(&self, thread_id: Uuid) -> Result<Vec<Message>, StorageError>;

    /// Fetch the most recent message in a session, across all its threads.
    ///
    /// Used for conversation summaries. Returns `None` if the session has
    /// no messages yet.
    async fn get_latest_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Message>, StorageError>;

    /// Fetch a single message by id. Returns `None` if not found.
    async fn get_message(&self, id: Uuid) -> Result<Option<Message>, StorageError>;

    /// Delete all messages belonging to a thread.
    ///
    /// Called when the thread itself is deleted, as a complement to the
    /// database's ON DELETE CASCADE (useful when callers need to know the
    /// count of removed rows or trigger application-level cleanup).
    async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError>;
}

#[async_trait]
pub trait ToolCallStore: Send + Sync {
    /// Insert a new tool call record. Returns the persisted record with its assigned id.
    async fn create_tool_call(
        &self,
        tool_call: NewToolCall,
    ) -> Result<ToolCallRecord, StorageError>;

    /// Fetch a single tool call by id. Returns `None` if not found.
    async fn get_tool_call(&self, id: Uuid) -> Result<Option<ToolCallRecord>, StorageError>;

    /// Fetch a single tool call by provider call id, scoped to the owning turn.
    async fn get_tool_call_by_turn_and_call_id(
        &self,
        turn_id: Uuid,
        call_id: &str,
    ) -> Result<Option<ToolCallRecord>, StorageError>;

    /// Write the completed result back onto an in-progress tool call.
    ///
    /// Matched by `call_id` scoped to `turn_id` to avoid cross-turn collisions.
    #[allow(clippy::too_many_arguments)]
    async fn update_tool_call_result(
        &self,
        call_id: &str,
        turn_id: Uuid,
        result: &str,
        status: &str,
        duration_ms: i64,
        truncated: bool,
        total_bytes: Option<i64>,
    ) -> Result<(), StorageError>;

    /// Fetch all tool calls for a turn, ordered by position ascending.
    async fn list_tool_calls_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ToolCallRecord>, StorageError>;
}

/// Store for context assembly report snapshots.
///
/// One snapshot per turn — stores the serialised JSON of an `AssemblyReport`
/// produced by the context assembly engine. Using raw JSON avoids coupling the
/// storage layer to the `AssemblyReport` type.
#[async_trait]
pub trait ContextSnapshotStore: Send + Sync {
    /// Persist an assembly report for `turn_id`. Replaces any existing entry.
    async fn store_snapshot(&self, turn_id: Uuid, report_json: &str) -> Result<(), StorageError>;
    /// Fetch the raw report JSON for `turn_id`. Returns `None` if no snapshot
    /// was stored for that turn.
    async fn get_snapshot(&self, turn_id: Uuid) -> Result<Option<String>, StorageError>;
    /// List recent context snapshots for a space, newest first.
    async fn list_snapshots_by_space(
        &self,
        space_id: Uuid,
        limit: usize,
    ) -> Result<Vec<ContextSnapshotRecord>, StorageError>;
}

/// Store for thread CRUD operations.
///
/// Threads scope a single agent's execution within a session. Each agent
/// invocation gets its own thread; the thread's state machine tracks whether
/// the agent is currently active.
#[async_trait]
pub trait ThreadStore: Send + Sync {
    /// Persist a new thread. Returns the stored record with its assigned id.
    async fn create(&self, thread: NewThread) -> Result<Thread, StorageError>;

    /// Fetch a thread by id. Returns `None` if not found.
    async fn get(&self, id: Uuid) -> Result<Option<Thread>, StorageError>;

    /// List all threads for a session ordered by created_at ascending.
    async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<Thread>, StorageError>;

    /// List all threads owned by an agent, ordered by created_at ascending.
    async fn list_threads_by_agent_id(&self, agent_id: &str) -> Result<Vec<Thread>, StorageError>;

    /// Transition a thread to a new state.
    ///
    /// Implementations must fetch the current state and reject invalid
    /// transitions with `StorageError::ConstraintViolation`. Returns
    /// `StorageError::NotFound` if `id` is unknown.
    async fn update_state(&self, id: Uuid, state: ThreadState) -> Result<(), StorageError>;

    /// Harness thread associated with this Swage thread, when one exists.
    async fn get_codex_thread_id(&self, _id: Uuid) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    /// Replace the harness thread mapping. A changed permission envelope may
    /// deliberately start a fresh Codex thread.
    async fn set_codex_thread_id(
        &self,
        _id: Uuid,
        _codex_thread_id: Option<&str>,
    ) -> Result<(), StorageError> {
        Err(StorageError::ConstraintViolation(
            "Codex thread mapping is unsupported by this store".to_string(),
        ))
    }

    async fn get_codex_tools_json(&self, _id: Uuid) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn set_codex_tools_json(
        &self,
        _id: Uuid,
        _tools_json: Option<&str>,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    /// Delete a thread. Returns `true` if it existed and was removed.
    async fn delete(&self, id: Uuid) -> Result<bool, StorageError>;

    /// Create a branch from `parent_thread_id` at `branched_from_message_id`.
    async fn create_branch(
        &self,
        parent_thread_id: Uuid,
        branched_from_message_id: Uuid,
    ) -> Result<ThreadBranch, StorageError>;

    /// List branch lineage rows for a session ordered by creation time.
    async fn list_branches_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ThreadBranch>, StorageError>;

    /// Fetch the branch lineage row for a child thread, if it is a branch.
    async fn get_branch(&self, child_thread_id: Uuid)
    -> Result<Option<ThreadBranch>, StorageError>;
}

/// Store for per-session skill activation state.
///
/// Tracks which skills are currently active for a session. State survives
/// client disconnections and gateway restarts because it is persisted in
/// the `active_skills` table, which cascades on session delete.
#[async_trait]
pub trait SkillActivationStore: Send + Sync {
    /// Mark a skill as active for a session. Idempotent — calling this for
    /// an already-active skill is a no-op (INSERT OR IGNORE).
    async fn activate_skill(&self, session_id: Uuid, skill_name: &str) -> Result<(), StorageError>;

    /// Remove a skill from the active set.
    ///
    /// Returns `true` if the skill was active and has been removed, or `false`
    /// if the skill was not in the active set (nothing to remove).
    async fn deactivate_skill(
        &self,
        session_id: Uuid,
        skill_name: &str,
    ) -> Result<bool, StorageError>;

    /// List all skills currently active for a session, ordered by activation
    /// time ascending (the order in which they were activated).
    async fn list_active_skills(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ActiveSkillRecord>, StorageError>;

    /// Remove all active skills for a session in one operation.
    ///
    /// Used when resetting or closing a session without deleting it.
    async fn clear_active_skills(&self, session_id: Uuid) -> Result<(), StorageError>;
}

/// Store for human-in-the-loop approval requests.
///
/// Each approval request is created when an agent turn hits an approval gate
/// (tool dispatch or hook). The turn is suspended in
/// `TurnState::AwaitingApproval` until the request reaches a terminal state
/// (Approved / Rejected / Expired).
#[async_trait]
pub trait ApprovalStore: Send + Sync {
    /// Persist a new approval request. Assigns `id`, sets `state = Pending`,
    /// and leaves `decided_at` as `None`. Returns the stored record.
    async fn create_approval(
        &self,
        approval: NewApprovalRequest,
    ) -> Result<ApprovalRequest, StorageError>;

    /// Fetch a single approval request by id. Returns `None` if not found.
    async fn get_approval(&self, id: Uuid) -> Result<Option<ApprovalRequest>, StorageError>;

    /// List all pending approval requests for a specific agent.
    ///
    /// Used by the agent runtime to check which of its own turns are
    /// suspended. Ordered by `created_at` ascending.
    async fn list_pending_by_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<ApprovalRequest>, StorageError>;

    /// List pending approval requests scoped to a specific session.
    ///
    /// Joins through turns → threads → session to return only approvals whose
    /// turn belongs to the given session. Used by the WebSocket handler to
    /// re-emit approvals without leaking cross-session data.
    async fn list_pending_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ApprovalRequest>, StorageError>;

    /// List all pending approval requests across all agents.
    ///
    /// Used during startup recovery and by the gateway to re-emit pending
    /// approvals to newly connected clients.
    async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError>;
    /// List all approval requests across all states.
    async fn list_approvals(&self) -> Result<Vec<ApprovalRequest>, StorageError>;

    /// Transition an approval request from Pending to `state`.
    ///
    /// Sets `decision_reason` and `decided_at` atomically. Returns `true` if
    /// the update succeeded (the request was in Pending state), or `false` if
    /// the request was already in a terminal state (concurrent decision). Only
    /// non-Pending → terminal transitions are valid.
    async fn update_approval_state(
        &self,
        id: Uuid,
        state: ApprovalState,
        reason: Option<&str>,
    ) -> Result<bool, StorageError>;

    /// Mark as Expired any pending approval whose parent turn no longer exists.
    ///
    /// Called during startup recovery to handle the case where an agent was
    /// killed with approvals in flight. Returns the number of rows updated.
    async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError>;

    /// Persist approval memory for a specific session and tool request fingerprint.
    ///
    /// Used for session-scoped auto-approval: once a user approves a matching
    /// request, subsequent identical tool invocations in the same session can
    /// skip the explicit approval gate.
    async fn remember_approval(
        &self,
        session_id: Uuid,
        tool_name: &str,
        request_fingerprint: &str,
    ) -> Result<(), StorageError>;

    /// Return whether a matching approval memory entry exists for this session.
    async fn has_approval_memory(
        &self,
        session_id: Uuid,
        tool_name: &str,
        request_fingerprint: &str,
    ) -> Result<bool, StorageError>;
}

/// Persistent mailbox for inter-agent messages.
///
/// Follows actor-model semantics: messages are store-and-forward for durable
/// kinds (Delegate, Result, Cancel, Shutdown) and at-most-once for Notify.
/// Status transitions are atomic — only one transition from Pending to a
/// terminal state succeeds.
#[async_trait]
pub trait AgentMessageStore: Send + Sync {
    /// Persist a new agent message with status = Pending.
    async fn create_agent_message(
        &self,
        msg: NewAgentMessage,
    ) -> Result<AgentMessage, StorageError>;

    /// Retrieve a single message by ID.
    async fn get_agent_message(&self, id: Uuid) -> Result<Option<AgentMessage>, StorageError>;

    /// List all pending messages for a recipient agent, ordered by creation time.
    ///
    /// Used by the mailbox on agent instantiation to discover queued work.
    async fn list_pending_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<AgentMessage>, StorageError>;

    /// Atomically transition a message to a terminal status (Delivered or Failed).
    ///
    /// Returns `true` if the transition succeeded (message was Pending), `false`
    /// if the message was already in a terminal state (concurrent delivery).
    /// Sets `delivered_at` when transitioning to Delivered.
    async fn update_agent_message_status(
        &self,
        id: Uuid,
        status: swage_core::MessageStatus,
        failure_reason: Option<&str>,
    ) -> Result<bool, StorageError>;

    /// Update the payload of an existing message.
    ///
    /// Used by `DelegationExecutor` to annotate delegate messages with the
    /// child thread_id after creation, enabling scoped cancel fallback.
    async fn update_agent_message_payload(
        &self,
        id: Uuid,
        payload: serde_json::Value,
    ) -> Result<(), StorageError>;

    /// List all messages sharing a correlation ID, ordered by creation time.
    ///
    /// Used to trace delegation chains (Delegate → Result/Cancel).
    async fn list_by_correlation(
        &self,
        correlation_id: Uuid,
    ) -> Result<Vec<AgentMessage>, StorageError>;

    /// Expire pending messages older than `max_age`, transitioning them to
    /// Failed with reason "delivery timeout". Returns the count of expired messages.
    async fn expire_stale_messages(
        &self,
        max_age: std::time::Duration,
    ) -> Result<u64, StorageError>;
}

/// Durable schedule and run storage.
#[async_trait]
pub trait ScheduleStore: Send + Sync {
    async fn create_schedule(&self, schedule: NewSchedule) -> Result<Schedule, StorageError>;

    async fn get_schedule(&self, id: Uuid) -> Result<Option<Schedule>, StorageError>;

    async fn list_schedules(&self) -> Result<Vec<Schedule>, StorageError>;

    async fn list_due_schedules(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Schedule>, StorageError>;

    async fn update_schedule(
        &self,
        id: Uuid,
        update: UpdateSchedule,
    ) -> Result<Schedule, StorageError>;

    async fn delete_schedule(&self, id: Uuid) -> Result<bool, StorageError>;

    async fn create_schedule_run(&self, run: NewScheduleRun) -> Result<ScheduleRun, StorageError>;

    async fn get_schedule_run(&self, id: Uuid) -> Result<Option<ScheduleRun>, StorageError>;

    async fn list_schedule_runs(&self, schedule_id: Uuid)
    -> Result<Vec<ScheduleRun>, StorageError>;

    async fn update_schedule_run(
        &self,
        id: Uuid,
        update: ScheduleRunUpdate,
    ) -> Result<(), StorageError>;

    /// Reap terminal schedule-run rows outside the configured local history
    /// window. Pending and running rows are always retained.
    async fn prune_schedule_runs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        max_age: std::time::Duration,
        max_per_schedule: u32,
    ) -> Result<u64, StorageError>;

    /// Fail every pending/running run — startup repair after an unclean
    /// shutdown orphaned them.
    async fn recover_incomplete_schedule_runs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError>;
}

/// Durable queue for inbound external-channel messages.
#[async_trait]
pub trait ChannelQueueStore: Send + Sync {
    async fn ingest_channel_inbound(
        &self,
        messages: Vec<NewChannelInboundMessage>,
        cursor_integration: &str,
        cursor_account: &str,
        cursor: &str,
    ) -> Result<usize, StorageError>;

    async fn get_channel_cursor(
        &self,
        integration: &str,
        account: &str,
    ) -> Result<Option<String>, StorageError>;

    async fn list_pending_channel_origins(&self)
    -> Result<Vec<PendingChannelOrigin>, StorageError>;

    async fn list_pending_by_origin(
        &self,
        origin: PendingChannelOrigin,
    ) -> Result<Vec<ChannelInboundMessage>, StorageError>;

    async fn begin_channel_turn(
        &self,
        thread_id: Uuid,
        agent_id: &str,
        composed_input: &str,
        message_ids: Vec<Uuid>,
    ) -> Result<Turn, StorageError>;

    async fn discard_channel_messages(
        &self,
        message_ids: Vec<Uuid>,
        reason: &str,
    ) -> Result<u64, StorageError>;

    async fn requeue_presented_for_inflight_turns(&self) -> Result<u64, StorageError>;
}

/// Durable saved workflow definitions and detached run state.
#[async_trait]
pub trait WorkflowStore: Send + Sync {
    async fn create_workflow_script(
        &self,
        script: NewWorkflowScript,
    ) -> Result<(WorkflowScriptRecord, WorkflowScriptVersion), StorageError>;
    async fn append_workflow_script_version(
        &self,
        script_id: Uuid,
        source: &str,
    ) -> Result<WorkflowScriptVersion, StorageError>;
    async fn get_workflow_script(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkflowScriptRecord>, StorageError>;
    async fn get_workflow_script_head(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkflowScriptVersion>, StorageError>;
    async fn get_workflow_script_version(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkflowScriptVersion>, StorageError>;
    async fn update_workflow_script(
        &self,
        id: Uuid,
        description: Option<&str>,
        delivery_grant: Option<serde_json::Value>,
    ) -> Result<WorkflowScriptRecord, StorageError>;
    async fn list_workflow_scripts(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<WorkflowScriptRecord>, StorageError>;
    async fn create_workflow_run(
        &self,
        run: NewWorkflowRun,
    ) -> Result<WorkflowRunRecord, StorageError>;
    async fn get_workflow_run(&self, id: Uuid) -> Result<Option<WorkflowRunRecord>, StorageError>;
    async fn list_workflow_runs(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<WorkflowRunRecord>, StorageError>;
    async fn update_workflow_run_state(
        &self,
        id: Uuid,
        state: &str,
        final_output: Option<serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError>;
    async fn interrupt_running_workflow_runs(&self) -> Result<u64, StorageError>;
    async fn create_workflow_journal_intent(
        &self,
        intent: NewWorkflowJournalIntent,
    ) -> Result<WorkflowJournalRecord, StorageError>;
    async fn complete_workflow_journal_entry(
        &self,
        id: Uuid,
        result: serde_json::Value,
    ) -> Result<WorkflowJournalRecord, StorageError>;
    async fn list_workflow_journal(
        &self,
        run_id: Uuid,
    ) -> Result<Vec<WorkflowJournalRecord>, StorageError>;
}

// ---------------------------------------------------------------------------
// Storage supertrait
// ---------------------------------------------------------------------------

/// Combined storage interface implemented by `SqliteStorage`.
///
/// `SpaceStore` is not included — it is independently available and consumers
/// that need space operations import it separately.
pub trait Storage:
    SessionStore
    + WorkspaceAttachmentStore
    + AttachmentStore
    + BackgroundJobStore
    + MaterialStore
    + DeviceStore
    + ChannelLinkStore
    + DeliveryStore
    + AuditStore
    + AgentMemoryStore
    + AgentDefinitionStore
    + ConfigurationVersionStore
    + ObservationStore
    + SecretStore
    + ThreadStore
    + TurnStore
    + MessageStore
    + ToolCallStore
    + ContextSnapshotStore
    + SkillActivationStore
    + ApprovalStore
    + AgentMessageStore
    + ScheduleStore
    + ChannelQueueStore
    + WorkflowStore
    + Send
    + Sync
{
}

/// Blanket implementation so any type satisfying all sub-traits gets Storage for free.
impl<T> Storage for T where
    T: SessionStore
        + WorkspaceAttachmentStore
        + AttachmentStore
        + BackgroundJobStore
        + MaterialStore
        + DeviceStore
        + ChannelLinkStore
        + DeliveryStore
        + AuditStore
        + AgentMemoryStore
        + AgentDefinitionStore
        + ConfigurationVersionStore
        + ObservationStore
        + SecretStore
        + ThreadStore
        + TurnStore
        + MessageStore
        + ToolCallStore
        + ContextSnapshotStore
        + SkillActivationStore
        + ApprovalStore
        + AgentMessageStore
        + ScheduleStore
        + ChannelQueueStore
        + WorkflowStore
        + Send
        + Sync
{
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewToolCall, ToolCallRecord};
    use crate::store::ContextSnapshotStore;

    /// Compile-time check: a type implementing all sub-traits satisfies Storage.
    ///
    /// This struct exists only to verify the trait bounds compile correctly.
    /// It panics if any method is actually called.
    struct StubStorage;

    #[async_trait]
    impl SpaceStore for StubStorage {
        async fn create(&self, _: NewSpace) -> Result<Space, StorageError> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<Space>, StorageError> {
            unimplemented!()
        }
        async fn get_by_name(&self, _: &str) -> Result<Option<Space>, StorageError> {
            unimplemented!()
        }
        async fn get_or_create_default(&self) -> Result<Space, StorageError> {
            unimplemented!()
        }
        async fn list_all(&self) -> Result<Vec<Space>, StorageError> {
            unimplemented!()
        }
        async fn get_by_slug(&self, _: &str) -> Result<Option<Space>, StorageError> {
            unimplemented!()
        }
        async fn update(
            &self,
            _: Uuid,
            _: Option<String>,
            _: Option<String>,
            _: Option<SpaceSlug>,
        ) -> Result<Space, StorageError> {
            unimplemented!()
        }
        async fn delete(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl SessionStore for StubStorage {
        async fn create_session(&self, _: NewSession) -> Result<Session, StorageError> {
            unimplemented!()
        }
        async fn get_session(&self, _: Uuid) -> Result<Option<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_by_space(&self, _: Uuid) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_sessions_by_agent_id(&self, _: &str) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn delete_session(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_activity(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn set_session_active_workspace(
            &self,
            _: Uuid,
            _: Option<Uuid>,
        ) -> Result<Session, StorageError> {
            unimplemented!()
        }
        async fn set_session_selected_model(
            &self,
            _: Uuid,
            _: Option<String>,
        ) -> Result<Session, StorageError> {
            unimplemented!()
        }
        async fn find_session_by_channel_origin(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<Session>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl DeviceStore for StubStorage {
        async fn upsert_device(
            &self,
            _: NewDeviceRegistration,
        ) -> Result<DeviceRegistration, StorageError> {
            unimplemented!()
        }
        async fn list_devices(&self) -> Result<Vec<DeviceRegistration>, StorageError> {
            unimplemented!()
        }
        async fn delete_device(&self, _: &str) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ChannelLinkStore for StubStorage {
        async fn upsert_channel_link(
            &self,
            _: NewChannelLink,
        ) -> Result<ChannelLink, StorageError> {
            unimplemented!()
        }
        async fn get_channel_link(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<ChannelLink>, StorageError> {
            unimplemented!()
        }
        async fn list_channel_links(&self) -> Result<Vec<ChannelLink>, StorageError> {
            unimplemented!()
        }
        async fn delete_channel_link(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl DeliveryStore for StubStorage {
        async fn create_delivery_record(
            &self,
            _: NewDeliveryRecord,
        ) -> Result<DeliveryRecord, StorageError> {
            unimplemented!()
        }
        async fn list_delivery_records(&self) -> Result<Vec<DeliveryRecord>, StorageError> {
            unimplemented!()
        }
        async fn list_delivery_records_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<DeliveryRecord>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ChannelQueueStore for StubStorage {
        async fn ingest_channel_inbound(
            &self,
            _: Vec<NewChannelInboundMessage>,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<usize, StorageError> {
            unimplemented!()
        }

        async fn get_channel_cursor(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<String>, StorageError> {
            unimplemented!()
        }

        async fn list_pending_channel_origins(
            &self,
        ) -> Result<Vec<PendingChannelOrigin>, StorageError> {
            unimplemented!()
        }

        async fn list_pending_by_origin(
            &self,
            _: PendingChannelOrigin,
        ) -> Result<Vec<ChannelInboundMessage>, StorageError> {
            unimplemented!()
        }

        async fn begin_channel_turn(
            &self,
            _: Uuid,
            _: &str,
            _: &str,
            _: Vec<Uuid>,
        ) -> Result<Turn, StorageError> {
            unimplemented!()
        }

        async fn discard_channel_messages(
            &self,
            _: Vec<Uuid>,
            _: &str,
        ) -> Result<u64, StorageError> {
            unimplemented!()
        }

        async fn requeue_presented_for_inflight_turns(&self) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl AuditStore for StubStorage {
        async fn create_audit_event(&self, _: NewAuditEvent) -> Result<AuditEvent, StorageError> {
            unimplemented!()
        }
        async fn list_audit_events(&self) -> Result<Vec<AuditEvent>, StorageError> {
            unimplemented!()
        }
        async fn list_audit_events_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<AuditEvent>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl AgentMemoryStore for StubStorage {
        async fn create_agent_memory(
            &self,
            _: NewAgentMemoryRecord,
        ) -> Result<AgentMemoryRecord, StorageError> {
            unimplemented!()
        }

        async fn get_agent_memory(
            &self,
            _: Uuid,
        ) -> Result<Option<AgentMemoryRecord>, StorageError> {
            unimplemented!()
        }

        async fn list_agent_memories(
            &self,
            _: Uuid,
            _: Option<&str>,
            _: Option<Uuid>,
            _: Option<&str>,
            _: usize,
        ) -> Result<Vec<AgentMemoryRecord>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ConfigurationVersionStore for StubStorage {
        async fn create_configuration_version(
            &self,
            _: NewConfigurationVersion,
        ) -> Result<ConfigurationVersion, StorageError> {
            unimplemented!()
        }

        async fn get_configuration_version(
            &self,
            _: Uuid,
        ) -> Result<Option<ConfigurationVersion>, StorageError> {
            unimplemented!()
        }

        async fn list_configuration_versions(
            &self,
            _: Option<Uuid>,
        ) -> Result<Vec<ConfigurationVersion>, StorageError> {
            unimplemented!()
        }

        async fn list_configuration_versions_for_entity(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Vec<ConfigurationVersion>, StorageError> {
            unimplemented!()
        }

        async fn get_latest_configuration_version(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<ConfigurationVersion>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl AgentDefinitionStore for StubStorage {
        async fn upsert_agent_definition(
            &self,
            _: NewAgentDefinition,
        ) -> Result<AgentDefinition, StorageError> {
            unimplemented!()
        }

        async fn resolve_agent_definition(
            &self,
            _: &str,
            _: Uuid,
        ) -> Result<Option<AgentDefinition>, StorageError> {
            unimplemented!()
        }

        async fn list_agent_definitions(
            &self,
            _: Option<Uuid>,
        ) -> Result<Vec<AgentDefinition>, StorageError> {
            unimplemented!()
        }

        async fn delete_agent_definition(
            &self,
            _: &str,
            _: Option<Uuid>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ObservationStore for StubStorage {
        async fn record_observation(
            &self,
            _: NewSourceObservation,
        ) -> Result<SourceObservation, StorageError> {
            unimplemented!()
        }

        async fn get_observation(
            &self,
            _: Uuid,
        ) -> Result<Option<SourceObservation>, StorageError> {
            unimplemented!()
        }

        async fn list_observations_by_space(
            &self,
            _: Uuid,
        ) -> Result<Vec<SourceObservation>, StorageError> {
            unimplemented!()
        }

        async fn add_extraction(
            &self,
            _: Uuid,
            _: String,
            _: String,
        ) -> Result<ExtractionDerivation, StorageError> {
            unimplemented!()
        }

        async fn get_extraction(
            &self,
            _: Uuid,
            _: u32,
        ) -> Result<Option<ExtractionDerivation>, StorageError> {
            unimplemented!()
        }

        async fn search_extractions(
            &self,
            _: Uuid,
            _: &str,
            _: usize,
        ) -> Result<Vec<KnowledgeSearchHit>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl SecretStore for StubStorage {
        async fn create_secret(&self, _: NewSecretRecord) -> Result<SecretRecord, StorageError> {
            unimplemented!()
        }
        async fn list_secrets(&self) -> Result<Vec<SecretRecord>, StorageError> {
            unimplemented!()
        }
        async fn get_secret(&self, _: Uuid) -> Result<Option<SecretRecord>, StorageError> {
            unimplemented!()
        }
        async fn update_secret_value(
            &self,
            _: Uuid,
            _: Option<String>,
            _: Option<String>,
            _: serde_json::Value,
        ) -> Result<SecretRecord, StorageError> {
            unimplemented!()
        }
        async fn revoke_secret(&self, _: Uuid) -> Result<SecretRecord, StorageError> {
            unimplemented!()
        }
        async fn restore_secret(&self, _: Uuid) -> Result<SecretRecord, StorageError> {
            unimplemented!()
        }
        async fn get_managed_secret_value(&self, _: Uuid) -> Result<Option<String>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl WorkspaceAttachmentStore for StubStorage {
        async fn create_workspace_attachment(
            &self,
            _: NewWorkspaceAttachment,
        ) -> Result<WorkspaceAttachment, StorageError> {
            unimplemented!()
        }

        async fn get_workspace_attachment(
            &self,
            _: Uuid,
        ) -> Result<Option<WorkspaceAttachment>, StorageError> {
            unimplemented!()
        }

        async fn list_workspace_attachments_by_space(
            &self,
            _: Uuid,
        ) -> Result<Vec<WorkspaceAttachment>, StorageError> {
            unimplemented!()
        }

        async fn set_workspace_trust(
            &self,
            _: Uuid,
            _: bool,
        ) -> Result<WorkspaceAttachment, StorageError> {
            unimplemented!()
        }

        async fn set_workspace_missing_state(
            &self,
            _: Uuid,
            _: bool,
        ) -> Result<WorkspaceAttachment, StorageError> {
            unimplemented!()
        }

        async fn set_space_default_workspace(
            &self,
            _: Uuid,
            _: Option<Uuid>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn delete_workspace_attachment(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl AttachmentStore for StubStorage {
        async fn queue_attachment_ingestion(
            &self,
            _: NewAttachment,
            _: NewBackgroundJob,
        ) -> Result<(Attachment, BackgroundJob), StorageError> {
            unimplemented!()
        }

        async fn get_attachment(&self, _: Uuid) -> Result<Option<Attachment>, StorageError> {
            unimplemented!()
        }

        async fn list_attachments_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<Attachment>, StorageError> {
            unimplemented!()
        }

        async fn list_attachments_by_space(
            &self,
            _: Uuid,
        ) -> Result<Vec<Attachment>, StorageError> {
            unimplemented!()
        }

        async fn update_attachment_ingestion(
            &self,
            _: Uuid,
            _: AttachmentStatus,
        ) -> Result<Attachment, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl BackgroundJobStore for StubStorage {
        async fn get_background_job(&self, _: Uuid) -> Result<Option<BackgroundJob>, StorageError> {
            unimplemented!()
        }

        async fn list_runnable_background_jobs(&self) -> Result<Vec<BackgroundJob>, StorageError> {
            unimplemented!()
        }

        async fn mark_background_job_running(
            &self,
            _: Uuid,
        ) -> Result<BackgroundJob, StorageError> {
            unimplemented!()
        }

        async fn complete_background_job(
            &self,
            _: Uuid,
            _: Option<&serde_json::Value>,
        ) -> Result<BackgroundJob, StorageError> {
            unimplemented!()
        }

        async fn fail_background_job(
            &self,
            _: Uuid,
            _: &str,
        ) -> Result<BackgroundJob, StorageError> {
            unimplemented!()
        }

        async fn retry_background_job(&self, _: Uuid) -> Result<BackgroundJob, StorageError> {
            unimplemented!()
        }

        async fn list_background_jobs_by_attachment(
            &self,
            _: Uuid,
        ) -> Result<Vec<BackgroundJob>, StorageError> {
            unimplemented!()
        }

        async fn recover_running_background_jobs(&self, _: &str) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl MaterialStore for StubStorage {
        async fn create_material(&self, _: NewMaterial) -> Result<Material, StorageError> {
            unimplemented!()
        }

        async fn get_material(&self, _: Uuid) -> Result<Option<Material>, StorageError> {
            unimplemented!()
        }

        async fn get_material_by_source_attachment(
            &self,
            _: Uuid,
        ) -> Result<Option<Material>, StorageError> {
            unimplemented!()
        }

        async fn list_materials_by_session(&self, _: Uuid) -> Result<Vec<Material>, StorageError> {
            unimplemented!()
        }

        async fn list_materials_by_space(&self, _: Uuid) -> Result<Vec<Material>, StorageError> {
            unimplemented!()
        }

        async fn update_material_content(
            &self,
            _: Uuid,
            _: &str,
            _: &str,
        ) -> Result<Material, StorageError> {
            unimplemented!()
        }

        async fn archive_material(&self, _: Uuid) -> Result<Material, StorageError> {
            unimplemented!()
        }

        async fn restore_material(&self, _: Uuid) -> Result<Material, StorageError> {
            unimplemented!()
        }

        async fn promote_material(
            &self,
            _: Uuid,
            _: crate::model::AttachmentScopeKind,
            _: Option<Uuid>,
            _: Option<Uuid>,
            _: &str,
        ) -> Result<Material, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TurnStore for StubStorage {
        async fn create_turn(&self, _: NewTurn) -> Result<Turn, StorageError> {
            unimplemented!()
        }
        async fn get_turn(&self, _: Uuid) -> Result<Option<Turn>, StorageError> {
            unimplemented!()
        }
        async fn list_turns_by_thread(&self, _thread_id: Uuid) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_turn_state(&self, _: Uuid, _: TurnState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn find_in_progress_by_agent(
            &self,
            _agent_id: &str,
        ) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_response(&self, _id: Uuid, _response: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_tokens(
            &self,
            _id: Uuid,
            _input: i64,
            _output: i64,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_cost(&self, _id: Uuid, _cost: i64) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_inbound_audit(
            &self,
            _id: Uuid,
            _inbound_audit: &InboundAudit,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_structured_output(
            &self,
            _id: Uuid,
            _output: &serde_json::Value,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ContextSnapshotStore for StubStorage {
        async fn store_snapshot(
            &self,
            _turn_id: Uuid,
            _report_json: &str,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn get_snapshot(&self, _turn_id: Uuid) -> Result<Option<String>, StorageError> {
            unimplemented!()
        }
        async fn list_snapshots_by_space(
            &self,
            _space_id: Uuid,
            _limit: usize,
        ) -> Result<Vec<ContextSnapshotRecord>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl MessageStore for StubStorage {
        async fn create_message(&self, _: NewMessage) -> Result<Message, StorageError> {
            unimplemented!()
        }
        async fn append_message(&self, _: AppendMessage) -> Result<Message, StorageError> {
            unimplemented!()
        }
        async fn list_messages_by_thread(&self, _: Uuid) -> Result<Vec<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_latest_for_session(&self, _: Uuid) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_message(&self, _: Uuid) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn delete_by_thread(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ThreadStore for StubStorage {
        async fn create(&self, _: NewThread) -> Result<Thread, StorageError> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_by_session(&self, _: Uuid) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_threads_by_agent_id(&self, _: &str) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn update_state(&self, _: Uuid, _: ThreadState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn delete(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn create_branch(&self, _: Uuid, _: Uuid) -> Result<ThreadBranch, StorageError> {
            unimplemented!()
        }
        async fn list_branches_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<ThreadBranch>, StorageError> {
            unimplemented!()
        }
        async fn get_branch(&self, _: Uuid) -> Result<Option<ThreadBranch>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ToolCallStore for StubStorage {
        async fn create_tool_call(&self, _: NewToolCall) -> Result<ToolCallRecord, StorageError> {
            unimplemented!()
        }
        async fn get_tool_call(&self, _: Uuid) -> Result<Option<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
        async fn get_tool_call_by_turn_and_call_id(
            &self,
            _: Uuid,
            _: &str,
        ) -> Result<Option<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
        async fn update_tool_call_result(
            &self,
            _call_id: &str,
            _turn_id: Uuid,
            _result: &str,
            _status: &str,
            _duration_ms: i64,
            _truncated: bool,
            _total_bytes: Option<i64>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_tool_calls_by_turn(
            &self,
            _: Uuid,
        ) -> Result<Vec<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl SkillActivationStore for StubStorage {
        async fn activate_skill(&self, _: Uuid, _: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn deactivate_skill(&self, _: Uuid, _: &str) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn list_active_skills(
            &self,
            _: Uuid,
        ) -> Result<Vec<ActiveSkillRecord>, StorageError> {
            unimplemented!()
        }
        async fn clear_active_skills(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ApprovalStore for StubStorage {
        async fn create_approval(
            &self,
            _: NewApprovalRequest,
        ) -> Result<ApprovalRequest, StorageError> {
            unimplemented!()
        }
        async fn get_approval(&self, _: Uuid) -> Result<Option<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_by_agent(
            &self,
            _: &str,
        ) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_approvals(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn update_approval_state(
            &self,
            _: Uuid,
            _: ApprovalState,
            _: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError> {
            unimplemented!()
        }
        async fn remember_approval(&self, _: Uuid, _: &str, _: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn has_approval_memory(
            &self,
            _: Uuid,
            _: &str,
            _: &str,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ScheduleStore for StubStorage {
        async fn create_schedule(&self, _: NewSchedule) -> Result<Schedule, StorageError> {
            unimplemented!()
        }
        async fn get_schedule(&self, _: Uuid) -> Result<Option<Schedule>, StorageError> {
            unimplemented!()
        }
        async fn list_schedules(&self) -> Result<Vec<Schedule>, StorageError> {
            unimplemented!()
        }
        async fn list_due_schedules(
            &self,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<Vec<Schedule>, StorageError> {
            unimplemented!()
        }
        async fn update_schedule(
            &self,
            _: Uuid,
            _: UpdateSchedule,
        ) -> Result<Schedule, StorageError> {
            unimplemented!()
        }
        async fn delete_schedule(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn create_schedule_run(
            &self,
            _: NewScheduleRun,
        ) -> Result<ScheduleRun, StorageError> {
            unimplemented!()
        }
        async fn get_schedule_run(&self, _: Uuid) -> Result<Option<ScheduleRun>, StorageError> {
            unimplemented!()
        }
        async fn list_schedule_runs(&self, _: Uuid) -> Result<Vec<ScheduleRun>, StorageError> {
            unimplemented!()
        }
        async fn update_schedule_run(
            &self,
            _: Uuid,
            _: ScheduleRunUpdate,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }

        async fn prune_schedule_runs(
            &self,
            _: chrono::DateTime<chrono::Utc>,
            _: std::time::Duration,
            _: u32,
        ) -> Result<u64, StorageError> {
            unimplemented!()
        }

        async fn recover_incomplete_schedule_runs(&self, _: &str) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl AgentMessageStore for StubStorage {
        async fn create_agent_message(
            &self,
            _: NewAgentMessage,
        ) -> Result<AgentMessage, StorageError> {
            unimplemented!()
        }
        async fn get_agent_message(&self, _: Uuid) -> Result<Option<AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_for_agent(&self, _: &str) -> Result<Vec<AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn update_agent_message_status(
            &self,
            _: Uuid,
            _: swage_core::MessageStatus,
            _: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_agent_message_payload(
            &self,
            _: Uuid,
            _: serde_json::Value,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_by_correlation(&self, _: Uuid) -> Result<Vec<AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn expire_stale_messages(&self, _: std::time::Duration) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl WorkflowStore for StubStorage {
        async fn create_workflow_script(
            &self,
            _: NewWorkflowScript,
        ) -> Result<(WorkflowScriptRecord, WorkflowScriptVersion), StorageError> {
            unimplemented!()
        }
        async fn append_workflow_script_version(
            &self,
            _: Uuid,
            _: &str,
        ) -> Result<WorkflowScriptVersion, StorageError> {
            unimplemented!()
        }
        async fn get_workflow_script(
            &self,
            _: Uuid,
        ) -> Result<Option<WorkflowScriptRecord>, StorageError> {
            unimplemented!()
        }
        async fn get_workflow_script_head(
            &self,
            _: Uuid,
        ) -> Result<Option<WorkflowScriptVersion>, StorageError> {
            unimplemented!()
        }
        async fn get_workflow_script_version(
            &self,
            _: Uuid,
        ) -> Result<Option<WorkflowScriptVersion>, StorageError> {
            unimplemented!()
        }
        async fn update_workflow_script(
            &self,
            _: Uuid,
            _: Option<&str>,
            _: Option<serde_json::Value>,
        ) -> Result<WorkflowScriptRecord, StorageError> {
            unimplemented!()
        }
        async fn list_workflow_scripts(
            &self,
            _: Uuid,
        ) -> Result<Vec<WorkflowScriptRecord>, StorageError> {
            unimplemented!()
        }
        async fn create_workflow_run(
            &self,
            _: NewWorkflowRun,
        ) -> Result<WorkflowRunRecord, StorageError> {
            unimplemented!()
        }
        async fn get_workflow_run(
            &self,
            _: Uuid,
        ) -> Result<Option<WorkflowRunRecord>, StorageError> {
            unimplemented!()
        }
        async fn list_workflow_runs(
            &self,
            _: Uuid,
        ) -> Result<Vec<WorkflowRunRecord>, StorageError> {
            unimplemented!()
        }
        async fn update_workflow_run_state(
            &self,
            _: Uuid,
            _: &str,
            _: Option<serde_json::Value>,
            _: Option<&str>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn interrupt_running_workflow_runs(&self) -> Result<u64, StorageError> {
            unimplemented!()
        }
        async fn create_workflow_journal_intent(
            &self,
            _: NewWorkflowJournalIntent,
        ) -> Result<WorkflowJournalRecord, StorageError> {
            unimplemented!()
        }
        async fn complete_workflow_journal_entry(
            &self,
            _: Uuid,
            _: serde_json::Value,
        ) -> Result<WorkflowJournalRecord, StorageError> {
            unimplemented!()
        }
        async fn list_workflow_journal(
            &self,
            _: Uuid,
        ) -> Result<Vec<WorkflowJournalRecord>, StorageError> {
            unimplemented!()
        }
    }

    // --- MessageStore: thread-ownership API compile checks ---

    /// list_messages_by_thread exists on MessageStore and is callable via T: MessageStore.
    #[test]
    fn message_store_list_messages_by_thread_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            std::mem::drop(t.list_messages_by_thread(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// get_message exists on MessageStore.
    #[test]
    fn message_store_get_message_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            std::mem::drop(t.get_message(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// delete_by_thread exists on MessageStore.
    #[test]
    fn message_store_delete_by_thread_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            std::mem::drop(t.delete_by_thread(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// get_latest_for_session (renamed from get_latest_message_for_session) exists.
    #[test]
    fn message_store_get_latest_for_session_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            std::mem::drop(t.get_latest_for_session(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Verify that a type implementing all sub-traits satisfies the Storage supertrait.
    #[test]
    fn storage_supertrait_includes_tool_call_store() {
        // This is a compile-time check: if Storage doesn't require ToolCallStore,
        // the blanket impl and this constraint would be inconsistent.
        fn _assert_is_storage<T: Storage>(_: &T) {}
        let stub = StubStorage;
        _assert_is_storage(&stub);
    }

    /// Compile-time check: StubStorage implements ThreadStore.
    #[test]
    fn thread_store_compile_check() {
        fn _assert_is_thread_store<T: ThreadStore>(_: &T) {}
        let stub = StubStorage;
        _assert_is_thread_store(&stub);
    }

    /// Compile-time check: Storage implies ThreadStore.
    ///
    /// `update_state` is unique to `ThreadStore`. Calling it via a
    /// `T: Storage` bound only compiles when Storage ⊇ ThreadStore.
    #[test]
    fn storage_supertrait_includes_thread_store() {
        fn _assert<T: Storage>(t: &T) {
            std::mem::drop(t.update_state(Uuid::new_v4(), ThreadState::Processing));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: TurnStore has list_turns_by_thread and find_in_progress_by_agent.
    #[test]
    fn turn_store_new_methods_compile_check() {
        fn _assert_turn_store<T: TurnStore>(_: &T) {}
        let stub = StubStorage;
        _assert_turn_store(&stub);
        // The existence of the impl above is enough — if the new methods are missing
        // from StubStorage, the trait impl won't compile.
    }

    /// Compile-time check: TurnStore has update_cost.
    #[test]
    fn turn_store_update_cost_compile_check() {
        fn _assert<T: TurnStore>(t: &T) {
            std::mem::drop(t.update_cost(Uuid::new_v4(), 42));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: ContextSnapshotStore is accessible and has the expected methods.
    #[test]
    fn context_snapshot_store_compile_check() {
        fn _assert<T: ContextSnapshotStore>(t: &T) {
            std::mem::drop(t.store_snapshot(Uuid::new_v4(), "{\"total_tokens\":100}"));
            std::mem::drop(t.get_snapshot(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: SkillActivationStore has the expected methods.
    #[test]
    fn skill_activation_store_compile_check() {
        fn _assert<T: SkillActivationStore>(t: &T) {
            std::mem::drop(t.activate_skill(Uuid::new_v4(), "forge"));
            std::mem::drop(t.deactivate_skill(Uuid::new_v4(), "forge"));
            std::mem::drop(t.list_active_skills(Uuid::new_v4()));
            std::mem::drop(t.clear_active_skills(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: Storage implies SkillActivationStore.
    #[test]
    fn storage_supertrait_includes_skill_activation_store() {
        fn _assert<T: Storage>(t: &T) {
            std::mem::drop(t.activate_skill(Uuid::new_v4(), "forge"));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: ApprovalStore has the expected methods.
    #[test]
    fn approval_store_compile_check() {
        fn _assert<T: ApprovalStore>(t: &T) {
            std::mem::drop(t.get_approval(Uuid::new_v4()));
            std::mem::drop(t.list_pending_by_agent("agent-1"));
            std::mem::drop(t.list_all_pending());
            std::mem::drop(t.list_approvals());
            std::mem::drop(t.cleanup_orphaned_approvals());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: Storage implies ApprovalStore.
    ///
    /// `list_all_pending` is unique to ApprovalStore. Calling it via a
    /// `T: Storage` bound only compiles when Storage ⊇ ApprovalStore.
    #[test]
    fn storage_supertrait_includes_approval_store() {
        fn _assert<T: Storage>(t: &T) {
            std::mem::drop(t.list_all_pending());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    #[test]
    fn delivery_store_compile_check() {
        fn _assert<T: DeliveryStore>(t: &T) {
            std::mem::drop(t.list_delivery_records());
            std::mem::drop(t.list_delivery_records_by_session(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    #[test]
    fn audit_store_compile_check() {
        fn _assert<T: AuditStore>(t: &T) {
            std::mem::drop(t.create_audit_event(NewAuditEvent {
                id: "audit".into(),
                session_id: None,
                entity_kind: "secret".into(),
                entity_id: "1".into(),
                event_kind: "created".into(),
                summary: "created".into(),
                metadata: serde_json::json!({}),
            }));
            std::mem::drop(t.list_audit_events());
            std::mem::drop(t.list_audit_events_by_session(Uuid::new_v4()));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    #[test]
    fn agent_memory_store_compile_check() {
        fn _assert<T: AgentMemoryStore>(t: &T) {
            let id = Uuid::new_v4();
            std::mem::drop(t.create_agent_memory(NewAgentMemoryRecord {
                id,
                space_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                turn_id: Uuid::new_v4(),
                agent_id: "agent-a".into(),
                memory_kind: "insight".into(),
                title: "Remember this".into(),
                content: "content".into(),
                tags: vec!["tag".into()],
                outcome_status: Some("succeeded".into()),
                source_summary: "turn completed".into(),
                metadata: serde_json::json!({}),
            }));
            std::mem::drop(t.get_agent_memory(id));
            std::mem::drop(t.list_agent_memories(
                Uuid::new_v4(),
                Some("agent-a"),
                None,
                Some("tag"),
                10,
            ));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    #[test]
    fn configuration_version_store_compile_check() {
        fn _assert<T: ConfigurationVersionStore>(t: &T) {
            let id = Uuid::new_v4();
            std::mem::drop(t.create_configuration_version(NewConfigurationVersion {
                id,
                space_id: Some(Uuid::new_v4()),
                session_id: None,
                entity_kind: "space".into(),
                entity_id: "space-1".into(),
                version_number: 1,
                operation_kind: "create_space".into(),
                source: "test".into(),
                summary: "created space".into(),
                snapshot: serde_json::json!({"name": "workspace-core"}),
                diff: serde_json::json!([]),
                previous_version_id: None,
                rollback_to_version_id: None,
            }));
            std::mem::drop(t.get_configuration_version(id));
            std::mem::drop(t.list_configuration_versions(Some(Uuid::new_v4())));
            std::mem::drop(t.list_configuration_versions_for_entity("space", "space-1"));
            std::mem::drop(t.get_latest_configuration_version("space", "space-1"));
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    #[test]
    fn secret_store_compile_check() {
        fn _assert<T: SecretStore>(t: &T) {
            let id = Uuid::new_v4();
            std::mem::drop(t.create_secret(NewSecretRecord {
                id,
                name: "provider".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: "managed".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                external_reference: None,
                metadata: serde_json::json!({}),
                value: Some("secret".into()),
            }));
            std::mem::drop(t.list_secrets());
            std::mem::drop(t.get_secret(id));
            std::mem::drop(t.update_secret_value(
                id,
                Some("rotated".into()),
                None,
                serde_json::json!({}),
            ));
            std::mem::drop(t.revoke_secret(id));
            std::mem::drop(t.restore_secret(id));
            std::mem::drop(t.get_managed_secret_value(id));
        }
        let stub = StubStorage;
        _assert(&stub);
    }
}
