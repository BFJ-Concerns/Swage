#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use axum::{Json, Router, routing::post};
use reqwest::StatusCode;
use rusqlite::Connection;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::channel_gateway::ExternalChannelIntegration;
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    NewChannelInboundMessage, NewChannelLink, SqliteStorage,
    store::{
        ChannelLinkStore, ChannelQueueStore, DeliveryStore, SessionStore, SpaceStore, ThreadStore,
    },
};

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
        let chunks = self.rounds.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![StreamChunk::Done {
                finish_reason: None,
            }]
        });
        Box::pin(futures::stream::iter(chunks))
    }
}

async fn start_test_server(provider: impl LlmProvider + 'static) -> SocketAddr {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
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

#[tokio::test(flavor = "multi_thread")]
async fn queued_channel_drain_carries_the_created_turn_id_into_the_delivery_record() {
    let directory = tempfile::tempdir().expect("queue database directory");
    let database_path = directory.path().join("queue.db");
    let storage = Arc::new(SqliteStorage::open(&database_path).expect("queue storage"));
    let space = storage
        .get_or_create_default()
        .await
        .expect("default space");
    storage
        .upsert_channel_link(NewChannelLink {
            integration: "dev_dm".into(),
            account: "queue-account".into(),
            conversation: "queue-user".into(),
            space_id: space.id,
            agent_id: "gateway".into(),
            session_id: None,
            home_target: None,
        })
        .await
        .expect("channel link");
    storage
        .ingest_channel_inbound(
            vec![NewChannelInboundMessage {
                integration: "dev_dm".into(),
                account: "queue-account".into(),
                conversation: "queue-user".into(),
                external_id: "queued-1".into(),
                sender: "queue-user".into(),
                content: "drain this queued message".into(),
            }],
            "dev_dm",
            "queue-account",
            "queued-1",
        )
        .await
        .expect("queue inbound message");

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "queued reply".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]])),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    swage_gateway::channel_gateway::drain_channel_queue_checked(&state)
        .await
        .expect("drain queued channel origin");

    let session = storage
        .find_session_by_channel_origin("dev_dm", "queue-account", "queue-user")
        .await
        .expect("look up channel session")
        .expect("drain creates the linked session");
    let turn = storage
        .list_by_session(session.id)
        .await
        .expect("list channel threads")
        .into_iter()
        .next()
        .expect("channel thread");
    let turn = swage_storage::store::TurnStore::list_turns_by_thread(&*storage, turn.id)
        .await
        .expect("list channel turns")
        .into_iter()
        .next()
        .expect("drain creates one turn");
    let deliveries = storage
        .list_delivery_records_by_session(session.id)
        .await
        .expect("read durable delivery record");
    let inbound_turn_id: Option<String> = Connection::open(&database_path)
        .expect("open queue database for durable provenance check")
        .query_row(
            "SELECT turn_id
             FROM channel_inbound_messages
             WHERE integration = ?1
               AND account = ?2
               AND conversation = ?3
               AND external_id = ?4",
            ["dev_dm", "queue-account", "queue-user", "queued-1"],
            |row| row.get(0),
        )
        .expect("the inbound row remains available for provenance audit");

    assert_eq!(deliveries.len(), 1);
    assert_eq!(inbound_turn_id, Some(turn.id.to_string()));
    assert_eq!(deliveries[0].turn_id, Some(turn.id));
    assert!(
        storage
            .list_pending_channel_origins()
            .await
            .expect("inspect queue after drain")
            .is_empty(),
        "the queued row must have been claimed by the turn before delivery"
    );
}

