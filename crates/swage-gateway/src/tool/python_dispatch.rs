//! GatewayToolDispatcher — bridges Python tool calls to the gateway's dispatch pipeline.
//!
//! Created once per `python_execute` invocation. All sub-tool events and storage
//! records emitted by this dispatcher carry the `parent_call_id` of the enclosing
//! `python_execute` call, enabling the frontend to render them nested.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use swage_python::dispatch::{DispatchResult, ToolDispatcher};
use swage_storage::model::NewToolCall;
use swage_storage::store::Storage;
// Alias to avoid name collision with the swage_python ToolDispatcher trait.
use swage_tool::dispatch::ToolDispatcher as SwageToolDispatcher;

use crate::websocket::ServerEvent;

// ---------------------------------------------------------------------------
// GatewayToolDispatcher
// ---------------------------------------------------------------------------

/// Implements `ToolDispatcher` by routing Python tool calls through the
/// gateway's `SwageToolDispatcher` (the full permission → rate-limit →
/// validate → execute → leak-detect pipeline).
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
    /// The gateway's dispatch pipeline — handles lookup, permissions, rate
    /// limiting, schema validation, execution, and leak detection.
    dispatcher: Arc<SwageToolDispatcher>,
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
    /// Working directory forwarded to tools that derive paths from context.
    working_directory: PathBuf,
    /// Monotonically increasing position counter for ordering sub-tool records.
    ///
    /// `AtomicI32` provides interior mutability — `dispatch()` takes `&self`
    /// but must assign a unique, sequentially increasing position to each call.
    position_counter: AtomicI32,
}

