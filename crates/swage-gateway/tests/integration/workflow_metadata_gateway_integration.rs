#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use serde_json::{Value, json};
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{SpaceStore, SqliteStorage, Storage};
use tokio::net::TcpListener;

use crate::support::{ProcessEnvGuard, scratch_tempdir};

struct StubProvider;

#[async_trait::async_trait]
impl LlmProvider for StubProvider {
    fn model_id(&self) -> &str {
        "stub"
    }
    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unreachable!("metadata proof does not call a provider")
    }
    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("metadata proof does not call a provider")
    }
    fn stream(
        &self,
        _: CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        unreachable!("metadata proof does not call a provider")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_metadata_and_explicit_updates_persist_through_live_gateway_requests() {
    let root = scratch_tempdir();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let space = storage.get_or_create_default().await.unwrap();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    let base = format!("http://{address}");
    let client = reqwest::Client::new();
    let created: Value = client.post(format!("{base}/api/spaces/{}/workflow-scripts", space.id)).json(&json!({"name":"request-name","description":"request description","source":"export const meta = { enabled: false, empty: null, retries: 3, name: \"header-name\", description: \"header description\" }; return \"created\";","delivery_grant":null})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(created["name"], "header-name");
    assert_eq!(created["description"], "header description");
    let updated: Value = client.put(format!("{base}/api/workflow-scripts/{}", created["id"].as_str().unwrap())).json(&json!({"description":"ignored request description","source":"export const meta = { enabled: false, empty: null, retries: 4, name: \"updated-header-name\", description: \"updated header description\" }; return \"updated\";","delivery_grant":null})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(
        updated["name"], "header-name",
        "source updates retain the saved name"
    );
    assert_eq!(updated["description"], "ignored request description");
    assert_eq!(updated["head_version"], 2);
    let reloaded: Value = client
        .get(format!(
            "{base}/api/workflow-scripts/{}",
            created["id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        reloaded, updated,
        "saved workflow fields survive a fresh gateway read"
    );
    server.abort();
}
