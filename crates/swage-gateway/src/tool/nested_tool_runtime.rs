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
            execution: None,
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
        let record = match self.storage.create_tool_call(new_call).await {
            Ok(record) => record,
            Err(error) => {
                // No row was written, so there is nothing to settle; the call is
                // still finished for the client's sake (its ToolEnd), and the
                // refused dispatch is the loud response the contract asks of a
                // constitutive write (record_durability.rs, class 1).
                let failure = error.to_string();
                let _ = self.ws_tx.send(ServerEvent::ToolEnd {
                    execution: Some(swage_protocol::client::conversation::ToolExecution::refused()),
                    call_id: call_id.clone(),
                    tool: tool_name.to_string(),
                    result: Value::String(failure.clone()),
                    status: "error".to_string(),
                    duration_ms: 0,
                    truncated: false,
                    total_bytes: None,
                    parent_call_id: Some(self.parent_call_id.clone()),
                });
                return Err(WorkerToolError {
                    message: failure,
                    result: None,
                });
            }
        };

        let ctx = SerializableToolContext {
            call_id: Some(call_id.clone()),
            turn_id: Some(self.turn_id),
            ..self.base_context.clone()
        };
        let agent_id = ctx.agent_id().to_owned();

        // Nested native file tools run on the gateway through a no-strategy
        // dispatcher. Their wire context has no attachment authority, so check
        // the gateway's stored permissions before entering that dispatcher.
        let gateway_ctx = crate::tool_executor::GatewayToolContext::new(
            &ctx,
            self.ws_tx.clone(),
            self.storage.clone(),
        );
        let dispatched = match crate::tool::workspace::require_native_file_write_access(
            tool_name,
            &arguments,
            &gateway_ctx,
        )
        .await
        {
            Ok(()) => {
                self.dispatcher
                    .dispatch(tool_name, arguments, &ctx, &agent_id)
                    .await
            }
            Err(error) => Err(error),
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        // The settlement is the second half of a constitutive record
        // (record_durability.rs, class 1): a call whose outcome could not be
        // written did not auditably happen, so the caller is told so rather
        // than handed a result the trail will never show.
        match dispatched {
            Ok(output) => {
                if let Err(error) = self
                    .finish_tool_call(
                        record.id,
                        &call_id,
                        tool_name,
                        &output.result,
                        if output.execution.outcome
                            == swage_protocol::client::conversation::ToolOutcome::Succeeded
                        {
                            "success"
                        } else {
                            "error"
                        },
                        duration_ms,
                        output.execution,
                    )
                    .await
                {
                    return Err(WorkerToolError {
                        message: format!(
                            "tool '{tool_name}' ran but its record could not be settled: {error}"
                        ),
                        result: None,
                    });
                }
                if output.execution.outcome
                    == swage_protocol::client::conversation::ToolOutcome::Succeeded
                {
                    Ok(output.result)
                } else {
                    Err(WorkerToolError {
                        message: output.result.to_string(),
                        result: Some(output.result),
                    })
                }
            }
            Err(error) => {
                let failure = Value::String(error.to_string());
                let message = match self
                    .finish_tool_call(
                        record.id,
                        &call_id,
                        tool_name,
                        &failure,
                        "error",
                        duration_ms,
                        error.execution(),
                    )
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
        record_id: Uuid,
        call_id: &str,
        tool_name: &str,
        result: &Value,
        status: &str,
        duration_ms: u64,
        execution: swage_protocol::client::conversation::ToolExecution,
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
                record_id,
                &result_text,
                &model_result,
                status,
                duration_ms as i64,
                metadata.truncated,
                metadata.total_bytes,
                Some(execution),
            )
            .await;

        let _ = self.ws_tx.send(ServerEvent::ToolEnd {
            execution: Some(execution),
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
    async fn nested_native_file_mutations_respect_attachment_access() {
        use swage_storage::{NewWorkspaceAttachment, WorkspaceAttachmentStore};

        for writable in [false, true] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("existing.txt"), "original").unwrap();
            let storage = Arc::new(SqliteStorage::in_memory().unwrap());
            let turn_id = create_turn(storage.as_ref()).await;
            let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
            let thread = ThreadStore::get(storage.as_ref(), turn.thread_id)
                .await
                .unwrap()
                .unwrap();
            let session = storage
                .get_session(thread.session_id)
                .await
                .unwrap()
                .unwrap();
            storage
                .create_workspace_attachment(NewWorkspaceAttachment {
                    topic_id: session.topic_id,
                    label: "nested workspace".into(),
                    path: root.path().to_string_lossy().into_owned(),
                    permissions: if writable {
                        vec!["read".into(), "write".into()]
                    } else {
                        vec!["read".into()]
                    },
                    is_trusted: true,
                })
                .await
                .unwrap();
            let dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(
                crate::startup::workspace_tools(&swage_config::ResolvedConfig::default()),
            ));
            let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut context = parent_context(turn_id);
            context.session_id = session.id.to_string();
            context.working_directory = root.path().to_path_buf();
            context.allowed_paths = vec![root.path().to_path_buf()];
            let runtime = NestedToolRuntime::new(
                dispatcher,
                ws_tx,
                storage,
                turn_id,
                "python_parent".into(),
                serialise_tool_context(&context),
            );
            let write = runtime
                .dispatch_tool(
                    "file_write",
                    json!({"path": "probe.txt", "content": "written"}),
                )
                .await;
            let edit = runtime
                .dispatch_tool(
                    "file_edit",
                    json!({"changes": [{"path": "existing.txt", "edits": [
                        {"type": "replace", "old_text": "original", "new_text": "edited"}
                    ]}]}),
                )
                .await;
            if writable {
                assert!(
                    write.is_ok(),
                    "writable nested file_write failed: {write:?}"
                );
                assert!(edit.is_ok(), "writable nested file_edit failed: {edit:?}");
                assert_eq!(
                    std::fs::read_to_string(root.path().join("probe.txt")).unwrap(),
                    "written"
                );
                assert_eq!(
                    std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
                    "edited"
                );
            } else {
                for (tool, result) in [("file_write", write), ("file_edit", edit)] {
                    let error = result
                        .expect_err("nested native mutation must refuse read-only attachment");
                    assert!(
                        error.message.contains("read-only access"),
                        "{tool}: {error:?}"
                    );
                }
                assert!(!root.path().join("probe.txt").exists());
                assert_eq!(
                    std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
                    "original"
                );
            }
            let mut statuses = Vec::new();
            while let Ok(event) = ws_rx.try_recv() {
                if let ServerEvent::ToolEnd { status, .. } = event {
                    statuses.push(status);
                }
            }
            assert_eq!(
                statuses,
                vec![if writable { "success" } else { "error" }; 2]
            );
        }
    }

    #[cfg(unix)]
    async fn nested_dangling_symlink_write(chain: bool, writable: bool) {
        use std::os::unix::fs::symlink;
        use swage_storage::{NewWorkspaceAttachment, WorkspaceAttachmentStore};
        let root = tempfile::tempdir().unwrap();
        let target_root = root.path().join("target");
        std::fs::create_dir(&target_root).unwrap();
        let target = target_root.join("probe.txt");
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let turn_id = create_turn(storage.as_ref()).await;
        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        let thread = ThreadStore::get(storage.as_ref(), turn.thread_id)
            .await
            .unwrap()
            .unwrap();
        let session = storage
            .get_session(thread.session_id)
            .await
            .unwrap()
            .unwrap();
        for (path, can_write) in [(root.path(), true), (target_root.as_path(), writable)] {
            storage
                .create_workspace_attachment(NewWorkspaceAttachment {
                    topic_id: session.topic_id,
                    label: "symlink workspace".into(),
                    path: path.to_string_lossy().into_owned(),
                    permissions: if can_write {
                        vec!["read".into(), "write".into()]
                    } else {
                        vec!["read".into()]
                    },
                    is_trusted: true,
                })
                .await
                .unwrap();
        }
        if chain {
            symlink(&target, root.path().join("second-link")).unwrap();
            symlink("second-link", root.path().join("link")).unwrap();
        } else {
            symlink("target/probe.txt", root.path().join("link")).unwrap();
        }
        let mut context = parent_context(turn_id);
        context.session_id = session.id.to_string();
        context.working_directory = root.path().to_path_buf();
        context.allowed_paths = vec![root.path().to_path_buf()];
        let dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(
            crate::startup::workspace_tools(&swage_config::ResolvedConfig::default()),
        ));
        let (ws_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let runtime = NestedToolRuntime::new(
            dispatcher,
            ws_tx,
            storage,
            turn_id,
            "python_parent".into(),
            serialise_tool_context(&context),
        );
        let result = runtime
            .dispatch_tool("file_write", json!({"path": "link", "content": "written"}))
            .await;
        if writable {
            assert!(
                result.is_ok()
                    && std::fs::read_to_string(&target).is_ok_and(|text| text == "written"),
                "nested writable dangling symlink must create its target: {result:?}"
            );
        } else {
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.message.contains("read-only access"))
                    && !target.exists(),
                "nested dangling symlink must refuse read-only target without creating it: {result:?}; target exists: {}",
                target.exists()
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn nested_file_write_refuses_dangling_symlink() {
        nested_dangling_symlink_write(false, false).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn nested_file_write_refuses_dangling_symlink_chain() {
        nested_dangling_symlink_write(true, false).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn nested_file_write_allows_writable_dangling_symlink() {
        nested_dangling_symlink_write(false, true).await;
    }

    async fn nested_attachment_regression(scenario: &str) {
        use swage_storage::{NewWorkspaceAttachment, WorkspaceAttachmentStore};
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let turn_id = create_turn(storage.as_ref()).await;
        let turn = storage.get_turn(turn_id).await.unwrap().unwrap();
        let thread = ThreadStore::get(storage.as_ref(), turn.thread_id)
            .await
            .unwrap()
            .unwrap();
        let session = storage
            .get_session(thread.session_id)
            .await
            .unwrap()
            .unwrap();
        let writable = scenario != "child";
        storage
            .create_workspace_attachment(NewWorkspaceAttachment {
                topic_id: session.topic_id,
                label: "parent workspace".into(),
                path: root.path().to_string_lossy().into_owned(),
                permissions: if writable {
                    vec!["read".into(), "write".into()]
                } else {
                    vec!["read".into()]
                },
                is_trusted: true,
            })
            .await
            .unwrap();
        let target_root = if scenario == "child" {
            child.as_path()
        } else if scenario == "trimmed" {
            other.path()
        } else {
            root.path()
        };
        if scenario != "lookup" {
            let attachment = storage
                .create_workspace_attachment(NewWorkspaceAttachment {
                    topic_id: session.topic_id,
                    label: "target workspace".into(),
                    path: target_root.to_string_lossy().into_owned(),
                    permissions: if scenario == "child" {
                        vec!["read".into(), "write".into()]
                    } else {
                        vec!["read".into()]
                    },
                    is_trusted: true,
                })
                .await
                .unwrap();
            if scenario == "child" {
                storage
                    .set_session_active_workspace(session.id, Some(attachment.id))
                    .await
                    .unwrap();
            }
        } else {
            storage.with_conn(|conn| conn.execute_batch("ALTER TABLE workspace_attachments RENAME TO unavailable_workspace_attachments")).await.unwrap();
        }
        let target = target_root.join("existing.txt");
        std::fs::write(&target, "original").unwrap();
        let mut context = parent_context(turn_id);
        context.session_id = session.id.to_string();
        context.working_directory = if scenario == "child" {
            child.clone()
        } else {
            root.path().to_path_buf()
        };
        context.allowed_paths = vec![root.path().to_path_buf(), other.path().to_path_buf()];
        let dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(
            crate::startup::workspace_tools(&swage_config::ResolvedConfig::default()),
        ));
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime = NestedToolRuntime::new(
            dispatcher,
            ws_tx,
            storage,
            turn_id,
            "python_parent".into(),
            serialise_tool_context(&context),
        );
        let path = if scenario == "trimmed" {
            format!("  {}  ", target.display())
        } else {
            "existing.txt".into()
        };
        let edit = runtime.dispatch_tool("file_edit", json!({"changes": [
            {"path": path, "edits": [{"type": "replace", "old_text": "original", "new_text": "edited"}]}
        ]})).await;
        if scenario == "child" {
            assert!(edit.is_ok(), "active writable child edit failed: {edit:?}");
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "edited");
        } else {
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                "original",
                "nested edit mutated a refused target"
            );
            let error = edit.expect_err("nested edit must refuse");
            assert!(
                error.message.contains(if scenario == "lookup" {
                    "Cannot check workspace write access"
                } else {
                    "read-only access"
                }),
                "{error:?}"
            );
            if scenario == "lookup" {
                assert!(
                    error
                        .message
                        .contains("no such table: workspace_attachments"),
                    "{error:?}"
                );
            }
        }
        let write = runtime
            .dispatch_tool(
                "file_write",
                json!({"path": "nested/probe.txt", "content": "written"}),
            )
            .await;
        if scenario == "child" {
            assert!(
                write.is_ok(),
                "active writable child write failed: {write:?}"
            );
            assert_eq!(
                std::fs::read_to_string(child.join("nested/probe.txt")).unwrap(),
                "written"
            );
        } else if scenario == "lookup" {
            assert!(!root.path().join("nested").exists());
            let error = write.expect_err("nested write must refuse attachment lookup failure");
            assert!(
                error
                    .message
                    .contains("Cannot check workspace write access"),
                "{error:?}"
            );
            assert!(
                error
                    .message
                    .contains("no such table: workspace_attachments"),
                "{error:?}"
            );
        } else {
            assert!(
                write.is_ok(),
                "writable working directory control failed: {write:?}"
            );
        }
        let statuses: Vec<_> = std::iter::from_fn(|| ws_rx.try_recv().ok())
            .filter_map(|event| {
                if let ServerEvent::ToolEnd { status, .. } = event {
                    Some(status)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            statuses,
            match scenario {
                "child" => vec!["success", "success"],
                "lookup" => vec!["error", "error"],
                _ => vec!["error", "success"],
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nested_file_edit_refuses_trimmed_read_only_path() {
        nested_attachment_regression("trimmed").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nested_file_mutations_allow_active_writable_child() {
        nested_attachment_regression("child").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nested_file_mutations_refuse_attachment_lookup_failure() {
        nested_attachment_regression("lookup").await;
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
        let record = storage
            .create_tool_call(swage_storage::model::NewToolCall {
                execution: None,
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
                record.id,
                "nested-secret",
                "secret_tool",
                &json!({"token": "sk-abcdefghijklmnopqrstuvwxyz123456"}),
                "success",
                1,
                swage_protocol::client::conversation::ToolExecution::succeeded(),
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