impl GatewayToolDispatcher {
    pub fn new(
        dispatcher: Arc<SwageToolDispatcher>,
        handle: tokio::runtime::Handle,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        storage: Arc<dyn Storage>,
        turn_id: Uuid,
        parent_call_id: String,
        working_directory: PathBuf,
    ) -> Self {
        Self {
            dispatcher,
            handle,
            ws_tx,
            storage,
            turn_id,
            parent_call_id,
            working_directory,
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
        let _ = self.handle.block_on(
            self.handle.spawn(async move { storage.create_tool_call(new_call).await }),
        );

        // Dispatch the tool call through the full pipeline.
        // Build a context inside the spawn so it can be borrowed by dispatch().
        let dispatcher = Arc::clone(&self.dispatcher);
        let working_directory = self.working_directory.clone();
        let tool_name_owned = tool_name.to_string();

        let start = Instant::now();
        let exec = self
            .handle
            .block_on(self.handle.spawn(async move {
                let ctx = swage_tool::context::MockToolContext {
                    agent_id: "gateway-dispatcher".to_string(),
                    session_id: String::new(),
                    working_directory,
                    allowed_paths: vec![],
                    user_info: None,
                };
                dispatcher
                    .dispatch(&tool_name_owned, arguments, &ctx, "gateway-dispatcher")
                    .await
            }))
            .expect("tool dispatch task should not panic");
        let duration_ms = start.elapsed().as_millis() as u64;

        let (output, is_error) = match exec {
            Ok(tool_output) => (
                tool_output.result["output"].as_str().unwrap_or("").to_string(),
                false,
            ),
            Err(e) => (e.to_string(), true),
        };

        let status = if is_error { "error" } else { "success" };

        // Send ToolEnd to the WebSocket.
        let _ = self.ws_tx.send(ServerEvent::ToolEnd {
            call_id: call_id.clone(),
            tool: tool_name.to_string(),
            result: serde_json::json!(&output),
            status: status.to_string(),
            duration_ms,
            truncated: false,
            total_bytes: None,
            parent_call_id: Some(self.parent_call_id.clone()),
        });

        // Persist the completed result.
        self.update_result(&call_id, &output, status, duration_ms, false, None);

        DispatchResult { content: output, is_error }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

impl GatewayToolDispatcher {
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
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use swage_core::ToolError;
    use swage_storage::model::{NewProject, NewSession, NewThread, NewTurn, TurnState};
    use swage_storage::store::{ProjectStore, SessionStore, Storage, ThreadStore, ToolCallStore, TurnStore};
    use swage_storage::SqliteStorage;
    use swage_tool::context::ToolContext;
    use swage_tool::dispatch::ToolDispatcher as SwageToolDispatcher;

    use super::*;
    use crate::tool::Tool;
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
        async fn execute(
            &self,
            arguments: serde_json::Value,
            _ctx: &dyn ToolContext,
        ) -> Result<serde_json::Value, ToolError> {
            let text = arguments["text"].as_str().unwrap_or("").to_string();
            Ok(json!({"output": text}))
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
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _ctx: &dyn ToolContext,
        ) -> Result<serde_json::Value, ToolError> {
            Err(ToolError::ExecutionFailed {
                name: "failing".to_string(),
                message: "tool failed".to_string(),
            })
        }
    }

    // ---- Helpers ------------------------------------------------------------

    fn dispatcher_with_echo() -> Arc<SwageToolDispatcher> {
        Arc::new(SwageToolDispatcher::allow_all(vec![
            Arc::new(EchoTool) as Arc<dyn swage_tool::tool::Tool>,
        ]))
    }

    fn empty_dispatcher() -> Arc<SwageToolDispatcher> {
        Arc::new(SwageToolDispatcher::allow_all(vec![]))
    }

    fn make_dispatcher(
        sub_dispatcher: Arc<SwageToolDispatcher>,
        ws_tx: mpsc::UnboundedSender<ServerEvent>,
        storage: Arc<dyn Storage>,
        turn_id: Uuid,
        parent_call_id: &str,
    ) -> GatewayToolDispatcher {
        GatewayToolDispatcher::new(
            sub_dispatcher,
            tokio::runtime::Handle::current(),
            ws_tx,
            storage,
            turn_id,
            parent_call_id.to_string(),
            PathBuf::from("/tmp"),
        )
    }

    fn collect_events(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        events
    }

    /// Set up an in-memory DB with a project, session, thread, and turn so that
    /// `create_tool_call` can satisfy foreign key constraints.
    async fn make_seeded_turn() -> (Arc<dyn Storage>, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = ProjectStore::create(
            &storage,
            NewProject { name: "test".into(), slug: "test".into(), workspace_path: None },
        )
        .await
        .unwrap();
        let session = storage
            .create_session(NewSession { project_id: project.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &storage,
            NewThread { session_id: session.id, agent_id: "gateway".to_string() },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".to_string(),
                state: TurnState::Processing,
                parent_turn_id: None,
            })
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
            dispatcher_with_echo(),
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
            empty_dispatcher(),
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
            dispatcher_with_echo(),
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
            dispatcher_with_echo(),
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
            dispatcher_with_echo(),
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
            dispatcher_with_echo(),
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

    /// Tool that returns Err must produce an error result.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_failing_tool_returns_error_result() {
        let sub_dispatcher = Arc::new(SwageToolDispatcher::allow_all(vec![
            Arc::new(FailingTool) as Arc<dyn swage_tool::tool::Tool>,
        ]));
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(sub_dispatcher, tx, storage, Uuid::new_v4(), "py_fail"));

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

    /// A tool with an explicit deny permission is blocked at the pipeline gate.
    ///
    /// This verifies the security property: even if Python code calls swage.shell(),
    /// the GatewayToolDispatcher routes through the full swage-tool pipeline, so a
    /// deny-listed tool returns a permission error rather than executing.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_denied_tool_returns_permission_error() {
        use std::collections::HashMap;
        use swage_safety::LeakDetector;
        use swage_tool::permission::PermissionConfig;
        use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};

        // Build a sub-dispatcher that registers "echo" but explicitly denies it.
        // This simulates an agent config where shell (or any tool) is blocked.
        let mut registry = swage_tool::registry::ToolRegistry::new();
        registry.register_builtin(Arc::new(EchoTool) as Arc<dyn swage_tool::tool::Tool>).unwrap();
        let restricted_dispatcher = Arc::new(SwageToolDispatcher::new(
            Arc::new(registry),
            PermissionConfig {
                allow: vec![],
                deny: vec!["echo".to_string()],
                bundles: HashMap::new(),
                require_approval: vec![],
            },
            Arc::new(RateLimiter::new()),
            RateLimitConfig::default(),
            Arc::new(LeakDetector::default()),
            false,
        ));

        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, _rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(
            restricted_dispatcher,
            tx,
            storage,
            Uuid::new_v4(),
            "py_deny",
        ));

        let result = tokio::task::spawn_blocking(move || {
            d.dispatch("echo", json!({"text": "should be blocked"}))
        })
        .await
        .unwrap();

        assert!(result.is_error, "denied tool should return an error result");
        assert!(
            result.content.contains("permission denied"),
            "error should describe permission denial, got: {}",
            result.content
        );
    }

    /// Unknown tool must still emit matching ToolStart + ToolEnd events so the
    /// client state machine stays consistent.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_unknown_tool_still_emits_tool_start_and_end() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap()) as Arc<dyn Storage>;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let d = Arc::new(make_dispatcher(
            empty_dispatcher(),
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
