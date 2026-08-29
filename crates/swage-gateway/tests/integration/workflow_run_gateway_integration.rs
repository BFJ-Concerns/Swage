#![allow(clippy::unwrap_used)]

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    NewWorkspaceAttachment, SpaceStore, SqliteStorage, Storage, WorkspaceAttachmentStore,
};
use tokio::net::TcpListener;
use tokio::process::Command;
use uuid::Uuid;

use crate::support::{ProcessEnvGuard, scratch_tempdir};

struct PlacementProvider;

#[async_trait::async_trait]
impl LlmProvider for PlacementProvider {
    fn model_id(&self) -> &str {
        "placement-scripted"
    }

    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unreachable!("workflow calls stream")
    }

    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("workflow calls stream")
    }

    fn stream(
        &self,
        _: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter([
            StreamChunk::Delta {
                content: "placed worker completed".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]))
    }
}

async fn git(repo: &std::path::Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .await
            .unwrap()
            .success(),
        "git {args:?}"
    );
}

struct ResearchSweepProvider {
    stage_prompts: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LlmProvider for ResearchSweepProvider {
    fn model_id(&self) -> &str {
        "research-sweep-scripted"
    }

    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unreachable!("workflow calls stream")
    }

    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("workflow calls stream")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let prompt = request
            .messages
            .iter()
            .map(swage_core::ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n");
        self.stage_prompts.lock().unwrap().push(prompt.clone());
        let landed_id = Uuid::from_u128(1);
        let output = if prompt.contains("You are scoping a research sweep.") {
            r#"{"sources":[{"url":"https://example.test/landed","why":"primary"},{"url":"https://example.test/failed","why":"comparison"}]}"#.to_owned()
        } else if prompt.contains("Source: https://example.test/landed") {
            format!(
                r#"{{"url":"https://example.test/landed","recorded":true,"observation_id":"{landed_id}","note":"recorded"}}"#
            )
        } else if prompt.contains("Source: https://example.test/failed") {
            r#"{"url":"https://example.test/failed","recorded":false,"observation_id":null,"note":"unreachable"}"#.to_owned()
        } else if prompt.contains("Organise them into the space's knowledge base.") {
            r#"{"orientation_cards":0,"assertions":0,"note":"Nothing further to organise."}"#
                .to_owned()
        } else if prompt.contains("Deliver research findings.") {
            format!(
                r#"{{"findings":[{{"claim":"The landed source supports the purpose.","citation_id":"{landed_id}"}}],"withheld":[{{"claim":"The comparison is conclusive.","reason":"The comparison source was unreachable."}}]}}"#
            )
        } else {
            panic!("unexpected research-sweep stage: {prompt}");
        };
        Box::pin(futures::stream::iter([
            StreamChunk::Delta { content: output },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_workflow_runs_through_the_gateway_engine_and_persists_its_final_output() {
    let root = scratch_tempdir();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let repo = root.path().join("placement-repo");
    tokio::fs::create_dir(&repo).await.unwrap();
    git(&repo, &["init"]).await;
    git(&repo, &["config", "user.email", "test@example.invalid"]).await;
    git(&repo, &["config", "user.name", "Test"]).await;
    tokio::fs::write(repo.join("seed"), "seed").await.unwrap();
    git(&repo, &["add", "seed"]).await;
    git(&repo, &["commit", "-m", "seed"]).await;
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let space = storage.get_or_create_default().await.unwrap();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            space_id: space.id,
            label: "placement repository".into(),
            path: repo.to_string_lossy().into_owned(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .unwrap();
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::new(PlacementProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        root.path().to_path_buf(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    let base = format!("http://{address}");
    let client = reqwest::Client::new();
    let cwd = serde_json::to_string(&repo.to_string_lossy()).unwrap();
    let source = format!(
        r#"await agent("exercise placement listing", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
        const placements = await workerPlacements();
        return {{ placements }};"#
    );
    let script: Value = client.post(format!("{base}/api/spaces/{}/workflow-scripts", space.id)).json(&json!({"name":"final output","description":"engine output","source":source,"delivery_grant":null})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let started: Value = client
        .post(format!(
            "{base}/api/workflow-scripts/{}/runs",
            script["id"].as_str().unwrap()
        ))
        .json(&json!({"args":null,"budget":null}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = started["id"].as_str().unwrap();
    let detail = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail: Value = client
                .get(format!("{base}/api/workflow-runs/{run_id}"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["run"]["state"] == "completed" {
                break detail;
            }
            assert_ne!(detail["run"]["state"], "failed", "{detail:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let paths = detail["run"]["final_output"]["placements"]["paths"]
        .as_array()
        .unwrap();
    assert_eq!(paths.len(), 1, "the listing must retain the placed worker");
    assert!(
        paths[0]
            .as_str()
            .unwrap()
            .contains(&format!("worker-worktrees/{run_id}/worker-0")),
        "the final output must carry the engine's provisioned placement: {paths:?}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn shipped_research_sweep_runs_valid_arguments_through_each_staged_role() {
    let root = scratch_tempdir();
    let mut environment = ProcessEnvGuard::new();
    environment.set_swage_home(root.path());
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let space = storage.get_or_create_default().await.unwrap();
    let provider = Arc::new(ResearchSweepProvider {
        stage_prompts: Mutex::new(vec![]),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
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
    let scripts: Value = client
        .get(format!("{base}/api/spaces/{}/workflow-scripts", space.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let script_id = scripts["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|script| script["name"] == "research-sweep")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let started: Value = client.post(format!("{base}/api/workflow-scripts/{script_id}/runs")).json(&json!({"args":{"topic":"contract evidence","purpose":"prove the staged research contract"},"budget":null})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let run_id = started["id"].as_str().unwrap();
    let detail = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail: Value = client
                .get(format!("{base}/api/workflow-runs/{run_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["run"]["state"] == "completed" {
                break detail;
            }
            assert_ne!(detail["run"]["state"], "failed", "{detail:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(detail["run"]["final_output"]["topic"], "contract evidence");
    assert_eq!(
        detail["run"]["final_output"]["failed_sources"][0]["url"],
        "https://example.test/failed"
    );
    let prompts = provider.stage_prompts.lock().unwrap();
    assert!(
        prompts
            .iter()
            .any(|prompt| prompt.contains("scoping a research sweep"))
    );
    assert!(
        prompts
            .iter()
            .any(|prompt| prompt.contains("Gather one source"))
    );
    assert!(
        prompts
            .iter()
            .any(|prompt| prompt.contains("Organise them"))
    );
    assert!(
        prompts
            .iter()
            .any(|prompt| prompt.contains("Deliver research findings"))
    );
    server.abort();
}
