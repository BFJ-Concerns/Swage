mod agent_definition;
mod conversation;
mod session_variable;
mod space;
mod task_definition;
mod workspace_attachment;

pub use agent_definition::*;
pub use conversation::*;
pub use session_variable::*;
pub use space::*;
pub use task_definition::*;
pub use workspace_attachment::*;

#[cfg(test)]
use serde_json::Value as JsonValue;
#[cfg(test)]
use swage_core::{GatewayError, Role, StorageError};
#[cfg(test)]
use swage_storage::model::{NewAgentDefinition, NewSession, NewSpace, SpaceSlug};
#[cfg(test)]
use swage_storage::store::{AgentDefinitionStore, SessionStore, SpaceStore, Storage};
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
            storage as Arc<dyn SpaceStore>,
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
        let space = storage
            .get_or_create_default()
            .await
            .expect("default space should exist");
        let service = WorkspaceAttachmentService::new_for_tests(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn SpaceStore>,
        );
        (service, space.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_workspace_persists_canonical_path() {
        let (service, space_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        std::fs::create_dir(&workspace).expect("workspace dir");
        let non_canonical = workspace.join("..").join("actual");

        let response = service
            .attach_workspace(
                space_id,
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
        let (service, space_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let attached = service
            .attach_workspace(
                space_id,
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
            .list_workspaces(space_id)
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
    async fn attach_workspace_can_become_space_default() {
        let (service, space_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("default");
        std::fs::create_dir(&workspace).expect("workspace dir");

        let response = service
            .attach_workspace(
                space_id,
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
        let (service, space_id) = make_workspace_service().await;
        let session = service
            .storage
            .create_session(NewSession {
                space_id,
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
                space_id,
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
        let (service, space_id) = make_workspace_service().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("actual");
        let symlink = temp.path().join("alias");
        std::fs::create_dir(&workspace).expect("workspace dir");
        std::os::unix::fs::symlink(&workspace, &symlink).expect("workspace symlink");

        service
            .attach_workspace(
                space_id,
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
                space_id,
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
        assert_ne!(resp.space_id, Uuid::nil());
    }

    #[tokio::test]
    async fn create_conversation_multiple_times_yields_distinct_ids() {
        let svc = make_service();

        let a = svc.create_conversation().await.unwrap();
        let b = svc.create_conversation().await.unwrap();

        // Both conversations should belong to the same default space.
        assert_eq!(a.space_id, b.space_id);
        // But have distinct session IDs.
        assert_ne!(a.id, b.id);
    }

    // -----------------------------------------------------------------------
    // agent selection at session creation
    // -----------------------------------------------------------------------

    /// Sqlite-backed service so agent-definition resolution is real rather
    /// than a mock stub returning `None`.
    fn make_definition_service() -> (ConversationService, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite should initialise"));
        let service = ConversationService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
        );
        (service, storage)
    }

    fn definition(id: &str, space_id: Option<Uuid>) -> NewAgentDefinition {
        NewAgentDefinition {
            id: id.to_string(),
            space_id,
            display_name: id.to_string(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: None,
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_conversation_defaults_to_the_gateway_agent() {
        let (svc, storage) = make_definition_service();

        let resp = svc
            .create_conversation_from_request(CreateConversationRequest::default())
            .await
            .unwrap();

        let session = storage.get_session(resp.id).await.unwrap().unwrap();
        assert_eq!(session.agent_id, "gateway");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_effort_override_wins_and_is_persisted() {
        let config_root = tempfile::tempdir().expect("config root");
        let (svc, storage) = make_definition_service();
        let mut config = swage_config::ResolvedConfig::default();
        config.models.default = "model-a".to_string();
        config.models.entries.insert(
            "model-a".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::Low),
                ..Default::default()
            },
        );

        let response = svc
            .with_config(config, config_root.path().to_path_buf())
            .create_conversation_from_request(CreateConversationRequest {
                selected_model_id: Some("model-a".to_string()),
                agent_id: None,
                effort: Some(swage_protocol::client::settings::Effort::High),
            })
            .await
            .unwrap();

        let session = storage.get_session(response.id).await.unwrap().unwrap();
        assert_eq!(session.metadata.unwrap()["effort"], "high");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_inherits_and_persists_model_default_effort() {
        let config_root = tempfile::tempdir().expect("config root");
        let (svc, storage) = make_definition_service();
        let mut config = swage_config::ResolvedConfig::default();
        config.models.default = "model-a".to_string();
        config.models.entries.insert(
            "model-a".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::Medium),
                ..Default::default()
            },
        );

        let response = svc
            .with_config(config, config_root.path().to_path_buf())
            .create_conversation_from_request(CreateConversationRequest::default())
            .await
            .unwrap();

        let session = storage.get_session(response.id).await.unwrap().unwrap();
        assert_eq!(session.metadata.unwrap()["effort"], "medium");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_space_effort_uses_retained_config_root() {
        let retained_root = tempfile::tempdir().expect("retained config root");
        let space_dir = retained_root.path().join("spaces/default");
        std::fs::create_dir_all(&space_dir).expect("create space config directory");
        std::fs::write(
            retained_root.path().join("config.toml"),
            r#"
[providers]
default = "test"

[providers.entries.test]
protocol = "openai"

[models]
default = "model-a"

[models.entries.model-a]
provider_id = "test"
model_id = "global-model"
default_effort = "low"
"#,
        )
        .expect("write global config");
        std::fs::write(
            space_dir.join("config.toml"),
            r#"
[models.entries.model-a]
provider_id = "test"
model_id = "space-model"
default_effort = "high"
"#,
        )
        .expect("write space config");

        let (svc, storage) = make_definition_service();
        let response = svc
            .with_config(
                swage_config::ResolvedConfig::default(),
                retained_root.path().to_path_buf(),
            )
            .create_conversation_from_request(CreateConversationRequest::default())
            .await
            .unwrap();

        let session = storage.get_session(response.id).await.unwrap().unwrap();
        assert_eq!(session.metadata.unwrap()["effort"], "high");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_inherits_selected_model_default_effort() {
        let config_root = tempfile::tempdir().expect("config root");
        let (svc, storage) = make_definition_service();
        let mut config = swage_config::ResolvedConfig::default();
        config.models.default = "model-a".to_string();
        config.models.entries.insert(
            "model-a".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::Low),
                ..Default::default()
            },
        );
        config.models.entries.insert(
            "model-b".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::High),
                ..Default::default()
            },
        );

        let response = svc
            .with_config(config, config_root.path().to_path_buf())
            .create_conversation_from_request(CreateConversationRequest {
                selected_model_id: Some("model-b".to_string()),
                agent_id: None,
                effort: None,
            })
            .await
            .unwrap();

        let session = storage.get_session(response.id).await.unwrap().unwrap();
        assert_eq!(session.metadata.unwrap()["effort"], "high");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_inherits_aliased_model_default_effort() {
        let config_root = tempfile::tempdir().expect("config root");
        let (svc, storage) = make_definition_service();
        let mut config = swage_config::ResolvedConfig::default();
        config.models.default = "model-default".to_string();
        config.models.entries.insert(
            "model-default".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::Low),
                ..Default::default()
            },
        );
        config.models.entries.insert(
            "model-a".to_string(),
            swage_config::ModelEndpointConfig {
                aliases: vec!["friendly-a".to_string()],
                default_effort: Some(swage_protocol::client::settings::Effort::Medium),
                ..Default::default()
            },
        );

        let response = svc
            .with_config(config, config_root.path().to_path_buf())
            .create_conversation_with_model(Some("friendly-a".to_string()))
            .await
            .unwrap();

        let session = storage.get_session(response.id).await.unwrap().unwrap();
        assert_eq!(session.metadata.unwrap()["effort"], "medium");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_with_unknown_model_omits_effort() {
        let config_root = tempfile::tempdir().expect("config root");
        let (svc, storage) = make_definition_service();
        let mut config = swage_config::ResolvedConfig::default();
        config.models.default = "model-a".to_string();
        config.models.entries.insert(
            "model-a".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::Low),
                ..Default::default()
            },
        );

        let response = svc
            .with_config(config, config_root.path().to_path_buf())
            .create_conversation_with_model(Some("unknown-model".to_string()))
            .await
            .unwrap();

        let session = storage.get_session(response.id).await.unwrap().unwrap();
        assert!(session.metadata.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_inherits_agent_models_default_effort() {
        let config_root = tempfile::tempdir().expect("config root");
        let (svc, storage) = make_definition_service();
        let mut agent = definition("researcher", None);
        agent.model_id = Some("model-b".to_string());
        storage.upsert_agent_definition(agent).await.unwrap();

        let mut config = swage_config::ResolvedConfig::default();
        config.models.default = "model-a".to_string();
        config.models.entries.insert(
            "model-a".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::Low),
                ..Default::default()
            },
        );
        config.models.entries.insert(
            "model-b".to_string(),
            swage_config::ModelEndpointConfig {
                default_effort: Some(swage_protocol::client::settings::Effort::High),
                ..Default::default()
            },
        );

        let response = svc
            .with_config(config, config_root.path().to_path_buf())
            .create_conversation_from_request(CreateConversationRequest {
                selected_model_id: None,
                agent_id: Some("researcher".to_string()),
                effort: None,
            })
            .await
            .unwrap();

        let session = storage.get_session(response.id).await.unwrap().unwrap();
        assert_eq!(session.metadata.unwrap()["effort"], "high");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_conversation_for_named_agent_runs_under_that_definition() {
        let (svc, storage) = make_definition_service();
        storage
            .upsert_agent_definition(definition("researcher", None))
            .await
            .unwrap();

        let resp = svc
            .create_conversation_from_request(CreateConversationRequest {
                selected_model_id: None,
                agent_id: Some("researcher".to_string()),
                effort: None,
            })
            .await
            .unwrap();

        let session = storage.get_session(resp.id).await.unwrap().unwrap();
        assert_eq!(session.agent_id, "researcher");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_space_conversation_accepts_a_shipped_research_agent() {
        let (svc, storage) = make_definition_service();

        let resp = svc
            .create_conversation_from_request(CreateConversationRequest {
                selected_model_id: None,
                agent_id: Some("synthesist".to_string()),
                effort: None,
            })
            .await
            .expect("shipped definition should be selectable without an authored row");

        let session = storage.get_session(resp.id).await.unwrap().unwrap();
        assert_eq!(session.agent_id, "synthesist");
        assert!(
            storage
                .list_agent_definitions(None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_conversation_rejects_an_unknown_agent_selector() {
        let (svc, _storage) = make_definition_service();

        let error = svc
            .create_conversation_from_request(CreateConversationRequest {
                selected_model_id: None,
                agent_id: Some("nonexistent".to_string()),
                effort: None,
            })
            .await
            .expect_err("an unbacked selector should be rejected");

        assert!(matches!(error, GatewayError::ValidationError(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_space_session_accepts_a_definition_scoped_to_that_space() {
        let (svc, storage) = make_definition_service();
        let space = storage.get_or_create_default().await.unwrap();
        storage
            .upsert_agent_definition(definition("triage", Some(space.id)))
            .await
            .unwrap();

        let resp = svc
            .create_session_in_space_from_request(
                space.id,
                CreateConversationRequest {
                    selected_model_id: None,
                    agent_id: Some("triage".to_string()),
                    effort: None,
                },
            )
            .await
            .unwrap();

        let session = storage.get_session(resp.id).await.unwrap().unwrap();
        assert_eq!(session.agent_id, "triage");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_space_session_rejects_a_definition_scoped_to_another_space() {
        let (svc, storage) = make_definition_service();
        let space = storage.get_or_create_default().await.unwrap();
        // Scoped to a space that is not the target: invisible to the
        // selector, so validation must reject it rather than fall through
        // to the default agent.
        let other = storage
            .create(NewSpace {
                name: "Other".to_string(),
                slug: SpaceSlug::new("other").unwrap(),
            })
            .await
            .unwrap();
        storage
            .upsert_agent_definition(definition("triage", Some(other.id)))
            .await
            .unwrap();

        let error = svc
            .create_session_in_space_from_request(
                space.id,
                CreateConversationRequest {
                    selected_model_id: None,
                    agent_id: Some("triage".to_string()),
                    effort: None,
                },
            )
            .await
            .expect_err("a definition invisible to the space should be rejected");

        assert!(matches!(error, GatewayError::ValidationError(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_space_session_reports_a_missing_space_before_the_selector() {
        let (svc, _storage) = make_definition_service();

        let error = svc
            .create_session_in_space_from_request(
                Uuid::new_v4(),
                CreateConversationRequest {
                    selected_model_id: None,
                    agent_id: Some("nonexistent".to_string()),
                    effort: None,
                },
            )
            .await
            .expect_err("a missing space should be reported");

        assert!(matches!(error, GatewayError::SpaceNotFound { .. }));
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
        assert_eq!(
            detail.messages[0].turn_state,
            Some(swage_protocol::client::conversation::TurnState::Completed)
        );
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
                display_summary: None,
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
                display_summary: None,
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
                display_summary: None,
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
