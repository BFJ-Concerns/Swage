use std::sync::Arc;

use swage_provider::LlmProvider;
use swage_storage::model::Session;

use super::*;
use crate::turn::tests::{MockProvider, MockStorage};

// -----------------------------------------------------------------------
// Event serialisation / deserialisation round-trips
// -----------------------------------------------------------------------

#[test]
fn client_event_send_message_deserialises() {
    let json = r#"{"type": "send_message", "content": "hello"}"#;
    let event: ClientEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(event, ClientEvent::SendMessage { content, .. } if content == "hello"));
}

#[test]
fn client_event_cancel_deserialises() {
    let json = r#"{"type": "cancel"}"#;
    let event: ClientEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(event, ClientEvent::Cancel {}));
}

#[test]
fn server_event_token_serialises() {
    let event = ServerEvent::Token {
        content: "hi".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "token");
    assert_eq!(json["content"], "hi");
}

#[test]
fn server_event_turn_complete_serialises() {
    let id = Uuid::new_v4();
    let event = ServerEvent::TurnComplete {
        turn_id: id,
        input_tokens: None,
        output_tokens: None,
        cost_microcents: None,
        structured_output: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "turn_complete");
    assert_eq!(json["turn_id"], id.to_string());
    // Optional fields should be absent when None.
    assert!(json.get("input_tokens").is_none());
    assert!(json.get("output_tokens").is_none());
    assert!(json.get("cost_microcents").is_none());
    assert!(json.get("structured_output").is_none());
}

#[test]
fn server_event_turn_complete_with_usage_serialises() {
    let id = Uuid::new_v4();
    let event = ServerEvent::TurnComplete {
        turn_id: id,
        input_tokens: Some(100),
        output_tokens: Some(50),
        cost_microcents: Some(1500),
        structured_output: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "turn_complete");
    assert_eq!(json["turn_id"], id.to_string());
    assert_eq!(json["input_tokens"], 100);
    assert_eq!(json["output_tokens"], 50);
    assert_eq!(json["cost_microcents"], 1500);
}

#[test]
fn server_event_error_serialises() {
    let event = ServerEvent::Error {
        code: "provider_error".to_string(),
        message: "overloaded".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "error");
    assert_eq!(json["code"], "provider_error");
    assert_eq!(json["message"], "overloaded");
}

#[test]
fn server_event_cancelled_serialises() {
    let event = ServerEvent::Cancelled {};
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "cancelled");
}

#[test]
fn server_event_tool_start_serialises() {
    let event = ServerEvent::ToolStart {
        call_id: "toolu_01abc".to_string(),
        tool: "shell".to_string(),
        arguments: serde_json::json!({"command": "ls -la"}),
        parent_call_id: None,
        summary: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "tool_start");
    assert_eq!(json["call_id"], "toolu_01abc");
    assert_eq!(json["tool"], "shell");
    assert_eq!(json["arguments"]["command"], "ls -la");
}

