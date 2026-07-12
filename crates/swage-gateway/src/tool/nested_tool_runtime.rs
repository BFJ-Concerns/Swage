use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use swage_core::{compact_tool_arguments_for_model, extract_tool_result_metadata};
use swage_python::worker_protocol::WorkerToolError;
use swage_storage::model::NewToolCall;
use swage_storage::store::Storage;
use swage_tool::context::{SerializableToolContext, ToolContext};
use swage_tool::dispatch::ToolDispatcher;

use crate::websocket::ServerEvent;

pub struct NestedToolRuntime {
    dispatcher: Arc<ToolDispatcher>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    storage: Arc<dyn Storage>,
    turn_id: Uuid,
    parent_call_id: String,
    base_context: SerializableToolContext,
    position_counter: AtomicI32,
}

impl NestedToolRuntime {
    pub fn new(
        dispatcher: Arc<ToolDispatcher>,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        storage: Arc<dyn Storage>,
        turn_id: Uuid,
        parent_call_id: String,
        base_context: SerializableToolContext,
    ) -> Self {
        Self {
            dispatcher,
            ws_tx,
            storage,
            turn_id,
            parent_call_id,
            base_context,
            position_counter: AtomicI32::new(0),
        }
    }
}

#[async_trait]
impl swage_python::NestedToolHost for NestedToolRuntime {
    async fn dispatch_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value, WorkerToolError> {
        let call_id = Uuid::new_v4().to_string();
        let position = self.position_counter.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();

        let _ = self.ws_tx.send(ServerEvent::ToolStart {
            call_id: call_id.clone(),
            tool: tool_name.to_string(),
            arguments: arguments.clone(),
            parent_call_id: Some(self.parent_call_id.clone()),
        });

        let new_call = NewToolCall {
            turn_id: self.turn_id,
            agent_id: "gateway".to_string(),
            call_id: call_id.clone(),
            tool: tool_name.to_string(),
            arguments: serde_json::to_string(&arguments).unwrap_or_default(),
            model_arguments: compact_tool_arguments_for_model(&arguments),
            result: None,
            model_result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
            provider_round: 0,
            position,
            parent_call_id: Some(self.parent_call_id.clone()),
        };
        if let Err(error) = self.storage.create_tool_call(new_call).await {
            let failure = error.to_string();
            self.finish_tool_call(
                &call_id,
                tool_name,
                &Value::String(failure.clone()),
                "error",
                0,
            )
            .await;
            return Err(WorkerToolError {
                message: failure,
                result: None,
            });
        }

        let ctx = SerializableToolContext {
            call_id: Some(call_id.clone()),
            turn_id: Some(self.turn_id),
            ..self.base_context.clone()
        };
        let agent_id = ctx.agent_id().to_owned();

        let dispatched = self
            .dispatcher
            .dispatch(tool_name, arguments, &ctx, &agent_id)
            .await;
        let duration_ms = start.elapsed().as_millis() as u64;

        match dispatched {
            Ok(output) => {
                self.finish_tool_call(&call_id, tool_name, &output.result, "success", duration_ms)
                    .await;
                Ok(output.result)
            }
            Err(error) => {
                let failure = Value::String(error.to_string());
                self.finish_tool_call(&call_id, tool_name, &failure, "error", duration_ms)
                    .await;
                Err(WorkerToolError {
                    message: error.to_string(),
                    result: None,
                })
            }
        }
    }
}

