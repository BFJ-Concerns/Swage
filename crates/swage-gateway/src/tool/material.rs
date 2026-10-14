//! Material tool — the agent's hands on the reference material its operator
//! attached.
//!
//! One tool, `material_read`: the text of an active material the session can
//! see — its own, or its topic's — by the id the context lists, a part at a
//! time when it is longer than the installation's output limit. Host-only
//! (`requires_device_execution`): it reaches the gateway's material store.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_storage::{AttachmentScopeKind, MaterialStatus};
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::state::AppState;
use crate::tool::Tool;

pub(crate) const MATERIAL_READ_TOOL: &str = "material_read";

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: MATERIAL_READ_TOOL.to_owned(),
        message: message.into(),
    }
}

fn failed(message: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed {
        name: MATERIAL_READ_TOOL.to_owned(),
        message: message.to_string(),
    }
}

pub struct MaterialReadTool {
    pub max_output_bytes: usize,
}

#[async_trait]
impl Tool for MaterialReadTool {
    fn name(&self) -> &str {
        MATERIAL_READ_TOOL
    }

    fn description(&self) -> &str {
        "Read a reference material your operator attached to this session or its topic, by the \
         id listed under Reference material in your context, and return its text as `output`. \
         A material longer than the installation's output limit comes back cut at that limit \
         and marked `truncated`, with its size in `total_bytes` and the place to carry on from \
         in `next_offset_bytes`; pass that as `offset_bytes` to read the next part. Only active \
         material of this session or its topic can be read. Reading changes nothing."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The material's id, as listed under Reference material."
                },
                "offset_bytes": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Where in the material to start, counted in bytes: the `next_offset_bytes` of the result being continued. Omit to read from the beginning."
                }
            },
            "required": ["id"]
        })
    }

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("id"),
        }
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let state: &AppState = super::require_gateway_state(
            ctx,
            MATERIAL_READ_TOOL,
            "the material tool runs only inside a gateway host turn",
        )?;
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid("'id' is required"))?
            .parse::<Uuid>()
            .map_err(|_| invalid("'id' must be a material id from the Reference material list"))?;
        let offset = match params.get("offset_bytes") {
            None | Some(Value::Null) => 0,
            Some(value) => value
                .as_u64()
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or_else(|| invalid("'offset_bytes' must be a non-negative integer"))?,
        };
        let session_id = ctx
            .session_id()
            .parse::<Uuid>()
            .map_err(|_| failed("the calling session has no usable id"))?;
        let session = state
            .storage
            .get_session(session_id)
            .await
            .map_err(failed)?
            .ok_or_else(|| failed("the calling session no longer exists"))?;
        let material = state
            .storage
            .get_material(id)
            .await
            .map_err(failed)?
            .filter(|material| material.status == MaterialStatus::Active)
            .filter(|material| match material.scope_kind {
                AttachmentScopeKind::Session => material.session_id == Some(session_id),
                AttachmentScopeKind::Topic => material.topic_id == Some(session.topic_id),
            })
            .ok_or_else(|| {
                invalid(format!(
                    "no active material {id} is attached to this session or its topic"
                ))
            })?;
        let text = &material.content_text;
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err(invalid(
                "'offset_bytes' must be a `next_offset_bytes` from an earlier read of this material",
            ));
        }
        let mut end = offset.saturating_add(self.max_output_bytes).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == offset && end < text.len() {
            return Err(failed(
                "the installation's output limit is too small to return the next character",
            ));
        }
        let mut result = json!({
            "id": material.id,
            "title": material.title,
            "mime_type": material.mime_type,
            "output": &text[offset..end],
        });
        if offset > 0 || end < text.len() {
            result["offset_bytes"] = json!(offset);
            result["total_bytes"] = json!(text.len());
        }
        if end < text.len() {
            result["truncated"] = json!(true);
            result["next_offset_bytes"] = json!(end);
            result["follow_up"] = json!({
                "tool": MATERIAL_READ_TOOL,
                "arguments": {"id": material.id, "offset_bytes": end},
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_storage::store::{MaterialStore, SessionStore, TopicStore};
    use swage_storage::{
        MaterialSourceKind, NewMaterial, NewTopic, SqliteStorage, model::NewSession,
    };
    use tokio::sync::mpsc;

    use super::*;
    use crate::tool_executor::GatewayToolContext;
    use crate::turn::tests::MockProvider;

    fn new_material(
        scope_kind: AttachmentScopeKind,
        session_id: Option<Uuid>,
        topic_id: Option<Uuid>,
        title: &str,
    ) -> NewMaterial {
        NewMaterial {
            id: Uuid::new_v4(),
            scope_kind,
            session_id,
            topic_id,
            source_kind: MaterialSourceKind::Manual,
            source_attachment_id: None,
            title: title.into(),
            relative_path: format!("{}.md", title.to_lowercase()),
            mime_type: "text/markdown".into(),
            content_text: format!("# {title}\n\nbody of {title}"),
        }
    }

    /// A topic with one session, a context for that session, and the store.
    async fn fixture() -> (GatewayToolContext, Arc<SqliteStorage>, Uuid, Uuid) {
        let root = tempfile::tempdir().unwrap();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Orchard".into(),
                slug: "orchard".into(),
            },
        )
        .await
        .unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "agent".into(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(MockProvider::new(vec![])) as Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.keep(),
        );
        let base = swage_tool::context::MockToolContext::new("agent", session.id.to_string());
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(
            &base,
            ws_tx,
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
        )
        .with_app_state(Some(state));
        (ctx, storage, session.id, topic.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_the_sessions_own_and_its_topics_active_material_whole() {
        let (ctx, storage, session_id, topic_id) = fixture().await;
        let own = storage
            .create_material(new_material(
                AttachmentScopeKind::Session,
                Some(session_id),
                Some(topic_id),
                "Plan",
            ))
            .await
            .unwrap();
        let shared = storage
            .create_material(new_material(
                AttachmentScopeKind::Topic,
                None,
                Some(topic_id),
                "Guide",
            ))
            .await
            .unwrap();

        for (material, title) in [(own, "Plan"), (shared, "Guide")] {
            let result = MaterialReadTool {
                max_output_bytes: 1024,
            }
            .execute(json!({ "id": material.id.to_string() }), &ctx)
            .await
            .unwrap();
            assert_eq!(result["title"], title);
            assert_eq!(result["output"], format!("# {title}\n\nbody of {title}"));
            assert!(result.get("truncated").is_none());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refuses_archived_material_another_sessions_and_another_topics() {
        let (ctx, storage, session_id, topic_id) = fixture().await;
        let archived = storage
            .create_material(new_material(
                AttachmentScopeKind::Session,
                Some(session_id),
                Some(topic_id),
                "Old",
            ))
            .await
            .unwrap();
        storage.archive_material(archived.id).await.unwrap();
        let other_session = storage
            .create_session(swage_storage::fixtures::new_session(topic_id))
            .await
            .unwrap();
        let foreign = storage
            .create_material(new_material(
                AttachmentScopeKind::Session,
                Some(other_session.id),
                Some(topic_id),
                "Theirs",
            ))
            .await
            .unwrap();
        let elsewhere = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Elsewhere".into(),
                slug: "elsewhere".into(),
            },
        )
        .await
        .unwrap();
        let other_topic = storage
            .create_material(new_material(
                AttachmentScopeKind::Topic,
                None,
                Some(elsewhere.id),
                "Far",
            ))
            .await
            .unwrap();

        for material in [archived, foreign, other_topic] {
            let error = MaterialReadTool {
                max_output_bytes: 1024,
            }
            .execute(json!({ "id": material.id.to_string() }), &ctx)
            .await
            .unwrap_err();
            let ToolError::InvalidParameters { message, .. } = error else {
                panic!("expected a refusal naming the id, got {error:?}");
            };
            assert!(message.contains(&material.id.to_string()), "{message}");
            assert!(message.contains("no active material"));
        }

        let error = MaterialReadTool {
            max_output_bytes: 1024,
        }
        .execute(json!({ "id": "not-an-id" }), &ctx)
        .await
        .unwrap_err();
        assert!(matches!(error, ToolError::InvalidParameters { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_material_longer_than_the_limit_reads_in_parts_that_join_to_the_whole() {
        let (ctx, storage, session_id, topic_id) = fixture().await;
        let text = "é".repeat(30) + "end";
        let material = storage
            .create_material(NewMaterial {
                content_text: text.clone(),
                ..new_material(
                    AttachmentScopeKind::Session,
                    Some(session_id),
                    Some(topic_id),
                    "Long",
                )
            })
            .await
            .unwrap();
        let tool = MaterialReadTool {
            max_output_bytes: 25,
        };
        let mut joined = String::new();
        let mut offset = None;
        loop {
            let mut params = json!({ "id": material.id.to_string() });
            if let Some(offset) = offset {
                params["offset_bytes"] = json!(offset);
            }
            let result = tool.execute(params, &ctx).await.unwrap();
            joined.push_str(result["output"].as_str().unwrap());
            assert_eq!(result["total_bytes"], text.len());
            match result.get("next_offset_bytes") {
                Some(next) => offset = next.as_u64(),
                None => break,
            }
        }
        assert_eq!(joined, text);
    }
}