async fn start_file_backed_test_server(
    db_path: &std::path::Path,
    provider: impl LlmProvider + 'static,
) -> SocketAddr {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
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

async fn start_webhook_server() -> (String, mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = Router::new().route(
        "/",
        post(move |Json(body): Json<Value>| {
            let tx = tx.clone();
            async move {
                tx.send(body).unwrap();
                StatusCode::OK
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/"), rx)
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

fn channel_target(integration: &str, account: &str, conversation: &str) -> Value {
    json!({
        "kind": "channel",
        "integration": integration,
        "account": account,
        "conversation": conversation,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn dev_dm_origin_reuses_session_and_delivers_to_origin_by_default() {
    let addr = start_test_server(MockProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "first reply".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "second reply".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
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
    assert_eq!(
        body_b["delivered_to"].as_array().unwrap(),
        &vec![channel_target("dev_dm", "workspace-a", "alice")]
    );

    let (_, log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            body_b["session_id"].as_str().unwrap()
        ),
    )
    .await;
    let deliveries = log["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0]["content"], "first reply");
    assert_eq!(
        deliveries[0]["target"],
        channel_target("dev_dm", "workspace-a", "alice")
    );
    assert_eq!(deliveries[1]["content"], "second reply");
    assert_eq!(
        deliveries[1]["target"],
        channel_target("dev_dm", "workspace-a", "alice")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn external_channel_origin_reuses_session_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("channels.db");

    let first_addr = start_file_backed_test_server(
        &db_path,
        MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "before restart".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]]),
    )
    .await;

    let (first_status, first_body) = post_json(
        first_addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "hello"
        }),
    )
    .await;
    assert_eq!(first_status, StatusCode::ACCEPTED);

    let second_addr = start_file_backed_test_server(
        &db_path,
        MockProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "after restart".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]]),
    )
    .await;

    let (second_status, second_body) = post_json(
        second_addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "again"
        }),
    )
    .await;
    assert_eq!(second_status, StatusCode::ACCEPTED);
    assert_eq!(first_body["session_id"], second_body["session_id"]);

    let (_, log) = get_json(
        second_addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            second_body["session_id"].as_str().unwrap()
        ),
    )
    .await;
    let deliveries = log["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0]["content"], "before restart");
    assert_eq!(deliveries[1]["content"], "after restart");
}

#[tokio::test(flavor = "multi_thread")]
async fn routing_keeps_direct_messages_and_threads_isolated() {
    let addr = start_test_server(MockProvider::new(vec![
        vec![
            StreamChunk::Delta {
                content: "alice".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "bob".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "thread one".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "thread two".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
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

    let (_, alice_log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            alice["session_id"].as_str().unwrap()
        ),
    )
    .await;
    let (_, bob_log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            bob["session_id"].as_str().unwrap()
        ),
    )
    .await;
    let (_, thread_one_log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            thread_one["session_id"].as_str().unwrap()
        ),
    )
    .await;
    let (_, thread_two_log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            thread_two["session_id"].as_str().unwrap()
        ),
    )
    .await;

    assert_eq!(alice_log["deliveries"][0]["content"], "alice");
    assert_eq!(
        alice_log["deliveries"][0]["target"],
        channel_target("dev_dm", "workspace-a", "alice")
    );
    assert_eq!(bob_log["deliveries"][0]["content"], "bob");
    assert_eq!(
        bob_log["deliveries"][0]["target"],
        channel_target("dev_dm", "workspace-a", "bob")
    );
    assert_eq!(thread_one_log["deliveries"][0]["content"], "thread one");
    assert_eq!(
        thread_one_log["deliveries"][0]["target"],
        channel_target("dev_thread", "workspace-a", "room-1/topic-1")
    );
    assert_eq!(thread_two_log["deliveries"][0]["content"], "thread two");
    assert_eq!(
        thread_two_log["deliveries"][0]["target"],
        channel_target("dev_thread", "workspace-a", "room-1/topic-2")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_home_target_and_local_storage_are_delivered_through_one_router() {
    let addr = start_test_server(MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "routed".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]]))
    .await;

    let (status, body) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "route this",
            "routing": {
                "home_target": {
                    "kind": "channel",
                    "integration": "dev_thread",
                    "account": "workspace-b",
                    "conversation": "room-9/topic-3"
                },
                "store_local_output": true
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        body["delivered_to"].as_array().unwrap(),
        &vec![
            channel_target("dev_dm", "workspace-a", "alice"),
            channel_target("dev_thread", "workspace-b", "room-9/topic-3"),
            json!({"kind": "local_storage"}),
        ]
    );

    let (_, log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            body["session_id"].as_str().unwrap()
        ),
    )
    .await;
    let deliveries = log["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 3);
    assert_eq!(
        deliveries[0]["target"],
        channel_target("dev_dm", "workspace-a", "alice")
    );
    assert_eq!(
        deliveries[1]["target"],
        channel_target("dev_thread", "workspace-b", "room-9/topic-3")
    );
    assert_eq!(deliveries[2]["target"], json!({"kind": "local_storage"}));
    assert!(
        deliveries
            .iter()
            .all(|delivery| delivery["content"] == "routed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_webhook_delivery_can_skip_origin() {
    let addr = start_test_server(MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "rerouted".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]]))
    .await;
    let (webhook_url, mut webhook_events) = start_webhook_server().await;

    let (status, body) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "webhook only",
            "routing": {
                "deliver_to_origin": false,
                "explicit_targets": [
                    {
                        "kind": "webhook",
                        "url": webhook_url
                    }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        body["delivered_to"].as_array().unwrap(),
        &vec![json!({"kind": "webhook", "url": webhook_url})]
    );

    let webhook_payload = webhook_events.recv().await.expect("webhook payload");
    assert_eq!(webhook_payload["session_id"], body["session_id"]);
    assert_eq!(webhook_payload["content"], "rerouted");

    let (_, log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            body["session_id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(log["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(
        log["deliveries"][0]["target"],
        json!({"kind": "webhook", "url": webhook_url})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overlapping_origin_and_explicit_rules_do_not_duplicate_delivery() {
    let addr = start_test_server(MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "once".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]]))
    .await;

    let (status, body) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "dedupe",
            "routing": {
                "home_target": {
                    "kind": "channel",
                    "integration": "dev_dm",
                    "account": "workspace-a",
                    "conversation": "alice"
                },
                "explicit_targets": [
                    {
                        "kind": "channel",
                        "integration": "dev_dm",
                        "account": "workspace-a",
                        "conversation": "alice"
                    }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        body["delivered_to"].as_array().unwrap(),
        &vec![channel_target("dev_dm", "workspace-a", "alice")]
    );

    let (_, log) = get_json(
        addr,
        &format!(
            "/api/deliveries/log?session_id={}",
            body["session_id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(log["deliveries"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_webhook_target_is_rejected() {
    let addr = start_test_server(MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "ignored".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]]))
    .await;

    let (status, body) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "bad url",
            "routing": {
                "deliver_to_origin": false,
                "explicit_targets": [
                    {
                        "kind": "webhook",
                        "url": "not a url"
                    }
                ]
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("invalid webhook target URL")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_webhook_target_fails_explicitly() {
    let addr = start_test_server(MockProvider::new(vec![vec![
        StreamChunk::Delta {
            content: "ignored".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]]))
    .await;

    let (status, body) = post_json(
        addr,
        "/api/channels/dev-dm/inbound",
        json!({
            "account": "workspace-a",
            "user_id": "alice",
            "text": "down webhook",
            "routing": {
                "deliver_to_origin": false,
                "explicit_targets": [
                    {
                        "kind": "webhook",
                        "url": "http://127.0.0.1:1/"
                    }
                ]
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("webhook delivery failed")
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
