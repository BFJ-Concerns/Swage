#![allow(clippy::unwrap_used)]

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{
    server::build_router,
    state::AppState,
    tool::{Tool, ToolDispatcher},
};
use swage_provider::LlmProvider;
use swage_storage::{SpaceStore, SqliteStorage, Storage};

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

struct RecordingProvider {
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait::async_trait]
impl LlmProvider for RecordingProvider {
    fn model_id(&self) -> &str {
        "union-schema-recording"
    }
    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unreachable!("turn uses streaming")
    }
    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("turn uses streaming")
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

#[tokio::test(flavor = "multi_thread")]
async fn gateway_turn_dispatches_registered_union_tool_schema_to_provider() {
    let root = scratch_tempdir();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let provider = Arc::new(RecordingProvider {
        requests: Mutex::new(vec![]),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
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
    let requests = provider.requests.lock().unwrap();
    let parameters = &requests[0]
        .tool_definitions
        .iter()
        .find(|tool| tool["name"] == "lookup")
        .expect("registered lookup reaches provider")["parameters"];
    assert_eq!(
        parameters["anyOf"],
        json!([{"required": ["url"]}, {"required": ["query"]}])
    );
    assert_eq!(
        parameters["allOf"],
        json!([{"required": ["source"]}, {"properties": {"source": {"type": "string"}}}])
    );
    server.abort();
}
