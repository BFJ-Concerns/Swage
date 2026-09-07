use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use swage_core::{Role, StorageError};
use uuid::Uuid;

/// One durable string value scoped to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionVariable {
    pub session_id: Uuid,
    pub name: String,
    pub value: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Persisted context snapshot metadata for space-scoped listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextSnapshotRecord {
    pub turn_id: Uuid,
    pub session_id: Uuid,
    pub space_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub report_json: String,
}

/// Turn lifecycle state.
///
/// State transitions are consumer-driven — the storage layer records whatever
/// state the consumer sets without enforcing a state machine. Enforcement is the
/// responsibility of the agent runtime (swage-agent).
///
/// Lifecycle: Pending (created) → Processing (LLM call started), potentially
/// suspended in AwaitingApproval or Delegating, then terminal (Completed /
/// Failed / Interrupted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    /// Turn created; LLM call not yet started.
    Pending,
    /// LLM call in progress.
    Processing,
    /// Turn finished successfully.
    Completed,
    /// Turn ended with an error.
    Failed,
    /// Turn was cut short (e.g. by a stop signal).
    Interrupted,
    /// Turn is suspended, waiting for human review of a tool call or
    /// hook-triggered approval request.
    ///
    /// State transitions:
    ///   Processing → AwaitingApproval (approval gate suspends the turn)
    ///   AwaitingApproval → Processing  (approved or rejected — turn resumes)
    ///   AwaitingApproval → Interrupted (timeout with interrupt policy)
    ///
    /// Stored as "awaiting_approval" in the SQLite text column — no ALTER TABLE
    /// needed to introduce this state.
    AwaitingApproval,

    /// Turn is suspended while a tool-as-delegation is in progress.
    ///
    /// The parent agent's turn pauses while the delegate agent executes. On
    /// completion the turn transitions back to Processing. On gateway restart,
    /// turns in this state are transitioned to Interrupted (the in-flight
    /// delegation was lost).
    ///
    /// State transitions:
    ///   Processing → Delegating     (delegate tool invoked)
    ///   Delegating → Processing     (delegation completed — turn resumes)
    ///   Delegating → Interrupted    (gateway restart recovery)
    Delegating,
}

impl TurnState {
    /// Whether no more work may run as part of this turn.
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed | Self::Interrupted => true,
            Self::Pending | Self::Processing | Self::AwaitingApproval | Self::Delegating => false,
        }
    }
}

impl std::fmt::Display for TurnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TurnState::Pending => "pending",
            TurnState::Processing => "processing",
            TurnState::Completed => "completed",
            TurnState::Failed => "failed",
            TurnState::Interrupted => "interrupted",
            TurnState::AwaitingApproval => "awaiting_approval",
            TurnState::Delegating => "delegating",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TurnState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(TurnState::Pending),
            "processing" => Ok(TurnState::Processing),
            "completed" => Ok(TurnState::Completed),
            "failed" => Ok(TurnState::Failed),
            "interrupted" => Ok(TurnState::Interrupted),
            "awaiting_approval" => Ok(TurnState::AwaitingApproval),
            "delegating" => Ok(TurnState::Delegating),
            other => Err(format!("unknown turn state: {other}")),
        }
    }
}

/// Filesystem-safe identifier for a space directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpaceSlug(String);

impl SpaceSlug {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.is_empty()
            || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            || !value.chars().any(|c| c.is_ascii_alphanumeric())
        {
            return Err(
                "space slug must contain ASCII letters or digits and may only use hyphens"
                    .to_string(),
            );
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SpaceSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::ops::Deref for SpaceSlug {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for SpaceSlug {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<std::path::Path> for SpaceSlug {
    fn as_ref(&self) -> &std::path::Path {
        std::path::Path::new(self.as_str())
    }
}

impl FromStr for SpaceSlug {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl From<&str> for SpaceSlug {
    fn from(value: &str) -> Self {
        Self::new(value).expect("space slug literals must be valid")
    }
}

impl From<String> for SpaceSlug {
    fn from(value: String) -> Self {
        Self::new(value).expect("space slug strings must be valid")
    }
}

impl PartialEq<&str> for SpaceSlug {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// A space groups conversations and provides configuration scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: Uuid,
    pub name: String,
    /// Short operator-authored summary shown in space pickers and previews.
    pub description: String,
    /// True for the single built-in fallback space.
    ///
    /// This is structural identity, not presentation. The space may be
    /// renamed without ceasing to be the default space.
    pub is_default: bool,
    /// Filesystem-safe identifier used for the space's directory under
    /// `~/.swage/spaces/{slug}/`.
    pub slug: SpaceSlug,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// External resource attached to a space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceAttachment {
    pub id: Uuid,
    pub space_id: Uuid,
    pub label: String,
    pub path: String,
    pub canonical_path: String,
    pub is_missing: bool,
    pub is_default: bool,
    pub permissions: Vec<String>,
    pub is_trusted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A session (conversation) within a space, scoped to a single agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub space_id: Uuid,
    /// The agent that owns this session. Used to scope queries in multi-agent
    /// deployments so each agent sees only its own sessions.
    pub agent_id: String,
    /// Host-managed opaque metadata associated with the session.
    ///
    /// This stays outside prompt and WebSocket transport surfaces unless a
    /// caller explicitly reads it through storage or a higher-level API.
    pub metadata: Option<serde_json::Value>,
    /// Optional configured model key selected for this conversation.
    pub selected_model_id: Option<String>,
    pub active_workspace_attachment_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A persisted non-local device registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistration {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: String,
    pub shared_token: String,
    pub workspace_root: String,
    pub workspace_roots: Vec<String>,
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

/// A durable onboarding link for an external communication channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelLink {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub space_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub home_target: Option<serde_json::Value>,
    pub linked_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A durable record of a resolved outbound delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub id: Uuid,
    /// The session that produced a delivery. Absent when a schedule never
    /// reached a session and records why it made no delivery.
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub target: serde_json::Value,
    pub content: String,
    pub status: String,
    pub error: Option<String>,
    pub delivered_at: DateTime<Utc>,
}

/// Durable inbound channel message state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelInboundMessageState {
    Pending,
    Presented,
    Discarded,
}

impl std::fmt::Display for ChannelInboundMessageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Presented => "presented",
            Self::Discarded => "discarded",
        })
    }
}

impl std::str::FromStr for ChannelInboundMessageState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "presented" => Ok(Self::Presented),
            "discarded" => Ok(Self::Discarded),
            other => Err(format!("unknown channel inbound state: {other}")),
        }
    }
}

/// One durable inbound channel message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInboundMessage {
    pub id: Uuid,
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub external_id: String,
    pub sender: String,
    pub content: String,
    pub state: ChannelInboundMessageState,
    pub discard_reason: Option<String>,
    pub turn_id: Option<Uuid>,
    pub received_at: DateTime<Utc>,
    pub presented_at: Option<DateTime<Utc>>,
}

/// A pending channel origin that has messages ready to drain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingChannelOrigin {
    pub integration: String,
    pub account: String,
    pub conversation: String,
}

/// Input for inserting a durable inbound channel message.
#[derive(Debug, Clone)]
pub struct NewChannelInboundMessage {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub external_id: String,
    pub sender: String,
    pub content: String,
}

/// A durable audit-trail event for operator inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub session_id: Option<Uuid>,
    pub entity_kind: String,
    pub entity_id: String,
    pub event_kind: String,
    pub summary: String,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// A durable agent-authored memory record tied to concrete run provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMemoryRecord {
    pub id: Uuid,
    pub space_id: Uuid,
    pub session_id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub memory_kind: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub outcome_status: Option<String>,
    pub source_summary: String,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a durable agent-memory record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewAgentMemoryRecord {
    pub id: Uuid,
    pub space_id: Uuid,
    pub session_id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub memory_kind: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub outcome_status: Option<String>,
    pub source_summary: String,
    pub metadata: serde_json::Value,
}

/// A centrally managed secret or credential reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRecord {
    pub id: Uuid,
    pub name: String,
    pub material_kind: String,
    pub owner_kind: String,
    pub backend_kind: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub reference: String,
    pub external_reference: Option<String>,
    pub metadata: serde_json::Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Input for creating a new secret record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSecretRecord {
    pub id: Uuid,
    pub name: String,
    pub material_kind: String,
    pub owner_kind: String,
    pub backend_kind: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub external_reference: Option<String>,
    pub metadata: serde_json::Value,
    pub value: Option<String>,
}

/// Attachment scope for user-supplied material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentScopeKind {
    Session,
    Space,
}

impl std::fmt::Display for AttachmentScopeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AttachmentScopeKind::Session => "session",
            AttachmentScopeKind::Space => "space",
        };
        write!(f, "{s}")
    }
}

impl FromStr for AttachmentScopeKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "session" => Ok(AttachmentScopeKind::Session),
            "space" => Ok(AttachmentScopeKind::Space),
            other => Err(format!("unknown attachment scope: {other}")),
        }
    }
}

/// Stable attachment kind used by ingestion and capability gating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Document,
    Image,
}

impl std::fmt::Display for AttachmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AttachmentKind::Document => "document",
            AttachmentKind::Image => "image",
        };
        write!(f, "{s}")
    }
}

impl FromStr for AttachmentKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "document" => Ok(AttachmentKind::Document),
            "image" => Ok(AttachmentKind::Image),
            other => Err(format!("unknown attachment kind: {other}")),
        }
    }
}

/// Attachment ingestion lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentStatus {
    Pending,
    Ready,
    Failed,
}

