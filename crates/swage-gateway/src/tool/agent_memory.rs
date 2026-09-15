use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_storage::{AgentMemoryRecord, NewAgentMemoryRecord};
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;

pub struct AgentMemoryTool;

#[async_trait]
impl Tool for AgentMemoryTool {
    fn name(&self) -> &str {
        "agent_memory"
    }

    fn description(&self) -> &str {
        "Your own private memory: save an insight, preference, procedure, or outcome now, search for it in any future session, or review how a remembered item changed over time. Records here are yours alone — for notes shared with everyone working in the topic, use the memory tool instead."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["remember", "recall", "history"]
                },
                "title": {
                    "type": "string",
                    "description": "Short memory title. Required for remember."
                },
                "content": {
                    "type": "string",
                    "description": "Memory content. Required for remember."
                },
                "memory_kind": {
                    "type": "string",
                    "description": "Memory category, for example insight, preference, procedure, or outcome."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "outcome_status": {
                    "type": "string",
                    "description": "Optional outcome label such as succeeded, failed, or mixed."
                },
                "metadata": {
                    "type": "object",
                    "description": "Optional structured metadata to retain with the memory."
                },
                "query": {
                    "type": "string",
                    "description": "Search text for recall."
                },
                "scope": {
                    "type": "string",
                    "enum": ["agent", "session"],
                    "description": "Recall scope. Agent scope searches all of the caller's own memories across every session. Session scope limits results to the current session."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50
                }
            },
            "required": ["operation"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let operation =
            params["operation"]
                .as_str()
                .ok_or_else(|| ToolError::InvalidParameters {
                    name: self.name().to_owned(),
                    message: "Missing required argument: operation".to_owned(),
                })?;
        let gateway_ctx = super::require_gateway_context(
            ctx,
            self.name(),
            "agent_memory requires gateway execution context",
        )?;

        match operation {
            "remember" => self.remember(gateway_ctx, &params, ctx).await,
            "recall" => self.recall(gateway_ctx, &params, ctx).await,
            "history" => self.history(gateway_ctx, &params, ctx).await,
            other => Err(ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: format!(
                    "Unknown operation '{other}': must be remember, recall, or history"
                ),
            }),
        }
    }
}

impl AgentMemoryTool {
    async fn remember(
        &self,
        gateway_ctx: &GatewayToolContext,
        params: &Value,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        let turn_id = ctx.turn_id().ok_or_else(|| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message: "agent_memory remember requires a real turn ID".to_owned(),
        })?;
        let title = required_non_empty_string(self.name(), params, "title")?;
        let content = required_non_empty_string(self.name(), params, "content")?;
        let memory_kind = optional_non_empty_string(params, "memory_kind")
            .unwrap_or_else(|| "insight".to_string());
        let outcome_status = optional_non_empty_string(params, "outcome_status");
        let tags = parse_tags(self.name(), params)?;
        let metadata = params.get("metadata").cloned().unwrap_or_else(|| json!({}));

