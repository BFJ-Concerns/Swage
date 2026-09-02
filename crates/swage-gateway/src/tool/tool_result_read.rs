//! Tool-result retrieval for compact replay references.
//!
//! Compact tool replay keeps provider-visible history small. This tool lets the
//! model follow an explicit stored-result reference when it needs the omitted
//! bytes, while checking that the referenced turn belongs to the active thread.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::{ToolError, parse_tool_result_reference};
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::truncated_output_value;
use crate::tool_executor::GatewayToolContext;

pub struct ToolResultReadTool {
    pub max_output_bytes: usize,
}

#[async_trait]
impl Tool for ToolResultReadTool {
    fn name(&self) -> &str {
        "tool_result_read"
    }

    fn description(&self) -> &str {
        "Fetch the complete output of an earlier tool call when the conversation shows only a truncated version with a reference id."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "reference": {
                    "type": "string",
                    "description": "The full_result_ref id shown alongside a truncated tool result."
                }
            },
            "required": ["reference"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let reference = params
            .get("reference")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "reference must be a non-empty string".to_owned(),
            })?;
        let parsed =
            parse_tool_result_reference(reference).ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "reference must be a compact tool-result reference".to_owned(),
            })?;
        let current_turn_id = ctx.turn_id().ok_or_else(|| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message: "tool_result_read requires a turn ID".to_owned(),
        })?;
        let gateway_ctx = super::require_gateway_context(
            ctx,
            self.name(),
            "tool_result_read requires gateway execution context",
        )?;

        let current_turn = gateway_ctx
            .storage()
            .get_turn(current_turn_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Turn {current_turn_id} was not found"),
            })?;
        let referenced_turn = gateway_ctx
            .storage()
            .get_turn(parsed.turn_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| unavailable_reference(self.name()))?;

        if referenced_turn.thread_id != current_turn.thread_id {
            return Err(unavailable_reference(self.name()));
        }

        let call = gateway_ctx
            .storage()
            .get_tool_call_by_turn_and_call_id(parsed.turn_id, &parsed.call_id)
            .await
            .map_err(storage_error(self.name()))?
            .ok_or_else(|| unavailable_reference(self.name()))?;
        let result = call
            .result
            .as_deref()
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: "Referenced tool call has not completed".to_owned(),
            })?;

        let mut value = truncated_output_value(result, self.max_output_bytes);
        let object = value
            .as_object_mut()
            .expect("truncated_output_value always returns an object");
        object.insert("reference".to_string(), json!(reference));
        object.insert("tool".to_string(), json!(call.tool));
        object.insert("call_id".to_string(), json!(call.call_id));
        object.insert("status".to_string(), json!(call.status));
        object.insert("bytes".to_string(), json!(result.len()));
        Ok(value)
    }
}

fn unavailable_reference(name: &str) -> ToolError {
    ToolError::ExecutionFailed {
        name: name.to_owned(),
        message: "Reference is not available in this conversation".to_owned(),
    }
}