impl std::fmt::Display for AttachmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AttachmentStatus::Pending => "pending",
            AttachmentStatus::Ready => "ready",
            AttachmentStatus::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl FromStr for AttachmentStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(AttachmentStatus::Pending),
            "ready" => Ok(AttachmentStatus::Ready),
            "failed" => Ok(AttachmentStatus::Failed),
            other => Err(format!("unknown attachment status: {other}")),
        }
    }
}

/// A durable attachment for session or space context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Uuid,
    pub scope_kind: AttachmentScopeKind,
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub kind: AttachmentKind,
    pub status: AttachmentStatus,
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub byte_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewAttachment {
    pub id: Uuid,
    pub scope_kind: AttachmentScopeKind,
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub kind: AttachmentKind,
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// Background-job kind. Currently used for attachment ingestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobKind {
    AttachmentIngestion,
}

impl std::fmt::Display for BackgroundJobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BackgroundJobKind::AttachmentIngestion => "attachment_ingestion",
        };
        write!(f, "{s}")
    }
}

impl FromStr for BackgroundJobKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "attachment_ingestion" => Ok(BackgroundJobKind::AttachmentIngestion),
            other => Err(format!("unknown background job kind: {other}")),
        }
    }
}

/// Durable background-job lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobState {
    Queued,
    Running,
    Retried,
    Completed,
    Failed,
}

impl std::fmt::Display for BackgroundJobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BackgroundJobState::Queued => "queued",
            BackgroundJobState::Running => "running",
            BackgroundJobState::Retried => "retried",
            BackgroundJobState::Completed => "completed",
            BackgroundJobState::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl FromStr for BackgroundJobState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(BackgroundJobState::Queued),
            "running" => Ok(BackgroundJobState::Running),
            "retried" => Ok(BackgroundJobState::Retried),
            "completed" => Ok(BackgroundJobState::Completed),
            "failed" => Ok(BackgroundJobState::Failed),
            other => Err(format!("unknown background job state: {other}")),
        }
    }
}

/// A durable background job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundJob {
    pub id: Uuid,
    pub kind: BackgroundJobKind,
    pub state: BackgroundJobState,
    pub attachment_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub attempt_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewBackgroundJob {
    pub id: Uuid,
    pub kind: BackgroundJobKind,
    pub attachment_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

/// Stable source for a managed material record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialSourceKind {
    Manual,
    AttachmentIngestion,
}

impl std::fmt::Display for MaterialSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MaterialSourceKind::Manual => "manual",
            MaterialSourceKind::AttachmentIngestion => "attachment_ingestion",
        };
        write!(f, "{s}")
    }
}

impl FromStr for MaterialSourceKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "manual" => Ok(MaterialSourceKind::Manual),
            "attachment_ingestion" => Ok(MaterialSourceKind::AttachmentIngestion),
            other => Err(format!("unknown material source kind: {other}")),
        }
    }
}

/// Durable lifecycle for managed materials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialStatus {
    Active,
    Archived,
}

impl std::fmt::Display for MaterialStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MaterialStatus::Active => "active",
            MaterialStatus::Archived => "archived",
        };
        write!(f, "{s}")
    }
}

impl FromStr for MaterialStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(MaterialStatus::Active),
            "archived" => Ok(MaterialStatus::Archived),
            other => Err(format!("unknown material status: {other}")),
        }
    }
}

/// Canonical managed material derived from manual authoring or attachment ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub id: Uuid,
    pub scope_kind: AttachmentScopeKind,
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub source_kind: MaterialSourceKind,
    pub source_attachment_id: Option<Uuid>,
    pub status: MaterialStatus,
    pub title: String,
    pub relative_path: String,
    pub mime_type: String,
    pub content_text: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewMaterial {
    pub id: Uuid,
    pub scope_kind: AttachmentScopeKind,
    pub session_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub source_kind: MaterialSourceKind,
    pub source_attachment_id: Option<Uuid>,
    pub title: String,
    pub relative_path: String,
    pub mime_type: String,
    pub content_text: String,
}

/// Where a source observation came from: a URL, a local file, or both
/// (a local capture of remote content records both).
///
/// At least one of the two is always present — the schema enforces it, and
/// `NewSourceObservation` construction should too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationOrigin {
    pub url: Option<String>,
    pub local_path: Option<String>,
}

/// One immutable capture of an external source in a space's knowledge base.
///
/// Observations are the knowledge base's unit of storage and are never
/// mutated or deleted: a fresh capture of the same origin is a new row, and
/// curation reorganises through separate append-only assertions. The raw
/// body lives as a content-addressed file under the space data root
/// (addressed by `body_sha256`); this row holds metadata only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceObservation {
    pub id: Uuid,
    pub space_id: Uuid,
    pub origin: ObservationOrigin,
    pub retrieved_at: DateTime<Utc>,
    /// Lowercase hex SHA-256 of the raw body — the body file's address.
    pub body_sha256: String,
    pub body_bytes: u64,
    pub media_type: String,
    /// Scope that created this observation, when it was first recorded.
    /// Every scoped capture is represented separately by `SourceCapture`.
    pub capture_scope: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Input for recording a new source observation.
#[derive(Debug, Clone)]
pub struct NewSourceObservation {
    pub space_id: Uuid,
    pub origin: ObservationOrigin,
    pub retrieved_at: DateTime<Utc>,
    pub body_sha256: String,
    pub body_bytes: u64,
    pub media_type: String,
    /// Stable identity for the unit of work making this capture (a
    /// workflow's journalled agent call), or `None` for one-off captures.
    /// Re-recording the same origin under the same scope deduplicates to
    /// the first capture, so retried work cannot fork the evidence. Body
    /// identity still wins: a capture whose bytes match existing evidence
    /// returns that observation and records the current scope and origin as
    /// a separate capture act.
    pub capture_scope: Option<String>,
}

/// One scoped act of capturing source material, even when its body resolves
/// to an observation that already exists in the knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCapture {
    pub observation_id: Uuid,
    pub space_id: Uuid,
    pub origin: ObservationOrigin,
    pub capture_scope: String,
    pub captured_at: DateTime<Utc>,
}

/// One versioned text extraction of an observation's raw body.
///
/// Versions are assigned by the store (per observation, ascending) and an
/// existing derivation is never mutated: re-extraction inserts the next
/// version, so a citation pinned to (observation, version, offsets) stays
/// valid across extractor upgrades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionDerivation {
    pub id: Uuid,
    pub observation_id: Uuid,
    pub version: u32,
    pub extracted_text: String,
    /// What produced this text (e.g. "html-to-text v1") — provenance for
    /// judging whether a better extraction is worth deriving.
    pub extractor: String,
    pub created_at: DateTime<Utc>,
}

/// One full-text search hit over a space's knowledge base.
///
/// Carries everything a citation needs: the observation, the derivation
/// version searched, and the matching passage. The offset is absent when a
/// lossy search transformation prevents the passage being located exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchHit {
    pub observation_id: Uuid,
    pub derivation_version: u32,
    pub origin: ObservationOrigin,
    pub retrieved_at: DateTime<Utc>,
    /// The matching passage, exactly as it appears in the derivation text.
    pub snippet: String,
    /// Byte offset of `snippet` within the derivation's extracted text, when
    /// the search-produced passage can be located exactly.
    pub snippet_offset: Option<usize>,
}

/// An immutable citation pinned to one extraction derivation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeCitation {
    pub id: Uuid,
    pub space_id: Uuid,
    pub observation_id: Uuid,
    pub derivation_version: u32,
    /// Byte offset of `quoted_text` within the pinned derivation.
    pub start_offset: usize,
    pub end_offset: usize,
    pub quoted_text: String,
    pub created_at: DateTime<Utc>,
}

/// Model-supplied citation identity. Offsets are deliberately absent: the
/// store locates the exact quote in the pinned derivation.
#[derive(Debug, Clone)]
pub struct NewKnowledgeCitation {
    pub space_id: Uuid,
    pub observation_id: Uuid,
    pub derivation_version: u32,
    pub quoted_text: String,
}

/// Immutable agent-authored orientation derived from whole observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrientationCard {
    pub id: Uuid,
    pub space_id: Uuid,
    pub title: String,
    pub content: String,
    pub authored_by: String,
    pub observation_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewOrientationCard {
    pub space_id: Uuid,
    pub title: String,
    pub content: String,
    pub authored_by: String,
    pub observation_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrientationCardLifecycleKind {
    Superseded,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrientationCardLifecycle {
    pub id: Uuid,
    pub card_id: Uuid,
    pub kind: OrientationCardLifecycleKind,
    pub replacement_card_id: Option<Uuid>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

/// The observation-level judgement recorded by a curation assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CurationAssertionKind {
    DuplicateOf,
    SupersededBy,
    Stale,
}

impl std::fmt::Display for CurationAssertionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DuplicateOf => "duplicate-of",
            Self::SupersededBy => "superseded-by",
            Self::Stale => "stale",
        })
    }
}

impl std::str::FromStr for CurationAssertionKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "duplicate-of" => Ok(Self::DuplicateOf),
            "superseded-by" => Ok(Self::SupersededBy),
            "stale" => Ok(Self::Stale),
            other => Err(format!("unknown curation assertion kind: {other}")),
        }
    }
}

/// An immutable observation-level curation judgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurationAssertion {
    pub id: Uuid,
    pub space_id: Uuid,
    pub observation_id: Uuid,
    pub kind: CurationAssertionKind,
    /// The preferred or replacement observation for relationship assertions.
    pub target_observation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Input for appending an observation-level curation judgement.
#[derive(Debug, Clone)]
pub struct NewCurationAssertion {
    pub space_id: Uuid,
    pub observation_id: Uuid,
    pub kind: CurationAssertionKind,
    pub target_observation_id: Option<Uuid>,
}

