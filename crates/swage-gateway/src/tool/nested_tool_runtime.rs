use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use swage_python::worker_protocol::WorkerToolError;
use swage_storage::model::NewToolCall;
use swage_storage::store::Storage;
use swage_tool::dispatch::ToolDispatcher;

use crate::websocket::ServerEvent;

pub struct NestedToolRuntime {
    dispatcher: Arc<ToolDispatcher>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    storage: Arc<dyn Storage>,
    turn_id: Uuid,
    parent_call_id: String,
    working_directory: PathBuf,
    position_counter: AtomicI32,
}

impl NestedToolRuntime {
    pub fn new(
        dispatcher: Arc<ToolDispatcher>,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        storage: Arc<dyn Storage>,
        turn_id: Uuid,
        parent_call_id: String,
        working_directory: PathBuf,
    ) -> Self {
        Self {
            dispatcher,
            ws_tx,
            storage,
            turn_id,
            parent_call_id,
            working_directory,
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
            result: None,
            status: None,
            duration_ms: None,
            truncated: false,
            total_bytes: None,
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

        let ctx = swage_tool::context::MockToolContext {
            agent_id: "gateway-dispatcher".to_string(),
            session_id: String::new(),
            working_directory: self.working_directory.clone(),
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };

        let dispatched = self
            .dispatcher
            .dispatch(tool_name, arguments, &ctx, "gateway-dispatcher")
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
        let _ = self
            .storage
            .update_tool_call_result(
                call_id,
                self.turn_id,
                &result_text,
                status,
                duration_ms as i64,
                false,
                None,
            )
            .await;

        let _ = self.ws_tx.send(ServerEvent::ToolEnd {
            call_id: call_id.to_string(),
            tool: tool_name.to_string(),
            result: result.clone(),
            status: status.to_string(),
            duration_ms,
            truncated: false,
            total_bytes: None,
            parent_call_id: Some(self.parent_call_id.clone()),
        });
    }
}
