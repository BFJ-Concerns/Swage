#![allow(clippy::unwrap_used)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    extract::{Json, State},
    http::header,
    routing::post,
};
use serde_json::{Value, json};
use swage_gateway::{
    server::build_router,
    state::AppState,
    tool::{Tool, ToolDispatcher},
};
use swage_provider::{LlmProvider, openai::OpenAiProvider, provider::ProviderConfig};
use swage_storage::{SqliteStorage, Storage, TopicStore};

use crate::support::{ProcessEnvGuard, scratch_tempdir, start_live_server};

struct UnionTool;

#[async_trait::async_trait]
impl Tool for UnionTool {
    fn name(&self) -> &str {
        "lookup"
    }
    fn description(&self) -> &str {
        "Look up a source."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "anyOf": [{"required": ["url"]}, {"required": ["query"]}],
            "allOf": [{"required": ["source"]}, {"properties": {"source": {"type": "string"}}}]
        })
    }
    async fn execute(
        &self,
        _: Value,
        _: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, swage_core::ToolError> {
        unreachable!("this journey only inspects the dispatched provider request")
    }
}

async fn record_openai_request(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(request): Json<Value>,
) -> ([(header::HeaderName, &'static str); 1], &'static str) {
    requests.lock().unwrap().push(request);
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn gateway_turn_projects_registered_union_tool_schema_on_openai_wire() {
    let root = scratch_tempdir();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let upstream_requests = Arc::new(Mutex::new(vec![]));
    let upstream_router = Router::new()
        .route("/chat/completions", post(record_openai_request))
        .with_state(Arc::clone(&upstream_requests));
    let (upstream_address, upstream_server) = start_live_server(upstream_router).await;
    let provider = Arc::new(OpenAiProvider::from_config(ProviderConfig::new(
        "test-key",
        "test-model",
        format!("http://{upstream_address}"),
    )));
    let state = AppState::for_test(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        ToolDispatcher::allow_all(vec![Arc::new(UnionTool) as Arc<dyn Tool>]),
        None,
    );
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
    let submitted: Value = client.post(format!("{base}/api/turns")).json(&json!({"conversation_id": conversation["id"], "thread_id": null, "message": "use lookup", "output_schema": null})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let turn_id = submitted["turn_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let turn: Value = client
                .get(format!("{base}/api/turns/{turn_id}"))
                .send()
                .await
                .unwrap()
                .error_for_status()
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
    let requests = upstream_requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the adapter must dispatch one wire request"
    );
    let parameters = &requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "lookup")
        .expect("registered lookup reaches the OpenAI wire format")["function"]["parameters"];
    assert!(parameters.get("anyOf").is_none());
    assert!(parameters.get("allOf").is_none());
    let guidance = parameters["description"]
        .as_str()
        .expect("the projected schema retains local-validation guidance");
    assert!(guidance.contains("anyOf (at least one of: requires `url`; or requires `query`)"));
    assert!(guidance.contains(
        "allOf (all of: requires `source`; and an additional locally validated alternative)"
    ));
    server.abort();
    upstream_server.abort();
}