/// A durable agent definition — the bundle that gives a session its entire
/// character, selected at session start by agent id.
///
/// `space_id` `None` is a global definition; a space-scoped definition with
/// the same id shadows it within that space. The permission overlay
/// intersects with the resolved config's grants and never expands them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// The stable agent id sessions carry (`sessions.agent_id`).
    pub id: String,
    pub space_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    /// The agent's own instructions, appended after operator instructions.
    pub instructions: String,
    /// Model preference, used when the session has no explicit selection.
    pub model_id: Option<String>,
    /// Optional tool-permission overlay (serialised `PermissionConfig`).
    pub tool_permissions: Option<serde_json::Value>,
    /// Skills this definition activates when they are available to the agent.
    pub skills: Vec<String>,
    /// JSON Schema enforced for every final response from this agent.
    pub output_schema: Option<serde_json::Value>,
    /// Definition-specific validation retry limit.
    pub output_max_retries: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating or replacing an agent definition. Mutations travel as
/// recorded configuration changes, so the caller supplies whole definitions
/// rather than field patches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewAgentDefinition {
    pub id: String,
    pub space_id: Option<Uuid>,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model_id: Option<String>,
    pub tool_permissions: Option<serde_json::Value>,
    #[serde(default)]
    pub skills: Vec<String>,
    pub output_schema: Option<serde_json::Value>,
    pub output_max_retries: Option<u32>,
}

/// A durable named work contract resolved when a child is dispatched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDefinition {
    pub id: String,
    pub instructions: String,
    pub model_id: Option<String>,
    pub tool_permissions: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
    pub output_max_retries: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whole replacement used by audited task-definition mutations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTaskDefinition {
    pub id: String,
    pub instructions: String,
    pub model_id: Option<String>,
    pub tool_permissions: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
    pub output_max_retries: Option<u32>,
}

/// A manually appended audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewAuditEvent {
    pub id: String,
    pub session_id: Option<Uuid>,
    pub entity_kind: String,
    pub entity_id: String,
    pub event_kind: String,
    pub summary: String,
    pub metadata: serde_json::Value,
}

/// A single agent turn within a thread.
///
/// Turns belong to a thread (not directly to a session). Each turn captures
/// one round-trip with the LLM: the user's input, the model's response, and
/// the token counts for billing and context management.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboundAudit {
    /// Final canonical input after `BeforeInbound` hook processing.
    pub canonical_input: String,
    /// Ordered hook names that returned `ContinueModified`.
    pub modified_by_hooks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    /// The thread that owns this turn.
    pub thread_id: Uuid,
    /// The agent that executed this turn.
    pub agent_id: String,
    pub state: TurnState,
    /// The raw user input that started this turn, if captured.
    pub user_input: Option<String>,
    /// Audit metadata for canonical inbound rewriting.
    ///
    /// Stored only when one or more hooks modified the inbound content.
    pub inbound_audit: Option<InboundAudit>,
    /// The model's final text response, once the turn completes.
    pub assistant_response: Option<String>,
    /// Prompt tokens consumed by the LLM call.
    pub input_tokens: Option<i64>,
    /// Completion tokens generated by the LLM call.
    pub output_tokens: Option<i64>,
    /// Computed cost in microcents, derived from provider pricing data.
    /// `None` when pricing was not configured for the model at call time.
    pub cost_microcents: Option<i64>,
    /// Parsed, validated JSON from the agent's response when output schema
    /// validation succeeded. `None` when no schema is configured, or when
    /// validation failed after retry exhaustion.
    pub structured_output: Option<serde_json::Value>,
    /// References the delegating agent's turn for delegation chain tracking.
    /// `None` for direct (user-initiated) turns; `Some(parent_id)` for turns
    /// created by inter-agent delegation. Multi-hop chains (A → B → C) are
    /// traceable by following the parent references.
    pub parent_turn_id: Option<Uuid>,
    /// Prompt tokens spent by this turn's delegated children (transitively),
    /// accumulated as each child completes. A billing figure, deliberately
    /// separate from `input_tokens`: the children's tokens never occupied
    /// this turn's context window, so the context measure reads only the
    /// turn's own fields while billing sums own + delegated. `None` when no
    /// delegation has completed.
    pub delegated_input_tokens: Option<i64>,
    /// Completion tokens spent by this turn's delegated children
    /// (transitively); see `delegated_input_tokens`.
    pub delegated_output_tokens: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// A page: an agent-run tab on a space, assembled from module instances.
///
/// The database is authoritative for page content; renderings are derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: Uuid,
    pub space_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    /// Advanced on every module mutation, so "has this page moved?" is one
    /// column read.
    pub updated_at: DateTime<Utc>,
    /// The template this page was forked from, if any. Recorded origin, not
    /// a live link: the page's modules are its own from birth, and neither
    /// side's later edits reach the other.
    pub forked_from_template_id: Option<Uuid>,
    /// When the fork happened, alongside `forked_from_template_id`.
    pub forked_at: Option<DateTime<Utc>>,
}

/// A module instance on a page: work product seeded from a canonical module
/// kind, legitimately divergent from the moment it is used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageModule {
    pub id: Uuid,
    pub page_id: Uuid,
    /// The canonical module kind this instance was seeded from ("document"
    /// first). Kinds are compiled-in until module authoring lands.
    pub kind: String,
    /// Ordering index within the page.
    pub position: i32,
    /// The module's durable source, in the kind's own format (markdown for
    /// the document module).
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a page. `id` and timestamps are storage-assigned.
#[derive(Debug, Clone)]
pub struct NewPage {
    pub space_id: Uuid,
    pub title: String,
}

/// Input for adding a module instance to a page.
#[derive(Debug, Clone)]
pub struct NewPageModule {
    pub page_id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
}

/// A note hung on one of a page's modules — the shared reference a user and
/// an agent both point at instead of describing a location to each other.
///
/// Anchoring is at module grain and only there: `module_id` names the module
/// the note is about. Nothing here addresses the page as a whole, and nothing
/// addresses a position inside a module's content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageAnnotation {
    pub id: Uuid,
    pub page_id: Uuid,
    pub module_id: Uuid,
    /// The note's text, in whatever words its author used.
    pub body: String,
    /// Free-form labels the author attached, for sorting and filtering.
    pub tags: Vec<String>,
    /// Who left the note: an agent id, or "user" for the web surface.
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A reusable page shape a space keeps: a named set of module rows that
/// seeds new pages. Space-local — templates are neither shared nor
/// distributed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageTemplate {
    pub id: Uuid,
    pub space_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for leaving an annotation. `id` and timestamps are storage-assigned.
#[derive(Debug, Clone)]
pub struct NewPageAnnotation {
    pub page_id: Uuid,
    pub module_id: Uuid,
    pub body: String,
    pub tags: Vec<String>,
    pub author: String,
}

/// A module row on a template: the same shape a page module has, copied
/// into the forked page's own rows at creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageTemplateModule {
    pub id: Uuid,
    pub template_id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a page template.
#[derive(Debug, Clone)]
pub struct NewPageTemplate {
    pub space_id: Uuid,
    pub name: String,
}

/// Input for a module seeded into a template at its creation.
#[derive(Debug, Clone)]
pub struct NewPageTemplateModuleInput {
    pub kind: String,
    pub position: i32,
    pub source: String,
}

/// Input for adding a module row to a template.
#[derive(Debug, Clone)]
pub struct NewPageTemplateModule {
    pub template_id: Uuid,
    pub kind: String,
    pub position: i32,
    pub source: String,
}

/// A message within a thread.
///
/// Messages are scoped to a thread and may optionally reference the turn that
/// produced them. `position` is monotonically increasing within the thread so
/// that the full conversation history can be reconstructed in order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    /// The thread this message belongs to.
    pub thread_id: Uuid,
    /// The turn that produced this message, when known.
    ///
    /// Older rows and synthetic thread bootstrap messages may not have a turn.
    pub turn_id: Option<Uuid>,
    pub role: Role,
    pub content: String,
    /// Content parts submitted beside the text, one generic JSON value per
    /// part exactly as the client wire carried it. Storage keeps them
    /// opaque; the gateway parses them against the wire contract when it
    /// projects the message and rejects a value outside it.
    pub attachments: Vec<serde_json::Value>,
    /// Monotonically increasing position within the thread.
    pub position: i32,
    /// The agent that produced or received this message.
    pub agent_id: String,
    /// Provider-assigned correlation ID for tool result messages (e.g. `call_abc123`).
    ///
    /// `None` for ordinary user/assistant messages.
    pub tool_call_id: Option<String>,
    /// Name of the tool associated with this message, for tool result messages.
    ///
    /// `None` for ordinary user/assistant messages.
    pub tool_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A tool call within a turn — persisted for conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: Uuid,
    pub turn_id: Uuid,
    /// The agent that issued this tool call, for multi-agent query scoping.
    pub agent_id: String,
    /// Provider-assigned identifier for this individual call (e.g. Anthropic `tool_use` id).
    pub call_id: String,
    /// Name of the tool that was called.
    pub tool: String,
    /// Tool arguments serialised as a JSON string.
    pub arguments: String,
    /// Compact provider-visible replay envelope for the tool arguments.
    ///
    /// Kept separate from `arguments`, which remains the raw debugger/storage
    /// payload.
    #[serde(default)]
    pub model_arguments: String,
    /// Raw output returned by the tool, if it has completed.
    pub result: Option<String>,
    /// Compact provider-visible replay envelope for the tool result.
    pub model_result: Option<String>,
    /// Execution outcome ("success", "error", etc.).
    pub status: Option<String>,
    /// Wall-clock execution time in milliseconds.
    pub duration_ms: Option<i64>,
    /// True when the result was cut off due to length limits.
    pub truncated: bool,
    /// Total byte count of the untruncated result, for reporting.
    pub total_bytes: Option<i64>,
    /// Provider request round that produced this tool call within the turn.
    ///
    /// Multiple calls can share a round when the model requests several tools in
    /// one response. Nested calls may be recorded before their parent call is
    /// persisted and can inherit the parent round during replay reconstruction.
    #[serde(default)]
    pub provider_round: i32,
    /// Ordering index within the parent turn.
    pub position: i32,
    pub created_at: DateTime<Utc>,
    /// call_id of the enclosing tool call, if this is a sub-tool invocation.
    ///
    /// None for native (top-level) tool calls. Set when a tool such as
    /// python_execute dispatches further tool calls on behalf of the model.
    pub parent_call_id: Option<String>,
    /// One-line human-readable headline for the call, derived from the
    /// tool's declared display hints and the call's arguments at execution
    /// time. None when the tool declares no hint — renderers then use their
    /// generic fallback.
    #[serde(default)]
    pub display_summary: Option<String>,
}

