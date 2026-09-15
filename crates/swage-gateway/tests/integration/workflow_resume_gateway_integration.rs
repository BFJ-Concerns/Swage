#![allow(clippy::unwrap_used)]

use std::{
    net::TcpListener as StdTcpListener,
    pin::Pin,
    process::Command,
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
use swage_storage::{
    NewWorkflowRun, ObservationStore, SqliteStorage, Storage, TopicStore, WorkflowStore,
};
use tokio::net::TcpListener;

use crate::support::scratch_tempdir;

struct InterruptedKnowledgeProvider {
    rounds: AtomicUsize,
}

fn crashed_workflow_run(
    topic_id: uuid::Uuid,
    source: Option<&str>,
    source_hash: &str,
) -> NewWorkflowRun {
    NewWorkflowRun {
        topic_id,
        parent_run_id: None,
        parent_seq: None,
        script_version_id: None,
        inline_workflow_id: None,
        inline_workflow_name: None,
        inline_workflow_version: None,
        carrying_skill: None,
        task: None,
        source: source.map(str::to_owned),
        source_hash: source_hash.into(),
        args: Value::Null,
        budget: json!({}),
        engine_version: "test".into(),
        dispatch_bound_agent_ids: vec![],
        dispatch_bound_task_definition_ids: vec![],
        workflow_call_tools: None,
        inherited_worker_placement: None,
    }
}

/// Journey: a gateway dies with running workflows -> the next gateway boot
/// marks and lists them -> its first wake-loop pass replays the resumable run.
///
/// Proves: startup.rs invokes crash recovery before starting the wake loop,
/// and the loop consumes the interrupted-run listing. One deliberately
/// unreplayable run remains interrupted, making the boot listing observable
/// after the valid run has replayed.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_boot_recovers_and_wakes_runs_left_running_by_a_crash() {
    let root = scratch_tempdir();
    let runtime = scratch_tempdir();
    let storage = Arc::new(SqliteStorage::open(&root.path().join("swage.db")).unwrap());
    let topic = storage.general_scope().await.unwrap();
    let replayable = storage
        .create_workflow_run(crashed_workflow_run(
            topic.id,
            Some("return null;"),
            "boot-recovery-replayable",
        ))
        .await
        .unwrap();
    let listed_after_recovery = storage
        .create_workflow_run(crashed_workflow_run(
            topic.id,
            None,
            "boot-recovery-unreplayable",
        ))
        .await
        .unwrap();

    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let start = Command::new(env!("CARGO_BIN_EXE_swage"))
        .env("SWAGE_HOME", root.path())
        .env("XDG_RUNTIME_DIR", runtime.path())
        .args(["gateway", "start", "--port", &port])
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "gateway startup failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&start.stdout),
        String::from_utf8_lossy(&start.stderr),
    );

    let recovery_result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let replayed = storage
                .get_workflow_run(replayable.id)
                .await
                .unwrap()
                .unwrap();
            let unreplayable = storage
                .get_workflow_run(listed_after_recovery.id)
                .await
                .unwrap()
                .unwrap();
            if replayed.state == "completed" && unreplayable.state == "interrupted" {
                let resumable = storage.list_interrupted_workflow_runs().await.unwrap();
                break (replayed, resumable);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;

    let stop = Command::new(env!("CARGO_BIN_EXE_swage"))
        .env("SWAGE_HOME", root.path())
        .env("XDG_RUNTIME_DIR", runtime.path())
        .args(["gateway", "stop", "--force"])
        .output()
        .unwrap();
    assert!(
        stop.status.success(),
        "gateway shutdown failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr),
    );

    let (replayed, resumable) = recovery_result
        .expect("gateway boot should mark and list abandoned runs before replaying the valid one");
    assert_eq!(replayed.state, "completed");
    assert!(
        resumable
            .iter()
            .any(|run| run.id == listed_after_recovery.id),
        "the boot wake loop's interrupted-run listing should retain the recovered run"
    );
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

/// Journey: gateway crash leaves a workflow running -> boot recovery marks and
/// lists it -> the live gateway resumes its journalled work over HTTP.
///
/// Proves: the startup recovery transition produces the exact interrupted-run
/// work list the boot wake loop consumes. A regression in either marking or
/// listing leaves the HTTP resume path unreachable.
#[tokio::test(flavor = "multi_thread")]
async fn boot_recovery_marks_lists_and_resumes_an_interrupted_knowledge_worker_over_http() {
    let root = scratch_tempdir();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    let provider = Arc::new(InterruptedKnowledgeProvider {
        rounds: AtomicUsize::new(0),
    });
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
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
        .post(format!("{base}/api/topics/{}/workflow-runs", topic.id))
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
            let observations = storage.list_observations_by_topic(topic.id).await.unwrap();
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

    // This is the same durable recovery operation startup.rs runs before it
    // spawns the workflow wake loop. The original request remains in-flight,
    // exactly as it would after the gateway process dies.
    assert_eq!(
        storage.interrupt_running_workflow_runs().await.unwrap(),
        1,
        "boot recovery should mark the abandoned running workflow"
    );
    let resumable_runs = storage.list_interrupted_workflow_runs().await.unwrap();
    assert!(
        resumable_runs.iter().any(|run| run.id == run_uuid),
        "the boot wake loop's interrupted-run listing should include the recovered workflow"
    );
    let recovered = storage.get_workflow_run(run_uuid).await.unwrap().unwrap();
    assert_eq!(recovered.state, "interrupted");
    assert!(
        recovered.completed_at.is_none(),
        "startup recovery must leave the replayable run non-terminal"
    );

    // Exercise the same replay operation that the wake loop applies to each
    // item from that listing, through the gateway's public resume endpoint.
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
    let observations = storage.list_observations_by_topic(topic.id).await.unwrap();
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
