#![allow(clippy::unwrap_used)]

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use swage_config::{ModelEndpointConfig, Protocol, ProviderEndpointConfig, ResolvedConfig};
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{ContextSnapshotStore, SessionStore, SqliteStorage, Storage, TopicStore};
use tokio::{net::TcpListener, task::JoinHandle};

use crate::support::{ProcessEnvGuard, scratch_tempdir};

struct CapturingProvider {
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait::async_trait]
impl LlmProvider for CapturingProvider {
    fn model_id(&self) -> &str {
        "capturing"
    }

    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unreachable!("turn proof streams")
    }

    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("turn proof streams")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(futures::stream::iter([
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ]))
    }
}

async fn start_server(
    provider: Arc<CapturingProvider>,
) -> (String, Arc<SqliteStorage>, JoinHandle<()>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let mut config = ResolvedConfig::default();
    config.models.default = "default".into();
    config.models.entries.insert(
        "default".into(),
        ModelEndpointConfig {
            provider_id: "default-provider".into(),
            model_id: "default-wire".into(),
            ..Default::default()
        },
    );
    config.models.entries.insert(
        "alternate".into(),
        ModelEndpointConfig {
            provider_id: "alternate-provider".into(),
            model_id: "alternate-wire".into(),
            aliases: vec!["fast".into()],
            ..Default::default()
        },
    );
    // A turn is served only by its model's configured provider, so each
    // catalogue entry names a provider the injected mock stands in for.
    for provider_id in ["default-provider", "alternate-provider"] {
        config.providers.entries.insert(
            provider_id.into(),
            ProviderEndpointConfig {
                protocol: Protocol::OpenAi,
                base_url: Some("http://mock-provider.invalid/v1".into()),
                ..Default::default()
            },
        );
    }
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        provider as Arc<dyn LlmProvider>,
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    (format!("http://{address}"), storage, server)
}

async fn submit_and_wait(base: &str, conversation_id: &str, model: Value) -> String {
    let submitted: Value = reqwest::Client::new()
        .post(format!("{base}/api/turns"))
        .json(&json!({"conversation_id": conversation_id, "thread_id": null, "message": "reply", "output_schema": null, "model": model}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let turn_id = submitted["turn_id"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let turn: Value = reqwest::Client::new()
                .get(format!("{base}/api/turns/{turn_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if turn["state"] == "completed" {
                break;
            }
            assert_ne!(turn["state"], "failed", "{turn:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    turn_id
}

#[tokio::test(flavor = "multi_thread")]
async fn turn_request_alias_uses_the_canonical_provider_model_without_reselecting_the_conversation()
{
    let root = scratch_tempdir();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let provider = Arc::new(CapturingProvider {
        requests: Mutex::new(vec![]),
    });
    let (base, storage, server) = start_server(Arc::clone(&provider)).await;
    let conversation: Value = reqwest::Client::new()
        .post(format!("{base}/api/conversations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let turn_id = submit_and_wait(&base, conversation["id"].as_str().unwrap(), json!("fast")).await;
    assert_eq!(
        provider.requests.lock().unwrap()[0].model.as_deref(),
        Some("alternate-wire")
    );
    let session = storage
        .get_session(conversation["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.selected_model_id, None);
    let snapshot: Value = serde_json::from_str(
        &storage
            .get_snapshot(turn_id.parse().unwrap())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["catalogue_entry"],
        "alternate"
    );
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["source"],
        "request"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn turn_without_a_model_records_the_persisted_conversation_selection_as_its_provenance() {
    let root = scratch_tempdir();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let provider = Arc::new(CapturingProvider {
        requests: Mutex::new(vec![]),
    });
    let (base, storage, server) = start_server(Arc::clone(&provider)).await;
    let conversation: Value = reqwest::Client::new()
        .post(format!("{base}/api/conversations"))
        .json(&json!({"selected_model_id":"alternate"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let submitted: Value = reqwest::Client::new().post(format!("{base}/api/turns")).json(&json!({"conversation_id":conversation["id"],"thread_id":null,"message":"reply","output_schema":null})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let turn_id = submitted["turn_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let turn: Value = reqwest::Client::new()
                .get(format!("{base}/api/turns/{turn_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if turn["state"] == "completed" {
                break;
            }
            assert_ne!(turn["state"], "failed", "{turn:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        provider.requests.lock().unwrap()[0].model.as_deref(),
        Some("alternate-wire")
    );
    let snapshot: Value = serde_json::from_str(
        &storage
            .get_snapshot(turn_id.parse().unwrap())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["source"],
        "conversation"
    );
    assert_eq!(
        snapshot["provider_visible"]["effective_model"]["catalogue_entry"],
        "alternate"
    );
    server.abort();
}

#[test]
fn null_turn_model_is_omitted_by_the_shared_request_serialiser() {
    let request = swage_protocol::client::turn::SubmitTurnRequest {
        conversation_id: uuid::Uuid::new_v4(),
        thread_id: None,
        message: "reply".into(),
        output_schema: None,
        compaction_focus: None,
        model: None,
        attachments: Vec::new(),
    };
    assert!(
        serde_json::to_value(request)
            .unwrap()
            .get("model")
            .is_none()
    );
}
