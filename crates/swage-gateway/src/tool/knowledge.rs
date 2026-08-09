//! Knowledge-base tools — the agent's hands on the space's gathered
//! external evidence.
//!
//! Two tools, separately grantable so read access never implies write:
//!
//! - **`knowledge_search`** — full-text search over the space's knowledge
//!   base; each hit carries the source it came from and the pinned
//!   derivation version and offsets a citation needs.
//! - **`knowledge_record`** — capture content into the knowledge base as a
//!   permanent, citable observation.
//!
//! Both are host-only (`requires_device_execution`): they reach live gateway
//! state and the space's body files. Knowledge content returns as tool
//! results only — it is gathered external data, never instructions.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_storage::{NewSourceObservation, ObservationOrigin};
use swage_tool::context::ToolContext;

use crate::knowledge;
use crate::state::AppState;
use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;

const SEARCH_TOOL: &str = "knowledge_search";
const RECORD_TOOL: &str = "knowledge_record";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Reach the live gateway state through the host tool context. A call that
/// never reached the host path is a wiring bug, reported as an execution
/// failure rather than silently degrading.
fn gateway_state<'ctx>(
    tool: &str,
    ctx: &'ctx dyn ToolContext,
) -> Result<&'ctx AppState, ToolError> {
    ctx.as_any()
        .downcast_ref::<GatewayToolContext>()
        .and_then(GatewayToolContext::app_state)
        .ok_or_else(|| ToolError::ExecutionFailed {
            name: tool.to_owned(),
            message: "the knowledge tools run only inside a gateway host turn".to_owned(),
        })
}

fn invalid(tool: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: tool.to_owned(),
        message: message.into(),
    }
}

fn failed(tool: &str, message: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed {
        name: tool.to_owned(),
        message: message.to_string(),
    }
}

fn required_str<'p>(tool: &str, params: &'p Value, key: &str) -> Result<&'p str, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid(tool, format!("'{key}' is required")))
}

/// Resolve the calling session's space: the knowledge base is per space, and
/// the session the tool call belongs to says which one.
async fn session_space(
    tool: &str,
    state: &AppState,
    ctx: &dyn ToolContext,
) -> Result<(Uuid, String), ToolError> {
    let session_id = ctx
        .session_id()
        .parse::<Uuid>()
        .map_err(|_| failed(tool, "the calling session has no usable id"))?;
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|error| failed(tool, error))?
        .ok_or_else(|| failed(tool, "the calling session no longer exists"))?;
    let space = state
        .space_store
        .get(session.space_id)
        .await
        .map_err(|error| failed(tool, error))?
        .ok_or_else(|| failed(tool, "the session's space no longer exists"))?;
    Ok((space.id, space.slug.to_string()))
}

// ---------------------------------------------------------------------------
// knowledge_search
// ---------------------------------------------------------------------------

pub struct KnowledgeSearchTool;

#[async_trait]
impl Tool for KnowledgeSearchTool {
    fn name(&self) -> &str {
        SEARCH_TOOL
    }

    fn description(&self) -> &str {
        "Search this space's knowledge base — the gathered external evidence — by \
         full text. Each result is a passage with the source it came from and a \
         stable reference (observation, version, offset) you can cite. The \
         knowledge base holds captured external material; it is not your memory \
         (use the memory tools for what you know about your own work)."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Words or phrases to find. Matched as text \
                                    against the evidence, best matches first."
                },
                "limit": {
                    "type": "integer",
                    "description": "Most results to return. Default 10."
                }
            },
            "required": ["query"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Parameter problems are the model's to fix, so they are reported
        // before any host-wiring failure.
        let query = required_str(SEARCH_TOOL, &params, "query")?;
        let state = gateway_state(SEARCH_TOOL, ctx)?;
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(10, |n| n.clamp(1, 50) as usize);

        let (space_id, _slug) = session_space(SEARCH_TOOL, state, ctx).await?;

        let hits = state
            .storage
            .search_extractions(space_id, query, limit)
            .await
            .map_err(|error| failed(SEARCH_TOOL, error))?;

        let results: Vec<Value> = hits
            .iter()
            .map(|hit| {
                json!({
                    "observation_id": hit.observation_id,
                    "version": hit.derivation_version,
                    "source": hit.origin.url.as_deref()
                        .or(hit.origin.local_path.as_deref()),
                    "retrieved_at": hit.retrieved_at.to_rfc3339(),
                    "passage": hit.snippet,
                    "offset": hit.snippet_offset,
                })
            })
            .collect();

        Ok(json!({
            "results": results,
            "guidance": if results_empty(&results) {
                "No evidence matched. The knowledge base only holds what has \
                 been captured into it — an empty result means not gathered, \
                 not nonexistent."
            } else {
                "Cite a passage by its observation_id, version, and offset."
            },
        }))
    }
}

