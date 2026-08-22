#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;
use uuid::Uuid;

use swage_config::HierarchyLevel;
use swage_context::briefing::BriefingElement;
use swage_context::element::ContextElement;
use swage_context::named_element::ResolvedNamedElement;
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::server::build_router;
use swage_gateway::service::ConversationService;
use swage_gateway::state::AppState;
use swage_gateway::tool::session_variable::SessionVariableTool;
use swage_gateway::tool::{Tool, ToolDispatcher, ToolRegistry};
use swage_gateway::tool_executor::GatewayToolContext;
use swage_provider::LlmProvider;
use swage_storage::{
    AuditStore, SessionStore, SessionVariableStore, SpaceStore, SqliteStorage, Storage,
    ThreadStore, TurnStore,
};
use swage_tool::context::MockToolContext;
use swage_tool::permission::PermissionConfig;
use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};

struct UnusedProvider;
#[async_trait]
impl LlmProvider for UnusedProvider {
    fn model_id(&self) -> &str {
        "unused"
    }
    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unreachable!()
    }
    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!()
    }
    fn stream(
        &self,
        _: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        unreachable!()
    }
}

struct ToolCallingProvider {
    rounds: std::sync::Mutex<Vec<Vec<StreamChunk>>>,
    requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
}

impl ToolCallingProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> (Self, Arc<std::sync::Mutex<Vec<CompletionRequest>>>) {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                rounds: std::sync::Mutex::new(rounds),
                requests: Arc::clone(&requests),
            },
            requests,
        )
    }
}
#[async_trait]
impl LlmProvider for ToolCallingProvider {
    fn model_id(&self) -> &str {
        "tool-caller"
    }
    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unreachable!()
    }
    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!()
    }
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(futures::stream::iter(self.rounds.lock().unwrap().remove(0)))
    }
}

fn dispatcher() -> ToolDispatcher {
    let mut registry = ToolRegistry::default();
    registry
        .register_builtin(Arc::new(SessionVariableTool))
        .unwrap();
    ToolDispatcher::new(
        Arc::new(registry),
        PermissionConfig {
            allow: vec!["session-authoring".into()],
            deny: vec![],
            bundles: HashMap::from([("session-authoring".into(), vec!["session_values".into()])]),
            require_approval: vec![],
        },
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(swage_safety::LeakDetector::default()),
        false,
    )
}

fn host_context(state: &AppState, session_id: Uuid) -> GatewayToolContext {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let context = MockToolContext::new("gateway", session_id.to_string());
    GatewayToolContext::new(&context, tx, state.storage.clone()).with_app_state(Some(state.clone()))
}

fn render(variables: BTreeMap<String, String>) -> Vec<ChatMessage> {
    let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(BriefingElement::new(
        ResolvedNamedElement {
            name: "briefing".into(),
            level: HierarchyLevel::Global,
            path: "elements/briefing.md".into(),
            content: "Task: {{task}}; owner: {{owner}}".into(),
        },
    ))];
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("briefing.js");
    std::fs::write(&path, "add(elements[0].render());").unwrap();
    let output = swage_context::js_assembly::assemble_named(
        "briefing",
        Some(&path),
        &elements,
        &swage_config::model::ContextConfig::default(),
        &swage_context::tokeniser::TiktokenTokeniser::new("gpt-5"),
        &variables,
    );
    assert!(
        output.report.errors.is_empty(),
        "assembly should not fall back: {:?}",
        output.report.errors
    );
    output.messages
}

