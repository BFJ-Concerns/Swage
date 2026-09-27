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

        let display_summary = self.dispatcher.display_summary(tool_name, &arguments);
        let _ = self.ws_tx.send(ServerEvent::ToolStart {
            call_id: call_id.clone(),
            tool: tool_name.to_string(),
            arguments: arguments.clone(),
            parent_call_id: Some(self.parent_call_id.clone()),
            summary: display_summary.clone(),
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
            display_summary,
        };
        if let Err(error) = self.storage.create_tool_call(new_call).await {
            // No row was written, so there is nothing to settle; the call is
            // still finished for the client's sake (its ToolEnd), and the
            // refused dispatch is the loud response the contract asks of a
            // constitutive write (record_durability.rs, class 1).
            let failure = error.to_string();
            let _ = self
                .finish_tool_call(
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

        // The settlement is the second half of a constitutive record
        // (record_durability.rs, class 1): a call whose outcome could not be
        // written did not auditably happen, so the caller is told so rather
        // than handed a result the trail will never show.
        match dispatched {
            Ok(output) => {
                if let Err(error) = self
                    .finish_tool_call(&call_id, tool_name, &output.result, "success", duration_ms)
                    .await
                {
                    return Err(WorkerToolError {
                        message: format!(
                            "tool '{tool_name}' ran but its record could not be settled: {error}"
                        ),
                        result: None,
                    });
                }
                Ok(output.result)
            }
            Err(error) => {
                let failure = Value::String(error.to_string());
                let message = match self
                    .finish_tool_call(&call_id, tool_name, &failure, "error", duration_ms)
                    .await
                {
                    Ok(()) => error.to_string(),
                    Err(settlement) => {
                        format!("{error}; and the call's record could not be settled: {settlement}")
                    }
                };
                Err(WorkerToolError {
                    message,
                    result: None,
                })
            }
        }
    }
}

impl NestedToolRuntime {
    /// Settle the call's row and tell the client it ended. The settlement's
    /// failure is returned, never dropped: the caller decides what a call
    /// with no recorded outcome means to the model.
    async fn finish_tool_call(
        &self,
        call_id: &str,
        tool_name: &str,
        result: &Value,
        status: &str,
        duration_ms: u64,
    ) -> Result<(), swage_core::StorageError> {
        let result_text = serde_json::to_string(result).unwrap_or_else(|_| result.to_string());
        // History can replay this result to the provider. Scan it before
        // compaction, as the ordinary agent call path does.
        let safe_result = swage_safety::LeakDetector::default().scan(&result_text);
        let model_result = swage_core::compact_tool_result_for_model_with_reference(
            &safe_result,
            status != "success",
            swage_core::tool_result_reference(&self.turn_id.to_string(), call_id),
        );
        let metadata = extract_tool_result_metadata(result);
        let event_total_bytes = metadata
            .total_bytes
            .and_then(|total| u64::try_from(total).ok());
        let settled = self
            .storage
            .update_tool_call_result(
                call_id,
                self.turn_id,
                &result_text,
                &model_result,
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
        settled
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use crate::websocket::ServerEvent;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use swage_core::ToolError;
    use swage_python::NestedToolHost;
    use swage_storage::fixtures::new_session;
    use swage_storage::model::{NewSession, NewThread, NewTurn, TurnState};
    use swage_storage::sqlite::SqliteStorage;
    use swage_storage::store::{
        SessionStore, Storage, ThreadStore, ToolCallStore, TopicStore, TurnStore,
    };
    use swage_tool::context::{MockToolContext, ToolContext, UserInfo, serialise_tool_context};
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

    #[tokio::test(flavor = "multi_thread")]
    async fn nested_model_result_redacts_credentials_before_replay() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let turn_id = create_turn(storage.as_ref()).await;
        let dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(
            Vec::<Arc<dyn Tool>>::new(),
        ));
        let (ws_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let runtime = NestedToolRuntime::new(
            dispatcher,
            ws_tx,
            storage.clone(),
            turn_id,
            "python_parent".into(),
            serialise_tool_context(&parent_context(turn_id)),
        );
        storage
            .create_tool_call(swage_storage::model::NewToolCall {
                turn_id,
                agent_id: "gateway".into(),
                call_id: "nested-secret".into(),
                tool: "secret_tool".into(),
                arguments: "{}".into(),
                model_arguments: "{}".into(),
                result: None,
                model_result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                provider_round: 1,
                position: 0,
                parent_call_id: Some("python_parent".into()),
                display_summary: None,
            })
            .await
            .unwrap();
        runtime
            .finish_tool_call(
                "nested-secret",
                "secret_tool",
                &json!({"token": "sk-abcdefghijklmnopqrstuvwxyz123456"}),
                "success",
                1,
            )
            .await
            .unwrap();
        let call = storage
            .list_tool_calls_by_turn(turn_id)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert!(
            call.result
                .unwrap()
                .contains("sk-abcdefghijklmnopqrstuvwxyz123456")
        );
        assert!(
            !call
                .model_result
                .unwrap()
                .contains("sk-abcdefghijklmnopqrstuvwxyz123456"),
            "the provider-facing nested result contains the credential"
        );
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
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "agent-a".into(),
                ..new_session(topic.id)
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

    fn parent_context(turn_id: uuid::Uuid) -> MockToolContext {
        MockToolContext {
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
        }
    }

    #[tokio::test]
    async fn a_call_whose_settlement_fails_is_reported_as_unrecorded() {
        let storage = Arc::new(crate::turn::tests::MockStorage::new());
        let topic = storage.general_scope().await.unwrap();
        let session = storage.create_session(new_session(topic.id)).await.unwrap();
        let thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: "gateway".into(),
            },
        )
        .await
        .unwrap();
        let turn_id = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".into(),
                state: TurnState::Processing,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap()
            .id;
        storage.fail_tool_call_settlement();
        let tool: Arc<dyn Tool> = Arc::new(ObservingTool {
            observed: Arc::new(Mutex::new(None)),
        });
        let dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(vec![tool]));
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime = NestedToolRuntime::new(
            dispatcher,
            ws_tx,
            storage.clone() as Arc<dyn Storage>,
            turn_id,
            "python_parent".into(),
            serialise_tool_context(&parent_context(turn_id)),
        );

        let error = runtime
            .dispatch_tool("observe_context", json!({}))
            .await
            .expect_err(
                "the tool ran, but a call with no recorded outcome did not auditably happen",
            );
        assert!(
            error.message.contains("could not be settled"),
            "{}",
            error.message
        );

        let mut events = Vec::new();
        while let Ok(event) = ws_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ServerEvent::ToolEnd { .. })),
            "the client is still told the call ended"
        );
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
        let parent_context = parent_context(turn_id);
        let runtime = NestedToolRuntime::new(
            dispatcher,
            ws_tx,
            storage_dyn,
            turn_id,
            "python_parent".into(),
            serialise_tool_context(&parent_context),
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
