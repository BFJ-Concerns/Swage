use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use axum::Router;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::channel_gateway::ExternalChannelIntegration;
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;

struct MockProvider {
    rounds: Mutex<VecDeque<Vec<StreamChunk>>>,
}

impl MockProvider {
    fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: Mutex::new(rounds.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "external-channel-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("integration tests use streaming only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("integration tests use streaming only")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamChunk::Done { finish_reason: None }]);
        Box::pin(futures::stream::iter(chunks))
    }
}

async fn start_test_server(provider: impl LlmProvider + 'static) -> SocketAddr {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::ProjectStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        None,
    );
    let router: Router = build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

async fn post_json(addr: SocketAddr, path: &str, body: Value) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json::<Value>().await.unwrap();
    (status, body)
}

async fn get_json(addr: SocketAddr, path: &str) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json::<Value>().await.unwrap();
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn dev_dm_origin_reuses_session_and_delivers_to_home_target() {
    let addr = start_test_server(MockProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "first reply".into(),
            },
            StreamChunk::Done { finish_reason: None },
        ],
        vec![
            StreamChunk::Delta {
                content: "second reply".into(),
            },
            StreamChunk::Done { finish_reason: None },
        ],
    ]))
    .await;

    let (status_a, body_a) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "hello"
        }),
    )
    .await;
    assert_eq!(status_a, StatusCode::ACCEPTED);

    let (status_b, body_b) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "hello again"
        }),
    )
    .await;
    assert_eq!(status_b, StatusCode::ACCEPTED);
    assert_eq!(body_a["session_id"], body_b["session_id"]);
    assert_eq!(body_b["delivered_to"]["integration"], "dev_dm");
    assert_eq!(body_b["delivered_to"]["conversation"], "alice");

    let (_, outbox) = get_json(
        addr,
        "/api/channels/outbox?integration=dev_dm&account=workspace-a&conversation=alice",
    )
    .await;
    let deliveries = outbox["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0]["content"], "first reply");
    assert_eq!(deliveries[1]["content"], "second reply");
}

#[tokio::test(flavor = "multi_thread")]
async fn routing_keeps_direct_messages_and_threads_isolated() {
    let addr = start_test_server(MockProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "alice".into(),
            },
            StreamChunk::Done { finish_reason: None },
        ],
        vec![
            StreamChunk::Delta {
                content: "bob".into(),
            },
            StreamChunk::Done { finish_reason: None },
        ],
        vec![
            StreamChunk::Delta {
                content: "thread one".into(),
            },
            StreamChunk::Done { finish_reason: None },
        ],
        vec![
            StreamChunk::Delta {
                content: "thread two".into(),
            },
            StreamChunk::Done { finish_reason: None },
        ],
    ]))
    .await;

    let (_, alice) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({"account":"workspace-a","user_id":"alice","text":"a"}),
    )
    .await;
    let (_, bob) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({"account":"workspace-a","user_id":"bob","text":"b"}),
    )
    .await;
    let (_, thread_one) = post_json(
        addr,
        "/api/channels/dev-thread/inbound",
        json!({"account":"workspace-a","channel_id":"room-1","thread_id":"topic-1","text":"t1"}),
    )
    .await;
    let (_, thread_two) = post_json(
        addr,
        "/api/channels/dev-thread/inbound",
        json!({"account":"workspace-a","channel_id":"room-1","thread_id":"topic-2","text":"t2"}),
    )
    .await;

    assert_ne!(alice["session_id"], bob["session_id"]);
    assert_ne!(thread_one["session_id"], thread_two["session_id"]);
    assert_ne!(alice["session_id"], thread_one["session_id"]);

    let (_, alice_outbox) = get_json(
        addr,
        "/api/channels/outbox?integration=dev_dm&account=workspace-a&conversation=alice",
    )
    .await;
    let (_, bob_outbox) = get_json(
        addr,
        "/api/channels/outbox?integration=dev_dm&account=workspace-a&conversation=bob",
    )
    .await;
    let (_, thread_one_outbox) = get_json(
        addr,
        "/api/channels/outbox?integration=dev_thread&account=workspace-a&conversation=room-1/topic-1",
    )
    .await;
    let (_, thread_two_outbox) = get_json(
        addr,
        "/api/channels/outbox?integration=dev_thread&account=workspace-a&conversation=room-1/topic-2",
    )
    .await;

    assert_eq!(alice_outbox["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(alice_outbox["deliveries"][0]["content"], "alice");
    assert_eq!(bob_outbox["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(bob_outbox["deliveries"][0]["content"], "bob");
    assert_eq!(thread_one_outbox["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(thread_one_outbox["deliveries"][0]["content"], "thread one");
    assert_eq!(thread_two_outbox["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(thread_two_outbox["deliveries"][0]["content"], "thread two");
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_delivery_target_overrides_home_target() {
    let addr = start_test_server(MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "rerouted".into(),
        },
        StreamChunk::Done { finish_reason: None },
    ]]))
    .await;

    let (status, body) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "route elsewhere",
            "delivery_target": {
                "integration": "dev_thread",
                "account": "workspace-b",
                "conversation": "room-9/topic-3"
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivered_to"]["integration"], "dev_thread");
    assert_eq!(body["delivered_to"]["conversation"], "room-9/topic-3");

    let (_, rerouted) = get_json(
        addr,
        "/api/channels/outbox?integration=dev_thread&account=workspace-b&conversation=room-9/topic-3",
    )
    .await;
    assert_eq!(rerouted["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(rerouted["deliveries"][0]["content"], "rerouted");

    let (_, original_home) = get_json(
        addr,
        "/api/channels/outbox?integration=dev_dm&account=workspace-a&conversation=alice",
    )
    .await;
    assert!(
        original_home["deliveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn external_channel_integrations_serialise_as_documented_names() {
    assert_eq!(
        serde_json::to_value(ExternalChannelIntegration::DevDm).unwrap(),
        Value::String("dev_dm".into())
    );
    assert_eq!(
        serde_json::to_value(ExternalChannelIntegration::DevThread).unwrap(),
        Value::String("dev_thread".into())
    );
}