async fn wait_for_message_delivery(
    client: &reqwest::Client,
    address: std::net::SocketAddr,
    message_id: Uuid,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status: serde_json::Value = client
            .get(format!("http://{address}/api/messages/{message_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        match status["status"].as_str() {
            Some("delivered") => return,
            Some("failed") => panic!("external delegation failed: {status}"),
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "external delegation did not complete: {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn real_tool_audits_whole_state_and_updated_values_recompose() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        storage.clone() as Arc<dyn Storage>,
        storage.clone(),
        Arc::new(UnusedProvider),
        swage_config::ResolvedConfig::default(),
        ToolDispatcher::allow_all(vec![]),
        None,
    );
    let space = state.space_store.get_or_create_default().await.unwrap();
    let session = ConversationService::new(state.storage.clone(), state.space_store.clone())
        .create_session_in_space_for_agent(space.id, "gateway", None)
        .await
        .unwrap();
    let context = host_context(&state, session.id);
    let dispatcher = dispatcher();

    let tool = SessionVariableTool;
    assert_eq!(tool.parameters()["required"], json!(["action", "name"]));
    let invalid = dispatcher
        .dispatch(
            "session_values",
            json!({"action":"set","name":"Not Valid","value":"untouched"}),
            &context,
            "gateway",
        )
        .await
        .unwrap_err();
    assert!(
        format!("{invalid:?}").contains("lowercase ASCII"),
        "validation error should preserve the name rule: {invalid:?}"
    );
    assert!(
        state
            .storage
            .list_session_variables(session.id)
            .await
            .unwrap()
            .is_empty()
    );

    dispatcher
        .dispatch(
            "session_values",
            json!({"action":"set","name":"task","value":"inspect storage"}),
            &context,
            "gateway",
        )
        .await
        .unwrap();
    let first = state
        .storage
        .list_session_variables(session.id)
        .await
        .unwrap()
        .into_iter()
        .map(|value| (value.name, value.value))
        .collect();
    assert_eq!(
        render(first)[0].text_content(),
        "Task: inspect storage; owner: ⟦unset:owner⟧"
    );

    dispatcher
        .dispatch(
            "session_values",
            json!({"action":"set","name":"owner","value":"operator"}),
            &context,
            "gateway",
        )
        .await
        .unwrap();

    dispatcher
        .dispatch(
            "session_values",
            json!({"action":"set","name":"task","value":"verify audit"}),
            &context,
            "gateway",
        )
        .await
        .unwrap();
    let second = state
        .storage
        .list_session_variables(session.id)
        .await
        .unwrap()
        .into_iter()
        .map(|value| (value.name, value.value))
        .collect();
    assert_eq!(
        render(second)[0].text_content(),
        "Task: verify audit; owner: operator"
    );

    let events = storage
        .list_audit_events_by_session(session.id)
        .await
        .unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].session_id, Some(session.id));
    assert_eq!(events[2].metadata["prior_state"], json!({}));
    assert_eq!(
        events[2].metadata["new_state"],
        json!({"task":"inspect storage"})
    );
    assert_eq!(
        events[1].metadata["prior_state"],
        json!({"task":"inspect storage"})
    );
    assert_eq!(
        events[1].metadata["new_state"],
        json!({"owner":"operator", "task":"inspect storage"})
    );
    assert_eq!(
        events[0].metadata["prior_state"],
        json!({"owner":"operator", "task":"inspect storage"})
    );
    assert_eq!(
        events[0].metadata["new_state"],
        json!({"owner":"operator", "task":"verify audit"})
    );

    dispatcher
        .dispatch(
            "session_values",
            json!({"action":"remove","name":"task"}),
            &context,
            "gateway",
        )
        .await
        .unwrap();
    let remaining = state
        .storage
        .list_session_variables(session.id)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].name, "owner");
    assert_eq!(remaining[0].value, "operator");
    let events = storage
        .list_audit_events_by_session(session.id)
        .await
        .unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].metadata["action"], "remove_session_value");
    assert_eq!(
        events[0].metadata["prior_state"],
        json!({"owner":"operator", "task":"verify audit"})
    );
    assert_eq!(events[0].metadata["new_state"], json!({"owner":"operator"}));

    let missing = dispatcher
        .dispatch(
            "session_values",
            json!({"action":"remove","name":"task"}),
            &context,
            "gateway",
        )
        .await
        .unwrap_err();
    assert!(
        format!("{missing:?}").contains("no session value 'task' exists"),
        "missing removal should preserve the validation error: {missing:?}"
    );
    assert_eq!(
        storage
            .list_audit_events_by_session(session.id)
            .await
            .unwrap()
            .len(),
        4,
        "a rejected removal must not create an audit event"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_listener_turns_set_and_update_the_values_used_by_composition() {
    let (provider, captured_requests) = ToolCallingProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "set-1".into(),
                name: "session_values".into(),
                arguments: json!({"action":"set","name":"task","value":"inspect storage"}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "saved".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "set-2".into(),
                name: "session_values".into(),
                arguments: json!({"action":"set","name":"task","value":"verify audit"}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "updated".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let swage_home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(swage_home.path().join("assemblies")).unwrap();
    std::fs::write(
        swage_home.path().join("assemblies/briefing.js"),
        "add(elements.find(element => element.name === 'prompts')); add(elements.find(element => element.name === 'current-work').render());",
    )
    .unwrap();
    let element_path = swage_home
        .path()
        .join("spaces/default/elements/current-work.md");
    std::fs::create_dir_all(element_path.parent().unwrap()).unwrap();
    std::fs::write(&element_path, "Current work: {{task}}").unwrap();
    let mut environment = crate::support::ProcessEnvGuard::new();
    environment.set_swage_home(swage_home.path());
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let mut config = swage_config::ResolvedConfig::default();
    config.context.assembly = "briefing".into();
    config.instructions = "Task: {{task}}; owner: {{owner}}".into();
    let state = AppState::new(
        storage.clone() as Arc<dyn Storage>,
        storage.clone(),
        Arc::new(provider),
        config,
        ToolDispatcher::allow_all(vec![Arc::new(SessionVariableTool) as Arc<dyn Tool>]),
        None,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    let conversation: serde_json::Value = reqwest::Client::new()
        .post(format!("http://{address}/api/conversations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id: Uuid = conversation["id"].as_str().unwrap().parse().unwrap();
    let (socket, _) = tokio_tungstenite::connect_async(format!(
        "ws://{address}/api/conversations/{session_id}/ws"
    ))
    .await
    .unwrap();
    let (mut sink, mut stream) = socket.split();

    for prompt in ["save the task", "update the task"] {
        sink.send(tungstenite::Message::Text(
            json!({"type":"send_message","content":prompt})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(message) = stream.next().await {
                let message = message.unwrap();
                if let tungstenite::Message::Text(text) = message {
                    let event: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if event["type"] == "turn_complete" {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        let variables = storage
            .list_session_variables(session_id)
            .await
            .unwrap()
            .into_iter()
            .map(|value| (value.name, value.value))
            .collect();
        let expected = if prompt.starts_with("save") {
            "Task: inspect storage; owner: ⟦unset:owner⟧"
        } else {
            "Task: verify audit; owner: ⟦unset:owner⟧"
        };
        assert_eq!(render(variables)[0].text_content(), expected);
    }
    let events: Vec<_> = storage
        .list_audit_events_by_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.event_kind == "configuration_change" && event.entity_kind == "session_values"
        })
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].metadata["prior_state"],
        json!({"task":"inspect storage"})
    );
    assert_eq!(
        events[0].metadata["new_state"],
        json!({"task":"verify audit"})
    );

    let requests = captured_requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let first_context = requests[0]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        first_context.contains("Task: {{task}}; owner: {{owner}}"),
        "literal prompts should preserve brace-shaped text: {first_context}"
    );
    assert!(
        first_context.contains("Current work: ⟦unset:task⟧"),
        "the foreground turn should compose named elements from its populated hierarchy: {first_context}"
    );
    let second_turn_context = requests[2]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_turn_context.contains("Task: {{task}}; owner: {{owner}}"),
        "literal prompts should remain unchanged after session values are saved: {second_turn_context}"
    );
    assert!(
        second_turn_context.contains("Current work: inspect storage"),
        "the real session_values update should re-render the named element: {second_turn_context}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_http_delegations_apply_call_scoped_context_composition() {
    let (provider, captured_requests) = ToolCallingProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "first child response".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "second child response".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]);
    let swage_home = tempfile::tempdir().unwrap();
    let element_path = swage_home
        .path()
        .join("spaces/default/elements/current-work.md");
    std::fs::create_dir_all(element_path.parent().unwrap()).unwrap();
    std::fs::write(&element_path, "Briefing for {{owner}}: first version").unwrap();
    std::fs::write(
        element_path.parent().unwrap().join("unrequested.md"),
        "This must not reach the child",
    )
    .unwrap();
    let assembly_path = swage_home.path().join("assemblies/all-selected.js");
    std::fs::create_dir_all(assembly_path.parent().unwrap()).unwrap();
    std::fs::write(
        assembly_path,
        "for (const element of elements) add(typeof element.render === 'function' ? element.render() : element);",
    )
    .unwrap();
    let mut environment = crate::support::ProcessEnvGuard::new();
    environment.set_swage_home(swage_home.path());

    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let space = storage.get_or_create_default().await.unwrap();
    let state = AppState::new(
        storage.clone() as Arc<dyn Storage>,
        storage.clone(),
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        ToolDispatcher::allow_all(vec![]),
        None,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    let client = reqwest::Client::new();

    let dispatch = |task: &'static str| {
        let client = client.clone();
        async move {
            let response: serde_json::Value = client
                .post(format!("http://{address}/api/messages"))
                .json(&json!({
                    "sender_agent_id": "agent-a",
                    "recipient_agent_id": "agent-b",
                    "kind": "delegate",
                    "correlation_id": null,
                    "space_id": space.id,
                    "sender_turn_id": null,
                    "payload": {
                        "task": task,
                        "elements": ["current-work"],
                        "variables": {"owner": "HTTP owner"},
                        "assembly": "all-selected"
                    }
                }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            response["id"].as_str().unwrap().parse::<Uuid>().unwrap()
        }
    };

    let first_message = dispatch("first dispatch").await;
    wait_for_message_delivery(&client, address, first_message).await;
    let first_session = storage.list_sessions_by_agent_id("agent-b").await.unwrap()[0].clone();
    let first_thread = storage.list_by_session(first_session.id).await.unwrap()[0].clone();
    let first_turn = storage.list_turns_by_thread(first_thread.id).await.unwrap()[0].clone();
    let first_report: serde_json::Value = client
        .get(format!(
            "http://{address}/api/turns/{}/context",
            first_turn.id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        first_report["report"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["name"] == "current-work"),
        "the live child report should name the authored element: {first_report}"
    );
    assert_eq!(first_report["report"]["assembly_name"], "all-selected");
    assert!(
        first_report["report"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|element| element["name"] != "unrequested"),
        "the all-elements assembly must not see an unrequested briefing: {first_report}"
    );

    std::fs::write(
        &element_path,
        "Briefing for {{owner}}: edited for the next dispatch",
    )
    .unwrap();
    let second_message = dispatch("second dispatch").await;
    wait_for_message_delivery(&client, address, second_message).await;
    let second_session = storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .unwrap()
        .into_iter()
        .find(|session| session.id != first_session.id)
        .expect("second HTTP dispatch should create a new child session");
    let second_thread = storage.list_by_session(second_session.id).await.unwrap()[0].clone();
    let second_turn = storage
        .list_turns_by_thread(second_thread.id)
        .await
        .unwrap()[0]
        .clone();
    let second_report: serde_json::Value = client
        .get(format!(
            "http://{address}/api/turns/{}/context",
            second_turn.id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        second_report["report"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["name"] == "current-work"),
        "the second live child report should retain the element identity: {second_report}"
    );
    assert_eq!(second_report["report"]["assembly_name"], "all-selected");
    assert!(
        second_report["report"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|element| element["name"] != "unrequested"),
        "the second all-elements assembly must not see an unrequested briefing: {second_report}"
    );

    for session in storage.list_sessions_by_agent_id("agent-b").await.unwrap() {
        assert!(
            storage
                .list_session_variables(session.id)
                .await
                .unwrap()
                .is_empty(),
            "call-scoped bindings must not persist to the child session"
        );
    }

    let requests = captured_requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first_context = requests[0]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    let second_context = requests[1]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        first_context.contains("Briefing for HTTP owner: first version"),
        "{first_context}"
    );
    assert!(
        second_context.contains("Briefing for HTTP owner: edited for the next dispatch"),
        "{second_context}"
    );
    assert!(
        !second_context.contains("first version"),
        "{second_context}"
    );
    assert!(
        !first_context.contains("This must not reach the child")
            && !second_context.contains("This must not reach the child"),
        "{second_context}"
    );
}
