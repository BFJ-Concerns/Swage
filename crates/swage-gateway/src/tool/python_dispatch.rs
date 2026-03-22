//! GatewayToolDispatcher — bridges Python tool calls to the gateway's ToolRegistry.
//!
//! Created once per `python_execute` invocation. All sub-tool events and storage
//! records emitted by this dispatcher carry the `parent_call_id` of the enclosing
//! `python_execute` call, enabling the frontend to render them nested.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use swage_python::dispatch::{DispatchResult, ToolDispatcher};
use swage_storage::model::NewToolCall;
use swage_storage::store::Storage;

use crate::tool::ToolRegistry;
use crate::websocket::ServerEvent;

// ---------------------------------------------------------------------------
// GatewayToolDispatcher
// ---------------------------------------------------------------------------

/// Implements `ToolDispatcher` by routing Python tool calls through the
/// gateway's `ToolRegistry`.
///
/// # Async-to-sync bridging
///
/// `dispatch()` is synchronous (required by the trait). It bridges async tool
/// execution by spawning each operation as a tokio task and blocking on the
/// join handle. Spawning rather than calling `block_on` directly is necessary
/// because `SqliteStorage::with_conn` uses `block_in_place` internally, which
/// requires running on a tokio worker thread.
///
/// This is safe because `python_execute` always runs the dispatcher inside
/// `spawn_blocking`, so blocking the current thread is acceptable.
pub struct GatewayToolDispatcher {
    registry: Arc<ToolRegistry>,
    /// Tokio handle used to drive async operations from the blocking thread.
    handle: tokio::runtime::Handle,
    /// Channel for forwarding sub-tool lifecycle events to the WebSocket sender.
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    /// Persistent storage for sub-tool call records.
    storage: Arc<dyn Storage>,
    /// Turn that `python_execute` is running within.
    turn_id: Uuid,
    /// call_id of the enclosing `python_execute` invocation.
    parent_call_id: String,
    /// Monotonically increasing position counter for ordering sub-tool records.
    ///
    /// `AtomicI32` provides interior mutability — `dispatch()` takes `&self`
    /// but must assign a unique, sequentially increasing position to each call.
    position_counter: AtomicI32,
}

impl GatewayToolDispatcher {
    pub fn new(
        registry: Arc<ToolRegistry>,
        handle: tokio::runtime::Handle,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        storage: Arc<dyn Storage>,
        turn_id: Uuid,
        parent_call_id: String,
    ) -> Self {
        Self {
            registry,
            handle,
            ws_tx,
            storage,
            turn_id,
            parent_call_id,
            position_counter: AtomicI32::new(0),
        }
    }
}