/// Input for inserting a new tool call record.
///
/// `id` and `created_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewToolCall {
    pub turn_id: Uuid,
    /// The agent that issued this tool call.
    pub agent_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: String,
    pub model_arguments: String,
    pub result: Option<String>,
    pub model_result: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<i64>,
    pub truncated: bool,
    pub total_bytes: Option<i64>,
    pub provider_round: i32,
    pub position: i32,
    /// call_id of the enclosing tool call, if this is a sub-tool invocation.
    ///
    /// None for native (top-level) tool calls.
    pub parent_call_id: Option<String>,
    /// One-line display headline derived from the tool's display hints;
    /// None when the tool declares no hint.
    pub display_summary: Option<String>,
}

/// Thread lifecycle state.
///
/// Threads move through a structured state machine — see `can_transition_to`
/// for the permitted edges. Completed is a terminal state with no exits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Idle,
    Processing,
    Completed,
    Interrupted,
}

impl ThreadState {
    /// Whether this state can transition to `target`.
    ///
    /// Permitted edges:
    ///   Idle        → Processing, Completed
    ///   Processing  → Idle, Completed, Interrupted
    ///   Interrupted → Idle
    ///   Completed   → (terminal, no exits)
    pub fn can_transition_to(&self, target: &ThreadState) -> bool {
        matches!(
            (self, target),
            (ThreadState::Idle, ThreadState::Processing)
                | (ThreadState::Idle, ThreadState::Completed)
                | (ThreadState::Processing, ThreadState::Idle)
                | (ThreadState::Processing, ThreadState::Completed)
                | (ThreadState::Processing, ThreadState::Interrupted)
                | (ThreadState::Interrupted, ThreadState::Idle)
        )
    }
}

impl std::fmt::Display for ThreadState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ThreadState::Idle => "idle",
            ThreadState::Processing => "processing",
            ThreadState::Completed => "completed",
            ThreadState::Interrupted => "interrupted",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ThreadState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idle" => Ok(ThreadState::Idle),
            "processing" => Ok(ThreadState::Processing),
            "completed" => Ok(ThreadState::Completed),
            "interrupted" => Ok(ThreadState::Interrupted),
            other => Err(format!("unknown thread state: {other}")),
        }
    }
}

/// A thread within a session, scoped to a single agent.
///
/// A session may contain multiple threads — one per agent invocation. Threads
/// own their turns and messages; the thread's state tracks whether the agent
/// is currently active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: Uuid,
    /// The session this thread belongs to.
    pub session_id: Uuid,
    /// The agent that owns this thread.
    pub agent_id: String,
    pub state: ThreadState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Branch lineage connecting a child thread to its source thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadBranch {
    pub child_thread_id: Uuid,
    pub parent_thread_id: Uuid,
    pub branched_from_message_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// A skill that is currently active for a session.
///
/// Skills are activated by name and persist until explicitly deactivated or
/// until the session is deleted. The `activated_at` timestamp is used to
/// reconstruct the activation order when listing active skills.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSkillRecord {
    pub session_id: Uuid,
    pub skill_name: String,
    pub activated_at: DateTime<Utc>,
}

// Wire-visible schedule types come from the protocol crate — one
// definition, stored as TEXT columns via their string forms.
pub use swage_protocol::client::schedule::{
    GateConfig, GateDecision, GateVerdict, ScheduleCreator, ScheduleRunState, ScheduleRunTrigger,
    ScheduleStatus, SessionPolicy, SkipReason,
};

/// Canonical schedule kinds supported by the scheduler.
///
/// Stored beside the raw expression; the pair reconstructs the wire-level
/// `ScheduleSpec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    OneShot,
    Interval,
    Cron,
}

impl std::fmt::Display for ScheduleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ScheduleKind::OneShot => "one_shot",
            ScheduleKind::Interval => "interval",
            ScheduleKind::Cron => "cron",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ScheduleKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "one_shot" => Ok(ScheduleKind::OneShot),
            "interval" => Ok(ScheduleKind::Interval),
            "cron" => Ok(ScheduleKind::Cron),
            other => Err(format!("unknown schedule kind: {other}")),
        }
    }
}

/// A durable schedule definition.
///
/// The session policy is first-class: fresh-policy schedules carry their
/// target space and agent, continuation-policy schedules the session they
/// keep returning to. No agent id is copied off a session at creation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: Uuid,
    pub message: String,
    pub status: ScheduleStatus,
    pub kind: ScheduleKind,
    /// Original schedule expression as authored.
    pub expression: String,
    pub session_policy: SessionPolicy,
    /// Where this schedule came from — a person or an agent.
    pub creator: ScheduleCreator,
    /// Delivery routing configuration serialised as JSON.
    pub delivery_routing: serde_json::Value,
    pub gate: Option<GateConfig>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A recorded execution of a schedule.
///
/// Every due occurrence produces one of these — executed, gated out, or
/// skipped for overlap — so the run history is the complete account of
/// what the scheduler did and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub trigger: ScheduleRunTrigger,
    pub state: ScheduleRunState,
    /// Set when `state` is `Skipped`: what made the skip deliberate.
    pub skip_reason: Option<SkipReason>,
    pub scheduled_for: Option<DateTime<Utc>>,
    /// The session the run executed in. Absent when the run never reached
    /// one (gated out or skipped).
    pub session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    /// The gate's verdict and reasoning, when the schedule declares a gate.
    pub gate: Option<GateDecision>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Lifecycle state of an approval request.
///
/// Transitions: Pending → Approved | Rejected (human decision) or Expired
/// (timeout fired). Stored as snake_case text in SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    /// Waiting for a human decision.
    Pending,
    /// The human approved the request.
    Approved,
    /// The human rejected the request.
    Rejected,
    /// The timeout elapsed before a decision was made.
    Expired,
}

impl std::fmt::Display for ApprovalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ApprovalState::Pending => "pending",
            ApprovalState::Approved => "approved",
            ApprovalState::Rejected => "rejected",
            ApprovalState::Expired => "expired",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ApprovalState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(ApprovalState::Pending),
            "approved" => Ok(ApprovalState::Approved),
            "rejected" => Ok(ApprovalState::Rejected),
            "expired" => Ok(ApprovalState::Expired),
            other => Err(format!("unknown approval state: {other}")),
        }
    }
}

/// The category of gate that triggered the approval request.
///
/// Determines which of the optional `tool_*` / `hook_name` fields on
/// `ApprovalRequest` are populated.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    /// Approval was triggered by a tool dispatch gate.
    Tool,
    /// Approval was triggered by a filesystem location gate.
    Location,
    /// Approval was triggered by a hook outcome.
    Hook,
}

impl std::fmt::Display for ApprovalKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ApprovalKind::Tool => "tool",
            ApprovalKind::Location => "location",
            ApprovalKind::Hook => "hook",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ApprovalKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tool" => Ok(ApprovalKind::Tool),
            "location" => Ok(ApprovalKind::Location),
            "hook" => Ok(ApprovalKind::Hook),
            other => Err(format!("unknown approval kind: {other}")),
        }
    }
}

/// A persisted approval request — one row in the `approval_requests` table.
///
/// Created when an agent turn hits an approval gate (tool dispatch or hook).
/// The turn is suspended in `TurnState::AwaitingApproval` until this record
/// reaches a terminal state (Approved / Rejected / Expired).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: Uuid,
    /// The turn that is suspended awaiting this approval.
    pub turn_id: Uuid,
    /// Agent that triggered the gate.
    pub agent_id: String,
    /// Whether this is a tool-dispatch gate or a hook gate.
    pub kind: ApprovalKind,
    /// Populated when `kind == Tool`.
    pub tool_name: Option<String>,
    /// Tool call arguments, if available, for display in the approval UI.
    pub tool_arguments: Option<serde_json::Value>,
    /// Populated when `kind == Hook`.
    pub hook_name: Option<String>,
    /// Human-readable description of what is being approved.
    pub prompt: String,
    /// Additional structured context for the approval UI (e.g. environment).
    pub context: Option<serde_json::Value>,
    pub state: ApprovalState,
    /// Wall-clock time at which this request expires.
    pub timeout_at: DateTime<Utc>,
    /// What happens when the timeout fires without a decision.
    pub timeout_policy: swage_core::ApprovalTimeoutPolicy,
    /// Free-text reason provided by the approver (or by the timeout handler).
    pub decision_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set when the request reaches a terminal state.
    pub decided_at: Option<DateTime<Utc>>,
}

/// Input for creating a new approval request.
///
/// `id`, `state` (`Pending`), and `decided_at` are assigned by the storage
/// layer — callers supply only the fields they control.
#[derive(Debug, Clone)]
pub struct NewApprovalRequest {
    pub turn_id: Uuid,
    pub agent_id: String,
    pub kind: ApprovalKind,
    pub tool_name: Option<String>,
    pub tool_arguments: Option<serde_json::Value>,
    pub hook_name: Option<String>,
    pub prompt: String,
    pub context: Option<serde_json::Value>,
    pub timeout_at: DateTime<Utc>,
    pub timeout_policy: swage_core::ApprovalTimeoutPolicy,
}

