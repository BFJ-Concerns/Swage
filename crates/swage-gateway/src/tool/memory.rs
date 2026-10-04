//! The two memory tools: `memory_read`, offered to every session, and
//! `memory_write`, held by the platform's curator alone.
//!
//! Both resolve the calling session's scope on the gateway host and address
//! entries by the reference the index spells (`general/people/alice`,
//! `topic/conventions`). A conversing agent reads what the curator recorded;
//! it is never offered a way to write, so the write tool declares its holder
//! and the per-turn dispatcher withholds it from every other agent.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::memory::{
    MEMORY_CURATOR_AGENT_ID, MemoryReference, read_entry, scope_location_for, write_entry,
};
use crate::tool::Tool;

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

fn required_str<'a>(tool: &str, params: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    params[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(tool, format!("Missing required argument: {key}")))
}

/// The calling session's scope and the config root the store lives under.
async fn session_memory_scope(
    tool: &str,
    ctx: &dyn ToolContext,
) -> Result<(std::path::PathBuf, swage_core::ScopeLocation), ToolError> {
    let state = super::require_gateway_state(
        ctx,
        tool,
        format!("{tool} runs on the gateway host, inside a session"),
    )?;
    let (_, scope, _) = super::knowledge::session_topic(tool, state, ctx).await?;
    Ok((state.config_root.clone(), scope))
}

// ---------------------------------------------------------------------------
// memory_read
// ---------------------------------------------------------------------------

pub struct MemoryReadTool;

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        "memory_read"
    }

    fn description(&self) -> &str {
        "Read one entry of your operator's memory by the reference the memory index in your context gives it — general/<entry> for what the operator has stated about themselves, topic/<entry> for what they have stated about this topic. Returns the entry's summary and body as the curator recorded them. Read an entry when its index summary bears on the conversation; the index is the only list there is, so a reference not in it does not exist."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "reference": {
                    "type": "string",
                    "description": "The entry's reference as the memory index spells it, such as general/people/alice or topic/conventions."
                }
            },
            "required": ["reference"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let tool = self.name();
        let reference = MemoryReference::parse(required_str(tool, &params, "reference")?)
            .map_err(|error| invalid(tool, error.to_string()))?;
        let (root, session_scope) = session_memory_scope(tool, ctx).await?;
        let location = scope_location_for(&reference, &session_scope)
            .map_err(|error| invalid(tool, error.to_string()))?;
        match read_entry(&root, &location, &reference).await {
            Ok(Some(entry)) => Ok(json!({
                "reference": reference.to_string(),
                "summary": entry.summary,
                "body": entry.body,
            })),
            Ok(None) => Err(failed(
                tool,
                format!("no memory entry is recorded at '{reference}'"),
            )),
            Err(error) => Err(failed(tool, error)),
        }
    }
}

// ---------------------------------------------------------------------------
// memory_write
// ---------------------------------------------------------------------------

