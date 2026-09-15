#![allow(clippy::unwrap_used)]

use std::{pin::Pin, sync::Arc, time::Duration};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{SqliteStorage, Storage, TopicStore, WorkflowStore};

use crate::support::{ProcessEnvGuard, start_live_server};

struct StubProvider;

#[async_trait::async_trait]
impl LlmProvider for StubProvider {
    fn model_id(&self) -> &str {
        "stub"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("workflow proof does not call a provider")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("workflow proof does not call a provider")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        unimplemented!("workflow proof does not call a provider")
    }
}

async fn wait_for_run(client: &reqwest::Client, base: &str, run_id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = client
                .get(format!("{base}/api/workflow-runs/{run_id}"))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            let detail: Value = response.json().await.unwrap();
            if detail["run"]["state"] != "running" {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("workflow run should finish")
}

#[tokio::test(flavor = "multi_thread")]
async fn carried_workflow_runs_over_http_and_authored_source_shadows_it() {
    let root = tempfile::tempdir().unwrap();
    let skill_dir = root.path().join("skills/operator-proof");
    std::fs::create_dir_all(skill_dir.join("workflows")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Operator carried workflow proof.\n---\n\nInstructions.\n",
    )
    .unwrap();
    let carried_source = "return { source: 'carried' };\n";
    std::fs::write(skill_dir.join("workflows/proof.js"), carried_source).unwrap();

    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (address, server) = start_live_server(build_router(state)).await;
    let base = format!("http://{address}");
    let client = reqwest::Client::new();

    let list: Value = client
        .get(format!("{base}/api/topics/{}/workflow-scripts", topic.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let carried = list["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|script| script["name"] == "proof")
        .expect("carried workflow should be discovered");
    let carried_id = carried["id"].as_str().unwrap();
    let started: Value = client
        .post(format!("{base}/api/workflow-scripts/{carried_id}/runs"))
        .json(&json!({"args": null, "budget": null}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let carried_detail = wait_for_run(&client, &base, started["id"].as_str().unwrap()).await;
    assert_eq!(
        carried_detail["run"]["final_output"],
        json!({"source": "carried"})
    );
    assert_eq!(carried_detail["run"]["carrying_skill"], "operator-proof");
    let carried_record = storage
        .get_workflow_run(started["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(carried_record.source.as_deref(), Some(carried_source));
    assert_eq!(
        carried_record.source_hash,
        Sha256::digest(carried_source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );

    let authored: Value = client
        .post(format!("{base}/api/topics/{}/workflow-scripts", topic.id))
        .json(&json!({
            "name": "proof",
            "description": "Authored shadow",
            "source": "return { source: 'authored' };",
            "delivery_grant": null
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let shadowed_list: Value = client
        .get(format!("{base}/api/topics/{}/workflow-scripts", topic.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let effective_proof = shadowed_list["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|script| script["name"] == "proof")
        .unwrap();
    assert_eq!(effective_proof["id"], authored["id"]);
    let shadowed: Value = client
        .post(format!(
            "{base}/api/workflow-scripts/{}/runs",
            authored["id"].as_str().unwrap()
        ))
        .json(&json!({"args": null, "budget": null}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let shadowed_detail = wait_for_run(&client, &base, shadowed["id"].as_str().unwrap()).await;
    assert_eq!(
        shadowed_detail["run"]["final_output"],
        json!({"source": "authored"})
    );
    assert!(shadowed_detail["run"]["carrying_skill"].is_null());

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_run_rejects_operator_events_without_adding_to_the_queue() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (address, server) = start_live_server(build_router(state)).await;
    let base = format!("http://{address}");
    let client = reqwest::Client::new();

    let started: Value = client
        .post(format!("{base}/api/topics/{}/workflow-runs", topic.id))
        .json(&json!({"source": "return { done: true };", "args": null, "budget": null}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = started["id"].as_str().unwrap();
    let detail = wait_for_run(&client, &base, run_id).await;
    assert_eq!(detail["run"]["state"], "completed");

    let rejected = client
        .post(format!("{base}/api/workflow-runs/{run_id}/events"))
        .json(&json!({"kind": "operator.decision", "payload": {"verdict": "accept"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        storage
            .list_workflow_run_events(run_id.parse().unwrap())
            .await
            .unwrap()
            .len(),
        0,
        "a terminal run must not gain a queued operator event"
    );

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn inline_run_keeps_anonymous_identity_in_its_public_projection() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (address, server) = start_live_server(build_router(state)).await;
    let base = format!("http://{address}");
    let client = reqwest::Client::new();

    let started: Value = client
        .post(format!("{base}/api/topics/{}/workflow-runs", topic.id))
        .json(&json!({"source": "return { done: true };", "args": null, "budget": null}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = started["id"].as_str().unwrap();
    assert!(started["script_id"].is_null());
    assert!(started["script_name"].is_null());
    assert!(started["script_version"].is_null());

    let detail = wait_for_run(&client, &base, run_id).await;
    assert!(detail["run"]["script_id"].is_null());
    assert!(detail["run"]["script_name"].is_null());
    assert!(detail["run"]["script_version"].is_null());
    assert_eq!(detail["run"]["source"], "return { done: true };");

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_run_rejects_operator_events_without_adding_to_the_queue() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (address, server) = start_live_server(build_router(state)).await;
    let base = format!("http://{address}");
    let client = reqwest::Client::new();

    let started: Value = client
        .post(format!("{base}/api/topics/{}/workflow-runs", topic.id))
        .json(&json!({
            "source": "return await wait({ timerMs: 120000 });",
            "args": null,
            "budget": null
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = started["id"].as_str().unwrap();
    let parked = wait_for_run(&client, &base, run_id).await;
    assert_eq!(parked["run"]["state"], "parked");

    let cancelled: Value = client
        .post(format!("{base}/api/workflow-runs/{run_id}/cancel"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["state"], "cancelled");

    let rejected = client
        .post(format!("{base}/api/workflow-runs/{run_id}/events"))
        .json(&json!({"kind": "operator.decision", "payload": {"verdict": "accept"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
    assert!(
        storage
            .list_workflow_run_events(run_id.parse().unwrap())
            .await
            .unwrap()
            .is_empty(),
        "a cancelled run must not gain a queued operator event"
    );

    server.abort();
}
