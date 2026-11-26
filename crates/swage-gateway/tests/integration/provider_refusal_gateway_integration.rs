#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Router, extract::State, http::header, routing::post};
use futures::StreamExt;
use serde_json::{Value, json};
use swage_gateway::{server::build_router, state::AppState, tool::ToolDispatcher};
use swage_provider::{
    anthropic::AnthropicProvider,
    provider::ProviderConfig,
    retry::{RetryConfig, RetryProvider},
};
use swage_storage::{SqliteStorage, Storage, ToolCallStore, TopicStore};

use crate::support::{ProcessEnvGuard, start_live_server, ws_recv_lenient};

// Supply provider wire events, never a turn state or a conversation message.
fn response(reason: &str, text: &str) -> String {
    let mut events = vec![(
        "message_start",
        json!({"type":"message_start", "message":{"usage":{"input_tokens":17,"output_tokens":0}}}),
    )];
    if !text.is_empty() {
        events.push(("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})));
        events.push(("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}})));
        events.push((
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ));
    }
    events.push(("message_delta", json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":if text.is_empty() {0} else {5}}})));
    events.push(("message_stop", json!({"type":"message_stop"})));
    events
        .into_iter()
        .map(|(name, data)| format!("event: {name}\ndata: {data}\n\n"))
        .collect()
}

async fn serve_response(
    State(rounds): State<Arc<Mutex<Vec<String>>>>,
) -> ([(header::HeaderName, &'static str); 2], String) {
    let body = rounds.lock().unwrap().remove(0);
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (
                header::HeaderName::from_static("request-id"),
                "req-refusal-fixture",
            ),
        ],
        body,
    )
}

fn tool_response() -> String {
    [
        ("message_start", json!({"type":"message_start","message":{"usage":{"input_tokens":17,"output_tokens":0}}})),
        ("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"saved-task","name":"session_values","input":{}}})),
        ("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json": "{\"action\":\"set\",\"name\":\"task\",\"value\":\"inspect storage\"}"}})),
        ("content_block_stop", json!({"type":"content_block_stop","index":0})),
        ("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}})),
        ("message_stop", json!({"type":"message_stop"})),
    ].into_iter().map(|(name, data)| format!("event: {name}\ndata: {data}\n\n")).collect()
}

async fn account_for_response(
    reason: &str,
    text: &str,
    expected_state: &str,
    expected_notice: &str,
    attempts: usize,
    tools_first: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let rounds = Arc::new(Mutex::new(
        tools_first
            .then(tool_response)
            .into_iter()
            .chain((0..attempts).map(|_| response(reason, text)))
            .chain([response(
                "end_turn",
                "{\"answer\":\"Benign follow-up completed.\"}",
            )])
            .collect::<Vec<_>>(),
    ));
    let (upstream, upstream_server) = start_live_server(
        Router::new()
            .route("/v1/messages", post(serve_response))
            .with_state(Arc::clone(&rounds)),
    )
    .await;
    let provider = RetryProvider::new(
        AnthropicProvider::from_config(ProviderConfig::new(
            "fixture-key",
            "fixture-model",
            format!("http://{upstream}"),
        )),
        RetryConfig {
            max_retries: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        },
    );
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::for_test(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        ToolDispatcher::allow_all(vec![Arc::new(
            swage_gateway::tool::session_variable::SessionVariableTool,
        ) as Arc<dyn swage_gateway::tool::Tool>]),
        None,
    );
    let mut announcements = state.broadcast.subscribe();
    let (address, server) = start_live_server(build_router(state)).await;
    let base = format!("http://{address}");
    let client = reqwest::Client::new();
    let conversation: Value = client
        .post(format!("{base}/api/conversations"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let conversation_id = conversation["id"].as_str().unwrap();
    let (socket, _) = tokio_tungstenite::connect_async(format!(
        "ws://{address}/api/conversations/{conversation_id}/ws"
    ))
    .await
    .unwrap();
    let (_socket_sink, mut socket_stream) = socket.split();
    let submitted: Value = client.post(format!("{base}/api/turns")).json(&json!({"conversation_id":conversation_id,"message":"Read a harmless file.","output_schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let turn_id = submitted["turn_id"].as_str().unwrap();
    let turn = wait_for_turn(&client, &base, turn_id).await;
    assert_eq!(
        turn["state"], expected_state,
        "the provider's cause must survive turn settlement: {turn}"
    );
    let detail = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail: Value = client
                .get(format!("{base}/api/conversations/{conversation_id}"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["origin"] == "platform")
            {
                break detail;
            }
            // The assertion, rather than a timeout, diagnoses a missing account.
            if rounds.lock().unwrap().len() == 1 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let final_detail: Value = client
                    .get(format!("{base}/api/conversations/{conversation_id}"))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                break final_detail;
            }
        }
    })
    .await
    .unwrap();
    if tools_first {
        let records = storage
            .list_tool_calls_by_turn(turn_id.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(records.len(), 1, "the earlier tool round must be recorded");
        assert_eq!(
            records[0].status.as_deref(),
            Some("success"),
            "the earlier tool effect must succeed"
        );
        let projected = detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "assistant")
            .flat_map(|message| message["tool_calls"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            projected.len(),
            1,
            "reloaded conversation must retain earlier tool effects: {detail}"
        );
        assert_eq!(projected[0]["call_id"], "saved-task");
        assert_eq!(projected[0]["status"], "success");
        assert_eq!(projected[0]["arguments"]["value"], "inspect storage");
        assert!(
            !projected[0]["result"].is_null(),
            "the result remains visible"
        );
    }
    let notice = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["origin"] == "platform")
        .expect("the settled cause needs a durable platform account");
    assert!(
        notice["content"]
            .as_str()
            .unwrap()
            .contains(expected_notice),
        "{notice}"
    );
    assert_eq!(notice["turn_state"], expected_state);
    assert_eq!(notice["turn_id"], turn_id);
    if !text.is_empty() {
        assert!(
            detail["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "assistant"
                    && message["content"] == text
                    && message["turn_state"] == expected_state),
            "provider text remains recorded with its true state: {detail}"
        );
    }
    assert!(
        !detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "assistant"
                && m["content"] == ""
                && m["turn_state"] == "completed")
    );
    assert!(notice["content"].as_str().unwrap().contains("amend"));
    let mut announced = false;
    while let Ok(event) = announcements.try_recv() {
        if let swage_protocol::client::websocket::ServerEvent::MessageAppended {
            session_id,
            message,
        } = event
            && session_id.to_string() == conversation_id
            && message.id.to_string() == notice["id"].as_str().unwrap()
        {
            announced = true;
        }
    }
    assert!(announced, "the recorded account must be broadcast");
    let received = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = ws_recv_lenient(&mut socket_stream).await {
            if event["type"] == "message_appended" {
                return Some(event);
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    let received = received.expect("the conversation socket must receive the recorded account");
    assert_eq!(received["session_id"], conversation_id);
    assert_eq!(
        received["message"], *notice,
        "the socket carries the complete recorded view without a refetch"
    );
    assert_eq!(
        rounds.lock().unwrap().len(),
        1,
        "refusal must not consume the benign follow-up as a retry"
    );
    let (_, summaries) =
        crate::support::live_http_json(address, reqwest::Method::GET, "/api/conversations", None)
            .await;
    let summary = summaries["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["id"] == conversation_id)
        .unwrap();
    assert!(
        !summary["message_preview"]
            .as_str()
            .unwrap()
            .trim()
            .is_empty(),
        "the sidebar must remain non-blank immediately after the abnormal turn: {summary}"
    );
    assert_eq!(
        summary["message_preview"],
        if text.is_empty() {
            "Read a harmless file."
        } else {
            text
        },
        "a terminal platform account must preserve the person's preview"
    );
    let snapshot: Value = client
        .get(format!("{base}/api/turns/{turn_id}/context"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        snapshot.to_string().contains("req-refusal-fixture"),
        "the response witness is retained"
    );
    assert!(
        snapshot.to_string().contains(reason),
        "the received stop reason is retained"
    );
    let next: Value = client.post(format!("{base}/api/turns")).json(&json!({"conversation_id":conversation_id,"message":"Please continue with the benign request.","output_schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let next_turn = wait_for_turn(&client, &base, next["turn_id"].as_str().unwrap()).await;
    assert_eq!(next_turn["state"], "completed");
    let (_, summaries) =
        crate::support::live_http_json(address, reqwest::Method::GET, "/api/conversations", None)
            .await;
    assert!(
        summaries
            .to_string()
            .contains("Benign follow-up completed."),
        "a non-empty preview remains available: {summaries}"
    );
    server.abort();
    upstream_server.abort();
}

async fn wait_for_turn(client: &reqwest::Client, base: &str, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let turn: Value = client
                .get(format!("{base}/api/turns/{id}"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            if !["pending", "processing"].contains(&turn["state"].as_str().unwrap()) {
                break turn;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_refusal_has_a_durable_and_announced_account() {
    account_for_response(
        "refusal",
        "",
        "refused",
        "The model refused this request",
        1,
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_refusal_preserves_the_models_text_and_account() {
    account_for_response(
        "refusal",
        "I cannot fulfil that request.",
        "refused",
        "The model refused this request",
        1,
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exhausted_empty_response_has_an_anomaly_account_after_retry() {
    account_for_response(
        "end_turn",
        "",
        "failed",
        "empty provider response",
        2,
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_round_then_refusal_retains_calls_after_reload() {
    account_for_response(
        "refusal",
        "",
        "refused",
        "The model refused this request",
        1,
        true,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_round_then_empty_response_retains_calls_after_reload() {
    account_for_response("end_turn", "", "failed", "empty provider response", 2, true).await;
}