#[test]
fn server_event_tool_start_omits_parent_call_id_when_none() {
    let event = ServerEvent::ToolStart {
        call_id: "toolu_native".to_string(),
        tool: "shell".to_string(),
        arguments: serde_json::json!({}),
        parent_call_id: None,
        summary: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    // parent_call_id must be absent entirely, not serialised as null,
    // so existing clients that don't handle nesting remain unaffected.
    assert!(
        !json.as_object().unwrap().contains_key("parent_call_id"),
        "parent_call_id should be absent when None, got: {json}"
    );
}

#[test]
fn server_event_tool_start_includes_parent_call_id_when_some() {
    let event = ServerEvent::ToolStart {
        call_id: "sub_01".to_string(),
        tool: "shell".to_string(),
        arguments: serde_json::json!({}),
        parent_call_id: Some("py_call_01".to_string()),
        summary: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["parent_call_id"], "py_call_01");
}

#[test]
fn server_event_tool_end_serialises() {
    let event = ServerEvent::ToolEnd {
        call_id: "toolu_01abc".to_string(),
        tool: "shell".to_string(),
        result: serde_json::json!("file1\nfile2\nfile3"),
        status: "success".to_string(),
        duration_ms: 142,
        truncated: false,
        total_bytes: Some(25),
        parent_call_id: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "tool_end");
    assert_eq!(json["call_id"], "toolu_01abc");
    assert_eq!(json["tool"], "shell");
    assert_eq!(json["result"], "file1\nfile2\nfile3");
    assert_eq!(json["status"], "success");
    assert_eq!(json["duration_ms"], 142);
    assert_eq!(json["truncated"], false);
    assert_eq!(json["total_bytes"], 25);
}

#[test]
fn server_event_tool_end_omits_parent_call_id_when_none() {
    let event = ServerEvent::ToolEnd {
        call_id: "toolu_native".to_string(),
        tool: "shell".to_string(),
        result: serde_json::json!("ok"),
        status: "success".to_string(),
        duration_ms: 10,
        truncated: false,
        total_bytes: None,
        parent_call_id: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert!(
        !json.as_object().unwrap().contains_key("parent_call_id"),
        "parent_call_id should be absent when None"
    );
}

#[test]
fn server_event_tool_end_includes_parent_call_id_when_some() {
    let event = ServerEvent::ToolEnd {
        call_id: "sub_01".to_string(),
        tool: "shell".to_string(),
        result: serde_json::json!("output"),
        status: "success".to_string(),
        duration_ms: 5,
        truncated: false,
        total_bytes: None,
        parent_call_id: Some("py_call_01".to_string()),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["parent_call_id"], "py_call_01");
}

#[test]
fn server_event_tool_end_serialises_without_total_bytes() {
    let event = ServerEvent::ToolEnd {
        call_id: "toolu_02".to_string(),
        tool: "file_read".to_string(),
        result: serde_json::json!({"content": "hello"}),
        status: "success".to_string(),
        duration_ms: 5,
        truncated: false,
        total_bytes: None,
        parent_call_id: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "tool_end");
    assert!(json["total_bytes"].is_null());
    // Structured result (object) serialises correctly.
    assert_eq!(json["result"]["content"], "hello");
}

#[test]
fn server_event_tool_end_truncated() {
    let event = ServerEvent::ToolEnd {
        call_id: "toolu_03".to_string(),
        tool: "shell".to_string(),
        result: serde_json::json!("partial output..."),
        status: "success".to_string(),
        duration_ms: 30_000,
        truncated: true,
        total_bytes: Some(1_048_576),
        parent_call_id: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert!(json["truncated"].as_bool().unwrap());
    assert_eq!(json["total_bytes"], 1_048_576);
}

// -----------------------------------------------------------------------
// Observability event serialisation / deserialisation (ogpt)
// -----------------------------------------------------------------------

#[test]
fn client_event_subscribe_deserialises() {
    let json = r#"{"type": "subscribe", "channels": ["context", "tools"]}"#;
    let event: ClientEvent = serde_json::from_str(json).unwrap();
    match event {
        ClientEvent::Subscribe { channels } => {
            assert_eq!(channels, vec!["context", "tools"]);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn client_event_subscribe_empty_channels_deserialises() {
    let json = r#"{"type": "subscribe", "channels": []}"#;
    let event: ClientEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(event, ClientEvent::Subscribe { channels } if channels.is_empty()));
}

#[test]
fn client_event_unsubscribe_deserialises() {
    let json = r#"{"type": "unsubscribe", "channels": ["tools"]}"#;
    let event: ClientEvent = serde_json::from_str(json).unwrap();
    match event {
        ClientEvent::Unsubscribe { channels } => {
            assert_eq!(channels, vec!["tools"]);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn server_event_context_assembled_serialises() {
    let turn_id = Uuid::new_v4();
    let report = swage_protocol::client::context::AssemblyReport {
        total_tokens: 512,
        ..Default::default()
    };
    let event = ServerEvent::ContextAssembled { turn_id, report };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "context_assembled");
    assert_eq!(json["turn_id"], turn_id.to_string());
    assert_eq!(json["report"]["assembly_name"], "default");
    assert_eq!(json["report"]["total_tokens"], 512);
}

#[test]
fn server_event_context_assembled_round_trips() {
    let turn_id = Uuid::new_v4();
    let report = swage_protocol::client::context::AssemblyReport {
        assembly_name: "custom".into(),
        errors: vec![swage_protocol::client::context::AssemblyError {
            assembly_name: "custom".into(),
            error_kind: "timeout".into(),
            message: "timed out".into(),
            fallback_used: "default".into(),
        }],
        ..Default::default()
    };
    let event = ServerEvent::ContextAssembled { turn_id, report };
    let serialised = serde_json::to_string(&event).unwrap();
    // Verify type tag is present in the raw JSON string.
    assert!(
        serialised.contains(r#""type":"context_assembled""#),
        "expected context_assembled type tag, got: {serialised}",
    );
}

#[test]
fn server_event_shutdown_serialises() {
    let event = ServerEvent::Shutdown {
        reason: "idle timeout".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "shutdown");
    assert_eq!(json["reason"], "idle timeout");
}

#[test]
fn server_event_shutdown_round_trips() {
    let event = ServerEvent::Shutdown {
        reason: "admin request".to_string(),
    };
    let serialised = serde_json::to_string(&event).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&serialised).unwrap();
    assert_eq!(parsed["type"], "shutdown");
    assert_eq!(parsed["reason"], "admin request");
}

// -----------------------------------------------------------------------
// Skill event serialisation / deserialisation [e65s]
// -----------------------------------------------------------------------

#[test]
fn client_event_activate_skill_deserialises() {
    let json = r#"{"type": "activate_skill", "name": "code-review"}"#;
    let event: ClientEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(event, ClientEvent::ActivateSkill { name } if name == "code-review"));
}

#[test]
fn client_event_deactivate_skill_deserialises() {
    let json = r#"{"type": "deactivate_skill", "name": "code-review"}"#;
    let event: ClientEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(event, ClientEvent::DeactivateSkill { name } if name == "code-review"));
}

#[test]
fn server_event_skill_activated_serialises() {
    let event = ServerEvent::SkillActivated {
        name: "code-review".to_string(),
        display_name: "Code Review".to_string(),
        description: "Reviews code for quality issues".to_string(),
        token_count: 512,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "skill_activated");
    assert_eq!(json["name"], "code-review");
    assert_eq!(json["display_name"], "Code Review");
    assert_eq!(json["description"], "Reviews code for quality issues");
    assert_eq!(json["token_count"], 512);
}

#[test]
fn server_event_skill_already_active_serialises() {
    let event = ServerEvent::SkillAlreadyActive {
        name: "code-review".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "skill_already_active");
    assert_eq!(json["name"], "code-review");
}

#[test]
fn server_event_skill_deactivated_serialises() {
    let event = ServerEvent::SkillDeactivated {
        name: "code-review".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "skill_deactivated");
    assert_eq!(json["name"], "code-review");
}

// -----------------------------------------------------------------------
// Skill dispatch tests [5cpd]
// -----------------------------------------------------------------------

#[tokio::test]
async fn activate_skill_with_missing_session_surfaces_discovery_error() {
    let state = make_empty_state();
    let session_id = Uuid::new_v4();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let json = r#"{"type":"activate_skill","name":"ghost-skill"}"#;
    dispatch_client_message(json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive an event");
    match event {
        ServerEvent::Error { code, message } => {
            assert_eq!(code, error_codes::INTERNAL_ERROR);
            assert!(message.contains("skill discovery failed"));
            assert!(message.contains("session"));
        }
        other => panic!("expected Error event, got: {other:?}"),
    }
}

#[tokio::test]
async fn slash_command_with_missing_session_surfaces_discovery_error() {
    let state = make_empty_state();
    let session_id = Uuid::new_v4();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let json = r#"{"type":"send_message","content":"/code-review"}"#;
    dispatch_client_message(json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive an event");
    match event {
        ServerEvent::Error { code, message } => {
            assert_eq!(code, error_codes::INTERNAL_ERROR);
            assert!(message.contains("skill discovery failed"));
            assert!(message.contains("session"));
        }
        other => panic!("expected Error event, got: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn failed_bare_skill_activation_emits_only_the_activation_error() {
    let (state, session_id, storage) = make_state_with_session_and_storage().await;
    let skill_root = tempfile::TempDir::new().expect("skill root");
    let skill_dir = skill_root.path().join("skills/code-review");
    std::fs::create_dir_all(&skill_dir).expect("create skill directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Review code\n---\n\nReview the code.",
    )
    .expect("write skill");
    let _skill_root = crate::skill::TestSkillRootGuard::at(session_id, skill_root.path());
    storage.fail_skill_activations();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    let keepalive = tx.clone();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    dispatch_client_message(
        r#"{"type":"send_message","content":"/code-review"}"#,
        &state,
        session_id,
        tx,
        sub,
    )
    .await;

    let event = rx.recv().await.expect("activation error");
    assert!(matches!(
        event,
        ServerEvent::Error { code, message }
            if code == error_codes::INTERNAL_ERROR && message == "skill activation failed"
    ));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "failed activation must not also start a turn and emit a second signal"
    );
    drop(keepalive);
}

/// ActivateSkill for an unknown skill name sends a skill_not_found error event.
///
/// The mock storage has no skill root on disk, so discover_available returns
/// an empty set, causing SkillManager::activate to return NotFound.
#[tokio::test(flavor = "current_thread")]
async fn activate_skill_event_for_unknown_skill_sends_not_found_error() {
    let (state, session_id) = make_state_with_session().await;
    let _skill_root = crate::skill::TestSkillRootGuard::empty(session_id);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let json = r#"{"type":"activate_skill","name":"ghost-skill"}"#;
    dispatch_client_message(json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive an event");
    match event {
        ServerEvent::Error { code, .. } => {
            assert_eq!(
                code, "skill_not_found",
                "expected skill_not_found error code"
            );
        }
        other => panic!("expected Error event, got: {other:?}"),
    }
}

/// DeactivateSkill for a skill not in available (and not in storage) sends SkillDeactivated.
///
/// Since the skill is not in the available set there's no agent-level check,
/// and storage.deactivate_skill is a no-op that returns Ok(false) → success.
#[tokio::test(flavor = "current_thread")]
async fn deactivate_skill_event_sends_skill_deactivated() {
    let (state, session_id) = make_state_with_session().await;
    let _skill_root = crate::skill::TestSkillRootGuard::empty(session_id);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let json = r#"{"type":"deactivate_skill","name":"code-review"}"#;
    dispatch_client_message(json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive an event");
    assert!(
        matches!(event, ServerEvent::SkillDeactivated { name } if name == "code-review"),
        "expected SkillDeactivated event",
    );
}

/// A plain (non-slash) SendMessage still dispatches normally — no skill events.
#[tokio::test]
async fn plain_send_message_does_not_emit_skill_event() {
    let (state, session_id) = make_state_with_session().await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let json = r#"{"type":"send_message","content":"hello world"}"#;
    dispatch_client_message(json, &state, session_id, tx, sub).await;

    // The turn spawned by MockProvider will eventually send an event, but
    // we're only verifying no *skill* event appears first.
    // Give the spawned turn a moment to produce events, then drain.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(
                event,
                ServerEvent::SkillActivated { .. } | ServerEvent::SkillAlreadyActive { .. }
            ),
            "unexpected skill event for plain message: {event:?}",
        );
    }
}

// -----------------------------------------------------------------------
// Precondition tests (tested directly to avoid WebSocket upgrade ceremony)
//
// axum's WebSocketUpgrade extractor validates the actual TCP connection
// in integration scenarios; for unit testing the precondition logic we
// call check_upgrade_preconditions directly instead.
// -----------------------------------------------------------------------

/// Build an AppState with no sessions.
fn make_empty_state() -> AppState {
    let storage = Arc::new(MockStorage::new());
    let storage_arc: Arc<dyn swage_storage::store::Storage> =
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>;
    let topic_store: Arc<dyn swage_storage::store::TopicStore> =
        storage as Arc<dyn swage_storage::store::TopicStore>;
    AppState::new(
        storage_arc,
        topic_store,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    )
}

/// Build an AppState with a pre-inserted session.
async fn make_state_with_session() -> (AppState, Uuid) {
    let (state, session_id, _) = make_state_with_session_and_storage().await;
    (state, session_id)
}

async fn make_state_with_session_and_storage() -> (AppState, Uuid, Arc<MockStorage>) {
    use swage_storage::store::TopicStore;

    let storage = Arc::new(MockStorage::new());
    let session_id = Uuid::new_v4();

    let topic = storage
        .get_or_create_default()
        .await
        .expect("create default topic");

    storage.db.lock().await.sessions.insert(
        session_id,
        Session {
            id: session_id,
            topic_id: topic.id,
            agent_id: "gateway".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: None,
            selected_model_id: None,
            active_workspace_attachment_id: None,
        },
    );

    let storage_arc: Arc<dyn swage_storage::store::Storage> =
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>;
    let topic_store: Arc<dyn swage_storage::store::TopicStore> =
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>;
    let state = AppState::new(
        storage_arc,
        topic_store,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    (state, session_id, storage)
}

#[tokio::test]
async fn preconditions_reject_non_existent_conversation() {
    let state = make_empty_state();
    let missing_id = Uuid::new_v4();

    let response = check_upgrade_preconditions(&state, missing_id)
        .await
        .expect("should return Some rejection");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn preconditions_reject_duplicate_connection() {
    let (state, session_id) = make_state_with_session().await;

    // Pre-mark this session as already connected.
    state.active_connections.lock().await.insert(session_id);

    let response = check_upgrade_preconditions(&state, session_id)
        .await
        .expect("should return Some rejection");

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn preconditions_pass_for_valid_unconnected_session() {
    let (state, session_id) = make_state_with_session().await;

    let result = check_upgrade_preconditions(&state, session_id).await;
    assert!(result.is_none(), "valid session should pass preconditions");
}

// -----------------------------------------------------------------------
// Schema validation events [z4ov]
// -----------------------------------------------------------------------

#[test]
fn schema_validation_error_serialises() {
    let err = SchemaValidationError {
        path: "/properties/name".to_string(),
        message: "required property missing".to_string(),
    };
    let json = serde_json::to_value(&err).unwrap();
    assert_eq!(json["path"], "/properties/name");
    assert_eq!(json["message"], "required property missing");
}

#[test]
fn turn_complete_with_structured_output_serialises() {
    let id = Uuid::new_v4();
    let event = ServerEvent::TurnComplete {
        turn_id: id,
        input_tokens: None,
        output_tokens: None,
        cost_microcents: None,
        structured_output: Some(serde_json::json!({"x": 1})),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "turn_complete");
    assert_eq!(json["structured_output"]["x"], 1);
}

#[test]
fn turn_complete_without_structured_output_omits_field() {
    let id = Uuid::new_v4();
    let event = ServerEvent::TurnComplete {
        turn_id: id,
        input_tokens: None,
        output_tokens: None,
        cost_microcents: None,
        structured_output: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert!(
        !json.as_object().unwrap().contains_key("structured_output"),
        "structured_output should be absent when None, got: {json}",
    );
}

#[test]
fn output_schema_retry_serialises() {
    let turn_id = Uuid::new_v4();
    let event = ServerEvent::OutputSchemaRetry {
        turn_id,
        attempt: 1,
        max_retries: 3,
        errors: vec![SchemaValidationError {
            path: "/name".to_string(),
            message: "required".to_string(),
        }],
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "output_schema_retry");
    assert_eq!(json["turn_id"], turn_id.to_string());
    assert_eq!(json["attempt"], 1);
    assert_eq!(json["max_retries"], 3);
    assert_eq!(json["errors"][0]["path"], "/name");
    assert_eq!(json["errors"][0]["message"], "required");
}

#[test]
fn provider_retry_started_serialises() {
    let turn_id = Uuid::new_v4();
    let event = ServerEvent::ProviderRetryStarted {
        turn_id,
        attempt: 2,
        max_retries: 3,
        provider: "anthropic".to_string(),
        wait_ms: 4_000,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "provider_retry_started");
    assert_eq!(json["turn_id"], turn_id.to_string());
    assert_eq!(json["attempt"], 2);
    assert_eq!(json["max_retries"], 3);
    assert_eq!(json["provider"], "anthropic");
    assert_eq!(json["wait_ms"], 4_000);
}

#[test]
fn provider_retry_settled_serialises() {
    let turn_id = Uuid::new_v4();
    let event = ServerEvent::ProviderRetrySettled {
        turn_id,
        attempt: 2,
        max_retries: 3,
        provider: "anthropic".to_string(),
        success: true,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "provider_retry_settled");
    assert_eq!(json["success"], true);
}

#[test]
fn output_schema_failed_serialises() {
    let turn_id = Uuid::new_v4();
    let event = ServerEvent::OutputSchemaFailed {
        turn_id,
        attempts: 3,
        errors: vec![SchemaValidationError {
            path: "/name".to_string(),
            message: "required".to_string(),
        }],
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "output_schema_failed");
    assert_eq!(json["turn_id"], turn_id.to_string());
    assert_eq!(json["attempts"], 3);
    assert_eq!(json["errors"][0]["path"], "/name");
    assert_eq!(json["errors"][0]["message"], "required");
}

#[test]
fn output_schema_retry_empty_errors_serialises() {
    // Retry with no specific validation errors (e.g. parse failure).
    let turn_id = Uuid::new_v4();
    let event = ServerEvent::OutputSchemaRetry {
        turn_id,
        attempt: 2,
        max_retries: 3,
        errors: vec![],
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "output_schema_retry");
    assert_eq!(json["errors"], serde_json::json!([]));
}

// -----------------------------------------------------------------------
// ApprovalDecision client event deserialisation [viuf]
// -----------------------------------------------------------------------

#[test]
fn client_event_approval_decision_approved_deserialises() {
    let expected_id = Uuid::new_v4();
    let json = format!(
        r#"{{"type": "approval_decision", "approval_id": "{expected_id}", "decision": "approved"}}"#
    );
    let event: ClientEvent = serde_json::from_str(&json).unwrap();
    match event {
        ClientEvent::ApprovalDecision {
            approval_id,
            decision,
            scope,
            reason,
        } => {
            assert_eq!(approval_id, expected_id);
            assert_eq!(decision, "approved");
            assert_eq!(scope, swage_agent::approval::ApprovalDecisionScope::Once);
            assert!(reason.is_none(), "reason should be None when absent");
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn client_event_approval_decision_rejected_with_reason_deserialises() {
    let expected_id = Uuid::new_v4();
    let json = format!(
        r#"{{"type": "approval_decision", "approval_id": "{expected_id}", "decision": "rejected", "reason": "too risky"}}"#
    );
    let event: ClientEvent = serde_json::from_str(&json).unwrap();
    match event {
        ClientEvent::ApprovalDecision {
            approval_id,
            decision,
            scope,
            reason,
        } => {
            assert_eq!(approval_id, expected_id);
            assert_eq!(decision, "rejected");
            assert_eq!(scope, swage_agent::approval::ApprovalDecisionScope::Once);
            assert_eq!(reason.as_deref(), Some("too risky"));
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Approval event serialisation [c79w]
// -----------------------------------------------------------------------

#[test]
fn server_event_approval_required_serialises() {
    let approval_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let event = ServerEvent::ApprovalRequired {
        approval_id,
        turn_id,
        agent_id: "gateway".to_string(),
        kind: "tool_call".to_string(),
        tool_name: Some("shell".to_string()),
        tool_arguments: Some(serde_json::json!({"command": "rm -rf /"})),
        prompt: "Allow shell execution?".to_string(),
        context: Some(serde_json::json!({"risk": "high"})),
        timeout_at: "2026-01-01T00:00:00Z".to_string(),
        timeout_policy: "reject".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "approval_required");
    assert_eq!(json["approval_id"], approval_id.to_string());
    assert_eq!(json["turn_id"], turn_id.to_string());
    assert_eq!(json["agent_id"], "gateway");
    assert_eq!(json["kind"], "tool_call");
    assert_eq!(json["tool_name"], "shell");
    assert_eq!(json["tool_arguments"]["command"], "rm -rf /");
    assert_eq!(json["prompt"], "Allow shell execution?");
    assert_eq!(json["context"]["risk"], "high");
    assert_eq!(json["timeout_at"], "2026-01-01T00:00:00Z");
    assert_eq!(json["timeout_policy"], "reject");
}

#[test]
fn server_event_approval_required_omits_optional_fields_when_none() {
    let event = ServerEvent::ApprovalRequired {
        approval_id: Uuid::new_v4(),
        turn_id: Uuid::new_v4(),
        agent_id: "gateway".to_string(),
        kind: "generic".to_string(),
        tool_name: None,
        tool_arguments: None,
        prompt: "Proceed?".to_string(),
        context: None,
        timeout_at: "2026-01-01T00:00:00Z".to_string(),
        timeout_policy: "interrupt".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "approval_required");
    let obj = json.as_object().unwrap();
    assert!(
        !obj.contains_key("tool_name"),
        "tool_name should be absent when None"
    );
    assert!(
        !obj.contains_key("tool_arguments"),
        "tool_arguments should be absent when None"
    );
    assert!(
        !obj.contains_key("context"),
        "context should be absent when None"
    );
}

#[test]
fn server_event_approval_confirmed_serialises() {
    let event = ServerEvent::ApprovalConfirmed {
        approval_id: Uuid::new_v4(),
        decision: "approved".to_string(),
        reason: Some("looks safe".to_string()),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "approval_confirmed");
    assert_eq!(json["decision"], "approved");
    assert_eq!(json["reason"], "looks safe");
}

#[test]
fn server_event_approval_confirmed_omits_reason_when_none() {
    let event = ServerEvent::ApprovalConfirmed {
        approval_id: Uuid::new_v4(),
        decision: "rejected".to_string(),
        reason: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "approval_confirmed");
    assert!(
        !json.as_object().unwrap().contains_key("reason"),
        "reason should be absent when None"
    );
}

#[test]
fn server_event_approval_expired_serialises() {
    let event = ServerEvent::ApprovalExpired {
        approval_id: Uuid::new_v4(),
        timeout_policy: "reject".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "approval_expired");
    assert_eq!(json["timeout_policy"], "reject");
}

#[test]
fn server_event_approval_decision_error_serialises() {
    let event = ServerEvent::ApprovalDecisionError {
        approval_id: Uuid::new_v4(),
        error: "already_decided".to_string(),
        message: "Approval was already resolved.".to_string(),
        current_state: Some("approved".to_string()),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "approval_decision_error");
    assert_eq!(json["error"], "already_decided");
    assert_eq!(json["message"], "Approval was already resolved.");
    assert_eq!(json["current_state"], "approved");
}

#[test]
fn server_event_approval_decision_error_omits_current_state_when_none() {
    let event = ServerEvent::ApprovalDecisionError {
        approval_id: Uuid::new_v4(),
        error: "not_found".to_string(),
        message: "Approval request not found.".to_string(),
        current_state: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "approval_decision_error");
    assert!(
        !json.as_object().unwrap().contains_key("current_state"),
        "current_state should be absent when None"
    );
}

// -----------------------------------------------------------------------
// dispatch_client_message — ApprovalDecision handler
// -----------------------------------------------------------------------

#[tokio::test]
async fn approval_decision_invalid_uuid_sends_error() {
    let state = make_empty_state();
    let session_id = Uuid::new_v4();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let json = r#"{"type":"approval_decision","approval_id":"not-a-uuid","decision":"approved"}"#;
    dispatch_client_message(json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive an error event");
    match event {
        ServerEvent::Error { code, message } => {
            assert_eq!(code, "internal_error");
            assert!(message.contains("invalid event"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn approval_decision_invalid_value_sends_error() {
    let state = make_empty_state();
    let session_id = Uuid::new_v4();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let id = Uuid::new_v4();
    let json = format!(r#"{{"type":"approval_decision","approval_id":"{id}","decision":"maybe"}}"#);
    dispatch_client_message(&json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive an error event");
    match event {
        ServerEvent::ApprovalDecisionError { error, .. } => {
            assert_eq!(error, "invalid_decision");
        }
        other => panic!("expected ApprovalDecisionError, got {other:?}"),
    }
}

#[tokio::test]
async fn approval_decision_not_found_sends_error() {
    let state = make_empty_state();
    let session_id = Uuid::new_v4();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));

    let id = Uuid::new_v4();
    let json =
        format!(r#"{{"type":"approval_decision","approval_id":"{id}","decision":"approved"}}"#);
    dispatch_client_message(&json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive an error event");
    match event {
        ServerEvent::ApprovalDecisionError {
            error, approval_id, ..
        } => {
            assert_eq!(error, "not_found");
            assert_eq!(approval_id, id);
        }
        other => panic!("expected ApprovalDecisionError, got {other:?}"),
    }
}

#[tokio::test]
async fn approval_decision_already_decided_sends_error() {
    use swage_storage::{ApprovalKind, ApprovalState, NewApprovalRequest};

    let state = make_empty_state();
    let session_id = Uuid::new_v4();

    // Create a pending approval and immediately mark it approved.
    let approval = state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id: Uuid::new_v4(),
            agent_id: "gateway".to_string(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".to_string()),
            tool_arguments: None,
            hook_name: None,
            prompt: "Allow shell?".to_string(),
            context: None,
            timeout_at: chrono::Utc::now(),
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        })
        .await
        .expect("create approval");

    // Pre-decide the approval so the handler sees it as already resolved.
    state
        .storage
        .update_approval_state(approval.id, ApprovalState::Approved, None)
        .await
        .expect("update state");

    let (tx, mut rx) = mpsc::unbounded_channel();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));
    let json = format!(
        r#"{{"type":"approval_decision","approval_id":"{}","decision":"rejected"}}"#,
        approval.id
    );
    dispatch_client_message(&json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive error event");
    match event {
        ServerEvent::ApprovalDecisionError {
            error,
            current_state,
            ..
        } => {
            assert_eq!(error, "already_decided");
            assert!(
                current_state.is_some(),
                "current_state should reflect the existing state"
            );
        }
        other => panic!("expected ApprovalDecisionError, got {other:?}"),
    }
}

#[tokio::test]
async fn approval_decision_success_sends_confirmed() {
    use swage_storage::{ApprovalKind, NewApprovalRequest};

    let state = make_empty_state();
    let session_id = Uuid::new_v4();

    // Create a pending approval.
    let approval = state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id: Uuid::new_v4(),
            agent_id: "gateway".to_string(),
            kind: ApprovalKind::Tool,
            tool_name: Some("write_file".to_string()),
            tool_arguments: None,
            hook_name: None,
            prompt: "Write to /tmp?".to_string(),
            context: None,
            timeout_at: chrono::Utc::now(),
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        })
        .await
        .expect("create approval");

    let (tx, mut rx) = mpsc::unbounded_channel();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));
    let json = format!(
        r#"{{"type":"approval_decision","approval_id":"{}","decision":"approved","reason":"Looks safe"}}"#,
        approval.id
    );
    dispatch_client_message(&json, &state, session_id, tx, sub).await;

    let event = rx.recv().await.expect("should receive ApprovalConfirmed");
    match event {
        ServerEvent::ApprovalConfirmed {
            approval_id,
            decision,
            reason,
        } => {
            assert_eq!(approval_id, approval.id);
            assert_eq!(decision, "approved");
            assert_eq!(reason, Some("Looks safe".to_string()));
        }
        other => panic!("expected ApprovalConfirmed, got {other:?}"),
    }
}

#[tokio::test]
async fn approval_decision_fires_oneshot_channel() {
    use swage_storage::{ApprovalKind, NewApprovalRequest};

    let state = make_empty_state();
    let session_id = Uuid::new_v4();

    // Create a pending approval.
    let approval = state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id: Uuid::new_v4(),
            agent_id: "gateway".to_string(),
            kind: ApprovalKind::Tool,
            tool_name: Some("deploy".to_string()),
            tool_arguments: None,
            hook_name: None,
            prompt: "Deploy to prod?".to_string(),
            context: None,
            timeout_at: chrono::Utc::now(),
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        })
        .await
        .expect("create approval");

    // Register a oneshot channel so the handler can notify a waiting agent.
    let (oneshot_tx, oneshot_rx) = tokio::sync::oneshot::channel::<swage_agent::ApprovalDecision>();
    state
        .approval_channels
        .lock()
        .unwrap()
        .insert(approval.id, oneshot_tx);

    let (tx, _rx) = mpsc::unbounded_channel();
    let sub = Arc::new(Mutex::new(crate::subscription::SubscriptionState::new(
        false,
    )));
    let json = format!(
        r#"{{"type":"approval_decision","approval_id":"{}","decision":"rejected","reason":"Too risky"}}"#,
        approval.id
    );
    dispatch_client_message(&json, &state, session_id, tx, sub).await;

    // The oneshot should have fired with the rejection.
    let decision = oneshot_rx.await.expect("oneshot should have fired");
    assert_eq!(
        decision.decision,
        swage_agent::approval::ApprovalDecisionValue::Rejected
    );
    assert_eq!(decision.reason, Some("Too risky".to_string()));
}