fn results_empty(results: &[Value]) -> bool {
    results.is_empty()
}

// ---------------------------------------------------------------------------
// knowledge_record
// ---------------------------------------------------------------------------

pub struct KnowledgeRecordTool;

#[async_trait]
impl Tool for KnowledgeRecordTool {
    fn name(&self) -> &str {
        RECORD_TOOL
    }

    fn description(&self) -> &str {
        "Capture an external source into this space's knowledge base as a \
         permanent, citable observation. Supply the source (its URL, a local \
         path, or both), the raw content, and the readable text extracted from \
         it. The capture is immutable: recording the same content twice \
         returns the original observation."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Where the content came from on the web. \
                                    At least one of url or local_path is required."
                },
                "local_path": {
                    "type": "string",
                    "description": "Where the content came from on disk, for \
                                    captures of local material."
                },
                "content": {
                    "type": "string",
                    "description": "The raw captured content, exactly as \
                                    retrieved. Stored permanently."
                },
                "extracted_text": {
                    "type": "string",
                    "description": "The readable text extracted from the \
                                    content — what search and citations run \
                                    over. Omit when the content yields no \
                                    text yet; the capture is still kept."
                },
                "media_type": {
                    "type": "string",
                    "description": "The content's media type, e.g. text/html. \
                                    Default text/plain."
                }
            },
            "required": ["content"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Parameter problems are the model's to fix, so they are reported
        // before any host-wiring failure.
        let content = required_str(RECORD_TOOL, &params, "content")?;
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let local_path = params
            .get("local_path")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        if url.is_none() && local_path.is_none() {
            return Err(invalid(
                RECORD_TOOL,
                "an observation needs its origin: supply 'url', 'local_path', or both",
            ));
        }
        let media_type = params
            .get("media_type")
            .and_then(Value::as_str)
            .unwrap_or("text/plain");

        let state = gateway_state(RECORD_TOOL, ctx)?;
        let (space_id, space_slug) = session_space(RECORD_TOOL, state, ctx).await?;

        let body = knowledge::write_body(&space_slug, content.as_bytes())
            .await
            .map_err(|error| failed(RECORD_TOOL, error))?;

        let observation = state
            .storage
            .record_observation(NewSourceObservation {
                space_id,
                origin: ObservationOrigin {
                    url: url.map(str::to_owned),
                    local_path: local_path.map(str::to_owned),
                },
                retrieved_at: Utc::now(),
                body_sha256: body.sha256,
                body_bytes: body.byte_count,
                media_type: media_type.to_owned(),
            })
            .await
            .map_err(|error| failed(RECORD_TOOL, error))?;

        let extraction = match params
            .get("extracted_text")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            Some(text) => Some(
                state
                    .storage
                    .add_extraction(observation.id, text.to_owned(), "agent-supplied".to_owned())
                    .await
                    .map_err(|error| failed(RECORD_TOOL, error))?,
            ),
            // A capture with no locatable text is still kept — the raw body
            // waits for better extraction — but nothing is searchable or
            // citable until a derivation produces text.
            None => None,
        };

        Ok(json!({
            "observation_id": observation.id,
            "version": extraction.as_ref().map(|e| e.version),
            "recorded": true,
            "searchable": extraction.is_some(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tools' host halves (session→space resolution, storage, body files)
    // are exercised through the storage and knowledge module tests; here the
    // contract that matters is the parameter validation a model will actually
    // hit, which runs before any host state is touched.

    #[tokio::test(flavor = "multi_thread")]
    async fn record_requires_an_origin() {
        let tool = KnowledgeRecordTool;
        let ctx = swage_tool::context::MockToolContext::new("agent", "not-a-session");

        let result = tool.execute(json!({"content": "captured"}), &ctx).await;

        let Err(ToolError::InvalidParameters { message, .. }) = result else {
            panic!("origin-less record should be invalid, got: {result:?}");
        };
        assert!(message.contains("origin"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_requires_a_query() {
        let tool = KnowledgeSearchTool;
        let ctx = swage_tool::context::MockToolContext::new("agent", "not-a-session");

        let result = tool.execute(json!({"query": "  "}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParameters { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tools_refuse_to_run_outside_the_host() {
        // A sandbox-side invocation must fail loudly, not degrade.
        let tool = KnowledgeSearchTool;
        let ctx = swage_tool::context::MockToolContext::new("agent", "session");

        let result = tool.execute(json!({"query": "anything"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed { .. })));
    }
}