impl NestedToolRuntime {
    async fn finish_tool_call(
        &self,
        call_id: &str,
        tool_name: &str,
        result: &Value,
        status: &str,
        duration_ms: u64,
    ) {
        let result_text = serde_json::to_string(result).unwrap_or_else(|_| result.to_string());
        let metadata = extract_tool_result_metadata(result);
        let event_total_bytes = metadata
            .total_bytes
            .and_then(|total| u64::try_from(total).ok());
        let _ = self
            .storage
            .update_tool_call_result(
                call_id,
                self.turn_id,
                &result_text,
                status,
                duration_ms as i64,
                metadata.truncated,
                metadata.total_bytes,
            )
            .await;

        let _ = self.ws_tx.send(ServerEvent::ToolEnd {
            call_id: call_id.to_string(),
            tool: tool_name.to_string(),
            result: result.clone(),
            status: status.to_string(),
            duration_ms,
            truncated: metadata.truncated,
            total_bytes: event_total_bytes,
            parent_call_id: Some(self.parent_call_id.clone()),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use swage_core::ToolError;
    use swage_python::NestedToolHost;
    use swage_storage::model::{NewSession, NewThread, NewTurn, TurnState};
    use swage_storage::sqlite::SqliteStorage;
    use swage_storage::store::{
        SessionStore, SpaceStore, Storage, ThreadStore, ToolCallStore, TurnStore,
    };
    use swage_tool::context::{MockToolContext, SerializableToolContext, ToolContext, UserInfo};
    use swage_tool::tool::Tool;

    use super::NestedToolRuntime;

    #[derive(Debug, Clone)]
    struct ObservedContext {
        agent_id: String,
        session_id: String,
        working_directory: PathBuf,
        allowed_paths: Vec<PathBuf>,
        user_name: Option<String>,
        call_id: Option<String>,
        turn_id: Option<uuid::Uuid>,
    }

    struct ObservingTool {
        observed: Arc<Mutex<Option<ObservedContext>>>,
    }

    #[async_trait]
    impl Tool for ObservingTool {
        fn name(&self) -> &str {
            "observe_context"
        }

        fn description(&self) -> &str {
            "Record the tool context received by a nested dispatch."
        }

        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        }

        async fn execute(&self, _params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            let observed = ObservedContext {
                agent_id: ctx.agent_id().to_owned(),
                session_id: ctx.session_id().to_owned(),
                working_directory: ctx.working_directory().to_owned(),
                allowed_paths: ctx.allowed_paths().to_owned(),
                user_name: ctx.user_info().map(|user| user.name.clone()),
                call_id: ctx.call_id().map(ToOwned::to_owned),
                turn_id: ctx.turn_id(),
            };
            *self.observed.lock().expect("observed context lock") = Some(observed);
            Ok(json!({ "ok": true }))
        }
    }

    async fn create_turn(storage: &SqliteStorage) -> uuid::Uuid {
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
            storage,
            NewThread {
                session_id: session.id,
                agent_id: "agent-a".into(),
            },
        )
        .await
        .unwrap();
        storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Processing,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nested_dispatch_inherits_parent_context_and_stamps_child_call() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let storage_dyn: Arc<dyn Storage> = storage.clone();
        let turn_id = create_turn(storage.as_ref()).await;
        let observed = Arc::new(Mutex::new(None));
        let tool: Arc<dyn Tool> = Arc::new(ObservingTool {
            observed: Arc::clone(&observed),
        });
        let dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(vec![tool]));
        let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let parent_context = MockToolContext {
            agent_id: "agent-a".into(),
            session_id: "session-123".into(),
            working_directory: PathBuf::from("/workspace/project"),
            allowed_paths: vec![PathBuf::from("/workspace/project")],
            user_info: Some(UserInfo {
                id: "user-1".into(),
                name: "Taylor".into(),
                metadata: HashMap::new(),
            }),
            call_id: Some("python_parent".into()),
            turn_id: Some(turn_id),
        };
        let runtime = NestedToolRuntime::new(
            dispatcher,
            ws_tx,
            storage_dyn,
            turn_id,
            "python_parent".into(),
            SerializableToolContext::from_context(&parent_context),
        );

        let output = runtime
            .dispatch_tool("observe_context", json!({}))
            .await
            .unwrap();

        assert_eq!(output, json!({ "ok": true }));
        let observed = observed
            .lock()
            .expect("observed context lock")
            .clone()
            .expect("nested tool should observe context");
        assert_eq!(observed.agent_id, "agent-a");
        assert_eq!(observed.session_id, "session-123");
        assert_eq!(observed.working_directory, Path::new("/workspace/project"));
        assert_eq!(
            observed.allowed_paths,
            vec![PathBuf::from("/workspace/project")]
        );
        assert_eq!(observed.user_name.as_deref(), Some("Taylor"));
        assert_eq!(observed.turn_id, Some(turn_id));
        assert_ne!(observed.call_id.as_deref(), Some("python_parent"));

        let nested_call = storage
            .list_tool_calls_by_turn(turn_id)
            .await
            .unwrap()
            .pop()
            .expect("nested call should be persisted");
        assert_eq!(nested_call.tool, "observe_context");
        assert_eq!(nested_call.parent_call_id.as_deref(), Some("python_parent"));
        assert_eq!(
            observed.call_id.as_deref(),
            Some(nested_call.call_id.as_str())
        );
    }
}