impl NewApprovalRequest {
    pub fn validate(&self) -> Result<(), StorageError> {
        match self.kind {
            ApprovalKind::Tool => {
                if self.tool_name.is_none() {
                    return Err(StorageError::ConstraintViolation(
                        "tool approvals require tool_name".into(),
                    ));
                }
                if self.hook_name.is_some() {
                    return Err(StorageError::ConstraintViolation(
                        "tool approvals must not set hook_name".into(),
                    ));
                }
            }
            ApprovalKind::Location => {
                if self.context.is_none() {
                    return Err(StorageError::ConstraintViolation(
                        "location approvals require context".into(),
                    ));
                }
            }
            ApprovalKind::Hook => {
                if self.hook_name.is_none() && self.tool_name.is_none() {
                    return Err(StorageError::ConstraintViolation(
                        "hook approvals require hook_name or tool_name".into(),
                    ));
                }
                if self.tool_arguments.is_some() && self.tool_name.is_none() {
                    return Err(StorageError::ConstraintViolation(
                        "hook approvals must not set tool_arguments without tool_name".into(),
                    ));
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Agent messages — inter-agent communication
// ---------------------------------------------------------------------------

/// A persisted inter-agent message — one row in the `agent_messages` table.
///
/// Created by the sending agent and held in the storage layer until the
/// recipient agent polls its mailbox. The `status` field tracks delivery
/// lifecycle; `correlation_id` links Result and Cancel messages back to the
/// originating Delegate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: Uuid,
    /// The agent that originated this message.
    pub sender_agent_id: String,
    /// The intended recipient agent.
    pub recipient_agent_id: String,
    /// Semantic category of the message — drives routing and hook events.
    pub kind: swage_core::MessageKind,
    /// References the originating Delegate message. Required on Result and
    /// Cancel messages; None for Delegate and Notify.
    pub correlation_id: Option<Uuid>,
    /// Message-specific content as a JSON object.
    pub payload: serde_json::Value,
    /// Current delivery state (Pending → Delivered | Failed).
    pub status: swage_core::MessageStatus,
    /// Populated when `status == Failed` with a human-readable error description.
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set when `status` transitions to `Delivered`.
    pub delivered_at: Option<DateTime<Utc>>,
}

/// Input for creating a new inter-agent message.
///
/// `id`, `status` (`Pending`), `failure_reason`, `created_at`, and
/// `delivered_at` are assigned by the storage layer — callers supply only
/// the fields they control.
#[derive(Debug, Clone)]
pub struct NewAgentMessage {
    pub sender_agent_id: String,
    pub recipient_agent_id: String,
    pub kind: swage_core::MessageKind,
    /// Required for Result and Cancel; None for Delegate and Notify.
    pub correlation_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

impl NewAgentMessage {
    pub fn validate(&self) -> Result<(), StorageError> {
        match self.kind {
            swage_core::MessageKind::Result | swage_core::MessageKind::Cancel => {
                if self.correlation_id.is_none() {
                    return Err(StorageError::ConstraintViolation(format!(
                        "{} messages require correlation_id",
                        self.kind
                    )));
                }
            }
            swage_core::MessageKind::Delegate
            | swage_core::MessageKind::Notify
            | swage_core::MessageKind::Shutdown => {
                if self.correlation_id.is_some() {
                    return Err(StorageError::ConstraintViolation(format!(
                        "{} messages must not set correlation_id",
                        self.kind
                    )));
                }
            }
        }

        Ok(())
    }
}

/// Input for creating a new space.
#[derive(Debug, Clone)]
pub struct NewSpace {
    pub name: String,
    /// Filesystem-safe identifier for the space directory.
    pub slug: SpaceSlug,
}

/// Input for attaching a workspace to a space.
#[derive(Debug, Clone)]
pub struct NewWorkspaceAttachment {
    pub space_id: Uuid,
    pub label: String,
    pub path: String,
    pub permissions: Vec<String>,
    pub is_trusted: bool,
}

/// Input for creating a new session.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub space_id: Uuid,
    /// The agent that owns this session.
    pub agent_id: String,
    /// Host-managed opaque metadata to persist with the session.
    pub metadata: Option<serde_json::Value>,
    pub active_workspace_attachment_id: Option<Uuid>,
}

/// Input for persisting or replacing an device registration.
#[derive(Debug, Clone)]
pub struct NewDeviceRegistration {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: String,
    pub shared_token: String,
    pub workspace_root: String,
    pub workspace_roots: Vec<String>,
    pub capabilities: Vec<String>,
}

/// Input for creating or replacing a durable channel onboarding link.
#[derive(Debug, Clone)]
pub struct NewChannelLink {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub space_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub home_target: Option<serde_json::Value>,
}

/// Input for recording a durable outbound delivery.
#[derive(Debug, Clone)]
pub struct NewDeliveryRecord {
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub target: serde_json::Value,
    pub content: String,
    pub status: String,
    pub error: Option<String>,
}

/// Input for creating a new thread.
///
/// `id`, `state`, `created_at`, and `updated_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewThread {
    pub session_id: Uuid,
    pub agent_id: String,
}

/// Input for creating a new turn within a thread.
///
/// `id`, `created_at`, and `completed_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewTurn {
    pub thread_id: Uuid,
    pub agent_id: String,
    pub state: TurnState,
    /// Raw user input captured at turn creation time.
    pub user_input: Option<String>,
    /// Optional parent turn for delegation chain tracking. `None` for
    /// user-initiated turns.
    pub parent_turn_id: Option<Uuid>,
}

/// Input for creating a durable schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSchedule {
    /// Chosen by the caller so the recorded configuration change and the
    /// returned wire schedule can name the entity before it is written.
    pub id: Uuid,
    pub message: String,
    pub status: ScheduleStatus,
    pub kind: ScheduleKind,
    pub expression: String,
    pub session_policy: SessionPolicy,
    pub creator: ScheduleCreator,
    pub delivery_routing: serde_json::Value,
    pub gate: Option<GateConfig>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}

/// Partial update for an existing schedule.
///
/// The session policy is fixed at creation; there is deliberately no way
/// to move a schedule between sessions or agents after the fact.
#[derive(Debug, Clone, Default)]
pub struct UpdateSchedule {
    pub message: Option<String>,
    pub status: Option<ScheduleStatus>,
    pub kind: Option<ScheduleKind>,
    pub expression: Option<String>,
    pub delivery_routing: Option<serde_json::Value>,
    /// `Some(None)` removes the gate; `Some(Some(_))` replaces it.
    pub gate: Option<Option<GateConfig>>,
    pub next_run_at: Option<Option<DateTime<Utc>>>,
    pub last_run_at: Option<Option<DateTime<Utc>>>,
}

/// Input for recording a schedule run.
///
/// An executing run is created `Pending` and everything else — gate
/// decision, session, turn, output — arrives through [`ScheduleRunUpdate`]
/// as it progresses. A run known-terminal at creation (an overlap skip that
/// never runs) is created in its terminal state directly, so it falls
/// outside the one-in-flight guard rather than tripping it.
#[derive(Debug, Clone)]
pub struct NewScheduleRun {
    pub schedule_id: Uuid,
    pub trigger: ScheduleRunTrigger,
    pub state: ScheduleRunState,
    pub skip_reason: Option<SkipReason>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

impl NewScheduleRun {
    /// A run about to execute: created `Pending`, to be advanced through
    /// [`ScheduleRunUpdate`] as it progresses.
    pub fn pending(
        schedule_id: Uuid,
        trigger: ScheduleRunTrigger,
        scheduled_for: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            schedule_id,
            trigger,
            state: ScheduleRunState::Pending,
            skip_reason: None,
            scheduled_for,
            error_message: None,
        }
    }

    /// A run that never executes because a predecessor was still active:
    /// recorded straight into its terminal skipped state, outside the
    /// one-in-flight guard.
    pub fn skipped_overlap(
        schedule_id: Uuid,
        trigger: ScheduleRunTrigger,
        scheduled_for: Option<DateTime<Utc>>,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            schedule_id,
            trigger,
            state: ScheduleRunState::Skipped,
            skip_reason: Some(SkipReason::Overlap),
            scheduled_for,
            error_message: Some(error_message.into()),
        }
    }
}

/// Progress update for a schedule run.
///
/// Fields are only ever filled in, never cleared — a run's record
/// accumulates. `started_at`/`completed_at` are stamped by the storage
/// layer from the state transition.
#[derive(Debug, Clone, Default)]
pub struct ScheduleRunUpdate {
    pub state: Option<ScheduleRunState>,
    pub skip_reason: Option<SkipReason>,
    pub session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub gate: Option<GateDecision>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
}

/// Input for creating a new message within a thread.
///
/// `id` and `created_at` are assigned by the storage layer.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub thread_id: Uuid,
    /// The turn that produced this message, when known.
    pub turn_id: Option<Uuid>,
    pub role: Role,
    pub content: String,
    /// Content parts beside the text; empty for a text-only message.
    pub attachments: Vec<serde_json::Value>,
    /// Monotonically increasing position within the thread.
    pub position: i32,
    /// The agent that produced or received this message.
    pub agent_id: String,
}