impl ToolDispatcher for GatewayToolDispatcher {
    fn dispatch(&self, tool_name: &str, arguments: Value) -> DispatchResult {
        let call_id = Uuid::new_v4().to_string();
        let position = self.position_counter.fetch_add(1, Ordering::Relaxed);

        // Notify the client that a sub-tool call has started.
        let _ = self.ws_tx.send(ServerEvent::ToolStart {
            call_id: call_id.clone(),
            tool: tool_name.to_string(),
            arguments: arguments.clone(),
            parent_call_id: Some(self.parent_call_id.clone()),
        });

        // Persist the initial record (no result yet). Use handle.spawn()
        // rather than tokio::task::spawn() because this code may run on a
        // plain OS thread (spawned by execute_code) without a tokio context.
        // handle.spawn() places the task on the runtime regardless of the
        // calling thread, and block_on() drives it to completion.
        let storage = Arc::clone(&self.storage);
        let new_call = NewToolCall {
            turn_id: self.turn_id,
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
        let _ = self.handle.block_on(
            self.handle.spawn(async move { storage.create_tool_call(new_call).await }),
        );

        // Look up the tool. Unknown tools return a well-formed error so Python
        // code can inspect the result rather than panicking.
        let tool = match self.registry.get(tool_name) {
            Some(t) => t,
            None => {
                let content = format!("Tool '{tool_name}' is not registered.");
                self.send_error_end(&call_id, tool_name, &content, 0);
                self.update_result(&call_id, &content, "error", 0, false, None);
                return DispatchResult { content, is_error: true };
            }
        };

        // Execute the tool on the runtime via handle.spawn() so that
        // block_in_place is available inside tools that may use it internally.
        let start = Instant::now();
        let exec = self
            .handle
            .block_on(self.handle.spawn(async move { tool.execute(arguments).await }))
            .expect("tool execution task should not panic");
        let duration_ms = start.elapsed().as_millis() as u64;

        let status = if exec.is_error { "error" } else { "success" };

        // Send ToolEnd to the WebSocket.
        let _ = self.ws_tx.send(ServerEvent::ToolEnd {
            call_id: call_id.clone(),
            tool: tool_name.to_string(),
            result: serde_json::json!(&exec.content),
            status: status.to_string(),
            duration_ms,
            truncated: exec.truncated,
            total_bytes: exec.total_bytes.map(|b| b as u64),
            parent_call_id: Some(self.parent_call_id.clone()),
        });

        // Persist the completed result.
        self.update_result(
            &call_id,
            &exec.content,
            status,
            duration_ms,
            exec.truncated,
            exec.total_bytes.map(|b| b as i64),
        );

        DispatchResult {
            content: exec.content,
            is_error: exec.is_error,
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

impl GatewayToolDispatcher {
    /// Send a `ToolEnd` event with error status and no duration.
    ///
    /// Called when a tool is not found — the client still expects a matching
    /// end event for every start event to keep its state machine consistent.
    fn send_error_end(&self, call_id: &str, tool_name: &str, content: &str, duration_ms: u64) {
        let _ = self.ws_tx.send(ServerEvent::ToolEnd {
            call_id: call_id.to_string(),
            tool: tool_name.to_string(),
            result: serde_json::json!(content),
            status: "error".to_string(),
            duration_ms,
            truncated: false,
            total_bytes: None,
            parent_call_id: Some(self.parent_call_id.clone()),
        });
    }

    /// Persist the completed result for a sub-tool call record.
    fn update_result(
        &self,
        call_id: &str,
        content: &str,
        status: &'static str,
        duration_ms: u64,
        truncated: bool,
        total_bytes: Option<i64>,
    ) {
        let storage = Arc::clone(&self.storage);
        let call_id = call_id.to_string();
        let content = content.to_string();
        let turn_id = self.turn_id;
        let _ = self.handle.block_on(self.handle.spawn(async move {
            storage
                .update_tool_call_result(
                    &call_id,
                    turn_id,
                    &content,
                    status,
                    duration_ms as i64,
                    truncated,
                    total_bytes,
                )
                .await
        }));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use swage_storage::model::{NewProject, NewSession, NewTurn, TurnState};
    use swage_storage::store::{ProjectStore, SessionStore, Storage, ToolCallStore, TurnStore};
    use swage_storage::SqliteStorage;

    use super::*;
    use crate::tool::{Tool, ToolExecutionResult, ToolRegistry};
    use crate::websocket::ServerEvent;

    // ---- Mock tools ---------------------------------------------------------

    /// Echoes the "text" argument back unchanged.
    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echo text back"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
        }
        async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
            let text = arguments["text"].as_str().unwrap_or("").to_string();
            ToolExecutionResult {
                content: text,
                is_error: false,
                truncated: false,
                total_bytes: None,
            }
        }
    }

    /// Always returns an error result.
    struct FailingTool;

    #[async_trait::async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "failing"
        }
        fn description(&self) -> &str {
            "Always fails"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _arguments: serde_json::Value) -> ToolExecutionResult {
            ToolExecutionResult {
                content: "tool failed".to_string(),
                is_error: true,
                truncated: false,
                total_bytes: None,
            }
        }
    }

    // ---- Helpers ------------------------------------------------------------

    fn registry_with_echo() -> ToolRegistry {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg
    }

    fn make_dispatcher(
        registry: ToolRegistry,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        storage: Arc<dyn Storage>,
        turn_id: Uuid,
        parent_call_id: &str,
    ) -> GatewayToolDispatcher {
        GatewayToolDispatcher::new(
            Arc::new(registry),
            tokio::runtime::Handle::current(),
            ws_tx,
            storage,
            turn_id,
            parent_call_id.to_string(),
        )
    }

    fn collect_events(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        events
    }