fn storage_error(name: &str) -> impl FnOnce(swage_core::StorageError) -> ToolError + '_ {
    move |error| ToolError::ExecutionFailed {
        name: name.to_owned(),
        message: format!("storage error: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use swage_core::{Role, tool_result_reference};
    use swage_storage::model::{
        NewMessage, NewSession, NewThread, NewToolCall, NewTurn, TurnState,
    };
    use swage_storage::sqlite::SqliteStorage;
    use swage_storage::store::{
        MessageStore, SessionStore, SpaceStore, Storage, ThreadStore, ToolCallStore, TurnStore,
    };
    use swage_tool::context::MockToolContext;
    use tokio::sync::mpsc;

    use super::ToolResultReadTool;
    use crate::tool::Tool;
    use crate::tool_executor::GatewayToolContext;

    async fn test_turns() -> (Arc<SqliteStorage>, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let space = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
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
        let current_turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let referenced_turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Completed,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        (storage, session.id, current_turn.id, referenced_turn.id)
    }

    async fn gateway_context(
        storage: Arc<SqliteStorage>,
        session_id: uuid::Uuid,
        turn_id: uuid::Uuid,
    ) -> GatewayToolContext {
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let mut ctx = MockToolContext::new("agent-a", session_id.to_string());
        ctx.turn_id = Some(turn_id);
        GatewayToolContext::new(&ctx, ws_tx, storage as Arc<dyn Storage>)
    }

    fn new_call(turn_id: uuid::Uuid, call_id: &str, result: Option<String>) -> NewToolCall {
        NewToolCall {
            turn_id,
            agent_id: "agent-a".into(),
            call_id: call_id.into(),
            tool: "shell".into(),
            arguments: r#"{"cmd":"cat large.txt"}"#.into(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &json!({"cmd": "cat large.txt"}),
            ),
            result,
            model_result: None,
            status: Some("success".into()),
            duration_ms: Some(10),
            truncated: false,
            total_bytes: None,
            provider_round: 1,
            position: 0,
            parent_call_id: None,
            display_summary: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_referenced_tool_result_in_current_thread() {
        let (storage, session_id, current_turn_id, referenced_turn_id) = test_turns().await;
        storage
            .create_tool_call(new_call(
                referenced_turn_id,
                "call_1",
                Some("full stored result".into()),
            ))
            .await
            .unwrap();
        let ctx = gateway_context(Arc::clone(&storage), session_id, current_turn_id).await;
        let reference = tool_result_reference(&referenced_turn_id.to_string(), "call_1");

        let output = ToolResultReadTool {
            max_output_bytes: 1_000,
        }
        .execute(json!({ "reference": reference }), &ctx)
        .await
        .unwrap();

        assert_eq!(output["output"], "full stored result");
        assert_eq!(output["tool"], "shell");
        assert_eq!(output["call_id"], "call_1");
        assert_eq!(output["bytes"], "full stored result".len());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_references_from_other_threads() {
        let (storage, session_id, current_turn_id, referenced_turn_id) = test_turns().await;
        storage
            .create_tool_call(new_call(
                referenced_turn_id,
                "call_1",
                Some("full stored result".into()),
            ))
            .await
            .unwrap();
        let other_thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id,
                agent_id: "agent-a".into(),
            },
        )
        .await
        .unwrap();
        let other_turn = storage
            .create_turn(NewTurn {
                thread_id: other_thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let ctx = gateway_context(Arc::clone(&storage), session_id, other_turn.id).await;
        let reference = tool_result_reference(&referenced_turn_id.to_string(), "call_1");

        let error = ToolResultReadTool {
            max_output_bytes: 1_000,
        }
        .execute(json!({ "reference": reference }), &ctx)
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Reference is not available in this conversation")
        );
        assert_ne!(current_turn_id, other_turn.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_branch_local_copied_tool_result_reference() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let space = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "agent-a".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let parent_thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: "agent-a".into(),
            },
        )
        .await
        .unwrap();
        let parent_turn = storage
            .create_turn(NewTurn {
                thread_id: parent_thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Completed,
                user_input: Some("read it".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        storage
            .create_message(NewMessage {
                thread_id: parent_thread.id,
                attachments: Vec::new(),
                turn_id: Some(parent_turn.id),
                role: Role::User,
                content: "read it".into(),
                position: 0,
                agent_id: "agent-a".into(),
            })
            .await
            .unwrap();
        let assistant_message = storage
            .create_message(NewMessage {
                thread_id: parent_thread.id,
                attachments: Vec::new(),
                turn_id: Some(parent_turn.id),
                role: Role::Assistant,
                content: "done".into(),
                position: 1,
                agent_id: "agent-a".into(),
            })
            .await
            .unwrap();
        storage
            .create_tool_call(new_call(
                parent_turn.id,
                "call_1",
                Some("full copied branch result".into()),
            ))
            .await
            .unwrap();

        let branch = storage
            .create_branch(parent_thread.id, assistant_message.id)
            .await
            .unwrap();
        let child_turn = storage
            .list_turns_by_thread(branch.child_thread_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("branch should copy parent turn");
        let current_turn = storage
            .create_turn(NewTurn {
                thread_id: branch.child_thread_id,
                agent_id: "agent-a".into(),
                state: TurnState::Pending,
                user_input: Some("expand copied result".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let ctx = gateway_context(Arc::clone(&storage), session.id, current_turn.id).await;
        let reference = tool_result_reference(&child_turn.id.to_string(), "call_1");

        let output = ToolResultReadTool {
            max_output_bytes: 1_000,
        }
        .execute(json!({ "reference": reference }), &ctx)
        .await
        .unwrap();

        assert_eq!(output["output"], "full copied branch result");
        assert_eq!(output["tool"], "shell");
        assert_eq!(output["call_id"], "call_1");
    }
}