/// Input for appending a new message at the end of a thread.
///
/// The storage layer assigns the next monotonic position atomically.
#[derive(Debug, Clone)]
pub struct AppendMessage {
    pub thread_id: Uuid,
    /// The turn that produced this message, when known.
    pub turn_id: Option<Uuid>,
    pub role: Role,
    pub content: String,
    /// Content parts beside the text; empty for a text-only message.
    pub attachments: Vec<serde_json::Value>,
    /// The agent that produced or received this message.
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Workflow scripts and runs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowScriptRecord {
    pub id: Uuid,
    pub space_id: Uuid,
    pub name: String,
    pub description: String,
    pub delivery_grant: Option<serde_json::Value>,
    pub captured_sources_grant: bool,
    /// The conversation that authored this script, when a model authored it
    /// rather than an operator. Its presence is the durable mark that the
    /// script's authority is the authoring session's and no more.
    pub authoring_session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkflowScriptRecord {
    /// The delivery grant a run resolved from this script may actually use.
    ///
    /// Delivery is how a run's effects leave the machine, so a script a model
    /// wrote never carries one: were it otherwise, a model could author a
    /// script naming any delivery target and then start it, minting itself
    /// authority its own session never held. The stored column stays readable
    /// so an operator can see what was asked for; every run-start path reads
    /// the grant through here.
    pub fn effective_delivery_grant(&self) -> Option<serde_json::Value> {
        if self.authoring_session_id.is_some() {
            return None;
        }
        self.delivery_grant.clone()
    }
}

#[derive(Clone, Debug)]
pub struct NewWorkflowScript {
    pub space_id: Uuid,
    pub name: String,
    pub description: String,
    pub source: String,
    pub delivery_grant: Option<serde_json::Value>,
    pub captured_sources_grant: bool,
    /// Set when a model authored the script; see
    /// [`WorkflowScriptRecord::effective_delivery_grant`].
    pub authoring_session_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowScriptVersion {
    pub id: Uuid,
    pub script_id: Uuid,
    pub version: u32,
    pub source: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowRunWorkerPlacement {
    /// The exact directory that bounds every worker dispatched by the run.
    pub path: String,
    pub can_write: bool,
    /// The durable grant provenance used to re-authorise `path` on every
    /// worker start without widening back to the space's other attachments.
    pub authority_path: String,
    pub authority_context_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowRunRecord {
    pub id: Uuid,
    pub space_id: Uuid,
    /// Set on a child run started by a parent's `workflow()` call: the parent
    /// run and the journal seq of the call that started it. The pair is what
    /// makes composed-run recovery deterministic — a resumed parent finds its
    /// existing child by (parent_run_id, parent_seq) instead of starting a
    /// duplicate.
    pub parent_run_id: Option<Uuid>,
    pub parent_seq: Option<u64>,
    pub script_version_id: Option<Uuid>,
    pub script_id: Option<Uuid>,
    pub script_name: Option<String>,
    pub script_version: Option<u32>,
    /// Canonical name of the skill whose bundled workflow source this run used.
    pub carrying_skill: Option<String>,
    /// Opaque identity declared by the workflow script for grouping its runs.
    pub task: Option<String>,
    pub source: Option<String>,
    pub source_hash: String,
    pub args: serde_json::Value,
    pub budget: serde_json::Value,
    pub state: String,
    pub final_output: Option<serde_json::Value>,
    pub handover: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub engine_version: String,
    /// The full ordered lineage of agent contexts bounding this run. Worker
    /// sessions intersect their grants with every member — re-resolved at
    /// every worker turn, so nested dispatch cannot expand authority and
    /// suspension never freezes it. Empty means operator-started.
    pub dispatch_bound_agent_ids: Vec<String>,
    /// The full ordered lineage of task permission overlays bounding this
    /// run. Worker sessions re-resolve and intersect every definition at
    /// each turn; a missing definition therefore fails closed.
    pub dispatch_bound_task_definition_ids: Vec<String>,
    /// The call-scoped tool allow-list inherited from the worker session that
    /// launched this run. This is a final narrowing overlay on every worker
    /// the run dispatches, including workers of nested child runs.
    pub workflow_call_tools: Option<Vec<String>>,
    /// Filesystem authority inherited from the session that launched this
    /// run. `None` means the run was not launched by a placed worker.
    pub inherited_worker_placement: Option<WorkflowRunWorkerPlacement>,
    /// When a parked run is due to wake. Set only while `state` is
    /// `parked`; cleared on every other state transition.
    pub wake_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewWorkflowRun {
    pub space_id: Uuid,
    pub parent_run_id: Option<Uuid>,
    pub parent_seq: Option<u64>,
    pub script_version_id: Option<Uuid>,
    /// Identity carried beside inline source when the source belongs to an
    /// immutable workflow outside the authored-version tables.
    pub inline_workflow_id: Option<Uuid>,
    pub inline_workflow_name: Option<String>,
    pub inline_workflow_version: Option<u32>,
    pub carrying_skill: Option<String>,
    /// See `WorkflowRunRecord::task`.
    pub task: Option<String>,
    pub source: Option<String>,
    pub source_hash: String,
    pub args: serde_json::Value,
    pub budget: serde_json::Value,
    pub engine_version: String,
    /// See `WorkflowRunRecord::dispatch_bound_agent_ids`.
    pub dispatch_bound_agent_ids: Vec<String>,
    /// See `WorkflowRunRecord::dispatch_bound_task_definition_ids`.
    pub dispatch_bound_task_definition_ids: Vec<String>,
    /// See `WorkflowRunRecord::workflow_call_tools`.
    pub workflow_call_tools: Option<Vec<String>>,
    /// See `WorkflowRunRecord::inherited_worker_placement`.
    pub inherited_worker_placement: Option<WorkflowRunWorkerPlacement>,
}

/// One entry in a run's durable event queue. Events are namespaced data from
/// registered posters, never instructions. `seq` is assigned by the store,
/// dense per run;
/// `journal_entry_id` keys the row to the journal entry that posted it, so a
/// replayed post finds its existing row instead of duplicating it. A pending
/// event has `delivered_at` NULL; delivery stamps the journal seq of the
/// wake that carried it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowRunEventRecord {
    pub id: Uuid,
    pub run_id: Uuid,
    pub journal_entry_id: Uuid,
    pub seq: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub source_surface: String,
    pub source_agent_id: Option<String>,
    pub source_session_id: Option<Uuid>,
    pub source_turn_id: Option<Uuid>,
    pub posted_at: DateTime<Utc>,
    pub delivered_wake_seq: Option<u64>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewWorkflowRunEvent {
    pub run_id: Uuid,
    pub journal_entry_id: Uuid,
    pub kind: String,
    pub payload: serde_json::Value,
    pub source_surface: String,
    pub source_agent_id: Option<String>,
    pub source_session_id: Option<Uuid>,
    pub source_turn_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowJournalRecord {
    pub id: Uuid,
    pub run_id: Uuid,
    pub seq: u64,
    pub completed_order: Option<u64>,
    pub kind: String,
    pub status: String,
    pub args_hash: String,
    pub request: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub effect_kind: Option<String>,
    pub idempotency_key: Option<Uuid>,
    pub target_kind: Option<String>,
    /// What the call was dispatched with, written before any provider work.
    /// Generic JSON; the contract is enforced where the record is projected.
    pub dispatch: Option<serde_json::Value>,
    /// Every attempt the call ran, oldest first, each carrying the options
    /// its own turn resolved. A call replay re-dispatched appends rather
    /// than overwrites, so the interrupted attempt stays in the record.
    pub dispatch_attempts: Option<serde_json::Value>,
    /// Why the entry's work stopped, when that is not the work's own ending.
    pub termination: Option<serde_json::Value>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// One provider request round inside a turn.
///
/// The row is written when the request goes out and settled when the round
/// ends, so an outstanding request (no `completed_at`) is distinguishable
/// from one that came back slowly.
#[derive(Clone, Debug)]
pub struct ProviderRoundRecord {
    pub id: Uuid,
    pub turn_id: Uuid,
    pub round: i64,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Generic JSON; the outcome contract lives in swage-protocol.
    pub outcome: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct NewWorkflowJournalIntent {
    pub run_id: Uuid,
    pub seq: u64,
    pub kind: String,
    pub args_hash: String,
    pub request: serde_json::Value,
    pub effect_kind: Option<String>,
    pub idempotency_key: Option<Uuid>,
    pub target_kind: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ToolCallRecord serialisation ---

    #[test]
    fn tool_call_record_serialises_full() {
        let id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let now = Utc::now();
        let record = ToolCallRecord {
            id,
            turn_id,
            agent_id: "agent-1".to_string(),
            call_id: "toolu_01".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
            model_arguments: "compact arguments".to_string(),
            result: Some("file1\nfile2".to_string()),
            model_result: Some("compact result".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(42),
            truncated: false,
            total_bytes: Some(20),
            provider_round: 2,
            position: 0,
            created_at: now,
            parent_call_id: None,
            display_summary: None,
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.turn_id, turn_id);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.call_id, "toolu_01");
        assert_eq!(back.tool, "shell");
        assert_eq!(back.arguments, r#"{"command":"ls"}"#);
        assert_eq!(back.model_arguments, "compact arguments");
        assert_eq!(back.result, Some("file1\nfile2".to_string()));
        assert_eq!(back.model_result, Some("compact result".to_string()));
        assert_eq!(back.status, Some("success".to_string()));
        assert_eq!(back.duration_ms, Some(42));
        assert!(!back.truncated);
        assert_eq!(back.total_bytes, Some(20));
        assert_eq!(back.provider_round, 2);
        assert_eq!(back.position, 0);
    }

    #[test]
    fn tool_call_record_serialises_with_none_fields() {
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "agent-2".to_string(),
            call_id: "toolu_02".to_string(),
            tool: "file_read".to_string(),
            arguments: r#"{"path":"/tmp/x"}"#.to_string(),
            model_arguments: "compact arguments".to_string(),
            result: None,
            model_result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            provider_round: 0,
            position: 1,
            created_at: Utc::now(),
            parent_call_id: None,
            display_summary: None,
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert!(back.result.is_none());
        assert!(back.model_result.is_none());
        assert!(back.status.is_none());
        assert!(back.duration_ms.is_none());
        assert!(back.total_bytes.is_none());
        assert_eq!(back.provider_round, 0);
        assert_eq!(back.position, 1);
    }

    #[test]
    fn new_tool_call_is_debug_clone() {
        let original = NewToolCall {
            turn_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            call_id: "toolu_03".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"pwd"}"#.to_string(),
            model_arguments: "compact arguments".to_string(),
            result: Some("ok".to_string()),
            model_result: Some("compact result".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(10),
            truncated: false,
            total_bytes: Some(2),
            provider_round: 1,
            position: 0,
            parent_call_id: None,
            display_summary: None,
        };
        let cloned = original.clone();
        // Debug: just ensure it formats without panic.
        let _ = format!("{cloned:?}");
        assert_eq!(original.tool, cloned.tool);
    }

    #[test]
    fn tool_call_record_carries_parent_call_id() {
        let parent_id = "toolu_parent".to_string();
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            call_id: "toolu_sub".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            model_arguments: "compact arguments".to_string(),
            result: None,
            model_result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            provider_round: 1,
            position: 0,
            created_at: Utc::now(),
            parent_call_id: Some(parent_id.clone()),
            display_summary: None,
        };

        let json = serde_json::to_string(&record).expect("serialisation should succeed");
        let back: ToolCallRecord =
            serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.parent_call_id, Some(parent_id));
    }

    #[test]
    fn new_tool_call_carries_parent_call_id() {
        let sub = NewToolCall {
            turn_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            call_id: "sub_01".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{}"#.to_string(),
            model_arguments: "compact arguments".to_string(),
            result: None,
            model_result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            provider_round: 1,
            position: 0,
            parent_call_id: Some("parent_01".to_string()),
            display_summary: None,
        };
        assert_eq!(sub.parent_call_id.as_deref(), Some("parent_01"));

        // Top-level calls use None.
        let top = NewToolCall {
            parent_call_id: None,
            ..sub
        };
        assert!(top.parent_call_id.is_none());
    }

    // --- Space slug field ---

    #[test]
    fn space_slug_field_present_and_serialises() {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let space = Space {
            id,
            name: "my-space".to_string(),
            description: String::new(),
            is_default: false,
            slug: "my-space".into(),
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string(&space).expect("serialisation should succeed");
        assert!(
            json.contains("\"slug\":\"my-space\""),
            "slug should appear in JSON"
        );
    }

    #[test]
    fn new_space_slug_field_present() {
        let np = NewSpace {
            name: "test".to_string(),
            slug: "test".into(),
        };
        assert_eq!(np.slug, "test");
    }

    #[test]
    fn space_slug_rejects_values_without_ascii_alphanumerics() {
        assert!(SpaceSlug::new("").is_err());
        assert!(SpaceSlug::new("!!! --- ___").is_err());
        assert!(SpaceSlug::new("space/escape").is_err());
        assert!(SpaceSlug::new("space-01").is_ok());
    }

    // --- ApprovalState round-trips ---

    #[test]
    fn approval_state_pending_displays_as_pending() {
        assert_eq!(ApprovalState::Pending.to_string(), "pending");
    }

    #[test]
    fn approval_state_display_from_str_round_trip() {
        let states = [
            ApprovalState::Pending,
            ApprovalState::Approved,
            ApprovalState::Rejected,
            ApprovalState::Expired,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: ApprovalState = s.parse().expect("round-trip should succeed");
            assert_eq!(state, &parsed, "round-trip failed for {s}");
        }
    }

    #[test]
    fn approval_state_from_str_unknown_errors() {
        assert!("bogus".parse::<ApprovalState>().is_err());
    }

    // --- ApprovalKind round-trips ---

    #[test]
    fn approval_kind_display_from_str_round_trip() {
        let kinds = [ApprovalKind::Tool, ApprovalKind::Hook];
        for kind in &kinds {
            let s = kind.to_string();
            let parsed: ApprovalKind = s.parse().expect("round-trip should succeed");
            assert_eq!(kind, &parsed, "round-trip failed for {s}");
        }
    }

    #[test]
    fn approval_kind_from_str_unknown_errors() {
        assert!("bogus".parse::<ApprovalKind>().is_err());
    }

    // --- ApprovalRequest struct constructibility ---

    #[test]
    fn approval_request_is_constructible() {
        use swage_core::ApprovalTimeoutPolicy;

        let id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let now = Utc::now();
        let req = ApprovalRequest {
            id,
            turn_id,
            agent_id: "agent-1".to_string(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".to_string()),
            tool_arguments: Some(serde_json::json!({"cmd": "ls"})),
            hook_name: None,
            prompt: "Approve shell?".to_string(),
            context: None,
            state: ApprovalState::Pending,
            timeout_at: now,
            timeout_policy: ApprovalTimeoutPolicy::Reject,
            decision_reason: None,
            created_at: now,
            decided_at: None,
        };
        assert_eq!(req.id, id);
        assert_eq!(req.kind, ApprovalKind::Tool);
        assert_eq!(req.state, ApprovalState::Pending);
    }

    #[test]
    fn new_approval_request_is_constructible() {
        use swage_core::ApprovalTimeoutPolicy;

        let turn_id = Uuid::new_v4();
        let now = Utc::now();
        let req = NewApprovalRequest {
            turn_id,
            agent_id: "agent-2".to_string(),
            kind: ApprovalKind::Hook,
            tool_name: None,
            tool_arguments: None,
            hook_name: Some("pre_exec".to_string()),
            prompt: "Approve hook?".to_string(),
            context: Some(serde_json::json!({"step": "init"})),
            timeout_at: now,
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        };
        assert_eq!(req.turn_id, turn_id);
        assert_eq!(req.kind, ApprovalKind::Hook);
    }

    #[test]
    fn turn_state_pending_displays_as_pending() {
        assert_eq!(TurnState::Pending.to_string(), "pending");
    }

    #[test]
    fn turn_state_processing_displays_as_processing() {
        assert_eq!(TurnState::Processing.to_string(), "processing");
    }

    #[test]
    fn turn_state_display_from_str_round_trip() {
        // All variants must survive a Display→FromStr round-trip.
        let states = [
            TurnState::Pending,
            TurnState::Processing,
            TurnState::Completed,
            TurnState::Failed,
            TurnState::Interrupted,
            TurnState::AwaitingApproval,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: TurnState = s.parse().expect("round-trip should succeed");
            assert_eq!(state, &parsed);
        }
    }

    #[test]
    fn turn_state_awaiting_approval_displays_as_awaiting_approval() {
        assert_eq!(TurnState::AwaitingApproval.to_string(), "awaiting_approval");
    }

    #[test]
    fn turn_state_awaiting_approval_parses_from_str() {
        let parsed: TurnState = "awaiting_approval"
            .parse()
            .expect("awaiting_approval should parse");
        assert_eq!(parsed, TurnState::AwaitingApproval);
    }

    #[test]
    fn turn_state_from_str_unknown() {
        let result = "bogus".parse::<TurnState>();
        assert!(result.is_err());
    }

    // --- ThreadState transitions ---

    #[test]
    fn thread_state_valid_transitions() {
        // Idle can go to Processing or Completed.
        assert!(ThreadState::Idle.can_transition_to(&ThreadState::Processing));
        assert!(ThreadState::Idle.can_transition_to(&ThreadState::Completed));
        // Processing can go to Idle, Completed, or Interrupted.
        assert!(ThreadState::Processing.can_transition_to(&ThreadState::Idle));
        assert!(ThreadState::Processing.can_transition_to(&ThreadState::Completed));
        assert!(ThreadState::Processing.can_transition_to(&ThreadState::Interrupted));
        // Interrupted can only go back to Idle.
        assert!(ThreadState::Interrupted.can_transition_to(&ThreadState::Idle));
    }

    #[test]
    fn thread_state_invalid_transitions() {
        // Idle cannot stay Idle or go to Interrupted.
        assert!(!ThreadState::Idle.can_transition_to(&ThreadState::Idle));
        assert!(!ThreadState::Idle.can_transition_to(&ThreadState::Interrupted));
        // Processing cannot loop to itself.
        assert!(!ThreadState::Processing.can_transition_to(&ThreadState::Processing));
        // Completed is terminal — no exits.
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Idle));
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Processing));
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Interrupted));
        assert!(!ThreadState::Completed.can_transition_to(&ThreadState::Completed));
        // Interrupted cannot go to Processing or Completed directly.
        assert!(!ThreadState::Interrupted.can_transition_to(&ThreadState::Processing));
        assert!(!ThreadState::Interrupted.can_transition_to(&ThreadState::Completed));
        assert!(!ThreadState::Interrupted.can_transition_to(&ThreadState::Interrupted));
    }

    #[test]
    fn thread_state_display_from_str_round_trip() {
        let states = [
            ThreadState::Idle,
            ThreadState::Processing,
            ThreadState::Completed,
            ThreadState::Interrupted,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: ThreadState = s.parse().expect("round-trip should succeed");
            assert_eq!(state, &parsed);
        }
    }

    #[test]
    fn thread_state_from_str_unknown() {
        let result = "bogus".parse::<ThreadState>();
        assert!(result.is_err());
    }

    // --- Thread serialisation ---

    #[test]
    fn thread_serialises_and_deserialises() {
        let id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let now = Utc::now();
        let thread = Thread {
            id,
            session_id,
            agent_id: "agent-1".to_string(),
            state: ThreadState::Idle,
            created_at: now,
            updated_at: now,
        };

        let json = serde_json::to_string(&thread).expect("serialisation should succeed");
        let back: Thread = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.session_id, session_id);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.state, ThreadState::Idle);
    }

    // --- Message: thread ownership + agent_id + tool fields ---

    #[test]
    fn message_serialises_full() {
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();
        let msg = Message {
            id,
            thread_id,
            attachments: Vec::new(),
            turn_id: Some(Uuid::new_v4()),
            role: Role::User,
            content: "hello".to_string(),
            position: 0,
            agent_id: "agent-1".to_string(),
            tool_call_id: Some("call_abc123".to_string()),
            tool_name: Some("shell".to_string()),
            created_at: now,
        };

        let json = serde_json::to_string(&msg).expect("serialisation should succeed");
        let back: Message = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.thread_id, thread_id);
        assert_eq!(back.role, Role::User);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.tool_call_id, Some("call_abc123".to_string()));
        assert_eq!(back.tool_name, Some("shell".to_string()));
        assert_eq!(back.position, 0);
    }

