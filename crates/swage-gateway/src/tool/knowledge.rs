//! Knowledge-base tools — the agent's hands on the topic's gathered
//! external evidence.
//!
//! Six tools, separately grantable so read access never implies any kind
//! of write:
//!
//! - **`knowledge_search`** — full-text search over the topic's knowledge
//!   base; each hit carries the source it came from and the pinned
//!   derivation version and offsets a citation needs.
//! - **`knowledge_record`** — capture content into the knowledge base as a
//!   permanent, citable observation.
//! - **`knowledge_curate`** — append an observation-level judgement without
//!   changing the gathered evidence.
//! - **`knowledge_cite`** — record an exact quote pinned to its derivation.
//! - **`knowledge_orient`** — discover evidence-backed orientation cards
//!   without placing their content in instructions.
//! - **`knowledge_curate_orientation`** — author and maintain those cards.
//!
//! All six are host-only (`requires_device_execution`): they reach live gateway
//! state and the topic's body files. Knowledge content returns as tool
//! results only — it is gathered external data, never instructions.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_protocol::client::knowledge::{
    KnowledgeSearchResponse, KnowledgeSearchResult, OrientationCardResult, OrientationFindResponse,
};
use swage_storage::{
    CurationAssertionKind, NewCurationAssertion, NewKnowledgeCitation, NewOrientationCard,
    NewSourceObservation, ObservationOrigin,
};
use swage_tool::context::ToolContext;

use crate::knowledge;
use crate::state::AppState;
use crate::tool::Tool;

const SEARCH_TOOL: &str = "knowledge_search";
const RECORD_TOOL: &str = "knowledge_record";
const CURATE_TOOL: &str = "knowledge_curate";
const CITE_TOOL: &str = "knowledge_cite";
const ORIENT_TOOL: &str = "knowledge_orient";
const CURATE_ORIENTATION_TOOL: &str = "knowledge_curate_orientation";

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
    super::require_gateway_state(
        ctx,
        tool,
        "the knowledge tools run only inside a gateway host turn",
    )
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