        let turn = gateway_ctx
            .storage()
            .get_turn(turn_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Turn {turn_id} was not found"),
            })?;
        let thread = gateway_ctx
            .storage()
            .get(turn.thread_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Thread {} was not found", turn.thread_id),
            })?;
        let session = gateway_ctx
            .storage()
            .get_session(thread.session_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Session {} was not found", thread.session_id),
            })?;

        let source_summary = self.resolve_source_summary(gateway_ctx).await?;

        let record = gateway_ctx
            .storage()
            .create_agent_memory(NewAgentMemoryRecord {
                id: Uuid::new_v4(),
                topic_id: session.topic_id,
                session_id: session.id,
                turn_id,
                agent_id: ctx.agent_id().to_string(),
                memory_kind,
                title,
                content,
                tags,
                outcome_status,
                source_summary,
                metadata,
            })
            .await
            .map_err(storage_error(self.name()))?;

        Ok(json!({
            "memory": memory_to_json(&record),
        }))
    }

    async fn recall(
        &self,
        gateway_ctx: &GatewayToolContext,
        params: &Value,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        let session_id = parse_context_session_id(self.name(), ctx)?;
        let session = gateway_ctx
            .storage()
            .get_session(session_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Session {session_id} was not found"),
            })?;
        let scope = params
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("agent");
        let query = optional_non_empty_string(params, "query");
        let limit = parse_limit(params);
        let (agent_id, session_scope) = match scope {
            "agent" => (Some(ctx.agent_id()), None),
            "session" => (None, Some(session.id)),
            other => {
                return Err(ToolError::InvalidParameters {
                    name: self.name().to_owned(),
                    message: format!("Unknown scope '{other}': must be agent or session"),
                });
            }
        };

        let records = gateway_ctx
            .storage()
            .list_agent_memories(
                session.topic_id,
                agent_id,
                session_scope,
                query.as_deref(),
                limit,
            )
            .await
            .map_err(storage_error(self.name()))?;

        Ok(json!({
            "memories": records.iter().map(memory_to_json).collect::<Vec<_>>(),
        }))
    }

    async fn history(
        &self,
        gateway_ctx: &GatewayToolContext,
        params: &Value,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        let session_id = parse_context_session_id(self.name(), ctx)?;
        let limit = parse_limit(params);
        let session = gateway_ctx
            .storage()
            .get_session(session_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Session {session_id} was not found"),
            })?;
        let memories = gateway_ctx
            .storage()
            .list_agent_memories(session.topic_id, Some(ctx.agent_id()), None, None, limit)
            .await
            .map_err(storage_error(self.name()))?;
        let audit_events = gateway_ctx
            .storage()
            .list_audit_events_by_session(session.id)
            .await
            .map_err(storage_error(self.name()))?
            .into_iter()
            .filter(|event| event.entity_kind != "secret")
            .take(limit)
            .map(|event| {
                json!({
                    "id": event.id,
                    "entity_kind": event.entity_kind,
                    "entity_id": event.entity_id,
                    "event_kind": event.event_kind,
                    "summary": event.summary,
                    "metadata": event.metadata,
                    "created_at": event.created_at,
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "memories": memories.iter().map(memory_to_json).collect::<Vec<_>>(),
            "audit_events": audit_events,
        }))
    }

    async fn resolve_source_summary(
        &self,
        gateway_ctx: &GatewayToolContext,
    ) -> Result<String, ToolError> {
        let recent_calls = gateway_ctx
            .storage()
            .list_tool_calls_by_turn(parse_required_turn_id(self.name(), gateway_ctx)?)
            .await
            .map_err(storage_error(self.name()))?;
        if recent_calls.is_empty() {
            Ok("conversation turn".to_string())
        } else {
            Ok(format!(
                "conversation turn after {} tool call(s)",
                recent_calls.len()
            ))
        }
    }
}

fn parse_required_turn_id(name: &str, ctx: &dyn ToolContext) -> Result<Uuid, ToolError> {
    ctx.turn_id().ok_or_else(|| ToolError::ExecutionFailed {
        name: name.to_owned(),
        message: "agent_memory requires a turn ID".to_owned(),
    })
}

fn parse_context_session_id(name: &str, ctx: &dyn ToolContext) -> Result<Uuid, ToolError> {
    ctx.session_id()
        .parse::<Uuid>()
        .map_err(|error| ToolError::ExecutionFailed {
            name: name.to_owned(),
            message: format!("invalid session ID in tool context: {error}"),
        })
}

fn required_non_empty_string(name: &str, params: &Value, key: &str) -> Result<String, ToolError> {
    let value = params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolError::InvalidParameters {
            name: name.to_owned(),
            message: format!("{key} must be a non-empty string"),
        })?;
    Ok(value.to_string())
}

fn optional_non_empty_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_tags(name: &str, params: &Value) -> Result<Vec<String>, ToolError> {
    let Some(tags) = params.get("tags") else {
        return Ok(Vec::new());
    };
    let Some(items) = tags.as_array() else {
        return Err(ToolError::InvalidParameters {
            name: name.to_owned(),
            message: "tags must be an array of strings".to_owned(),
        });
    };
    let mut parsed = Vec::with_capacity(items.len());
    for item in items {
        let Some(tag) = item
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Err(ToolError::InvalidParameters {
                name: name.to_owned(),
                message: "tags must contain only non-empty strings".to_owned(),
            });
        };
        parsed.push(tag.to_string());
    }
    Ok(parsed)
}

fn parse_limit(params: &Value) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 50) as usize)
        .unwrap_or(10)
}

fn storage_error(name: &str) -> impl FnOnce(swage_core::StorageError) -> ToolError + '_ {
    move |error| ToolError::ExecutionFailed {
        name: name.to_owned(),
        message: error.to_string(),
    }
}

