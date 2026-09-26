//! The platform's own documentation, hosted by the gateway.
//!
//! The gateway ships the agent-facing documentation set compiled into the
//! binary and serves it read-only, so a client — or an agent in a session
//! with no repository attached — reads how the platform behaves from the
//! gateway it is talking to. Documents are addressed by name, the file stem
//! of the source under `agent-docs/`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One hosted document, without its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DocumentSummary {
    /// The document's address: `GET /api/docs/{name}`.
    pub name: String,
    /// The document's first heading.
    pub title: String,
    /// The one-line description the document carries in its front matter.
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DocumentListResponse {
    pub documents: Vec<DocumentSummary>,
}

/// One hosted document with its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct DocumentResponse {
    pub name: String,
    pub title: String,
    pub description: String,
    /// The document's Markdown, front matter included.
    pub content: String,
}