/// Resolve the calling session's topic: the knowledge base is per topic, and
/// the session the tool call belongs to says which one. The third element is
/// the session's journalled-call scope — retry identity for idempotent
/// writes — absent on anything but a workflow agent-call worker.
async fn session_topic(
    tool: &str,
    state: &AppState,
    ctx: &dyn ToolContext,
) -> Result<(Uuid, String, Option<String>), ToolError> {
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
    let topic = state
        .topic_store
        .get(session.topic_id)
        .await
        .map_err(|error| failed(tool, error))?
        .ok_or_else(|| failed(tool, "the session's topic no longer exists"))?;
    let capture_scope = crate::turn::agent_call_scope(session.metadata.as_ref());
    Ok((topic.id, topic.slug.to_string(), capture_scope))
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
        "Find evidence for a question in this topic's gathered external material by \
         full text. Use knowledge_orient first when you do not yet know what the corpus \
         contains or which vocabulary it uses. Each result here is a passage with the source it came from and a \
         pinned observation and version; record a citation with the knowledge_cite \
         tool, passing those values and the exact quote. The \
         knowledge base holds captured external material; it is not your memory \
         (for what you know about your own work, use agent_memory for your \
         private records or memory for the topic's shared notes)."
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

        let (topic_id, _slug, _scope) = session_topic(SEARCH_TOOL, state, ctx).await?;

        let hits = state
            .storage
            .search_extractions(topic_id, query, limit)
            .await
            .map_err(|error| failed(SEARCH_TOOL, error))?;

        let results: Vec<KnowledgeSearchResult> = hits
            .into_iter()
            .map(|hit| KnowledgeSearchResult {
                observation_id: hit.observation_id,
                version: hit.derivation_version,
                source: hit.origin.url.or(hit.origin.local_path),
                retrieved_at: hit.retrieved_at.to_rfc3339(),
                passage: hit.snippet,
                offset: hit.snippet_offset,
            })
            .collect();
        let response = KnowledgeSearchResponse {
            guidance: if results.is_empty() {
                "No evidence matched. The knowledge base only holds what has \
                 been captured into it — an empty result means not gathered, \
                 not nonexistent."
            } else {
                "Cite a passage by its observation_id, version, and exact quote. An offset is \
                 included only when the search passage was located exactly."
            }
            .into(),
            results,
        };
        serde_json::to_value(response).map_err(|error| failed(SEARCH_TOOL, error))
    }
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
        "Capture an external source into this topic's knowledge base as a \
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
        let (topic_id, topic_slug, capture_scope) = session_topic(RECORD_TOOL, state, ctx).await?;

        let body = knowledge::write_body(&state.config_root, &topic_slug, content.as_bytes())
            .await
            .map_err(|error| failed(RECORD_TOOL, error))?;
        let captured_body_sha256 = body.sha256;

        // A workflow worker records under its journalled call's scope, so a
        // call re-executed after an interruption returns the first attempt's
        // observation instead of forking the evidence. (The retried body
        // file, if its bytes differ, stays as an unreferenced
        // content-addressed file — write-once, harmless.)
        let scoped = capture_scope.is_some();
        let observation = state
            .storage
            .record_observation(NewSourceObservation {
                topic_id,
                origin: ObservationOrigin {
                    url: url.map(str::to_owned),
                    local_path: local_path.map(str::to_owned),
                },
                retrieved_at: Utc::now(),
                body_sha256: captured_body_sha256.clone(),
                body_bytes: body.byte_count,
                media_type: media_type.to_owned(),
                capture_scope,
            })
            .await
            .map_err(|error| failed(RECORD_TOOL, error))?;

        let extraction = match params
            .get("extracted_text")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            Some(text) => {
                // Under a capture scope this may be the first attempt's row.
                // An extraction it already carries stands. If that attempt
                // stopped before extracting, only text derived from the same
                // retained bytes may be attached; changed retry bytes cannot
                // supply a derivation for the immutable first body.
                let existing = if scoped {
                    state
                        .storage
                        .latest_extraction(observation.id)
                        .await
                        .map_err(|error| failed(RECORD_TOOL, error))?
                } else {
                    None
                };
                match existing {
                    Some(existing) => Some(existing),
                    None if observation.body_sha256 == captured_body_sha256 => Some(
                        state
                            .storage
                            .add_extraction(
                                observation.id,
                                text.to_owned(),
                                "agent-supplied".to_owned(),
                            )
                            .await
                            .map_err(|error| failed(RECORD_TOOL, error))?,
                    ),
                    None => None,
                }
            }
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

// ---------------------------------------------------------------------------
// knowledge_curate
// ---------------------------------------------------------------------------

pub struct KnowledgeCurateTool;

#[async_trait]
impl Tool for KnowledgeCurateTool {
    fn name(&self) -> &str {
        CURATE_TOOL
    }

    fn description(&self) -> &str {
        "Record a judgement about an observation in this topic's knowledge base. \
         Mark it as a duplicate of another observation, superseded by another \
         observation, or stale. Judgements are permanent history: a later \
         judgement becomes current without changing either observation."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "observation_id": {
                    "type": "string",
                    "description": "The observation being retired from default retrieval."
                },
                "kind": {
                    "type": "string",
                    "enum": ["duplicate-of", "superseded-by", "stale"],
                    "description": "The judgement to record."
                },
                "target_observation_id": {
                    "type": "string",
                    "description": "The observation to prefer instead. Required for \
                                    duplicate-of and superseded-by; omit for stale."
                }
            },
            "required": ["observation_id", "kind"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Fully validate model-provided parameters before consulting host state.
        let observation_id = required_str(CURATE_TOOL, &params, "observation_id")?
            .parse::<Uuid>()
            .map_err(|_| invalid(CURATE_TOOL, "'observation_id' must be a UUID"))?;
        let kind = required_str(CURATE_TOOL, &params, "kind")?
            .parse::<CurationAssertionKind>()
            .map_err(|_| {
                invalid(
                    CURATE_TOOL,
                    "'kind' must be duplicate-of, superseded-by, or stale",
                )
            })?;
        let target_observation_id = params
            .get("target_observation_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                value
                    .parse::<Uuid>()
                    .map_err(|_| invalid(CURATE_TOOL, "'target_observation_id' must be a UUID"))
            })
            .transpose()?;
        match (kind, target_observation_id) {
            (CurationAssertionKind::Stale, Some(_)) => {
                return Err(invalid(CURATE_TOOL, "'stale' must not have a target"));
            }
            (CurationAssertionKind::DuplicateOf | CurationAssertionKind::SupersededBy, None) => {
                return Err(invalid(CURATE_TOOL, "this judgement requires a target"));
            }
            _ => {}
        }

