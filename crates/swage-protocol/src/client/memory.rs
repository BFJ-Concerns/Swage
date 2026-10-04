//! The operator's memory as the Settings view reads and edits it: every page
//! of every scope listed, any page read, and changes made by instructing the
//! platform's curator — never by editing a page's text directly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Which memory store a request addresses: the operator's general memory,
/// or one topic's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum MemoryScopeSelector {
    General,
    Topic { topic_id: Uuid },
}

/// One memory page as the index lists it: its reference and the curator's
/// one-sentence summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct MemoryPageSummary {
    /// The reference the index spells: `general/<entry>` or `topic/<entry>`.
    pub reference: String,
    /// Absent when the page carries no summary line yet.
    pub summary: Option<String>,
}

/// Every page of one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct MemoryPageListResponse {
    pub scope: MemoryScopeSelector,
    pub pages: Vec<MemoryPageSummary>,
}

/// One page, whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct MemoryPageResponse {
    pub reference: String,
    pub summary: Option<String>,
    pub body: String,
}

/// An instruction to the curator about the operator's memory — the only way
/// a page changes from Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct MemoryInstructionRequest {
    pub scope: MemoryScopeSelector,
    pub instruction: String,
}

/// What the curator did with an instruction: its reply, and every page it
/// changed, whole, so the view shows the result as instructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct MemoryInstructionResponse {
    pub reply: String,
    pub pages_changed: Vec<MemoryPageResponse>,
    /// The curator session the instruction ran in — its record of the pass.
    pub curator_session_id: Uuid,
    pub turn_id: Uuid,
}
