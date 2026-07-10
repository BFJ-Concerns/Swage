mod bench;
mod conversation;
mod team;
mod topology_run;
mod workspace_attachment;

pub use bench::*;
pub use conversation::*;
pub use team::*;
pub use topology_run::*;
pub use workspace_attachment::*;

#[cfg(test)]
use serde_json::Value as JsonValue;
#[cfg(test)]
use swage_core::{GatewayError, Role, StorageError};
#[cfg(test)]
use swage_storage::model::NewSession;
#[cfg(test)]
use swage_storage::store::{BenchStore, Storage};
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use swage_core::{compact_tool_arguments_for_model, compact_tool_result_for_model};
    use swage_storage::model::{NewMessage, NewToolCall, NewTurn, TurnState};
    use swage_storage::sqlite::SqliteStorage;

    use super::*;
    use crate::turn::tests::MockStorage;

    fn make_service() -> ConversationService {
        let storage = Arc::new(MockStorage::new());
        ConversationService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn BenchStore>,
        )
    }

    fn model_arguments(raw: &str) -> String {
        let value = serde_json::from_str(raw).unwrap_or_else(|_| JsonValue::String(raw.into()));
        compact_tool_arguments_for_model(&value)
    }

    fn model_result(raw: &str) -> String {
        compact_tool_result_for_model(raw, false)
    }

    async fn make_workspace_service() -> (WorkspaceAttachmentService, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite should initialise"));
        let bench = storage
            .get_or_create_default()
            .await
            .expect("default bench should exist");
        let service = WorkspaceAttachmentService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn BenchStore>,
        );
        (service, bench.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_persists_canonical_path() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        std::fs::create_dir(&workspace).expect("workspace dir");
        let non_canonical = workspace.join("..").join("actual");

        let response = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: non_canonical.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");

        assert_eq!(
            response.path,
            workspace
                .canonicalize()
                .expect("canonical workspace")
                .to_string_lossy()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_workspaces_persists_missing_folder_state() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let attached = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");
        assert!(!attached.is_missing);

        std::fs::remove_dir(&workspace).expect("workspace dir should be removable");

        let list = service
            .list_workspaces(bench_id)
            .await
            .expect("workspaces should list");
        assert_eq!(list.workspaces.len(), 1);
        assert!(list.workspaces[0].is_missing);

        let stored = service
            .storage
            .get_workspace_attachment(attached.id)
            .await
            .expect("workspace lookup")
            .expect("workspace should still exist");
        assert!(stored.is_missing);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_can_become_bench_default() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("default");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let response = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: true,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");

        assert!(response.is_default);
        let stored = service
            .storage
            .get_workspace_attachment(response.id)
            .await
            .expect("workspace lookup")
            .expect("workspace should exist");
        assert!(stored.is_default);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_can_become_session_active_workspace() {
        let (service, bench_id) = make_workspace_service().await;
        let session = service
            .storage
            .create_session(NewSession {
                bench_id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session should create");
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("session");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let response = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: Some(session.id),
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("workspace should attach");

        let session = service
            .storage
            .get_session(session.id)
            .await
            .expect("session lookup")
            .expect("session should exist");
        assert_eq!(session.active_workspace_attachment_id, Some(response.id));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_rejects_duplicate_canonical_path() {
        let (service, bench_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        let symlink = temp.path().join("alias");
        std::fs::create_dir(&workspace).expect("workspace dir");
        std::os::unix::fs::symlink(&workspace, &symlink).expect("workspace symlink");

        service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: workspace.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect("first attachment should succeed");

        let error = service
            .attach_workspace(
                bench_id,
                CreateWorkspaceAttachmentRequest {
                    path: symlink.to_string_lossy().into_owned(),
                    label: None,
                    set_as_default: false,
                    active_session_id: None,
                    permissions: vec![],
                    is_trusted: false,
                },
            )
            .await
            .expect_err("canonical duplicate should be rejected");

        assert!(matches!(
            error,
            GatewayError::Storage(StorageError::ConstraintViolation(_))
        ));
    }

    // -----------------------------------------------------------------------
    // create_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_conversation_returns_valid_response() {
        let svc = make_service();

        let resp = svc
            .create_conversation()
            .await
            .expect("create should succeed");

        // Non-nil UUID expected.
        assert_ne!(resp.id, Uuid::nil());
        assert_ne!(resp.bench_id, Uuid::nil());
    }

    #[tokio::test]
    async fn create_conversation_multiple_times_yields_distinct_ids() {
        let svc = make_service();

        let a = svc.create_conversation().await.unwrap();
        let b = svc.create_conversation().await.unwrap();

        // Both conversations should belong to the same default bench.
        assert_eq!(a.bench_id, b.bench_id);
        // But have distinct session IDs.
        assert_ne!(a.id, b.id);
    }

    // -----------------------------------------------------------------------
    // list_conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_conversations_includes_all_created_sessions() {
        let svc = make_service();
        svc.create_conversation().await.unwrap();
        svc.create_conversation().await.unwrap();

        let list = svc.list_conversations().await.expect("list should succeed");

        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn list_conversations_shows_message_preview_and_interrupted_flag() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        // Get the default thread created alongside the session.
        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        // Insert a turn and a message via the underlying storage.
        let turn = svc
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::User,
                content: "Hello!".to_string(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        // Mark the turn as Interrupted so `has_interrupted_turn` is set.
        svc.storage
            .update_turn_state(turn.id, TurnState::Interrupted)
            .await
            .unwrap();

        let list = svc.list_conversations().await.unwrap();
        let summary = list.iter().find(|s| s.id == session_id).unwrap();

        assert_eq!(summary.message_preview.as_deref(), Some("Hello!"));
        assert!(summary.has_interrupted_turn);
    }

    #[tokio::test]
    async fn list_conversations_no_interrupted_turn_flag_when_completed() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let list = svc.list_conversations().await.unwrap();
        let summary = list.iter().find(|s| s.id == session_id).unwrap();

        assert!(!summary.has_interrupted_turn);
    }

    // -----------------------------------------------------------------------
    // get_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversation_returns_messages_with_turn_states() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::User,
                content: "Hi".to_string(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();
        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "Hello!".to_string(),
                position: 1,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();

        assert_eq!(detail.messages.len(), 2);
        assert_eq!(detail.messages[0].role, "user");
        assert_eq!(detail.messages[0].content, "Hi");
        assert_eq!(detail.messages[0].turn_state, Some(TurnState::Completed));
        assert_eq!(detail.messages[1].role, "assistant");
        assert_eq!(detail.messages[1].content, "Hello!");
    }

    #[tokio::test]
    async fn get_conversation_returns_not_found_for_missing_id() {
        let svc = make_service();
        let missing = Uuid::new_v4();

        let err = svc.get_conversation(missing).await.unwrap_err();

        assert!(
            matches!(err, GatewayError::ConversationNotFound { id } if id == missing),
            "expected ConversationNotFound, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // delete_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_conversation_then_get_returns_not_found() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();

        svc.delete_conversation(resp.id)
            .await
            .expect("delete should succeed");

        let err = svc.get_conversation(resp.id).await.unwrap_err();
        assert!(matches!(err, GatewayError::ConversationNotFound { .. }));
    }

    #[tokio::test]
    async fn delete_non_existent_conversation_returns_not_found() {
        let svc = make_service();
        let missing = Uuid::new_v4();

        let err = svc.delete_conversation(missing).await.unwrap_err();
        assert!(matches!(err, GatewayError::ConversationNotFound { .. }));
    }

    // -----------------------------------------------------------------------
    // tool_calls in get_conversation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversation_attaches_tool_calls_to_assistant_messages() {
        use swage_storage::model::NewToolCall;

        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::User,
                content: "list files".into(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "I'll run ls for you.".into(),
                position: 1,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                call_id: "toolu_01".into(),
                tool: "shell".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
                model_arguments: model_arguments(r#"{"cmd":"ls"}"#),
                result: Some("file1\nfile2".into()),
                model_result: Some(model_result("file1\nfile2")),
                status: Some("success".into()),
                duration_ms: Some(10),
                truncated: false,
                total_bytes: Some(12),
                provider_round: 1,
                position: 0,
                parent_call_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();
        assert_eq!(detail.messages.len(), 2);

        // User message should have no tool calls.
        let user_msg = &detail.messages[0];
        assert_eq!(user_msg.role, "user");
        assert!(
            user_msg.tool_calls.is_empty(),
            "user message should have no tool calls"
        );

        // Assistant message should carry the tool call.
        let asst_msg = &detail.messages[1];
        assert_eq!(asst_msg.role, "assistant");
        assert_eq!(asst_msg.tool_calls.len(), 1);
        let tc = &asst_msg.tool_calls[0];
        assert_eq!(tc.call_id, "toolu_01");
        assert_eq!(tc.tool, "shell");
        assert_eq!(tc.status.as_deref(), Some("success"));
        assert!(!tc.truncated);
    }

    #[tokio::test]
    async fn get_conversation_tool_call_view_carries_parent_call_id() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "Ran python".into(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();

        // Native python_execute call.
        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                call_id: "py_01".into(),
                tool: "python_execute".into(),
                arguments: r#"{"code":"1+1"}"#.into(),
                model_arguments: model_arguments(r#"{"code":"1+1"}"#),
                result: None,
                model_result: None,
                status: None,
                duration_ms: None,
                truncated: false,
                total_bytes: None,
                provider_round: 1,
                position: 0,
                parent_call_id: None,
            })
            .await
            .unwrap();

        // Sub-tool dispatched from within python_execute.
        svc.storage
            .create_tool_call(NewToolCall {
                turn_id: turn.id,
                agent_id: "gateway".to_string(),
                call_id: "sub_01".into(),
                tool: "shell".into(),
                arguments: r#"{"cmd":"echo hi"}"#.into(),
                model_arguments: model_arguments(r#"{"cmd":"echo hi"}"#),
                result: Some("hi".into()),
                model_result: Some(model_result("hi")),
                status: Some("success".into()),
                duration_ms: Some(5),
                truncated: false,
                total_bytes: Some(2),
                provider_round: 0,
                position: 1,
                parent_call_id: Some("py_01".into()),
            })
            .await
            .unwrap();

        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();
        let tool_calls = &detail.messages[0].tool_calls;
        assert_eq!(tool_calls.len(), 2);

        let native = &tool_calls[0];
        assert_eq!(native.call_id, "py_01");
        assert!(
            native.parent_call_id.is_none(),
            "native call should have no parent"
        );

        let sub = &tool_calls[1];
        assert_eq!(sub.call_id, "sub_01");
        assert_eq!(sub.parent_call_id.as_deref(), Some("py_01"));
    }

    #[tokio::test]
    async fn get_conversation_tool_calls_empty_when_none_recorded() {
        let svc = make_service();
        let resp = svc.create_conversation().await.unwrap();
        let session_id = resp.id;

        let threads = svc.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let turn = svc
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        svc.storage
            .create_message(NewMessage {
                thread_id,
                turn_id: Some(turn.id),
                role: Role::Assistant,
                content: "Hello".into(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .unwrap();
        svc.storage
            .update_turn_state(turn.id, TurnState::Completed)
            .await
            .unwrap();

        let detail = svc.get_conversation(session_id).await.unwrap();
        assert_eq!(detail.messages[0].tool_calls.len(), 0);
    }
}