        let state = gateway_state(CURATE_TOOL, ctx)?;
        let (topic_id, _slug, _scope) = session_topic(CURATE_TOOL, state, ctx).await?;
        let assertion = state
            .storage
            .add_curation_assertion(NewCurationAssertion {
                topic_id,
                observation_id,
                kind,
                target_observation_id,
            })
            .await
            .map_err(|error| failed(CURATE_TOOL, error))?;

        Ok(json!({
            "assertion_id": assertion.id,
            "observation_id": assertion.observation_id,
            "kind": assertion.kind,
            "target_observation_id": assertion.target_observation_id,
            "recorded": true,
        }))
    }
}

// ---------------------------------------------------------------------------
// knowledge_cite
// ---------------------------------------------------------------------------

pub struct KnowledgeCiteTool;

#[async_trait]
impl Tool for KnowledgeCiteTool {
    fn name(&self) -> &str {
        CITE_TOOL
    }

    fn description(&self) -> &str {
        "Record an exact quoted passage from this topic's knowledge base as a \
         durable citation. Supply the observation and version returned by \
         search plus the exact quote. The platform locates and verifies the \
         quote; paraphrased or absent text is refused."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "observation_id": {
                    "type": "string",
                    "description": "The observation containing the quoted passage."
                },
                "version": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The extraction derivation version returned by search."
                },
                "quote": {
                    "type": "string",
                    "description": "The exact text to cite, copied from the searched passage."
                }
            },
            "required": ["observation_id", "version", "quote"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let observation_id = required_str(CITE_TOOL, &params, "observation_id")?
            .parse::<Uuid>()
            .map_err(|_| invalid(CITE_TOOL, "'observation_id' must be a UUID"))?;
        let derivation_version = params
            .get("version")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid(CITE_TOOL, "'version' must be a positive integer"))?;
        let quoted_text = required_str(CITE_TOOL, &params, "quote")?.to_owned();

        let state = gateway_state(CITE_TOOL, ctx)?;
        let (topic_id, _slug, _scope) = session_topic(CITE_TOOL, state, ctx).await?;
        let citation = state
            .storage
            .add_citation(NewKnowledgeCitation {
                topic_id,
                observation_id,
                derivation_version,
                quoted_text,
            })
            .await
            .map_err(|error| failed(CITE_TOOL, error))?;

        Ok(json!({
            "citation_id": citation.id,
            "observation_id": citation.observation_id,
            "version": citation.derivation_version,
            "start_offset": citation.start_offset,
            "end_offset": citation.end_offset,
            "quote": citation.quoted_text,
            "recorded": true,
        }))
    }
}

// ---------------------------------------------------------------------------
// knowledge_orient
// ---------------------------------------------------------------------------

pub struct KnowledgeOrientTool;

#[async_trait]
impl Tool for KnowledgeOrientTool {
    fn name(&self) -> &str {
        ORIENT_TOOL
    }

