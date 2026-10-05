//! Authorship of the messages the platform writes into a conversation.
//!
//! A delegation's brief in the child, its completion notice in the parent and
//! a schedule's prompt all ride the user role to the model, but none was typed
//! by the person. Each must be stored with the origin that composed it and
//! project that origin on the conversation the web client reads, so only the
//! person's own messages read as theirs.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite;
use uuid::Uuid;

use swage_core::{MessageKind, StreamChunk};
use swage_gateway::schedule;
use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_gateway::tool::ToolDispatcher;
use swage_protocol::client::schedule::{
    CreateScheduleRequest, ScheduleCreator, ScheduleSpec, SessionPolicy,
};
use swage_storage::model::NewAgentMessage;
use swage_storage::store::{SessionStore, Storage, ThreadStore};
use swage_storage::{MessageStore, SqliteStorage, TopicStore};

use crate::support::{CapturingProvider, start_live_server};

fn text_round(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::Delta {
            content: text.into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]
}

fn live_state(storage: &Arc<SqliteStorage>, rounds: Vec<Vec<StreamChunk>>) -> AppState {
    let (provider, _requests) = CapturingProvider::new("origin-model", rounds);
    AppState::new(
        storage.clone() as Arc<dyn Storage>,
        storage.clone(),
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        ToolDispatcher::allow_all(vec![]),
        None,
    )
}

async fn conversation_messages(
    client: &reqwest::Client,
    address: std::net::SocketAddr,
    session_id: Uuid,
) -> Vec<Value> {
    let detail: Value = client
        .get(format!("http://{address}/api/conversations/{session_id}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    detail["messages"].as_array().unwrap().clone()
}

/// The single projected message whose content starts with `prefix`.
fn projected<'a>(messages: &'a [Value], prefix: &str) -> &'a Value {
    let matching: Vec<&Value> = messages
        .iter()
        .filter(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.starts_with(prefix))
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected one message starting {prefix:?}: {messages:?}"
    );
    matching[0]
}

/// The stored origin of the single recorded message starting with `prefix`.
async fn stored_origin(
    storage: &SqliteStorage,
    session_id: Uuid,
    prefix: &str,
) -> swage_storage::MessageOrigin {
    let thread = storage.list_by_session(session_id).await.unwrap()[0].clone();
    let rows: Vec<_> = MessageStore::list_by_thread(storage, thread.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|row| row.content.starts_with(prefix))
        .collect();
    assert_eq!(rows.len(), 1, "expected one stored row starting {prefix:?}");
    rows[0].origin
}

async fn wait_for_delivery(client: &reqwest::Client, address: std::net::SocketAddr, id: Uuid) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status: Value = client
            .get(format!("http://{address}/api/messages/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        match status["status"].as_str() {
            Some("delivered") => return,
            Some("failed") => panic!("delegation failed: {status}"),
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "delegation did not complete: {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delegated_brief_is_recorded_and_projected_as_the_delegations() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    let state = live_state(&storage, vec![text_round("child answer")]);
    let (address, _server) = start_live_server(build_router(state)).await;
    let client = reqwest::Client::new();

    let response: Value = client
        .post(format!("http://{address}/api/messages"))
        .json(&json!({
            "sender_agent_id": "agent-a",
            "recipient_agent_id": "agent-b",
            "kind": "delegate",
            "correlation_id": null,
            "topic_id": topic.id,
            "sender_turn_id": null,
            "payload": {"task": "Summarise the quarterly report"}
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let message_id: Uuid = response["id"].as_str().unwrap().parse().unwrap();
    wait_for_delivery(&client, address, message_id).await;

    let child = storage.list_sessions_by_agent_id("agent-b").await.unwrap()[0].clone();
    let messages = conversation_messages(&client, address, child.id).await;
    let brief = projected(&messages, "Summarise the quarterly report");
    assert_eq!(brief["role"], "user", "the brief still reaches the model as its task");
    assert_eq!(brief["origin"], "delegation");
    assert_eq!(
        stored_origin(&storage, child.id, "Summarise the quarterly report").await,
        swage_storage::MessageOrigin::Delegation
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delegated_completion_notice_is_the_delegations_and_only_typed_prompts_are_the_persons()
{
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = live_state(&storage, vec![text_round("noted")]);
    let router = swage_agent::MessageRouter::new(storage.clone() as Arc<dyn Storage>, None);
    let (address, _server) = start_live_server(build_router(state)).await;
    let client = reqwest::Client::new();

    let conversation: Value = client
        .post(format!("http://{address}/api/conversations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id: Uuid = conversation["id"].as_str().unwrap().parse().unwrap();
    let parent_agent = storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap()
        .agent_id;
    // A detached child finished while the parent was idle: its Result waits
    // in the parent agent's mailbox for the parent's next turn.
    router
        .send_message(NewAgentMessage {
            sender_agent_id: "researcher".into(),
            recipient_agent_id: parent_agent,
            kind: MessageKind::Result,
            correlation_id: None,
            payload: json!({"response": "Gathered 3 sources."}),
        })
        .await
        .unwrap();

    let (socket, _) = tokio_tungstenite::connect_async(format!(
        "ws://{address}/api/conversations/{session_id}/ws"
    ))
    .await
    .unwrap();
    let (mut sink, mut stream) = socket.split();
    sink.send(tungstenite::Message::Text(
        json!({"type":"send_message","content":"What did the researcher find?"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = stream.next().await {
            if let tungstenite::Message::Text(text) = message.unwrap() {
                let event: Value = serde_json::from_str(&text).unwrap();
                if event["type"] == "turn_complete" {
                    return;
                }
            }
        }
        panic!("conversation stream closed before the turn completed")
    })
    .await
    .expect("parent turn completes");

    let messages = conversation_messages(&client, address, session_id).await;
    let notice = projected(&messages, "Delegated agent researcher completed:");
    assert_eq!(notice["origin"], "delegation");
    assert_eq!(
        stored_origin(&storage, session_id, "Delegated agent researcher completed:").await,
        swage_storage::MessageOrigin::Delegation
    );
    // One prompt typed, so exactly one message is the person's.
    let persons: Vec<&Value> = messages
        .iter()
        .filter(|message| message["origin"] == "user")
        .collect();
    assert_eq!(persons.len(), 1, "only the typed prompt is the person's: {messages:?}");
    assert_eq!(persons[0]["content"], "What did the researcher find?");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_schedule_runs_prompt_is_recorded_and_projected_as_the_schedules() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = live_state(&storage, vec![text_round("queue is empty")]);
    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Check the queue.".into(),
            spec: ScheduleSpec::Interval {
                every: "every 1 hours".into(),
            },
            session: SessionPolicy::Fresh {
                topic_id: None,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: None,
        },
        ScheduleCreator::User,
    )
    .await
    .unwrap();
    let run = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    let session_id = run.session_id.expect("the run records its session");
    let (address, _server) = start_live_server(build_router(state)).await;
    let client = reqwest::Client::new();

    let messages = conversation_messages(&client, address, session_id).await;
    let prompt = projected(&messages, "Check the queue.");
    assert!(
        prompt["content"]
            .as_str()
            .unwrap()
            .contains("first run of this schedule"),
        "the prompt carries its generated previous-run section: {prompt}"
    );
    assert_eq!(prompt["role"], "user", "the prompt still reaches the model as its task");
    assert_eq!(prompt["origin"], "schedule");
    assert_eq!(
        stored_origin(&storage, session_id, "Check the queue.").await,
        swage_storage::MessageOrigin::Schedule
    );
}