    /// Set up an in-memory DB with a project, session, and turn so that
    /// `create_tool_call` can satisfy foreign key constraints.
    async fn make_seeded_turn() -> (Arc<dyn Storage>, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject { name: "test".into(), slug: "test".into(), workspace_path: None })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession { project_id: project.id })
            .await
            .unwrap();
        let turn = storage
            .create_turn(NewTurn { session_id: session.id, state: TurnState::InProgress })
            .await
            .unwrap();
        (Arc::new(storage) as Arc<dyn Storage>, turn.id)
    }

    // ---- Tests --------------------------------------------------------------

    /// Happy path: known tool returns its output.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_returns_tool_output() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, _rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(
            registry_with_echo(),
            tx,
            storage,
            Uuid::new_v4(),
            "py_01",
        ));

        let result = tokio::task::spawn_blocking(move || {
            d.dispatch("echo", json!({"text": "hello world"}))
        })
        .await
        .unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "hello world");
    }

    /// Unknown tool name returns an error.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_unknown_tool_returns_error() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, _rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(
            ToolRegistry::new(),
            tx,
            storage,
            Uuid::new_v4(),
            "py_02",
        ));

        let result = tokio::task::spawn_blocking(move || {
            d.dispatch("nonexistent", json!({}))
        })
        .await
        .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("nonexistent"), "error message should name the tool");
    }

    /// ToolStart event must carry the parent's call_id.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_sends_tool_start_with_parent_call_id() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(
            registry_with_echo(),
            tx,
            storage,
            Uuid::new_v4(),
            "python_001",
        ));

        tokio::task::spawn_blocking(move || d.dispatch("echo", json!({"text": "t"})))
            .await
            .unwrap();

        let events = collect_events(&mut rx);
        let start = events.iter().find(|e| matches!(e, ServerEvent::ToolStart { .. }));
        assert!(start.is_some(), "expected a ToolStart event");

        if let Some(ServerEvent::ToolStart { tool, parent_call_id, .. }) = start {
            assert_eq!(tool, "echo");
            assert_eq!(parent_call_id.as_deref(), Some("python_001"));
        }
    }

    /// ToolEnd event must carry the parent's call_id and success status.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_sends_tool_end_with_parent_call_id() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(
            registry_with_echo(),
            tx,
            storage,
            Uuid::new_v4(),
            "python_002",
        ));

        tokio::task::spawn_blocking(move || d.dispatch("echo", json!({"text": "end"})))
            .await
            .unwrap();

        let events = collect_events(&mut rx);
        let end = events.iter().find(|e| matches!(e, ServerEvent::ToolEnd { .. }));
        assert!(end.is_some(), "expected a ToolEnd event");

        if let Some(ServerEvent::ToolEnd { tool, status, parent_call_id, .. }) = end {
            assert_eq!(tool, "echo");
            assert_eq!(status, "success");
            assert_eq!(parent_call_id.as_deref(), Some("python_002"));
        }
    }

    /// Tool call record must be persisted with parent_call_id and final result.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_stores_tool_call_record_with_parent_call_id() {
        let (storage, turn_id) = make_seeded_turn().await;
        let storage_check = Arc::clone(&storage);
        let (tx, _rx) = mpsc::unbounded_channel();

        let d = Arc::new(make_dispatcher(
            registry_with_echo(),
            tx,
            storage,
            turn_id,
            "py_store",
        ));

        tokio::task::spawn_blocking(move || d.dispatch("echo", json!({"text": "stored"})))
            .await
            .unwrap();

        let records = storage_check.list_tool_calls_by_turn(turn_id).await.unwrap();
        assert_eq!(records.len(), 1, "expected one tool call record");
        let r = &records[0];
        assert_eq!(r.tool, "echo");
        assert_eq!(r.parent_call_id.as_deref(), Some("py_store"));
        assert_eq!(r.result.as_deref(), Some("stored"));
        assert_eq!(r.status.as_deref(), Some("success"));
    }

    /// Sequential calls receive monotonically increasing positions.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_increments_position_for_sequential_calls() {
        let (storage, turn_id) = make_seeded_turn().await;
        let storage_check = Arc::clone(&storage);
        let (tx, _rx) = mpsc::unbounded_channel();

        let d = Arc::new(make_dispatcher(
            registry_with_echo(),
            tx,
            storage,
            turn_id,
            "py_pos",
        ));

        tokio::task::spawn_blocking(move || {
            d.dispatch("echo", json!({"text": "first"}));
            d.dispatch("echo", json!({"text": "second"}));
        })
        .await
        .unwrap();

        let records = storage_check.list_tool_calls_by_turn(turn_id).await.unwrap();
        assert_eq!(records.len(), 2, "both calls should be persisted");
        // Records are ordered by position ASC.
        assert_eq!(records[0].position, 0);
        assert_eq!(records[1].position, 1);
    }

    /// Tool that returns is_error=true must produce an error result.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_failing_tool_returns_error_result() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FailingTool));
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(registry, tx, storage, Uuid::new_v4(), "py_fail"));

        let result =
            tokio::task::spawn_blocking(move || d.dispatch("failing", json!({})))
                .await
                .unwrap();

        assert!(result.is_error);

        let events = collect_events(&mut rx);
        let end = events.iter().find(|e| matches!(e, ServerEvent::ToolEnd { .. }));
        assert!(end.is_some());
        if let Some(ServerEvent::ToolEnd { status, .. }) = end {
            assert_eq!(status, "error");
        }
    }

    /// Unknown tool must still emit matching ToolStart + ToolEnd events so the
    /// client state machine stays consistent.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_unknown_tool_still_emits_tool_start_and_end() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(
            ToolRegistry::new(),
            tx,
            storage,
            Uuid::new_v4(),
            "py_miss",
        ));

        tokio::task::spawn_blocking(move || d.dispatch("missing", json!({})))
            .await
            .unwrap();

        let events = collect_events(&mut rx);
        assert!(
            events.iter().any(|e| matches!(e, ServerEvent::ToolStart { .. })),
            "expected ToolStart even for unknown tool"
        );
        let error_end = events.iter().find(|e| {
            matches!(e, ServerEvent::ToolEnd { status, .. } if status == "error")
        });
        assert!(error_end.is_some(), "expected ToolEnd with error status");

        // The error end must also carry parent_call_id.
        if let Some(ServerEvent::ToolEnd { parent_call_id, .. }) = error_end {
            assert_eq!(parent_call_id.as_deref(), Some("py_miss"));
        }
    }
}
