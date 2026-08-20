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

use swage_config::{HierarchyLevel, prompt_rule::PromptRule};
use swage_context::element::ContextElement;
use swage_context::prompts::PromptsElement;
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
use swage_storage::{AuditStore, SessionVariableStore, SqliteStorage, Storage};
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

struct ToolCallingProvider(std::sync::Mutex<Vec<Vec<StreamChunk>>>);
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
        _: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(self.0.lock().unwrap().remove(0)))
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
    let elements: Vec<Box<dyn ContextElement>> =
        vec![Box::new(PromptsElement::new(vec![PromptRule {
            priority: 0,
            content: "Task: {{task}}; owner: {{owner}}".into(),
            source_level: HierarchyLevel::Global,
            file_path: "briefing".into(),
        }]))];
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
        "Task: verify audit; owner: ⟦unset:owner⟧"
    );

    let events = storage
        .list_audit_events_by_session(session.id)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].session_id, Some(session.id));
    assert_eq!(events[1].metadata["prior_state"], json!({}));
    assert_eq!(
        events[1].metadata["new_state"],
        json!({"task":"inspect storage"})
    );
    assert_eq!(
        events[0].metadata["prior_state"],
        json!({"task":"inspect storage"})
    );
    assert_eq!(
        events[0].metadata["new_state"],
        json!({"task":"verify audit"})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_listener_turns_set_and_update_the_values_used_by_composition() {
    let provider = ToolCallingProvider(std::sync::Mutex::new(vec![
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
    ]));
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        storage.clone() as Arc<dyn Storage>,
        storage.clone(),
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
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
}