pub struct MemoryWriteTool;

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }

    fn description(&self) -> &str {
        "Record or replace one entry of the operator's memory, whole: its reference (general/<entry> for a fact about the operator, topic/<entry> for a fact about this topic), the one-sentence summary the index will show, and the body. Writing replaces the entry's summary and body entirely, so read the entry first and carry forward what still holds. Record only what the operator stated, in their words, each fact with a reference to the conversation it came from."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "reference": {
                    "type": "string",
                    "description": "The entry's reference: general/<entry> or topic/<entry>; path segments use letters, digits, hyphens and underscores."
                },
                "summary": {
                    "type": "string",
                    "description": "One sentence the index shows for this entry — enough to pick it from the index alone."
                },
                "body": {
                    "type": "string",
                    "description": "The entry's whole body: what is currently true about the subject, each fact with its conversation reference."
                }
            },
            "required": ["reference", "summary", "body"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    fn platform_holder(&self) -> Option<&'static str> {
        Some(MEMORY_CURATOR_AGENT_ID)
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let tool = self.name();
        let reference = MemoryReference::parse(required_str(tool, &params, "reference")?)
            .map_err(|error| invalid(tool, error.to_string()))?;
        let summary = required_str(tool, &params, "summary")?;
        let body = required_str(tool, &params, "body")?;
        if summary.contains('\n') {
            return Err(invalid(tool, "summary must be a single line"));
        }
        let (root, session_scope) = session_memory_scope(tool, ctx).await?;
        let location = scope_location_for(&reference, &session_scope)
            .map_err(|error| invalid(tool, error.to_string()))?;
        write_entry(&root, &location, &reference, summary, body)
            .await
            .map_err(|error| failed(tool, error))?;
        Ok(json!({
            "reference": reference.to_string(),
            "recorded": true,
            "bytes": body.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use swage_provider::LlmProvider;
    use swage_storage::store::Storage;
    use swage_storage::{
        NewSession, NewTopic, SessionOrigin, SessionStore, SqliteStorage, TopicStore,
    };
    use swage_tool::context::MockToolContext;
    use tokio::sync::mpsc;

    use crate::state::AppState;
    use crate::tool_executor::GatewayToolContext;
    use crate::turn::tests::MockProvider;

    use super::*;

    /// A real session and scope row under a temporary config root.
    async fn ctx_for_scope(dir: &TempDir, named_topic: bool) -> GatewayToolContext {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = if named_topic {
            storage
                .create(NewTopic {
                    name: "My topic".into(),
                    slug: "my-topic".into(),
                })
                .await
                .unwrap()
        } else {
            storage.general_scope().await.unwrap()
        };
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "test-agent".into(),
                origin: SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            dir.path().to_path_buf(),
        );
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let base = MockToolContext {
            agent_id: "test-agent".into(),
            session_id: session.id.to_string(),
            working_directory: scratch,
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        GatewayToolContext::new(&base, ws_tx, storage as Arc<dyn Storage>)
            .with_app_state(Some(state))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_written_entry_is_read_back_by_its_reference_in_either_scope() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_for_scope(&dir, true).await;

        let written = MemoryWriteTool
            .execute(
                json!({
                    "reference": "topic/conventions",
                    "summary": "This topic uses metric units only.",
                    "body": "Metric only (conversation 2026-10-04)."
                }),
                &ctx,
            )
            .await
            .expect("write");
        assert_eq!(written["recorded"], true);
        assert!(
            dir.path()
                .join("topics/my-topic/memory/conventions.md")
                .exists(),
            "topic memory lives under the topic's data directory"
        );

        MemoryWriteTool
            .execute(
                json!({
                    "reference": "general/people/alice",
                    "summary": "Alice is the operator's sister.",
                    "body": "Alice lives in Leeds (conversation 2026-10-04)."
                }),
                &ctx,
            )
            .await
            .expect("write general");

        let read = MemoryReadTool
            .execute(json!({ "reference": "topic/conventions" }), &ctx)
            .await
            .expect("read");
        assert_eq!(read["summary"], "This topic uses metric units only.");
        assert_eq!(read["body"], "Metric only (conversation 2026-10-04).");
        let read = MemoryReadTool
            .execute(json!({ "reference": "general/people/alice" }), &ctx)
            .await
            .expect("read general");
        assert_eq!(read["reference"], "general/people/alice");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_without_a_topic_has_no_topic_memory_to_read_or_write() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_for_scope(&dir, false).await;
        let error = MemoryReadTool
            .execute(json!({ "reference": "topic/conventions" }), &ctx)
            .await
            .expect_err("refused");
        assert!(
            matches!(error, ToolError::InvalidParameters { ref message, .. } if message.contains("no topic"))
        );
        let error = MemoryWriteTool
            .execute(
                json!({ "reference": "topic/x", "summary": "s", "body": "b" }),
                &ctx,
            )
            .await
            .expect_err("refused");
        assert!(matches!(error, ToolError::InvalidParameters { .. }));
        assert!(
            !dir.path().join("general/memory").exists(),
            "nothing was written"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unrecorded_reference_and_a_malformed_one_are_refused_distinctly() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_for_scope(&dir, true).await;
        let missing = MemoryReadTool
            .execute(json!({ "reference": "general/people/nobody" }), &ctx)
            .await
            .expect_err("missing");
        assert!(
            matches!(missing, ToolError::ExecutionFailed { ref message, .. } if message.contains("no memory entry"))
        );
        for bad in ["people/alice", "general/../x", "general/a b"] {
            let error = MemoryReadTool
                .execute(json!({ "reference": bad }), &ctx)
                .await
                .expect_err(bad);
            assert!(
                matches!(error, ToolError::InvalidParameters { .. }),
                "{bad}"
            );
        }
        let error = MemoryWriteTool
            .execute(
                json!({ "reference": "general/x", "summary": "two\nlines", "body": "b" }),
                &ctx,
            )
            .await
            .expect_err("multi-line summary");
        assert!(matches!(error, ToolError::InvalidParameters { .. }));
    }

    #[test]
    fn only_the_curator_holds_the_write_tool() {
        assert_eq!(
            MemoryWriteTool.platform_holder(),
            Some(MEMORY_CURATOR_AGENT_ID)
        );
        assert_eq!(MemoryReadTool.platform_holder(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn both_tools_refuse_to_run_outside_the_host() {
        let base = MockToolContext {
            agent_id: "test-agent".into(),
            session_id: uuid::Uuid::new_v4().to_string(),
            working_directory: std::env::temp_dir(),
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        assert!(
            MemoryReadTool
                .execute(json!({ "reference": "general/profile" }), &base)
                .await
                .is_err()
        );
        assert!(
            MemoryWriteTool
                .execute(
                    json!({ "reference": "general/profile", "summary": "s", "body": "b" }),
                    &base
                )
                .await
                .is_err()
        );
    }
}