    #[test]
    fn message_tool_fields_optional() {
        // tool_call_id and tool_name may both be None for non-tool messages.
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();
        let msg = Message {
            id,
            thread_id,
            attachments: Vec::new(),
            turn_id: None,
            role: Role::Assistant,
            content: "sure".to_string(),
            position: 1,
            agent_id: "agent-1".to_string(),
            tool_call_id: None,
            tool_name: None,
            created_at: now,
        };

        let json = serde_json::to_string(&msg).expect("serialisation should succeed");
        let back: Message = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert!(back.tool_call_id.is_none());
        assert!(back.tool_name.is_none());
    }

    #[test]
    fn new_message_carries_thread_id_and_agent_id() {
        let thread_id = Uuid::new_v4();
        let nm = NewMessage {
            thread_id,
            attachments: Vec::new(),
            turn_id: None,
            role: Role::User,
            content: "test".to_string(),
            position: 0,
            agent_id: "agent-x".to_string(),
        };
        assert_eq!(nm.thread_id, thread_id);
        assert_eq!(nm.agent_id, "agent-x");
    }

    #[test]
    fn new_message_is_debug_clone() {
        let original = NewMessage {
            thread_id: Uuid::new_v4(),
            attachments: Vec::new(),
            turn_id: Some(Uuid::new_v4()),
            role: Role::User,
            content: "hi".to_string(),
            position: 0,
            agent_id: "agent-y".to_string(),
        };
        let cloned = original.clone();
        let _ = format!("{cloned:?}");
        assert_eq!(original.thread_id, cloned.thread_id);
        assert_eq!(original.agent_id, cloned.agent_id);
    }