fn memory_to_json(record: &AgentMemoryRecord) -> Value {
    json!({
        "id": record.id,
        "topic_id": record.topic_id,
        "session_id": record.session_id,
        "turn_id": record.turn_id,
        "agent_id": record.agent_id,
        "memory_kind": record.memory_kind,
        "title": record.title,
        "content": record.content,
        "tags": record.tags,
        "outcome_status": record.outcome_status,
        "source_summary": record.source_summary,
        "metadata": record.metadata,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use swage_storage::{
        NewAgentMemoryRecord, NewAuditEvent, NewSession, NewThread, NewTurn, SqliteStorage,
        TurnState,
        store::{AgentMemoryStore, AuditStore, SessionStore, ThreadStore, TopicStore, TurnStore},
    };
    use swage_tool::context::MockToolContext;

    use super::AgentMemoryTool;
    use crate::tool::Tool;
    use crate::tool_executor::GatewayToolContext;

    async fn gateway_context(
        storage: Arc<SqliteStorage>,
        turn_id: uuid::Uuid,
        session_id: uuid::Uuid,
        agent_id: &str,
    ) -> GatewayToolContext {
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let mut ctx = MockToolContext::new(agent_id, session_id.to_string());
        ctx.turn_id = Some(turn_id);
        GatewayToolContext::new(
            &ctx,
            ws_tx,
            storage as Arc<dyn swage_storage::store::Storage>,
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remember_and_recall_persist_memory() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "agent-a".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: "agent-a".into(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Completed,
                user_input: Some("remember".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let ctx = gateway_context(Arc::clone(&storage), turn.id, session.id, "agent-a").await;
        let tool = AgentMemoryTool;
        let remembered = tool
            .execute(
                json!({
                    "operation": "remember",
                    "title": "Research outcome",
                    "content": "The preview route must stay wired.",
                    "memory_kind": "outcome",
                    "tags": ["preview"],
                    "outcome_status": "succeeded",
                    "metadata": {"confidence": "high"}
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(remembered["memory"]["turn_id"], turn.id.to_string());
        let recalled = tool
            .execute(
                json!({
                    "operation": "recall",
                    "scope": "agent",
                    "query": "preview"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(recalled["memories"].as_array().unwrap().len(), 1);
        assert_eq!(
            recalled["memories"][0]["source_summary"],
            "conversation turn"
        );

        let persisted = storage
            .list_agent_memories(topic.id, Some("agent-a"), None, None, 10)
            .await
            .unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].turn_id, turn.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn history_filters_secret_events_and_recall_respects_scope_boundaries() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "agent-a".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: "agent-a".into(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Completed,
                user_input: Some("remember".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        storage
            .create_agent_memory(NewAgentMemoryRecord {
                id: Uuid::new_v4(),
                topic_id: topic.id,
                session_id: session.id,
                turn_id: turn.id,
                agent_id: "agent-a".into(),
                memory_kind: "insight".into(),
                title: "Allowed".into(),
                content: "Visible to agent-a".into(),
                tags: vec!["visible".into()],
                outcome_status: None,
                source_summary: "conversation turn".into(),
                metadata: json!({}),
            })
            .await
            .unwrap();
        storage
            .create_agent_memory(NewAgentMemoryRecord {
                id: Uuid::new_v4(),
                topic_id: topic.id,
                session_id: session.id,
                turn_id: turn.id,
                agent_id: "agent-b".into(),
                memory_kind: "insight".into(),
                title: "Blocked".into(),
                content: "Must not leak".into(),
                tags: vec!["hidden".into()],
                outcome_status: None,
                source_summary: "conversation turn".into(),
                metadata: json!({}),
            })
            .await
            .unwrap();
        storage
            .create_audit_event(NewAuditEvent {
                id: "secret-audit".into(),
                session_id: Some(session.id),
                entity_kind: "secret".into(),
                entity_id: "secret-1".into(),
                event_kind: "rotated".into(),
                summary: "secret rotated".into(),
                metadata: json!({"value": "should-not-leak"}),
            })
            .await
            .unwrap();

        let ctx = gateway_context(Arc::clone(&storage), turn.id, session.id, "agent-a").await;
        let tool = AgentMemoryTool;
        let recalled = tool
            .execute(json!({"operation": "recall", "scope": "agent"}), &ctx)
            .await
            .unwrap();
        assert_eq!(recalled["memories"].as_array().unwrap().len(), 1);
        assert_eq!(recalled["memories"][0]["title"], "Allowed");

        let history = tool
            .execute(json!({"operation": "history"}), &ctx)
            .await
            .unwrap();
        let audit_events = history["audit_events"].as_array().unwrap();
        assert!(
            audit_events
                .iter()
                .all(|event| event["entity_kind"] != "secret"),
            "secret audit events must stay outside the model-visible history: {audit_events:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remember_requires_gateway_context() {
        let tool = AgentMemoryTool;
        let ctx = MockToolContext::new("agent-a", Uuid::new_v4().to_string());

        let error = tool
            .execute(
                json!({
                    "operation": "remember",
                    "title": "No gateway",
                    "content": "Should fail"
                }),
                &ctx,
            )
            .await
            .expect_err("plain tool context should not allow agent_memory");

        assert!(error.to_string().contains("gateway execution context"));
    }
}