    fn description(&self) -> &str {
        "Discover this topic's evidence-backed orientation cards before choosing corpus vocabulary. Call without a query on first contact; once useful terms are known, an optional query searches cards. Cards are derived signposts, not evidence: use knowledge_search independently to inspect and cite the underlying observations."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Optional FTS query. Omit on first contact." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
            },
            "required": []
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let state = gateway_state(ORIENT_TOOL, ctx)?;
        let (topic_id, _, _) = session_topic(ORIENT_TOOL, state, ctx).await?;
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(20, |value| value.clamp(1, 50) as usize);
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let cards = state
            .storage
            .find_orientation_cards(topic_id, query, limit)
            .await
            .map_err(|error| failed(ORIENT_TOOL, error))?
            .into_iter()
            .map(|card| OrientationCardResult {
                card_id: card.id,
                title: card.title,
                content: card.content,
                authored_by: card.authored_by,
                observation_ids: card.observation_ids,
                created_at: card.created_at.to_rfc3339(),
            })
            .collect();
        serde_json::to_value(OrientationFindResponse { cards, guidance: "Cards are orientation only. Search the evidence independently with knowledge_search; a card is never required to reach an observation.".into() }).map_err(|error| failed(ORIENT_TOOL, error))
    }
}

// ---------------------------------------------------------------------------
// knowledge_curate_orientation
// ---------------------------------------------------------------------------

pub struct KnowledgeCurateOrientationTool;

#[async_trait]
impl Tool for KnowledgeCurateOrientationTool {
    fn name(&self) -> &str {
        CURATE_ORIENTATION_TOOL
    }