    #[test]
    fn new_thread_is_debug_clone() {
        let session_id = Uuid::new_v4();
        let original = NewThread {
            session_id,
            agent_id: "agent-x".to_string(),
        };
        let cloned = original.clone();
        let _ = format!("{cloned:?}");
        assert_eq!(original.session_id, cloned.session_id);
        assert_eq!(original.agent_id, cloned.agent_id);
    }

    // --- Session agent_id ---

    #[test]
    fn session_includes_agent_id() {
        let id = Uuid::new_v4();
        let space_id = Uuid::new_v4();
        let now = Utc::now();
        let session = Session {
            id,
            space_id,
            agent_id: "my-agent".to_string(),
            metadata: None,
            selected_model_id: None,
            active_workspace_attachment_id: None,
            created_at: now,
            updated_at: now,
        };

        let json = serde_json::to_string(&session).expect("serialisation should succeed");
        let back: Session = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.agent_id, "my-agent");
    }

    #[test]
    fn new_session_includes_agent_id() {
        let ns = NewSession {
            space_id: Uuid::new_v4(),
            agent_id: "config-agent".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        };
        assert_eq!(ns.agent_id, "config-agent");
    }

    // --- Turn serialisation ---

    #[test]
    fn turn_serialises_with_all_fields() {
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();
        let turn = Turn {
            id,
            thread_id,
            agent_id: "agent-1".to_string(),
            state: TurnState::Processing,
            parent_turn_id: None,
            user_input: Some("hello".to_string()),
            inbound_audit: None,
            assistant_response: Some("hi".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
            cost_microcents: None,
            structured_output: None,
            created_at: now,
            completed_at: None,
            delegated_input_tokens: None,
            delegated_output_tokens: None,
        };

        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");

        assert_eq!(back.id, id);
        assert_eq!(back.thread_id, thread_id);
        assert_eq!(back.agent_id, "agent-1");
        assert_eq!(back.state, TurnState::Processing);
        assert_eq!(back.user_input, Some("hello".to_string()));
        assert_eq!(back.assistant_response, Some("hi".to_string()));
        assert_eq!(back.input_tokens, Some(10));
        assert_eq!(back.output_tokens, Some(20));
        assert!(back.completed_at.is_none());
    }

    #[test]
    fn new_turn_carries_thread_and_agent() {
        let thread_id = Uuid::new_v4();
        let turn = NewTurn {
            thread_id,
            agent_id: "agent-2".to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        };
        assert_eq!(turn.thread_id, thread_id);
        assert_eq!(turn.agent_id, "agent-2");
    }

    // --- Turn.cost_microcents ---

    #[test]
    fn turn_cost_microcents_field_is_optional() {
        let id = Uuid::new_v4();
        let thread_id = Uuid::new_v4();
        let now = Utc::now();

        let turn_with_cost = Turn {
            id,
            thread_id,
            agent_id: "agent-1".to_string(),
            state: TurnState::Completed,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_microcents: Some(42),
            structured_output: None,
            created_at: now,
            completed_at: Some(now),
            delegated_input_tokens: None,
            delegated_output_tokens: None,
        };
        assert_eq!(turn_with_cost.cost_microcents, Some(42));

        let turn_no_cost = Turn {
            cost_microcents: None,
            ..turn_with_cost
        };
        assert_eq!(turn_no_cost.cost_microcents, None);
    }

    #[test]
    fn turn_serialises_with_cost_microcents() {
        let turn = Turn {
            id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            state: TurnState::Completed,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_microcents: Some(999),
            structured_output: None,
            created_at: Utc::now(),
            completed_at: None,
            delegated_input_tokens: None,
            delegated_output_tokens: None,
        };
        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        assert!(
            json.contains("cost_microcents"),
            "cost_microcents field should appear in JSON"
        );
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");
        assert_eq!(back.cost_microcents, Some(999));
    }

    // --- Turn.structured_output ---

    #[test]
    fn turn_serialises_with_structured_output_present() {
        let schema_result = serde_json::json!({"answer": "Paris", "confidence": 0.97});
        let turn = Turn {
            id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            state: TurnState::Completed,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: Some("the answer is Paris".to_string()),
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: Some(schema_result.clone()),
            created_at: Utc::now(),
            completed_at: None,
            delegated_input_tokens: None,
            delegated_output_tokens: None,
        };

        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        assert!(
            json.contains("structured_output"),
            "structured_output should appear in JSON"
        );
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");
        assert_eq!(back.structured_output, Some(schema_result));
    }

    #[test]
    fn turn_serialises_with_structured_output_absent() {
        let turn = Turn {
            id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            agent_id: "agent-1".to_string(),
            state: TurnState::Pending,
            parent_turn_id: None,
            user_input: None,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: None,
            created_at: Utc::now(),
            completed_at: None,
            delegated_input_tokens: None,
            delegated_output_tokens: None,
        };

        let json = serde_json::to_string(&turn).expect("serialisation should succeed");
        let back: Turn = serde_json::from_str(&json).expect("deserialisation should succeed");
        assert!(back.structured_output.is_none());
    }

    // --- AgentMessage constructibility ---

    #[test]
    fn agent_message_is_constructible() {
        use swage_core::{MessageKind, MessageStatus};
        let id = Uuid::new_v4();
        let now = Utc::now();
        let msg = AgentMessage {
            id,
            sender_agent_id: "agent-a".to_string(),
            recipient_agent_id: "agent-b".to_string(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "summarise"}),
            status: MessageStatus::Pending,
            failure_reason: None,
            created_at: now,
            delivered_at: None,
        };
        assert_eq!(msg.id, id);
        assert_eq!(msg.kind, MessageKind::Delegate);
        assert_eq!(msg.status, MessageStatus::Pending);
    }

    #[test]
    fn agent_message_serialises_and_deserialises() {
        use swage_core::{MessageKind, MessageStatus};
        let now = Utc::now();
        let msg = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: "a".to_string(),
            recipient_agent_id: "b".to_string(),
            kind: MessageKind::Notify,
            correlation_id: Some(Uuid::new_v4()),
            payload: serde_json::json!({"content": "hello"}),
            status: MessageStatus::Delivered,
            failure_reason: None,
            created_at: now,
            delivered_at: Some(now),
        };
        let json = serde_json::to_string(&msg).expect("serialisation should succeed");
        let back: AgentMessage =
            serde_json::from_str(&json).expect("deserialisation should succeed");
        assert_eq!(back.sender_agent_id, "a");
        assert_eq!(back.recipient_agent_id, "b");
        assert_eq!(back.kind, MessageKind::Notify);
        assert_eq!(back.status, MessageStatus::Delivered);
    }

    #[test]
    fn new_agent_message_is_constructible() {
        use swage_core::MessageKind;
        let sender = Uuid::new_v4();
        let _ = sender; // correlation_id is Option<Uuid>, not necessarily sender
        let nm = NewAgentMessage {
            sender_agent_id: "agent-a".to_string(),
            recipient_agent_id: "agent-b".to_string(),
            kind: MessageKind::Cancel,
            correlation_id: Some(Uuid::new_v4()),
            payload: serde_json::json!({}),
        };
        assert_eq!(nm.sender_agent_id, "agent-a");
        assert_eq!(nm.kind, MessageKind::Cancel);
        assert!(nm.correlation_id.is_some());
    }
}
