#![allow(clippy::unwrap_used)]

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState, tool::knowledge::KnowledgeRecordTool};
use swage_provider::LlmProvider;
use swage_storage::{ObservationStore, SpaceStore, SqliteStorage, Storage, WorkflowStore};
use tokio::net::TcpListener;

use crate::support::scratch_tempdir;

struct InterruptedKnowledgeProvider {
    rounds: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for InterruptedKnowledgeProvider {
    fn model_id(&self) -> &str {
        "interrupted-knowledge-scripted"
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
        match self.rounds.fetch_add(1, Ordering::SeqCst) {
            0 => Box::pin(futures::stream::iter([
                StreamChunk::ToolUse {
                    call_id: "first-capture".into(),
                    name: "knowledge_record".into(),
                    arguments: json!({
                        "url": "https://example.test/interrupted",
                        "content": "first captured body",
                        "extracted_text": "first extracted text",
                    }),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ])),
            // The first worker has executed its tool but has not settled its
            // journalled agent call when the durable run is interrupted.
            1 => Box::pin(futures::stream::pending()),
            2 => Box::pin(futures::stream::iter([
                StreamChunk::ToolUse {
                    call_id: "resumed-capture".into(),
                    name: "knowledge_record".into(),
                    arguments: json!({
                        "url": "https://example.test/interrupted",
                        "content": "resumed captured body",
                        "extracted_text": "resumed extracted text",
                    }),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ])),
            3 => Box::pin(futures::stream::iter([
                StreamChunk::Delta {
                    content: "capture retained".into(),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop),
                },
            ])),
            round => panic!("unexpected provider round {round}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_knowledge_record_worker_resumes_and_deduplicates_its_capture_over_http() {
    let root = scratch_tempdir();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let space = storage.get_or_create_default().await.unwrap();
    let provider = Arc::new(InterruptedKnowledgeProvider {
        rounds: AtomicUsize::new(0),
    });
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![
            Arc::new(KnowledgeRecordTool) as Arc<dyn swage_gateway::tool::Tool>
        ]),
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
    let started: Value = client
        .post(format!("{base}/api/spaces/{}/workflow-runs", space.id))
        .json(&json!({
            "source": "return await agent('Record the source', { agent: 'researcher', tools: ['knowledge_record'], maxAttempts: 1 });",
            "args": null,
            "budget": null,
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
    let run_uuid = run_id.parse().unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let observations = storage.list_observations_by_space(space.id).await.unwrap();
            let journal = storage.list_workflow_journal(run_uuid).await.unwrap();
            if observations.len() == 1
                && journal.len() == 1
                && journal[0].kind == "agent_call"
                && journal[0].status == "started"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first attempt should record knowledge before settling its agent call");

    storage
        .update_workflow_run_state(run_uuid, "interrupted", None, None, None)
        .await
        .unwrap();
    let resumed: Value = client
        .post(format!("{base}/api/workflow-runs/{run_id}/resume"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed["state"], "running");

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
    .expect("the resumed run should complete");

    assert_eq!(detail["run"]["final_output"], "capture retained");
    assert_eq!(provider.rounds.load(Ordering::SeqCst), 4);
    let observations = storage.list_observations_by_space(space.id).await.unwrap();
    assert_eq!(
        observations.len(),
        1,
        "both attempts share the journalled call scope and retain one observation"
    );
    assert_eq!(
        observations[0].body_bytes,
        "first captured body".len() as u64
    );
    let journal = storage.list_workflow_journal(run_uuid).await.unwrap();
    let agent_calls = journal
        .iter()
        .filter(|entry| entry.kind == "agent_call")
        .collect::<Vec<_>>();
    assert_eq!(agent_calls.len(), 1);
    assert_eq!(agent_calls[0].status, "completed");
    server.abort();
}
