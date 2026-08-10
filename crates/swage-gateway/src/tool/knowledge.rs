//! Knowledge-base tools — the agent's hands on the space's gathered
//! external evidence.
//!
//! Four tools, separately grantable so read access never implies any kind
//! of write:
//!
//! - **`knowledge_search`** — full-text search over the space's knowledge
//!   base; each hit carries the source it came from and the pinned
//!   derivation version and offsets a citation needs.
//! - **`knowledge_record`** — capture content into the knowledge base as a
//!   permanent, citable observation.
//! - **`knowledge_curate`** — append an observation-level judgement without
//!   changing the gathered evidence.
//! - **`knowledge_cite`** — record an exact quote pinned to its derivation.
//!
//! All four are host-only (`requires_device_execution`): they reach live gateway
//! state and the space's body files. Knowledge content returns as tool
//! results only — it is gathered external data, never instructions.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_protocol::client::knowledge::{KnowledgeSearchResponse, KnowledgeSearchResult};
use swage_storage::{
    CurationAssertionKind, NewCurationAssertion, NewKnowledgeCitation, NewSourceObservation,
    ObservationOrigin,
};
use swage_tool::context::ToolContext;

use crate::knowledge;
use crate::state::AppState;
use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;

const SEARCH_TOOL: &str = "knowledge_search";
const RECORD_TOOL: &str = "knowledge_record";
const CURATE_TOOL: &str = "knowledge_curate";
const CITE_TOOL: &str = "knowledge_cite";

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

        let results: Vec<KnowledgeSearchResult> = hits
            .into_iter()
            .map(|hit| KnowledgeSearchResult {
                observation_id: hit.observation_id,
                version: hit.derivation_version,
                source: hit.origin.url.or(hit.origin.local_path),
                retrieved_at: hit.retrieved_at,
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
        "Record a judgement about an observation in this space's knowledge base. \
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
        let (space_id, _slug) = session_space(CURATE_TOOL, state, ctx).await?;
        let assertion = state
            .storage
            .add_curation_assertion(NewCurationAssertion {
                space_id,
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
        "Record an exact quoted passage from this space's knowledge base as a \
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
        let (space_id, _slug) = session_space(CITE_TOOL, state, ctx).await?;
        let citation = state
            .storage
            .add_citation(NewKnowledgeCitation {
                space_id,
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
        CitationStore, NewSession, NewSourceObservation, ObservationStore, SessionStore,
        SpaceStore, SqliteStorage,
    };

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

    #[tokio::test(flavor = "multi_thread")]
    async fn cite_delegates_to_host_storage_and_refuses_fabricated_quotes() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let space = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "researcher".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let observation = storage
            .record_observation(NewSourceObservation {
                space_id: space.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/evidence".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "tool-citation-body".into(),
                body_bytes: 64,
                media_type: "text/plain".into(),
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
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
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
        let space = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "curator".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let subject = storage
            .record_observation(NewSourceObservation {
                space_id: space.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/subject".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "subject-hash".into(),
                body_bytes: 10,
                media_type: "text/plain".into(),
            })
            .await
            .unwrap();
        let target = storage
            .record_observation(NewSourceObservation {
                space_id: space.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/target".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "target-hash".into(),
                body_bytes: 10,
                media_type: "text/plain".into(),
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
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
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
            .search_extractions(space.id, "Zorbulator", 10)
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