    fn description(&self) -> &str {
        "Maintain this topic's evidence-backed orientation cards. Author creates an immutable card grounded in source observations. Supersede and retire append terminal lifecycle records; use supersede when another active card should replace the old signpost."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["author", "supersede", "retire"] },
                "title": { "type": "string" },
                "content": { "type": "string" },
                "observation_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "minItems": 1 },
                "card_id": { "type": "string", "format": "uuid" },
                "replacement_card_id": { "type": "string", "format": "uuid" }
            },
            "required": ["operation"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let operation = required_str(CURATE_ORIENTATION_TOOL, &params, "operation")?;
        if !matches!(operation, "author" | "supersede" | "retire") {
            return Err(invalid(
                CURATE_ORIENTATION_TOOL,
                "'operation' must be author, supersede, or retire",
            ));
        }
        let parsed = match operation {
            "author" => {
                let title = required_str(CURATE_ORIENTATION_TOOL, &params, "title")?.to_owned();
                let content = required_str(CURATE_ORIENTATION_TOOL, &params, "content")?.to_owned();
                let values = params
                    .get("observation_ids")
                    .and_then(Value::as_array)
                    .filter(|values| !values.is_empty())
                    .ok_or_else(|| {
                        invalid(
                            CURATE_ORIENTATION_TOOL,
                            "'observation_ids' needs at least one UUID",
                        )
                    })?;
                let ids = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .and_then(|value| value.parse::<Uuid>().ok())
                            .ok_or_else(|| {
                                invalid(
                                    CURATE_ORIENTATION_TOOL,
                                    "every 'observation_ids' value must be a UUID",
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Some((title, content, ids))
            }
            _ => None,
        };
        let card_id = if matches!(operation, "supersede" | "retire") {
            Some(
                required_str(CURATE_ORIENTATION_TOOL, &params, "card_id")?
                    .parse::<Uuid>()
                    .map_err(|_| invalid(CURATE_ORIENTATION_TOOL, "'card_id' must be a UUID"))?,
            )
        } else {
            None
        };
        let replacement_card_id = if operation == "supersede" {
            Some(
                required_str(CURATE_ORIENTATION_TOOL, &params, "replacement_card_id")?
                    .parse::<Uuid>()
                    .map_err(|_| {
                        invalid(
                            CURATE_ORIENTATION_TOOL,
                            "'replacement_card_id' must be a UUID",
                        )
                    })?,
            )
        } else {
            None
        };

        let state = gateway_state(CURATE_ORIENTATION_TOOL, ctx)?;
        let (topic_id, _, _) = session_topic(CURATE_ORIENTATION_TOOL, state, ctx).await?;
        let session_id = ctx.session_id().parse::<Uuid>().ok();
        match operation {
            "author" => {
                let (title, content, observation_ids) =
                    parsed.expect("validated author parameters");
                let card = state
                    .storage
                    .create_orientation_card(
                        NewOrientationCard {
                            topic_id,
                            title,
                            content,
                            authored_by: ctx.agent_id().to_owned(),
                            observation_ids,
                        },
                        session_id,
                    )
                    .await
                    .map_err(|error| failed(CURATE_ORIENTATION_TOOL, error))?;
                Ok(
                    json!({"card_id": card.id, "authored_by": card.authored_by, "observation_ids": card.observation_ids, "recorded": true}),
                )
            }
            "supersede" => {
                let record = state
                    .storage
                    .supersede_orientation_card(
                        topic_id,
                        card_id.expect("supersede parameters validated card_id"),
                        replacement_card_id.expect("supersede parameters validated replacement"),
                        ctx.agent_id().to_owned(),
                        session_id,
                    )
                    .await
                    .map_err(|error| failed(CURATE_ORIENTATION_TOOL, error))?;
                Ok(
                    json!({"card_id": record.card_id, "replacement_card_id": record.replacement_card_id, "superseded": true}),
                )
            }
            "retire" => {
                let record = state
                    .storage
                    .retire_orientation_card(
                        topic_id,
                        card_id.expect("retire parameters validated card_id"),
                        ctx.agent_id().to_owned(),
                        session_id,
                    )
                    .await
                    .map_err(|error| failed(CURATE_ORIENTATION_TOOL, error))?;
                Ok(json!({"card_id": record.card_id, "retired": true}))
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::*;
    use crate::state::AppState;
    use crate::tool_executor::GatewayToolContext;
    use crate::turn::tests::MockProvider;
    use swage_provider::LlmProvider;
    use swage_storage::{
        AuditStore, CitationStore, NewOrientationCard, NewSession, NewSourceObservation, NewTopic,
        ObservationStore, OrientationCardStore, SessionStore, SqliteStorage, TopicStore,
    };

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
    async fn record_dedups_retries_through_the_sessions_call_scope() {
        let root = tempfile::tempdir().unwrap();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.get_or_create_default().await.unwrap();
        // A workflow agent-call worker: the dispatch stamped the journalled
        // call's identity into the session metadata.
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "researcher".into(),
                metadata: Some(json!({
                    (crate::turn::AGENT_CALL_SCOPE_METADATA_KEY): "entry-77"
                })),
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        let base = swage_tool::context::MockToolContext::new("researcher", session.id.to_string());
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(
            &base,
            ws_tx,
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
        )
        .with_app_state(Some(state));

        let tool = KnowledgeRecordTool;
        let first = tool
            .execute(
                json!({
                    "content": "the page as first fetched",
                    "url": "https://example.test/page",
                    "extracted_text": "readable text from the first fetch",
                }),
                &ctx,
            )
            .await
            .unwrap();
        // The interrupted call re-runs and fetches changed bytes; the scope
        // makes it the same capture, and the first attempt's extraction
        // stands rather than gaining a duplicate derivation.
        let retried = tool
            .execute(
                json!({
                    "content": "the page as fetched on the resumed attempt",
                    "url": "https://example.test/page",
                    "extracted_text": "readable text from the resumed fetch",
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(first["observation_id"], retried["observation_id"]);
        assert_eq!(retried["version"], first["version"]);
        assert_eq!(retried["searchable"], true);
        let observations = storage.list_observations_by_topic(topic.id).await.unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].capture_scope.as_deref(), Some("entry-77"));
        let latest = storage
            .latest_extraction(observations[0].id)
            .await
            .unwrap()
            .expect("extraction");
        assert_eq!(latest.version, 1, "the retry must not stack a derivation");
        assert_eq!(latest.extracted_text, "readable text from the first fetch");

        let first_without_text = tool
            .execute(
                json!({
                    "content": "first retained bytes without an extraction",
                    "url": "https://example.test/changed-before-extraction",
                }),
                &ctx,
            )
            .await
            .unwrap();
        let changed_retry = tool
            .execute(
                json!({
                    "content": "different bytes fetched on retry",
                    "url": "https://example.test/changed-before-extraction",
                    "extracted_text": "text derived from the different retry bytes",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            changed_retry["observation_id"],
            first_without_text["observation_id"]
        );
        assert_eq!(changed_retry["searchable"], false);
        assert_eq!(changed_retry["version"], Value::Null);
        let retained_observation_id = first_without_text["observation_id"]
            .as_str()
            .expect("observation id")
            .parse()
            .expect("observation UUID");
        assert!(
            storage
                .latest_extraction(retained_observation_id)
                .await
                .unwrap()
                .is_none(),
            "retry text from different bytes must not become citable"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_requires_a_query() {
        let tool = KnowledgeSearchTool;
        let ctx = swage_tool::context::MockToolContext::new("agent", "not-a-session");

        let result = tool.execute(json!({"query": "  "}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParameters { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn curate_schema_describes_the_three_judgements() {
        let schema = KnowledgeCurateTool.parameters();
        assert_eq!(
            schema["properties"]["kind"]["enum"],
            json!(["duplicate-of", "superseded-by", "stale"])
        );
        assert_eq!(schema["required"], json!(["observation_id", "kind"]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn curate_validates_before_host_access() {
        let tool = KnowledgeCurateTool;
        let ctx = swage_tool::context::MockToolContext::new("agent", "not-a-session");

        for params in [
            json!({"observation_id": "not-a-uuid", "kind": "stale"}),
            json!({"observation_id": Uuid::new_v4(), "kind": "unknown"}),
            json!({"observation_id": Uuid::new_v4(), "kind": "duplicate-of"}),
            json!({
                "observation_id": Uuid::new_v4(),
                "kind": "stale",
                "target_observation_id": Uuid::new_v4()
            }),
        ] {
            assert!(matches!(
                tool.execute(params, &ctx).await,
                Err(ToolError::InvalidParameters { .. })
            ));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cite_validates_all_model_parameters_before_host_access() {
        let tool = KnowledgeCiteTool;
        let ctx = swage_tool::context::MockToolContext::new("agent", "not-a-session");
        for params in [
            json!({"observation_id": "no", "version": 1, "quote": "exact"}),
            json!({"observation_id": Uuid::new_v4(), "version": 0, "quote": "exact"}),
            json!({"observation_id": Uuid::new_v4(), "version": 1, "quote": "  "}),
        ] {
            assert!(matches!(
                tool.execute(params, &ctx).await,
                Err(ToolError::InvalidParameters { .. })
            ));
        }
    }

    #[test]
    fn orientation_declaration_supports_queryless_first_contact() {
        let tool = KnowledgeOrientTool;
        let curate = KnowledgeCurateOrientationTool;
        let search = KnowledgeSearchTool;
        assert!(
            tool.description()
                .contains("before choosing corpus vocabulary")
        );
        assert!(tool.description().contains("without a query"));
        assert!(
            search
                .description()
                .contains("Find evidence for a question")
        );
        assert!(search.description().contains("Use knowledge_orient first"));
        assert!(
            !tool.parameters()["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "query")
        );
        assert!(tool.requires_device_execution());
        assert!(tool.parameters()["properties"].get("operation").is_none());
        assert_eq!(
            curate.parameters()["properties"]["operation"]["enum"],
            json!(["author", "supersede", "retire"])
        );
        assert!(curate.requires_device_execution());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn orientation_validates_mutations_before_host_access() {
        let tool = KnowledgeCurateOrientationTool;
        let ctx = swage_tool::context::MockToolContext::new("agent", "not-a-session");
        for params in [
            json!({"operation": "unknown"}),
            json!({"operation": "author", "title": "title", "content": "content", "observation_ids": []}),
            json!({"operation": "retire", "card_id": "not-a-uuid"}),
            json!({"operation": "supersede", "card_id": Uuid::new_v4()}),
        ] {
            assert!(matches!(
                tool.execute(params, &ctx).await,
                Err(ToolError::InvalidParameters { .. })
            ));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn orientation_tools_run_the_scoped_lifecycle_and_record_the_session() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.get_or_create_default().await.unwrap();
        let other_topic = storage
            .create(NewTopic {
                name: "Other knowledge".into(),
                slug: "other-knowledge".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "curator".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let observation = storage
            .record_observation(NewSourceObservation {
                topic_id: topic.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/local-evidence".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "local-orientation-body".into(),
                body_bytes: 32,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        let other_observation = storage
            .record_observation(NewSourceObservation {
                topic_id: other_topic.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/foreign-evidence".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "foreign-orientation-body".into(),
                body_bytes: 32,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        let foreign_card = storage
            .create_orientation_card(
                NewOrientationCard {
                    topic_id: other_topic.id,
                    title: "Foreign vocabulary".into(),
                    content: "Must remain outside this session's authority".into(),
                    authored_by: "other-curator".into(),
                    observation_ids: vec![other_observation.id],
                },
                None,
            )
            .await
            .unwrap();

        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let base = swage_tool::context::MockToolContext::new("curator", session.id.to_string());
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(
            &base,
            ws_tx,
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
        )
        .with_app_state(Some(state));
        let curate = KnowledgeCurateOrientationTool;

        let first = curate
            .execute(
                json!({
                    "operation": "author",
                    "title": "Original vocabulary",
                    "content": "The first signpost",
                    "observation_ids": [observation.id],
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(first["authored_by"], "curator");
        assert_eq!(first["observation_ids"], json!([observation.id]));
        assert_eq!(first["recorded"], true);
        let first_id = first["card_id"].as_str().unwrap().parse::<Uuid>().unwrap();

        let replacement = curate
            .execute(
                json!({
                    "operation": "author",
                    "title": "Preferred vocabulary",
                    "content": "The current signpost",
                    "observation_ids": [observation.id],
                }),
                &ctx,
            )
            .await
            .unwrap();
        let replacement_id = replacement["card_id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap();

        let found = KnowledgeOrientTool.execute(json!({}), &ctx).await.unwrap();
        assert_eq!(found["cards"].as_array().unwrap().len(), 2);
        assert!(
            found["guidance"]
                .as_str()
                .unwrap()
                .contains("orientation only")
        );
        let preferred = found["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["card_id"] == json!(replacement_id))
            .unwrap();
        assert_eq!(preferred["title"], "Preferred vocabulary");
        assert_eq!(preferred["content"], "The current signpost");
        assert_eq!(preferred["authored_by"], "curator");
        assert_eq!(preferred["observation_ids"], json!([observation.id]));
        assert!(preferred["created_at"].as_str().is_some());

        let foreign_retire = curate
            .execute(
                json!({"operation": "retire", "card_id": foreign_card.id}),
                &ctx,
            )
            .await;
        assert!(matches!(
            foreign_retire,
            Err(ToolError::ExecutionFailed { .. })
        ));
        let foreign_replacement = curate
            .execute(
                json!({
                    "operation": "supersede",
                    "card_id": first_id,
                    "replacement_card_id": foreign_card.id,
                }),
                &ctx,
            )
            .await;
        assert!(matches!(
            foreign_replacement,
            Err(ToolError::ExecutionFailed { .. })
        ));
        assert_eq!(
            storage
                .find_orientation_cards(other_topic.id, None, 20)
                .await
                .unwrap()
                .len(),
            1
        );

        let superseded = curate
            .execute(
                json!({
                    "operation": "supersede",
                    "card_id": first_id,
                    "replacement_card_id": replacement_id,
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(superseded["card_id"], json!(first_id));
        assert_eq!(superseded["replacement_card_id"], json!(replacement_id));
        assert_eq!(superseded["superseded"], true);
        let active = KnowledgeOrientTool.execute(json!({}), &ctx).await.unwrap();
        assert_eq!(active["cards"].as_array().unwrap().len(), 1);
        assert_eq!(active["cards"][0]["card_id"], json!(replacement_id));

        let retired = curate
            .execute(
                json!({"operation": "retire", "card_id": replacement_id}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(retired["card_id"], json!(replacement_id));
        assert_eq!(retired["retired"], true);
        assert!(
            KnowledgeOrientTool.execute(json!({}), &ctx).await.unwrap()["cards"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let mut events = storage
            .list_audit_events()
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.session_id == Some(session.id))
            .collect::<Vec<_>>();
        events.sort_by(|left, right| left.event_kind.cmp(&right.event_kind));
        assert_eq!(events.len(), 4);
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_kind.as_str())
                .collect::<Vec<_>>(),
            vec!["created", "created", "retired", "superseded"]
        );
        assert!(
            events
                .iter()
                .all(|event| event.metadata["actor"] == "curator")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cite_delegates_to_host_storage_and_refuses_fabricated_quotes() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "researcher".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let observation = storage
            .record_observation(NewSourceObservation {
                topic_id: topic.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/evidence".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "tool-citation-body".into(),
                body_bytes: 64,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        let derivation = storage
            .add_extraction(
                observation.id,
                "before exact searched quote after".into(),
                "test".into(),
            )
            .await
            .unwrap();
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let base = swage_tool::context::MockToolContext::new("researcher", session.id.to_string());
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(
            &base,
            ws_tx,
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
        )
        .with_app_state(Some(state));

        let result = KnowledgeCiteTool
            .execute(
                json!({
                    "observation_id": observation.id, "version": derivation.version,
                    "quote": "exact searched quote",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["start_offset"], json!("before ".len()));
        let citation_id = result["citation_id"].as_str().unwrap().parse().unwrap();
        assert!(storage.get_citation(citation_id).await.unwrap().is_some());

        let fabricated = KnowledgeCiteTool
            .execute(
                json!({
                    "observation_id": observation.id, "version": derivation.version,
                    "quote": "fabricated quote",
                }),
                &ctx,
            )
            .await;
        assert!(matches!(fabricated, Err(ToolError::ExecutionFailed { .. })));
        let count = storage
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM knowledge_citations", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn supersession_retires_the_subject_and_preserves_the_preferred_target() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "curator".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let subject = storage
            .record_observation(NewSourceObservation {
                topic_id: topic.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/subject".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "subject-hash".into(),
                body_bytes: 10,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        let target = storage
            .record_observation(NewSourceObservation {
                topic_id: topic.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/target".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "target-hash".into(),
                body_bytes: 10,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        storage
            .add_extraction(
                subject.id,
                "Zorbulator manual 2019 edition".into(),
                "test".into(),
            )
            .await
            .unwrap();
        storage
            .add_extraction(
                target.id,
                "Zorbulator manual 2026 edition".into(),
                "test".into(),
            )
            .await
            .unwrap();
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let base = swage_tool::context::MockToolContext::new("curator", session.id.to_string());
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(
            &base,
            ws_tx,
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
        )
        .with_app_state(Some(state));

        let result = KnowledgeCurateTool
            .execute(
                json!({
                    "observation_id": subject.id,
                    "kind": "superseded-by",
                    "target_observation_id": target.id
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["observation_id"], json!(subject.id));
        assert_eq!(result["target_observation_id"], json!(target.id));
        assert_eq!(result["kind"], json!("superseded-by"));

        let hits = storage
            .search_extractions(topic.id, "Zorbulator", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation_id, target.id);
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
