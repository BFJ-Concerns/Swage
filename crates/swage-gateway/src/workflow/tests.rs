/// Create a committed local repository for worktree and placement tests.
fn seed_git_repo(repo: &std::path::Path) {
    std::fs::create_dir(repo).expect("create repository");
    for args in [
        &["init"][..],
        &["config", "user.email", "test@example.invalid"][..],
        &["config", "user.name", "Test"][..],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .status()
                .expect("configure repository")
                .success()
        );
    }
    std::fs::write(repo.join("seed"), "seed").expect("write seed");
    for args in [&["add", "seed"][..], &["commit", "-m", "seed"][..]] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .status()
                .expect("commit seed")
                .success()
        );
    }
}
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use futures::StreamExt;
use swage_protocol::client::workflow::{WorkflowJournalEntryKind, WorkflowJournalEntryStatus};
use swage_storage::{
    AgentDefinitionStore, AuditStore, CitationStore, ContextSnapshotStore, NewSourceObservation,
    NewWorkflowScript, NewWorkspaceAttachment, ObservationOrigin, ObservationStore,
    OrientationCardStore, ProviderRoundStore, SessionStore, SqliteStorage, Storage,
    TaskDefinitionStore, ThreadState, ThreadStore, ToolCallStore, TopicStore, TurnState, TurnStore,
    WorkflowStore, WorkspaceAttachmentStore,
};
use swage_workflow::AgentSessionRef;

/// A run and a started journal entry for a worker call, as the host
/// would have created them before dispatching one.
///
/// A call's dispatch record attaches to its journal entry, so a fixture
/// driving `run_ephemeral_agent_call` needs a real entry rather than a
/// fresh UUID standing in for one.
async fn journalled_agent_call(storage: &SqliteStorage, topic_id: Uuid) -> (Uuid, Uuid) {
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some("agent call fixture".into()),
            source_hash: source_hash("agent call fixture"),
            ..swage_storage::fixtures::new_workflow_run(topic_id)
        })
        .await
        .expect("run");
    let request = serde_json::json!({"kind": "agent_call"});
    let entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: swage_workflow::journal_args_hash(&request),
            request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("started entry");
    (run.id, entry.id)
}

#[test]
fn schema_exhaustion_marker_is_decoded_only_for_replayed_failures() {
    let marker =
        WorkflowHostError::Message("SchemaExhaustedError: recorded schema failure".to_string());

    let live = classify_agent_call_failure(marker.clone(), "live-call".into(), false);
    assert!(matches!(live, WorkflowHostError::AgentCall { .. }));

    let replayed = classify_agent_call_failure(marker, "replayed-call".into(), true);
    assert!(matches!(
        replayed,
        WorkflowHostError::SchemaExhausted { .. }
    ));
}

/// The research sweep as the shipped research skill carries it.
fn research_sweep_source() -> &'static str {
    crate::shipped_skills::workflow_files("research")
        .find(|file| file.path.ends_with("/research-sweep.js"))
        .expect("embedded research sweep")
        .contents
}

/// The carried workflow's per-topic identity, as listing assigns it.
fn research_sweep_id(topic_id: Uuid) -> Uuid {
    crate::synthetic_id::synthetic_id(
        CARRIED_WORKFLOW_ID_DOMAIN,
        topic_id,
        "research/research-sweep",
    )
}

fn workflow_test_state() -> (AppState, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (state, storage)
}

async fn grant_workspace(storage: &SqliteStorage, topic_id: Uuid, path: &std::path::Path) -> Uuid {
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            topic_id,
            label: "test workspace".into(),
            path: path.to_string_lossy().into_owned(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .expect("grant test workspace")
        .id
}

async fn store_test_agent(storage: &SqliteStorage, topic_id: Uuid, agent_id: &str) {
    storage
        .upsert_agent_definition(swage_storage::NewAgentDefinition {
            id: agent_id.into(),
            topic_id: Some(topic_id),
            display_name: "Workflow test agent".into(),
            description: String::new(),
            instructions: String::new(),
            model_id: None,
            tool_permissions: None,
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store workflow test agent definition");
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_topic_lists_every_shipped_carried_workflow_and_nothing_else() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);

    let Json(list) = list_scripts(State(state), Path(topic.id))
        .await
        .expect("workflow list");

    // Nothing ships as a global built-in: every listed workflow on a
    // fresh topic names the shipped skill that carries it.
    let carried = list
        .scripts
        .iter()
        .map(|script| {
            (
                script.name.as_str(),
                script.carrying_skill.as_deref().expect("a carrying skill"),
            )
        })
        .collect::<Vec<_>>();
    assert!(carried.contains(&("research-sweep", "research")));
    assert!(carried.contains(&("build-verify", "assembly")));
    assert!(carried.contains(&("teardown-audit", "maestro")));
    assert!(
        list.scripts
            .iter()
            .all(|script| script.topic_id == topic.id)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn authored_workflow_shadows_the_same_named_shipped_workflow() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let router = crate::server::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test gateway");
    let address = listener.local_addr().expect("test gateway address");
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve test gateway");
    });
    let client = reqwest::Client::new();
    let base_url = format!("http://{address}");

    let initial_list = client
        .get(format!(
            "{base_url}/api/topics/{}/workflow-scripts",
            topic.id
        ))
        .send()
        .await
        .expect("list built-ins over HTTP")
        .error_for_status()
        .expect("built-in listing response")
        .json::<Value>()
        .await
        .expect("serialise built-in listing");
    let shipped = initial_list["scripts"]
        .as_array()
        .expect("script array")
        .iter()
        .find(|script| script["name"] == "research-sweep")
        .expect("shipped research-sweep");
    let shipped_id = shipped["id"].as_str().expect("shipped id").to_string();
    assert_eq!(shipped["carrying_skill"], "research");

    let authored = client
        .post(format!(
            "{base_url}/api/topics/{}/workflow-scripts",
            topic.id
        ))
        .json(&serde_json::json!({
            "name": "research-sweep",
            "description": "Operator variant",
            "source": "return 'operator';",
            "delivery_grant": null
        }))
        .send()
        .await
        .expect("create authored shadow over HTTP")
        .error_for_status()
        .expect("authored shadow response")
        .json::<Value>()
        .await
        .expect("serialise authored shadow");
    let authored_id = authored["id"].as_str().expect("authored id");
    assert_ne!(authored_id, shipped_id);
    assert!(authored["carrying_skill"].is_null());

    let hidden_read = client
        .get(format!("{base_url}/api/workflow-scripts/{shipped_id}"))
        .send()
        .await
        .expect("read hidden shipped workflow over HTTP");
    assert_eq!(hidden_read.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        hidden_read
            .json::<Value>()
            .await
            .expect("serialise hidden read rejection")["code"],
        "not_found"
    );

    let hidden_start = client
        .post(format!("{base_url}/api/workflow-scripts/{shipped_id}/runs"))
        .json(&serde_json::json!({ "args": null, "budget": null }))
        .send()
        .await
        .expect("start hidden shipped workflow over HTTP");
    assert_eq!(hidden_start.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        hidden_start
            .json::<Value>()
            .await
            .expect("serialise hidden start rejection")["code"],
        "not_found"
    );

    let read = client
        .get(format!("{base_url}/api/workflow-scripts/{authored_id}"))
        .send()
        .await
        .expect("read authored winner over HTTP")
        .error_for_status()
        .expect("authored winner response")
        .json::<Value>()
        .await
        .expect("serialise authored winner");
    assert_eq!(read["id"], authored_id);
    assert_eq!(read["name"], "research-sweep");
    assert!(read["carrying_skill"].is_null());

    let started = client
        .post(format!(
            "{base_url}/api/workflow-scripts/{authored_id}/runs"
        ))
        .json(&serde_json::json!({ "args": null, "budget": null }))
        .send()
        .await
        .expect("start authored winner over HTTP")
        .error_for_status()
        .expect("authored winner launch response")
        .json::<Value>()
        .await
        .expect("serialise authored winner launch");
    assert_eq!(started["script_id"], authored_id);
    assert_eq!(started["script_name"], "research-sweep");
    assert_eq!(started["script_version"], 1);
    let started_id = Uuid::parse_str(started["id"].as_str().expect("started run id"))
        .expect("valid started run id");
    wait_for_terminal(&state, started_id).await;
    let run = storage
        .get_workflow_run(started_id)
        .await
        .expect("run query")
        .expect("run");
    assert_eq!(
        run.script_id.map(|id| id.to_string()).as_deref(),
        Some(authored_id)
    );
    assert_eq!(run.script_name.as_deref(), Some("research-sweep"));
    assert_eq!(run.script_version, Some(1));
    assert_eq!(run.source, None);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_carried_workflow_shadows_the_same_named_shipped_workflow_when_listed_and_started()
{
    let root = tempfile::tempdir().expect("skill root");
    let skill_dir = root.path().join("skills/research-shadow");
    std::fs::create_dir_all(skill_dir.join("workflows")).expect("workflow directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Carried built-in shadow proof.\n---\n\nInstructions.\n",
    )
    .expect("skill definition");
    let carried_source = "return { source: 'carried-shadow' };\n";
    std::fs::write(
        skill_dir.join("workflows/research-sweep.js"),
        carried_source,
    )
    .expect("carried workflow");

    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::at(topic.id, root.path());

    let Json(list) = list_scripts(State(state.clone()), Path(topic.id))
        .await
        .expect("workflow list");
    let listed = list
        .scripts
        .into_iter()
        .find(|script| script.name == "research-sweep")
        .expect("research sweep");
    assert_eq!(listed.carrying_skill.as_deref(), Some("research-shadow"));

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(listed.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start carried built-in shadow");
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run query")
        .expect("run");
    assert_eq!(
        run.final_output,
        Some(serde_json::json!({"source": "carried-shadow"}))
    );
    assert_eq!(run.source.as_deref(), Some(carried_source));
    assert_eq!(run.carrying_skill.as_deref(), Some("research-shadow"));
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_carried_workflow_metadata_does_not_hide_valid_workflows() {
    let root = tempfile::tempdir().expect("skill root");
    let skill_dir = root.path().join("skills/invalid-meta");
    std::fs::create_dir_all(skill_dir.join("workflows")).expect("workflow directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Invalid metadata proof.\n---\n\nInstructions.\n",
    )
    .expect("skill definition");
    std::fs::write(
        skill_dir.join("workflows/invalid.js"),
        r#"export const meta = { argumentSchema: { type: "object" } }; return {};"#,
    )
    .expect("carried workflow");
    std::fs::write(
        skill_dir.join("workflows/valid.js"),
        "return { valid: true };",
    )
    .expect("carried workflow");

    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::at(topic.id, root.path());

    let Json(list) = list_scripts(State(state), Path(topic.id))
        .await
        .expect("valid carried workflows remain listable");
    assert!(
        list.scripts.iter().any(|script| script.name == "valid"),
        "the valid carried workflow must remain listed: {list:?}"
    );
    assert!(
        !list.scripts.iter().any(|script| script.name == "invalid"),
        "the malformed carried workflow must be excluded: {list:?}"
    );
    let audit = storage
        .list_audit_events()
        .await
        .expect("carried-workflow audit records");
    assert!(audit.iter().any(|event| {
        event.entity_kind == "workflow_script"
            && event.event_kind == "excluded"
            && event.metadata["skill"] == "invalid-meta"
            && event.metadata["workflow"] == "invalid"
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_run_rejects_invalid_arguments_before_creating_a_run() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "constrained".into(),
            description: String::new(),
            source: r#"export const meta = { argumentSchema: {"type":"object","required":["task"],"properties":{"task":{"type":"string"}}} }; return {};"#.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("workflow");

    let error = start_saved_run(
        State(state),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: serde_json::json!({"task": 42}),
            budget: None,
        }),
    )
    .await
    .expect_err("invalid arguments must be rejected");
    assert!(
        error.message().contains("/task"),
        "validation must name the failing field: {error:?}"
    );
    assert!(
        storage
            .list_workflow_runs(topic.id)
            .await
            .expect("runs")
            .is_empty(),
        "rejected arguments must not create a run"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shipped_research_sweep_can_be_read_and_started_but_not_updated() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let Json(list) = list_scripts(State(state.clone()), Path(topic.id))
        .await
        .expect("workflow list");
    let listed = list
        .scripts
        .into_iter()
        .find(|script| script.name == "research-sweep")
        .expect("research sweep");

    let Json(read) = get_script(State(state.clone()), Path(listed.id))
        .await
        .expect("read shipped workflow");
    assert_eq!(read.id, listed.id);
    assert_eq!(read.carrying_skill.as_deref(), Some("research"));

    let update_error = update_script(
        State(state.clone()),
        Path(listed.id),
        Json(UpdateWorkflowScriptRequest {
            description: Some("changed".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("shipped update must fail");
    assert!(
        matches!(update_error.cause(), Some(swage_core::GatewayError::TopicConflict(message)) if message.contains("immutable"))
    );

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(listed.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start shipped research sweep");
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run query")
        .expect("run");
    let embedded = research_sweep_source();
    assert_eq!(run.engine_version, "rquickjs-0.12.2");
    assert_eq!(run.state, "failed");
    assert_eq!(run.script_version_id, None);
    assert_eq!(run.script_id, Some(listed.id));
    assert_eq!(run.script_name.as_deref(), Some("research-sweep"));
    assert_eq!(run.script_version, Some(1));
    assert_eq!(run.carrying_skill.as_deref(), Some("research"));
    assert_eq!(run.source.as_deref(), Some(embedded));
    assert_eq!(run.source_hash, source_hash(embedded));
}

#[tokio::test(flavor = "multi_thread")]
async fn updating_an_absent_workflow_script_returns_not_found() {
    let (state, _) = workflow_test_state();
    let missing = Uuid::new_v4();

    let error = update_script(
        State(state),
        Path(missing),
        Json(UpdateWorkflowScriptRequest {
            description: Some("still absent".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("an absent workflow script must remain absent");

    assert_eq!(error.status(), axum::http::StatusCode::NOT_FOUND);
    assert_eq!(
        error.code(),
        swage_protocol::client::error::ErrorCode::NotFound
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn listed_shipped_carried_workflow_can_start_but_not_update() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let Json(list) = list_scripts(State(state.clone()), Path(topic.id))
        .await
        .expect("workflow list");
    let listed = list
        .scripts
        .into_iter()
        .find(|script| script.name == "build-verify")
        .expect("shipped carried workflow");
    assert_eq!(listed.carrying_skill.as_deref(), Some("assembly"));

    let update_error = update_script(
        State(state.clone()),
        Path(listed.id),
        Json(UpdateWorkflowScriptRequest {
            source: Some("return 'changed';".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("carried update must fail");
    assert!(
        matches!(update_error.cause(), Some(swage_core::GatewayError::TopicConflict(message)) if message.contains("immutable"))
    );

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(listed.id),
        Json(StartWorkflowRunRequest {
            args: serde_json::json!({
                "briefPath": "/workspace/brief.md",
                "worktree": "/workspace",
                "commissionPath": "/workspace/commission.md",
                "spinePath": "/workspace/spine.md",
                "verificationPath": "/workspace/verification.md",
                "unitDir": "/workspace/unit",
                "mergePolicy": "direct-merge",
                "annexeConfigPath": "/workspace/config.toml"
            }),
            budget: None,
        }),
    )
    .await
    .expect("start shipped carried workflow");
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run query")
        .expect("run");
    let embedded = crate::shipped_skills::workflow_files("assembly")
        .find(|file| file.path.ends_with("/build-verify.js"))
        .expect("embedded build verify")
        .contents;
    assert_eq!(run.carrying_skill.as_deref(), Some("assembly"));
    assert_eq!(run.source.as_deref(), Some(embedded));
    assert_eq!(run.source_hash, source_hash(embedded));
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("workflow journal");
    assert!(
        journal.iter().any(|entry| {
            entry.kind == "phase" && entry.request == serde_json::json!({ "message": "restate" })
        }),
        "run: {run:?}; journal: {journal:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_tool_name_resolution_reaches_a_shipped_carried_workflow() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let session = storage
        .create_session(NewSession {
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("session");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(session.id);

    let started = GatewayWorkflowLauncher::new(state.clone())
        .start_by_name(session.id, "research-sweep", Value::Null, None, false)
        .await
        .expect("shipped name resolution");
    let run_id: Uuid = serde_json::from_value(started["id"].clone()).expect("run id");
    wait_for_terminal(&state, run_id).await;
    let run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run query")
        .expect("run");
    assert_eq!(run.script_id, Some(research_sweep_id(topic.id)));
    assert_eq!(run.script_name.as_deref(), Some("research-sweep"));
    assert_eq!(run.script_version, Some(1));
    assert_eq!(run.carrying_skill.as_deref(), Some("research"));
    assert_eq!(run.source.as_deref(), Some(research_sweep_source()));
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_composition_resolves_a_shipped_carried_child() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let (parent, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "parent".into(),
            description: String::new(),
            source: "return await workflow('research-sweep', {});".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("parent");

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(parent.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start parent");
    wait_for_terminal(&state, started.id).await;
    let parent_run = storage
        .get_workflow_run(started.id)
        .await
        .expect("parent query")
        .expect("parent run");
    assert_eq!(parent_run.state, "failed");
    assert!(
        !parent_run
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("was not found")
    );
    let child = storage
        .list_workflow_runs(topic.id)
        .await
        .expect("runs")
        .into_iter()
        .find(|run| run.parent_run_id == Some(started.id))
        .expect("shipped child run");
    assert_eq!(child.engine_version, "rquickjs-0.12.2");
    assert_eq!(child.script_id, Some(research_sweep_id(topic.id)));
    assert_eq!(child.script_name.as_deref(), Some("research-sweep"));
    assert_eq!(child.script_version, Some(1));
    assert_eq!(child.carrying_skill.as_deref(), Some("research"));
    assert_eq!(child.source.as_deref(), Some(research_sweep_source()));
    assert!(
        child
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("args.subject"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_skill_workflow_runs_by_name_and_authored_source_shadows_it() {
    const AGENT_ID: &str = "workflow-agent";
    let root = tempfile::tempdir().expect("skill root");
    let skill_dir = root.path().join("skills/operator-proof");
    std::fs::create_dir_all(skill_dir.join("workflows")).expect("workflow directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Operator carried workflow proof.\n---\n\nInstructions.\n",
    )
    .expect("skill definition");
    let carried_source = "return { source: 'carried' };\n";
    std::fs::write(skill_dir.join("workflows/proof.js"), carried_source).expect("carried workflow");

    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    store_test_agent(&storage, topic.id, AGENT_ID).await;
    let session = storage
        .create_session(NewSession {
            agent_id: AGENT_ID.into(),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("session");
    let _skill_root = crate::skill::TestSkillRootGuard::at(session.id, root.path());

    let available = crate::websocket::discover_available_for_session(&state, session.id)
        .await
        .expect("skill discovery");
    let listed = list_effective_workflows(&state, topic.id, &available)
        .await
        .expect("effective workflows");
    let carried = listed
        .iter()
        .find(|workflow| workflow.name == "proof")
        .expect("carried workflow listed");
    assert_eq!(carried.carrying_skill.as_deref(), Some("operator-proof"));
    assert!(carried.immutable);

    let started = GatewayWorkflowLauncher::new(state.clone())
        .start_by_name(session.id, "proof", Value::Null, None, false)
        .await
        .expect("start carried workflow without saving");
    let run_id: Uuid = serde_json::from_value(started["id"].clone()).expect("run id");
    wait_for_terminal(&state, run_id).await;
    let carried_run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run query")
        .expect("carried run");
    assert_eq!(
        carried_run.final_output,
        Some(serde_json::json!({"source": "carried"}))
    );
    assert_eq!(carried_run.source.as_deref(), Some(carried_source));
    assert_eq!(carried_run.source_hash, source_hash(carried_source));
    assert_eq!(
        carried_run.carrying_skill.as_deref(),
        Some("operator-proof")
    );
    let Json(detail) = get_run(State(state.clone()), Path(run_id))
        .await
        .expect("wire run detail");
    assert_eq!(detail.run.carrying_skill.as_deref(), Some("operator-proof"));

    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "proof".into(),
            description: "Authored shadow".into(),
            source: "return { source: 'authored' };".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("authored shadow");
    let shadowed = GatewayWorkflowLauncher::new(state.clone())
        .start_by_name(session.id, "proof", Value::Null, None, false)
        .await
        .expect("start authored shadow");
    let shadowed_id: Uuid =
        serde_json::from_value(shadowed["id"].clone()).expect("shadowed run id");
    wait_for_terminal(&state, shadowed_id).await;
    let shadowed_run = storage
        .get_workflow_run(shadowed_id)
        .await
        .expect("shadowed query")
        .expect("shadowed run");
    assert_eq!(
        shadowed_run.final_output,
        Some(serde_json::json!({"source": "authored"}))
    );
    assert_eq!(shadowed_run.carrying_skill, None);
    assert!(shadowed_run.source.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn composed_shipped_workflow_retains_embedded_source_and_skill_attribution() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let (parent, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "parent".into(),
            description: String::new(),
            source: "return await workflow('teardown-audit', {});".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("parent");

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(parent.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start parent");
    wait_for_terminal(&state, started.id).await;
    let child = storage
        .list_workflow_runs(topic.id)
        .await
        .expect("runs")
        .into_iter()
        .find(|run| run.parent_run_id == Some(started.id))
        .expect("shipped child run");
    let embedded = crate::shipped_skills::workflow_files("maestro")
        .find(|file| file.path.ends_with("/teardown-audit.js"))
        .expect("embedded teardown audit")
        .contents;
    assert_eq!(child.carrying_skill.as_deref(), Some("maestro"));
    assert_eq!(child.source.as_deref(), Some(embedded));
    assert_eq!(child.source_hash, source_hash(embedded));
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_carried_run_resumes_from_its_recorded_inline_source() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let recorded_source = "return { resumed: true };";
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            carrying_skill: Some("operator-proof".into()),
            source: Some(recorded_source.into()),
            source_hash: source_hash(recorded_source),
            budget: serde_json::to_value(WorkflowBudget::default()).expect("budget"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    storage
        .update_workflow_run_state(run.id, "interrupted", None, None, None)
        .await
        .expect("interrupt run");

    let Json(resumed) = resume_run(State(state.clone()), Path(run.id))
        .await
        .expect("resume carried run");
    wait_for_terminal(&state, resumed.id).await;
    let resumed = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("resumed run");
    assert_eq!(
        resumed.final_output,
        Some(serde_json::json!({"resumed": true}))
    );
    assert_eq!(resumed.source.as_deref(), Some(recorded_source));
    assert_eq!(resumed.source_hash, source_hash(recorded_source));
    assert_eq!(resumed.carrying_skill.as_deref(), Some("operator-proof"));
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_pre_change_agent_call_replays_before_schema_validation() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"return await agent("reuse recorded result", { schema: { type: 7 } });"#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(WorkflowBudget::default()).expect("budget"),
            engine_version: "pre-change".into(),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = AgentCallRequest {
        schema: Some(serde_json::json!({"type": 7})),
        max_attempts: 3,
        ..swage_workflow::fixtures::agent_call_request("reuse recorded result".into())
    };
    let request_json = serde_json::to_value(&request).expect("agent request");
    let recorded = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&request_json),
            request: request_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("recorded agent call");
    storage
        .complete_workflow_journal_entry(
            recorded.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("recorded before validation existed".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 1,
                    output_tokens: 1,
                },
                working_directory: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
                served_model: None,
                worktree: None,
                provider_route: None,
            })
            .expect("recorded result"),
        )
        .await
        .expect("complete recorded call");
    storage
        .update_workflow_run_state(run.id, "interrupted", None, None, None)
        .await
        .expect("interrupt run");

    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let Json(resumed) = resume_run(State(state.clone()), Path(run.id))
        .await
        .expect("resume pre-change run");
    wait_for_terminal(&state, resumed.id).await;
    let resumed = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("resumed run");
    assert_eq!(
        resumed.state, "completed",
        "run error: {:?}",
        resumed.error_message
    );
    assert_eq!(
        resumed.final_output,
        Some(Value::String("recorded before validation existed".into()))
    );
    assert!(
        provider.requests.lock().expect("requests").is_empty(),
        "a replayed journal result must not spawn a provider request"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_run_reapplies_its_persisted_dispatch_bound_after_a_grant_change() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let swage_home = crate::test_support::SwageHomeGuard::new();
    let lead_dir = swage_home.path().join("general/agents/lead");
    std::fs::create_dir_all(&lead_dir).expect("create lead definition");
    let lead_config = lead_dir.join("config.toml");
    std::fs::write(
        &lead_config,
        "[tools]\nallow = [\"shell\"]\ndeny = []\nbundles = {}\n",
    )
    .expect("write original grant");
    let source =
        r#"return await agent("continue bounded work", {agent: "default", maxAttempts: 1});"#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(WorkflowBudget::default()).expect("budget"),
            dispatch_bound_agent_ids: vec!["lead".into()],
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    storage
        .update_workflow_run_state(run.id, "interrupted", None, None, None)
        .await
        .expect("interrupt run");

    std::fs::write(
        &lead_config,
        "[tools]\nallow = [\"knowledge_search\"]\ndeny = []\nbundles = {}\n",
    )
    .expect("change dispatching grant");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
            scripted_text_output("bounded work completed"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(NamedTool::new("shell")) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let Json(resumed) = resume_run(State(state.clone()), Path(run.id))
        .await
        .expect("resume bounded run");
    wait_for_terminal(&state, resumed.id).await;
    let persisted = storage
        .get_workflow_run(run.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(persisted.dispatch_bound_agent_ids, vec!["lead"]);
    assert_eq!(
        persisted.state, "completed",
        "run error: {:?}",
        persisted.error_message
    );
    let requests = provider.requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .tool_definitions
            .iter()
            .all(|definition| definition["name"] != "shell"),
        "the resumed request must honour the changed dispatching grant"
    );
}

#[tokio::test]
async fn workflow_agent_call_executes_granted_approval_gated_tool_without_parking() {
    let rounds = vec![
        vec![
            swage_core::StreamChunk::ToolUse {
                call_id: "workflow-echo".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "workflow" }),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            swage_core::StreamChunk::Delta {
                content: "Workflow tool completed".into(),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ];
    let (state, _) = crate::turn::tests::make_state_with_approval_gated_echo(rounds).await;
    let topic = state
        .topic_store
        .general_scope()
        .await
        .expect("default topic");
    let result = run_ephemeral_agent_call(
        &state,
        Uuid::new_v4(),
        topic.id,
        &[],
        &[],
        Uuid::new_v4(),
        AgentCallRequest {
            agent: Some("gateway".into()),
            ..swage_workflow::fixtures::agent_call_request("Use echo".into())
        },
        None,
        CancellationToken::new(),
    )
    .await
    .expect("workflow agent call should execute without an approval answer");

    assert_eq!(
        result.output,
        Value::String("Workflow tool completed".into())
    );
    assert!(
        state.storage.list_approvals().await.unwrap().is_empty(),
        "an unattended workflow turn must not create approval rows"
    );
    let calls = state
        .storage
        .list_tool_calls_by_turn(result.turn_id.expect("turn id"))
        .await
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].status.as_deref(), Some("success"));
    assert_eq!(
        calls[0].result.as_deref(),
        Some("{\"output\":\"workflow\"}")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_agent_call_records_requested_skills_in_its_child_report() {
    let swage_home = tempfile::TempDir::new().expect("temp swage home");
    let skill_dir = swage_home.path().join("skills/workflow-specialist");
    std::fs::create_dir_all(&skill_dir).expect("create workflow skill directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Workflow specialist\n---\nUse the workflow specialist skill.\n",
    )
    .expect("write workflow skill");
    let _guard = crate::test_support::SwageHomeGuard::set_to(swage_home.path());

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![
            swage_core::StreamChunk::Delta {
                content: "specialist result".into(),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let request = |skills| AgentCallRequest {
        agent: Some("workflow-worker".into()),
        skills,
        ..swage_workflow::fixtures::agent_call_request("Use the requested skill".into())
    };

    let (fixture_run_id, fixture_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let result = run_ephemeral_agent_call(
        &state,
        fixture_run_id,
        topic.id,
        &[],
        &[],
        fixture_entry_id,
        request(Some(vec!["workflow-specialist".into()])),
        None,
        CancellationToken::new(),
    )
    .await
    .expect("workflow agent call");

    let snapshot = storage
        .get_snapshot(result.turn_id.expect("workflow child turn"))
        .await
        .expect("read child report")
        .expect("workflow child report should be persisted");
    let snapshot: Value = serde_json::from_str(&snapshot).expect("child report JSON");
    let skills = snapshot["elements"]
        .as_array()
        .expect("child report elements")
        .iter()
        .find(|element| element["name"] == "skills")
        .expect("workflow child report should record requested skills");
    assert!(
        skills["source"]
            .as_str()
            .is_some_and(|source| source.contains("workflow-specialist")),
        "workflow child report should name the requested skill: {skills}"
    );

    let (fixture_run_id, fixture_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let error = run_ephemeral_agent_call(
        &state,
        fixture_run_id,
        topic.id,
        &[],
        &[],
        fixture_entry_id,
        request(Some(vec!["missing-workflow-skill".into()])),
        None,
        CancellationToken::new(),
    )
    .await
    .expect_err("unavailable workflow skill must refuse the dispatch");
    assert!(
        error.to_string().contains("missing-workflow-skill"),
        "error was: {error}"
    );
    assert_eq!(
        storage
            .list_sessions_by_agent_id("workflow-worker")
            .await
            .unwrap()
            .len(),
        1,
        "failed skill admission must not create a child session"
    );
}

/// A shipped workflow's roles are bare dispatches: a fresh topic holds no
/// definition for them, and the role's whole grant rides the call's
/// `tools` list, narrowed further by a later call the same way.
#[tokio::test(flavor = "multi_thread")]
async fn fresh_topic_workflow_call_grants_a_role_through_its_call_scoped_tools() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output("gathered"),
            scripted_text_output("scoped"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all([
            Arc::new(NamedTool::new("knowledge_record")) as Arc<dyn crate::tool::Tool>,
            Arc::new(NamedTool::new("web_search")) as Arc<dyn crate::tool::Tool>,
            Arc::new(NamedTool::new("shell")) as Arc<dyn crate::tool::Tool>,
            Arc::new(NamedTool::new("config_apply")) as Arc<dyn crate::tool::Tool>,
        ]),
        None,
    );
    assert!(
        crate::service::resolve_effective_agent_definition(
            storage.as_ref(),
            "researcher",
            topic.id
        )
        .await
        .unwrap()
        .is_none(),
        "a workflow role is not a definition"
    );

    let (gather_run_id, journal_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let gather = |prompt: &str, tools: Vec<&str>| AgentCallRequest {
        tools: Some(tools.into_iter().map(String::from).collect()),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    };
    let result = run_ephemeral_agent_call(
        &state,
        gather_run_id,
        topic.id,
        &[],
        &[],
        journal_entry_id,
        gather(
            "Record gathered source material in the knowledge base.\n\nGather the selected source",
            vec!["web_search", "knowledge_record"],
        ),
        None,
        CancellationToken::new(),
    )
    .await
    .expect("bare role workflow call");

    assert_eq!(result.output, Value::String("gathered".into()));
    let requests = provider.requests.lock().expect("requests");
    let advertised_tools = requests[0]
        .tool_definitions
        .iter()
        .map(|definition| definition["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(advertised_tools, vec!["knowledge_record", "web_search"]);
    let context = requests[0]
        .messages
        .iter()
        .map(swage_core::ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(context.contains("Record gathered source material in the knowledge base"));
    drop(requests);

    // The worker session carries its journalled call's identity, so its
    // knowledge captures dedup across an interrupted call's re-run.
    let session = storage
        .list_sessions_by_agent_id(BARE_WORKFLOW_AGENT_ID)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("worker session");
    assert_eq!(
        crate::turn::agent_call_scope(session.metadata.as_ref()).as_deref(),
        Some(journal_entry_id.to_string().as_str())
    );

    let (fixture_run_id, fixture_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let scoped = run_ephemeral_agent_call(
        &state,
        fixture_run_id,
        topic.id,
        &[],
        &[],
        fixture_entry_id,
        gather("Scope sources without recording", vec!["web_search"]),
        None,
        CancellationToken::new(),
    )
    .await
    .expect("narrower bare role workflow call");
    assert_eq!(scoped.output, Value::String("scoped".into()));
    let requests = provider.requests.lock().expect("requests");
    let advertised_tools = requests[1]
        .tool_definitions
        .iter()
        .map(|definition| definition["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(advertised_tools, vec!["web_search"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn captured_sources_query_journals_the_run_scoped_store_read() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let worker = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: "worker".into(),
            request: serde_json::json!({}),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("worker entry");
    let prior = storage
        .record_observation(NewSourceObservation {
            topic_id: topic.id,
            origin: ObservationOrigin {
                url: Some("https://example.test/prior".into()),
                local_path: None,
            },
            retrieved_at: chrono::Utc::now(),
            body_sha256: "a".repeat(64),
            body_bytes: 5,
            media_type: "text/plain".into(),
            capture_scope: None,
        })
        .await
        .expect("record prior source");
    let recorded = storage
        .record_observation(NewSourceObservation {
            topic_id: topic.id,
            origin: ObservationOrigin {
                url: Some("https://example.test/first".into()),
                local_path: None,
            },
            retrieved_at: chrono::Utc::now(),
            body_sha256: "a".repeat(64),
            body_bytes: 5,
            media_type: "text/plain".into(),
            capture_scope: Some(worker.id.to_string()),
        })
        .await
        .expect("record source");
    assert_eq!(
        recorded.id, prior.id,
        "the body deduplicates to prior evidence"
    );
    let other_run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("other run");
    let other_worker = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: other_run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: "other worker".into(),
            request: serde_json::json!({}),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("other worker entry");
    storage
        .record_observation(NewSourceObservation {
            topic_id: topic.id,
            origin: ObservationOrigin {
                url: Some("https://example.test/other-run".into()),
                local_path: None,
            },
            retrieved_at: chrono::Utc::now(),
            body_sha256: "c".repeat(64),
            body_bytes: 5,
            media_type: "text/plain".into(),
            capture_scope: Some(other_worker.id.to_string()),
        })
        .await
        .expect("record other-run source");
    let query = state
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "captured_sources".into(),
            args_hash: journal_args_hash(&serde_json::json!({"action": "captured_sources"})),
            request: serde_json::json!({"action": "captured_sources"}),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("query intent");
    let reading = complete_captured_sources(&state, run.id, topic.id, query.id)
        .await
        .expect("complete query");
    assert_eq!(
        reading,
        serde_json::json!({"sources": [{"url": "https://example.test/first", "observation_id": recorded.id}]})
    );
    let independent = storage
        .list_source_captures_by_scopes(topic.id, vec![worker.id.to_string()])
        .await
        .expect("independent store read");
    assert_eq!(
        independent
            .iter()
            .map(|source| source.observation_id)
            .collect::<Vec<_>>(),
        vec![recorded.id]
    );

    storage
        .record_observation(NewSourceObservation {
            topic_id: topic.id,
            origin: ObservationOrigin {
                url: Some("https://example.test/later".into()),
                local_path: None,
            },
            retrieved_at: chrono::Utc::now(),
            body_sha256: "b".repeat(64),
            body_bytes: 5,
            media_type: "text/plain".into(),
            capture_scope: Some(worker.id.to_string()),
        })
        .await
        .expect("record later source");
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let (tx, rx) = mpsc::unbounded_channel();
    let host = tokio::spawn(run_host_commands(
        state.clone(),
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget::default(),
        replay,
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    tx.send(HostCommand::Record {
        kind: "log".into(),
        message: "consume the recorded worker slot".into(),
    })
    .expect("send replay worker slot");
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::CapturedSources { reply })
        .expect("send replayed captured-sources query");
    let replayed = receive
        .await
        .expect("receive replayed captured-sources query")
        .expect("replayed captured-sources query");
    drop(tx);
    host.await.expect("host loop");
    assert_eq!(replayed, reading, "replay must use the journalled reading");
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_resolves_captured_sources_from_the_persisted_authority_root() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let source = "return typeof capturedSources;";
    let (script, version) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "saved-sweep".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: true,
            authoring_session_id: None,
        })
        .await
        .expect("saved script");
    storage
        .update_workflow_script(script.id, None, None, Some(true), None)
        .await
        .expect("grant captured sources to saved script");

    let saved = storage
        .create_workflow_run(NewWorkflowRun {
            script_version_id: Some(version.id),
            source: None,
            source_hash: source_hash(source),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("saved run");
    assert!(
        captured_sources_grant_for_run(&state, &saved)
            .await
            .expect("resolve saved grant")
    );
    launch_existing_run(state.clone(), saved.clone(), source.into(), None)
        .await
        .expect("launch saved run");
    wait_for_terminal(&state, saved.id).await;
    assert_eq!(
        storage
            .get_workflow_run(saved.id)
            .await
            .expect("saved run query")
            .expect("saved run")
            .final_output,
        Some(Value::String("function".into()))
    );

    let (_, ungranted_version) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "saved-ungranted-sweep".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: false,
            authoring_session_id: None,
        })
        .await
        .expect("ungranted saved script");
    let ungranted = storage
        .create_workflow_run(NewWorkflowRun {
            script_version_id: Some(ungranted_version.id),
            source: None,
            source_hash: source_hash(source),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("ungranted saved run");
    assert!(
        !captured_sources_grant_for_run(&state, &ungranted)
            .await
            .expect("resolve ungranted saved-script grant"),
        "a saved script without the grant must retain its persisted denial"
    );
    launch_existing_run(state.clone(), ungranted.clone(), source.into(), None)
        .await
        .expect("launch ungranted saved run");
    wait_for_terminal(&state, ungranted.id).await;
    assert_eq!(
        storage
            .get_workflow_run(ungranted.id)
            .await
            .expect("ungranted saved run query")
            .expect("ungranted saved run")
            .final_output,
        Some(Value::String("undefined".into()))
    );

    let carried = storage
        .create_workflow_run(NewWorkflowRun {
            inline_workflow_id: Some(crate::synthetic_id::synthetic_id(
                CARRIED_WORKFLOW_ID_DOMAIN,
                topic.id,
                "skill/research-sweep",
            )),
            inline_workflow_name: Some("research-sweep".into()),
            inline_workflow_version: Some(1),
            carrying_skill: Some("research".into()),
            source: Some(source.into()),
            source_hash: source_hash(source),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("carried run");
    assert!(
        !captured_sources_grant_for_run(&state, &carried)
            .await
            .expect("resolve carried grant"),
        "a counterfeit research carrier must retain its advertised denial"
    );
    launch_existing_run(state.clone(), carried.clone(), source.into(), None)
        .await
        .expect("launch ungranted carried run");
    wait_for_terminal(&state, carried.id).await;
    assert_eq!(
        storage
            .get_workflow_run(carried.id)
            .await
            .expect("ungranted carried run query")
            .expect("ungranted carried run")
            .final_output,
        Some(Value::String("undefined".into()))
    );

    let builtin = storage
        .create_workflow_run(NewWorkflowRun {
            inline_workflow_id: Some(research_sweep_id(topic.id)),
            inline_workflow_name: Some("research-sweep".into()),
            inline_workflow_version: Some(1),
            carrying_skill: Some("research".into()),
            source: Some(source.into()),
            source_hash: source_hash(source),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("built-in run");
    assert!(
        captured_sources_grant_for_run(&state, &builtin)
            .await
            .expect("resolve shipped research grant")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn research_sweep_executes_its_gather_organise_and_synthesis_contract() {
    let _swage_home = crate::test_support::SwageHomeGuard::new();
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let deduplicated = storage
        .record_observation(NewSourceObservation {
            topic_id: topic.id,
            origin: ObservationOrigin {
                url: Some("https://example.test/landed".into()),
                local_path: None,
            },
            retrieved_at: chrono::Utc::now(),
            body_sha256: source_hash(RESEARCH_SWEEP_QUOTE),
            body_bytes: RESEARCH_SWEEP_QUOTE.len() as u64,
            media_type: "text/plain".into(),
            capture_scope: None,
        })
        .await
        .expect("seed deduplicated source");
    let provider = Arc::new(ResearchSweepRecordingProvider {
        requests: std::sync::Mutex::new(Vec::new()),
        observation_id: std::sync::Mutex::new(None),
    });
    let placeholder_tool_names = [
        "web_search",
        "browser_navigate",
        "browser_snapshot",
        "knowledge_curate",
    ];
    let mut tools = placeholder_tool_names
        .into_iter()
        .map(|name| Arc::new(NamedTool::new(name)) as Arc<dyn crate::tool::Tool>)
        .collect::<Vec<_>>();
    tools.extend([
        // The fetch the gatherer captures through: it retains the body
        // and names it the way url_fetch does, without the network.
        Arc::new(RetainingFetchStub) as Arc<dyn crate::tool::Tool>,
        Arc::new(crate::tool::knowledge::KnowledgeRecordTool) as Arc<dyn crate::tool::Tool>,
        Arc::new(crate::tool::knowledge::KnowledgeSearchTool) as Arc<dyn crate::tool::Tool>,
        Arc::new(crate::tool::knowledge::KnowledgeCiteTool) as Arc<dyn crate::tool::Tool>,
        Arc::new(crate::tool::knowledge::KnowledgeCurateOrientationTool)
            as Arc<dyn crate::tool::Tool>,
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(tools),
        None,
    );

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(research_sweep_id(topic.id)),
        Json(StartWorkflowRunRequest {
            args: serde_json::json!({
                "subject": "contract evidence",
                "purpose": "prove the shipped sweep contract"
            }),
            budget: None,
        }),
    )
    .await
    .expect("start research sweep with valid arguments");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run query")
        .expect("research sweep run");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let output = run.final_output.expect("terminal research report");
    // The gatherer for the comparison source claimed a recording the
    // knowledge base does not hold: the store decides, and the report
    // says both what the gatherer claimed and what the store shows.
    assert_eq!(output["failed_sources"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        output["failed_sources"][0]["url"],
        "https://example.test/failed"
    );
    let failure_reason = output["failed_sources"][0]["reason"]
        .as_str()
        .expect("failed source carries its reason");
    assert!(
        failure_reason.contains("holds no observation for this source"),
        "{failure_reason}"
    );
    assert!(failure_reason.ends_with("unreachable"), "{failure_reason}");
    assert_eq!(
        output["withheld_claims"],
        serde_json::json!([{
            "claim": "The comparison is conclusive.",
            "reason": "The comparison source was unreachable."
        }])
    );
    assert_eq!(output["organised"]["orientation_cards"], 1);
    let citation_id = output["findings"][0]["citation_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("terminal finding carries a citation id returned by knowledge_cite");
    // Each finding carries its own trail — the accepted quote, the cited
    // observation and the source's origin — so a reader follows the claim
    // to its evidence without a backend lookup.
    assert_eq!(output["findings"][0]["quote"], RESEARCH_SWEEP_QUOTE);
    assert_eq!(
        output["findings"][0]["source"],
        "https://example.test/landed"
    );

    let observations = storage
        .list_observations_by_topic(topic.id)
        .await
        .expect("persisted observations");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].id, deduplicated.id);
    assert_eq!(
        observations[0].origin.url.as_deref(),
        Some("https://example.test/landed")
    );
    let cards = storage
        .find_orientation_cards(topic.id, None, 10)
        .await
        .expect("persisted orientation cards");
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].observation_ids, vec![observations[0].id]);
    let citation = storage
        .get_citation(citation_id)
        .await
        .expect("citation query")
        .expect("persisted citation");
    assert_eq!(citation.observation_id, observations[0].id);
    assert_eq!(citation.quoted_text, RESEARCH_SWEEP_QUOTE);
    assert_eq!(
        output["findings"][0]["observation_id"],
        citation.observation_id.to_string()
    );

    let requests = provider.requests.lock().expect("requests");
    // Scope, gather (fetch, record, report) ×2 sources, organise ×2, synthesise ×2.
    assert_eq!(requests.len(), 9);
    let request_text = |request: &swage_core::CompletionRequest| {
        request
            .messages
            .iter()
            .map(swage_core::ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let organiser = requests
        .iter()
        .find(|request| request_text(request).contains("Organise them into"))
        .expect("organiser request");
    let organiser_text = request_text(organiser);
    assert!(organiser_text.contains(&observations[0].id.to_string()));
    assert!(!organiser_text.contains("https://example.test/failed"));

    let synthesist = requests
        .iter()
        .find(|request| request_text(request).contains("Deliver research findings"))
        .expect("synthesist request");
    let synthesist_tools = synthesist
        .tool_definitions
        .iter()
        .map(|definition| definition["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(synthesist_tools, vec!["knowledge_cite", "knowledge_search"]);
    assert!(
        request_text(synthesist)
            .contains("Put the claim in findings only after knowledge_cite accepts")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_journal_preserves_and_recomposes_call_scoped_context() {
    let swage_home = crate::test_support::SwageHomeGuard::new();
    std::fs::create_dir_all(swage_home.path().join("assemblies"))
        .expect("create assemblies directory");
    std::fs::create_dir_all(swage_home.path().join("elements")).expect("create elements directory");
    std::fs::write(
        swage_home.path().join("assemblies/focused.js"),
        "for (const element of elements) add(typeof element.render === 'function' ? element.render() : element);",
    )
    .expect("write named assembly");
    std::fs::write(
        swage_home.path().join("elements/current-work.md"),
        "Owner: {{owner}}",
    )
    .expect("write selected element");
    std::fs::write(
        swage_home.path().join("elements/unrequested.md"),
        "Must not reach the child",
    )
    .expect("write unrequested element");

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output("first child result"),
            scripted_text_output("resumed child result"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: None,
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: None,
        },
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let request = AgentCallRequest {
        agent: Some("default".into()),
        elements: Some(vec!["current-work".into()]),
        variables: Some(std::collections::BTreeMap::from([(
            "owner".into(),
            "journal owner".into(),
        )])),
        assembly: Some("focused".into()),
        ..swage_workflow::fixtures::agent_call_request("Use the briefing".into())
    };
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: request.clone(),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("send initial agent call");
    let first_result = receive
        .await
        .expect("receive initial agent result")
        .expect("initial agent call");

    let initial_journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("read initial journal");
    assert_eq!(initial_journal.len(), 1);
    let expected = serde_json::to_value(&request).expect("serialise expected request");
    assert_eq!(initial_journal[0].request, expected);
    let replayed_request: AgentCallRequest =
        serde_json::from_value(initial_journal[0].request.clone())
            .expect("reconstruct agent request from journal");

    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: replayed_request,
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("send replayed agent call");
    let replayed_result = receive
        .await
        .expect("receive replayed agent result")
        .expect("replayed agent call");
    drop(tx);
    task.await.expect("host loop");

    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("read journal");
    assert_eq!(journal.len(), 2);
    assert!(journal.iter().all(|entry| entry.request == expected));

    let mut reports = Vec::new();
    let mut tool_roots = Vec::new();
    for result in [&first_result, &replayed_result] {
        let report = storage
            .get_snapshot(result.turn_id.expect("turn id"))
            .await
            .expect("read snapshot")
            .expect("workflow child assembly report");
        let mut report: Value = serde_json::from_str(&report).expect("report JSON");
        assert_eq!(report["assembly_name"], "focused");
        assert!(
            report["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|element| element["name"] == "current-work")
        );
        assert!(
            report["elements"]
                .as_array()
                .unwrap()
                .iter()
                .all(|element| element["name"] != "unrequested")
        );
        report.as_object_mut().unwrap().remove("duration");
        report.as_object_mut().unwrap().remove("provider_visible");
        report.as_object_mut().unwrap().remove("total_tokens");
        tool_roots.push(
            report["tool_scope"]["effective_root"]
                .as_str()
                .expect("effective tool root")
                .to_owned(),
        );
        report.as_object_mut().unwrap().remove("tool_scope");
        if let Some(orientation) = report["elements"].as_array_mut().and_then(|elements| {
            elements
                .iter_mut()
                .find(|element| element["name"] == "orientation")
        }) {
            orientation.as_object_mut().unwrap().remove("token_count");
        }
        reports.push(report);
    }
    assert_ne!(
        tool_roots[0], tool_roots[1],
        "separate workflow calls must receive separate managed scratch roots"
    );
    for (result, root) in [&first_result, &replayed_result]
        .into_iter()
        .zip(tool_roots)
    {
        assert!(
            root.ends_with(&format!(
                "/sessions/{}",
                result.session_id.expect("session id")
            )),
            "effective tool root must belong to the workflow call's session: {root}"
        );
    }
    assert_eq!(reports[0], reports[1]);
    let requests = provider.requests.lock().expect("provider requests");
    assert!(requests.iter().all(|request| {
        request
            .messages
            .iter()
            .any(|message| message.text_content().contains("Owner: journal owner"))
    }));
}

#[test]
fn budget_reserve_requires_a_total_token_ceiling() {
    let error = validate_budget(&WorkflowBudget {
        max_agent_calls: None,
        per_route: Default::default(),
        max_total_tokens: None,
        reserved_closing_tokens: Some(1),
        max_concurrent_agent_calls: None,
    })
    .expect_err("reserve without ceiling must be rejected");

    assert!(matches!(
        error.cause(),
        Some(swage_core::GatewayError::ValidationError(message))
            if message == "reserved_closing_tokens requires max_total_tokens"
    ));
}

#[test]
fn budget_reserve_cannot_exceed_total_token_ceiling() {
    let error = validate_budget(&WorkflowBudget {
        max_agent_calls: None,
        per_route: Default::default(),
        max_total_tokens: Some(10),
        reserved_closing_tokens: Some(11),
        max_concurrent_agent_calls: None,
    })
    .expect_err("reserve above ceiling must be rejected");

    assert!(matches!(
        error.cause(),
        Some(swage_core::GatewayError::ValidationError(message))
            if message == "reserved_closing_tokens cannot exceed max_total_tokens"
    ));
}

struct RecordingDeliveryCapability {
    effect_kind: swage_workflow::EffectKind,
    probe_result: Option<EffectReceipt>,
    probes: AtomicUsize,
    deliveries: AtomicUsize,
}

struct FailingDeliveryCapability {
    error: &'static str,
}

struct CancellingDeliveryCapability {
    entered: Arc<Notify>,
}

struct SettlementProvider {
    calls: AtomicUsize,
    pending_call_started: Arc<Notify>,
}

struct PendingWorkingProvider {
    calls: AtomicUsize,
    pending_calls: usize,
    pending_call_started: Arc<Notify>,
}

struct ReleasableProvider {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for ReleasableProvider {
    fn model_id(&self) -> &str {
        "releasable-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        let started = Arc::clone(&self.started);
        let release = Arc::clone(&self.release);
        let first = futures::stream::once(async move {
            started.notify_one();
            release.notified().await;
            swage_core::StreamChunk::Delta {
                content: "finished after wake".into(),
            }
        });
        Box::pin(futures::StreamExt::chain(
            first,
            futures::stream::iter([
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ]),
        ))
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for SettlementProvider {
    fn model_id(&self) -> &str {
        "settlement-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Box::pin(futures::stream::iter([
                swage_core::StreamChunk::Delta {
                    content: "settled".into(),
                },
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 300_000,
                    output_tokens: 100_000,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ]))
        } else {
            self.pending_call_started.notify_one();
            Box::pin(futures::stream::pending())
        }
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for PendingWorkingProvider {
    fn model_id(&self) -> &str {
        "pending-working-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.pending_calls {
            self.pending_call_started.notify_one();
            Box::pin(futures::stream::pending())
        } else {
            Box::pin(futures::stream::iter(scripted_text_output(
                "closing report",
            )))
        }
    }
}

struct WorkingBudgetScaffold {
    storage: Arc<SqliteStorage>,
    topic_id: Uuid,
    run_id: Uuid,
    state: AppState,
    pending_call_started: Arc<Notify>,
}

impl WorkingBudgetScaffold {
    async fn new(pending_calls: usize) -> Self {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                source_hash: source_hash("return null"),
                ..swage_storage::fixtures::new_workflow_run(topic.id)
            })
            .await
            .expect("run");
        let pending_call_started = Arc::new(Notify::new());
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(PendingWorkingProvider {
                calls: AtomicUsize::new(0),
                pending_calls,
                pending_call_started: Arc::clone(&pending_call_started),
            }),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        Self {
            storage,
            topic_id: topic.id,
            run_id: run.id,
            state,
            pending_call_started,
        }
    }

    fn spawn(
        self,
        budget: WorkflowBudget,
        replay: Vec<WorkflowJournalRecord>,
        agent_calls: u32,
        spent_tokens: u64,
    ) -> (
        mpsc::UnboundedSender<HostCommand>,
        Arc<Notify>,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_host_commands(
            self.state,
            self.run_id,
            self.topic_id,
            vec![],
            vec![],
            None,
            None,
            budget,
            replay,
            Arc::new(AtomicU32::new(agent_calls)),
            Arc::new(AtomicU64::new(spent_tokens)),
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            CancellationToken::new(),
            PARK_TIMER_THRESHOLD_MS,
            rx,
        ));
        (tx, self.pending_call_started, task)
    }
}

/// Streams scripted outputs in order and records each request's message
/// texts, so a test can assert what history a turn actually carried.
struct ScriptedRecordingProvider {
    outputs: std::sync::Mutex<std::collections::VecDeque<Vec<swage_core::StreamChunk>>>,
    requests: std::sync::Mutex<Vec<swage_core::CompletionRequest>>,
}

struct ParkedFirstDeltaProvider {
    parked: std::sync::Mutex<Option<oneshot::Sender<chrono::DateTime<chrono::Utc>>>>,
    release: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

struct PendingWithoutEvidenceProvider {
    pending: Arc<Notify>,
}

impl ParkedFirstDeltaProvider {
    fn new() -> (
        Self,
        oneshot::Receiver<chrono::DateTime<chrono::Utc>>,
        oneshot::Sender<()>,
    ) {
        let (parked_tx, parked_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        (
            Self {
                parked: std::sync::Mutex::new(Some(parked_tx)),
                release: std::sync::Mutex::new(Some(release_rx)),
            },
            parked_rx,
            release_tx,
        )
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for PendingWithoutEvidenceProvider {
    fn model_id(&self) -> &str {
        "pending-without-evidence"
    }

    async fn complete(
        &self,
        _: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        _: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        self.pending.notify_one();
        Box::pin(futures::stream::pending())
    }
}

const RESEARCH_SWEEP_QUOTE: &str = "The landed source supports the purpose.";

/// A url_fetch stand-in for the contract test: no network, but the
/// three moves a capture depends on — the body retained as a tool-output
/// file, the file and media type named in the result, and the capture
/// hint pointing at this call.
struct RetainingFetchStub;

#[async_trait::async_trait]
impl crate::tool::Tool for RetainingFetchStub {
    fn name(&self) -> &str {
        "url_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL (contract stand-in)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "url": { "type": "string" } },
            "required": ["url"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        use tokio::io::AsyncWriteExt;
        let path = crate::tool::workspace::tool_output_artifact_path(ctx, "raw-body.txt");
        let mut file = crate::tool::workspace::create_tool_output_artifact(ctx, &path.absolute)
            .await
            .map_err(|error| swage_core::ToolError::ExecutionFailed {
                name: "url_fetch".into(),
                message: error.to_string(),
            })?;
        file.write_all(RESEARCH_SWEEP_QUOTE.as_bytes())
            .await
            .map_err(|error| swage_core::ToolError::ExecutionFailed {
                name: "url_fetch".into(),
                message: error.to_string(),
            })?;
        file.flush().await.ok();
        let turn_id = ctx
            .turn_id()
            .expect("the fetch runs inside a recorded turn");
        let call_id = ctx.call_id().expect("the fetch runs as a recorded call");
        Ok(serde_json::json!({
            "output": RESEARCH_SWEEP_QUOTE,
            "raw_body_artifact_path": path.relative,
            "media_type": "text/plain",
            "capture": {
                "tool": "knowledge_record",
                "arguments": {
                    "fetch_call": swage_core::tool_result_reference(&turn_id.to_string(), call_id),
                },
            },
            "url": params["url"],
        }))
    }
}

/// Matches responses to stage prompts so concurrent gather calls cannot
/// consume one another's scripted outcome.
struct ResearchSweepRecordingProvider {
    requests: std::sync::Mutex<Vec<swage_core::CompletionRequest>>,
    observation_id: std::sync::Mutex<Option<Uuid>>,
}

struct OrderedFailingProvider {
    storage: Arc<SqliteStorage>,
    run_id: Arc<tokio::sync::OnceCell<Uuid>>,
    concurrent_entered: Arc<Notify>,
}

#[derive(Default)]
struct PlacementWritingProvider {
    calls: std::sync::Mutex<std::collections::HashMap<&'static str, u32>>,
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for PlacementWritingProvider {
    fn model_id(&self) -> &str {
        "placement-writer"
    }
    async fn complete(
        &self,
        _: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!()
    }
    async fn complete_with_tools(
        &self,
        _: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!()
    }
    fn stream(
        &self,
        request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        let text = request
            .messages
            .iter()
            .map(swage_core::ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n");
        if text.contains("leave worktree unchanged") {
            return Box::pin(futures::stream::iter(scripted_text_output("unchanged")));
        }
        let file = if text.contains("retained marker") {
            "retained-marker"
        } else if text.contains("first-marker") {
            "first-marker"
        } else {
            "second-marker"
        };
        let mut calls = self.calls.lock().expect("placement calls");
        let count = calls.entry(file).or_default();
        *count += 1;
        if *count > 1 {
            return Box::pin(futures::stream::iter(scripted_text_output("written")));
        }
        Box::pin(futures::stream::iter(vec![
            swage_core::StreamChunk::ToolUse {
                call_id: format!("write-{file}"),
                name: "placement_write".into(),
                arguments: serde_json::json!({"file": file}),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ]))
    }
}

#[derive(Default)]
struct PlacementWriteTool {
    writes: std::sync::Mutex<Vec<(String, std::path::PathBuf)>>,
}

#[async_trait::async_trait]
impl crate::tool::Tool for PlacementWriteTool {
    fn name(&self) -> &str {
        "placement_write"
    }
    fn description(&self) -> &str {
        "Write a marker in the worker's working directory"
    }
    fn parameters(&self) -> Value {
        serde_json::json!({"type":"object","properties":{"file":{"type":"string"}},"required":["file"]})
    }
    async fn execute(
        &self,
        arguments: Value,
        ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, swage_core::ToolError> {
        let file =
            arguments["file"]
                .as_str()
                .ok_or_else(|| swage_core::ToolError::InvalidParameters {
                    name: self.name().into(),
                    message: "file is required".into(),
                })?;
        std::fs::write(ctx.working_directory().join(file), file).map_err(|error| {
            swage_core::ToolError::ExecutionFailed {
                name: self.name().into(),
                message: error.to_string(),
            }
        })?;
        self.writes
            .lock()
            .expect("placement writes")
            .push((file.to_string(), ctx.working_directory().to_path_buf()));
        Ok(serde_json::json!({"written": file}))
    }
}

fn scripted_text_output(content: &str) -> Vec<swage_core::StreamChunk> {
    scripted_text_output_with_usage(content, 1, 1)
}

fn scripted_text_output_with_usage(
    content: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<swage_core::StreamChunk> {
    vec![
        swage_core::StreamChunk::Delta {
            content: content.into(),
        },
        swage_core::StreamChunk::Usage(swage_core::TokenUsage {
            input_tokens,
            output_tokens,
        }),
        swage_core::StreamChunk::Done {
            finish_reason: None,
        },
    ]
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for ScriptedRecordingProvider {
    fn model_id(&self) -> &str {
        "scripted-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        self.requests.lock().expect("requests").push(request);
        match self.outputs.lock().expect("outputs").pop_front() {
            Some(output) => Box::pin(futures::stream::iter(output)),
            None => Box::pin(futures::stream::pending()),
        }
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for ParkedFirstDeltaProvider {
    fn model_id(&self) -> &str {
        "parked-first-delta"
    }

    async fn complete(
        &self,
        _: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        let prompt = request
            .messages
            .iter()
            .map(swage_core::ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n");
        if !prompt.contains("parked worker") {
            return Box::pin(futures::stream::iter(scripted_text_output("evidence")));
        }
        let parked = self.parked.lock().expect("parked sender").take();
        let release = self
            .release
            .lock()
            .expect("release receiver")
            .take()
            .expect("one parked call");
        Box::pin(
            futures::stream::once(async move {
                parked
                    .expect("first parked call")
                    .send(chrono::Utc::now())
                    .expect("park signal");
                release.await.expect("release parked worker");
                swage_core::StreamChunk::Delta {
                    content: "evidence".into(),
                }
            })
            .chain(futures::stream::iter(vec![
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ])),
        )
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for ResearchSweepRecordingProvider {
    fn model_id(&self) -> &str {
        "research-sweep-scripted-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        let text = request
            .messages
            .iter()
            .map(swage_core::ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n");
        let tool_result = |call_id: &str| {
            request
                .messages
                .iter()
                .find_map(|message| {
                    if message.tool_call_id() == Some(call_id) {
                        return Some(message.text_content());
                    }
                    message.tool_replay().and_then(|parts| {
                        parts.iter().find_map(|part| match part {
                            swage_core::ToolReplayPart::ToolResult {
                                call_id: result_call_id,
                                content,
                                ..
                            } if result_call_id == call_id => Some(content.clone()),
                            _ => None,
                        })
                    })
                })
                .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        };
        let output = if text.contains("You are scoping a research sweep.") {
            scripted_text_output(
                r#"{"sources":[{"url":"https://example.test/landed","why":"primary evidence"},{"url":"https://example.test/failed","why":"comparison"}]}"#,
            )
        } else if text.contains("Source: https://example.test/landed") {
            match (tool_result("fetch-landed"), tool_result("record-landed")) {
                (_, Some(result)) => {
                    let observation_id = result["observation_id"]
                        .as_str()
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .expect("record observation id");
                    assert_eq!(
                        result["citable"], true,
                        "a capture bound to the fetch is citable"
                    );
                    *self.observation_id.lock().expect("observation id") = Some(observation_id);
                    scripted_text_output(&format!(
                        r#"{{"url":"https://example.test/landed","recorded":true,"observation_id":"{observation_id}","note":"recorded"}}"#,
                    ))
                }
                // The fetch is in hand: capture it through the reference
                // its result names, and nothing else.
                (Some(fetched), None) => vec![
                    swage_core::StreamChunk::ToolUse {
                        call_id: "record-landed".into(),
                        name: "knowledge_record".into(),
                        arguments: serde_json::json!({
                            "fetch_call": fetched["capture"]["arguments"]["fetch_call"],
                        }),
                    },
                    swage_core::StreamChunk::Done {
                        finish_reason: None,
                    },
                ],
                (None, None) => vec![
                    swage_core::StreamChunk::ToolUse {
                        call_id: "fetch-landed".into(),
                        name: "url_fetch".into(),
                        arguments: serde_json::json!({ "url": "https://example.test/landed" }),
                    },
                    swage_core::StreamChunk::Done {
                        finish_reason: None,
                    },
                ],
            }
        } else if text.contains("Source: https://example.test/failed") {
            scripted_text_output(
                r#"{"url":"https://example.test/failed","recorded":true,"observation_id":"00000000-0000-0000-0000-000000000001","note":"unreachable"}"#,
            )
        } else if text.contains("Organise them into the topic's knowledge base.") {
            match tool_result("orient-landed") {
                Some(_) => scripted_text_output(
                    r#"{"orientation_cards":1,"assertions":0,"note":"Added an evidence signpost."}"#,
                ),
                None => {
                    let observation_id = self
                        .observation_id
                        .lock()
                        .expect("observation id")
                        .expect("organiser receives the gatherer's recorded observation");
                    vec![
                        swage_core::StreamChunk::ToolUse {
                            call_id: "orient-landed".into(),
                            name: "knowledge_curate_orientation".into(),
                            arguments: serde_json::json!({
                                "operation": "author",
                                "title": "Landed contract evidence",
                                "content": "The landed source is evidence for the sweep purpose.",
                                "observation_ids": [observation_id],
                            }),
                        },
                        swage_core::StreamChunk::Done {
                            finish_reason: None,
                        },
                    ]
                }
            }
        } else if text.contains("Deliver research findings.") {
            match tool_result("cite-landed") {
                Some(result) => scripted_text_output(&format!(
                    r#"{{"findings":[{{"claim":"The landed source supports the purpose.","citation_id":"{}","quote":"{}","observation_id":"{}","source":"https://example.test/landed"}}],"withheld":[{{"claim":"The comparison is conclusive.","reason":"The comparison source was unreachable."}}]}}"#,
                    result["citation_id"].as_str().expect("citation id"),
                    result["quote"].as_str().expect("accepted quote"),
                    result["observation_id"]
                        .as_str()
                        .expect("cited observation"),
                )),
                None => {
                    let observation_id = self
                        .observation_id
                        .lock()
                        .expect("observation id")
                        .expect("synthesist receives the landed observation");
                    vec![
                        swage_core::StreamChunk::ToolUse {
                            call_id: "search-landed".into(),
                            name: "knowledge_search".into(),
                            arguments: serde_json::json!({"query": "landed source supports"}),
                        },
                        swage_core::StreamChunk::ToolUse {
                            call_id: "cite-landed".into(),
                            name: "knowledge_cite".into(),
                            arguments: serde_json::json!({
                                "observation_id": observation_id,
                                "version": 1,
                                "quote": RESEARCH_SWEEP_QUOTE,
                            }),
                        },
                        swage_core::StreamChunk::Done {
                            finish_reason: None,
                        },
                    ]
                }
            }
        } else {
            panic!("unexpected research sweep request: {text}")
        };
        self.requests.lock().expect("requests").push(request);
        Box::pin(futures::stream::iter(output))
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for OrderedFailingProvider {
    fn model_id(&self) -> &str {
        "ordered-failing-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        let is_terminal_call = request
            .messages
            .iter()
            .flat_map(|message| message.content())
            .any(|part| {
                matches!(part, swage_core::ContentPart::Text { text } if text.contains("terminal call fails first"))
            });
        let storage = Arc::clone(&self.storage);
        let run_id = Arc::clone(&self.run_id);
        let concurrent_entered = Arc::clone(&self.concurrent_entered);
        Box::pin(futures::stream::once(async move {
            if is_terminal_call {
                concurrent_entered.notified().await;
            } else {
                concurrent_entered.notify_one();
                loop {
                    let Some(run_id) = run_id.get() else {
                        tokio::task::yield_now().await;
                        continue;
                    };
                    let terminal_settled = storage
                        .list_workflow_journal(*run_id)
                        .await
                        .expect("failure ordering journal")
                        .iter()
                        .any(|entry| {
                            entry.request["label"] == "terminal call" && entry.status == "failed"
                        });
                    if terminal_settled {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
            swage_core::StreamChunk::Error {
                message: if is_terminal_call {
                    "terminal call provider failure".into()
                } else {
                    "later concurrent provider failure".into()
                },
            }
        }))
    }
}

use crate::test_support::NamedTool;

#[async_trait::async_trait]
impl DeliveryCapability for CancellingDeliveryCapability {
    fn kind(&self) -> &'static str {
        "cancelling"
    }

    fn effect_kind(&self) -> swage_workflow::EffectKind {
        swage_workflow::EffectKind::IdempotencyKey
    }

    async fn deliver(
        &self,
        _key: Uuid,
        _payload: EffectPayload,
        _effect_seq: u64,
        cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String> {
        self.entered.notify_one();
        cancellation.cancelled().await;
        Err("WorkflowCancelledError: run cancelled".into())
    }

    async fn probe(
        &self,
        _key: Uuid,
        _payload: &EffectPayload,
    ) -> Result<Option<EffectReceipt>, String> {
        unreachable!("idempotency-key effects do not probe")
    }
}

#[async_trait::async_trait]
impl DeliveryCapability for FailingDeliveryCapability {
    fn kind(&self) -> &'static str {
        "failing"
    }
    fn effect_kind(&self) -> swage_workflow::EffectKind {
        swage_workflow::EffectKind::IdempotencyKey
    }
    async fn deliver(
        &self,
        _key: Uuid,
        _payload: EffectPayload,
        _effect_seq: u64,
        _cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String> {
        Err(self.error.into())
    }

    async fn probe(
        &self,
        _key: Uuid,
        _payload: &EffectPayload,
    ) -> Result<Option<EffectReceipt>, String> {
        Err(self.error.into())
    }
}

#[async_trait::async_trait]
impl DeliveryCapability for RecordingDeliveryCapability {
    fn kind(&self) -> &'static str {
        "recording"
    }

    fn effect_kind(&self) -> swage_workflow::EffectKind {
        self.effect_kind
    }

    async fn deliver(
        &self,
        _key: Uuid,
        _payload: EffectPayload,
        effect_seq: u64,
        _cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String> {
        self.deliveries.fetch_add(1, Ordering::SeqCst);
        Ok(EffectReceipt {
            delivered: true,
            target: "delivered".into(),
            effect_seq,
        })
    }

    async fn probe(
        &self,
        _key: Uuid,
        _payload: &EffectPayload,
    ) -> Result<Option<EffectReceipt>, String> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        Ok(self.probe_result.clone())
    }
}

async fn started_effect_entry() -> (AppState, Arc<SqliteStorage>, Uuid, Uuid) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = serde_json::to_value(recovery_payload()).expect("effect request");
    let entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "effect".into(),
            args_hash: journal_args_hash(&request),
            request,
            effect_kind: Some("test".into()),
            idempotency_key: Some(Uuid::new_v4()),
            target_kind: Some("recording".into()),
        })
        .await
        .expect("started entry");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (state, storage, run.id, entry.id)
}

fn recovery_payload() -> EffectPayload {
    EffectPayload {
        content: "recover me".into(),
        metadata: None,
        target: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn started_idempotency_key_effect_resends_once_without_probing() {
    let (state, _, _, entry_id) = started_effect_entry().await;
    let capability = Arc::new(RecordingDeliveryCapability {
        effect_kind: swage_workflow::EffectKind::IdempotencyKey,
        probe_result: None,
        probes: AtomicUsize::new(0),
        deliveries: AtomicUsize::new(0),
    });

    let receipt = complete_effect_entry(
        &state,
        Arc::clone(&capability) as Arc<dyn DeliveryCapability>,
        entry_id,
        recovery_payload(),
        CancellationToken::new(),
        7,
        Uuid::new_v4(),
    )
    .await
    .expect("recover");

    assert_eq!(receipt.target, "delivered");
    assert_eq!(receipt.effect_seq, 7);
    assert_eq!(capability.probes.load(Ordering::SeqCst), 0);
    assert_eq!(capability.deliveries.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn started_check_before_redo_effect_uses_probe_receipt_without_resending() {
    let (state, _, _, entry_id) = started_effect_entry().await;
    let capability = Arc::new(RecordingDeliveryCapability {
        effect_kind: swage_workflow::EffectKind::CheckBeforeRedo,
        probe_result: Some(EffectReceipt {
            delivered: true,
            target: "already-delivered".into(),
            effect_seq: 3,
        }),
        probes: AtomicUsize::new(0),
        deliveries: AtomicUsize::new(0),
    });

    let receipt = complete_effect_entry(
        &state,
        Arc::clone(&capability) as Arc<dyn DeliveryCapability>,
        entry_id,
        recovery_payload(),
        CancellationToken::new(),
        7,
        Uuid::new_v4(),
    )
    .await
    .expect("recover");

    assert_eq!(receipt.target, "already-delivered");
    assert_eq!(receipt.effect_seq, 3);
    assert_eq!(capability.probes.load(Ordering::SeqCst), 1);
    assert_eq!(capability.deliveries.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn started_check_before_redo_effect_resends_once_after_probe_miss() {
    let (state, _, _, entry_id) = started_effect_entry().await;
    let capability = Arc::new(RecordingDeliveryCapability {
        effect_kind: swage_workflow::EffectKind::CheckBeforeRedo,
        probe_result: None,
        probes: AtomicUsize::new(0),
        deliveries: AtomicUsize::new(0),
    });

    let receipt = complete_effect_entry(
        &state,
        Arc::clone(&capability) as Arc<dyn DeliveryCapability>,
        entry_id,
        recovery_payload(),
        CancellationToken::new(),
        7,
        Uuid::new_v4(),
    )
    .await
    .expect("recover");

    assert_eq!(receipt.target, "delivered");
    assert_eq!(receipt.effect_seq, 7);
    assert_eq!(capability.probes.load(Ordering::SeqCst), 1);
    assert_eq!(capability.deliveries.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_effect_settles_with_its_error() {
    let error = "DeliveryError: receiver refused";
    let (state, _, run_id, entry_id) = started_effect_entry().await;
    let result = complete_effect_entry(
        &state,
        Arc::new(FailingDeliveryCapability { error }),
        entry_id,
        recovery_payload(),
        CancellationToken::new(),
        0,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(result.expect_err("effect fails"), error);
    let entry = state
        .storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal")
        .into_iter()
        .find(|entry| entry.id == entry_id)
        .expect("entry");
    assert_eq!(entry.status, "failed");
    assert_eq!(entry.error_message.as_deref(), Some(error));
}

#[tokio::test(flavor = "multi_thread")]
async fn replayed_failed_effect_returns_recorded_error_without_recovery() {
    let error = "DeliveryError: receiver refused";
    let (state, storage, run_id, entry_id) = started_effect_entry().await;
    state
        .fail_workflow_journal_entry(entry_id, error)
        .await
        .expect("fail entry");
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal");
    let topic = storage.general_scope().await.expect("topic");
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: None,
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: None,
        },
        replay,
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Deliver {
        payload: recovery_payload(),
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("replay effect");

    assert_eq!(
        receive.await.expect("reply").expect_err("recorded failure"),
        error
    );
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn delivered_effect_with_completion_persistence_failure_settles_as_failed() {
    let (state, storage, run_id, entry_id) = started_effect_entry().await;
    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TEMP TRIGGER reject_effect_completion
                 BEFORE UPDATE OF status ON workflow_journal
                 WHEN NEW.id = OLD.id AND NEW.status = 'completed'
                 BEGIN SELECT RAISE(FAIL, 'injected completion persistence failure'); END;",
            )
        })
        .await
        .expect("install failure trigger");
    let capability = Arc::new(RecordingDeliveryCapability {
        effect_kind: swage_workflow::EffectKind::IdempotencyKey,
        probe_result: None,
        probes: AtomicUsize::new(0),
        deliveries: AtomicUsize::new(0),
    });

    let error = complete_effect_entry(
        &state,
        Arc::clone(&capability) as Arc<dyn DeliveryCapability>,
        entry_id,
        recovery_payload(),
        CancellationToken::new(),
        0,
        Uuid::new_v4(),
    )
    .await
    .expect_err("completion persistence fails");

    assert!(error.contains("injected completion persistence failure"));
    assert_eq!(capability.deliveries.load(Ordering::SeqCst), 1);
    let entry = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal")
        .into_iter()
        .find(|entry| entry.id == entry_id)
        .expect("entry");
    assert_eq!(entry.status, "failed");
    assert_eq!(entry.error_message.as_deref(), Some(error.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_an_in_flight_effect_leaves_its_entry_started() {
    let (state, _, run_id, entry_id) = started_effect_entry().await;
    let cancellation = CancellationToken::new();
    let entered = Arc::new(Notify::new());
    let completion = tokio::spawn({
        let state = state.clone();
        let cancellation = cancellation.clone();
        let entered = Arc::clone(&entered);
        async move {
            complete_effect_entry(
                &state,
                Arc::new(CancellingDeliveryCapability { entered }),
                entry_id,
                recovery_payload(),
                cancellation,
                0,
                Uuid::new_v4(),
            )
            .await
        }
    });
    entered.notified().await;
    cancellation.cancel();
    assert_eq!(
        completion
            .await
            .expect("completion task")
            .expect_err("effect is cancelled"),
        "WorkflowCancelledError: run cancelled"
    );
    let entry = state
        .storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal")
        .into_iter()
        .find(|entry| entry.id == entry_id)
        .expect("entry");
    assert_eq!(entry.status, "started");
    assert_eq!(entry.error_message, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_and_failed_journal_broadcasts_match_persisted_entries() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let mut events = state.broadcast.subscribe();
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "console.log('visible'); return null".into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;
    let mut observed = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            ServerEvent::WorkflowEntryStarted { event }
            | ServerEvent::WorkflowEntryCompleted { event } => observed.push(event.entry),
            _ => {}
        }
    }
    let log_events = observed
        .into_iter()
        .filter(|entry| entry.kind == WorkflowJournalEntryKind::Log)
        .collect::<Vec<_>>();
    assert_eq!(log_events.len(), 2);
    assert_eq!(log_events[0].id, log_events[1].id);
    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("detail");
    let log = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::Log)
        .expect("logged journal entry");
    assert_eq!(log_events[1].status, log.status);
    assert_eq!(log_events[1].completed_order, log.completed_order);

    let failed = state
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: started.id,
            seq: 2,
            kind: "effect".into(),
            args_hash: journal_args_hash(&Value::Null),
            request: Value::Null,
            effect_kind: Some("test".into()),
            idempotency_key: Some(Uuid::new_v4()),
            target_kind: Some("recording".into()),
        })
        .await
        .expect("failed intent");
    state
        .fail_workflow_journal_entry(failed.id, "DeliveryError: refused")
        .await
        .expect("fail entry");
    let started_event = loop {
        if let ServerEvent::WorkflowEntryStarted { event } =
            events.recv().await.expect("started broadcast")
        {
            break event;
        }
    };
    let failed_event = loop {
        if let ServerEvent::WorkflowEntryCompleted { event } =
            events.recv().await.expect("failed broadcast")
        {
            break event;
        }
    };
    assert_eq!(started_event.entry.id, failed.id);
    assert_eq!(failed_event.entry.id, failed.id);
    assert_eq!(
        failed_event.entry.status,
        swage_protocol::client::workflow::WorkflowJournalEntryStatus::Failed
    );
    let persisted = state
        .storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal")
        .into_iter()
        .find(|entry| entry.id == failed.id)
        .expect("failed entry");
    assert_eq!(persisted.status, "failed");
    assert_eq!(
        persisted.error_message.as_deref(),
        Some("DeliveryError: refused")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_working_budget_admits_both_parallel_calls() {
    let scaffold = WorkingBudgetScaffold::new(2).await;
    let (tx, pending_call_started, task) = scaffold.spawn(
        WorkflowBudget {
            max_agent_calls: Some(2),
            per_route: Default::default(),
            max_total_tokens: Some(1_000),
            reserved_closing_tokens: Some(100),
            max_concurrent_agent_calls: Some(2),
        },
        vec![],
        0,
        0,
    );
    let request = |prompt: &str| AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    };
    let first_cancellation = CancellationToken::new();
    let (first_reply, first_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("parallel one"),
        cancellation: first_cancellation.clone(),
        reply: first_reply,
    })
    .expect("first parallel call");
    pending_call_started.notified().await;

    let second_cancellation = CancellationToken::new();
    let (second_reply, second_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("parallel two"),
        cancellation: second_cancellation.clone(),
        reply: second_reply,
    })
    .expect("second parallel call");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        pending_call_started.notified(),
    )
    .await
    .expect("both configured parallel calls reach the provider");

    first_cancellation.cancel();
    second_cancellation.cancel();
    assert!(first_receive.await.expect("first reply").is_err());
    assert!(second_receive.await.expect("second reply").is_err());
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn token_reservation_refuses_a_concurrent_call_at_admission() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::PendingProvider),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: Some(1_000_000),
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: Some(2),
        },
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let request = AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request("hold capacity".into())
    };
    let first_cancellation = CancellationToken::new();
    let (first_reply, first_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request.clone(),
        cancellation: first_cancellation.clone(),
        reply: first_reply,
    })
    .expect("first call");
    let second_cancellation = CancellationToken::new();
    let (second_reply, second_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request.clone(),
        cancellation: second_cancellation.clone(),
        reply: second_reply,
    })
    .expect("second call");
    let (third_reply, third_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request,
        cancellation: CancellationToken::new(),
        reply: third_reply,
    })
    .expect("third call");
    assert_eq!(
        third_receive.await.expect("reply").expect_err("refused"),
        "BudgetExceededError: token limit reached".into()
    );
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let refusal = journal
        .iter()
        .find(|entry| entry.seq == 2)
        .expect("refusal is durable before its reply");
    assert_eq!(refusal.status, "failed");
    assert_eq!(
        refusal.error_message.as_deref(),
        Some("BudgetExceededError: token limit reached")
    );
    first_cancellation.cancel();
    second_cancellation.cancel();
    let first_error = first_receive
        .await
        .expect("first reply")
        .expect_err("cancelled after admission");
    let second_error = second_receive
        .await
        .expect("second reply")
        .expect_err("cancelled after admission");
    assert!(!first_error.to_string().starts_with("BudgetExceededError:"));
    assert!(!second_error.to_string().starts_with("BudgetExceededError:"));
    let settled_journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("settled journal");
    assert_eq!(settled_journal.len(), 3);
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continued_call_lands_on_the_same_worker_session_with_history() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let worker_directory = tempfile::tempdir().expect("worker directory");
    grant_workspace(&storage, topic.id, worker_directory.path()).await;
    let swage_home = crate::test_support::SwageHomeGuard::new();
    let skill_dir = swage_home.path().join("skills/workflow-specialist");
    std::fs::create_dir_all(&skill_dir).expect("create workflow skill directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Workflow specialist\n---\nUse the persisted workflow specialist skill.\n",
    )
    .expect("write workflow skill");
    for (agent, allow) in [("lead", "shell"), ("worker", "shell")] {
        let agent_dir = swage_home.path().join("general/agents").join(agent);
        std::fs::create_dir_all(&agent_dir).expect("create agent config directory");
        std::fs::write(
            agent_dir.join("config.toml"),
            format!("[tools]\nallow = [\"{allow}\"]\ndeny = []\nbundles = {{}}\n"),
        )
        .expect("write agent config");
    }
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            dispatch_bound_agent_ids: vec!["lead".into()],
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            vec![
                swage_core::StreamChunk::ToolUse {
                    call_id: "continued-history-tool".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({}),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                swage_core::StreamChunk::Delta {
                    content: "restated: gather the sources".into(),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                swage_core::StreamChunk::Delta {
                    content: "the work is done".into(),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ],
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(NamedTool::new("shell")) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec!["lead".into()],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: None,
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: None,
        },
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));

    let (first_reply, first_receive) = oneshot::channel();
    tx.send(HostCommand::DispatchAgent {
        request: AgentCallRequest {
            cwd: Some(worker_directory.path().to_string_lossy().into_owned()),
            agent: Some("worker".into()),
            skills: Some(vec!["workflow-specialist".into()]),
            ..swage_workflow::fixtures::agent_call_request("restate the brief".into())
        },
        cancellation: CancellationToken::new(),
        reply: first_reply,
    })
    .expect("first call");
    let first = first_receive
        .await
        .expect("first reply")
        .expect("first result");
    assert_eq!(
        first.working_directory.as_deref(),
        Some(worker_directory.path().to_string_lossy().as_ref()),
        "the first call reports its resolved worker directory"
    );
    let token = AgentSessionRef {
        session_id: first.session_id.expect("session id"),
        thread_id: first.thread_id.expect("thread id"),
    };

    // The worker session records its dispatching-context lineage, which
    // the turn path reads back on every start to intersect grants.
    let worker_session = storage
        .get_session(token.session_id)
        .await
        .expect("session query")
        .expect("worker session");
    assert_eq!(
        worker_session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(crate::turn::DISPATCH_BOUND_METADATA_KEY))
            .and_then(Value::as_array),
        Some(&vec![Value::String("lead".into())]),
    );
    assert_eq!(
        worker_session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("workflow_call_skills")),
        Some(&serde_json::json!(["workflow-specialist"])),
    );

    // Revoke shell after the first worker turn. The continuation must
    // re-resolve this changed bound rather than retaining the authority
    // it had when the worker session was created.
    let lead_config = swage_home.path().join("general/agents/lead/config.toml");
    std::fs::write(
        lead_config,
        "[tools]\nallow = [\"knowledge_search\"]\ndeny = []\nbundles = {}\n",
    )
    .expect("revoke lead shell grant");

    let (second_reply, second_receive) = oneshot::channel();
    tx.send(HostCommand::DispatchAgent {
        request: AgentCallRequest {
            agent: Some("worker".into()),
            session: Some(token.clone()),
            ..swage_workflow::fixtures::agent_call_request("now do the work".into())
        },
        cancellation: CancellationToken::new(),
        reply: second_reply,
    })
    .expect("second call");
    let second = second_receive
        .await
        .expect("second reply")
        .expect("second result");
    assert_eq!(
        second.working_directory.as_deref(),
        first.working_directory.as_deref(),
        "a continuation reports the immutable directory of its existing worker"
    );

    let second_entry_id = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal")
        .into_iter()
        .filter(|entry| entry.kind == "agent_call" && entry.status == "completed")
        .max_by_key(|entry| entry.seq)
        .expect("continued agent call entry")
        .id;
    let continued_session = storage
        .get_session(token.session_id)
        .await
        .expect("session query")
        .expect("continued worker session");
    assert_eq!(
        crate::turn::agent_call_scope(continued_session.metadata.as_ref()).as_deref(),
        Some(second_entry_id.to_string().as_str()),
        "a continuation carries its own journalled call scope"
    );

    let (replacement_reply, replacement_receive) = oneshot::channel();
    tx.send(HostCommand::DispatchAgent {
        request: AgentCallRequest {
            cwd: Some("/unused-replacement".into()),
            isolation: Some(swage_protocol::client::workflow::WorkerIsolation::Worktree),
            agent: Some("worker".into()),
            session: Some(token.clone()),
            ..swage_workflow::fixtures::agent_call_request("move the continued worker".into())
        },
        cancellation: CancellationToken::new(),
        reply: replacement_reply,
    })
    .expect("replacement call");
    assert_eq!(
        replacement_receive
            .await
            .expect("replacement reply")
            .expect_err("continued placement must be immutable"),
        "WorkerPlacementError: a continued worker keeps its original placement; cwd and isolation cannot be supplied with session".into()
    );
    assert_eq!(
        storage
            .list_workflow_journal(run.id)
            .await
            .expect("journal")
            .iter()
            .filter(|entry| entry.kind == "worker_placement")
            .count(),
        1,
        "a rejected continuation must not provision or journal an unused worktree"
    );

    // Same session, same thread: the restating worker IS the working one.
    assert_eq!(second.session_id, Some(token.session_id));
    assert_eq!(second.thread_id, Some(token.thread_id));
    assert_eq!(second.output, Value::String("the work is done".into()));
    let continued_result = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal")
        .into_iter()
        .filter(|entry| entry.kind == "agent_call" && entry.status == "completed")
        .max_by_key(|entry| entry.seq)
        .and_then(|entry| entry.result)
        .expect("continued agent call result");
    assert_eq!(
        continued_result["working_directory"],
        worker_directory.path().to_string_lossy().as_ref(),
        "the continued call journals its known worker directory"
    );
    let turns = storage
        .list_turns_by_thread(token.thread_id)
        .await
        .expect("turns");
    assert_eq!(turns.len(), 2, "both turns live on the one thread");

    // The continued turn met its own history: the second provider request
    // carries the first exchange, not a fresh context.
    let requests = provider.requests.lock().expect("requests");
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .tool_definitions
            .iter()
            .any(|definition| definition["name"] == "shell"),
        "shell is available while the lead still grants it"
    );
    assert!(
        requests[2]
            .tool_definitions
            .iter()
            .all(|definition| definition["name"] != "shell"),
        "the continued turn must observe the lead's revoked shell grant"
    );
    let second_request = &requests[2];
    let anthropic = swage_provider::anthropic::AnthropicProvider::new("test-key", "claude-test");
    let outbound =
        swage_provider::LlmProvider::provider_request_snapshot(&anthropic, second_request, true);
    if let Some(path) = std::env::var_os("SWAGE_PROVIDER_REQUEST_CAPTURE") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&outbound["body"])
                .expect("serialise outbound provider request"),
        )
        .expect("write outbound provider request capture");
    }
    let second_request_text = second_request
        .messages
        .iter()
        .map(swage_core::ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second_request_text.contains("restate the brief"));
    assert!(second_request_text.contains("restated: gather the sources"));
    assert!(
        second_request_text.contains("Use the persisted workflow specialist skill."),
        "the continued turn must compose the persisted skill selection: {second_request_text}"
    );
    let replayed_tool_names = second_request
        .messages
        .iter()
        .filter_map(swage_core::ChatMessage::tool_replay)
        .flat_map(|replay| replay.iter())
        .filter_map(|part| match part {
            swage_core::ToolReplayPart::ToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replayed_tool_names, vec!["shell"]);
    assert!(
        replayed_tool_names
            .iter()
            .all(|name| !name.trim().is_empty()),
        "every replayed tool_use entry in the continued provider request must carry its name"
    );
    let outbound_tool_names = outbound["body"]["messages"]
        .as_array()
        .expect("Anthropic request messages")
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "tool_use")
        .map(|block| block["name"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(outbound_tool_names, vec!["shell"]);
    assert!(
        outbound_tool_names
            .iter()
            .all(|name| !name.trim().is_empty()),
        "every tool_use in the Anthropic outbound body must carry its persisted name"
    );

    drop(tx);
    task.await.expect("host loop");
}

/// A run whose script is `source`, with a bounded lead and a provider
/// serving `outputs` in order. Returns the storage, the run, and the
/// provider so a test can read the journal and the provider's own view.
async fn run_held_worker_script(
    source: &str,
    outputs: Vec<Vec<swage_core::StreamChunk>>,
) -> (
    Arc<SqliteStorage>,
    WorkflowRunRecord,
    Arc<ScriptedRecordingProvider>,
    crate::test_support::SwageHomeGuard,
) {
    run_held_worker_script_with_config(source, outputs, swage_config::ResolvedConfig::default())
        .await
}

/// As `run_held_worker_script`, with every provider turn taking `delay`.
async fn run_held_worker_script_with_slow_turns(
    source: &str,
    outputs: Vec<Vec<swage_core::StreamChunk>>,
    config: swage_config::ResolvedConfig,
    delay: std::time::Duration,
) -> (
    Arc<SqliteStorage>,
    WorkflowRunRecord,
    Arc<ScriptedRecordingProvider>,
    crate::test_support::SwageHomeGuard,
) {
    run_held_worker_run(source, outputs, config, Some(delay)).await
}

/// A provider whose every turn takes `delay` to answer, so a test can tell
/// a reading taken when a turn started from one taken when it settled.
struct SlowScriptedProvider {
    inner: Arc<ScriptedRecordingProvider>,
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for SlowScriptedProvider {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    async fn complete(
        &self,
        request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        self.inner.complete(request).await
    }

    async fn complete_with_tools(
        &self,
        request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        self.inner.complete_with_tools(request).await
    }

    fn stream(
        &self,
        request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        let inner = self.inner.stream(request);
        let delay = self.delay;
        Box::pin(futures::StreamExt::flatten(futures::stream::once(
            async move {
                tokio::time::sleep(delay).await;
                inner
            },
        )))
    }
}

/// As `run_held_worker_script`, with the runtime configuration the run
/// starts against — a catalogue the worker's model resolves through.
async fn run_held_worker_script_with_config(
    source: &str,
    outputs: Vec<Vec<swage_core::StreamChunk>>,
    config: swage_config::ResolvedConfig,
) -> (
    Arc<SqliteStorage>,
    WorkflowRunRecord,
    Arc<ScriptedRecordingProvider>,
    crate::test_support::SwageHomeGuard,
) {
    run_held_worker_run(source, outputs, config, None).await
}

async fn run_held_worker_run(
    source: &str,
    outputs: Vec<Vec<swage_core::StreamChunk>>,
    config: swage_config::ResolvedConfig,
    delay: Option<std::time::Duration>,
) -> (
    Arc<SqliteStorage>,
    WorkflowRunRecord,
    Arc<ScriptedRecordingProvider>,
    crate::test_support::SwageHomeGuard,
) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let swage_home = crate::test_support::SwageHomeGuard::new();
    for agent in ["lead", "worker"] {
        let agent_dir = swage_home.path().join("general/agents").join(agent);
        std::fs::create_dir_all(&agent_dir).expect("create agent config directory");
        std::fs::write(
            agent_dir.join("config.toml"),
            "[tools]\nallow = [\"knowledge_search\"]\ndeny = []\nbundles = {}\n",
        )
        .expect("write agent config");
    }
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            dispatch_bound_agent_ids: vec!["lead".into()],
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(outputs)),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        match delay {
            Some(delay) => Arc::new(SlowScriptedProvider {
                inner: Arc::clone(&provider),
                delay,
            }) as Arc<dyn swage_provider::LlmProvider>,
            None => Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        },
        config,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(NamedTool::new("knowledge_search")) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );
    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("launch held-worker run");
    wait_for_terminal(&state, run.id).await;
    let finished = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("finished run");
    (storage, finished, provider, swage_home)
}

fn worker_journal_actions(journal: &[WorkflowJournalRecord]) -> Vec<(String, String)> {
    journal
        .iter()
        .filter(|entry| entry.kind == "worker")
        .map(|entry| {
            (
                entry
                    .request
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry.status.clone(),
            )
        })
        .collect()
}

#[test]
fn held_worker_replay_accepts_the_same_model_aliases_as_opening() {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.entries.insert(
        "steady".into(),
        swage_config::ModelEndpointConfig {
            aliases: vec!["steady-alias".into()],
            ..Default::default()
        },
    );
    config.models.entries.insert(
        "backup".into(),
        swage_config::ModelEndpointConfig {
            aliases: vec!["backup-alias".into()],
            ..Default::default()
        },
    );
    let requested = HeldWorkerOpen {
        model: Some("steady-alias".into()),
        fallback_model: Some("backup-alias".into()),
        ..HeldWorkerOpen::default()
    };
    let recorded = HeldWorkerOpen {
        model: Some("steady".into()),
        fallback_model: Some("backup".into()),
        ..HeldWorkerOpen::default()
    };

    assert!(held_worker_replay_matches(&config, &requested, &recorded));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_worker_carries_its_first_turn_into_its_second_and_records_its_life() {
    let source = r#"
        const worker = await session({agent: "worker", label: "reviewer"});
        await send(worker, "restate the brief");
        const reading = await staleness(worker);
        const second = await send(worker, "now do the work");
        await close(worker);
        return {second, reading};
    "#;
    let (storage, run, provider, _home) = run_held_worker_script(
        source,
        vec![
            scripted_text_output("restated: gather the sources"),
            scripted_text_output("the work is done"),
        ],
    )
    .await;
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let output = run.final_output.as_ref().expect("held-worker output");
    assert_eq!(output["second"], "the work is done");
    assert_eq!(output["reading"]["stale"], Value::Null);
    assert_eq!(output["reading"]["thresholdMs"], Value::Null);

    let requests = provider.requests.lock().expect("requests");
    assert_eq!(requests.len(), 2, "two turns reached the provider");
    let second_turn = requests[1]
        .messages
        .iter()
        .map(swage_core::ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_turn.contains("restate the brief")
            && second_turn.contains("restated: gather the sources"),
        "the second turn carries the first turn's exchange: {second_turn}"
    );
    drop(requests);

    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    assert_eq!(
        worker_journal_actions(&journal),
        vec![
            ("open".to_string(), "completed".to_string()),
            ("staleness".to_string(), "completed".to_string()),
            ("close".to_string(), "completed".to_string()),
        ],
        "creation, the staleness reading, and closure are all recorded facts"
    );
    assert_eq!(
        journal
            .iter()
            .filter(|entry| entry.kind == "agent_call" && entry.status == "completed")
            .count(),
        2,
        "each held turn is an ordinary journalled agent call"
    );
    let closure = journal
        .iter()
        .find(|entry| {
            entry.kind == "worker"
                && entry.request.get("action").and_then(Value::as_str) == Some("close")
        })
        .expect("closure entry");
    assert_eq!(closure.result.as_ref().unwrap()["closed_by"], "script");

    let opened = journal
        .iter()
        .find(|entry| {
            entry.kind == "worker"
                && entry.request.get("action").and_then(Value::as_str) == Some("open")
        })
        .expect("open entry");
    let worker_id = opened.result.as_ref().unwrap()["worker_id"]
        .as_str()
        .expect("worker id");
    assert!(
        journal
            .iter()
            .filter(|entry| entry.kind == "agent_call")
            .all(|entry| entry.request["worker"] == worker_id),
        "every turn names the worker it belongs to"
    );

    let original_output = run.final_output.clone();
    let original_worker_id = opened.id;
    let original_session_id = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .and_then(|entry| entry.result.as_ref())
        .and_then(|result| result["session_id"].as_str())
        .map(str::to_owned)
        .expect("recorded worker session");
    let original_staleness = journal
        .iter()
        .find(|entry| {
            entry.kind == "worker"
                && entry.request.get("action").and_then(Value::as_str) == Some("staleness")
        })
        .and_then(|entry| entry.result.clone())
        .expect("recorded staleness");
    storage
        .update_workflow_run_state(run.id, "interrupted", None, None, None)
        .await
        .expect("simulate process interruption");
    let replay_state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(NamedTool::new("knowledge_search")) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );
    let _resumed = resume_run(State(replay_state.clone()), Path(run.id))
        .await
        .expect("resume held-worker run");
    wait_for_terminal(&replay_state, run.id).await;
    let replayed = storage
        .get_workflow_run(run.id)
        .await
        .expect("run")
        .expect("replayed run");
    assert_eq!(
        replayed.state, "completed",
        "replay error: {:?}",
        replayed.error_message
    );
    assert_eq!(replayed.final_output, original_output);
    assert_eq!(provider.requests.lock().expect("requests").len(), 2);
    let replay_journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let replay_open = replay_journal
        .iter()
        .find(|entry| {
            entry.kind == "worker"
                && entry.request.get("action").and_then(Value::as_str) == Some("open")
        })
        .expect("replayed open");
    assert_eq!(replay_open.id, original_worker_id);
    assert_eq!(
        replay_journal
            .iter()
            .find(|entry| entry.kind == "agent_call")
            .and_then(|entry| entry.result.as_ref())
            .and_then(|result| result["session_id"].as_str()),
        Some(original_session_id.as_str()),
    );
    assert_eq!(
        replay_journal
            .iter()
            .find(|entry| {
                entry.kind == "worker"
                    && entry.request.get("action").and_then(Value::as_str) == Some("staleness")
            })
            .and_then(|entry| entry.result.as_ref()),
        Some(&original_staleness),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_throws_closes_the_worker_it_still_holds() {
    let source = r#"
        const worker = await session({agent: "worker"});
        await send(worker, "start something");
        throw new Error("the script gave up");
    "#;
    let (storage, run, _provider, _home) =
        run_held_worker_script(source, vec![scripted_text_output("started")]).await;
    assert_eq!(run.state, "failed");

    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let closure = journal
        .iter()
        .find(|entry| {
            entry.kind == "worker"
                && entry.request.get("action").and_then(Value::as_str) == Some("close")
        })
        .expect("a run that throws still closes its held worker");
    assert_eq!(closure.status, "completed");
    assert_eq!(closure.result.as_ref().unwrap()["closed_by"], "run_end");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_end_closure_is_journalled_between_the_last_turn_and_the_terminal_record() {
    let source = r#"
        const worker = await session({agent: "worker"});
        await send(worker, "start something");
        throw new Error("the script gave up");
    "#;
    let (storage, run, _provider, _home) =
        run_held_worker_script(source, vec![scripted_text_output("started")]).await;
    assert_eq!(run.state, "failed");

    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    assert!(
        journal
            .iter()
            .filter(|entry| is_positional_journal_entry(entry))
            .all(|entry| entry.seq < TERMINAL_BUDGET_SEQUENCE),
        "no script entry is numbered past the reserved terminal record: {:?}",
        journal.iter().map(|entry| entry.seq).collect::<Vec<_>>()
    );
    let ordered = journal
        .iter()
        .map(|entry| {
            (
                entry.kind.as_str(),
                entry
                    .request
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ordered,
        vec![
            ("worker", "open"),
            ("agent_call", ""),
            ("worker", "close"),
            ("terminal_budget", ""),
        ],
        "the journal reads in the order the run lived it"
    );
}

/// A catalogue with one configured default route, so a worker opened
/// without naming a model has something to be pinned to.
fn held_worker_route_config(threshold_ms: Option<u64>) -> swage_config::ResolvedConfig {
    let mut config = swage_config::ResolvedConfig::default();
    config.providers.default = "local".to_string();
    config.providers.entries.insert(
        "local".to_string(),
        swage_config::ProviderEndpointConfig {
            worker_staleness_threshold_ms: threshold_ms,
            protocol: swage_config::Protocol::OpenAi,
            base_url: Some("http://127.0.0.1:65535/v1".to_string()),
            api_key_secret_ref: None,
            api_key_env: None,
            command: None,
            state_dir: None,
        },
    );
    config.models.default = "steady".to_string();
    config.models.entries.insert(
        "steady".to_string(),
        swage_config::ModelEndpointConfig {
            provider_id: "local".to_string(),
            model_id: "steady-wire".to_string(),
            aliases: vec![],
            context_window: Some(4096),
            generation_preset: None,
            default_effort: Some(swage_protocol::client::settings::Effort::Low),
            capabilities: swage_config::ModelCapabilitiesConfig {
                reasoning_replay: false,
                effort: None,
            },
        },
    );
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_opened_without_a_model_is_pinned_to_the_route_it_opened_against() {
    let source = r#"
        const worker = await session({agent: "default"});
        await send(worker, "first");
        await send(worker, "second");
        await close(worker);
        return worker.creation;
    "#;
    let (storage, run, provider, _home) = run_held_worker_script_with_config(
        source,
        vec![
            scripted_text_output("first done"),
            scripted_text_output("second done"),
        ],
        held_worker_route_config(None),
    )
    .await;
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let creation = run.final_output.as_ref().expect("creation envelope");
    assert_eq!(
        creation["model"], "steady",
        "an omitted model is resolved and pinned when the worker opens"
    );
    assert_eq!(
        creation["effort"], "low",
        "the route's default effort is fixed at creation too"
    );

    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let opened = journal
        .iter()
        .find(|entry| {
            entry.kind == "worker"
                && entry.request.get("action").and_then(Value::as_str) == Some("open")
        })
        .expect("open entry");
    assert_eq!(
        opened.request["open"]["model"], "steady",
        "the recorded creation names the model the worker is held on"
    );

    let session_id = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .and_then(|entry| entry.result.as_ref())
        .and_then(|result| result["session_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("worker session id");
    assert_eq!(
        storage
            .get_session(session_id)
            .await
            .expect("session")
            .expect("worker session")
            .selected_model_id
            .as_deref(),
        Some("steady"),
        "the worker's own session persists the model it was opened on, so a \
         later change of the configured default cannot move its next turn"
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .expect("requests")
            .iter()
            .map(|request| request.model.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("steady-wire"), Some("steady-wire")],
        "every turn runs on the pinned route"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_call_on_a_worker_session_cannot_change_what_the_worker_was_opened_with() {
    let source = r#"
        const worker = await session({agent: "worker", tools: ["knowledge_search"]});
        await send(worker, "first");
        const refusals = {};
        try {
            await agent("harder", {
                agent: "worker",
                session: worker.session,
                tools: ["knowledge_search"],
                effort: "high",
            });
        } catch (error) { refusals.effort = error.message; }
        try {
            await agent("elsewhere", {
                agent: "worker",
                worker: worker.workerId,
                tools: [],
            });
        } catch (error) { refusals.tools = error.message; }
        await close(worker);
        return refusals;
    "#;
    let (_storage, run, provider, _home) = run_held_worker_script_with_config(
        source,
        vec![scripted_text_output("first done")],
        held_worker_route_config(None),
    )
    .await;
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let refusals = run.final_output.as_ref().expect("refusals");
    for field in ["effort", "tools"] {
        let refusal = refusals[field].as_str().unwrap_or_default();
        assert!(
            refusal.contains("HeldWorkerError") && refusal.contains(field),
            "a direct call carrying only the worker's token still meets the \
             worker's creation bounds ({field}): {refusal}"
        );
    }
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "neither escape reaches the provider"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_staleness_counts_idle_time_from_the_turn_that_settled() {
    let turn_duration = std::time::Duration::from_millis(700);
    let source = r#"
        const worker = await session({agent: "default"});
        await send(worker, "take your time");
        const reading = await staleness(worker);
        await close(worker);
        return reading;
    "#;
    let (_storage, run, _provider, _home) = run_held_worker_script_with_slow_turns(
        source,
        vec![scripted_text_output("done at last")],
        held_worker_route_config(Some(600_000)),
        turn_duration,
    )
    .await;
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let reading = run.final_output.as_ref().expect("staleness reading");
    assert_eq!(reading["thresholdMs"], 600_000);
    assert_eq!(reading["stale"], false);
    let idle_ms = reading["idleMs"].as_u64().expect("idle reading");
    assert!(
        idle_ms < turn_duration.as_millis() as u64 / 2,
        "idle time is the gap since the last turn settled, not the time \
         that turn spent working ({idle_ms}ms after a {}ms turn)",
        turn_duration.as_millis()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_staleness_uses_the_hot_applied_route_threshold() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let swage_home = crate::test_support::SwageHomeGuard::new();
    let worker_dir = swage_home.path().join("general/agents/worker");
    std::fs::create_dir_all(&worker_dir).expect("create worker config directory");
    std::fs::write(worker_dir.join("config.toml"), "").expect("write worker config");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        held_worker_route_config(None),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                const worker = await session({agent: "worker"});
                await wait({timerMs: 1_000});
                const reading = await staleness(worker);
                await close(worker);
                return reading;
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start workflow");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if storage
                .list_workflow_journal(started.id)
                .await
                .expect("journal")
                .iter()
                .any(|entry| {
                    entry.kind == "worker"
                        && entry.request.get("action").and_then(Value::as_str) == Some("open")
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker opens");
    state.replace_runtime(
        held_worker_route_config(Some(600_000)),
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
    );
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    assert_eq!(
        run.final_output.as_ref().expect("staleness reading")["thresholdMs"],
        600_000,
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_worker_turn_cannot_escalate_its_model_even_by_its_own_session_token() {
    let source = r#"
        const worker = await session({agent: "worker"});
        await send(worker, "first");
        let refusal = null;
        try {
            await agent("escalate", {
                agent: "worker",
                session: worker.session,
                model: "strong",
            });
        } catch (error) { refusal = error.message; }
        await close(worker);
        return refusal;
    "#;
    let (storage, run, provider, _home) =
        run_held_worker_script(source, vec![scripted_text_output("first done")]).await;
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let refusal = run
        .final_output
        .as_ref()
        .and_then(Value::as_str)
        .expect("refusal message");
    assert!(
        refusal.contains("HeldWorkerError") && refusal.contains("model"),
        "escalation is refused by the host, not merely by the script surface: {refusal}"
    );
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "the escalating turn never reaches the provider"
    );
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    assert_eq!(
        journal
            .iter()
            .filter(|entry| entry.kind == "agent_call" && entry.status == "completed")
            .count(),
        1,
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bare_worker_completes_with_dispatch_bounded_tools_and_an_agentless_journal() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let swage_home = crate::test_support::SwageHomeGuard::new();
    let lead_dir = swage_home.path().join("general/agents/lead");
    std::fs::create_dir_all(&lead_dir).expect("create lead config directory");
    std::fs::write(
        lead_dir.join("config.toml"),
        "[tools]\nallow = [\"knowledge_search\"]\ndeny = []\nbundles = {}\n",
    )
    .expect("write bounded lead config");
    let source = "return await agent('bounded work');";
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            dispatch_bound_agent_ids: vec!["lead".into()],
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "bounded work completed",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(NamedTool::new("shell")) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );
    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("launch bare workflow run");
    wait_for_terminal(&state, run.id).await;
    let completed_run = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("completed run");
    assert_eq!(completed_run.state, "completed");
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let agent_call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("bare agent call");
    assert_eq!(agent_call.status, "completed");
    assert!(agent_call.request.get("agent").is_none());
    assert_eq!(
        journal_wire(agent_call.clone(), None)
            .expect("journal wire projection")
            .agent_call
            .expect("agent-call detail")
            .agent_definition_id,
        None,
        "a bare call must not project an invented agent definition identity"
    );
    assert_eq!(agent_call.request["prompt"], "bounded work");
    assert_eq!(
        agent_call.result.as_ref().unwrap()["output"],
        "bounded work completed"
    );
    let session_id = agent_call.result.as_ref().unwrap()["session_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("journalled worker session id");
    let session = storage
        .get_session(session_id)
        .await
        .expect("session query")
        .expect("bare worker session");
    assert_eq!(session.agent_id, BARE_WORKFLOW_AGENT_ID);
    let requests = provider.requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].tool_definitions.is_empty(),
        "shell must be absent because the dispatching lead does not grant it"
    );
    drop(requests);

    storage
        .update_workflow_run_state(run.id, "running", None, None, None)
        .await
        .expect("mark replay running");
    let replay_record = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("replay run");
    launch_existing_run(state.clone(), replay_record, source.into(), None)
        .await
        .expect("replay completed workflow run");
    wait_for_terminal(&state, run.id).await;
    let replayed = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("replayed run");
    assert_eq!(replayed.state, "completed");
    assert_eq!(
        replayed.final_output,
        Some(Value::String("bounded work completed".into()))
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn identified_schema_exhaustion_returns_its_failure_envelope_and_replays_without_a_new_agent_call()
 {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output("not JSON"),
            scripted_text_output("still not JSON"),
            scripted_text_output(r#"{"answer":"fallback must not run"}"#),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        fallback_test_config(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"return await agent("return a JSON object", {
        model: "pinned", fallbackModel: "fallback",
        schema: {type: "object", required: ["answer"]}, maxAttempts: 2,
        identity: true, label: "schema leg", phase: "fan-in"
    });"#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start schema-constrained run");
    wait_for_terminal(&state, started.id).await;

    let completed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(completed.state, "completed");
    assert_eq!(
        completed
            .final_output
            .as_ref()
            .and_then(|value| value.get("label")),
        Some(&Value::String("schema leg".into()))
    );
    assert_eq!(
        completed
            .final_output
            .as_ref()
            .and_then(|value| value.get("phase")),
        Some(&Value::String("fan-in".into()))
    );
    assert_eq!(
        completed
            .final_output
            .as_ref()
            .and_then(|value| value.get("output")),
        Some(&Value::Null)
    );
    assert!(
        completed
            .final_output
            .as_ref()
            .and_then(|value| value["failure"]["message"].as_str())
            .is_some_and(|message| message.starts_with("SchemaExhaustedError: agent turn "))
    );
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    assert_eq!(journal.len(), 2);
    let agent_call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("failed schema-constrained agent call");
    assert_eq!(agent_call.status, "failed");
    assert!(
        agent_call
            .error_message
            .as_deref()
            .is_some_and(|error| error.starts_with("SchemaExhaustedError:"))
    );
    assert_eq!(
        journal
            .iter()
            .find(|entry| entry.kind == "terminal_budget")
            .expect("terminal budget audit")
            .status,
        "completed"
    );
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        2,
        "schema exhaustion must not invoke the declared fallback"
    );

    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("replay schema-constrained run");
    wait_for_terminal(&state, started.id).await;

    let replayed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(replayed.state, "completed");
    assert_eq!(replayed.final_output, completed.final_output);
    let replayed_journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    assert_eq!(replayed_journal.len(), 2);
    assert_eq!(
        replayed_journal
            .iter()
            .filter(|entry| entry.kind == "agent_call")
            .count(),
        1
    );
    assert_eq!(
        replayed_journal
            .iter()
            .filter(|entry| entry.kind == "terminal_budget")
            .count(),
        1
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_launched_by_a_placed_worker_keeps_its_exact_filesystem_authority() {
    let fixture = tempfile::tempdir().expect("workspace fixture");
    let workspace = fixture.path().join("workspace");
    let placed = workspace.join("placed");
    let sibling = workspace.join("sibling");
    std::fs::create_dir_all(&placed).expect("create placed directory");
    std::fs::create_dir_all(&sibling).expect("create sibling directory");

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let workspace_id = grant_workspace(storage.as_ref(), topic.id, &placed).await;
    grant_workspace(storage.as_ref(), topic.id, &sibling).await;
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "placed-launch".into(),
            description: String::new(),
            source: "return await agent('attempt sibling read', { maxAttempts: 1 });".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("store workflow");
    let authority_context_id = Uuid::new_v4();
    let placed = std::fs::canonicalize(&placed).expect("canonical placed directory");
    let session = storage
        .create_session(NewSession {
            metadata: Some(serde_json::json!({
                "worker_placement_cwd": placed.to_string_lossy(),
                "worker_placement_can_write": true,
                "worker_placement_authority_path": placed.to_string_lossy(),
                "worker_placement_authority_context_id": authority_context_id,
            })),
            active_workspace_attachment_id: Some(workspace_id),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("placed session");
    let outputs = std::collections::VecDeque::from([scripted_text_output("work complete")]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(ScriptedRecordingProvider {
            outputs: std::sync::Mutex::new(outputs),
            requests: std::sync::Mutex::new(vec![]),
        }),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let started = GatewayWorkflowLauncher::new(state.clone())
        .start_by_name(session.id, "placed-launch", Value::Null, None, false)
        .await
        .expect("start workflow from placed session");
    let run_id: Uuid = serde_json::from_value(started["id"].clone()).expect("run id");
    wait_for_terminal(&state, run_id).await;
    let run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run query")
        .expect("run");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let inherited = run
        .inherited_worker_placement
        .expect("durable inherited placement");
    assert_eq!(inherited.path, placed.to_string_lossy());
    let worker_session_id = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal")
        .into_iter()
        .find(|entry| entry.kind == "agent_call")
        .and_then(|entry| entry.result)
        .and_then(|result| result["session_id"].as_str().map(str::to_owned))
        .and_then(|id| Uuid::parse_str(&id).ok())
        .expect("worker session id");
    let worker_session = storage
        .get_session(worker_session_id)
        .await
        .expect("worker session query")
        .expect("worker session");
    assert_eq!(
        worker_session
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("worker_placement_cwd"))
            .and_then(Value::as_str),
        Some(placed.to_string_lossy().as_ref()),
        "the child session must carry the placed root so the shared turn path clears sibling attachments"
    );

    storage
        .delete_workspace_attachment(workspace_id)
        .await
        .expect("revoke workspace");
    let child_error = reauthorise_recorded_worker_placement(
        &state,
        topic.id,
        runtime_worker_placement(&inherited),
    )
    .await
    .expect_err("each later child start must re-authorise the durable placement");
    assert!(child_error.contains("permitted workspace roots"));
    let error = GatewayWorkflowLauncher::new(state)
        .start_by_name(session.id, "placed-launch", Value::Null, None, false)
        .await
        .expect_err("revoked placement must fail closed");
    assert!(error.contains("worker placement is no longer authorised"));
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_launches_carry_task_bounds_and_fail_closed_when_the_task_disappears() {
    let _guard = crate::test_support::SwageHomeGuard::new();
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    storage
        .upsert_task_definition(swage_storage::NewTaskDefinition {
            id: "restricted".into(),
            instructions: "Remain within the restricted task.".into(),
            model_id: None,
            tool_permissions: Some(serde_json::json!({
                "allow": [], "deny": [], "bundles": {}, "require_approval": []
            })),
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store task definition");
    let source = "return await agent('bounded workflow work');";
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "bounded-workflow".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("store workflow");
    let session = storage
        .create_session(NewSession {
            metadata: Some(serde_json::json!({"task_definition": "restricted"})),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("session");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(session.id);
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "bounded work completed",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let config = swage_config::ResolvedConfig {
        tools: swage_config::ToolConfig {
            permissions: swage_config::ToolPermissionConfig {
                allow: vec!["shell".into()],
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(NamedTool::new("shell")) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let started = GatewayWorkflowLauncher::new(state.clone())
        .start_by_name(session.id, "bounded-workflow", Value::Null, None, false)
        .await
        .expect("start task-bounded workflow");
    let run_id: Uuid = serde_json::from_value(started["id"].clone()).expect("run id");
    wait_for_terminal(&state, run_id).await;
    let run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run query")
        .expect("run");
    assert_eq!(run.dispatch_bound_task_definition_ids, vec!["restricted"]);
    assert_eq!(run.state, "completed");
    let journal = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal");
    let worker_session_id = journal[0].result.as_ref().unwrap()["session_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("worker session id");
    let worker_session = storage
        .get_session(worker_session_id)
        .await
        .expect("worker session query")
        .expect("worker session");
    assert_eq!(
        crate::turn::dispatch_bound_task_definition_ids(worker_session.metadata.as_ref())
            .expect("worker task bounds"),
        vec!["restricted"]
    );
    assert!(
        provider.requests.lock().expect("requests")[0]
            .tool_definitions
            .is_empty(),
        "the caller's task overlay must remove shell from the workflow worker"
    );

    assert!(
        storage
            .delete_task_definition("restricted")
            .await
            .expect("delete task definition")
    );
    let started = GatewayWorkflowLauncher::new(state.clone())
        .start_by_name(session.id, "bounded-workflow", Value::Null, None, false)
        .await
        .expect("start workflow with stale task bound");
    let stale_run_id: Uuid = serde_json::from_value(started["id"].clone()).expect("stale run id");
    wait_for_terminal(&state, stale_run_id).await;
    let stale_run = storage
        .get_workflow_run(stale_run_id)
        .await
        .expect("stale run query")
        .expect("stale run");
    assert_eq!(stale_run.state, "failed");
    assert!(
        stale_run
            .error_message
            .as_deref()
            .is_some_and(|error| error.contains("no task definition 'restricted' exists"))
    );
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "a missing inherited task bound must fail before provider dispatch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_launches_preserve_call_scoped_tools_through_nested_runs() {
    let _guard = crate::test_support::SwageHomeGuard::new();
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "inner-bounded".into(),
            description: String::new(),
            source: "return await agent('nested bounded work');".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("store inner workflow");
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "outer-bounded".into(),
            description: String::new(),
            source: "return await workflow('inner-bounded', {});".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("store outer workflow");
    let session = storage
        .create_session(NewSession {
            metadata: Some(serde_json::json!({
                crate::turn::WORKFLOW_CALL_TOOLS_METADATA_KEY: ["workflow_run"]
            })),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("call-scoped worker session");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(session.id);
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "nested bounded work completed",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let config = swage_config::ResolvedConfig {
        tools: swage_config::ToolConfig {
            permissions: swage_config::ToolPermissionConfig {
                allow: vec!["workflow_run".into(), "shell".into()],
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(crate::tool::workflow_run::WorkflowRunTool) as Arc<dyn crate::tool::Tool>,
            Arc::new(NamedTool::new("shell")) as Arc<dyn crate::tool::Tool>,
        ]),
        None,
    );

    let started = GatewayWorkflowLauncher::new(state.clone())
        .start_by_name(session.id, "outer-bounded", Value::Null, None, false)
        .await
        .expect("launch workflow from call-scoped worker");
    let parent_id: Uuid = serde_json::from_value(started["id"].clone()).expect("run id");
    wait_for_terminal(&state, parent_id).await;

    let runs = storage
        .list_workflow_runs(topic.id)
        .await
        .expect("workflow runs");
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| {
        run.workflow_call_tools.as_deref() == Some(["workflow_run".to_string()].as_slice())
    }));
    let request = provider
        .requests
        .lock()
        .expect("requests")
        .first()
        .cloned()
        .expect("nested worker request");
    let advertised_tools = request
        .tool_definitions
        .iter()
        .map(|definition| definition["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(advertised_tools, vec!["workflow_run"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_dispatch_re_resolves_and_applies_named_tasks() {
    let _guard = crate::test_support::SwageHomeGuard::new();
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    storage
        .upsert_task_definition(swage_storage::NewTaskDefinition {
            id: "review".into(),
            instructions: "First workflow task instruction.".into(),
            model_id: Some("first".into()),
            tool_permissions: Some(serde_json::json!({
                "allow": [], "deny": [], "bundles": {}, "require_approval": []
            })),
            output_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["result"],
                "properties": {"result": {"type": "string"}}
            })),
            output_max_retries: Some(1),
        })
        .await
        .expect("store first definition");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output("not valid JSON"),
            scripted_text_output(r#"{"call_level_override":true}"#),
            scripted_text_output(r#"{"call_level_override":true}"#),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let config = swage_config::ResolvedConfig {
        instructions: "Base worker instruction.".into(),
        models: swage_config::ModelCatalogConfig {
            default: "first".into(),
            entries: [
                (
                    "first".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "test".into(),
                        model_id: "first-wire".into(),
                        ..Default::default()
                    },
                ),
                (
                    "second".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "test".into(),
                        model_id: "second-wire".into(),
                        ..Default::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
        },
        tools: swage_config::ToolConfig {
            permissions: swage_config::ToolPermissionConfig {
                allow: vec!["shell".into()],
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        crate::test_support::with_mock_provider_entries(config),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(NamedTool::new("shell")) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );
    let request = || AgentCallRequest {
        task: Some("review".into()),
        // A call-level schema governs this invocation, while the named
        // task still supplies its configured retry count.
        schema: Some(serde_json::json!({
            "type": "object",
            "required": ["call_level_override"],
            "properties": {"call_level_override": {"const": true}}
        })),
        ..swage_workflow::fixtures::agent_call_request("Use the current review contract".into())
    };

    let (fixture_run_id, fixture_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let first = run_ephemeral_agent_call(
        &state,
        fixture_run_id,
        topic.id,
        &[],
        &[],
        fixture_entry_id,
        request(),
        None,
        CancellationToken::new(),
    )
    .await
    .expect("first named dispatch");
    assert_eq!(
        first.output,
        serde_json::json!({"call_level_override":true})
    );
    {
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(
            requests.len(),
            2,
            "the task's one retry must apply even though the call requested one total attempt"
        );
        let text = requests[0]
            .messages
            .iter()
            .map(swage_core::ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("First workflow task instruction."));
        assert!(
            text.contains("call_level_override"),
            "the provider contract must contain the call-level schema: {text}"
        );
        assert_eq!(requests[0].model.as_deref(), Some("first-wire"));
        assert!(
            requests[0].tool_definitions.is_empty(),
            "the task's empty allow-set must narrow the base shell grant"
        );
    }
    assert!(
        storage
            .list_audit_events()
            .await
            .unwrap()
            .iter()
            .any(|event| {
                event.event_kind == "variant_selected"
                    && event.entity_id == crate::experimentation::LOCATION_TASK_PROMPT
            })
    );

    storage
        .upsert_task_definition(swage_storage::NewTaskDefinition {
            id: "review".into(),
            instructions: "Edited workflow task instruction.".into(),
            model_id: Some("second".into()),
            tool_permissions: Some(serde_json::json!({
                "allow": [], "deny": [], "bundles": {}, "require_approval": []
            })),
            output_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["updated"],
                "properties": {"updated": {"const": true}}
            })),
            output_max_retries: Some(0),
        })
        .await
        .expect("edit definition");
    let (fixture_run_id, fixture_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let second = run_ephemeral_agent_call(
        &state,
        fixture_run_id,
        topic.id,
        &[],
        &[],
        fixture_entry_id,
        request(),
        None,
        CancellationToken::new(),
    )
    .await
    .expect("redispatch edited definition");
    assert_eq!(
        second.output,
        serde_json::json!({"call_level_override":true})
    );
    let requests = provider.requests.lock().expect("requests");
    let edited = &requests[2];
    let edited_text = edited
        .messages
        .iter()
        .map(swage_core::ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(edited_text.contains("Edited workflow task instruction."));
    assert!(!edited_text.contains("First workflow task instruction."));
    assert_eq!(edited.model.as_deref(), Some("second-wire"));
    drop(requests);

    let missing = AgentCallRequest {
        task: Some("missing".into()),
        ..request()
    };
    let (fixture_run_id, fixture_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let error = run_ephemeral_agent_call(
        &state,
        fixture_run_id,
        topic.id,
        &[],
        &[],
        fixture_entry_id,
        missing,
        None,
        CancellationToken::new(),
    )
    .await
    .expect_err("missing task must fail before provider dispatch");
    assert!(
        error
            .to_string()
            .contains("no task definition 'missing' exists"),
        "unexpected missing-task error: {error}"
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_provisioning_journal_replays_lists_and_rejects_changed_arguments() {
    let fixture = tempfile::tempdir().expect("scratch fixture");
    let repo = fixture.path().join("repo");
    seed_git_repo(&repo);

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let repo_grant = grant_workspace(storage.as_ref(), topic.id, &repo).await;
    let newly_allowed = fixture.path().join("newly-allowed");
    std::fs::create_dir(&newly_allowed).expect("newly allowed fixture");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let make_state = || {
        AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(ScriptedRecordingProvider {
                outputs: std::sync::Mutex::new(Default::default()),
                requests: std::sync::Mutex::new(vec![]),
            }),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
        .with_worktree_root(fixture.path().join("worker-worktrees"))
    };
    let state = make_state();
    let run_loop = |state: AppState, replay: Vec<WorkflowJournalRecord>| {
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_host_commands(
            state,
            run.id,
            topic.id,
            vec![],
            vec![],
            None,
            None,
            WorkflowBudget::default(),
            replay,
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            CancellationToken::new(),
            PARK_TIMER_THRESHOLD_MS,
            rx,
        ));
        (tx, task)
    };
    let (tx, task) = run_loop(state.clone(), vec![]);
    let (allowed_reply, allowed_receive) = oneshot::channel();
    tx.send(HostCommand::ResolveWorkerDirectory {
        cwd: repo.to_string_lossy().into_owned(),
        reply: allowed_reply,
    })
    .unwrap();
    let allowed = allowed_receive.await.unwrap().unwrap().unwrap();
    assert_eq!(allowed.path, repo.to_string_lossy());
    assert!(allowed.can_write);
    let (denied_reply, denied_receive) = oneshot::channel();
    tx.send(HostCommand::ResolveWorkerDirectory {
        cwd: newly_allowed.to_string_lossy().into_owned(),
        reply: denied_reply,
    })
    .unwrap();
    assert!(
        denied_receive
            .await
            .unwrap()
            .unwrap_err()
            .contains("outside the dispatching context's permitted workspace roots")
    );
    let (placement_reply, placement_receive) = oneshot::channel();
    tx.send(HostCommand::ProvisionWorker {
        cwd: Some(repo.to_string_lossy().into_owned()),
        isolation: swage_protocol::client::workflow::WorkerIsolation::Worktree,
        reply: placement_reply,
    })
    .unwrap();
    let first_placement = placement_receive.await.unwrap().unwrap().unwrap();
    let first_path = first_placement.path;
    let (list_reply, list_receive) = oneshot::channel();
    tx.send(HostCommand::ListWorkerPlacements { reply: list_reply })
        .unwrap();
    assert_eq!(
        list_receive.await.unwrap().unwrap()["paths"],
        serde_json::json!([first_path])
    );
    drop(tx);
    task.await.unwrap();

    let replay = storage.list_workflow_journal(run.id).await.unwrap();
    assert_eq!(
        replay
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "worker_placement",
            "worker_placement",
            "worker_placement",
            "worker_placement"
        ]
    );
    assert_eq!(replay[2].request["cwd"], repo.to_string_lossy().as_ref());
    assert_eq!(replay[2].result.as_ref().unwrap()["path"], first_path);

    assert!(
        storage
            .delete_workspace_attachment(repo_grant)
            .await
            .expect("revoke repository grant")
    );
    grant_workspace(storage.as_ref(), topic.id, &newly_allowed).await;
    let replay_state = make_state();
    let (replay_tx, replay_task) = run_loop(replay_state.clone(), replay.clone());
    let (reply, receive) = oneshot::channel();
    replay_tx
        .send(HostCommand::ResolveWorkerDirectory {
            cwd: repo.to_string_lossy().into_owned(),
            reply,
        })
        .unwrap();
    assert!(
        receive
            .await
            .unwrap()
            .unwrap_err()
            .contains("outside the dispatching context's permitted workspace roots"),
        "a replayed directory resolution must re-authorise its recorded path"
    );
    let (reply, receive) = oneshot::channel();
    replay_tx
        .send(HostCommand::ResolveWorkerDirectory {
            cwd: newly_allowed.to_string_lossy().into_owned(),
            reply,
        })
        .unwrap();
    assert!(
        receive
            .await
            .unwrap()
            .unwrap_err()
            .contains("outside the dispatching context's permitted workspace roots"),
        "replay must return the journalled rejection after live policy changes"
    );
    let (reply, receive) = oneshot::channel();
    replay_tx
        .send(HostCommand::ProvisionWorker {
            cwd: Some(repo.to_string_lossy().into_owned()),
            isolation: swage_protocol::client::workflow::WorkerIsolation::Worktree,
            reply,
        })
        .unwrap();
    assert!(
        receive
            .await
            .unwrap()
            .unwrap_err()
            .contains("outside the dispatching context's permitted workspace roots"),
        "a replayed worktree must not be restored after its repository grant is revoked"
    );
    let (reply, receive) = oneshot::channel();
    replay_tx
        .send(HostCommand::ListWorkerPlacements { reply })
        .unwrap();
    assert_eq!(
        receive.await.unwrap().unwrap()["paths"],
        serde_json::json!([first_path])
    );
    drop(replay_tx);
    replay_task.await.unwrap();
    assert_eq!(
        storage.list_workflow_journal(run.id).await.unwrap().len(),
        4,
        "replay must not add a worktree journal entry"
    );
    assert_eq!(replay_state.worktree_host.list_owner(run.id).await.len(), 0);
    replay_state.worktree_host.cleanup_owner(run.id).await;

    let (changed_tx, changed_task) = run_loop(make_state(), replay);
    let (reply, receive) = oneshot::channel();
    changed_tx
        .send(HostCommand::ResolveWorkerDirectory {
            cwd: newly_allowed.to_string_lossy().into_owned(),
            reply,
        })
        .unwrap();
    assert_eq!(
        receive.await.unwrap().unwrap_err(),
        "NondeterministicReplayError"
    );
    drop(changed_tx);
    changed_task.await.unwrap();
    state.worktree_host.shutdown().await;
    assert!(
        !fixture
            .path()
            .join("worker-worktrees")
            .join(run.id.to_string())
            .exists()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_read_only_attachment_placement_stays_read_only() {
    let fixture = tempfile::tempdir().expect("scratch fixture");
    let attachment = fixture.path().join("read-only");
    std::fs::create_dir(&attachment).unwrap();
    std::fs::write(attachment.join("readable.txt"), "readable proof").unwrap();
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            topic_id: topic.id,
            label: "read-only placement".into(),
            path: attachment.to_string_lossy().into_owned(),
            permissions: vec!["read".into()],
            is_trusted: true,
        })
        .await
        .unwrap();
    let outputs = std::collections::VecDeque::from([
        vec![
            swage_core::StreamChunk::ToolUse {
                call_id: "read-placement".into(),
                name: "file_read".into(),
                arguments: serde_json::json!({"path": "readable.txt"}),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            swage_core::StreamChunk::ToolUse {
                call_id: "write-placement".into(),
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "blocked.txt",
                    "content": "must not be written",
                }),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ],
        scripted_text_output("read succeeded and write was refused"),
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(ScriptedRecordingProvider {
            outputs: std::sync::Mutex::new(outputs),
            requests: std::sync::Mutex::new(vec![]),
        }),
        crate::turn::tests::sandboxed_turn_config(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(crate::turn::tests::SandboxedFileTool::new("file_read"))
                as Arc<dyn crate::tool::Tool>,
            Arc::new(crate::turn::tests::SandboxedFileTool::new("file_write"))
                as Arc<dyn crate::tool::Tool>,
        ]),
        None,
    )
    .with_worktree_root(fixture.path().join("worker-worktrees"))
    .with_test_sandbox();
    let cwd = serde_json::to_string(&attachment.to_string_lossy()).unwrap();
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: format!(
                r#"return await agent("exercise read-only placement", {{ cwd: {cwd}, maxAttempts: 1 }});"#
            ),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start read-only placement workflow");
    wait_for_terminal(&state, started.id).await;
    let run = storage.get_workflow_run(started.id).await.unwrap().unwrap();
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let turn_id = storage
        .list_workflow_journal(started.id)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.kind == "agent_call")
        .and_then(|entry| entry.result)
        .and_then(|result| result["turn_id"].as_str().map(str::to_owned))
        .and_then(|id| Uuid::parse_str(&id).ok())
        .expect("workflow agent call turn id");
    let calls = storage.list_tool_calls_by_turn(turn_id).await.unwrap();
    assert_eq!(
        calls.len(),
        2,
        "read and write calls should both be audited"
    );
    assert_eq!(calls[0].tool, "file_read");
    assert_eq!(calls[0].status.as_deref(), Some("success"));
    assert!(
        calls[0]
            .result
            .as_deref()
            .is_some_and(|result| result.contains("readable proof")),
        "read should succeed inside the placed attachment: {calls:?}"
    );
    assert_eq!(calls[1].tool, "file_write");
    assert_eq!(calls[1].status.as_deref(), Some("error"));
    assert!(
        !attachment.join("blocked.txt").exists(),
        "read-only placement must reject the sandboxed write"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_workflow_workers_write_only_inside_their_own_worktrees() {
    Box::pin(assert_live_workflow_workers_write_only_inside_their_own_worktrees()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_worker_worktree_journey_completes_twenty_times_under_parallel_load() {
    // Four independent journeys overlap while the rest of the crate runs.
    // Each journey retains the original placement, journal and cleanup assertions.
    use futures::{StreamExt, stream};
    stream::iter(1..=20)
        .map(|attempt| async move {
            Box::pin(assert_live_workflow_workers_write_only_inside_their_own_worktrees()).await;
            eprintln!("live worker worktree journey completed: {attempt}/20");
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
}

async fn assert_live_workflow_workers_write_only_inside_their_own_worktrees() {
    let fixture = tempfile::tempdir().expect("scratch fixture");
    let repo = fixture.path().join("repo");
    seed_git_repo(&repo);

    let base_commit = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("read repository base")
            .stdout,
    )
    .expect("base commit is UTF-8")
    .trim()
    .to_string();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    grant_workspace(storage.as_ref(), topic.id, &repo).await;
    let placement_tool = Arc::new(PlacementWriteTool::default());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(PlacementWritingProvider::default()),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::clone(&placement_tool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    )
    .with_worktree_root(fixture.path().join("worker-worktrees"));
    let cwd = serde_json::to_string(&repo.to_string_lossy()).unwrap();
    let source = format!(
        r#"
        const first = agentStart("write first-marker", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
        const second = agentStart("write second-marker", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
        const unchanged = agentStart("leave worktree unchanged", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
        await Promise.all([first.result, second.result, unchanged.result]);
        return {{ first: first.workingDirectory, second: second.workingDirectory, unchanged: unchanged.workingDirectory }};
    "#
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source,
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_concurrent_agent_calls: Some(3),
                ..WorkflowBudget::default()
            }),
        }),
    )
    .await
    .expect("start live placement run");
    wait_for_terminal(&state, started.id).await;
    let run = storage.get_workflow_run(started.id).await.unwrap().unwrap();
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let journal = storage.list_workflow_journal(started.id).await.unwrap();
    let detail = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("project completed worker calls")
        .0;
    let projected_first = detail
        .journal
        .iter()
        .find(|entry| {
            entry.kind == WorkflowJournalEntryKind::AgentCall
                && entry.request["prompt"] == "write first-marker"
        })
        .and_then(|entry| entry.result.as_ref())
        .expect("the completed worker result survives its typed projection");
    assert!(matches!(
        projected_first,
        WorkflowJournalResult::AgentCall {
            output,
            worktree: Some(_),
            ..
        } if output == "written"
    ));
    let paths = journal
        .iter()
        .filter(|entry| entry.kind == "worker_placement")
        .filter_map(|entry| entry.result.as_ref()?.get("path")?.as_str())
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 3);
    let agent_call_results = journal
        .iter()
        .filter(|entry| entry.kind == "agent_call")
        .map(|entry| {
            assert_eq!(
                entry.request["cwd"],
                repo.to_string_lossy().as_ref(),
                "the journal retains the caller's requested directory"
            );
            entry
                .result
                .as_ref()
                .and_then(|result| result["working_directory"].as_str())
                .map(std::path::PathBuf::from)
                .expect("completed worker call records its host-resolved directory")
        })
        .collect::<Vec<_>>();
    assert_eq!(agent_call_results.len(), 3);
    assert!(
        agent_call_results.iter().all(|path| paths.contains(path)),
        "each call result must report one of this run's provisioned paths"
    );
    assert_ne!(
        agent_call_results[0], agent_call_results[1],
        "parallel isolated siblings expose independently verifiable directories"
    );
    let writes = placement_tool.writes.lock().expect("placement writes");
    assert_eq!(writes.len(), 2);
    assert!(
        writes
            .iter()
            .any(|(file, path)| file == "first-marker" && paths.contains(path))
    );
    assert!(
        writes
            .iter()
            .any(|(file, path)| file == "second-marker" && paths.contains(path))
    );
    assert_ne!(
        writes[0].1, writes[1].1,
        "workers must use distinct tool roots"
    );
    drop(writes);
    assert!(!repo.join("first-marker").exists() && !repo.join("second-marker").exists());
    let path_for = |prompt: &str| {
        journal
            .iter()
            .find(|entry| entry.kind == "agent_call" && entry.request["prompt"] == prompt)
            .and_then(|entry| entry.result.as_ref())
            .and_then(|result| result["working_directory"].as_str())
            .map(std::path::PathBuf::from)
            .expect("worker call records its host-resolved directory")
    };
    let completion_for = |prompt: &str| {
        journal
            .iter()
            .find(|entry| entry.kind == "agent_call" && entry.request["prompt"] == prompt)
            .and_then(|entry| entry.result.as_ref())
            .and_then(|result| result.get("worktree"))
            .cloned()
            .map(serde_json::from_value::<swage_workflow::WorktreeCompletion>)
            .transpose()
            .expect("worktree completion record is well formed")
            .expect("isolated worker call persists its worktree completion")
    };
    let first_path = path_for("write first-marker");
    let second_path = path_for("write second-marker");
    let unchanged_path = path_for("leave worktree unchanged");
    for prompt in ["write first-marker", "write second-marker"] {
        let completion = completion_for(prompt);
        assert_eq!(completion.base_commit, base_commit);
        assert_eq!(completion.tip_commit, base_commit);
        assert!(completion.changed);
        assert!(!completion.removed);
    }
    let unchanged_completion = completion_for("leave worktree unchanged");
    assert_eq!(unchanged_completion.base_commit, base_commit);
    assert_eq!(unchanged_completion.tip_commit, base_commit);
    assert!(!unchanged_completion.changed);
    assert!(unchanged_completion.removed);
    assert!(
        first_path.join("first-marker").exists(),
        "the first marker remains inside its changed worktree"
    );
    assert!(
        second_path.join("second-marker").exists(),
        "the second marker remains inside its changed worktree"
    );
    let completed_run_root = fixture
        .path()
        .join("worker-worktrees")
        .join(started.id.to_string());
    for _ in 0..20 {
        if !unchanged_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !unchanged_path.exists(),
        "the unchanged worktree must be removed after its async cleanup: {unchanged_path:?}"
    );
    assert!(
        first_path.exists() && second_path.exists(),
        "changed worktrees remain available as completion evidence: {first_path:?}, {second_path:?}"
    );
    assert!(
        completed_run_root.exists(),
        "retained worktrees keep their per-run directory"
    );

    let failed_source = format!(
        r#"
        await agent("write retained marker", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
        throw new Error("retain failed run for inspection");
    "#
    );
    let Json(failed) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: failed_source,
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start failed placement run");
    wait_for_terminal(&state, failed.id).await;
    let failed_record = storage.get_workflow_run(failed.id).await.unwrap().unwrap();
    assert_eq!(failed_record.state, "failed");
    let retained_path = storage
        .list_workflow_journal(failed.id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|entry| {
            (entry.kind == "worker_placement")
                .then(|| entry.result?["path"].as_str().map(std::path::PathBuf::from))?
        })
        .expect("failed run should retain its worktree path");
    assert!(
        retained_path.join("retained-marker").exists(),
        "the failed run keeps its changed worker worktree"
    );
    let failed_account = terminal_account_of(&storage, failed.id).await;
    assert_eq!(
        failed_account.workers.done[0].placement.as_deref(),
        Some(retained_path.to_string_lossy().as_ref())
    );
    assert!(
        failed_account.workers.done[0]
            .worktree
            .as_ref()
            .is_some_and(|completion| completion.changed && !completion.removed)
    );
    assert_eq!(
        failed_account
            .partial_results
            .as_ref()
            .expect("a run that never returned offers its completed work")
            .results[0]
            .placement
            .as_deref(),
        Some(retained_path.to_string_lossy().as_ref())
    );

    let cancelled_source = format!(
        r#"
        await agent("prepare cancelled run", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
        return await wait({{ timerMs: 120000 }});
    "#
    );
    let Json(cancelled) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: cancelled_source,
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start cancellable placement run");
    wait_for_run_state(&storage, cancelled.id, "parked").await;
    wait_for_workflow_thread_exit(cancelled.id).await;
    let cancelled_path = storage
        .list_workflow_journal(cancelled.id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|entry| {
            (entry.kind == "worker_placement")
                .then(|| entry.result?["path"].as_str().map(std::path::PathBuf::from))?
        })
        .expect("cancelled run should have a worktree path");
    let Json(cancelled_record) = cancel_run(State(state.clone()), Path(cancelled.id))
        .await
        .expect("cancel parked placement run");
    assert_eq!(cancelled_record.state, WorkflowRunState::Cancelled);
    assert!(
        !cancelled_path.exists(),
        "a terminal cancelled run reclaims its completed worker worktree"
    );
    let cancelled_account = terminal_account_of(&storage, cancelled.id).await;
    assert!(cancelled_account.workers.done[0].placement.is_none());
    assert!(
        cancelled_account.workers.done[0]
            .worktree
            .as_ref()
            .is_some_and(|completion| !completion.changed && completion.removed)
    );

    // A worktree no account names is still the gateway's to reclaim; this
    // one stands in for the ordinary case shutdown exists for.
    let unclaimed_owner = Uuid::new_v4();
    let unclaimed_path = state
        .worktree_host
        .provision(unclaimed_owner, 0, &repo)
        .await
        .expect("provision a worktree no account names");
    assert!(unclaimed_path.path.exists());

    state.worktree_host.shutdown().await;
    assert!(
        !unclaimed_path.path.exists(),
        "shutdown must still reclaim a worktree no terminal account names"
    );
    assert!(
        retained_path.join("retained-marker").exists() && !cancelled_path.exists(),
        "shutdown keeps the changed placement named by the failed run's account"
    );

    // Nor may the next gateway's reclaim sweep take them. It starts with
    // an empty map, so the claim has to be readable from the disk.
    let leftover_owner = Uuid::new_v4();
    let leftover_path = state
        .worktree_host
        .provision(leftover_owner, 0, &repo)
        .await
        .expect("provision a crash-stranded worktree");
    let restarted = crate::worktree::WorktreeHost::new(crate::worktree::WorktreeConfig {
        root: fixture.path().join("worker-worktrees"),
    });
    restarted
        .recover(&crate::worktree::WorktreeRecoveryClaims::default())
        .await
        .expect("startup worktree recovery");
    assert!(
        !leftover_path.path.exists(),
        "startup recovery must still reclaim a crash-stranded worktree no account names"
    );
    assert!(
        retained_path.join("retained-marker").exists() && !cancelled_path.exists(),
        "startup recovery honours the failed run account's durable placement claim"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_cwd_placement_replays_in_dispatch_order() {
    let fixture = tempfile::tempdir().expect("scratch fixture");
    let workspace = fixture.path().join("workspace");
    let first_cwd = workspace.join("first");
    let second_cwd = workspace.join("second");
    std::fs::create_dir_all(&first_cwd).unwrap();
    std::fs::create_dir_all(&second_cwd).unwrap();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    grant_workspace(storage.as_ref(), topic.id, &workspace).await;
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output("first result"),
            scripted_text_output("second result"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    )
    .with_worktree_root(fixture.path().join("worker-worktrees"));
    let first_json = serde_json::to_string(&first_cwd.to_string_lossy()).unwrap();
    let second_json = serde_json::to_string(&second_cwd.to_string_lossy()).unwrap();
    let source = format!(
        r#"
        return await Promise.all([
            agent("first placed call", {{ cwd: {first_json}, maxAttempts: 1 }}),
            agent("second placed call", {{ cwd: {second_json}, maxAttempts: 1 }})
        ]);
    "#
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.clone(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_concurrent_agent_calls: Some(2),
                ..WorkflowBudget::default()
            }),
        }),
    )
    .await
    .expect("start parallel cwd run");
    wait_for_terminal(&state, started.id).await;
    let first_record = storage.get_workflow_run(started.id).await.unwrap().unwrap();
    assert_eq!(
        first_record.state, "completed",
        "{:?}",
        first_record.error_message
    );
    let first_journal = storage.list_workflow_journal(started.id).await.unwrap();
    assert_eq!(
        first_journal
            .iter()
            .filter(|entry| entry.kind == "agent_call")
            .map(|entry| entry.request["prompt"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["first placed call", "second placed call"]
    );

    storage
        .update_workflow_run_state(started.id, "running", None, None, None)
        .await
        .unwrap();
    let replay_record = storage.get_workflow_run(started.id).await.unwrap().unwrap();
    launch_existing_run(state.clone(), replay_record, source, None)
        .await
        .expect("relaunch placed parallel run");
    wait_for_terminal(&state, started.id).await;
    let replayed = storage.get_workflow_run(started.id).await.unwrap().unwrap();
    assert_eq!(replayed.state, "completed", "{:?}", replayed.error_message);
    let replay_journal = storage.list_workflow_journal(started.id).await.unwrap();
    assert_eq!(
        replay_journal
            .iter()
            .map(|entry| (entry.id, entry.seq, entry.kind.clone()))
            .collect::<Vec<_>>(),
        first_journal
            .iter()
            .map(|entry| (entry.id, entry.seq, entry.kind.clone()))
            .collect::<Vec<_>>(),
        "parallel replay must consume the original entries without reordering or appending"
    );
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        2,
        "completed replay must not invoke either worker again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn timed_out_agent_call_is_cancelled_journalled_and_replayed_without_reinvocation() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"return await agent("slow work", {
        effort: "high", timeoutMs: 10000, maxAttempts: 1
    });"#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("launch timed workflow run");
    wait_for_terminal(&state, run.id).await;

    let failed_run = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("failed run");
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal read-back");
    assert_eq!(failed_run.state, "failed");
    let agent_call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("timed-out agent call");
    assert_eq!(agent_call.status, "failed");
    assert_eq!(agent_call.request["effort"], "high");
    assert_eq!(agent_call.request["timeout_ms"], 10000);
    assert!(
        agent_call
            .error_message
            .as_deref()
            .is_some_and(|error| error.starts_with("AgentCallTimeoutError:"))
    );
    {
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].effort.as_ref().map(|effort| effort.requested),
            Some(swage_core::Effort::High)
        );
    }
    wait_for_worker_turn_interrupted(&storage).await;
    assert!(
        state.active_turns.lock().await.is_empty(),
        "timed-out worker must release its active-turn registration"
    );

    storage
        .update_workflow_run_state(run.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let replay_record = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("replay run");
    storage
        .update_workflow_run_state(run.id, "running", None, None, None)
        .await
        .expect("mark replay running");
    launch_existing_run(state.clone(), replay_record, source.into(), None)
        .await
        .expect("replay timed workflow run");
    wait_for_terminal(&state, run.id).await;
    let replayed = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("replayed run");
    assert_eq!(replayed.state, "failed");
    assert!(
        replayed
            .error_message
            .as_deref()
            .is_some_and(|error| error.contains("AgentCallTimeoutError:"))
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn parent_cancellation_wins_over_a_configured_agent_call_timeout() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"return await agent("cancelled work", {
        timeoutMs: 60000, maxAttempts: 1
    });"#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(WorkflowBudget {
                max_agent_calls: Some(1),
                per_route: Default::default(),
                max_total_tokens: Some(10),
                reserved_closing_tokens: Some(4),
                max_concurrent_agent_calls: Some(1),
            })
            .expect("budget"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("launch cancellable workflow run");
    wait_for_provider_requests(&provider, 1).await;
    let Json(cancel_requested) = cancel_run(State(state.clone()), Path(run.id))
        .await
        .expect("cancel running workflow");
    assert_eq!(cancel_requested.id, run.id);
    wait_for_terminal(&state, run.id).await;

    let cancelled_run = storage
        .get_workflow_run(run.id)
        .await
        .expect("run query")
        .expect("cancelled run");
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal read-back");
    assert_eq!(cancelled_run.state, "cancelled");
    let agent_call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("cancelled agent call");
    assert_eq!(agent_call.status, "failed");
    assert_eq!(
        agent_call.error_message.as_deref(),
        Some("WorkflowCancelledError: run cancelled")
    );
    let terminal = journal
        .iter()
        .find(|entry| entry.kind == "terminal_budget")
        .expect("terminal budget entry");
    assert_eq!(terminal.status, "completed");
    assert_eq!(terminal.result.as_ref().unwrap()["spent"], 0);
    assert_eq!(terminal.result.as_ref().unwrap()["remaining"], 10);
    assert_eq!(terminal.result.as_ref().unwrap()["closing_remaining"], 4);
    assert_eq!(
        journal
            .iter()
            .filter(|entry| entry.kind == "terminal_budget")
            .count(),
        1
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
    wait_for_worker_turn_interrupted(&storage).await;
    assert!(
        state.active_turns.lock().await.is_empty(),
        "cancelled worker must release its active-turn registration"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_parked_run_settles_journal_and_records_terminal_budget() {
    workflow_journey(async {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let source = format!(
            r#"
            await agent("working segment", {{agent: "default"}});
            await budgetSnapshot();
            await agent("closing segment", {{agent: "default"}});
            const held = await session({{agent: "default"}});
            return await wait({{ timerMs: {} }});
        "#,
            PARK_TIMER_THRESHOLD_MS + 1
        );
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                source: Some(source.clone()),
                source_hash: source_hash(&source),
                budget: serde_json::to_value(WorkflowBudget {
                    max_agent_calls: Some(3),
                    per_route: Default::default(),
                    max_total_tokens: Some(10),
                    reserved_closing_tokens: Some(4),
                    max_concurrent_agent_calls: Some(1),
                })
                .expect("budget"),
                ..swage_storage::fixtures::new_workflow_run(topic.id)
            })
            .await
            .expect("run");
        let provider = Arc::new(ScriptedRecordingProvider {
            outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
                scripted_text_output_with_usage("working segment", 2, 3),
                scripted_text_output_with_usage("closing segment", 1, 1),
            ])),
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let root = tempfile::tempdir().expect("isolated workflow root");
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider,
            workflow_worker_test_config(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        launch_existing_run(state.clone(), run.clone(), source, None)
            .await
            .expect("launch waiting run");
        wait_for_run_state(&storage, run.id, "parked").await;
        workflow_thread_exit(run.id).await;
        active_workflow_run_release(&state, run.id).await;

        let api = WorkflowTestApi::start(state.clone()).await;
        let cancel_requested = api.cancel(run.id).await;
        assert_eq!(cancel_requested.id, run.id);
        wait_for_run_state(&storage, run.id, "cancelled").await;
        let journal = storage
            .list_workflow_journal(run.id)
            .await
            .expect("cancelled journal");
        let timer = journal
            .iter()
            .find(|entry| entry.kind == "timer")
            .expect("timer entry");
        assert_eq!(timer.status, "failed");
        assert_eq!(
            timer.error_message.as_deref(),
            Some("WorkflowCancelledError: run cancelled")
        );
        let terminal = journal
            .iter()
            .find(|entry| entry.kind == "terminal_budget")
            .expect("terminal budget entry");
        assert_eq!(terminal.status, "completed");
        assert_eq!(terminal.result.as_ref().unwrap()["spent"], 7);
        assert_eq!(terminal.result.as_ref().unwrap()["remaining"], 3);
        assert_eq!(terminal.result.as_ref().unwrap()["closing_remaining"], 2);
        let held_close = journal
            .iter()
            .find(|entry| {
                entry.kind == "worker"
                    && entry.request["action"] == "close"
                    && entry.request["closed_by"] == "run_end"
            })
            .expect("cancelling a parked run closes its held worker");
        assert_eq!(held_close.status, "completed");
        assert_eq!(
            journal
                .iter()
                .filter(|entry| entry.kind == "terminal_budget")
                .count(),
            1
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parked_cancellation_losing_to_a_wake_keeps_the_resumed_runs_account() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"
        await awaitEvent({ kinds: ["operator.continue"] });
        return await agent("finish after wake", { agent: "default", maxAttempts: 1 });
    "#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let provider_started = Arc::new(Notify::new());
    let provider_release = Arc::new(Notify::new());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(ReleasableProvider {
            started: Arc::clone(&provider_started),
            release: Arc::clone(&provider_release),
        }),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("launch awaiting run");
    wait_for_run_state(&storage, run.id, "parked").await;
    wait_for_workflow_thread_exit(run.id).await;
    wait_for_active_workflow_run_release(&state, run.id).await;

    let cancellation_pause = workflow_test_hook::install(run.id);
    let cancellation_state = state.clone();
    let cancellation =
        tokio::spawn(async move { cancel_run(State(cancellation_state), Path(run.id)).await });
    cancellation_pause
        .entered
        .acquire()
        .await
        .expect("cancellation pause remains open")
        .forget();

    let Json(posted) = Box::pin(post_operator_run_event(
        State(state.clone()),
        Path(run.id),
        Json(PostWorkflowRunEventRequest {
            kind: "operator.continue".into(),
            payload: serde_json::json!({"approved": true}),
        }),
    ))
    .await
    .expect("wake the parked run");
    assert!(posted.wake_provisioned);
    provider_started.notified().await;

    cancellation_pause.release.add_permits(1);
    let cancellation = cancellation.await.expect("cancellation task");
    provider_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let resumed = storage
                .get_workflow_run(run.id)
                .await
                .expect("resumed run")
                .expect("resumed run remains present");
            if resumed.state == "completed" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resumed run completes");

    let account = terminal_account_of(&storage, run.id).await;
    assert_eq!(account.ending, WorkflowRunEnding::Returned);
    assert!(
        cancellation.is_err_and(|error| error.code() == ErrorCode::Conflict),
        "the stale parked cancellation must lose to the resumed generation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn parked_cancellation_losing_after_return_withdraws_its_retention_claim() {
    let fixture = tempfile::tempdir().expect("scratch fixture");
    let repo = fixture.path().join("repo");
    seed_git_repo(&repo);

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    grant_workspace(storage.as_ref(), topic.id, &repo).await;
    let placement_tool = Arc::new(PlacementWriteTool::default());
    let worktree_root = fixture.path().join("worker-worktrees");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(PlacementWritingProvider::default()),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![placement_tool as Arc<dyn crate::tool::Tool>]),
        None,
    )
    .with_worktree_root(worktree_root.clone());
    let cwd = serde_json::to_string(&repo.to_string_lossy()).expect("serialise cwd");
    let source = format!(
        r#"
            await agent("write retained marker", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
            await awaitEvent({{ kinds: ["operator.continue"] }});
            return {{ done: true }};
        "#
    );
    let Json(run) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source,
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start placed waiting run");
    wait_for_run_state(&storage, run.id, "parked").await;
    wait_for_workflow_thread_exit(run.id).await;
    wait_for_active_workflow_run_release(&state, run.id).await;
    let placement = storage
        .list_workflow_journal(run.id)
        .await
        .expect("parked run journal")
        .into_iter()
        .find_map(|entry| {
            (entry.kind == "worker_placement")
                .then(|| entry.result?["path"].as_str().map(std::path::PathBuf::from))?
        })
        .expect("parked run records its isolated worker placement");
    let retained_marker = worktree_root
        .join(run.id.to_string())
        .join(".retained-placements");
    assert!(placement.join("retained-marker").exists());

    let cancellation_start = workflow_test_hook::install(run.id);
    let returned_cleanup = workflow_cleanup_test_hook::install_at(
        run.id,
        workflow_cleanup_test_hook::Point::ReturnedCleanup,
    );
    let cancellation_retained = workflow_cleanup_test_hook::install_at(
        run.id,
        workflow_cleanup_test_hook::Point::ParkedCancellationRetained,
    );
    let cancellation_state = state.clone();
    let cancellation =
        tokio::spawn(async move { cancel_run(State(cancellation_state), Path(run.id)).await });
    cancellation_start
        .entered
        .acquire()
        .await
        .expect("cancellation start pause remains open")
        .forget();

    let Json(posted) = Box::pin(post_operator_run_event(
        State(state.clone()),
        Path(run.id),
        Json(PostWorkflowRunEventRequest {
            kind: "operator.continue".into(),
            payload: serde_json::json!({"approved": true}),
        }),
    ))
    .await
    .expect("wake the parked run");
    assert!(posted.wake_provisioned);
    returned_cleanup
        .entered
        .acquire()
        .await
        .expect("returned cleanup pause remains open")
        .forget();

    cancellation_start.release.add_permits(1);
    cancellation_retained
        .entered
        .acquire()
        .await
        .expect("retention pause remains open")
        .forget();
    assert!(
        retained_marker.exists(),
        "the stale cancellation has claimed the placement"
    );

    returned_cleanup.release.add_permits(1);
    returned_cleanup
        .finished
        .acquire()
        .await
        .expect("returned cleanup pause remains open")
        .forget();
    assert!(
        placement.exists(),
        "the stale claim makes the returned-path cleanup skip the placement"
    );

    cancellation_retained.release.add_permits(1);
    let cancellation = cancellation.await.expect("cancellation task");
    assert!(
        cancellation.is_err_and(|error| error.code() == ErrorCode::Conflict),
        "the stale parked cancellation must lose to the returned run"
    );
    assert!(
        !placement.exists(),
        "a returned run must not retain a worker worktree after a stale cancellation loses"
    );
    assert!(
        !retained_marker.exists(),
        "a returned run must not retain a durable placement claim"
    );
    assert_eq!(
        terminal_account_of(&storage, run.id).await.ending,
        WorkflowRunEnding::Returned
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_parked_cancellation_preserves_a_failed_winner_retention_claim() {
    let fixture = tempfile::tempdir().expect("scratch fixture");
    let repo = fixture.path().join("repo");
    seed_git_repo(&repo);

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    grant_workspace(storage.as_ref(), topic.id, &repo).await;
    let worktree_root = fixture.path().join("worker-worktrees");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(PlacementWritingProvider::default()),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(PlacementWriteTool::default()) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    )
    .with_worktree_root(worktree_root.clone());
    let cwd = serde_json::to_string(&repo.to_string_lossy()).expect("serialise cwd");
    let source = format!(
        r#"
            await agent("write retained marker", {{ cwd: {cwd}, isolation: "worktree", maxAttempts: 1 }});
            await awaitEvent({{ kinds: ["operator.continue"] }});
            throw new Error("failed after wake");
        "#
    );
    let Json(run) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source,
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start placed waiting run");
    wait_for_run_state(&storage, run.id, "parked").await;
    wait_for_workflow_thread_exit(run.id).await;
    wait_for_active_workflow_run_release(&state, run.id).await;
    let placement = storage
        .list_workflow_journal(run.id)
        .await
        .expect("parked run journal")
        .into_iter()
        .find_map(|entry| {
            (entry.kind == "worker_placement")
                .then(|| entry.result?["path"].as_str().map(std::path::PathBuf::from))?
        })
        .expect("parked run records its isolated worker placement");
    let retained_marker = worktree_root
        .join(run.id.to_string())
        .join(".retained-placements");
    assert!(placement.join("retained-marker").exists());

    let cancellation_start = workflow_test_hook::install(run.id);
    let cancellation_retained = workflow_cleanup_test_hook::install_at(
        run.id,
        workflow_cleanup_test_hook::Point::ParkedCancellationRetained,
    );
    let cancellation_state = state.clone();
    let cancellation =
        tokio::spawn(async move { cancel_run(State(cancellation_state), Path(run.id)).await });
    cancellation_start
        .entered
        .acquire()
        .await
        .expect("cancellation start pause remains open")
        .forget();

    let Json(posted) = Box::pin(post_operator_run_event(
        State(state.clone()),
        Path(run.id),
        Json(PostWorkflowRunEventRequest {
            kind: "operator.continue".into(),
            payload: serde_json::json!({"approved": true}),
        }),
    ))
    .await
    .expect("wake the parked run");
    assert!(posted.wake_provisioned);
    wait_for_run_state(&storage, run.id, "failed").await;
    assert!(retained_marker.exists());

    cancellation_start.release.add_permits(1);
    cancellation_retained
        .entered
        .acquire()
        .await
        .expect("retention pause remains open")
        .forget();
    cancellation_retained.release.add_permits(1);
    let cancellation = cancellation.await.expect("cancellation task");
    assert!(
        cancellation.is_err_and(|error| error.code() == ErrorCode::Conflict),
        "the stale parked cancellation must lose to the failed resumed generation"
    );

    let account = terminal_account_of(&storage, run.id).await;
    assert!(matches!(account.ending, WorkflowRunEnding::Threw { .. }));
    assert_eq!(account.workers.done.len(), 1);
    assert!(
        account.workers.done[0]
            .worktree
            .as_ref()
            .is_some_and(|worktree| worktree.changed),
        "the retained worker must carry its changed worktree account"
    );
    assert!(
        placement.exists(),
        "the winning account's worktree survives"
    );
    assert!(
        retained_marker.exists(),
        "the winning account's durable retention marker survives"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continuation_token_the_run_does_not_own_is_refused() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    // A real session in the same topic, but created outside this run —
    // exactly what a token must not be able to reach.
    let foreign_session = storage
        .create_session(swage_storage::NewSession {
            agent_id: "interactive".into(),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("foreign session");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::PendingProvider),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: None,
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: None,
        },
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: AgentCallRequest {
            agent: Some("worker".into()),
            session: Some(AgentSessionRef {
                session_id: foreign_session.id,
                thread_id: Uuid::new_v4(),
            }),
            ..swage_workflow::fixtures::agent_call_request("hijack".into())
        },
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("call");
    let error = receive.await.expect("reply").expect_err("refused");
    assert!(
        error.to_string().contains("was not created by this run"),
        "unexpected error: {error}"
    );
    // The refusal settles durably, so replay repeats it instead of
    // re-attempting the hijack.
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].status, "failed");
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continuation_cannot_change_its_task_definition() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let (fixture_run_id, fixture_entry_id) = journalled_agent_call(&storage, topic.id).await;
    let error = run_ephemeral_agent_call(
        &state,
        fixture_run_id,
        topic.id,
        &[],
        &[],
        fixture_entry_id,
        AgentCallRequest {
            task: Some("review".into()),
            session: Some(AgentSessionRef {
                session_id: Uuid::new_v4(),
                thread_id: Uuid::new_v4(),
            }),
            ..swage_workflow::fixtures::agent_call_request("continue".into())
        },
        None,
        CancellationToken::new(),
    )
    .await
    .expect_err("continued calls must not silently ignore a task selector");

    assert!(
        error
            .to_string()
            .contains("cannot be selected on a continued session")
    );
}

/// Serves scripted gate verdicts through `complete` and counts calls, so
/// tests can assert a replayed gate never re-consults the model.
struct ScriptedGateProvider {
    replies: std::sync::Mutex<std::collections::VecDeque<&'static str>>,
    calls: AtomicUsize,
}

struct RecordingGateProvider {
    model_id: &'static str,
    calls: AtomicUsize,
    requests: std::sync::Mutex<Vec<swage_core::CompletionRequest>>,
}

impl RecordingGateProvider {
    fn new(model_id: &'static str) -> Self {
        Self {
            model_id,
            calls: AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for RecordingGateProvider {
    fn model_id(&self) -> &str {
        self.model_id
    }

    async fn complete(
        &self,
        request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().expect("requests").push(request);
        Ok(swage_core::CompletionResponse {
            content: r#"{"wake":true,"reasoning":"event is decisive"}"#.into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: swage_core::FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("gate evaluations use complete")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("gate evaluations use complete")
    }
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for ScriptedGateProvider {
    fn model_id(&self) -> &str {
        "gate-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let content = self
            .replies
            .lock()
            .expect("replies")
            .pop_front()
            .expect("scripted gate reply");
        Ok(swage_core::CompletionResponse {
            content: content.into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: swage_core::FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("gate evaluations use complete")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("gate evaluations use complete")
    }
}

async fn event_gate_fixture<P: swage_provider::LlmProvider + 'static>(
    provider: Arc<P>,
) -> (
    Arc<SqliteStorage>,
    AppState,
    swage_storage::WorkflowRunRecord,
) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        provider,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (storage, state, run)
}

async fn configured_event_gate_fixture() -> (
    Arc<SqliteStorage>,
    AppState,
    swage_storage::WorkflowRunRecord,
    Arc<RecordingGateProvider>,
    Arc<RecordingGateProvider>,
) {
    configured_event_gate_fixture_with_models("default-provider-model", "provider-facing-model")
        .await
}

async fn configured_event_gate_fixture_with_models(
    default_provider_model: &'static str,
    selected_provider_model: &'static str,
) -> (
    Arc<SqliteStorage>,
    AppState,
    swage_storage::WorkflowRunRecord,
    Arc<RecordingGateProvider>,
    Arc<RecordingGateProvider>,
) {
    let default = Arc::new(RecordingGateProvider::new(default_provider_model));
    let selected = Arc::new(RecordingGateProvider::new(selected_provider_model));
    let provider = Arc::new(swage_provider::failover::FailoverProvider::with_defaults(
        vec![
            Arc::clone(&default) as Arc<dyn swage_provider::LlmProvider>,
            Arc::clone(&selected) as Arc<dyn swage_provider::LlmProvider>,
        ],
    ));
    let config = swage_config::ResolvedConfig {
        models: swage_config::ModelCatalogConfig {
            default: "default".into(),
            entries: [
                (
                    "default".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "default-provider".into(),
                        model_id: default_provider_model.into(),
                        ..Default::default()
                    },
                ),
                (
                    "gate-alias".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "selected-provider".into(),
                        model_id: selected_provider_model.into(),
                        aliases: vec!["cheap-gate".into()],
                        ..Default::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
        },
        ..Default::default()
    };
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        provider,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let entry = state
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "event".into(),
            args_hash: "test-event".into(),
            request: serde_json::json!({"kind": "completion"}),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("event intent");
    complete_event_post(
        &state,
        run.id,
        entry.id,
        RunEventPost {
            kind: "worker.completion".into(),
            payload: serde_json::json!({"worker": "w1"}),
        },
    )
    .await
    .expect("pending event");
    (storage, state, run, default, selected)
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_gate_model_sends_the_catalogue_entries_model_id() {
    let (_storage, state, run, _default, selected) = configured_event_gate_fixture().await;

    evaluate_run_gate(
        &state,
        run.id,
        &GateRequest {
            criteria: "wake on completion".into(),
            model: Some("cheap-gate".into()),
        },
        CancellationToken::new(),
    )
    .await
    .expect("gate verdict");

    let requests = selected.requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model.as_deref(), Some("provider-facing-model"));
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_gate_model_reaches_its_non_default_provider() {
    let (_storage, state, run, default, selected) = configured_event_gate_fixture().await;

    evaluate_run_gate(
        &state,
        run.id,
        &GateRequest {
            criteria: "wake on completion".into(),
            model: Some("gate-alias".into()),
        },
        CancellationToken::new(),
    )
    .await
    .expect("gate verdict");

    assert_eq!(selected.calls.load(Ordering::SeqCst), 1);
    assert_eq!(default.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_provider_resolution_is_model_id_only_when_provider_models_collide() {
    let (_storage, state, run, default, selected) =
        configured_event_gate_fixture_with_models("shared-model", "shared-model").await;

    evaluate_run_gate(
        &state,
        run.id,
        &GateRequest {
            criteria: "wake on completion".into(),
            model: Some("gate-alias".into()),
        },
        CancellationToken::new(),
    )
    .await
    .expect("gate verdict");

    // Gate evaluation passes only the catalogue entry's provider-facing
    // model ID to the shared provider resolver. Provider IDs are not part
    // of this contract, so a collision retains configured provider order.
    assert_eq!(default.calls.load(Ordering::SeqCst), 1);
    assert_eq!(selected.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_without_a_model_uses_the_default_model_and_provider() {
    let (_storage, state, run, default, selected) = configured_event_gate_fixture().await;

    evaluate_run_gate(
        &state,
        run.id,
        &GateRequest {
            criteria: "wake on completion".into(),
            model: None,
        },
        CancellationToken::new(),
    )
    .await
    .expect("gate verdict");

    assert_eq!(default.calls.load(Ordering::SeqCst), 1);
    assert_eq!(selected.calls.load(Ordering::SeqCst), 0);
    let requests = default.requests.lock().expect("requests");
    assert_eq!(requests[0].model.as_deref(), Some("default-provider-model"));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_gate_model_retains_the_catalogue_error() {
    let (_storage, state, run, default, selected) = configured_event_gate_fixture().await;

    let error = evaluate_run_gate(
        &state,
        run.id,
        &GateRequest {
            criteria: "wake on completion".into(),
            model: Some("missing".into()),
        },
        CancellationToken::new(),
    )
    .await
    .expect_err("unknown model");

    assert_eq!(
        error,
        "RunEventError: gate model 'missing' is not in the configured model catalogue"
    );
    assert_eq!(default.calls.load(Ordering::SeqCst), 0);
    assert_eq!(selected.calls.load(Ordering::SeqCst), 0);
}

fn workflow_thread_exists(run_id: Uuid) -> bool {
    let expected: String = format!("workflow-{run_id}").chars().take(15).collect();
    std::fs::read_dir("/proc/self/task")
        .expect("read process tasks")
        .filter_map(Result::ok)
        .any(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.trim() == expected)
        })
}

async fn wait_for_run_state(
    storage: &SqliteStorage,
    run_id: Uuid,
    expected: &str,
) -> WorkflowRunRecord {
    wait_for_run_state_in(storage, run_id, &[expected]).await
}

async fn wait_for_run_state_in(
    storage: &SqliteStorage,
    run_id: Uuid,
    expected: &[&str],
) -> WorkflowRunRecord {
    loop {
        let run = storage
            .get_workflow_run(run_id)
            .await
            .expect("get workflow run")
            .expect("workflow run");
        if expected.contains(&run.state.as_str()) {
            return run;
        }
        if matches!(run.state.as_str(), "completed" | "failed" | "cancelled") {
            panic!(
                "run {run_id} reached {} instead of one of {expected:?}: {:?}",
                run.state, run.error_message
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// A gateway turn's tool context for the answering agent: attribution is
/// the session's own, which is the whole point of answering through the
/// tool rather than through a parameter.
async fn hold_answering_context(
    storage: &Arc<SqliteStorage>,
    state: &AppState,
    topic_id: Uuid,
    agent_id: &str,
) -> (
    swage_storage::Session,
    Uuid,
    swage_tool::context::MockToolContext,
    tokio::sync::mpsc::UnboundedSender<crate::websocket::ServerEvent>,
) {
    let session = storage
        .create_session(NewSession {
            agent_id: agent_id.into(),
            ..swage_storage::fixtures::new_session(topic_id)
        })
        .await
        .expect("answering session");
    let turn_id = Uuid::new_v4();
    let mut base = swage_tool::context::MockToolContext::new(agent_id, session.id.to_string());
    base.turn_id = Some(turn_id);
    let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = state;
    (session, turn_id, base, ws_tx)
}

const HOLD_SCRIPT: &str = r#"
    const decision = await hold({
        content: "Ship the release?",
        verdicts: ["ship", "hold"],
    });
    return { verdict: decision.verdict, by: decision.answered_by.agent_id };
"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_is_invisible_and_unanswerable_from_another_topic() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let elsewhere = swage_storage::TopicStore::create(
        storage.as_ref(),
        swage_storage::model::NewTopic {
            name: "elsewhere".into(),
            slug: "elsewhere".parse().expect("slug"),
        },
    )
    .await
    .expect("a second topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: HOLD_SCRIPT.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start held run");
    wait_for_run_state(&storage, started.id, "parked").await;
    let hold_id = wait_for_open_hold(&storage, started.id).await;

    let (_session, _turn_id, base, ws_tx) =
        hold_answering_context(&storage, &state, elsewhere.id, "outsider").await;
    let context = crate::tool_executor::GatewayToolContext::new(
        &base,
        ws_tx,
        Arc::clone(&storage) as Arc<dyn Storage>,
    )
    .with_app_state(Some(state.clone()));

    let listed = crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({"action": "list"}),
        &context,
    )
    .await
    .expect("list open holds");
    assert!(
        listed["holds"].as_array().expect("holds").is_empty(),
        "a hold in another topic is not the caller's to see"
    );

    let refused = crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({
            "action": "answer",
            "hold_id": hold_id.to_string(),
            "verdict": "ship",
        }),
        &context,
    )
    .await
    .expect_err("a hold in another topic is not the caller's to settle");
    assert!(
        refused.to_string().contains("workflow run hold"),
        "the refusal names the hold as absent, not the topic: {refused}"
    );

    let hold = storage
        .get_workflow_run_hold(hold_id)
        .await
        .expect("read the hold back")
        .expect("the hold still exists");
    assert_eq!(hold.state, "open", "the decision keeps waiting");
    assert!(hold.verdict.is_none(), "no verdict was attributed");
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("read the run back")
        .expect("the run still exists");
    assert_eq!(run.state, "parked", "the run was not resumed from outside");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_run_parks_until_an_agent_answers_it_through_the_tool() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: HOLD_SCRIPT.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start held run");
    wait_for_run_state(&storage, started.id, "parked").await;
    wait_for_workflow_thread_exit(started.id).await;
    wait_for_active_workflow_run_release(&state, started.id).await;

    let (session, turn_id, base, ws_tx) =
        hold_answering_context(&storage, &state, topic.id, "decider").await;
    let context = crate::tool_executor::GatewayToolContext::new(
        &base,
        ws_tx,
        Arc::clone(&storage) as Arc<dyn Storage>,
    )
    .with_app_state(Some(state.clone()));

    let listed = crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({"action": "list"}),
        &context,
    )
    .await
    .expect("list open holds");
    assert_eq!(listed["holds"].as_array().expect("holds").len(), 1);
    let hold_id = listed["holds"][0]["id"].as_str().expect("hold id");
    assert_eq!(listed["holds"][0]["content"], "Ship the release?");
    assert_eq!(
        listed["holds"][0]["verdicts"],
        serde_json::json!(["ship", "hold"])
    );

    crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({
            "action": "answer",
            "hold_id": hold_id,
            "verdict": "ship",
            "agent_id": "spoofed",
        }),
        &context,
    )
    .await
    .expect("answer the hold");

    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({"verdict": "ship", "by": "decider"})),
        "the script resumes with the verdict it was answered with"
    );

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(detail.holds.len(), 1);
    let hold = &detail.holds[0];
    assert!(matches!(hold.state, WorkflowRunHoldState::Answered));
    assert_eq!(hold.verdict.as_deref(), Some("ship"));
    let answered_by = hold.answered_by.as_ref().expect("answering source");
    assert_eq!(answered_by.surface, "agent");
    assert_eq!(answered_by.agent_id.as_deref(), Some("decider"));
    assert_eq!(answered_by.session_id, Some(session.id));
    assert_eq!(answered_by.turn_id, Some(turn_id));
    assert!(hold.rejections.is_empty());
    assert!(
        detail
            .journal
            .iter()
            .any(|entry| matches!(entry.kind, WorkflowJournalEntryKind::Hold)),
        "the raise lands in the journal as its own kind"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answering_either_concurrent_hold_advances_the_workflow() {
    workflow_journey(async {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let root = tempfile::tempdir().expect("isolated concurrent hold root");
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        let api = WorkflowTestApi::start(state.clone()).await;
        let source = r#"
            const first = hold({ content: "Choose the release channel", verdicts: ["stable", "beta"] });
            const second = hold({ content: "Choose the announcement", verdicts: ["publish", "quiet"] });
            const answer = await Promise.race([first, second]);
            return { content: answer.content, verdict: answer.verdict };
        "#;
        let started = api.start_inline(
            topic.id,
            StartInlineWorkflowRunRequest {
                source: source.into(),
                args: Value::Null,
                budget: None,
            },
        )
        .await;
        wait_for_run_state(&storage, started.id, "parked").await;

        let holds = loop {
            let holds = api.detail(started.id).await.holds;
            if holds.len() == 2 {
                break holds;
            }
            tokio::task::yield_now().await;
        };
        let second = &holds[1];

        answer_run_hold(
            &state,
            AnswerWorkflowRunHoldRequest {
                hold_id: second.id,
                verdict: "publish".into(),
            },
            None,
            RunEventSource {
                surface: "operator",
                agent_id: None,
                session_id: None,
                turn_id: None,
            },
        )
        .await
        .expect("answer the later concurrent hold");

        wait_for_run_state(&storage, started.id, "completed").await;
        let completed = api.detail(started.id).await.run;
        assert_eq!(
            completed.final_output,
            Some(serde_json::json!({
                "content": "Choose the announcement",
                "verdict": "publish",
            }))
        );
        let holds = storage
            .list_workflow_run_holds(started.id)
            .await
            .expect("read terminal holds");
        assert!(
            holds.iter().any(|hold| hold.state == "closed"),
            "the losing concurrent hold closes when the run completes"
        );
        assert!(
            storage
                .list_open_workflow_run_holds(topic.id)
                .await
                .expect("list open holds")
                .is_empty(),
            "completed runs leave no answerable holds"
        );
        let detail = api.detail(started.id).await;
        assert!(
            detail
                .holds
                .iter()
                .any(|hold| matches!(hold.state, WorkflowRunHoldState::Closed))
        );
        assert_eq!(detail.hold_metrics.holds_open, 0);
        assert_eq!(detail.hold_metrics.holds_answered, 1);
        assert_eq!(detail.hold_metrics.answers_rejected, 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answering_the_first_concurrent_hold_advances_the_workflow() {
    workflow_journey(async {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let root = tempfile::tempdir().expect("isolated concurrent hold root");
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        let api = WorkflowTestApi::start(state.clone()).await;
        let source = r#"
            const first = hold({ content: "Choose the release channel", verdicts: ["stable", "beta"] });
            const second = hold({ content: "Choose the announcement", verdicts: ["publish", "quiet"] });
            const answer = await Promise.race([first, second]);
            return { content: answer.content, verdict: answer.verdict };
        "#;
        let started = api.start_inline(
            topic.id,
            StartInlineWorkflowRunRequest {
                source: source.into(),
                args: Value::Null,
                budget: None,
            },
        )
        .await;
        wait_for_run_state(&storage, started.id, "parked").await;

        let holds = loop {
            let holds = api.detail(started.id).await.holds;
            if holds.len() == 2 {
                break holds;
            }
            tokio::task::yield_now().await;
        };
        let first = &holds[0];
        assert!(
            matches!(holds[1].state, WorkflowRunHoldState::Open),
            "the second hold is still open when the first decision is answered"
        );

        answer_run_hold(
            &state,
            AnswerWorkflowRunHoldRequest {
                hold_id: first.id,
                verdict: "stable".into(),
            },
            None,
            RunEventSource {
                surface: "operator",
                agent_id: None,
                session_id: None,
                turn_id: None,
            },
        )
        .await
        .expect("answer the first concurrent hold");

        wait_for_run_state(&storage, started.id, "completed").await;
        let completed = api.detail(started.id).await.run;
        assert_eq!(
            completed.final_output,
            Some(serde_json::json!({
                "content": "Choose the release channel",
                "verdict": "stable",
            }))
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_outside_the_hold_s_set_is_refused_with_feedback_and_the_hold_keeps_waiting() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: HOLD_SCRIPT.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start held run");
    wait_for_run_state(&storage, started.id, "parked").await;

    let (session, turn_id, base, ws_tx) =
        hold_answering_context(&storage, &state, topic.id, "decider").await;
    let context = crate::tool_executor::GatewayToolContext::new(
        &base,
        ws_tx,
        Arc::clone(&storage) as Arc<dyn Storage>,
    )
    .with_app_state(Some(state.clone()));
    let holds = state
        .storage
        .list_workflow_run_holds(started.id)
        .await
        .expect("holds");
    let hold_id = holds[0].id;

    let refused = crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({"action": "answer", "hold_id": hold_id, "verdict": "maybe"}),
        &context,
    )
    .await
    .expect_err("an out-of-set verdict is refused");
    let feedback = refused.to_string();
    assert!(
        feedback.contains("maybe") && feedback.contains("ship") && feedback.contains("hold"),
        "the refusal names the attempt and the accepted set: {feedback}"
    );

    let still_parked = wait_for_run_state(&storage, started.id, "parked").await;
    assert_eq!(still_parked.state, "parked", "the hold keeps waiting");
    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    let hold = &detail.holds[0];
    assert!(matches!(hold.state, WorkflowRunHoldState::Open));
    assert_eq!(hold.rejections.len(), 1, "the refusal lands on the record");
    let rejection = &hold.rejections[0];
    assert_eq!(rejection.attempted_verdict, "maybe");
    assert_eq!(rejection.source.agent_id.as_deref(), Some("decider"));
    assert_eq!(rejection.source.session_id, Some(session.id));
    assert_eq!(rejection.source.turn_id, Some(turn_id));
    assert_eq!(
        state
            .storage
            .list_workflow_journal(started.id)
            .await
            .expect("journal")
            .len(),
        1,
        "a refused answer never moves the script's own sequence"
    );

    crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({"action": "answer", "hold_id": hold_id, "verdict": "hold"}),
        &context,
    )
    .await
    .expect("an in-set verdict is taken after a refusal");
    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({"verdict": "hold", "by": "decider"}))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_answer_cannot_be_recorded_after_the_hold_closes() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: HOLD_SCRIPT.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start held run");
    wait_for_run_state(&storage, started.id, "parked").await;
    let hold = storage
        .list_workflow_run_holds(started.id)
        .await
        .expect("holds")
        .into_iter()
        .next()
        .expect("open hold");

    storage
        .update_workflow_run_state(started.id, "running", None, None, None)
        .await
        .expect("enter terminalisation window");
    assert_eq!(
        storage
            .close_open_workflow_run_holds(started.id)
            .await
            .expect("close hold"),
        1
    );
    assert!(
        storage
            .create_workflow_run_hold_rejection(NewWorkflowRunHoldRejection {
                hold_id: hold.id,
                attempted_verdict: "maybe".into(),
                feedback: "not an accepted verdict".into(),
                source_surface: "operator".into(),
                source_agent_id: None,
                source_session_id: None,
                source_turn_id: None,
            })
            .await
            .expect("attempt atomic rejection insert")
            .is_none(),
        "the storage write must recheck the hold and run state"
    );

    let error = answer_run_hold(
        &state,
        AnswerWorkflowRunHoldRequest {
            hold_id: hold.id,
            verdict: "maybe".into(),
        },
        None,
        RunEventSource {
            surface: "operator",
            agent_id: None,
            session_id: None,
            turn_id: None,
        },
    )
    .await
    .expect_err("a closed hold cannot record a rejected answer");
    assert_eq!(
        error.code(),
        swage_protocol::client::error::ErrorCode::Conflict
    );
    assert!(
        storage
            .list_workflow_run_hold_rejections(hold.id)
            .await
            .expect("rejections")
            .is_empty(),
        "closed holds cannot gain rejection history"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_detail_counts_what_is_still_waiting_what_was_decided_and_what_was_refused() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    // Two decisions in sequence: the first is answered, the second is
    // still waiting after a refused attempt.
    let source = r#"
        const first = await hold({ content: "Ship?", verdicts: ["ship", "hold"] });
        const second = await hold({ content: "Announce?", verdicts: ["announce", "quiet"] });
        return { first: first.verdict, second: second.verdict };
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start held run");
    wait_for_run_state(&storage, started.id, "parked").await;

    let (_session, _turn_id, base, ws_tx) =
        hold_answering_context(&storage, &state, topic.id, "decider").await;
    let context = crate::tool_executor::GatewayToolContext::new(
        &base,
        ws_tx,
        Arc::clone(&storage) as Arc<dyn Storage>,
    )
    .with_app_state(Some(state.clone()));

    let first_hold = state
        .storage
        .list_workflow_run_holds(started.id)
        .await
        .expect("holds")[0]
        .id;
    crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({"action": "answer", "hold_id": first_hold, "verdict": "ship"}),
        &context,
    )
    .await
    .expect("answer the first decision");
    wait_for_run_state(&storage, started.id, "parked").await;

    let second_hold = *state
        .storage
        .list_workflow_run_holds(started.id)
        .await
        .expect("holds")
        .iter()
        .map(|hold| &hold.id)
        .nth(1)
        .expect("the second decision is raised");
    crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({"action": "answer", "hold_id": second_hold, "verdict": "maybe"}),
        &context,
    )
    .await
    .expect_err("an out-of-set answer is refused");

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(detail.hold_metrics.holds_answered, 1);
    assert_eq!(detail.hold_metrics.holds_open, 1);
    assert_eq!(detail.hold_metrics.answers_rejected, 1);
    assert_eq!(
        detail.hold_metrics.holds_open as usize + detail.hold_metrics.holds_answered as usize,
        detail.holds.len(),
        "the counts are derived from the very rows beside them"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_offering_fewer_than_two_verdicts_is_refused_at_raise() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        try {
            await hold({ content: "Ship it?", verdicts: ["ship", "ship"] });
            return { raised: true };
        } catch (error) {
            return { refused: String(error.message ?? error) };
        }
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start single-verdict run");
    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    let refused = completed
        .final_output
        .expect("output")
        .get("refused")
        .and_then(Value::as_str)
        .expect("the raise is refused, never opened")
        .to_owned();
    assert!(
        refused.contains("two"),
        "the refusal says a decision needs a choice: {refused}"
    );
    assert!(
        state
            .storage
            .list_workflow_run_holds(started.id)
            .await
            .expect("holds")
            .is_empty(),
        "a refused raise opens no hold"
    );
    assert!(
        state
            .storage
            .list_workflow_journal(started.id)
            .await
            .expect("journal")
            .iter()
            .all(|entry| entry.kind != "hold"),
        "a refused raise records no hold in the journal"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_run_waiting_on_a_hold_closes_the_unanswered_decision() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: HOLD_SCRIPT.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start held run");
    wait_for_run_state(&storage, started.id, "parked").await;

    let Json(cancelled) = cancel_run(State(state.clone()), Path(started.id))
        .await
        .expect("cancel the waiting run");
    assert_eq!(cancelled.state, WorkflowRunState::Cancelled);

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert!(matches!(detail.run.state, WorkflowRunState::Cancelled));
    let hold = &detail.holds[0];
    assert!(
        matches!(hold.state, WorkflowRunHoldState::Closed),
        "cancellation ends the wait without inventing a verdict"
    );
    assert!(hold.verdict.is_none());
    assert_eq!(detail.hold_metrics.holds_open, 0);
    assert!(
        storage
            .list_open_workflow_run_holds(topic.id)
            .await
            .expect("list actionable holds")
            .is_empty(),
        "cancelled runs leave no decision in the answering queue"
    );

    let (_session, _turn_id, base, ws_tx) =
        hold_answering_context(&storage, &state, topic.id, "decider").await;
    let context = crate::tool_executor::GatewayToolContext::new(
        &base,
        ws_tx,
        Arc::clone(&storage) as Arc<dyn Storage>,
    )
    .with_app_state(Some(state.clone()));
    let refused = crate::tool::Tool::execute(
        &crate::tool::hold::WorkflowHoldTool,
        serde_json::json!({"action": "answer", "hold_id": hold.id, "verdict": "ship"}),
        &context,
    )
    .await
    .expect_err("a cancelled run's hold cannot be answered");
    assert!(
        refused.to_string().contains("cancelled"),
        "the tool says why: {refused}"
    );
}

/// A run parked on a decision, with its engine torn down and the
/// decision waiting in the answering queue.
async fn parked_on_a_hold(
    state: &AppState,
    topic_id: Uuid,
    storage: &Arc<SqliteStorage>,
) -> (WorkflowRun, Uuid) {
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: HOLD_SCRIPT.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start held run");
    wait_for_run_state(storage, started.id, "parked").await;
    wait_for_active_workflow_run_release(state, started.id).await;
    let hold_id = wait_for_open_hold(storage, started.id).await;
    (started, hold_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answered_hold_reaches_its_script_after_a_crash_before_the_resume() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (started, hold_id) = parked_on_a_hold(&state, topic.id, &storage).await;

    // The answerer dies with the verdict recorded and the relaunch it
    // would have issued never issued — the crash a wake pass must
    // recover from on its own.
    let pause = workflow_pause_test_hook::install(PAUSE_AFTER_HOLD_SETTLEMENT, hold_id);
    let answering = tokio::spawn({
        let state = state.clone();
        async move { answer_open_hold(&state, hold_id).await }
    });
    pause
        .entered
        .acquire()
        .await
        .expect("settlement hook remains open")
        .forget();
    answering.abort();

    let owed = resume_owed_workflow_runs(&state).await;
    assert_eq!(owed, 1, "the wake pass owes the answered run its relaunch");
    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({"verdict": "ship", "by": null})),
        "an accepted verdict reaches the script even when nothing ran after it was recorded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_recovered_behind_a_settled_sequential_hold_parks() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        const first = await hold({ content: "Choose the release channel", verdicts: ["stable", "beta"] });
        const second = await hold({ content: "Choose the announcement", verdicts: ["publish", "quiet"] });
        return { first: first.verdict, second: second.verdict };
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start sequentially held run");
    wait_for_run_state(&storage, started.id, "parked").await;
    wait_for_active_workflow_run_release(&state, started.id).await;
    let first_id = wait_for_open_hold(&storage, started.id).await;
    answer_run_hold(
        &state,
        AnswerWorkflowRunHoldRequest {
            hold_id: first_id,
            verdict: "stable".into(),
        },
        None,
        RunEventSource {
            surface: "operator",
            agent_id: None,
            session_id: None,
            turn_id: None,
        },
    )
    .await
    .expect("answer the first hold");
    // The run resumes on the first verdict and parks on the second.
    wait_for_run_state(&storage, started.id, "parked").await;
    wait_for_active_workflow_run_release(&state, started.id).await;
    let second_id = storage
        .list_workflow_run_holds(started.id)
        .await
        .expect("list holds")
        .into_iter()
        .find(|hold| hold.state == "open")
        .expect("the second hold is open")
        .id;

    // The gateway dies before the second park is observed: recovery
    // finds the run interrupted with the first decision settled and the
    // second still open.
    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("interrupt");
    assert_eq!(resume_owed_workflow_runs(&state).await, 1);

    // A hold raised after a settled sibling is sequential, not
    // concurrent: recovery parks it durably instead of keeping an engine
    // up to watch it.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_for_run_state(&storage, started.id, "parked"),
    )
    .await
    .expect("a recovered sequential hold parks its run");
    wait_for_active_workflow_run_release(&state, started.id).await;

    answer_run_hold(
        &state,
        AnswerWorkflowRunHoldRequest {
            hold_id: second_id,
            verdict: "publish".into(),
        },
        None,
        RunEventSource {
            surface: "operator",
            agent_id: None,
            session_id: None,
            turn_id: None,
        },
    )
    .await
    .expect("answer the second hold");
    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({"first": "stable", "second": "publish"}))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_that_loses_to_a_cancellation_launches_no_engine() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (started, hold_id) = parked_on_a_hold(&state, topic.id, &storage).await;

    let pause = workflow_pause_test_hook::install(PAUSE_AFTER_HOLD_SETTLEMENT, hold_id);
    let answering = tokio::spawn({
        let state = state.clone();
        async move { answer_open_hold(&state, hold_id).await }
    });
    pause
        .entered
        .acquire()
        .await
        .expect("settlement hook remains open")
        .forget();

    let waiting = storage
        .get_workflow_run(started.id)
        .await
        .expect("read run")
        .expect("run");
    assert!(
        waiting
            .wake_at
            .is_some_and(|wake| wake <= chrono::Utc::now()),
        "the verdict and the run's wake land together: nothing observes an answered hold on a run still sleeping"
    );

    let Json(cancelled) = cancel_run(State(state.clone()), Path(started.id))
        .await
        .expect("the cancellation claims the still-parked run");
    assert_eq!(cancelled.state, WorkflowRunState::Cancelled);

    pause.release.add_permits(1);
    answering.await.expect("the answering call finishes");

    assert_eq!(
        resume_owed_workflow_runs(&state).await,
        0,
        "a cancelled run is owed no relaunch"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let settled = storage
        .get_workflow_run(started.id)
        .await
        .expect("read run")
        .expect("run");
    assert_eq!(
        settled.state, "cancelled",
        "the loser leaves the winner's state alone"
    );
    assert!(
        state.active_workflow_runs.lock().await.is_empty(),
        "no engine runs for a run whose cancellation won"
    );
    assert!(
        !workflow_thread_exists(started.id),
        "the cancelled run has no engine thread"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancellation_that_loses_to_an_answer_leaves_the_resumed_run_alone() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (started, hold_id) = parked_on_a_hold(&state, topic.id, &storage).await;

    // The cancellation reads a parked run and is held there while the
    // answer settles the hold, relaunches the run, and lets it finish.
    let pause = workflow_pause_test_hook::install(PAUSE_BEFORE_CANCELLATION_CLAIM, started.id);
    let cancelling = tokio::spawn({
        let state = state.clone();
        async move { cancel_run(State(state), Path(started.id)).await }
    });
    pause
        .entered
        .acquire()
        .await
        .expect("cancellation hook remains open")
        .forget();

    answer_open_hold(&state, hold_id).await;
    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({"verdict": "ship", "by": null}))
    );

    pause.release.add_permits(1);
    let refused = cancelling
        .await
        .expect("cancellation task")
        .expect_err("a cancellation that lost the claim settles nothing");
    assert!(
        refused.to_string().contains("no longer parked"),
        "the loser is told the winner's state: {refused}"
    );

    let settled = storage
        .get_workflow_run(started.id)
        .await
        .expect("read run")
        .expect("run");
    assert_eq!(
        settled.state, "completed",
        "a losing cancellation never overwrites the winner's outcome"
    );
    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert!(matches!(
        detail.holds[0].state,
        WorkflowRunHoldState::Answered
    ));
    assert!(
        detail
            .journal
            .iter()
            .all(|entry| !matches!(entry.status, WorkflowJournalEntryStatus::CutShort)),
        "a losing cancellation cuts nothing short of the run it did not claim"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_run_closes_a_decision_it_can_no_longer_receive() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    // The timer is armed before the hold is raised: an armed sibling
    // timer refuses the hold's park, so the engine stays up and the
    // script's failure reaches the open decision. Raised the other way
    // round, a host that drains the hold before the timer command is
    // queued parks the run on the decision, and the failure never comes.
    // Both calls are issued in one script tick, so their commands queue
    // in call order; the timer outlasts that tick by a wide margin.
    let source = r#"
        const failing = wait({ timerMs: 100 });
        hold({ content: "Ship the release?", verdicts: ["ship", "hold"] });
        await failing;
        throw new Error("the workflow cannot continue");
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start failing run");
    let failed = wait_for_run_state(&storage, started.id, "failed").await;
    assert_eq!(failed.state, "failed");

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert!(matches!(detail.run.state, WorkflowRunState::Failed));
    assert_eq!(detail.holds.len(), 1);
    assert!(matches!(
        detail.holds[0].state,
        WorkflowRunHoldState::Closed
    ));
    assert!(detail.holds[0].verdict.is_none());
    assert_eq!(detail.hold_metrics.holds_open, 0);
    assert!(
        storage
            .list_open_workflow_run_holds(topic.id)
            .await
            .expect("list open holds")
            .is_empty(),
        "failed runs offer no unanswerable decision"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_event_wakes_awaiting_run_and_replay_returns_recorded_answer() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gateway listener");
    let address = listener.local_addr().expect("gateway address");
    let server = tokio::spawn({
        let state = state.clone();
        async move {
            axum::serve(listener, crate::server::build_router(state))
                .await
                .expect("serve gateway router");
        }
    });
    let events_url = |run_id| format!("http://{address}/api/workflow-runs/{run_id}/events");
    let client = reqwest::Client::new();
    let source = r#"
        const answer = await awaitEvent({ kinds: ["operator.decision"] });
        return { verdict: answer.payload.verdict, source: answer.source.surface };
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start awaiting run");
    wait_for_run_state(&storage, started.id, "parked").await;

    let invalid = client
        .post(events_url(started.id))
        .json(&PostWorkflowRunEventRequest {
            kind: "not namespaced".into(),
            payload: Value::Null,
        })
        .send()
        .await
        .expect("send invalid operator post");
    assert_eq!(invalid.status().as_u16(), 422);
    assert_eq!(
        invalid
            .json::<Value>()
            .await
            .expect("decode invalid operator response")["code"],
        "validation_error"
    );

    let unrelated = client
        .post(events_url(started.id))
        .json(&PostWorkflowRunEventRequest {
            kind: "extension.future-kind".into(),
            payload: serde_json::json!({"online": true}),
        })
        .send()
        .await
        .expect("send unrelated operator post")
        .error_for_status()
        .expect("unrelated operator post status")
        .json::<PostWorkflowRunEventResponse>()
        .await
        .expect("decode unrelated operator response");
    assert!(unrelated.wake_provisioned);
    wait_for_run_state(&storage, started.id, "parked").await;

    let response = client
        .post(events_url(started.id))
        .json(&PostWorkflowRunEventRequest {
            kind: " operator.decision ".into(),
            payload: serde_json::json!({"nonce": "n-1", "verdict": "accept"}),
        })
        .send()
        .await
        .expect("send operator post")
        .error_for_status()
        .expect("operator post status")
        .json::<PostWorkflowRunEventResponse>()
        .await
        .expect("decode operator response");
    assert!(response.wake_provisioned);
    assert_eq!(response.event.kind, "operator.decision");
    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({"verdict": "accept", "source": "operator"}))
    );

    let events = storage
        .list_workflow_run_events(started.id)
        .await
        .expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "extension.future-kind");
    assert_eq!(events[1].kind, "operator.decision");
    assert!(
        events
            .iter()
            .all(|event| event.source_surface == "operator")
    );
    assert!(events[0].delivered_at.is_none());
    assert!(events[1].delivered_at.is_some());
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let await_event = journal
        .iter()
        .find(|entry| entry.kind == "await_event")
        .expect("await-event journal entry");
    assert_eq!(await_event.status, "completed");

    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("replay state");
    let replayed = resume_workflow_run_record(
        state.clone(),
        storage.get_workflow_run(started.id).await.unwrap().unwrap(),
    )
    .await
    .expect("replay run");
    assert_eq!(replayed.state, WorkflowRunState::Running);
    let replayed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(replayed.final_output, completed.final_output);
    assert_eq!(
        storage
            .list_workflow_run_events(started.id)
            .await
            .unwrap()
            .len(),
        2
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn shipped_holds_acknowledge_only_the_answered_request_across_replay() {
    tokio::time::timeout(std::time::Duration::from_secs(30), Box::pin(async {
        for pipeline in [
            include_str!("../../../../shipped/skills/assembly/workflows/build-verify.js"),
            include_str!("../../../../shipped/skills/assembly/workflows/docs-update.js"),
            include_str!("../../../../shipped/skills/assembly/workflows/estate-unit.js"),
        ] {
            let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
            let topic = storage.general_scope().await.expect("topic");
            let make_state = || AppState::new(
                Arc::clone(&storage) as Arc<dyn Storage>, Arc::clone(&storage) as Arc<dyn TopicStore>,
                Arc::new(crate::turn::tests::MockProvider::new(vec![])),
                swage_config::ResolvedConfig::default(), crate::tool::ToolDispatcher::allow_all(vec![]), None,
            );
            let state = make_state();
            let start = pipeline.find("let holdCount = 0;").unwrap();
            let end = pipeline[start..].find("// ---------------------------------------------------------------- restate").unwrap() + start;
            let source = format!(r#"{}
                await postEvent("worker.progress", {{ message: "keep pending" }});
                await postEvent("decision.needed", {{ nonce: "parallel", options: ["accept"] }});
                const first = await runHold("first", {{}}, ["accept"]);
                const second = await runHold("second", {{}}, ["accept"]);
                return [first.nonce, second.nonce];
            "#, &pipeline[start..end]);
            let Json(started) = start_inline_run(State(state.clone()), Path(topic.id), Json(StartInlineWorkflowRunRequest {
                source, args: Value::Null, budget: None,
            })).await.expect("start hold fixture");
            wait_for_run_state(&storage, started.id, "parked").await;
            for payload in [
                serde_json::json!({"nonce": "stale", "verdict": "accept"}),
                serde_json::json!({"nonce": "first-1", "verdict": "unknown"}),
                serde_json::json!({"nonce": "first-1", "verdict": "accept", "note": 17}),
            ] {
                Box::pin(post_operator_run_event(State(state.clone()), Path(started.id), Json(PostWorkflowRunEventRequest {
                    kind: "decision.answer".into(), payload,
                }))).await.expect("invalid answer");
                wait_for_run_state(&storage, started.id, "parked").await;
                let pending = storage.list_pending_workflow_run_events(started.id).await.unwrap();
                assert_eq!(pending.len(), 3, "invalid answer must not resolve the request");
                assert!(pending.iter().any(|event| event.payload["nonce"] == "first-1"));
            }
            Box::pin(post_operator_run_event(State(state.clone()), Path(started.id), Json(PostWorkflowRunEventRequest {
                kind: "decision.answer".into(), payload: serde_json::json!({"nonce": "first-1", "verdict": "accept"}),
            }))).await.expect("first answer");
            wait_for_run_state(&storage, started.id, "parked").await;
            let pending = storage.list_pending_workflow_run_events(started.id).await.unwrap();
            assert_eq!(pending.len(), 3);
            assert!(!pending.iter().any(|event| event.payload["nonce"] == "first-1"));
            assert!(pending.iter().any(|event| event.payload["nonce"] == "second-2"));

            // A fresh gateway reconstructs the parked run from its stored journal.
            drop(state);
            storage.update_workflow_run_state(started.id, "interrupted", None, None, None).await.unwrap();
            let state = make_state();
            resume_workflow_run_record(state.clone(), storage.get_workflow_run(started.id).await.unwrap().unwrap())
                .await.expect("resume on fresh gateway state");
            wait_for_run_state(&storage, started.id, "parked").await;
            Box::pin(post_operator_run_event(State(state.clone()), Path(started.id), Json(PostWorkflowRunEventRequest {
                kind: "decision.answer".into(), payload: serde_json::json!({"nonce": "second-2", "verdict": "accept"}),
            }))).await.expect("second answer");
            let completed = wait_for_run_state(&storage, started.id, "completed").await;
            assert_eq!(completed.final_output, Some(serde_json::json!(["first-1", "second-2"])));
            let pending = storage.list_pending_workflow_run_events(started.id).await.unwrap();
            assert_eq!(pending.len(), 2);
            assert_eq!(pending[0].kind, "worker.progress");
            assert_eq!(pending[1].payload["nonce"], "parallel");
            let events = storage.list_workflow_run_events(started.id).await.unwrap();
            assert_eq!(events.len(), 9, "replay does not duplicate requests or erase audit history");
        }
    })).await.expect("holds settle within test deadline");
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_event_acknowledgement_recovers_after_stamp_before_journal_settlement() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let event = storage
        .create_workflow_run_event(NewWorkflowRunEvent {
            run_id: run.id,
            journal_entry_id: Uuid::new_v4(),
            kind: "decision.needed".into(),
            payload: serde_json::json!({"nonce": "first"}),
            source_surface: "script".into(),
            source_agent_id: None,
            source_session_id: None,
            source_turn_id: None,
        })
        .await
        .unwrap();
    let request = serde_json::json!({"action": "deliver", "event_seq": event.seq});
    let intent = state
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "event".into(),
            args_hash: journal_args_hash(&request),
            request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .unwrap();
    storage
        .mark_workflow_run_event_delivered(run.id, event.seq, 0)
        .await
        .unwrap();
    let result = complete_events_delivery(
        &state,
        run.id,
        intent.id,
        0,
        &EventDelivery::One {
            event_seq: event.seq,
        },
    )
    .await
    .expect("recover started acknowledgement");
    assert_eq!(result, serde_json::json!({"delivered": 1}));
    let journal = storage.list_workflow_journal(run.id).await.unwrap();
    assert_eq!(journal[0].status, "completed");
    assert_eq!(journal[0].result, Some(result));
}

#[tokio::test(flavor = "multi_thread")]
async fn sequential_same_kind_awaits_each_receive_one_queued_event() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                await awaitEvent({ kinds: ["control.start"] });
                const first = await awaitEvent({ kinds: ["operator.decision"] });
                const second = await awaitEvent({ kinds: ["operator.decision"] });
                return {
                    seqs: [first.seq, second.seq],
                    verdicts: [first.payload.verdict, second.payload.verdict],
                };
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start awaiting run");
    wait_for_run_state(&storage, started.id, "parked").await;

    let Json(_) = Box::pin(post_operator_run_event(
        State(state.clone()),
        Path(started.id),
        Json(PostWorkflowRunEventRequest {
            kind: "operator.decision".into(),
            payload: serde_json::json!({"verdict": "first"}),
        }),
    ))
    .await
    .expect("queue first same-kind answer");
    wait_for_run_state(&storage, started.id, "parked").await;
    let Json(_) = Box::pin(post_operator_run_event(
        State(state.clone()),
        Path(started.id),
        Json(PostWorkflowRunEventRequest {
            kind: "operator.decision".into(),
            payload: serde_json::json!({"verdict": "second"}),
        }),
    ))
    .await
    .expect("queue second same-kind answer");
    wait_for_run_state(&storage, started.id, "parked").await;
    let Json(_) = Box::pin(post_operator_run_event(
        State(state),
        Path(started.id),
        Json(PostWorkflowRunEventRequest {
            kind: "control.start".into(),
            payload: Value::Null,
        }),
    ))
    .await
    .expect("release queued awaits");

    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({
            "seqs": [1, 2],
            "verdicts": ["first", "second"],
        }))
    );
    let events = storage
        .list_workflow_run_events(started.id)
        .await
        .expect("events");
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|event| event.delivered_at.is_some()));
    assert_ne!(events[0].delivered_wake_seq, events[1].delivered_wake_seq);
    assert_eq!(events[0].delivered_wake_seq, Some(1));
    assert_eq!(events[1].delivered_wake_seq, Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_recovers_event_claimed_before_await_journal_completion() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                const answer = await awaitEvent({ kinds: ["operator.decision"] });
                return { seq: answer.seq, verdict: answer.payload.verdict };
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start awaiting run");
    wait_for_run_state(&storage, started.id, "parked").await;

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("started await journal");
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].status, "started");
    let event = storage
        .create_workflow_run_event(NewWorkflowRunEvent {
            run_id: started.id,
            journal_entry_id: Uuid::new_v4(),
            kind: "operator.decision".into(),
            payload: serde_json::json!({"verdict": "accept"}),
            source_surface: "operator".into(),
            source_agent_id: None,
            source_session_id: None,
            source_turn_id: None,
        })
        .await
        .expect("persist event");
    assert!(
        storage
            .mark_workflow_run_event_delivered(started.id, event.seq, journal[0].seq)
            .await
            .expect("simulate delivery before crash")
    );
    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate process interruption");

    let replayed = resume_workflow_run_record(
        state,
        storage
            .get_workflow_run(started.id)
            .await
            .expect("get interrupted run")
            .expect("interrupted run"),
    )
    .await
    .expect("resume interrupted run");
    assert_eq!(replayed.state, WorkflowRunState::Running);
    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({"seq": event.seq, "verdict": "accept"}))
    );
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("recovered await journal");
    assert_eq!(journal[0].status, "completed");
    assert_eq!(journal[0].result.as_ref().unwrap()["seq"], event.seq);
    assert!(
        storage
            .list_pending_workflow_run_events(started.id)
            .await
            .expect("pending events")
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn script_event_operations_reject_invalid_kinds_without_journalling_them() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                let postError;
                let awaitError;
                try { await postEvent("", null); }
                catch (error) { postError = error.message; }
                try { await awaitEvent({ kinds: ["operator..decision"] }); }
                catch (error) { awaitError = error.message; }
                return { postError, awaitError };
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start invalid-kind run");

    let completed = wait_for_run_state(&storage, started.id, "completed").await;
    let output = completed.final_output.expect("invalid-kind output");
    assert!(
        output["postError"]
            .as_str()
            .is_some_and(|error| error.contains("non-empty namespaced name"))
    );
    assert!(
        output["awaitError"]
            .as_str()
            .is_some_and(|error| error.contains("non-empty namespaced name"))
    );
    assert!(
        storage
            .list_workflow_journal(started.id)
            .await
            .expect("invalid-kind journal")
            .iter()
            .all(|entry| entry.kind == "terminal_budget")
    );
    assert!(
        storage
            .list_workflow_run_events(started.id)
            .await
            .expect("invalid-kind events")
            .is_empty()
    );
}

async fn workflow_thread_exit(run_id: Uuid) {
    while workflow_thread_exists(run_id) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn active_workflow_run_release(state: &AppState, run_id: Uuid) {
    while state
        .active_workflow_runs
        .lock()
        .await
        .contains_key(&run_id)
    {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn wait_for_workflow_thread_exit(run_id: Uuid) {
    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        workflow_thread_exit(run_id),
    )
    .await
    .unwrap_or_else(|_| panic!("workflow thread for run {run_id} remained alive"));
}

async fn wait_for_active_workflow_run_release(state: &AppState, run_id: Uuid) {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        active_workflow_run_release(state, run_id),
    )
    .await
    .unwrap_or_else(|_| panic!("active workflow registration for run {run_id} remained installed"));
}

fn spawn_event_loop(
    state: AppState,
    run_id: Uuid,
    topic_id: Uuid,
    replay: Vec<WorkflowJournalRecord>,
) -> (
    mpsc::UnboundedSender<HostCommand>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, host) = queued_event_loop(state, run_id, topic_id, replay);
    (tx, tokio::spawn(host))
}

fn queued_event_loop(
    state: AppState,
    run_id: Uuid,
    topic_id: Uuid,
    replay: Vec<WorkflowJournalRecord>,
) -> (
    mpsc::UnboundedSender<HostCommand>,
    impl std::future::Future<Output = ()> + Send + 'static,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let host = run_host_commands(
        state,
        run_id,
        topic_id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: None,
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: None,
        },
        replay,
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    );
    (tx, host)
}

#[tokio::test(flavor = "multi_thread")]
async fn await_event_consumes_match_that_arrives_during_park_cas() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    storage
        .with_conn(|conn| {
            conn.execute_batch(&format!(
                "CREATE TEMP TRIGGER insert_event_before_await_park \
                 BEFORE UPDATE OF state ON workflow_runs \
                 WHEN OLD.id = '{}' AND OLD.state = 'running' AND NEW.state = 'parked' \
                 BEGIN \
                   INSERT INTO workflow_run_events (id, run_id, journal_entry_id, seq, kind, payload_json, source_surface, posted_at) \
                   VALUES (lower(hex(randomblob(16))), OLD.id, lower(hex(randomblob(16))), \
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM workflow_run_events WHERE run_id = OLD.id), \
                     'operator.decision', '{{\"verdict\":\"accept\"}}', 'operator', '2026-08-20T00:00:00Z'); \
                   SELECT RAISE(IGNORE); \
                 END;",
                run.id
            ))
        })
        .await
        .expect("install read-to-park race trigger");
    let (tx, task) = spawn_event_loop(state, run.id, run.topic_id, vec![]);

    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::AwaitEvent {
        request: AwaitEventRequest {
            kinds: vec!["operator.decision".into()],
        },
        reply,
    })
    .expect("await event");
    let answer = receive
        .await
        .expect("await reply")
        .expect("race winner is consumed");

    assert_eq!(answer["payload"]["verdict"], "accept");
    assert_eq!(answer["source"]["surface"], "operator");
    let events = storage
        .list_workflow_run_events(run.id)
        .await
        .expect("events");
    assert_eq!(events.len(), 1);
    assert!(events[0].delivered_at.is_some());
    drop(tx);
    task.await.expect("host loop");
}

async fn send_post_event(
    tx: &mpsc::UnboundedSender<HostCommand>,
    kind: &str,
    payload: Value,
) -> Result<Value, String> {
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::PostEvent {
        event: RunEventPost {
            kind: kind.into(),
            payload,
        },
        reply,
    })
    .expect("post event");
    receive.await.expect("post reply")
}

async fn send_gate(
    tx: &mpsc::UnboundedSender<HostCommand>,
    criteria: &str,
) -> Result<Value, String> {
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Gate {
        request: GateRequest {
            criteria: criteria.into(),
            model: None,
        },
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("gate");
    receive.await.expect("gate reply")
}

#[tokio::test(flavor = "multi_thread")]
async fn suppressed_events_ride_the_next_wake_and_metrics_derive() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::from([
            r#"{"wake": false, "reasoning": "only one pipeline has settled"}"#,
            r#"{"wake": true, "reasoning": "both pipelines settled with evidence"}"#,
        ])),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(Arc::clone(&provider)).await;
    let (tx, task) = spawn_event_loop(state.clone(), run.id, run.topic_id, vec![]);

    let first = send_post_event(
        &tx,
        "worker.completion",
        serde_json::json!({"worker": "w1"}),
    )
    .await
    .expect("first post");
    assert_eq!(first["seq"], 1);
    let suppressed = send_gate(&tx, "wake when every pipeline has settled")
        .await
        .expect("first gate");
    assert_eq!(suppressed["wake"], false);
    assert_eq!(suppressed["read_up_to_seq"], 1);

    // The suppressed event is still pending: the next gate reads it
    // again beside the new one — suppression consumes nothing.
    send_post_event(
        &tx,
        "worker.completion",
        serde_json::json!({"worker": "w2"}),
    )
    .await
    .expect("second post");
    let waking = send_gate(&tx, "wake when every pipeline has settled")
        .await
        .expect("second gate");
    assert_eq!(waking["wake"], true);
    assert_eq!(waking["events"].as_array().expect("events").len(), 2);
    assert_eq!(waking["read_up_to_seq"], 2);

    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::DeliverEvents {
        selection: EventDelivery::Through { up_to_seq: 2 },
        reply,
    })
    .expect("deliver");
    let delivered = receive.await.expect("deliver reply").expect("delivered");
    assert_eq!(delivered["delivered"], 2);
    drop(tx);
    task.await.expect("host loop");

    let detail = get_run(State(state), Path(run.id)).await.expect("detail").0;
    assert_eq!(detail.wake_metrics.gate_evaluations, 2);
    assert_eq!(detail.wake_metrics.gate_wakes, 1);
    assert_eq!(detail.wake_metrics.gate_suppressions, 1);
    assert_eq!(detail.wake_metrics.events_posted, 2);
    assert_eq!(detail.wake_metrics.events_pending, 0);
    let kinds: Vec<_> = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal")
        .into_iter()
        .map(|entry| entry.kind)
        .collect();
    assert_eq!(kinds, ["event", "gate", "event", "gate", "event"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_treats_an_unfamiliar_event_kind_as_wakeable_data() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::from([
            r#"{"wake": true, "reasoning": "the custom signal requires attention"}"#,
        ])),
        calls: AtomicUsize::new(0),
    });
    let (_storage, state, run) = event_gate_fixture(Arc::clone(&provider)).await;
    let (tx, task) = spawn_event_loop(state, run.id, run.topic_id, vec![]);

    send_post_event(
        &tx,
        " experiment.signal-v2 ",
        serde_json::json!({"outcome": "unexpected"}),
    )
    .await
    .expect("post unfamiliar kind");
    let verdict = send_gate(&tx, "wake for novel experimental outcomes")
        .await
        .expect("gate unfamiliar kind");

    assert_eq!(verdict["wake"], true);
    assert_eq!(verdict["events"][0]["kind"], "experiment.signal-v2");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn replayed_posts_and_gates_return_recorded_results_without_reexecution() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::from([
            r#"{"wake": false, "reasoning": "nothing decisive yet"}"#,
        ])),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(Arc::clone(&provider)).await;
    let (tx, task) = spawn_event_loop(state.clone(), run.id, run.topic_id, vec![]);
    send_post_event(
        &tx,
        "worker.escalation",
        serde_json::json!({"worker": "w1"}),
    )
    .await
    .expect("post");
    let live = send_gate(&tx, "wake on any escalation")
        .await
        .expect("gate");
    assert_eq!(live["wake"], false);
    drop(tx);
    task.await.expect("first loop");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    // Resume: the same script steps replay against the journal. The post
    // finds its existing row (no duplicate event) and the gate returns
    // its recorded verdict without consulting the model again.
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let (tx, task) = spawn_event_loop(state, run.id, run.topic_id, replay);
    let replayed_post = send_post_event(
        &tx,
        "worker.escalation",
        serde_json::json!({"worker": "w1"}),
    )
    .await
    .expect("replayed post");
    assert_eq!(replayed_post["seq"], 1);
    let replayed_gate = send_gate(&tx, "wake on any escalation")
        .await
        .expect("replayed gate");
    assert_eq!(replayed_gate["wake"], false);
    drop(tx);
    task.await.expect("second loop");
    assert_eq!(
        storage
            .list_workflow_run_events(run.id)
            .await
            .expect("events")
            .len(),
        1,
        "the replayed post duplicated nothing"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "the replayed gate never re-consulted the model"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_timer_parks_after_a_completion_handoff() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::from([
            r#"{"wake": false, "reasoning": "completion is recorded"}"#,
        ])),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let park = CancellationToken::new();
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        run.topic_id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: None,
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: None,
        },
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        park.clone(),
        50,
        rx,
    ));
    let pause = Arc::new(completion_handoff_test_support::Pause {
        run_id: run.id,
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        observed_in_flight: AtomicU32::new(u32::MAX),
    });
    completion_handoff_test_support::install(Arc::clone(&pause));

    let (gate_reply, gate_receive) = oneshot::channel();
    tx.send(HostCommand::Gate {
        request: GateRequest {
            criteria: "record completion before a long wait".into(),
            model: None,
        },
        cancellation: CancellationToken::new(),
        reply: gate_reply,
    })
    .expect("gate");
    pause.entered.notified().await;
    assert_eq!(
        gate_receive
            .await
            .expect("gate reply")
            .expect("gate result")["wake"],
        false
    );

    let (timer_reply, mut timer_receive) = oneshot::channel();
    tx.send(HostCommand::Timer {
        ms: 100,
        timer_id: 0,
        cancellation: CancellationToken::new(),
        reply: timer_reply,
    })
    .expect("long timer after completion hand-off");
    let parked_signal =
        tokio::time::timeout(std::time::Duration::from_millis(250), park.cancelled()).await;
    pause.release.notify_one();
    completion_handoff_test_support::remove();
    assert_eq!(
        pause.observed_in_flight.load(Ordering::SeqCst),
        0,
        "the completion hand-off must publish idleness before the reply"
    );
    assert!(
        parked_signal.is_ok(),
        "the long timer must park while the completion hand-off is held"
    );

    let parked = storage
        .get_workflow_run(run.id)
        .await
        .expect("read parked run")
        .expect("run");
    assert_eq!(parked.state, "parked");
    assert!(parked.wake_at.is_some(), "parked run records its wake time");
    assert!(matches!(
        timer_receive.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));

    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_timer_with_nothing_in_flight_parks_the_run() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let park = CancellationToken::new();
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        run.topic_id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: None,
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: None,
        },
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        park.clone(),
        50,
        rx,
    ));
    let (reply, mut receive) = oneshot::channel();
    tx.send(HostCommand::Timer {
        ms: 100,
        timer_id: 0,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("timer");
    // The park decision is the signal: state recorded, engine told to halt.
    park.cancelled().await;
    let parked = storage
        .get_workflow_run(run.id)
        .await
        .expect("get")
        .expect("run");
    assert_eq!(parked.state, "parked");
    assert!(
        parked.wake_at.is_some(),
        "a parked run records when it is due"
    );
    // The timer reply is held, never resolved: the halting engine must
    // not see a racing error it could catch.
    assert!(matches!(
        receive.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    drop(tx);
    task.await.expect("host loop");
}

/// A provider that holds its gate evaluation open, so the run has a
/// genuinely in-flight call while the test sends its next command.
struct BlockingGateProvider {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for BlockingGateProvider {
    fn model_id(&self) -> &str {
        "blocking-gate-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(swage_core::CompletionResponse {
            content: r#"{"wake":true,"reasoning":"released"}"#.into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: swage_core::FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("gate evaluations use complete")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("gate evaluations use complete")
    }
}

fn decision_hold(content: &str) -> HoldRequest {
    HoldRequest {
        content: content.into(),
        verdicts: vec!["ship".into(), "hold".into()],
    }
}

async fn wait_for_open_hold(storage: &Arc<SqliteStorage>, run_id: Uuid) -> Uuid {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let holds = storage
                .list_workflow_run_holds(run_id)
                .await
                .expect("list holds");
            if let Some(hold) = holds.first() {
                return hold.id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the hold is opened")
}

async fn answer_open_hold(state: &AppState, hold_id: Uuid) {
    answer_run_hold(
        state,
        AnswerWorkflowRunHoldRequest {
            hold_id,
            verdict: "ship".into(),
        },
        None,
        RunEventSource {
            surface: "operator",
            agent_id: None,
            session_id: None,
            turn_id: None,
        },
    )
    .await
    .expect("answer the open hold");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_raised_beside_an_armed_timer_refuses_the_park_and_keeps_its_engine() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let (tx, task) = spawn_event_loop(state.clone(), run.id, run.topic_id, vec![]);

    // Short of the park threshold, so this timer arms in process and is
    // still armed when the hold asks to park.
    let (timer_reply, _timer_receive) = oneshot::channel();
    tx.send(HostCommand::Timer {
        ms: 30_000,
        timer_id: 0,
        cancellation: CancellationToken::new(),
        reply: timer_reply,
    })
    .expect("arm a sibling timer");

    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Hold {
        request: decision_hold("ship it, or hold it?"),
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("raise the hold");

    let hold_id = wait_for_open_hold(&storage, run.id).await;
    let waiting = storage
        .get_workflow_run(run.id)
        .await
        .expect("read run")
        .expect("run");
    assert_eq!(
        waiting.state, "running",
        "an armed sibling timer refuses the durable park: parking would kill it unfired"
    );
    assert!(waiting.wake_at.is_none());

    // The engine stayed up, so the in-process watcher takes the answer.
    answer_open_hold(&state, hold_id).await;
    let answer = tokio::time::timeout(std::time::Duration::from_secs(5), receive)
        .await
        .expect("the refusing engine still watches its hold")
        .expect("hold reply")
        .expect("verdict");
    assert_eq!(answer["verdict"], "ship");

    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_queued_before_a_timer_keeps_the_engine_until_the_timer_fires() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let (tx, host) = queued_event_loop(state.clone(), run.id, run.topic_id, vec![]);

    let (hold_reply, hold_receive) = oneshot::channel();
    tx.send(HostCommand::Hold {
        request: decision_hold("ship it, or hold it?"),
        cancellation: CancellationToken::new(),
        reply: hold_reply,
    })
    .expect("queue the hold first");
    let (timer_reply, timer_receive) = oneshot::channel();
    tx.send(HostCommand::Timer {
        ms: 10,
        timer_id: 0,
        cancellation: CancellationToken::new(),
        reply: timer_reply,
    })
    .expect("queue the sibling timer second");

    // Start the consumer only once both messages are queued. A concurrently
    // running consumer could legitimately park before the timer is sent.
    let task = tokio::spawn(host);
    let fired = tokio::time::timeout(std::time::Duration::from_secs(2), timer_receive)
        .await
        .expect("the queued sibling timer fires without a replay")
        .expect("timer reply")
        .expect("timer result");
    assert_eq!(fired, serde_json::json!({"fired": true}));
    let (confirm_reply, confirm_receive) = oneshot::channel();
    tx.send(HostCommand::ConfirmTimer {
        timer_id: 0,
        reply: confirm_reply,
    })
    .expect("confirm the winning timer");
    confirm_receive
        .await
        .expect("timer confirmation reply")
        .expect("confirm timer");

    let hold_id = wait_for_open_hold(&storage, run.id).await;
    let waiting = storage
        .get_workflow_run(run.id)
        .await
        .expect("read run")
        .expect("run");
    assert_eq!(waiting.state, "running", "queued work refused the park");
    assert!(waiting.wake_at.is_none());

    answer_open_hold(&state, hold_id).await;
    let answer = hold_receive
        .await
        .expect("hold reply")
        .expect("hold answer");
    assert_eq!(answer["verdict"], "ship");
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_arriving_during_hold_parking_is_not_dispatched() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::from([
            r#"{"wake":true,"reasoning":"late work was dispatched"}"#,
        ])),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(Arc::clone(&provider)).await;
    let park = CancellationToken::new();
    let (tx, rx) = mpsc::unbounded_channel();
    let admission = Arc::new(StdMutex::new(HostCommandAdmission::default()));
    let spent_tokens = Arc::new(AtomicU64::new(0));
    let host = Arc::new(ChannelWorkflowHost {
        tx,
        admission: Arc::clone(&admission),
        spent_tokens: Arc::clone(&spent_tokens),
        max_total_tokens: None,
    });
    let task = tokio::spawn(run_host_commands_with_budget(
        state,
        run.id,
        run.topic_id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget::default(),
        vec![],
        Arc::new(AtomicU32::new(0)),
        spent_tokens,
        Arc::new(Mutex::new(RouteTokenBudgetState::default())),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        park.clone(),
        PARK_TIMER_THRESHOLD_MS,
        admission,
        rx,
    ));

    host.post_run_event(
        RunEventPost {
            kind: "worker.completion".into(),
            payload: serde_json::json!({"worker": "w1"}),
        },
        CancellationToken::new(),
    )
    .await
    .expect("post an event for the late gate");

    let pause = workflow_pause_test_hook::install(PAUSE_BEFORE_HOLD_PARK, run.id);
    let hold_host = Arc::clone(&host);
    let hold_task = tokio::spawn(async move {
        hold_host
            .raise_hold(
                decision_hold("ship it, or hold it?"),
                CancellationToken::new(),
            )
            .await
    });
    pause
        .entered
        .acquire()
        .await
        .expect("park hook remains open")
        .forget();

    let gate_host = Arc::clone(&host);
    let late_gate = tokio::spawn(async move {
        gate_host
            .evaluate_wake_gate(
                GateRequest {
                    criteria: "wake when the worker has settled".into(),
                    model: None,
                },
                CancellationToken::new(),
            )
            .await
    });
    tokio::task::yield_now().await;
    pause.release.add_permits(1);
    park.cancelled().await;

    let error = late_gate
        .await
        .expect("late gate task")
        .expect_err("a command admitted after parking starts must be refused");
    assert!(error.contains("parked"), "unexpected refusal: {error}");

    // Held-worker commands are admitted on the same terms as any other:
    // a run that has parked answers none of them.
    let opened = host
        .open_held_worker(
            HeldWorkerOpen {
                agent: Some("decider".into()),
                ..HeldWorkerOpen::default()
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("opening a worker on a parked run must be refused");
    assert!(opened.contains("parked"), "unexpected refusal: {opened}");
    let closed = host
        .close_held_worker(Uuid::new_v4(), CancellationToken::new())
        .await
        .expect_err("closing a worker on a parked run must be refused");
    assert!(closed.contains("parked"), "unexpected refusal: {closed}");
    let staleness = host
        .held_worker_staleness(Uuid::new_v4(), CancellationToken::new())
        .await
        .expect_err("reading worker staleness on a parked run must be refused");
    assert!(
        staleness.contains("parked"),
        "unexpected refusal: {staleness}"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "late work must never reach the provider"
    );
    assert_eq!(
        storage
            .get_workflow_run(run.id)
            .await
            .expect("read run")
            .expect("run")
            .state,
        "parked"
    );

    hold_task.abort();
    drop(host);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_does_not_park_over_commands_a_failed_park_handed_back() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::from([
            r#"{"wake":true,"reasoning":"the gate ran on a live run"}"#,
        ])),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(Arc::clone(&provider)).await;
    let park = CancellationToken::new();
    let (tx, rx) = mpsc::unbounded_channel();
    let admission = Arc::new(StdMutex::new(HostCommandAdmission::default()));
    let spent_tokens = Arc::new(AtomicU64::new(0));
    let host = Arc::new(ChannelWorkflowHost {
        tx,
        admission: Arc::clone(&admission),
        spent_tokens: Arc::clone(&spent_tokens),
        max_total_tokens: None,
    });
    let task = tokio::spawn(run_host_commands_with_budget(
        state.clone(),
        run.id,
        run.topic_id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget::default(),
        vec![],
        Arc::new(AtomicU32::new(0)),
        spent_tokens,
        Arc::new(Mutex::new(RouteTokenBudgetState::default())),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        park.clone(),
        PARK_TIMER_THRESHOLD_MS,
        admission,
        rx,
    ));

    // An undelivered event, so the late gate has something to judge and
    // reaches the provider rather than answering itself.
    host.post_run_event(
        RunEventPost {
            kind: "worker.completion".into(),
            payload: serde_json::json!({"worker": "w1"}),
        },
        CancellationToken::new(),
    )
    .await
    .expect("post an event for the late gate");

    // The first hold opens and closes admission for its park.
    let pause = workflow_pause_test_hook::install(PAUSE_BEFORE_HOLD_PARK, run.id);
    let first_host = Arc::clone(&host);
    let first_hold = tokio::spawn(async move {
        first_host
            .raise_hold(decision_hold("ship the first?"), CancellationToken::new())
            .await
    });
    pause
        .entered
        .acquire()
        .await
        .expect("park hook remains open")
        .forget();

    // A second hold and then a gate arrive while the first is parking.
    // Each is deferred at its send, so their order is fixed here: the
    // hold is handed back to the loop ahead of the gate.
    let mut second_hold =
        Box::pin(host.raise_hold(decision_hold("ship the second?"), CancellationToken::new()));
    assert!(futures::poll!(second_hold.as_mut()).is_pending());
    let mut late_gate = Box::pin(host.evaluate_wake_gate(
        GateRequest {
            criteria: "wake when the worker has settled".into(),
            model: None,
        },
        CancellationToken::new(),
    ));
    assert!(futures::poll!(late_gate.as_mut()).is_pending());

    // The first hold is answered before its park lands, so the park's
    // storage guard refuses it and both deferred commands come back to
    // the loop as queued work.
    let first_id = wait_for_open_hold(&storage, run.id).await;
    answer_open_hold(&state, first_id).await;
    pause.release.add_permits(1);
    let first = first_hold
        .await
        .expect("first hold task")
        .expect("the answered first hold reaches the script");
    assert_eq!(first["verdict"], "ship");

    // The second hold is processed with the gate still queued behind it:
    // it must not park, so the gate runs on a live run.
    late_gate
        .await
        .expect("a command a failed park handed back runs on the live run");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        storage
            .get_workflow_run(run.id)
            .await
            .expect("read run")
            .expect("run")
            .state,
        "running",
        "a hold must not park over a command the failed park handed back"
    );
    assert!(!park.is_cancelled());

    let second_id = storage
        .list_workflow_run_holds(run.id)
        .await
        .expect("list holds")
        .into_iter()
        .find(|hold| hold.state == "open")
        .expect("the second hold is still open")
        .id;
    answer_open_hold(&state, second_id).await;
    let second = second_hold
        .await
        .expect("the watched second hold reaches the script");
    assert_eq!(second["verdict"], "ship");

    drop(host);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_watcher_ends_when_a_guard_refused_wait_is_cancelled() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let (tx, task) = spawn_event_loop(state, run.id, run.topic_id, vec![]);

    let (timer_reply, _timer_receive) = oneshot::channel();
    tx.send(HostCommand::Timer {
        ms: 30_000,
        timer_id: 0,
        cancellation: CancellationToken::new(),
        reply: timer_reply,
    })
    .expect("arm a sibling timer");

    let cancellation = CancellationToken::new();
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Hold {
        request: decision_hold("ship it, or hold it?"),
        cancellation: cancellation.clone(),
        reply,
    })
    .expect("raise the hold");

    wait_for_open_hold(&storage, run.id).await;
    let waiting = storage
        .get_workflow_run(run.id)
        .await
        .expect("read run")
        .expect("run");
    assert_eq!(waiting.state, "running", "the timer refused the park");

    cancellation.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), receive)
        .await
        .expect("the in-process watcher ends on cancellation")
        .expect("hold reply")
        .expect_err("a cancelled hold does not produce a verdict");
    assert!(
        error.contains("WorkflowCancelledError"),
        "the watcher reports cancellation: {error}"
    );

    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_raised_beside_an_in_flight_call_refuses_the_park_and_keeps_its_engine() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(BlockingGateProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let (tx, task) = spawn_event_loop(state.clone(), run.id, run.topic_id, vec![]);

    send_post_event(
        &tx,
        "worker.completion",
        serde_json::json!({"worker": "w1"}),
    )
    .await
    .expect("post the event the gate reads");
    let (gate_reply, gate_receive) = oneshot::channel();
    tx.send(HostCommand::Gate {
        request: GateRequest {
            criteria: "wake when the worker has settled".into(),
            model: None,
        },
        cancellation: CancellationToken::new(),
        reply: gate_reply,
    })
    .expect("start a gate evaluation");
    // The evaluation is now inside the provider: the call is in flight.
    entered.notified().await;

    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Hold {
        request: decision_hold("ship it, or hold it?"),
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("raise the hold");

    let hold_id = wait_for_open_hold(&storage, run.id).await;
    let waiting = storage
        .get_workflow_run(run.id)
        .await
        .expect("read run")
        .expect("run");
    assert_eq!(
        waiting.state, "running",
        "an in-flight call refuses the durable park: wake replay would run it twice"
    );
    assert!(waiting.wake_at.is_none());

    answer_open_hold(&state, hold_id).await;
    let answer = tokio::time::timeout(std::time::Duration::from_secs(5), receive)
        .await
        .expect("the refusing engine still watches its hold")
        .expect("hold reply")
        .expect("verdict");
    assert_eq!(answer["verdict"], "ship");

    release.notify_one();
    assert_eq!(
        gate_receive
            .await
            .expect("gate reply")
            .expect("gate result")["wake"],
        true
    );
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_wait_beside_an_in_flight_call_refuses_the_park_and_takes_the_post_in_process() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(BlockingGateProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let (tx, task) = spawn_event_loop(state.clone(), run.id, run.topic_id, vec![]);

    send_post_event(
        &tx,
        "worker.completion",
        serde_json::json!({"worker": "w1"}),
    )
    .await
    .expect("post the event the gate reads");
    let (gate_reply, gate_receive) = oneshot::channel();
    tx.send(HostCommand::Gate {
        request: GateRequest {
            criteria: "wake when the worker has settled".into(),
            model: None,
        },
        cancellation: CancellationToken::new(),
        reply: gate_reply,
    })
    .expect("start a gate evaluation");
    // The evaluation is now inside the provider: the call is in flight.
    entered.notified().await;

    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::AwaitEvent {
        request: AwaitEventRequest {
            kinds: vec!["operator.decision".into()],
        },
        reply,
    })
    .expect("wait for a decision");
    // The wait has been considered: a later post is answered by the
    // in-process watcher, so ask the loop something and read the run.
    send_post_event(&tx, "operator.note", serde_json::json!({}))
        .await
        .expect("the loop is still serving");
    let waiting = storage
        .get_workflow_run(run.id)
        .await
        .expect("read run")
        .expect("run");
    assert_eq!(
        waiting.state, "running",
        "an in-flight call refuses the durable park: wake replay would run it twice"
    );
    assert!(waiting.wake_at.is_none());

    send_post_event(
        &tx,
        "operator.decision",
        serde_json::json!({"verdict": "accept"}),
    )
    .await
    .expect("post the awaited event");
    let answer = tokio::time::timeout(std::time::Duration::from_secs(5), receive)
        .await
        .expect("the refusing engine still watches for the post")
        .expect("await reply")
        .expect("event");
    assert_eq!(answer["payload"]["verdict"], "accept");

    release.notify_one();
    assert_eq!(
        gate_receive
            .await
            .expect("gate reply")
            .expect("gate result")["wake"],
        true
    );
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parked_run_releases_its_workflow_thread() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;

    launch_existing_run(
        state,
        run.clone(),
        "return await wait({ timerMs: 120_000 });".into(),
        None,
    )
    .await
    .expect("launch parked run");

    let parked = wait_for_run_state(&storage, run.id, "parked").await;
    assert!(parked.wake_at.is_some(), "parked run records its wake time");
    wait_for_workflow_thread_exit(run.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parked_run_survives_its_terminal_completion_task() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;

    launch_existing_run(
        state.clone(),
        run.clone(),
        "return await wait({ timerMs: 120_000 });".into(),
        None,
    )
    .await
    .expect("launch parked run");

    let parked = wait_for_run_state(&storage, run.id, "parked").await;
    let wake_at = parked.wake_at.expect("parked run records its wake time");
    wait_for_workflow_thread_exit(run.id).await;
    wait_for_active_workflow_run_release(&state, run.id).await;

    let after_terminal_task = storage
        .get_workflow_run(run.id)
        .await
        .expect("get parked run after terminal task")
        .expect("parked run after terminal task");
    assert_eq!(after_terminal_task.state, "parked");
    assert_eq!(after_terminal_task.wake_at, Some(wake_at));
}

#[tokio::test]
async fn completed_generation_does_not_remove_its_replacement_token() {
    let run_id = Uuid::new_v4();
    let completed = CancellationToken::new();
    let replacement = CancellationToken::new();
    let active = Mutex::new(std::collections::HashMap::from([(
        run_id,
        replacement.clone(),
    )]));

    remove_active_workflow_generation(&active, run_id, &completed).await;

    assert_eq!(active.lock().await.get(&run_id), Some(&replacement));
}

#[tokio::test(flavor = "multi_thread")]
async fn external_post_relaunch_keeps_the_replacement_cancellation_token() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    let source = r#"
        const answer = awaitEvent({ kinds: ["operator.decision"] });
        const resolved = await answer;
        await wait({ timerMs: 5_000 });
        return resolved.payload;
    "#;
    let cleanup_pause = workflow_cleanup_test_hook::install(run.id);
    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("start awaiting run");
    cleanup_pause
        .entered
        .acquire()
        .await
        .expect("workflow cleanup test hook remains open")
        .forget();
    let original = state
        .active_workflow_runs
        .lock()
        .await
        .get(&run.id)
        .cloned()
        .expect("the original generation remains installed while cleanup is paused");

    let Json(posted) = Box::pin(post_operator_run_event(
        State(state.clone()),
        Path(run.id),
        Json(PostWorkflowRunEventRequest {
            kind: "operator.decision".into(),
            payload: serde_json::json!({"verdict": "accept"}),
        }),
    ))
    .await
    .expect("post event and relaunch while the parked generation still owns its token");
    assert!(posted.wake_provisioned);

    let replacement = state
        .active_workflow_runs
        .lock()
        .await
        .get(&run.id)
        .cloned()
        .expect("the external post installs a replacement generation");
    assert_ne!(replacement, original);

    cleanup_pause.release.add_permits(1);
    cleanup_pause
        .finished
        .acquire()
        .await
        .expect("workflow cleanup test hook remains open")
        .forget();
    assert!(
        !cleanup_pause.removed_generation(),
        "the original generation must not remove the relaunched run's token"
    );

    replacement.cancel();
    wait_for_run_state_in(&storage, run.id, &["cancelled", "completed"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_completed_run_releases_its_workflow_thread() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;

    launch_existing_run(
        state.clone(),
        run.clone(),
        "return { done: true };".into(),
        None,
    )
    .await
    .expect("launch completed run");

    wait_for_run_state(&storage, run.id, "completed").await;
    wait_for_workflow_thread_exit(run.id).await;
    wait_for_active_workflow_run_release(&state, run.id).await;
    assert!(
        !state
            .active_workflow_runs
            .lock()
            .await
            .contains_key(&run.id),
        "completed run must release its active-run registration"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_woken_run_replays_its_timer_with_the_remaining_time() {
    let provider = Arc::new(ScriptedGateProvider {
        replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
        calls: AtomicUsize::new(0),
    });
    let (storage, state, run) = event_gate_fixture(provider).await;
    // A started timer entry whose recorded start is already further back
    // than the timer's length — how a woken parked run's journal looks.
    let request = serde_json::json!({ "ms": 80u64 });
    storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "timer".into(),
            args_hash: journal_args_hash(&request),
            request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("intent");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let (tx, task) = spawn_event_loop(state, run.id, run.topic_id, replay);
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Timer {
        ms: 80,
        timer_id: 0,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("timer");
    // The remainder is exhausted, so the timer owes an immediate firing —
    // not a fresh 80ms, and above all not a re-park.
    let fired = tokio::time::timeout(std::time::Duration::from_secs(5), receive)
        .await
        .expect("timer fired promptly")
        .expect("reply")
        .expect("fired");
    assert_eq!(fired, serde_json::json!({"fired": true}));
    let (confirm_reply, confirm_receive) = oneshot::channel();
    tx.send(HostCommand::ConfirmTimer {
        timer_id: 0,
        reply: confirm_reply,
    })
    .expect("confirm");
    confirm_receive
        .await
        .expect("confirm reply")
        .expect("confirmed");
    drop(tx);
    task.await.expect("host loop");
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal after");
    assert_eq!(journal[0].status, "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wake_pass_resumes_interrupted_and_due_parked_runs() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let make_run = |source: &str| NewWorkflowRun {
        source: Some(source.into()),
        source_hash: source_hash(source),
        ..swage_storage::fixtures::new_workflow_run(topic.id)
    };
    let interrupted = storage
        .create_workflow_run(make_run(r#"return "a-done""#))
        .await
        .expect("run a");
    storage
        .update_workflow_run_state(interrupted.id, "interrupted", None, None, None)
        .await
        .expect("interrupt");
    let due = storage
        .create_workflow_run(make_run(r#"return "b-woke""#))
        .await
        .expect("run b");
    storage
        .set_workflow_run_parked(due.id, chrono::Utc::now() - chrono::Duration::seconds(1))
        .await
        .expect("park due");
    let sleeping = storage
        .create_workflow_run(make_run(r#"return "c-later""#))
        .await
        .expect("run c");
    storage
        .set_workflow_run_parked(sleeping.id, chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .expect("park future");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::PendingProvider),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let resumed = resume_owed_workflow_runs(&state).await;
    assert_eq!(resumed, 2, "the interrupted run and the due parked run");
    wait_for_terminal(&state, interrupted.id).await;
    wait_for_terminal(&state, due.id).await;
    let finished = storage
        .get_workflow_run(interrupted.id)
        .await
        .expect("get a")
        .expect("run a");
    assert_eq!(finished.state, "completed");
    assert_eq!(finished.final_output, Some(Value::String("a-done".into())));
    let woken = storage
        .get_workflow_run(due.id)
        .await
        .expect("get b")
        .expect("run b");
    assert_eq!(woken.state, "completed");
    assert_eq!(woken.final_output, Some(Value::String("b-woke".into())));
    assert!(
        woken.wake_at.is_none(),
        "leaving parked clears the due time"
    );
    let still_parked = storage
        .get_workflow_run(sleeping.id)
        .await
        .expect("get c")
        .expect("run c");
    assert_eq!(still_parked.state, "parked", "not due, not touched");
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_recovery_redrives_a_continuation_after_its_worker_turn_was_orphaned() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"
        const worker = agentStart("restate", { agent: "worker" });
        await worker.result;
        return await agent("continue", { agent: "worker", session: worker.session });
    "#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let session = storage
        .create_session(swage_storage::NewSession {
            agent_id: "worker".into(),
            metadata: Some(serde_json::json!({})),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("worker session");
    let thread = swage_storage::store::ThreadStore::create(
        storage.as_ref(),
        swage_storage::NewThread {
            session_id: session.id,
            agent_id: "worker".into(),
        },
    )
    .await
    .expect("worker thread");
    storage
        .update_state(thread.id, ThreadState::Processing)
        .await
        .expect("persist processing worker thread");
    let orphan = storage
        .create_turn(swage_storage::NewTurn {
            thread_id: thread.id,
            agent_id: "worker".into(),
            state: TurnState::Processing,
            user_input: Some("continue".into()),
            parent_turn_id: None,
        })
        .await
        .expect("orphaned worker turn");
    let first_request = AgentCallRequest {
        agent: Some("worker".into()),
        max_attempts: 3,
        ..swage_workflow::fixtures::agent_call_request("restate".into())
    };
    let first_json = serde_json::to_value(&first_request).expect("first request");
    let first = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&first_json),
            request: first_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("first journal intent");
    storage
        .complete_workflow_journal_entry(
            first.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("restated".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 1,
                    output_tokens: 1,
                },
                working_directory: None,
                served_model: None,
                provider_route: None,
                session_id: Some(session.id),
                thread_id: Some(thread.id),
                turn_id: Some(Uuid::new_v4()),
                worktree: None,
            })
            .expect("first result"),
        )
        .await
        .expect("complete first call");
    let continuation = AgentCallRequest {
        agent: Some("worker".into()),
        session: Some(AgentSessionRef {
            session_id: session.id,
            thread_id: thread.id,
        }),
        max_attempts: 3,
        ..swage_workflow::fixtures::agent_call_request("continue".into())
    };
    let continuation_json = serde_json::to_value(&continuation).expect("continuation request");
    storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&continuation_json),
            request: continuation_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("started continuation");

    assert_eq!(
        crate::shutdown::recover_orphaned_turns(storage.as_ref(), storage.as_ref())
            .await
            .expect("recover turns"),
        1
    );
    assert_eq!(
        storage.get_turn(orphan.id).await.unwrap().unwrap().state,
        TurnState::Interrupted
    );
    assert_eq!(
        ThreadStore::get(storage.as_ref(), thread.id)
            .await
            .expect("read worker thread")
            .expect("worker thread")
            .state,
        ThreadState::Idle
    );
    assert_eq!(storage.interrupt_running_workflow_runs().await.unwrap(), 1);

    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "continued after recovery",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        provider.clone(),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    assert_eq!(resume_owed_workflow_runs(&state).await, 1);
    wait_for_run_state(&storage, run.id, "completed").await;
    let recovered = storage.get_workflow_run(run.id).await.unwrap().unwrap();
    assert_eq!(
        recovered.final_output,
        Some(Value::String("continued after recovery".into()))
    );
    assert!(
        !provider.requests.lock().expect("requests").is_empty(),
        "replayed worker call should reach the provider"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_recovery_keeps_turn_non_terminal_when_worker_thread_release_fails() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let session = storage
        .create_session(NewSession {
            agent_id: "worker".into(),
            metadata: Some(serde_json::json!({})),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .expect("worker session");
    let thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: "worker".into(),
        },
    )
    .await
    .expect("worker thread");
    storage
        .update_state(thread.id, ThreadState::Processing)
        .await
        .expect("persist processing worker thread");
    let orphan = storage
        .create_turn(swage_storage::NewTurn {
            thread_id: thread.id,
            agent_id: "worker".into(),
            state: TurnState::Processing,
            user_input: Some("continue".into()),
            parent_turn_id: None,
        })
        .await
        .expect("orphaned worker turn");

    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TEMP TRIGGER reject_orphaned_worker_thread_release
                 BEFORE UPDATE OF state ON threads
                 WHEN NEW.id = OLD.id AND NEW.state = 'idle'
                 BEGIN SELECT RAISE(FAIL, 'injected worker thread release failure'); END;",
            )
        })
        .await
        .expect("install thread release failure trigger");

    let error = crate::shutdown::recover_orphaned_turns(storage.as_ref(), storage.as_ref())
        .await
        .expect_err("thread release failure aborts recovery");
    assert!(
        error
            .to_string()
            .contains("injected worker thread release failure")
    );
    assert_eq!(
        storage.get_turn(orphan.id).await.unwrap().unwrap().state,
        TurnState::Processing,
        "a failed worker release must leave the turn recoverable on the next startup"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn token_reservation_settlement_releases_estimate_and_retains_actual_spend() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let pending_call_started = Arc::new(Notify::new());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(SettlementProvider {
            calls: AtomicUsize::new(0),
            pending_call_started: Arc::clone(&pending_call_started),
        }),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let spent = Arc::new(AtomicU64::new(0));
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: Some(1_000_000),
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: Some(2),
        },
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::clone(&spent),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let request = AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request("account usage".into())
    };
    let (first_reply, first_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request.clone(),
        cancellation: CancellationToken::new(),
        reply: first_reply,
    })
    .expect("settling call");
    let settled = first_receive
        .await
        .expect("first reply")
        .expect("first call");
    assert_eq!(settled.usage.known_total(), Some(400_000));
    assert_eq!(spent.load(Ordering::SeqCst), 400_000);

    let pending_cancellation = CancellationToken::new();
    let (second_reply, second_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request.clone(),
        cancellation: pending_cancellation.clone(),
        reply: second_reply,
    })
    .expect("call admitted after settlement");
    pending_call_started.notified().await;
    let (third_reply, third_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request,
        cancellation: CancellationToken::new(),
        reply: third_reply,
    })
    .expect("call competing with retained spend");
    assert_eq!(
        third_receive
            .await
            .expect("third reply")
            .expect_err("refused")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    pending_cancellation.cancel();
    assert!(second_receive.await.expect("second reply").is_err());
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn in_flight_working_reservations_do_not_consume_the_closing_partition() {
    let scaffold = WorkingBudgetScaffold::new(2).await;
    let (tx, pending_call_started, task) = scaffold.spawn(
        WorkflowBudget {
            max_agent_calls: Some(3),
            per_route: Default::default(),
            max_total_tokens: Some(6),
            reserved_closing_tokens: Some(2),
            max_concurrent_agent_calls: Some(4),
        },
        vec![],
        0,
        2,
    );
    let request = |prompt: &str| AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    };
    let first_cancellation = CancellationToken::new();
    let (first_reply, first_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("working one"),
        cancellation: first_cancellation.clone(),
        reply: first_reply,
    })
    .expect("first working call");
    pending_call_started.notified().await;
    let second_cancellation = CancellationToken::new();
    let (second_reply, second_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("working two"),
        cancellation: second_cancellation.clone(),
        reply: second_reply,
    })
    .expect("second working call");
    pending_call_started.notified().await;

    let (transition_reply, transition_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("cross working boundary"),
        cancellation: CancellationToken::new(),
        reply: transition_reply,
    })
    .expect("wind-down transition");
    assert_eq!(
        transition_receive
            .await
            .expect("transition reply")
            .expect_err("working allowance exhausted")
            .to_string(),
        "BudgetWindDownError: working token allowance exhausted"
    );

    let (closing_reply, closing_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("closing report"),
        cancellation: CancellationToken::new(),
        reply: closing_reply,
    })
    .expect("closing call");
    assert_eq!(
        closing_receive
            .await
            .expect("closing reply")
            .expect("closing capacity remains")
            .output,
        Value::String("closing report".into())
    );

    first_cancellation.cancel();
    second_cancellation.cancel();
    assert!(first_receive.await.expect("first working reply").is_err());
    assert!(second_receive.await.expect("second working reply").is_err());
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn replayed_in_flight_working_reservations_leave_the_closing_partition_available() {
    let scaffold = WorkingBudgetScaffold::new(2).await;
    let storage = Arc::clone(&scaffold.storage);
    let run_id = scaffold.run_id;
    let request = |prompt: &str| AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    };
    for (seq, prompt) in [(0, "working one"), (1, "working two")] {
        let request_json = serde_json::to_value(request(prompt)).expect("working request");
        storage
            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                run_id,
                seq,
                kind: "agent_call".into(),
                args_hash: journal_args_hash(&request_json),
                request: request_json,
                effect_kind: None,
                idempotency_key: None,
                target_kind: None,
            })
            .await
            .expect("started working call");
    }
    let transition_request = request("cross working boundary");
    let transition_json = serde_json::to_value(&transition_request).expect("transition request");
    let transition = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id,
            seq: 2,
            kind: "wind_down".into(),
            args_hash: journal_args_hash(&transition_json),
            request: transition_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("wind-down intent");
    storage
        .complete_workflow_journal_entry(
            transition.id,
            serde_json::json!({
                "spent": 0,
                "remaining": 0,
                "working_remaining": 0,
                "closing_remaining": 2,
                "winding_down": true,
            }),
        )
        .await
        .expect("wind-down transition");
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal");
    let (tx, pending_call_started, task) = scaffold.spawn(
        WorkflowBudget {
            max_agent_calls: Some(3),
            per_route: Default::default(),
            max_total_tokens: Some(6),
            reserved_closing_tokens: Some(2),
            max_concurrent_agent_calls: Some(4),
        },
        replay,
        2,
        0,
    );
    let first_cancellation = CancellationToken::new();
    let (first_reply, first_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("working one"),
        cancellation: first_cancellation.clone(),
        reply: first_reply,
    })
    .expect("first replayed working call");
    pending_call_started.notified().await;
    let second_cancellation = CancellationToken::new();
    let (second_reply, second_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("working two"),
        cancellation: second_cancellation.clone(),
        reply: second_reply,
    })
    .expect("second replayed working call");
    pending_call_started.notified().await;

    let (transition_reply, transition_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: transition_request,
        cancellation: CancellationToken::new(),
        reply: transition_reply,
    })
    .expect("replay wind-down transition");
    assert_eq!(
        transition_receive
            .await
            .expect("transition reply")
            .expect_err("recorded wind-down signal")
            .to_string(),
        "BudgetWindDownError: working token allowance exhausted"
    );

    let (closing_reply, closing_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("closing report"),
        cancellation: CancellationToken::new(),
        reply: closing_reply,
    })
    .expect("closing call");
    assert_eq!(
        closing_receive
            .await
            .expect("closing reply")
            .expect("closing capacity remains")
            .output,
        Value::String("closing report".into())
    );

    first_cancellation.cancel();
    second_cancellation.cancel();
    assert!(first_receive.await.expect("first working reply").is_err());
    assert!(second_receive.await.expect("second working reply").is_err());
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn replayed_started_calls_hold_token_reservations_until_settlement() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request("resume work".into())
    };
    let request_json = serde_json::to_value(&request).expect("request");
    for seq in 0..2 {
        storage
            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                run_id: run.id,
                seq,
                kind: "agent_call".into(),
                args_hash: journal_args_hash(&request_json),
                request: request_json.clone(),
                effect_kind: None,
                idempotency_key: None,
                target_kind: None,
            })
            .await
            .expect("started call");
    }
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::PendingProvider),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: None,
            per_route: Default::default(),
            max_total_tokens: Some(1_000_000),
            reserved_closing_tokens: None,
            max_concurrent_agent_calls: Some(2),
        },
        replay,
        Arc::new(AtomicU32::new(2)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let first_cancellation = CancellationToken::new();
    let (first_reply, first_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request.clone(),
        cancellation: first_cancellation.clone(),
        reply: first_reply,
    })
    .expect("first replay");
    let second_cancellation = CancellationToken::new();
    let (second_reply, second_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request.clone(),
        cancellation: second_cancellation.clone(),
        reply: second_reply,
    })
    .expect("second replay");
    let (third_reply, third_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request,
        cancellation: CancellationToken::new(),
        reply: third_reply,
    })
    .expect("new call");
    assert_eq!(
        third_receive
            .await
            .expect("third reply")
            .expect_err("refused")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    first_cancellation.cancel();
    second_cancellation.cancel();
    assert!(first_receive.await.expect("first reply").is_err());
    assert!(second_receive.await.expect("second reply").is_err());
    drop(tx);
    task.await.expect("host loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_latches_wind_down_only_after_a_started_working_call_settles() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let working_request = AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request("resume working call".into())
    };
    let working_json = serde_json::to_value(&working_request).expect("working request");
    storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&working_json),
            request: working_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("started working call");
    let snapshot_request = serde_json::json!({ "budget": "snapshot" });
    let snapshot = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "budget".into(),
            args_hash: journal_args_hash(&snapshot_request),
            request: snapshot_request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("snapshot intent");
    storage
        .complete_workflow_journal_entry(
            snapshot.id,
            serde_json::json!({
                "spent": 0,
                "remaining": 4,
                "working_remaining": 0,
                "closing_remaining": 2,
                "winding_down": true,
            }),
        )
        .await
        .expect("snapshot transition");
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output("resumed work"),
            scripted_text_output("closing report"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: Some(3),
            per_route: Default::default(),
            max_total_tokens: Some(6),
            reserved_closing_tokens: Some(2),
            max_concurrent_agent_calls: Some(1),
        },
        replay,
        Arc::new(AtomicU32::new(1)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let (work_reply, work_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: working_request,
        cancellation: CancellationToken::new(),
        reply: work_reply,
    })
    .expect("replay working call");
    assert_eq!(
        work_receive
            .await
            .expect("working reply")
            .expect("working result")
            .output,
        Value::String("resumed work".into())
    );
    let (snapshot_reply, snapshot_receive) = oneshot::channel();
    tx.send(HostCommand::BudgetSnapshot {
        reply: snapshot_reply,
    })
    .expect("replay snapshot");
    assert_eq!(
        snapshot_receive
            .await
            .expect("snapshot reply")
            .expect("snapshot result")["winding_down"],
        true
    );
    let closing_request = AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request("closing call".into())
    };
    let (closing_reply, closing_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: closing_request.clone(),
        cancellation: CancellationToken::new(),
        reply: closing_reply,
    })
    .expect("closing call");
    assert_eq!(
        closing_receive
            .await
            .expect("closing reply")
            .expect("closing result")
            .output,
        Value::String("closing report".into())
    );
    let (beyond_reply, beyond_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: closing_request,
        cancellation: CancellationToken::new(),
        reply: beyond_reply,
    })
    .expect("beyond closing call");
    assert_eq!(
        beyond_receive
            .await
            .expect("beyond reply")
            .expect_err("closing capacity exhausted")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host loop");
    assert_eq!(provider.requests.lock().expect("requests").len(), 2);
}

async fn assert_replayed_wind_down_transition(completed_transition: bool) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = |prompt: &str| AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    };
    let work_request = request("completed work");
    let work_json = serde_json::to_value(&work_request).expect("work request");
    let work = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&work_json),
            request: work_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("work intent");
    storage
        .complete_workflow_journal_entry(
            work.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("completed work".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 1,
                    output_tokens: 1,
                },
                working_directory: None,
                served_model: None,
                provider_route: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
                worktree: None,
            })
            .expect("work result"),
        )
        .await
        .expect("completed work");
    let transition_request = request("cross working boundary");
    let transition_json = serde_json::to_value(&transition_request).expect("transition request");
    let transition = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "wind_down".into(),
            args_hash: journal_args_hash(&transition_json),
            request: transition_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("wind-down intent");
    if completed_transition {
        storage
            .complete_workflow_journal_entry(
                transition.id,
                serde_json::json!({
                    // Model the raced durable reading: the completed work
                    // settled before the transition but was omitted here.
                    "spent": 0,
                    "remaining": 0,
                    "working_remaining": 0,
                    "closing_remaining": 2,
                    "winding_down": true,
                }),
            )
            .await
            .expect("completed wind-down transition");
    }
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "closing report",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: Some(3),
            per_route: Default::default(),
            max_total_tokens: Some(6),
            reserved_closing_tokens: Some(2),
            max_concurrent_agent_calls: Some(1),
        },
        replay,
        Arc::new(AtomicU32::new(1)),
        Arc::new(AtomicU64::new(2)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));

    let (work_reply, work_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: work_request,
        cancellation: CancellationToken::new(),
        reply: work_reply,
    })
    .expect("replay completed work");
    assert_eq!(
        work_receive
            .await
            .expect("work reply")
            .expect("recorded work")
            .output,
        Value::String("completed work".into())
    );
    let (transition_reply, transition_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: transition_request,
        cancellation: CancellationToken::new(),
        reply: transition_reply,
    })
    .expect("replay wind-down transition");
    assert_eq!(
        transition_receive
            .await
            .expect("transition reply")
            .expect_err("working partition exhausted")
            .to_string(),
        "BudgetWindDownError: working token allowance exhausted"
    );
    let closing_request = request("closing report");
    let (closing_reply, closing_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: closing_request.clone(),
        cancellation: CancellationToken::new(),
        reply: closing_reply,
    })
    .expect("closing call");
    assert_eq!(
        closing_receive
            .await
            .expect("closing reply")
            .expect("closing result")
            .output,
        Value::String("closing report".into())
    );
    let (beyond_reply, beyond_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: closing_request,
        cancellation: CancellationToken::new(),
        reply: beyond_reply,
    })
    .expect("past closing call");
    assert_eq!(
        beyond_receive
            .await
            .expect("past closing reply")
            .expect_err("closing partition exhausted")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host loop");

    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal after replay");
    let transitions = journal
        .iter()
        .filter(|entry| entry.kind == "wind_down")
        .collect::<Vec<_>>();
    assert_eq!(transitions.len(), 1, "transition completes in place");
    assert_eq!(transitions[0].status, "completed");
    if !completed_transition {
        assert_eq!(transitions[0].result.as_ref().unwrap()["spent"], 2);
    }
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_reconstructs_closing_limit_when_transition_reading_omits_settled_work() {
    assert_replayed_wind_down_transition(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_completes_a_started_wind_down_transition_once() {
    assert_replayed_wind_down_transition(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replayed_started_closing_call_keeps_its_reserved_partition() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = |prompt: &str| AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    };
    let work_request = request("completed work");
    let work_json = serde_json::to_value(&work_request).expect("work request");
    let work = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&work_json),
            request: work_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("work intent");
    storage
        .complete_workflow_journal_entry(
            work.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("completed work".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 1,
                    output_tokens: 1,
                },
                working_directory: None,
                served_model: None,
                provider_route: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
                worktree: None,
            })
            .expect("work result"),
        )
        .await
        .expect("completed work");
    let snapshot_request = serde_json::json!({ "budget": "snapshot" });
    let snapshot = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "budget".into(),
            args_hash: journal_args_hash(&snapshot_request),
            request: snapshot_request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("snapshot intent");
    storage
        .complete_workflow_journal_entry(
            snapshot.id,
            serde_json::json!({
                "spent": 2,
                "remaining": 2,
                "working_remaining": 0,
                "closing_remaining": 2,
                "winding_down": true,
            }),
        )
        .await
        .expect("snapshot transition");
    let closing_request = request("started closing call");
    let closing_json = serde_json::to_value(&closing_request).expect("closing request");
    storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 2,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&closing_json),
            request: closing_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("started closing call");
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "resumed closing report",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget {
            max_agent_calls: Some(3),
            per_route: Default::default(),
            max_total_tokens: Some(6),
            reserved_closing_tokens: Some(2),
            max_concurrent_agent_calls: Some(1),
        },
        replay,
        Arc::new(AtomicU32::new(2)),
        Arc::new(AtomicU64::new(2)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let (work_reply, work_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: work_request,
        cancellation: CancellationToken::new(),
        reply: work_reply,
    })
    .expect("completed work replay");
    assert_eq!(
        work_receive
            .await
            .expect("work reply")
            .expect("work result")
            .output,
        Value::String("completed work".into())
    );
    let (snapshot_reply, snapshot_receive) = oneshot::channel();
    tx.send(HostCommand::BudgetSnapshot {
        reply: snapshot_reply,
    })
    .expect("snapshot replay");
    snapshot_receive
        .await
        .expect("snapshot reply")
        .expect("snapshot result");
    let (closing_reply, closing_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: closing_request,
        cancellation: CancellationToken::new(),
        reply: closing_reply,
    })
    .expect("closing replay");
    assert_eq!(
        closing_receive
            .await
            .expect("closing reply")
            .expect("closing result")
            .output,
        Value::String("resumed closing report".into())
    );
    let (beyond_reply, beyond_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        resolved_working_directory: None,
        request: request("past closing partition"),
        cancellation: CancellationToken::new(),
        reply: beyond_reply,
    })
    .expect("beyond closing call");
    assert_eq!(
        beyond_receive
            .await
            .expect("beyond reply")
            .expect_err("closing capacity exhausted")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host loop");
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "completed provider work was not duplicated"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_run_excludes_journalled_refusal_from_agent_call_count() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = "try { await agent('must not execute', {agent:'default'}); } catch (_) {} return await agent('does execute', {agent:'default'});";
    let budget = WorkflowBudget {
        max_agent_calls: Some(1),
        per_route: Default::default(),
        max_total_tokens: Some(1_000_000),
        reserved_closing_tokens: None,
        max_concurrent_agent_calls: Some(4),
    };
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(&budget).expect("budget"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request("must not execute".into())
    };
    let request_json = serde_json::to_value(&request).expect("request");
    let refused = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&request_json),
            request: request_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("refused intent");
    storage
        .fail_workflow_journal_entry(refused.id, "BudgetExceededError: agent call limit reached")
        .await
        .expect("refuse call");
    storage
        .update_workflow_run_state(run.id, "interrupted", None, None, None)
        .await
        .expect("interrupt run");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![
            swage_core::StreamChunk::Delta {
                content: "admitted".into(),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let _resumed = resume_run(State(state.clone()), Path(run.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, run.id).await;
    let resumed = storage
        .get_workflow_run(run.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        resumed.state, "completed",
        "resume failed: {:?}",
        resumed.error_message
    );
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal");
    assert_eq!(journal.len(), 3);
    assert_eq!(journal[0].id, refused.id);
    assert_eq!(journal[1].kind, "agent_call");
    assert_eq!(journal[1].status, "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn inline_run_returns_detached_and_completes_from_recorded_args() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "return args".into(),
            args: serde_json::json!({ "value": 42 }),
            budget: None,
        }),
    )
    .await
    .expect("start run");
    assert_eq!(started.state, WorkflowRunState::Running);

    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let run = state
                .storage
                .get_workflow_run(started.id)
                .await
                .expect("run")
                .expect("present");
            if run.state != "running" {
                break run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("workflow completes");
    assert_eq!(completed.state, "completed");
    assert_eq!(
        completed.final_output,
        Some(serde_json::json!({ "value": 42 }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_run_task_identity_is_persisted_and_filters_the_listing() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"export const meta = { task: "run-task-identity" }; return null;"#;
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "task identity".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: false,
            authoring_session_id: None,
        })
        .await
        .expect("script");

    let Json(saved) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start saved run");
    let Json(inline) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");
    let Json(without_task) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "return null;".into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start unlabelled run");

    let stored = storage
        .get_workflow_run(saved.id)
        .await
        .expect("read saved run")
        .expect("saved run exists");
    assert_eq!(stored.task.as_deref(), Some("run-task-identity"));
    assert_eq!(without_task.task, None);

    let Json(list) = list_runs(
        State(state),
        Path(topic.id),
        Query(WorkflowRunListQuery {
            task: Some("run-task-identity".into()),
        }),
    )
    .await
    .expect("list task runs");
    assert_eq!(list.runs.len(), 2);
    assert!(list.runs.iter().any(|run| run.id == saved.id));
    assert!(list.runs.iter().any(|run| run.id == inline.id));
    assert!(
        list.runs
            .iter()
            .all(|run| run.task.as_deref() == Some("run-task-identity"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_inline_agent_prompt_fails_after_journalling_valid_work() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "valid task result",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"await agent("Valid authored task");
            await agent({ task: "Invalid authored task" });"#
                .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start invalid inline run");

    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "failed");
    assert!(
        run.error_message
            .as_deref()
            .is_some_and(|message| message.contains("agent prompt must be a string")),
        "unexpected terminal error: {:?}",
        run.error_message
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let agent_calls = journal
        .iter()
        .filter(|entry| entry.kind == "agent_call")
        .collect::<Vec<_>>();
    assert_eq!(
        agent_calls.len(),
        1,
        "only the valid call should reach the journal: {journal:?}"
    );
    assert_eq!(agent_calls[0].status, "completed");
    assert_eq!(
        agent_calls[0].request["prompt"],
        Value::String("Valid authored task".into())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inline_run_without_reserve_keeps_hard_ceiling_behaviour() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(
            ["one", "two", "three"].map(scripted_text_output),
        )),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        for (let i = 0; i < 3; i += 1) await agent(`work ${i}`, {agent: "default"});
        try { await agent("past ceiling", {agent: "default"}); }
        catch (error) { return error.name; }
        return "unexpected";
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: None,
                per_route: Default::default(),
                max_total_tokens: Some(6),
                reserved_closing_tokens: None,
                max_concurrent_agent_calls: Some(4),
            }),
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    assert_eq!(
        run.final_output,
        Some(serde_json::json!("BudgetExceededError"))
    );
    assert_eq!(run.budget["reserved_closing_tokens"], Value::Null);
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    assert!(!journal.iter().any(|entry| entry.kind == "wind_down"));
    let terminal = journal
        .iter()
        .find(|entry| entry.kind == "terminal_budget")
        .expect("terminal budget entry");
    let reading = terminal.result.as_ref().expect("terminal budget reading");
    assert_eq!(reading["spent"], 6);
    assert_eq!(reading["remaining"], 0);
    assert!(reading["closing_remaining"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn exhausted_schema_agent_call_completes_without_a_durable_handover() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(
            (0..3)
                .map(|_| {
                    vec![swage_core::StreamChunk::Done {
                        finish_reason: None,
                    }]
                })
                .collect::<Vec<_>>(),
        )),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await postEvent("operator.escalation", { decisionQueued: true });
        const grounding = await deliverEvents(1);
        return await agent(`make the lead decision from ${JSON.stringify(grounding)}`, {
            agent: "default",
            label: "lead segment 3",
            maxAttempts: 3,
            schema: {
                type: "object",
                required: ["decision"],
                properties: { decision: { type: "string" } }
            }
        });
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");

    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed");
    assert_eq!(run.final_output, Some(Value::Null));
    assert!(run.handover.is_none());
    assert!(run.error_message.is_none());

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let failed_call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("failed agent call");
    assert_eq!(failed_call.status, "failed");
    assert!(
        failed_call
            .error_message
            .as_deref()
            .is_some_and(|error| error.starts_with("SchemaExhaustedError:"))
    );

    let events = storage
        .list_workflow_run_events(started.id)
        .await
        .expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "operator.escalation");
    assert!(events[0].delivered_at.is_some());
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);
    assert!(provider.requests.lock().expect("requests")[0]
        .messages
        .iter()
        .flat_map(|message| message.content())
        .any(|part| matches!(part, swage_core::ContentPart::Text { text } if text.contains(r#""delivered":1"#))));

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(detail.run.final_output, Some(Value::Null));
    assert!(detail.run.handover.is_none());
    assert_eq!(detail.journal.len(), journal.len());
    assert_eq!(detail.wake_metrics.events_pending, 0);

    assert_eq!(resume_owed_workflow_runs(&state).await, 0);
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);
    assert_eq!(
        storage
            .get_workflow_run(started.id)
            .await
            .expect("run")
            .expect("present")
            .state,
        "completed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_failed_agent_call_keeps_interrupted_handover_event_accounting() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![vec![
            swage_core::StreamChunk::Error {
                message: "ordinary provider failure".into(),
            },
        ]])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await postEvent("operator.escalation", { decisionQueued: true });
        const grounding = await deliverEvents(1);
        await agent(`make the lead decision from ${JSON.stringify(grounding)}`, {
            agent: "default",
            label: "lead segment 3",
            maxAttempts: 1,
        });
        return "unexpected";
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");

    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "failed");
    assert_eq!(run.final_output, None);
    let handover = run
        .handover
        .unwrap_or_else(|| panic!("durable handover; terminal error: {:?}", run.error_message));
    assert_eq!(handover["classification"], "interrupted");
    assert_eq!(handover["failed_call"]["kind"], "agent_call");
    assert_eq!(handover["failed_call"]["label"], "lead segment 3");
    let call_error = handover["failed_call"]["error"]
        .as_str()
        .expect("failed call error");
    assert!(call_error.starts_with("AgentCallError:"));
    let terminal_error = run.error_message.as_deref().expect("terminal error");
    assert!(terminal_error.contains(call_error));
    assert!(terminal_error.contains("at <anonymous> (eval_script:"));
    assert!(!terminal_error.contains("AgentCallError: AgentCallError:"));

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let failed_call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("failed agent call");
    assert_eq!(failed_call.status, "failed");
    assert_eq!(
        handover["failed_call"]["journal_entry_id"],
        failed_call.id.to_string()
    );
    assert_eq!(handover["failed_call"]["seq"], failed_call.seq);
    let last_positional_seq = journal
        .iter()
        .filter(|entry| is_positional_journal_entry(entry))
        .map(|entry| entry.seq)
        .max()
        .expect("positional journal entry");
    assert_eq!(
        handover["last_journal_seq"], last_positional_seq,
        "the terminal budget audit record is not a replay position"
    );
    assert_ne!(last_positional_seq, TERMINAL_BUDGET_SEQUENCE);

    let events = storage
        .list_workflow_run_events(started.id)
        .await
        .expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "operator.escalation");
    assert!(events[0].delivered_at.is_some());
    assert_eq!(handover["events_delivered_to_failed_call"], 1);
    assert_eq!(handover["events_pending"], 0);
    assert_eq!(handover["events"][0]["id"], events[0].id.to_string());
    assert_eq!(
        handover["events"][0]["delivery"],
        "delivered_to_failed_call"
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
    assert!(provider.requests.lock().expect("requests")[0]
        .messages
        .iter()
        .flat_map(|message| message.content())
        .any(|part| matches!(part, swage_core::ContentPart::Text { text } if text.contains(r#""delivered":1"#))));

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(detail.run.final_output, None);
    let typed_handover = detail
        .run
        .handover
        .expect("typed handover at the client surface");
    assert!(matches!(
        typed_handover,
        WorkflowRunHandover::Interrupted { .. }
    ));
    assert_eq!(
        serde_json::to_value(&typed_handover).expect("handover value"),
        handover
    );
    assert_eq!(detail.journal.len(), journal.len());
    assert_eq!(detail.wake_metrics.events_pending, 0);

    assert_eq!(resume_owed_workflow_runs(&state).await, 0);
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        run.state,
        "failed",
        "tool-only failure must stay primary: {:?}; requests: {}",
        run.error_message,
        provider.requests.lock().expect("requests").len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_failures_attribute_handover_to_the_terminal_call() {
    workflow_journey(async {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let run_id = Arc::new(tokio::sync::OnceCell::new());
        let root = tempfile::tempdir().expect("isolated workflow root");
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(OrderedFailingProvider {
                storage: Arc::clone(&storage),
                run_id: Arc::clone(&run_id),
                concurrent_entered: Arc::new(Notify::new()),
            }),
            workflow_worker_test_config(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        let source = r#"
        const terminalCall = agent("terminal call fails first", {
            agent: "default",
            label: "terminal call",
            maxAttempts: 1,
        });
        const concurrentCall = agent("concurrent call fails later", {
            agent: "default",
            label: "later concurrent call",
            maxAttempts: 1,
        });
        // Both failures settle before the script propagates the terminal one.
        await Promise.allSettled([terminalCall, concurrentCall]);
        await terminalCall;
    "#;
        let api = WorkflowTestApi::start(state.clone()).await;
        let started = api
            .start_inline(
                topic.id,
                StartInlineWorkflowRunRequest {
                    source: source.into(),
                    args: Value::Null,
                    budget: Some(WorkflowBudget {
                        max_agent_calls: None,
                        per_route: Default::default(),
                        max_total_tokens: None,
                        reserved_closing_tokens: None,
                        max_concurrent_agent_calls: Some(2),
                    }),
                },
            )
            .await;
        run_id.set(started.id).expect("fixture run identity");

        wait_for_run_state(&storage, started.id, "failed").await;

        let run = storage
            .get_workflow_run(started.id)
            .await
            .expect("run")
            .expect("present");
        let handover = run.handover.expect("attributed terminal handover");
        let journal = storage
            .list_workflow_journal(started.id)
            .await
            .expect("journal");
        let failed_call_error = handover["failed_call"]["error"]
            .as_str()
            .expect("attributed call error");
        let terminal_error = run.error_message.as_deref().expect("terminal error");
        let terminal_call = journal
            .iter()
            .find(|entry| entry.request["label"] == "terminal call")
            .expect("terminal failed call");
        let concurrent_call = journal
            .iter()
            .find(|entry| entry.request["label"] == "later concurrent call")
            .expect("other concurrent failed call");
        assert_eq!(run.state, "failed");
        assert_eq!(terminal_call.status, "failed");
        assert_eq!(concurrent_call.status, "failed");
        assert!(
            concurrent_call
                .error_message
                .as_deref()
                .is_some_and(|error| error.contains("later concurrent provider failure")),
            "second provider failure settles before script rejection"
        );
        assert_eq!(handover["classification"], "interrupted");
        assert_eq!(
            handover["failed_call"]["label"],
            terminal_call.request["label"]
        );
        assert_eq!(
            handover["failed_call"]["journal_entry_id"],
            terminal_call.id.to_string()
        );
        assert_ne!(
            handover["failed_call"]["journal_entry_id"],
            concurrent_call.id.to_string()
        );
        assert_eq!(
            terminal_call.error_message.as_deref(),
            Some(failed_call_error),
            "the handover must retain the known terminal call's failure"
        );
        assert_ne!(
            concurrent_call.error_message.as_deref(),
            Some(failed_call_error),
            "the later concurrent failure must remain distinguishable"
        );
        assert!(terminal_error.contains(failed_call_error));
        assert!(
            !terminal_error.contains(
                concurrent_call
                    .error_message
                    .as_deref()
                    .expect("later concurrent failure text")
            )
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn caught_agent_failure_does_not_blame_the_call_for_a_later_script_error() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![vec![
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ]])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        try {
            await agent("failure the script handles", {
                agent: "default",
                label: "handled call",
                maxAttempts: 1,
                schema: {
                    type: "object",
                    required: ["decision"],
                    properties: { decision: { type: "string" } }
                }
            });
        } catch (_) {}
        throw new Error("later unrelated script failure");
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");

    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "failed");
    assert!(
        run.error_message
            .as_deref()
            .is_some_and(|error| { error.contains("later unrelated script failure") })
    );
    let handover = run.handover.expect("degraded terminal handover");
    assert_eq!(handover["classification"], "interrupted_unattributed");
    assert!(
        handover.get("failed_call").is_none(),
        "the unattributed shape carries no failed-call identity"
    );
    assert!(
        handover["terminal_error"]
            .as_str()
            .is_some_and(|error| error.contains("later unrelated script failure"))
    );
    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert!(matches!(
        detail.run.handover,
        Some(WorkflowRunHandover::InterruptedUnattributed { .. })
    ));
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_call_reconstruction_failure_still_leaves_a_handover() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"await agent("known failing call", {agent: "default", maxAttempts: 1});"#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(WorkflowBudget::default()).expect("budget"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = AgentCallRequest {
        agent: Some("default".into()),
        ..swage_workflow::fixtures::agent_call_request("known failing call".into())
    };
    let request = serde_json::to_value(request).expect("request");
    let mismatched = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "log".into(),
            args_hash: journal_args_hash(&request),
            request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("journal intent");
    storage
        .fail_workflow_journal_entry(mismatched.id, "AgentCallError: terminal failure")
        .await
        .expect("failed journal entry");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("launch run");
    wait_for_terminal(&state, run.id).await;
    let terminal = storage
        .get_workflow_run(run.id)
        .await
        .expect("run")
        .expect("present");
    let handover = terminal.handover.expect("durable handover");

    assert_eq!(terminal.state, "failed");
    assert_eq!(handover["classification"], "reconstruction_failed");
    assert_eq!(
        handover["failure"],
        "The journal read-back failed for a known agent call."
    );
    assert_eq!(
        handover["failed_call"]["journal_entry_id"],
        mismatched.id.to_string()
    );
    assert_eq!(handover["failed_call"]["seq"], mismatched.seq);
    assert!(
        handover["terminal_error"]
            .as_str()
            .is_some_and(|error| error.contains("AgentCallError: terminal failure"))
    );
    let Json(detail) = get_run(State(state.clone()), Path(run.id))
        .await
        .expect("run detail");
    assert!(matches!(
        detail.run.handover,
        Some(WorkflowRunHandover::ReconstructionFailed { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn uncaught_recorded_agent_failure_replays_without_provider_work_and_keeps_attribution() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"await agent("known failing call", {agent: "default", label: "recorded failure", maxAttempts: 1});"#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(WorkflowBudget::default()).expect("budget"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = serde_json::to_value(AgentCallRequest {
        agent: Some("default".into()),
        label: Some("recorded failure".into()),
        ..swage_workflow::fixtures::agent_call_request("known failing call".into())
    })
    .expect("request");
    let failed = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&request),
            request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("journal intent");
    let recorded_error = "AgentCallError: recorded provider failure";
    storage
        .fail_workflow_journal_entry(failed.id, recorded_error)
        .await
        .expect("record failure");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    launch_existing_run(state.clone(), run, source.into(), None)
        .await
        .expect("launch replay");
    wait_for_terminal(&state, failed.run_id).await;
    let terminal = storage
        .get_workflow_run(failed.run_id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(terminal.state, "failed");
    assert!(
        terminal
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains(recorded_error))
    );
    assert!(provider.requests.lock().expect("requests").is_empty());
    let handover = terminal.handover.expect("attributed handover");
    assert_eq!(handover["classification"], "interrupted");
    assert_eq!(handover["failed_call"]["label"], "recorded failure");
    assert_eq!(
        handover["failed_call"]["journal_entry_id"],
        failed.id.to_string()
    );
    assert_eq!(handover["failed_call"]["error"], recorded_error);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handover_record_outside_the_contract_is_rejected_not_passed_through() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = "return 1;";
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(WorkflowBudget::default()).expect("budget"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    storage
        .update_workflow_run_state(
            run.id,
            "failed",
            None,
            Some(serde_json::json!({ "classification": "legacy_blob" })),
            Some("stored before the typed contract"),
        )
        .await
        .expect("stored blob");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let error = get_run(State(state), Path(run.id))
        .await
        .expect_err("a handover outside the contract must not pass through untyped");
    let message = format!("{error:?}");
    assert!(
        message.contains("does not match the recorded contract"),
        "error names the contract violation: {message}"
    );
    assert!(
        message.contains(&run.id.to_string()),
        "error names the offending record: {message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn budget_snapshot_funds_the_following_closing_segment_from_the_reserve() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(
            ["work one", "work two", "the real closing report"].map(scripted_text_output),
        )),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("work one", {agent: "default"});
        await agent("work two", {agent: "default"});
        const beforeClosing = await budgetSnapshot();
        const closingReport = await agent("closing lead segment", {
            agent: "default",
            label: "lead segment 4",
        });
        const afterClosing = await budgetSnapshot();
        return {
            stopped_reason: "wind-down",
            closing_report: closingReport,
            before_closing: beforeClosing,
            after_closing: afterClosing,
        };
    "#;
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "snapshot-led closing segment".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("script");
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(3),
                per_route: Default::default(),
                max_total_tokens: Some(8),
                reserved_closing_tokens: Some(4),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start saved run");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let output = run.final_output.as_ref().expect("real closing output");
    assert_eq!(output["stopped_reason"], "wind-down");
    assert_eq!(output["closing_report"], "the real closing report");
    assert_eq!(output["before_closing"]["winding_down"], true);
    assert_eq!(output["before_closing"]["working_remaining"], 0);
    assert_eq!(output["before_closing"]["closing_remaining"], 4);
    assert_eq!(output["after_closing"]["closing_remaining"], 2);
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("terminal budget journal");
    let terminal = journal
        .iter()
        .find(|entry| entry.kind == "terminal_budget")
        .expect("terminal budget entry");
    let reading = terminal.result.as_ref().expect("terminal budget reading");
    assert_eq!(reading["spent"], 6);
    assert_eq!(reading["remaining"], 2);
    assert_eq!(reading["closing_remaining"], 2);

    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;
    let replayed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(replayed.state, "completed");
    assert_eq!(replayed.final_output.as_ref(), Some(output));
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_detail_survives_malformed_results_and_preserves_workflow_output() {
    let (state, storage) = workflow_test_state();
    let topic = storage.general_scope().await.expect("topic");
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "return { completed: true };".into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start run");
    wait_for_terminal(&state, started.id).await;

    let malformed_await = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: started.id,
            seq: 100,
            kind: "await_event".into(),
            args_hash: journal_args_hash(&Value::Null),
            request: Value::Null,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("malformed await intent");
    storage
        .complete_workflow_journal_entry(malformed_await.id, serde_json::json!({"delivered": 3}))
        .await
        .expect("malformed await result");

    let workflow_call = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: started.id,
            seq: 101,
            kind: "workflow_call".into(),
            args_hash: journal_args_hash(&Value::Null),
            request: Value::Null,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("workflow call intent");
    let workflow_output = serde_json::json!({"kind": "gate", "answer": 7});
    storage
        .complete_workflow_journal_entry(workflow_call.id, workflow_output.clone())
        .await
        .expect("workflow call result");

    let Json(detail) = get_run(State(state), Path(started.id))
        .await
        .expect("run detail remains available");
    assert_eq!(
        detail
            .journal
            .iter()
            .find(|entry| entry.id == malformed_await.id)
            .expect("malformed await entry remains visible")
            .result,
        None
    );
    assert!(matches!(
        detail
            .journal
            .iter()
            .find(|entry| entry.id == workflow_call.id)
            .expect("workflow call entry")
            .result
            .as_ref(),
        Some(WorkflowJournalResult::WorkflowCall { output }) if output == &workflow_output
    ));
    assert!(detail.journal.iter().any(|entry| {
        entry.kind == WorkflowJournalEntryKind::TerminalBudget && entry.result.is_some()
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_budget_is_read_back_and_replayed_without_recomputation() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(
            ["working one", "working two", "closing report"].map(scripted_text_output),
        )),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("working one", {agent: "default"});
        await agent("working two", {agent: "default"});
        try { await agent("cross working boundary", {agent: "default"}); } catch (_) {}
        return await agent("closing report", {agent: "default"});
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(4),
                per_route: Default::default(),
                max_total_tokens: Some(6),
                reserved_closing_tokens: Some(2),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("independent journal read-back");
    let terminal = journal
        .iter()
        .find(|entry| entry.kind == "terminal_budget")
        .expect("terminal budget entry");
    assert_eq!(terminal.status, "completed");
    assert_eq!(terminal.result.as_ref().unwrap()["closing_remaining"], 0);
    assert_eq!(terminal.result.as_ref().unwrap()["spent"], 6);
    assert_eq!(terminal.result.as_ref().unwrap()["remaining"], 0);
    let terminal_id = terminal.id;

    // The stored entry deliberately disagrees with the accounting that
    // produced it. Replay must surface the durable audit reading rather
    // than derive a replacement from its end-state counters.
    storage
        .with_conn(move |connection| {
            connection.execute_batch(&format!(
                "UPDATE workflow_journal SET result_json = '{{\"spent\":6,\"remaining\":0,\"closing_remaining\":99,\"usage_unavailable_calls\":0}}' WHERE id = '{terminal_id}';"
            ))?;
            Ok(())
        })
        .await
        .expect("replace stored terminal reading");
    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;

    let bad_entry_id = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal after replay")
        .into_iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("agent call beside terminal reading")
        .id;
    storage
        .with_conn(move |connection| {
            connection.execute_batch(&format!(
                "UPDATE workflow_journal SET result_json = '{{\"input_tokens\":\"invalid\"}}' WHERE id = '{bad_entry_id}';"
            ))?;
            Ok(())
        })
        .await
        .expect("replace one stored result with an invalid shape");

    let Json(detail) = get_run(State(state), Path(started.id))
        .await
        .expect("get run");
    let invalid = detail
        .journal
        .iter()
        .find(|entry| entry.id == bad_entry_id)
        .expect("invalid entry remains in run detail");
    assert_eq!(invalid.result, None);
    let replayed = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::TerminalBudget)
        .expect("projected terminal budget entry");
    assert!(matches!(
        replayed.result,
        Some(WorkflowJournalResult::TerminalBudget {
            closing_remaining: Some(99),
            ..
        })
    ));
    assert_eq!(
        detail
            .journal
            .iter()
            .filter(|entry| entry.kind == WorkflowJournalEntryKind::TerminalBudget)
            .count(),
        1
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_budget_audit_failure_preserves_the_engine_outcome() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TEMP TRIGGER reject_terminal_budget_completion
                 BEFORE UPDATE OF status ON workflow_journal
                 WHEN OLD.kind = 'terminal_budget' AND OLD.status = 'started'
                      AND NEW.status = 'completed'
                 BEGIN SELECT RAISE(FAIL, 'injected terminal audit failure'); END;",
            )
        })
        .await
        .expect("install terminal audit failure trigger");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let mut events = state.broadcast.subscribe();
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "return { settled: true };".into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start run");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed");
    assert_eq!(
        run.final_output,
        Some(serde_json::json!({ "settled": true }))
    );
    let terminal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal")
        .into_iter()
        .find(|entry| entry.kind == "terminal_budget")
        .expect("terminal budget intent");
    assert_eq!(terminal.status, "failed");
    let event = loop {
        if let ServerEvent::WorkflowEntryCompleted { event } =
            events.recv().await.expect("terminal journal broadcast")
            && event.entry.id == terminal.id
        {
            break event;
        }
    };
    assert_eq!(event.entry.status, WorkflowJournalEntryStatus::Failed);
}

#[tokio::test(flavor = "multi_thread")]
async fn attempt_five_snapshot_enters_wind_down_through_the_consumer_expression() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
            scripted_text_output_with_usage("working segment", 12_000, 164),
            scripted_text_output_with_usage("the real closing report", 2_000, 500),
            scripted_text_output("leaked working capacity"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("working segment", {agent: "default"});
        const snapshot = await budgetSnapshot();
        const closingAllowance = 5000;
        const windingDown =
            snapshot.remaining !== null && snapshot.remaining <= closingAllowance;
        if (!windingDown) throw new Error("consumer did not enter wind-down");
        const closingReport = await agent("closing lead segment", {agent: "default"});
        let beyondClosing = null;
        try { await agent("past closing partition", {agent: "default"}); }
        catch (error) { beyondClosing = error.name; }
        return {
            stopped_reason: "wind-down",
            closing_report: closingReport,
            beyond_closing: beyondClosing,
            snapshot,
            after_closing: await budgetSnapshot(),
        };
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(6),
                per_route: Default::default(),
                max_total_tokens: Some(18_000),
                reserved_closing_tokens: Some(5_000),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let output = run.final_output.expect("real closing output");
    assert_eq!(output["stopped_reason"], "wind-down");
    assert_eq!(output["closing_report"], "the real closing report");
    assert_eq!(output["beyond_closing"], "BudgetExceededError");
    assert_eq!(output["snapshot"]["spent"], 12_164);
    assert_eq!(output["snapshot"]["remaining"], 836);
    assert_eq!(output["snapshot"]["working_remaining"], 0);
    assert_eq!(output["snapshot"]["closing_remaining"], 5_000);
    assert_eq!(output["after_closing"]["remaining"], 2_500);
    assert_eq!(output["after_closing"]["working_remaining"], 0);
    assert_eq!(output["after_closing"]["closing_remaining"], 2_500);
    assert_eq!(provider.requests.lock().expect("requests").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_led_closing_calls_remain_bounded_by_the_closing_partition() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
            scripted_text_output("work one"),
            scripted_text_output("work two"),
            scripted_text_output_with_usage("work three", 2, 1),
            scripted_text_output("closing report"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("work one", {agent: "default"});
        await agent("work two", {agent: "default"});
        await agent("work three", {agent: "default"});
        const snapshot = await budgetSnapshot();
        const closingReport = await agent("closing lead segment", {agent: "default"});
        let beyondClosing = null;
        try { await agent("past closing capacity", {agent: "default"}); }
        catch (error) { beyondClosing = error.name; }
        return {snapshot, closingReport, beyondClosing, finalSnapshot: await budgetSnapshot()};
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(5),
                per_route: Default::default(),
                max_total_tokens: Some(10),
                reserved_closing_tokens: Some(3),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let output = run.final_output.expect("output");
    assert_eq!(output["snapshot"]["winding_down"], true);
    assert_eq!(output["snapshot"]["spent"], 7);
    assert_eq!(output["snapshot"]["remaining"], 0);
    assert_eq!(output["snapshot"]["working_remaining"], 0);
    assert_eq!(output["snapshot"]["closing_remaining"], 3);
    assert_eq!(output["closingReport"], "closing report");
    assert_eq!(output["beyondClosing"], "BudgetExceededError");
    assert_eq!(output["finalSnapshot"]["winding_down"], true);
    assert_eq!(output["finalSnapshot"]["spent"], 9);
    assert_eq!(output["finalSnapshot"]["remaining"], 1);
    assert_eq!(output["finalSnapshot"]["working_remaining"], 0);
    assert_eq!(output["finalSnapshot"]["closing_remaining"], 1);
    assert_eq!(provider.requests.lock().expect("requests").len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_snapshot_led_closing_call_keeps_its_attributed_handover() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
            scripted_text_output("work one"),
            scripted_text_output("work two"),
            vec![swage_core::StreamChunk::Error {
                message: "closing provider failure".into(),
            }],
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("work one", {agent: "default"});
        await agent("work two", {agent: "default"});
        await budgetSnapshot();
        await agent("closing lead segment", {
            agent: "default",
            label: "lead segment 4",
            maxAttempts: 1,
        });
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(3),
                per_route: Default::default(),
                max_total_tokens: Some(6),
                reserved_closing_tokens: Some(2),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "failed");
    let handover = run.handover.expect("attributed terminal handover");
    assert_eq!(handover["classification"], "interrupted");
    assert_eq!(handover["failed_call"]["label"], "lead segment 4");
    let call_error = handover["failed_call"]["error"]
        .as_str()
        .expect("failed call error");
    assert!(call_error.starts_with("AgentCallError:"));
    assert!(
        run.error_message
            .as_deref()
            .is_some_and(|error| error.contains(call_error))
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_run_winds_down_into_reserve_and_replays_the_boundary() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(
            [
                "work one",
                "work two",
                "work three",
                "closing one",
                "closing two",
                "closing three",
            ]
            .map(scripted_text_output),
        )),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        const burst = [0, 1, 2, 3].map((i) =>
            agentStart(`work ${i}`, {agent: "default"})
        );
        const burstResults = await Promise.all(
            burst.map((handle) => handle.result.catch((error) => error.name))
        );
        const signal = burstResults[3];
        const snapshot = await budgetSnapshot();
        const closing = [
            await agent("closing one", {agent: "default"}),
            await agent("closing two", {agent: "default"}),
            await agent("closing three", {agent: "default"}),
        ];
        let ceiling = null;
        try { await agent("past full ceiling", {agent: "default"}); }
        catch (error) { ceiling = error.name; }
        return {signal, snapshot, closing, ceiling};
    "#;
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "budget wind-down".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("script");
    let budget = WorkflowBudget {
        max_agent_calls: None,
        per_route: Default::default(),
        max_total_tokens: Some(14),
        reserved_closing_tokens: Some(8),
        max_concurrent_agent_calls: Some(3),
    };
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: Some(budget.clone()),
        }),
    )
    .await
    .expect("start saved run");
    assert_eq!(started.budget, budget);
    wait_for_terminal(&state, started.id).await;

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(
        detail.run.state,
        WorkflowRunState::Completed,
        "run error: {:?}",
        detail.run.error_message
    );
    let output = detail.run.final_output.as_ref().expect("output");
    assert_eq!(output["signal"], "BudgetWindDownError");
    assert_eq!(output["snapshot"]["winding_down"], true);
    assert_eq!(output["snapshot"]["working_remaining"], 0);
    assert_eq!(output["snapshot"]["closing_remaining"], 8);
    assert_eq!(
        output["closing"],
        serde_json::json!(["closing one", "closing two", "closing three"])
    );
    assert_eq!(output["ceiling"], "BudgetExceededError");
    let wind_down = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::WindDown)
        .expect("wind-down journal entry");
    assert_eq!(wind_down.status, WorkflowJournalEntryStatus::Completed);
    assert!(matches!(
        wind_down.result,
        Some(WorkflowJournalResult::WindDown {
            winding_down: true,
            ..
        })
    ));
    assert_eq!(provider.requests.lock().unwrap().len(), 6);

    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;
    let replayed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        replayed.state, "completed",
        "replay error: {:?}",
        replayed.error_message
    );
    assert_eq!(replayed.final_output.as_ref(), Some(output));
    assert_eq!(provider.requests.lock().unwrap().len(), 6);
    assert_eq!(
        storage
            .list_workflow_journal(started.id)
            .await
            .expect("journal")
            .iter()
            .filter(|entry| entry.kind == "wind_down")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_refusal_transition_only_latches_wind_down_through_a_later_snapshot() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(
            ["work"].map(scripted_text_output),
        )),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("work", {agent: "default"});
        let refusal = null;
        try { await agent("cross working boundary", {agent: "default"}); }
        catch (error) { refusal = error.message; }
        let retriedRefusal = null;
        try { await agent("retry working boundary", {agent: "default"}); }
        catch (error) { retriedRefusal = error.message; }
        return {refusal, retriedRefusal, snapshot: await budgetSnapshot()};
    "#;
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "failed wind-down journal".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("script");
    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TEMP TRIGGER reject_wind_down_insert
                 BEFORE INSERT ON workflow_journal
                 WHEN NEW.kind = 'wind_down'
                 BEGIN SELECT RAISE(FAIL, 'injected wind-down persistence failure'); END;",
            )
        })
        .await
        .expect("install failure trigger");

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(2),
                per_route: Default::default(),
                max_total_tokens: Some(4),
                reserved_closing_tokens: Some(2),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start saved run");
    wait_for_terminal(&state, started.id).await;

    let Json(detail) = get_run(State(state), Path(started.id))
        .await
        .expect("run detail");
    let output = detail.run.final_output.expect("output");
    assert!(
        output["refusal"]
            .as_str()
            .is_some_and(|message| message.contains("injected wind-down persistence failure"))
    );
    assert!(
        output["retriedRefusal"]
            .as_str()
            .is_some_and(|message| message.contains("injected wind-down persistence failure"))
    );
    assert_eq!(output["snapshot"]["winding_down"], true);
    assert!(
        detail
            .journal
            .iter()
            .all(|entry| entry.kind != WorkflowJournalEntryKind::WindDown)
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_refusal_transition_completion_does_not_latch_on_replay() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(
            ["work"].map(scripted_text_output),
        )),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("work", {agent: "default"});
        let refusal = null;
        try { await agent("cross working boundary", {agent: "default"}); }
        catch (error) { refusal = error.message; }
        return {refusal, snapshot: await budgetSnapshot()};
    "#;
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "failed wind-down completion".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("script");
    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TEMP TRIGGER reject_wind_down_completion
                 BEFORE UPDATE OF status ON workflow_journal
                 WHEN OLD.kind = 'wind_down' AND OLD.status = 'started'
                      AND NEW.status = 'completed'
                 BEGIN SELECT RAISE(FAIL, 'injected wind-down completion failure'); END;",
            )
        })
        .await
        .expect("install failure trigger");

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(2),
                per_route: Default::default(),
                max_total_tokens: Some(4),
                reserved_closing_tokens: Some(2),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start saved run");
    wait_for_terminal(&state, started.id).await;
    let first = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    let first_output = first.final_output.expect("output");
    assert!(
        first_output["refusal"]
            .as_str()
            .is_some_and(|message| message.contains("injected wind-down completion failure"))
    );
    assert_eq!(first_output["snapshot"]["winding_down"], true);
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let transition = journal
        .iter()
        .find(|entry| entry.kind == "wind_down")
        .expect("transition entry");
    assert_eq!(transition.status, "failed");
    assert!(
        transition
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("injected wind-down completion failure"))
    );
    let account = terminal_account_of(&storage, started.id).await;
    // The run caught the refusal and returned, so its account names a
    // return. The `wind_down` entry it left behind never completed, and a
    // failed entry is not the host's record of a crossing.
    assert!(matches!(account.ending, WorkflowRunEnding::Returned));

    storage
        .with_conn(|connection| {
            connection.execute_batch("DROP TRIGGER reject_wind_down_completion;")
        })
        .await
        .expect("remove failure trigger");
    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;
    let replayed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(replayed.state, "completed");
    assert_eq!(replayed.final_output.as_ref(), Some(&first_output));
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "replay does not duplicate completed provider work"
    );
}

#[test]
fn only_a_completed_wind_down_entry_names_the_closing_boundary_as_the_ending() {
    fn wind_down(status: &str) -> WorkflowJournalRecord {
        WorkflowJournalRecord {
            id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            seq: 1,
            completed_order: None,
            kind: "wind_down".into(),
            status: status.into(),
            args_hash: String::new(),
            request: Value::Null,
            result: None,
            terminal_metadata: None,
            error_message: None,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            dispatch: None,
            dispatch_attempts: None,
            termination: None,
        }
    }
    let threw = || WorkflowRunEnding::Threw {
        error: "boom".into(),
    };

    assert!(
        matches!(
            refine_ending(threw(), &[wind_down("completed")]),
            WorkflowRunEnding::WoundDown { .. }
        ),
        "a completed wind-down entry is the host's record of the crossing"
    );
    assert!(
        matches!(
            refine_ending(threw(), &[wind_down("failed")]),
            WorkflowRunEnding::Threw { .. }
        ),
        "a wind-down intent that never completed did not close the run"
    );
    assert!(
        matches!(
            refine_ending(threw(), &[wind_down("started")]),
            WorkflowRunEnding::Threw { .. }
        ),
        "a wind-down intent still in flight did not close the run"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_transition_completion_retries_before_latching_on_resume() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
            scripted_text_output_with_usage("working segment", 1, 2),
            scripted_text_output("closing report"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        await agent("working segment", {agent: "default"});
        const snapshot = await budgetSnapshot();
        const closing = await agent("closing report", {agent: "default"});
        let beyondClosing = null;
        try { await agent("past closing partition", {agent: "default"}); }
        catch (error) { beyondClosing = error.name; }
        return {snapshot, closing, beyondClosing};
    "#;
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "snapshot completion retry".into(),
            description: String::new(),
            source: source.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("script");
    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TEMP TRIGGER reject_budget_completion
                 BEFORE UPDATE OF status ON workflow_journal
                 WHEN OLD.kind = 'budget' AND OLD.status = 'started'
                      AND NEW.status = 'completed'
                 BEGIN SELECT RAISE(FAIL, 'injected budget completion failure'); END;",
            )
        })
        .await
        .expect("install failure trigger");

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(3),
                per_route: Default::default(),
                max_total_tokens: Some(5),
                reserved_closing_tokens: Some(2),
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start saved run");
    wait_for_terminal(&state, started.id).await;
    let first = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(first.state, "failed");
    assert!(
        first
            .error_message
            .as_deref()
            .is_some_and(|error| error.contains("injected budget completion failure"))
    );
    let first_journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let terminal = first_journal
        .iter()
        .find(|entry| entry.kind == "terminal_budget")
        .expect("failed run terminal budget entry");
    assert_eq!(terminal.status, "completed");
    assert_eq!(terminal.seq, TERMINAL_BUDGET_SEQUENCE);
    let snapshot = first_journal
        .iter()
        .find(|entry| entry.kind == "budget")
        .expect("snapshot intent");
    assert_eq!(snapshot.status, "started");

    storage
        .with_conn(|connection| connection.execute_batch("DROP TRIGGER reject_budget_completion;"))
        .await
        .expect("remove failure trigger");
    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;

    let replayed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        replayed.state, "completed",
        "replay error: {:?}",
        replayed.error_message
    );
    let output = replayed.final_output.expect("output");
    assert_eq!(output["snapshot"]["winding_down"], true);
    assert_eq!(output["snapshot"]["closing_remaining"], 2);
    assert_eq!(output["closing"], "closing report");
    assert_eq!(output["beyondClosing"], "BudgetExceededError");
    let replayed_journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let snapshots = replayed_journal
        .iter()
        .filter(|entry| entry.kind == "budget")
        .collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].status, "completed");
    assert_eq!(
        replayed_journal
            .iter()
            .filter(|entry| entry.kind == "terminal_budget")
            .count(),
        1,
        "replay retains the failed-run terminal audit entry without treating it as a script command"
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_substitutes_completed_delivery_without_posting_again() {
    let deliveries = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let receiver = axum::Router::new().route(
        "/post",
        axum::routing::post({
            let deliveries = Arc::clone(&deliveries);
            move || {
                let deliveries = Arc::clone(&deliveries);
                async move {
                    deliveries.fetch_add(1, Ordering::SeqCst);
                    axum::http::StatusCode::NO_CONTENT
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, receiver).await.expect("receiver");
    });

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "deliver-once".into(),
            description: String::new(),
            source: "return await deliver({ content: args.content })".into(),
            delivery_grant: Some(
                serde_json::json!([{"kind":"webhook","url":format!("http://{address}/post")}]),
            ),
            captured_sources_grant: false,
            authoring_session_id: None,
        })
        .await
        .expect("script");
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: serde_json::json!({"content":"checked"}),
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);

    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _resumed = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_run_projects_script_version_and_parallel_failure_log() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (script, version) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "trace-contract".into(),
            description: String::new(),
            source: "await parallel([()=>{throw new Error('branch failed')}]); return {spent:budget.spent(),remaining:budget.remaining()};".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("script");
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: None,
                per_route: Default::default(),
                max_total_tokens: Some(10),
                reserved_closing_tokens: None,
                max_concurrent_agent_calls: None,
            }),
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;

    let Json(detail) = get_run(State(state), Path(started.id))
        .await
        .expect("detail");
    assert_eq!(detail.run.script_id, Some(script.id));
    assert_eq!(detail.run.script_name.as_deref(), Some("trace-contract"));
    assert_eq!(detail.run.script_version, Some(version.version));
    assert_eq!(
        detail.run.final_output,
        Some(serde_json::json!({"spent":0,"remaining":10}))
    );
    assert!(detail.journal.iter().any(|entry| {
        entry.kind == swage_protocol::client::workflow::WorkflowJournalEntryKind::Log
            && entry
                .request
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("parallel thunk 0 failed"))
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn composed_run_executes_the_child_and_journals_its_output() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "child".into(),
            description: String::new(),
            source: "return { doubled: args.value * 2 };".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("child script");
    let (parent, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "parent".into(),
            description: String::new(),
            source: "const child = await workflow(\"child\", { value: 21 }); return child;".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("parent script");
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(parent.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed");
    assert_eq!(run.final_output, Some(serde_json::json!({"doubled": 42})));

    // The child is a real run carrying its parentage, and the parent's
    // journal holds a completed workflow_call entry with the child's
    // output — the provenance a resumed parent replays from.
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let call = journal
        .iter()
        .find(|entry| entry.kind == "workflow_call")
        .expect("workflow_call entry");
    assert_eq!(call.status, "completed");
    assert_eq!(call.result, Some(serde_json::json!({"doubled": 42})));
    let child = storage
        .find_child_workflow_run(started.id, call.seq)
        .await
        .expect("query")
        .expect("child run recorded");
    assert_eq!(child.state, "completed");
    assert_eq!(child.parent_run_id, Some(started.id));

    // A recorded call is replayed as recorded rather than revalidated as
    // a fresh call. This is the compatibility path for journals written
    // before pre-spawn validation existed.
    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;
    let replayed = storage
        .list_workflow_journal(started.id)
        .await
        .expect("replay journal");
    assert_eq!(
        replayed
            .iter()
            .filter(|entry| entry.kind == "workflow_call")
            .count(),
        1,
        "replay reuses the journalled child call instead of creating another"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_author_errors_fail_before_agent_or_workflow_intents() {
    for (source, forbidden_kind, expected_error) in [
        (
            r#"await agent("bad schema", { schema: { type: 7 } });"#,
            "agent_call",
            "agent schema is invalid",
        ),
        (
            r#"await workflow("missing workflow", {});"#,
            "workflow_call",
            "workflow script \"missing workflow\" was not found",
        ),
    ] {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let (script, _) = storage
            .create_workflow_script(NewWorkflowScript {
                topic_id: topic.id,
                name: format!("pre-spawn {forbidden_kind}"),
                description: String::new(),
                source: source.into(),
                delivery_grant: None,
                captured_sources_grant: false,
                authoring_session_id: None,
            })
            .await
            .expect("script");
        let Json(started) = start_saved_run(
            State(state.clone()),
            Path(script.id),
            Json(StartWorkflowRunRequest {
                args: Value::Null,
                budget: None,
            }),
        )
        .await
        .expect("start");
        wait_for_terminal(&state, started.id).await;

        let run = storage
            .get_workflow_run(started.id)
            .await
            .expect("run")
            .expect("present");
        assert_eq!(run.state, "failed");
        assert!(
            run.error_message
                .as_deref()
                .is_some_and(|message| message.contains(expected_error)),
            "expected {expected_error} in {:?}",
            run.error_message
        );
        let journal = storage
            .list_workflow_journal(started.id)
            .await
            .expect("journal");
        assert!(
            !journal.iter().any(|entry| entry.kind == forbidden_kind),
            "{forbidden_kind} must not be journalled for a rejected call: {journal:#?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_agent_option_run_records_its_name_without_worker_dispatch() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "unexpected worker dispatch",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "unknown agent option".into(),
            description: String::new(),
            source: r#"await agent("inspect the unit", { bogusOption: true });"#.into(),
            delivery_grant: None,
            captured_sources_grant: false,
            authoring_session_id: None,
        })
        .await
        .expect("script");
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "failed");
    let message = run.error_message.as_deref().expect("recorded failure");
    assert!(
        message.contains("UnknownAgentOptionError: unknown agent() option 'bogusOption'"),
        "the run record must name the author error and option: {message}"
    );
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    assert!(
        !journal.iter().any(|entry| entry.kind == "agent_call"),
        "a rejected option must not create a worker call: {journal:#?}"
    );
    assert!(provider.requests.lock().expect("requests").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caught_missing_workflow_keeps_positional_replay_dense() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "child".into(),
            description: String::new(),
            source: "return { doubled: args.value * 2 };".into(),
            delivery_grant: None,
            captured_sources_grant: false,
            authoring_session_id: None,
        })
        .await
        .expect("child script");
    let (parent, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "parent".into(),
            description: String::new(),
            source: r#"try {
                await workflow("missing workflow", {});
            } catch (error) {
                log(`caught ${error.name}`);
            }
            return await workflow("child", { value: 21 });"#
                .into(),
            delivery_grant: None,
            captured_sources_grant: false,
            authoring_session_id: None,
        })
        .await
        .expect("parent script");
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(parent.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed");
    assert_eq!(run.final_output, Some(serde_json::json!({"doubled": 42})));
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    assert!(
        journal.iter().any(|entry| entry.kind == "log"
            && entry
                .request
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("WorkflowReferenceError"))),
        "the script must catch the named reference error: {journal:#?}"
    );
    // A rejected call journals no row, so it must consume no sequence
    // number either: replay rebuilds a compact vector and indexes it by
    // position, and a gap would shift every later row out of place.
    for (index, entry) in journal
        .iter()
        .filter(|entry| is_positional_journal_entry(entry))
        .enumerate()
    {
        assert_eq!(
            entry.seq, index as u64,
            "journalled commands must stay positionally dense: {journal:#?}"
        );
    }

    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _ = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;
    let resumed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(resumed.state, "completed");
    assert_eq!(
        resumed.final_output,
        Some(serde_json::json!({"doubled": 42})),
        "the retried missing reference must not displace the recorded child call"
    );
    let replayed = storage
        .list_workflow_journal(started.id)
        .await
        .expect("replay journal");
    assert_eq!(
        replayed
            .iter()
            .filter(|entry| entry.kind == "workflow_call")
            .count(),
        1,
        "replay reuses the journalled child call instead of creating another"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_placed_agent_schema_fails_before_worktree_provisioning() {
    let fixture = tempfile::tempdir().expect("scratch fixture");
    let repo = fixture.path().join("repo");
    seed_git_repo(&repo);

    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    grant_workspace(storage.as_ref(), topic.id, &repo).await;
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    )
    .with_worktree_root(fixture.path().join("worker-worktrees"));
    let cwd = serde_json::to_string(&repo.to_string_lossy()).expect("serialise cwd");
    let (script, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "invalid placed schema".into(),
            description: String::new(),
            source: format!(
                r#"await agent("bad schema", {{ cwd: {cwd}, isolation: "worktree", schema: {{ type: 7 }} }});"#
            ),
            delivery_grant: None,
            captured_sources_grant: false,
            authoring_session_id: None,
        })
        .await
        .expect("script");

    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(script.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "failed");
    assert!(
        run.error_message
            .as_deref()
            .is_some_and(|message| message.contains("agent schema is invalid")),
        "expected schema error in {:?}",
        run.error_message
    );
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    assert!(
        !journal.iter().any(|entry| entry.kind == "worker_placement"),
        "invalid call must not journal placement: {journal:#?}"
    );
    assert!(
        state.worktree_host.list_owner(started.id).await.is_empty(),
        "invalid call must not provision a worktree"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_run_cannot_compose_further() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let _skill_root = crate::skill::TestSkillRootGuard::empty(topic.id);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "grandchild".into(),
            description: String::new(),
            source: "return 1;".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("grandchild script");
    storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "child".into(),
            description: String::new(),
            // In a child run `workflow` must be absent entirely.
            source: "return typeof workflow;".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("child script");
    let (parent, _) = storage
        .create_workflow_script(NewWorkflowScript {
            topic_id: topic.id,
            name: "parent".into(),
            description: String::new(),
            source: "return await workflow(\"child\");".into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        })
        .await
        .expect("parent script");
    let Json(started) = start_saved_run(
        State(state.clone()),
        Path(parent.id),
        Json(StartWorkflowRunRequest {
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed");
    assert_eq!(run.final_output, Some(serde_json::json!("undefined")));
}

#[tokio::test(flavor = "multi_thread")]
async fn timer_won_wait_is_journalled_and_replays_the_same_winner() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "const wake = await wait({ timerMs: 10 }); return wake.wake;".into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start");
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed");
    assert_eq!(run.final_output, Some(serde_json::json!("timer")));
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let timer = journal
        .iter()
        .find(|entry| entry.kind == "timer")
        .expect("timer entry");
    assert_eq!(timer.status, "completed");
    assert_eq!(timer.result, Some(serde_json::json!({"fired": true})));
    assert_eq!(timer.request, serde_json::json!({"ms": 10}));

    // Genuine replay: interrupt the finished run and resume it. The
    // completed timer entry must replay instantly to the same winner,
    // adding no second timer entry to the journal.
    storage
        .update_workflow_run_state(started.id, "interrupted", None, None, None)
        .await
        .expect("simulate restart");
    let _resumed = resume_run(State(state.clone()), Path(started.id))
        .await
        .expect("resume");
    wait_for_terminal(&state, started.id).await;
    let replayed = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(replayed.state, "completed");
    assert_eq!(replayed.final_output, Some(serde_json::json!("timer")));
    let replay_journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    assert_eq!(
        replay_journal
            .iter()
            .filter(|entry| entry.kind == "timer")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn route_budgets_isolate_admission_and_journal_their_balances() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output_with_usage("fallback", 1, 0),
            scripted_text_output_with_usage("alpha", 1, 0),
            scripted_text_output_with_usage("beta", 1, 0),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let config = swage_config::ResolvedConfig {
        providers: swage_config::ProviderCatalogConfig {
            default: "alpha-route".into(),
            entries: [
                (
                    "alpha-route".into(),
                    swage_config::ProviderEndpointConfig::default(),
                ),
                (
                    "beta-route".into(),
                    swage_config::ProviderEndpointConfig::default(),
                ),
                (
                    "fallback-route".into(),
                    swage_config::ProviderEndpointConfig::default(),
                ),
            ]
            .into_iter()
            .collect(),
        },
        models: swage_config::ModelCatalogConfig {
            default: "alpha".into(),
            entries: [
                (
                    "alpha".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "alpha-route".into(),
                        model_id: "alpha".into(),
                        ..Default::default()
                    },
                ),
                (
                    "beta".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "beta-route".into(),
                        model_id: "beta".into(),
                        ..Default::default()
                    },
                ),
                (
                    "fallback".into(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "fallback-route".into(),
                        model_id: "fallback".into(),
                        ..Default::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
        },
        ..Default::default()
    };
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_agent_calls": 5,
        "max_concurrent_agent_calls": 2,
        "max_total_tokens": 4,
        "per_route": {
            "alpha-route": {"max_total_tokens": 2, "reserved_closing_tokens": 1},
            "beta-route": {"max_total_tokens": 3}
        }
    }))
    .expect("route budget contract");
    let source = r#"
        const fallback = await agent("fallback", { model: "fallback", maxAttempts: 1 });
        const fallbackSpent = budget.spent();
        const fallbackRemaining = budget.remaining();
        const alpha = await agent("alpha", { model: "alpha", maxAttempts: 1 });
        const snapshot = await budgetSnapshot();
        let exhausted; try { await agent("alpha again", { model: "alpha", maxAttempts: 1 }); }
        catch (error) { exhausted = error.name; }
        const beta = await agent("beta", { model: "beta", maxAttempts: 1 });
        const spentAfterRoutes = budget.spent();
        const remainingAfterRoutes = budget.remaining();
        let unknown; try { await agent("unknown", { model: "missing", maxAttempts: 1 }); }
        catch (error) { unknown = error.name; }
        return {fallback, fallbackSpent, fallbackRemaining, alpha, beta, exhausted, unknown, snapshot, spentAfterRoutes, remainingAfterRoutes};
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(budget),
        }),
    )
    .await
    .expect("start route budget run");
    wait_for_terminal(&state, started.id).await;
    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    let output = detail.run.final_output.expect("output");
    assert_eq!(output["fallback"], "fallback");
    assert_eq!(output["alpha"], "alpha");
    assert_eq!(output["beta"], "beta", "another route remains admissible");
    assert_eq!(output["exhausted"], "BudgetWindDownError");
    assert_eq!(output["fallbackSpent"], 1);
    assert_eq!(output["fallbackRemaining"], 3);
    assert_eq!(output["spentAfterRoutes"], 1);
    assert_eq!(output["remainingAfterRoutes"], 3);
    assert_eq!(
        output["unknown"], "AgentCallError",
        "unknown model is not a budget refusal"
    );
    assert_eq!(output["snapshot"]["per_route"]["alpha-route"]["spent"], 1);
    assert_eq!(
        output["snapshot"]["per_route"]["alpha-route"]["remaining"],
        1
    );
    assert_eq!(
        output["snapshot"]["per_route"]["alpha-route"]["closing_remaining"],
        1
    );
    assert_eq!(output["snapshot"]["per_route"]["beta-route"]["spent"], 0);
    assert_eq!(
        output["snapshot"]["per_route"]["beta-route"]["remaining"],
        3
    );
    assert_eq!(
        output["snapshot"]["per_route"]["beta-route"]["closing_remaining"],
        Value::Null
    );
    let snapshot = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::Budget)
        .and_then(|entry| entry.result.as_ref())
        .expect("budget snapshot journal result");
    let snapshot = serde_json::to_value(snapshot).expect("snapshot result serialises");
    assert_eq!(snapshot["per_route"]["alpha-route"]["spent"], 1);
    assert_eq!(snapshot["per_route"]["beta-route"]["remaining"], 3);
    let wind_down = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::WindDown)
        .expect("route wind-down journal");
    assert_eq!(wind_down.status, WorkflowJournalEntryStatus::Completed);
    assert_eq!(wind_down.request["provider_route"], "alpha-route");
    assert!(matches!(
        wind_down.result.as_ref(),
        Some(WorkflowJournalResult::WindDown {
            closing_limit: Some(2),
            ..
        })
    ));
    let terminal = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::TerminalBudget)
        .expect("terminal budget journal");
    let Some(WorkflowJournalResult::TerminalBudget { per_route, .. }) = terminal.result.as_ref()
    else {
        panic!("terminal budget result has the expected contract");
    };
    assert_eq!(per_route["alpha-route"].spent, 1);
    assert_eq!(per_route["alpha-route"].remaining, Some(1));
    assert_eq!(per_route["alpha-route"].closing_remaining, Some(1));
    assert_eq!(per_route["beta-route"].spent, 1);
    assert_eq!(per_route["beta-route"].remaining, Some(2));
    assert_eq!(per_route["beta-route"].closing_remaining, None);
    assert_eq!(provider.requests.lock().expect("requests").len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn route_admission_uses_the_topic_model_catalogue() {
    let config_root = tempfile::tempdir().expect("config root");
    let topic_config_dir = config_root.path().join("topics/research");
    std::fs::create_dir_all(&topic_config_dir).expect("topic config directory");
    std::fs::write(
        topic_config_dir.join("config.toml"),
        r#"
[models]
default = "topic-model"

[models.entries.topic-model]
provider_id = "topic-route"
model_id = "topic-wire"

[providers]
default = "topic-route"

[providers.entries.topic-route]
protocol = "openai"
"#,
    )
    .expect("topic config");
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = TopicStore::create(
        storage.as_ref(),
        swage_storage::NewTopic {
            name: "research".into(),
            slug: "research".into(),
        },
    )
    .await
    .expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output_with_usage("must not run", 1, 0),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        route_test_config("global-model", "global-route"),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
        config_root.path().to_path_buf(),
    );
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "per_route": {
            "global-route": {"max_total_tokens": 5},
            "topic-route": {"max_total_tokens": 0}
        }
    }))
    .expect("route budget");
    assert_eq!(
        resolve_agent_budget_route(&state, topic.id, &route_test_request("topic default", None))
            .await
            .expect("resolve scoped route"),
        Some("topic-route".into())
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                try { await agent("topic default", { agent: "default" }); }
                catch (error) { return error.name; }
            "#
            .into(),
            args: Value::Null,
            budget: Some(budget),
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        run.final_output,
        Some(Value::String("BudgetExceededError".into()))
    );
    assert!(
        provider.requests.lock().expect("requests").is_empty(),
        "the exhausted topic route must refuse before provider work"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_route_keeps_its_recorded_closing_limit() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = |prompt: &str| AgentCallRequest {
        agent: Some("default".into()),
        model: Some("alpha".into()),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    };
    let journal_request = |request: &AgentCallRequest| {
        let mut value = serde_json::to_value(request).expect("request serialises");
        value["provider_route"] = Value::String("alpha-route".into());
        value
    };
    let working = request("working");
    let working_json = journal_request(&working);
    let working_entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&serde_json::to_value(&working).expect("working request")),
            request: working_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("working entry");
    storage
        .complete_workflow_journal_entry(
            working_entry.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("working".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 1,
                    output_tokens: 0,
                },
                working_directory: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
                served_model: None,
                provider_route: None,
                worktree: None,
            })
            .expect("working result"),
        )
        .await
        .expect("complete working");
    let transition = request("transition");
    let transition_entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "wind_down".into(),
            args_hash: journal_args_hash(
                &serde_json::to_value(&transition).expect("transition request"),
            ),
            request: journal_request(&transition),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("transition entry");
    storage
        .complete_workflow_journal_entry(
            transition_entry.id,
            serde_json::json!({
                "spent": 1,
                "remaining": 0,
                "working_remaining": 0,
                "closing_remaining": 1,
                "closing_limit": 2,
                "winding_down": true,
            }),
        )
        .await
        .expect("complete transition");
    let closing = request("closing");
    let closing_entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 2,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&serde_json::to_value(&closing).expect("closing request")),
            request: journal_request(&closing),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("closing entry");
    storage
        .complete_workflow_journal_entry(
            closing_entry.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("closing".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 1,
                    output_tokens: 0,
                },
                working_directory: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
                served_model: None,
                provider_route: None,
                worktree: None,
            })
            .expect("closing result"),
        )
        .await
        .expect("complete closing");
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "per_route": {"alpha-route": {"max_total_tokens": 2, "reserved_closing_tokens": 1}}
    }))
    .expect("route budget");
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("replay journal");
    let token_budget = Arc::new(Mutex::new(terminal_budget_accounting_from_journal(
        &replay, &budget,
    )));
    assert_eq!(
        token_budget
            .lock()
            .await
            .ledger_mut("alpha-route")
            .closing_limit,
        Some(2),
        "resume restores the route's original closing boundary"
    );
    let config = route_test_config("alpha", "alpha-route");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands_with_budget(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget,
        replay,
        Arc::new(AtomicU32::new(2)),
        Arc::new(AtomicU64::new(0)),
        token_budget,
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        Arc::new(StdMutex::new(HostCommandAdmission::default())),
        rx,
    ));
    for replayed in [working, transition, closing] {
        let (reply, receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: replayed,
            resolved_working_directory: None,
            cancellation: CancellationToken::new(),
            reply,
        })
        .expect("replay command");
        let _ = receive.await.expect("replay reply");
    }
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: request("after resume"),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("post-resume command");
    assert_eq!(
        receive
            .await
            .expect("post-resume reply")
            .expect_err("route closing capacity is exhausted")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host task");
    let journal = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal after resume");
    assert_eq!(
        journal
            .iter()
            .filter(|entry| entry.kind == "wind_down")
            .count(),
        1,
        "resume does not grant a second closing reserve"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_route_restores_credit_for_work_settled_after_wind_down() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let late_request = route_test_request("late working call", Some("alpha"));
    let mut late_json = serde_json::to_value(&late_request).expect("late request");
    late_json["provider_route"] = Value::String("alpha-route".into());
    let late = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(
                &serde_json::to_value(&late_request).expect("late request"),
            ),
            request: late_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("late working intent");
    journal_routed_agent_call(
        &storage,
        run_id,
        1,
        &route_test_request("settled working call", Some("alpha")),
        "alpha-route",
        Some(4),
    )
    .await;
    let transition_request = route_test_request("wind down", Some("alpha"));
    let mut transition_json =
        serde_json::to_value(&transition_request).expect("transition request");
    transition_json["provider_route"] = Value::String("alpha-route".into());
    let transition = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id,
            seq: 2,
            kind: "wind_down".into(),
            args_hash: journal_args_hash(
                &serde_json::to_value(&transition_request).expect("transition request"),
            ),
            request: transition_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("wind-down intent");
    storage
        .complete_workflow_journal_entry(
            transition.id,
            serde_json::json!({
                "spent": 4,
                "remaining": 0,
                "working_remaining": 0,
                "closing_remaining": 2,
                "closing_limit": 6,
                "winding_down": true,
            }),
        )
        .await
        .expect("wind-down completion");
    storage
        .complete_workflow_journal_entry(
            late.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("late work".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 3,
                    output_tokens: 0,
                },
                working_directory: None,
                served_model: Some("alpha".into()),
                provider_route: Some("alpha-route".into()),
                worktree: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
            })
            .expect("late result"),
        )
        .await
        .expect("late settlement");
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "per_route": {
            "alpha-route": {"max_total_tokens": 6, "reserved_closing_tokens": 2}
        }
    }))
    .expect("route budget");
    let journal = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal");
    let accounting = terminal_budget_accounting_from_journal(&journal, &budget);
    assert_eq!(accounting.ledgers["alpha-route"].spent, 7);
    assert_eq!(accounting.ledgers["alpha-route"].closing_limit, Some(9));
    assert_eq!(
        terminal_budget_reading(&accounting, &budget)["per_route"]["alpha-route"]["closing_remaining"],
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_completes_a_started_route_wind_down_once() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let working = route_test_request("working", Some("alpha"));
    journal_routed_agent_call(&storage, run_id, 0, &working, "alpha-route", Some(4)).await;
    let transition = route_test_request("wind down", Some("alpha"));
    let mut transition_json =
        serde_json::to_value(&transition).expect("transition request serialises");
    transition_json["provider_route"] = Value::String("alpha-route".into());
    storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id,
            seq: 1,
            kind: "wind_down".into(),
            args_hash: journal_args_hash(
                &serde_json::to_value(&transition).expect("transition request"),
            ),
            request: transition_json,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("started route wind-down");
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_concurrent_agent_calls": 1,
        "per_route": {
            "alpha-route": {"max_total_tokens": 8, "reserved_closing_tokens": 4}
        }
    }))
    .expect("route budget");
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("replay journal");
    let accounting = terminal_budget_accounting_from_journal(&replay, &budget);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(FixedUsageProvider),
        route_test_config("alpha", "alpha-route"),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands_with_budget(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget,
        replay,
        Arc::new(AtomicU32::new(1)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(accounting)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        Arc::new(StdMutex::new(HostCommandAdmission::default())),
        rx,
    ));
    for replayed in [working, transition] {
        let (reply, receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: replayed,
            resolved_working_directory: None,
            cancellation: CancellationToken::new(),
            reply,
        })
        .expect("replay command");
        let _ = receive.await.expect("replay reply");
    }
    let (closing_reply, closing_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("closing report", Some("alpha")),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: closing_reply,
    })
    .expect("closing command");
    closing_receive
        .await
        .expect("closing reply")
        .expect("one closing reserve remains");
    drop(tx);
    task.await.expect("host task");

    let journal = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal after replay");
    let transitions = journal
        .iter()
        .filter(|entry| entry.kind == "wind_down")
        .collect::<Vec<_>>();
    assert_eq!(transitions.len(), 1, "the intent completes in place");
    assert_eq!(transitions[0].status, "completed");
    assert_eq!(transitions[0].result.as_ref().unwrap()["closing_limit"], 8);
    assert_eq!(
        transitions[0].result.as_ref().unwrap()["closing_remaining"],
        4
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_unconfigured_route_spend_uses_the_fallback_ledger() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request = AgentCallRequest {
        agent: Some("default".into()),
        model: Some("unconfigured".into()),
        ..swage_workflow::fixtures::agent_call_request("unconfigured route".into())
    };
    let mut journal_request = serde_json::to_value(&request).expect("request serialises");
    journal_request["provider_route"] = Value::String("unconfigured-route".into());
    let entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&serde_json::to_value(&request).expect("request")),
            request: journal_request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("journal entry");
    storage
        .complete_workflow_journal_entry(
            entry.id,
            serde_json::to_value(AgentCallResult {
                output: Value::String("completed".into()),
                usage: WorkflowCallUsage::Known {
                    input_tokens: 5,
                    output_tokens: 0,
                },
                working_directory: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
                served_model: None,
                provider_route: None,
                worktree: None,
            })
            .expect("result"),
        )
        .await
        .expect("complete entry");
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_total_tokens": 10,
        "max_concurrent_agent_calls": 1,
        "per_route": {"alpha-route": {"max_total_tokens": 1}}
    }))
    .expect("budget");
    let replay = storage
        .list_workflow_journal(run.id)
        .await
        .expect("replay journal");
    assert_eq!(fallback_spent_from_journal(&replay, &budget), 5);
    let accounting = terminal_budget_accounting_from_journal(&replay, &budget);
    assert_eq!(accounting.default.spent, 5);
    assert!(
        !accounting.ledgers.contains_key("unconfigured-route"),
        "an unconfigured route is accounted by the fallback ledger"
    );
    let spent_tokens = Arc::new(AtomicU64::new(fallback_spent_from_journal(
        &replay, &budget,
    )));
    let config = route_test_config("unconfigured", "unconfigured-route");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands_with_budget(
        state,
        run.id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget.clone(),
        replay,
        Arc::new(AtomicU32::new(1)),
        Arc::clone(&spent_tokens),
        Arc::new(Mutex::new(accounting)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        Arc::new(StdMutex::new(HostCommandAdmission::default())),
        rx,
    ));
    let (replay_reply, replay_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: request.clone(),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: replay_reply,
    })
    .expect("replay command");
    replay_receive
        .await
        .expect("replay reply")
        .expect("replayed result");
    assert_eq!(spent_tokens.load(Ordering::SeqCst), 5);
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request,
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("next command");
    assert_eq!(
        receive
            .await
            .expect("next reply")
            .expect_err("the fallback ceiling is already partly spent")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host task");
}

fn route_test_config(model: &str, provider_id: &str) -> swage_config::ResolvedConfig {
    swage_config::ResolvedConfig {
        providers: swage_config::ProviderCatalogConfig {
            default: provider_id.into(),
            entries: [(
                provider_id.into(),
                swage_config::ProviderEndpointConfig::default(),
            )]
            .into_iter()
            .collect(),
        },
        models: swage_config::ModelCatalogConfig {
            default: model.into(),
            entries: [(
                model.into(),
                swage_config::ModelEndpointConfig {
                    provider_id: provider_id.into(),
                    model_id: model.into(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        },
        ..Default::default()
    }
}

fn route_test_request(prompt: &str, model: Option<&str>) -> AgentCallRequest {
    AgentCallRequest {
        agent: Some("default".into()),
        model: model.map(str::to_string),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    }
}

async fn empty_journal_run(storage: &SqliteStorage, topic_id: Uuid) -> Uuid {
    storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic_id)
        })
        .await
        .expect("run")
        .id
}

/// Journals `request` at `seq` on the named route, completing it with the
/// given usage when `completed_tokens` is present and leaving it started
/// otherwise.
async fn journal_routed_agent_call(
    storage: &SqliteStorage,
    run_id: Uuid,
    seq: u64,
    request: &AgentCallRequest,
    route: &str,
    completed_tokens: Option<u64>,
) {
    let request_json = serde_json::to_value(request).expect("request serialises");
    let mut journal_request = request_json.clone();
    journal_request["provider_route"] = Value::String(route.into());
    let entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id,
            seq,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&request_json),
            request: journal_request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("journal entry");
    if let Some(input_tokens) = completed_tokens {
        storage
            .complete_workflow_journal_entry(
                entry.id,
                serde_json::to_value(AgentCallResult {
                    output: Value::String("recorded".into()),
                    usage: WorkflowCallUsage::Known {
                        input_tokens,
                        output_tokens: 0,
                    },
                    working_directory: None,
                    session_id: None,
                    thread_id: None,
                    turn_id: None,
                    served_model: None,
                    provider_route: None,
                    worktree: None,
                })
                .expect("result"),
            )
            .await
            .expect("complete entry");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_does_not_readmit_a_started_route_call_after_later_spend() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let started_request = route_test_request("interrupted earlier call", Some("alpha"));
    journal_routed_agent_call(&storage, run_id, 0, &started_request, "alpha-route", None).await;
    let completed_request = route_test_request("later completed call", Some("alpha"));
    journal_routed_agent_call(
        &storage,
        run_id,
        1,
        &completed_request,
        "alpha-route",
        Some(4),
    )
    .await;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_concurrent_agent_calls": 2,
        "per_route": {"alpha-route": {"max_total_tokens": 4}}
    }))
    .expect("budget");
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("replay journal");
    let accounting = Arc::new(Mutex::new(terminal_budget_accounting_from_journal(
        &replay, &budget,
    )));
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output_with_usage("recovered", 1, 0),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        route_test_config("alpha", "alpha-route"),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands_with_budget(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget,
        replay,
        Arc::new(AtomicU32::new(2)),
        Arc::new(AtomicU64::new(0)),
        accounting,
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        Arc::new(StdMutex::new(HostCommandAdmission::default())),
        rx,
    ));
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: started_request,
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("resumed command");
    receive
        .await
        .expect("resumed reply")
        .expect("the already-admitted call re-executes");
    drop(tx);
    task.await.expect("host task");

    let replayed = storage
        .list_workflow_journal(run_id)
        .await
        .expect("replayed journal");
    assert_eq!(replayed[0].status, "completed");
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
}

/// Emits a fixed usage reading for every call, so a test can spend a
/// route's allowance a known amount at a time.
struct FixedUsageProvider;

#[async_trait::async_trait]
impl swage_provider::LlmProvider for FixedUsageProvider {
    fn model_id(&self) -> &str {
        "fixed-usage-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(scripted_text_output_with_usage(
            "route work",
            4,
            0,
        )))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_call_spends_the_route_that_serves_after_a_config_change() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let resumed_request = route_test_request("interrupted call", Some("alpha"));
    journal_routed_agent_call(&storage, run_id, 0, &resumed_request, "alpha-route", None).await;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_concurrent_agent_calls": 1,
        "per_route": {
            "alpha-route": {"max_total_tokens": 4},
            "beta-route": {"max_total_tokens": 4},
        }
    }))
    .expect("budget");
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("replay journal");
    // The model has moved to another provider since the run was interrupted.
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(FixedUsageProvider),
        route_test_config("alpha", "beta-route"),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget,
        replay,
        Arc::new(AtomicU32::new(1)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    let (resumed_reply, resumed_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: resumed_request,
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: resumed_reply,
    })
    .expect("resumed command");
    resumed_receive
        .await
        .expect("resumed reply")
        .expect("resumed result");
    let (exhausted_reply, exhausted_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("beyond the serving route ceiling", None),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: exhausted_reply,
    })
    .expect("exhausted command");
    assert_eq!(
        exhausted_receive
            .await
            .expect("exhausted reply")
            .expect_err("the route that served the resumed call is spent to its ceiling")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host task");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_fallback_call_spends_the_route_that_served_it() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let mut resumed_request = route_test_request("interrupted call", Some("pinned"));
    resumed_request.fallback_model = Some("fallback".into());
    journal_routed_agent_call(&storage, run_id, 0, &resumed_request, "primary-route", None).await;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_concurrent_agent_calls": 1,
        "per_route": {
            "primary-route": {"max_total_tokens": 8},
            "fallback-route": {"max_total_tokens": 2}
        }
    }))
    .expect("budget");
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("replay journal");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            vec![swage_core::StreamChunk::Error {
                message: "primary refused".into(),
            }],
            scripted_text_output_with_usage("fallback answer", 2, 0),
            scripted_text_output_with_usage("must not run", 1, 0),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        fallback_test_config(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget,
        replay,
        Arc::new(AtomicU32::new(1)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));

    let (resumed_reply, resumed_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: resumed_request,
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: resumed_reply,
    })
    .expect("resumed command");
    resumed_receive
        .await
        .expect("resumed reply")
        .expect("fallback serves the resumed call");

    let (next_reply, next_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("beyond the fallback ceiling", Some("fallback")),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: next_reply,
    })
    .expect("next command");
    assert_eq!(
        next_receive
            .await
            .expect("next reply")
            .expect_err("the serving route is spent to its ceiling")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host task");
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        2,
        "the refused call must not reach the provider"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_default_route_keeps_its_own_spend_across_replay() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let recorded = route_test_request("recorded spend", Some("plain"));
    journal_routed_agent_call(&storage, run_id, 0, &recorded, "default", Some(5)).await;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_total_tokens": 100,
        "max_concurrent_agent_calls": 1,
        "per_route": {"default": {"max_total_tokens": 5}}
    }))
    .expect("budget");
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("replay journal");
    let accounting = terminal_budget_accounting_from_journal(&replay, &budget);
    assert_eq!(
        accounting.ledgers["default"].spent, 5,
        "a provider route the budget configures keeps its own ledger, \
         whatever it is called"
    );
    assert_eq!(accounting.default.spent, 0);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(crate::turn::tests::MockProvider::new(vec![])),
        route_test_config("plain", "default"),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands_with_budget(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget,
        replay,
        Arc::new(AtomicU32::new(1)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(accounting)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        Arc::new(StdMutex::new(HostCommandAdmission::default())),
        rx,
    ));
    let (replayed_reply, replayed_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: recorded,
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: replayed_reply,
    })
    .expect("replay command");
    replayed_receive
        .await
        .expect("replay reply")
        .expect("recorded result");
    let (next_reply, next_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("beyond the ceiling", Some("plain")),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: next_reply,
    })
    .expect("next command");
    assert_eq!(
        next_receive
            .await
            .expect("next reply")
            .expect_err("the configured route is spent to its ceiling")
            .to_string(),
        "BudgetExceededError: token limit reached"
    );
    drop(tx);
    task.await.expect("host task");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_model_fails_as_its_own_error_on_a_spent_fallback_ledger() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let recorded = route_test_request("spend the fallback allowance", Some("alpha"));
    journal_routed_agent_call(&storage, run_id, 0, &recorded, "alpha-route", Some(1)).await;
    let budget = WorkflowBudget {
        max_agent_calls: Some(3),
        per_route: Default::default(),
        max_total_tokens: Some(1),
        reserved_closing_tokens: None,
        max_concurrent_agent_calls: Some(1),
    };
    let replay = storage
        .list_workflow_journal(run_id)
        .await
        .expect("replay journal");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        route_test_config("alpha", "alpha-route"),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands_with_budget(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget.clone(),
        replay.clone(),
        Arc::new(AtomicU32::new(1)),
        Arc::new(AtomicU64::new(1)),
        Arc::new(Mutex::new(terminal_budget_accounting_from_journal(
            &replay, &budget,
        ))),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        Arc::new(StdMutex::new(HostCommandAdmission::default())),
        rx,
    ));
    let (replay_reply, replay_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: recorded,
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: replay_reply,
    })
    .expect("replay command");
    replay_receive
        .await
        .expect("replay reply")
        .expect("recorded spend replays");
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("name a model that is not there", Some("missing")),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("unknown model command");
    let error = receive
        .await
        .expect("unknown model reply")
        .expect_err("the model is not in the catalogue")
        .to_string();
    assert!(
        error.starts_with("AgentCallError:") && error.contains("unknown model 'missing'"),
        "a call that can draw on no allowance reads as its own failure, \
         not as exhaustion: {error}"
    );
    drop(tx);
    task.await.expect("host task");
    assert!(
        storage
            .list_sessions_by_agent_id("default")
            .await
            .expect("worker sessions")
            .is_empty(),
        "model resolution must fail before a worker exists"
    );
    assert!(
        provider.requests.lock().expect("requests").is_empty(),
        "model resolution must fail before a provider request"
    );
    assert_eq!(
        storage
            .list_workflow_journal(run_id)
            .await
            .expect("journal")
            .iter()
            .filter(|entry| entry.kind == "agent_call")
            .count(),
        1,
        "the unknown-model call must not create a journal entry"
    );
}

/// Holds its first call open until released, so a test can settle a call
/// admitted while a route was working after that route has wound down.
struct HeldFirstCallProvider {
    calls: AtomicUsize,
    first_call_started: Arc<Notify>,
    release_first_call: Arc<Notify>,
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for HeldFirstCallProvider {
    fn model_id(&self) -> &str {
        "held-first-call-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let started = Arc::clone(&self.first_call_started);
            let release = Arc::clone(&self.release_first_call);
            Box::pin(futures::StreamExt::flatten(futures::stream::once(
                async move {
                    started.notify_one();
                    release.notified().await;
                    futures::stream::iter(scripted_text_output_with_usage("held work", 3, 0))
                },
            )))
        } else {
            Box::pin(futures::stream::iter(scripted_text_output_with_usage(
                "route work",
                4,
                0,
            )))
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_working_route_call_settling_after_wind_down_keeps_the_closing_allowance() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run_id = empty_journal_run(&storage, topic.id).await;
    let first_call_started = Arc::new(Notify::new());
    let release_first_call = Arc::new(Notify::new());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(HeldFirstCallProvider {
            calls: AtomicUsize::new(0),
            first_call_started: Arc::clone(&first_call_started),
            release_first_call: Arc::clone(&release_first_call),
        }),
        route_test_config("alpha", "alpha-route"),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_concurrent_agent_calls": 4,
        "per_route": {
            "alpha-route": {"max_total_tokens": 6, "reserved_closing_tokens": 2}
        }
    }))
    .expect("budget");
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run_id,
        topic.id,
        vec![],
        vec![],
        None,
        None,
        budget,
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    // Admitted while the route is still working, and held open.
    let (held_reply, held_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("held working call", None),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: held_reply,
    })
    .expect("held call");
    first_call_started.notified().await;
    // Spends the route's whole working partition.
    let (working_reply, working_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("settled working call", None),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: working_reply,
    })
    .expect("working call");
    working_receive
        .await
        .expect("working reply")
        .expect("working result");
    let (transition_reply, transition_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("cross the working boundary", None),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: transition_reply,
    })
    .expect("transition call");
    assert_eq!(
        transition_receive
            .await
            .expect("transition reply")
            .expect_err("the route's working allowance is spent")
            .to_string(),
        "BudgetWindDownError: working token allowance exhausted"
    );
    release_first_call.notify_one();
    held_receive
        .await
        .expect("held reply")
        .expect("held result");
    let (closing_reply, closing_receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request: route_test_request("closing report", None),
        resolved_working_directory: None,
        cancellation: CancellationToken::new(),
        reply: closing_reply,
    })
    .expect("closing call");
    closing_receive.await.expect("closing reply").expect(
        "the closing allowance this route reserved is not consumed by \
             a call admitted while it was working",
    );
    drop(tx);
    task.await.expect("host task");
}

fn fallback_test_config() -> swage_config::ResolvedConfig {
    let mut config = swage_config::ResolvedConfig::default();
    config.providers.entries.insert(
        "primary-route".into(),
        swage_config::ProviderEndpointConfig {
            worker_staleness_threshold_ms: Some(111),
            ..Default::default()
        },
    );
    config.providers.entries.insert(
        "fallback-route".into(),
        swage_config::ProviderEndpointConfig {
            worker_staleness_threshold_ms: Some(222),
            ..Default::default()
        },
    );
    config.models.default = "pinned".into();
    config.models.entries.insert(
        "pinned".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "primary-route".into(),
            model_id: "pinned-wire".into(),
            ..Default::default()
        },
    );
    config.models.entries.insert(
        "fallback".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "fallback-route".into(),
            model_id: "fallback-wire".into(),
            ..Default::default()
        },
    );
    config
}

async fn fallback_test_state(
    outputs: Vec<Vec<swage_core::StreamChunk>>,
) -> (
    AppState,
    Arc<SqliteStorage>,
    Arc<ScriptedRecordingProvider>,
    Uuid,
) {
    fallback_test_state_with_tools(outputs, vec![]).await
}

async fn fallback_test_state_with_tools(
    outputs: Vec<Vec<swage_core::StreamChunk>>,
    tools: Vec<Arc<dyn crate::tool::Tool>>,
) -> (
    AppState,
    Arc<SqliteStorage>,
    Arc<ScriptedRecordingProvider>,
    Uuid,
) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let mut config = fallback_test_config();
    config.tools.permissions.allow = tools.iter().map(|tool| tool.name().to_string()).collect();
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(outputs.into()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(tools),
        None,
    );
    (state, storage, provider, topic.id)
}

async fn run_declared_fallback(state: &AppState, topic_id: Uuid, model: &str) -> Uuid {
    let source = format!(
        r#"return await agent("answer", {{ agent: "default", model: {model:?}, fallbackModel: "fallback" }});"#
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source,
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(state, started.id).await;
    started.id
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_agent_calls_journal_the_effective_catalogue_model() {
    let (state, storage, provider, topic_id) = fallback_test_state(vec![
        scripted_text_output("pinned answer"),
        scripted_text_output("default answer"),
        scripted_text_output("task answer"),
    ])
    .await;
    let mut config = fallback_test_config();
    config.models.default = "default".into();
    config.models.entries.insert(
        "default".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "scripted".into(),
            model_id: "default-wire".into(),
            ..Default::default()
        },
    );
    config.models.entries.insert(
        "task-overlay".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "scripted".into(),
            model_id: "task-overlay-wire".into(),
            ..Default::default()
        },
    );
    state.replace_runtime(
        crate::test_support::with_mock_provider_entries(config),
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
    );
    storage
        .upsert_task_definition(swage_storage::NewTaskDefinition {
            id: "task-overlay".into(),
            instructions: "Use the task's model.".into(),
            model_id: Some("task-overlay".into()),
            tool_permissions: None,
            output_schema: None,
            output_max_retries: None,
        })
        .await
        .expect("store task overlay");

    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                const pinned = await agent("pinned", { agent: "default", model: "pinned" });
                const defaultModel = await agent("default", { agent: "default" });
                const task = await agent("task", { agent: "default", task: "task-overlay" });
                return { pinned, defaultModel, task };
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    let calls = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal")
        .into_iter()
        .filter(|entry| entry.kind == "agent_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls
            .iter()
            .map(|entry| {
                entry
                    .result
                    .as_ref()
                    .and_then(|result| result["served_model"].as_str())
            })
            .collect::<Vec<_>>(),
        vec![Some("pinned"), Some("default"), Some("task-overlay")],
        "each successful primary call must name the catalogue entry that actually served it"
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .expect("requests")
            .iter()
            .map(|request| request.model.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("pinned-wire"),
            Some("default-wire"),
            Some("task-overlay-wire")
        ],
        "each call must complete on its primary model without a fallback"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_fallback_model_retries_only_before_output_and_records_the_served_model() {
    let (state, storage, provider, topic_id) =
        fallback_test_state(vec![scripted_text_output("fallback answer")]).await;
    let run_id = run_declared_fallback(&state, topic_id, "missing").await;
    let run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        run.state, "completed",
        "an absent pinned model must use its declared fallback"
    );
    let journal = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal");
    let call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("agent call");
    assert_eq!(
        call.result
            .as_ref()
            .and_then(|result| result["served_model"].as_str()),
        Some("fallback")
    );
    assert_eq!(
        call.result
            .as_ref()
            .and_then(|result| result["provider_route"].as_str()),
        Some("fallback-route"),
        "the journal must retain the provider route that actually served"
    );
    let Json(detail) = get_run(State(state.clone()), Path(run_id))
        .await
        .expect("workflow detail");
    assert_eq!(
        detail
            .journal
            .iter()
            .find(|entry| entry.agent_call.is_some())
            .and_then(|entry| entry.agent_call.as_ref())
            .and_then(|call| call.served_model.as_deref()),
        Some("fallback"),
        "the workflow projection must expose the model that served"
    );
    let requests = provider.requests.lock().expect("requests");
    assert_eq!(
        requests.len(),
        1,
        "the unknown model must fail before a provider request"
    );
    assert_eq!(requests[0].model.as_deref(), Some("fallback-wire"));
    assert_eq!(
        requests[0]
            .messages
            .iter()
            .filter(|message| message.text_content() == "answer")
            .count(),
        1,
        "an unknown primary must pass the original prompt to its fallback exactly once"
    );

    let (state, storage, provider, topic_id) = fallback_test_state(vec![
        vec![swage_core::StreamChunk::Error {
            message: "pinned refused".into(),
        }],
        scripted_text_output("fallback answer"),
    ])
    .await;
    let run_id = run_declared_fallback(&state, topic_id, "pinned").await;
    let run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        run.state, "completed",
        "a pre-output provider failure must use the fallback"
    );
    let journal = storage
        .list_workflow_journal(run_id)
        .await
        .expect("journal");
    let call = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("agent call");
    assert_eq!(
        call.result
            .as_ref()
            .and_then(|result| result["served_model"].as_str()),
        Some("fallback")
    );
    let requests = provider.requests.lock().expect("requests");
    assert_eq!(
        requests
            .iter()
            .map(|request| request.model.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("pinned-wire"), Some("fallback-wire")]
    );
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|message| message.text_content() == "answer")
            .count(),
        1,
        "the fallback must reuse the primary prompt rather than append it again"
    );

    let (state, storage, provider, topic_id) = fallback_test_state(vec![vec![
        swage_core::StreamChunk::Delta {
            content: "partial".into(),
        },
        swage_core::StreamChunk::Error {
            message: "failed after output".into(),
        },
    ]])
    .await;
    let run_id = run_declared_fallback(&state, topic_id, "pinned").await;
    let run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        run.state, "failed",
        "a failure after output must preserve the worker failure"
    );
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "a partial response must not trigger fallback"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_fallback_spends_only_its_provider_route() {
    let (state, storage, _provider, topic_id) = fallback_test_state(vec![
        vec![swage_core::StreamChunk::Error {
            message: "primary refused".into(),
        }],
        scripted_text_output_with_usage("fallback answer", 2, 0),
    ])
    .await;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "per_route": {
            "primary-route": {"max_total_tokens": 7},
            "fallback-route": {"max_total_tokens": 3}
        }
    }))
    .expect("route budget");
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"return await agent("answer", { agent: "default", model: "pinned", fallbackModel: "fallback" });"#.into(),
            args: Value::Null,
            budget: Some(budget),
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let terminal = journal
        .iter()
        .find(|entry| entry.kind == "terminal_budget")
        .and_then(|entry| entry.result.as_ref())
        .expect("terminal budget result");
    assert_eq!(terminal["per_route"]["primary-route"]["spent"], 0);
    assert_eq!(terminal["per_route"]["fallback-route"]["spent"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_fallback_refuses_an_exhausted_provider_route_before_dispatch() {
    let (state, storage, provider, topic_id) = fallback_test_state(vec![
        vec![swage_core::StreamChunk::Error {
            message: "primary refused".into(),
        }],
        scripted_text_output("must not run"),
    ])
    .await;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "per_route": {
            "primary-route": {"max_total_tokens": 7},
            "fallback-route": {"max_total_tokens": 0}
        }
    }))
    .expect("route budget");
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                try {
                    await agent("answer", {
                        agent: "default", model: "pinned", fallbackModel: "fallback"
                    });
                } catch (error) {
                    return error.name;
                }
            "#
            .into(),
            args: Value::Null,
            budget: Some(budget),
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        run.final_output,
        Some(Value::String("BudgetExceededError".into()))
    );
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "the exhausted fallback route must refuse before its provider request"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_fallback_on_an_unconfigured_route_uses_the_fallback_ledger() {
    let (state, storage, provider, topic_id) = fallback_test_state(vec![
        vec![swage_core::StreamChunk::Error {
            message: "primary refused".into(),
        }],
        scripted_text_output("must not run"),
    ])
    .await;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_total_tokens": 0,
        "per_route": {
            "primary-route": {"max_total_tokens": 7}
        }
    }))
    .expect("route budget");
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                try {
                    await agent("answer", {
                        agent: "default", model: "pinned", fallbackModel: "fallback"
                    });
                } catch (error) {
                    return error.name;
                }
            "#
            .into(),
            args: Value::Null,
            budget: Some(budget),
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(
        run.final_output,
        Some(Value::String("BudgetExceededError".into()))
    );
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "the exhausted fallback ledger must refuse before the fallback provider request"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_fallback_does_not_retry_a_call_rejected_before_dispatch() {
    let (state, storage, provider, topic_id) =
        fallback_test_state(vec![scripted_text_output("fallback answer")]).await;
    let source = r#"return await agent("answer", {
        agent: "default", model: "pinned", fallbackModel: "fallback",
        schema: {type: 17}
    });"#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "failed");
    assert!(
        run.error_message
            .as_deref()
            .is_some_and(|error| error.contains("agent schema is invalid")),
        "the rejection must name the defect it found: {:?}",
        run.error_message
    );
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        0,
        "a call that never dispatched must not issue a fallback provider request"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_fallback_does_not_retry_after_a_tool_only_turn_fails() {
    let (state, _, requests) = crate::turn::tests::make_state_with_capturing_tools(vec![
        vec![
            swage_core::StreamChunk::ToolUse {
                call_id: "echo-1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "work"}),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![swage_core::StreamChunk::Error {
            message: "failed after tool".into(),
        }],
        scripted_text_output("must not be used"),
    ])
    .await;
    state.replace_runtime(
        fallback_test_config(),
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![Arc::new(crate::test_support::NamedTool::new(
            "echo",
        )) as Arc<dyn crate::tool::Tool>]),
    );
    let error = run_ephemeral_agent_call(
        &state,
        Uuid::new_v4(),
        state.topic_store.general_scope().await.expect("topic").id,
        &[],
        &[],
        Uuid::new_v4(),
        AgentCallRequest {
            agent: Some("gateway".into()),
            tools: Some(vec!["echo".into()]),
            model: Some("pinned".into()),
            fallback_model: Some("fallback".into()),
            ..swage_workflow::fixtures::agent_call_request("answer".into())
        },
        None,
        CancellationToken::new(),
    )
    .await
    .expect_err("the tool-only primary failure must not use the fallback");
    assert!(error.to_string().contains("AgentCallError"));
    assert_eq!(
        requests.lock().expect("requests").len(),
        2,
        "the failure after a tool dispatch must not start a fallback turn"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_fallback_retries_a_pre_output_failure_with_tools_configured() {
    let (state, storage, provider, topic_id) = fallback_test_state_with_tools(
        vec![
            vec![swage_core::StreamChunk::Error {
                message: "pinned refused".into(),
            }],
            scripted_text_output("fallback answer"),
        ],
        vec![Arc::new(crate::test_support::NamedTool::new("echo")) as Arc<dyn crate::tool::Tool>],
    )
    .await;
    let source = r#"return await agent("answer", {
        agent: "default", model: "pinned", fallbackModel: "fallback", tools: ["echo"]
    });"#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    assert_eq!(
        storage
            .get_workflow_run(started.id)
            .await
            .expect("run")
            .expect("present")
            .state,
        "completed"
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .expect("requests")
            .iter()
            .map(|request| request.model.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("pinned-wire"), Some("fallback-wire")],
        "configured tools do not count as a dispatched tool"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn held_worker_resolves_and_persists_the_fallback_model_for_continuation() {
    let (state, storage, provider, topic_id) = fallback_test_state(vec![
        vec![swage_core::StreamChunk::Error {
            message: "pinned refused".into(),
        }],
        scripted_text_output("created by fallback"),
        scripted_text_output("continued by fallback"),
    ])
    .await;
    let source = r#"
        const worker = await session({
            agent: "default", model: "pinned", fallbackModel: "fallback"
        });
        await send(worker, "create");
        const result = await send(worker, "continue");
        const reading = await staleness(worker);
        await close(worker);
        return {result, creation: worker.creation, reading};
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;
    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    assert_eq!(
        run.final_output.as_ref().expect("worker output")["reading"]["thresholdMs"],
        222,
        "staleness follows the fallback route that now serves the worker"
    );

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let calls = journal
        .iter()
        .filter(|entry| entry.kind == "agent_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    let open = journal
        .iter()
        .find(|entry| {
            entry.kind == "worker"
                && entry.request.get("action").and_then(Value::as_str) == Some("open")
        })
        .expect("worker open");
    assert_eq!(
        open.result.as_ref().expect("open result")["creation"]["fallback_model"],
        "fallback",
        "the resolved fallback is part of the worker's recorded creation envelope"
    );
    assert!(calls.iter().all(|entry| {
        entry
            .result
            .as_ref()
            .and_then(|result| result["served_model"].as_str())
            == Some("fallback")
    }));
    let session_id = calls[0]
        .result
        .as_ref()
        .and_then(|result| result["session_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("worker session id");
    assert_eq!(
        storage
            .get_session(session_id)
            .await
            .expect("session")
            .expect("worker session")
            .selected_model_id
            .as_deref(),
        Some("fallback")
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .expect("requests")
            .iter()
            .map(|request| request.model.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("pinned-wire"),
            Some("fallback-wire"),
            Some("fallback-wire")
        ],
        "continuation must use the persisted fallback without retrying the failed primary"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn continuation_ignores_declared_fallback_and_keeps_its_creation_model() {
    let (state, storage, provider, topic_id) = fallback_test_state(vec![
        scripted_text_output("created by pinned"),
        vec![swage_core::StreamChunk::Error {
            message: "pinned refused continuation".into(),
        }],
        scripted_text_output("fallback must not run"),
    ])
    .await;
    let source = r#"
        const worker = agentStart("create", { agent: "default", model: "pinned" });
        await worker.result;
        return await agent("continue", {
            agent: "default", session: worker.session, fallbackModel: "fallback"
        });
    "#;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start workflow");
    wait_for_terminal(&state, started.id).await;

    assert_eq!(
        storage
            .get_workflow_run(started.id)
            .await
            .expect("run")
            .expect("present")
            .state,
        "failed"
    );
    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("journal");
    let session_id = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .and_then(|entry| entry.result.as_ref())
        .and_then(|result| result["session_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("worker session id");
    assert_eq!(
        storage
            .get_session(session_id)
            .await
            .expect("session")
            .expect("worker session")
            .selected_model_id
            .as_deref(),
        Some("pinned")
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .expect("requests")
            .iter()
            .map(|request| request.model.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("pinned-wire"), Some("pinned-wire")],
        "the continuation must not run its declared fallback"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_run_shows_worker_liveness_before_first_evidence_and_after_settlement() {
    workflow_journey(async {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let (provider, parked, release) = ParkedFirstDeltaProvider::new();
        let root = tempfile::tempdir().expect("isolated workflow root");
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(provider) as Arc<dyn swage_provider::LlmProvider>,
            workflow_worker_test_config(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        let api = WorkflowTestApi::start(state.clone()).await;
        let dispatch_window_opened_at = chrono::Utc::now();
        let started = api.start_inline(topic.id, StartInlineWorkflowRunRequest {
            source:
                "return await Promise.all([agent('parked worker'), agent('streaming worker')]);"
                    .into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: None,
                max_total_tokens: None,
                reserved_closing_tokens: None,
                max_concurrent_agent_calls: Some(2),
                per_route: Default::default(),
            }),
        }).await;
        let provider_dispatch_at = parked.await.expect("provider park signal");

        let client = reqwest::Client::new();
        let run_url = api.run_url(started.id);
        let live = async {
            loop {
                let detail = client
                    .get(&run_url)
                    .send()
                    .await
                    .expect("request live detail")
                    .error_for_status()
                    .expect("live detail succeeds")
                    .json::<swage_protocol::client::workflow::WorkflowRunDetailResponse>()
                    .await
                    .expect("live detail deserialises");
                let parked = detail
                    .journal
                    .iter()
                    .find(|entry| entry.request["prompt"] == "parked worker")
                    .and_then(|entry| entry.agent_call.as_ref())
                    .and_then(|call| call.liveness.as_ref());
                let streaming = detail
                    .journal
                    .iter()
                    .find(|entry| entry.request["prompt"] == "streaming worker")
                    .and_then(|entry| entry.agent_call.as_ref())
                    .and_then(|call| call.liveness.as_ref());
                if parked.is_some_and(|stamp| stamp.first_evidence_at.is_none())
                    && streaming.is_some_and(|stamp| stamp.first_evidence_at.is_some())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        live.await;

        release.send(()).expect("release parked worker");
        wait_for_run_state(&storage, started.id, "completed").await;
        let settled = client
            .get(&run_url)
            .send()
            .await
            .expect("request settled detail")
            .error_for_status()
            .expect("settled detail succeeds")
            .json::<swage_protocol::client::workflow::WorkflowRunDetailResponse>()
            .await
            .expect("settled detail deserialises");
        let liveness = settled
            .journal
            .iter()
            .filter_map(|entry| {
                entry
                    .agent_call
                    .as_ref()
                    .and_then(|call| call.liveness.as_ref())
            })
            .collect::<Vec<_>>();
        assert_eq!(liveness.len(), 2, "both calls persist liveness");
        assert!(
            liveness
                .iter()
                .all(|stamp| stamp.first_evidence_at.is_some())
        );
        let parked_liveness = settled
            .journal
            .iter()
            .find(|entry| entry.request["prompt"] == "parked worker")
            .and_then(|entry| entry.agent_call.as_ref())
            .and_then(|call| call.liveness.as_ref())
            .expect("parked worker liveness");
        assert!(
            dispatch_window_opened_at <= parked_liveness.started_at
                && parked_liveness.started_at <= provider_dispatch_at,
            "started_at is captured between requesting and entering provider dispatch"
        );
        assert!(
            provider_dispatch_at <= parked_liveness.first_evidence_at.expect("first evidence"),
            "dispatch precedes the first provider evidence"
        );
        assert!(
            state.active_workflow_workers.lock().await.is_empty(),
            "settled success evicts every live overlay"
        );
        let journal = storage
            .list_workflow_journal(started.id)
            .await
            .expect("settled journal");
        for entry in journal.iter().filter(|entry| entry.kind == "agent_call") {
            assert!(
                stored_worker_liveness(&storage, entry.id)
                    .await
                    .is_some_and(|stamp| stamp.first_evidence_at.is_some()),
                "settled success stores its evidence stamps independently of the live overlay"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_run_keeps_liveness_across_live_to_durable_handoff() {
    workflow_journey(async {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let (provider, parked, release) = ParkedFirstDeltaProvider::new();
        let root = tempfile::tempdir().expect("isolated workflow root");
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(provider) as Arc<dyn swage_provider::LlmProvider>,
            workflow_worker_test_config(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        let api = WorkflowTestApi::start(state.clone()).await;
        let started = api
            .start_inline(
                topic.id,
                StartInlineWorkflowRunRequest {
                    source: "return await agent('parked worker');".into(),
                    args: Value::Null,
                    budget: None,
                },
            )
            .await;
        parked.await.expect("provider park signal");

        let pause = workflow_test_hook::install(started.id);
        let url = api.run_url(started.id);
        let detail_task = tokio::spawn(async move {
            reqwest::get(url)
                .await
                .expect("live detail request")
                .error_for_status()
                .expect("live detail succeeds")
                .json::<WorkflowRunDetailResponse>()
                .await
                .expect("live detail deserialises")
        });
        pause
            .entered
            .acquire()
            .await
            .expect("workflow run detail test hook remains open")
            .forget();
        release.send(()).expect("release parked worker");
        wait_for_run_state(&storage, started.id, "completed").await;
        assert!(
            state.active_workflow_workers.lock().await.is_empty(),
            "worker settles and evicts its live overlay while run detail is paused"
        );
        pause.release.add_permits(1);

        let detail = detail_task.await.expect("join live run detail request");
        let call = detail
            .journal
            .iter()
            .find_map(|entry| entry.agent_call.as_ref())
            .expect("agent call detail");
        assert!(
            call.liveness
                .as_ref()
                .is_some_and(|stamp| stamp.first_evidence_at.is_some()),
            "run detail uses durable liveness after the live overlay is evicted"
        );
    })
    .await;
}

async fn stored_worker_liveness(
    storage: &SqliteStorage,
    entry_id: Uuid,
) -> Option<swage_workflow::WorkerLiveness> {
    let metadata = storage
        .with_conn(move |connection| {
            connection.query_row(
                "SELECT terminal_metadata_json FROM workflow_journal WHERE id = ?1",
                [entry_id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
        })
        .await
        .expect("read durable terminal metadata")?;
    let metadata: Value = serde_json::from_str(&metadata).expect("terminal metadata JSON");
    serde_json::from_value(metadata.get("liveness")?.clone()).expect("stored worker liveness shape")
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_worker_persists_liveness_after_live_overlay_is_evicted() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(ScriptedRecordingProvider {
            outputs: std::sync::Mutex::new(std::collections::VecDeque::from([vec![
                swage_core::StreamChunk::Error {
                    message: "provider failed".into(),
                },
            ]])),
            requests: std::sync::Mutex::new(vec![]),
        }),
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "return await agent('failing worker', { maxAttempts: 1 });".into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start failing run");
    wait_for_terminal(&state, started.id).await;

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("failed journal");
    let failed = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("failed agent call");
    assert_eq!(failed.status, "failed");
    let stored_liveness = stored_worker_liveness(&storage, failed.id)
        .await
        .expect("failed call stores dispatch liveness");
    assert_eq!(
        stored_liveness.first_evidence_at, None,
        "a provider failure before output has no evidence stamp"
    );
    assert!(
        !state
            .active_workflow_workers
            .lock()
            .await
            .contains_key(&failed.id),
        "failed call evicts its live overlay"
    );
    let Json(detail) = get_run(State(state), Path(started.id))
        .await
        .expect("failed run detail");
    let detail_liveness = detail
        .journal
        .iter()
        .find(|entry| entry.id == failed.id)
        .and_then(|entry| entry.agent_call.as_ref())
        .and_then(|call| call.liveness.as_ref())
        .expect("failed run detail projects durable dispatch liveness");
    assert_eq!(detail_liveness.first_evidence_at, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_start_is_worker_evidence_before_any_text() {
    let (state, storage, _, topic_id) = fallback_test_state_with_tools(
        vec![
            vec![
                swage_core::StreamChunk::ToolUse {
                    call_id: "echo-1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "work"}),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![swage_core::StreamChunk::Error {
                message: "failed before text".into(),
            }],
        ],
        vec![Arc::new(crate::test_support::NamedTool::new("echo")) as Arc<dyn crate::tool::Tool>],
    )
    .await;
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic_id),
        Json(StartInlineWorkflowRunRequest {
            source:
                r#"return await agent("tool-only worker", { tools: ["echo"], maxAttempts: 1 });"#
                    .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start tool-only run");
    wait_for_terminal(&state, started.id).await;

    let journal = storage
        .list_workflow_journal(started.id)
        .await
        .expect("tool-only journal");
    let failed = journal
        .iter()
        .find(|entry| entry.kind == "agent_call")
        .expect("tool-only agent call");
    assert_eq!(failed.status, "failed");
    assert!(
        stored_worker_liveness(&storage, failed.id)
            .await
            .is_some_and(|stamp| stamp.first_evidence_at.is_some()),
        "ToolStart stamps evidence before any text is emitted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_worker_persists_liveness_after_live_overlay_is_evicted() {
    workflow_journey(async {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let pending = Arc::new(Notify::new());
        let root = tempfile::tempdir().expect("isolated workflow root");
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(PendingWithoutEvidenceProvider {
                pending: Arc::clone(&pending),
            }),
            workflow_worker_test_config(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        let api = WorkflowTestApi::start(state.clone()).await;
        let started = api
            .start_inline(
                topic.id,
                StartInlineWorkflowRunRequest {
                    source: "return await agent('cancelled worker');".into(),
                    args: Value::Null,
                    budget: None,
                },
            )
            .await;
        pending.notified().await;
        api.cancel(started.id).await;
        wait_for_run_state(&storage, started.id, "cancelled").await;

        let journal = storage
            .list_workflow_journal(started.id)
            .await
            .expect("cancelled journal");
        let cancelled = journal
            .iter()
            .find(|entry| entry.kind == "agent_call")
            .expect("cancelled agent call");
        assert_eq!(cancelled.status, "failed");
        let stored_liveness = stored_worker_liveness(&storage, cancelled.id)
            .await
            .expect("cancelled call stores dispatch liveness");
        assert_eq!(
            stored_liveness.first_evidence_at, None,
            "a worker cancelled before output has no evidence stamp"
        );
        assert!(
            !state
                .active_workflow_workers
                .lock()
                .await
                .contains_key(&cancelled.id),
            "cancelled call evicts its live overlay"
        );
        let detail = api.detail(started.id).await;
        let detail_liveness = detail
            .journal
            .iter()
            .find(|entry| entry.id == cancelled.id)
            .and_then(|entry| entry.agent_call.as_ref())
            .and_then(|call| call.liveness.as_ref())
            .expect("cancelled run detail projects durable dispatch liveness");
        assert_eq!(detail_liveness.first_evidence_at, None);
    })
    .await;
}

// Worker fixtures use the test-harness sandbox path, independent of the
// product's sandbox default and its degradation audit.
fn workflow_worker_test_config() -> swage_config::ResolvedConfig {
    let mut config = swage_config::ResolvedConfig::default();
    config.sandbox.enabled = true;
    config
}

/// Completion is event/state driven; this bound only diagnoses a hung journey.
fn workflow_journey(
    journey: impl std::future::Future<Output = ()>,
) -> impl std::future::Future<Output = ()> {
    let journey = Box::pin(journey);
    async move {
        tokio::time::timeout(std::time::Duration::from_secs(180), journey)
            .await
            .expect("workflow journey hung before its required event or state");
    }
}

struct WorkflowTestApi {
    address: std::net::SocketAddr,
    server: tokio::task::JoinHandle<()>,
}

impl WorkflowTestApi {
    async fn start(state: AppState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated workflow gateway");
        let address = listener.local_addr().expect("workflow gateway address");
        let router = crate::server::build_router(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve workflow gateway");
        });
        Self { address, server }
    }

    fn run_url(&self, id: Uuid) -> String {
        format!("http://{}/api/workflow-runs/{id}", self.address)
    }

    async fn start_inline(
        &self,
        topic: Uuid,
        request: StartInlineWorkflowRunRequest,
    ) -> WorkflowRun {
        reqwest::Client::new()
            .post(format!(
                "http://{}/api/topics/{topic}/workflow-runs",
                self.address
            ))
            .json(&request)
            .send()
            .await
            .expect("start workflow over HTTP")
            .error_for_status()
            .expect("workflow start succeeds")
            .json()
            .await
            .expect("workflow start deserialises")
    }

    async fn detail(&self, id: Uuid) -> WorkflowRunDetailResponse {
        reqwest::get(self.run_url(id))
            .await
            .expect("request workflow detail")
            .error_for_status()
            .expect("workflow detail succeeds")
            .json()
            .await
            .expect("workflow detail deserialises")
    }

    async fn cancel(&self, id: Uuid) -> WorkflowRun {
        reqwest::Client::new()
            .post(format!("{}/cancel", self.run_url(id)))
            .send()
            .await
            .expect("request workflow cancellation")
            .error_for_status()
            .expect("workflow cancellation succeeds")
            .json()
            .await
            .expect("workflow cancellation deserialises")
    }
}

impl Drop for WorkflowTestApi {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn wait_for_terminal(state: &AppState, id: Uuid) {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        loop {
            let run = state
                .storage
                .get_workflow_run(id)
                .await
                .expect("run")
                .expect("present");
            if run.state != "running" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal run");
}

async fn wait_for_provider_requests(provider: &ScriptedRecordingProvider, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if provider.requests.lock().expect("requests").len() >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider request");
}

async fn wait_for_worker_turn_interrupted(storage: &SqliteStorage) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let sessions = storage
                .list_sessions_by_agent_id(BARE_WORKFLOW_AGENT_ID)
                .await
                .expect("worker sessions");
            for session in sessions {
                for thread in storage
                    .list_by_session(session.id)
                    .await
                    .expect("worker threads")
                {
                    for turn in storage
                        .list_turns_by_thread(thread.id)
                        .await
                        .expect("worker turns")
                    {
                        match turn.state {
                            TurnState::Interrupted => return,
                            TurnState::Completed | TurnState::Failed => {
                                panic!("cancelled worker turn ended {:?}", turn.state)
                            }
                            _ => {}
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker turn becomes interrupted");
}

async fn recorded_terminal_account(storage: &SqliteStorage, run_id: Uuid) -> Value {
    storage
        .get_workflow_run(run_id)
        .await
        .expect("run")
        .expect("present")
        .terminal_account
        .expect("recorded terminal account")
}

async fn terminal_account_of(storage: &SqliteStorage, run_id: Uuid) -> WorkflowRunTerminalAccount {
    let record = storage
        .get_workflow_run(run_id)
        .await
        .expect("run")
        .expect("present");
    let account = record
        .terminal_account
        .unwrap_or_else(|| panic!("run {run_id} recorded no terminal account"));
    serde_json::from_value(account).expect("terminal account matches the recorded contract")
}

fn scripted_state(
    storage: &Arc<SqliteStorage>,
    outputs: Vec<Vec<swage_core::StreamChunk>>,
) -> (AppState, Arc<ScriptedRecordingProvider>) {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from(outputs)),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(storage) as Arc<dyn Storage>,
        Arc::clone(storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (state, provider)
}

#[tokio::test(flavor = "multi_thread")]
async fn returned_run_accounts_its_workers_and_offers_no_partial_results() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let (state, _provider) = scripted_state(&storage, vec![scripted_text_output("survey reply")]);
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                const survey = await agent("survey the ground", {agent: "default", label: "survey"});
                return { survey };
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let account = terminal_account_of(&storage, started.id).await;
    assert_eq!(account.ending, WorkflowRunEnding::Returned);
    assert_eq!(account.workers.done.len(), 1);
    assert_eq!(account.workers.done[0].label.as_deref(), Some("survey"));
    assert_eq!(
        account.workers.done[0].agent_definition_id.as_deref(),
        Some("default")
    );
    assert!(account.workers.done[0].termination.is_none());
    assert!(account.workers.failed.is_empty());
    assert!(account.workers.cut_short.is_empty());
    assert!(account.workers.unsettled.is_empty());
    assert!(
        account.partial_results.is_none(),
        "a run with a result of its own offers no partial results"
    );

    let Json(detail) = get_run(State(state.clone()), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(
        detail.run.terminal_account.expect("account on run detail"),
        account
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_account_write_failure_prevents_terminal_visibility() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER reject_terminal_account
                 BEFORE UPDATE OF terminal_account_json ON workflow_runs
                 WHEN NEW.terminal_account_json IS NOT NULL
                 BEGIN SELECT RAISE(FAIL, 'injected terminal account failure'); END;",
            )
        })
        .await
        .expect("install terminal account failure trigger");
    let (state, _provider) = scripted_state(&storage, vec![]);
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: "return { settled: true };".into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let record = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(record.state, "interrupted");
    assert!(record.terminal_account.is_none());
    assert!(
        record
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("injected terminal account failure")),
        "{:?}",
        record.error_message
    );

    let Json(detail) = get_run(State(state), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(detail.run.state, WorkflowRunState::Interrupted);
    assert!(detail.run.terminal_account.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_run_surfaces_its_completed_worker_as_a_partial_result() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let (state, _provider) =
        scripted_state(&storage, vec![scripted_text_output("survey findings")]);
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                await agent("survey the ground", {agent: "default", label: "survey"});
                throw new Error("the run failed after the survey");
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let account = terminal_account_of(&storage, started.id).await;
    let WorkflowRunEnding::Threw { error } = &account.ending else {
        panic!("unexpected ending: {:?}", account.ending);
    };
    assert!(error.contains("the run failed after the survey"), "{error}");
    assert_eq!(account.workers.done.len(), 1);
    assert_eq!(account.workers.done[0].label.as_deref(), Some("survey"));
    assert!(account.workers.failed.is_empty());
    let partial = account
        .partial_results
        .expect("a run that never returned offers its completed work");
    assert!(
        partial.reason.contains("failed before returning"),
        "{}",
        partial.reason
    );
    assert_eq!(partial.results.len(), 1);
    assert_eq!(partial.results[0].label.as_deref(), Some("survey"));
    assert_eq!(
        partial.results[0].output,
        Value::String("survey findings".into())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_run_accounts_for_the_worker_that_actually_failed() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let failure = "the survey provider rejected the request";
    let (state, _provider) = scripted_state(
        &storage,
        vec![vec![swage_core::StreamChunk::Error {
            message: failure.into(),
        }]],
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                await agent("survey the ground", {
                    agent: "default",
                    label: "failed survey",
                    maxAttempts: 1,
                });
            "#
            .into(),
            args: Value::Null,
            budget: None,
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let account = terminal_account_of(&storage, started.id).await;
    assert!(matches!(account.ending, WorkflowRunEnding::Threw { .. }));
    assert!(account.workers.done.is_empty());
    assert_eq!(account.workers.failed.len(), 1);
    let failed = &account.workers.failed[0];
    assert_eq!(failed.label.as_deref(), Some("failed survey"));
    assert!(
        matches!(
            &failed.termination,
            Some(WorkflowWorkerTermination::Failed { message }) if message.contains(failure)
        ),
        "{:?}",
        failed.termination
    );
    assert!(account.workers.cut_short.is_empty());
    assert!(account.workers.unsettled.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_threw_under_wind_down_names_the_closing_boundary_as_its_ending() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let (state, _provider) = scripted_state(
        &storage,
        vec![
            scripted_text_output("first segment"),
            scripted_text_output("second segment"),
            scripted_text_output("third segment"),
        ],
    );
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                const burst = [0, 1, 2, 3].map((i) =>
                    agentStart(`work ${i}`, {agent: "default", label: `worker ${i}`})
                );
                await Promise.all(
                    burst.map((handle) => handle.result.catch((error) => error.name))
                );
                throw new Error("stopped inside the closing partition");
            "#
            .into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: None,
                per_route: Default::default(),
                max_total_tokens: Some(14),
                reserved_closing_tokens: Some(8),
                max_concurrent_agent_calls: Some(3),
            }),
        }),
    )
    .await
    .expect("start inline run");
    wait_for_terminal(&state, started.id).await;

    let account = terminal_account_of(&storage, started.id).await;
    let WorkflowRunEnding::WoundDown { error } = &account.ending else {
        panic!("unexpected ending: {:?}", account.ending);
    };
    assert!(
        error.contains("stopped inside the closing partition"),
        "{error}"
    );
    assert_eq!(account.workers.done.len(), 3);
    let partial = account
        .partial_results
        .expect("wind-down leaves recoverable work");
    assert!(partial.reason.contains("wound down"));
    assert_eq!(partial.results.len(), 3);
    let mut outputs = partial
        .results
        .into_iter()
        .map(|result| result.output.as_str().expect("text result").to_string())
        .collect::<Vec<_>>();
    outputs.sort();
    assert_eq!(
        outputs,
        ["first segment", "second segment", "third segment"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_run_reads_its_in_flight_worker_as_cut_short_not_failed() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let (state, provider) = scripted_state(&storage, vec![scripted_text_output("finishing reply")]);
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: r#"
                const first = agentStart("finish this", {agent: "default", label: "finishing", maxAttempts: 1});
                await first.result;
                const second = agentStart("never answered", {agent: "default", label: "hanging", maxAttempts: 1});
                return await second.result;
            "#
            .into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(4),
                per_route: Default::default(),
                max_total_tokens: None,
                reserved_closing_tokens: None,
                max_concurrent_agent_calls: Some(2),
            }),
        }),
    )
    .await
    .expect("start inline run");
    wait_for_provider_requests(&provider, 2).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let journal = storage
                .list_workflow_journal(started.id)
                .await
                .expect("journal");
            if journal
                .iter()
                .any(|entry| entry.kind == "agent_call" && entry.status == "completed")
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the answered worker completes");
    cancel_run(State(state.clone()), Path(started.id))
        .await
        .expect("cancel the run");
    wait_for_terminal(&state, started.id).await;

    let account = terminal_account_of(&storage, started.id).await;
    assert_eq!(account.ending, WorkflowRunEnding::Cancelled);
    assert_eq!(
        account
            .workers
            .done
            .iter()
            .map(|worker| worker.label.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("finishing")]
    );
    assert_eq!(
        account
            .workers
            .cut_short
            .iter()
            .map(|worker| worker.label.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("hanging")],
        "the worker the cancellation caught in flight is cut short, not failed"
    );
    assert!(
        matches!(
            account.workers.cut_short[0].termination,
            Some(WorkflowWorkerTermination::CutShort { .. })
        ),
        "{:?}",
        account.workers.cut_short[0].termination
    );
    assert!(account.workers.failed.is_empty());
    let partial = account
        .partial_results
        .expect("cancelled run's recovered work");
    assert_eq!(partial.results.len(), 1);
    assert_eq!(
        partial.results[0].output,
        Value::String("finishing reply".into())
    );

    // The recorded blob is the wire shape, tag and all: a later reader
    // parses it back through the protocol contract, never by guesswork.
    let recorded = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present")
        .terminal_account
        .expect("recorded account");
    assert_eq!(recorded["ending"]["ending"], "cancelled");
    assert_eq!(
        recorded["workers"]["cut_short"][0]["termination"]["termination"],
        "cut_short"
    );
    assert_eq!(
        recorded["workers"]["cut_short"][0]["termination"]["cause"],
        "run_cancelled"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_the_gateway_could_not_resume_accounts_for_itself_without_settling_anything() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    // A run with neither source nor script version cannot be relaunched:
    // it stands in for the run whose gateway died under it.
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: None,
            source_hash: source_hash(""),
            budget: Value::Null,
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let request =
        serde_json::json!({"prompt": "in flight", "label": "in flight", "agent": "default"});
    storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 0,
            kind: "agent_call".into(),
            args_hash: journal_args_hash(&request),
            request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("in-flight worker");
    storage
        .update_workflow_run_state(run.id, "interrupted", None, None, None)
        .await
        .expect("interrupt the run");
    let (state, _provider) = scripted_state(&storage, vec![]);

    assert_eq!(resume_owed_workflow_runs(&state).await, 0);

    let account = terminal_account_of(&storage, run.id).await;
    let WorkflowRunEnding::InterruptedWithoutResume { reason } = &account.ending else {
        panic!("unexpected ending: {:?}", account.ending);
    };
    assert!(!reason.is_empty());
    assert_eq!(account.workers.unsettled.len(), 1);
    assert_eq!(
        account.workers.unsettled[0].label.as_deref(),
        Some("in flight")
    );
    assert_eq!(
        account.workers.unsettled[0].termination,
        Some(WorkflowWorkerTermination::Unsettled)
    );
    assert!(account.workers.cut_short.is_empty());
    // Nothing was alive to settle this worker, and the account does not
    // pretend otherwise: its journal entry is left exactly as found.
    let entry = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal")
        .into_iter()
        .next()
        .expect("journal entry");
    assert_eq!(entry.status, "started");
    assert_eq!(entry.error_message, None);
    // Resume policy is untouched: the run's own state did not move.
    assert_eq!(
        storage
            .get_workflow_run(run.id)
            .await
            .expect("run")
            .expect("present")
            .state,
        "interrupted"
    );
    // The run stays owed on every later wake pass, and each pass fails to
    // relaunch it again — but a run ends once, so the account it ended
    // with is the account it keeps rather than one re-settled at each
    // tick's instant.
    let first = recorded_terminal_account(&storage, run.id).await;
    assert_eq!(resume_owed_workflow_runs(&state).await, 0);
    assert_eq!(resume_owed_workflow_runs(&state).await, 0);
    assert_eq!(
        recorded_terminal_account(&storage, run.id).await,
        first,
        "a later recovery pass re-settled the account it had already written"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_detail_rejects_a_terminal_account_outside_the_contract() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let (state, _provider) = scripted_state(&storage, vec![]);
    // A run that has recorded no account of its own, so the blob under
    // test is the only one the projection can read.
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: None,
            source_hash: source_hash(""),
            budget: Value::Null,
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    storage
        .set_workflow_run_terminal_account(
            run.id,
            serde_json::json!({"ending": {"ending": "evaporated"}}),
        )
        .await
        .expect("write an out-of-contract account");

    let error = get_run(State(state.clone()), Path(run.id))
        .await
        .expect_err("an out-of-contract account is refused, never projected");
    assert!(
        error
            .to_string()
            .contains("terminal account does not match the recorded contract"),
        "{error}"
    );
}
async fn wait_for_open_worker_round(storage: &SqliteStorage) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let sessions = storage
                .list_sessions_by_agent_id(BARE_WORKFLOW_AGENT_ID)
                .await
                .expect("worker sessions");
            for session in sessions {
                for thread in storage
                    .list_by_session(session.id)
                    .await
                    .expect("worker threads")
                {
                    for turn in storage
                        .list_turns_by_thread(thread.id)
                        .await
                        .expect("worker turns")
                    {
                        if !storage
                            .list_provider_rounds_by_turn(turn.id)
                            .await
                            .expect("provider rounds")
                            .is_empty()
                        {
                            return;
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("open worker provider round");
}

/// The fixture behind the worker-call-record tests: one run, one host
/// loop, and a provider scripted with whatever the case needs.
async fn worker_call_record_fixture(
    provider: Arc<ScriptedRecordingProvider>,
) -> (Arc<SqliteStorage>, AppState, WorkflowRunRecord) {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source_hash: source_hash("return null"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        provider as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (storage, state, run)
}

fn worker_call_request(prompt: &str) -> AgentCallRequest {
    AgentCallRequest {
        agent: Some("default".into()),
        tools: Some(vec!["file_read".into()]),
        effort: Some(swage_protocol::client::settings::Effort::Low),
        ..swage_workflow::fixtures::agent_call_request(prompt.into())
    }
}

fn spawn_worker_call_host(
    state: AppState,
    run: &WorkflowRunRecord,
) -> (
    mpsc::UnboundedSender<HostCommand>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_host_commands(
        state,
        run.id,
        run.topic_id,
        vec![],
        vec![],
        None,
        None,
        WorkflowBudget::default(),
        vec![],
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        CancellationToken::new(),
        PARK_TIMER_THRESHOLD_MS,
        rx,
    ));
    (tx, task)
}

async fn send_worker_call(
    tx: &mpsc::UnboundedSender<HostCommand>,
    request: AgentCallRequest,
    resolved_working_directory: Option<ResolvedWorkerPlacement>,
) -> Result<AgentCallResult, WorkflowHostError> {
    let (reply, receive) = oneshot::channel();
    tx.send(HostCommand::Agent {
        request,
        resolved_working_directory,
        cancellation: CancellationToken::new(),
        reply,
    })
    .expect("send agent call");
    receive.await.expect("agent reply")
}

/// A placement the topic actually authorises: the directory is attached
/// as a trusted workspace, so the worker's turn re-authorises it rather
/// than failing closed on a path nothing grants.
async fn granted_worker_placement(
    storage: &SqliteStorage,
    topic_id: Uuid,
    directory: &std::path::Path,
) -> ResolvedWorkerPlacement {
    let attachment_id = grant_workspace(storage, topic_id, directory).await;
    let path = directory
        .canonicalize()
        .expect("canonical placement path")
        .to_string_lossy()
        .into_owned();
    ResolvedWorkerPlacement {
        path: path.clone(),
        can_write: true,
        authority_path: path,
        authority_context_id: attachment_id,
        worktree_seq: None,
        worktree_base_commit: None,
    }
}

/// A worker turn standing in for one attempt of a call: its own session,
/// thread, and turn, so the provider rounds hang off a real row.
async fn worker_turn(storage: &SqliteStorage, topic_id: Uuid, input: &str) -> Uuid {
    let session = storage
        .create_session(swage_storage::NewSession {
            agent_id: BARE_WORKFLOW_AGENT_ID.into(),
            metadata: Some(serde_json::json!({})),
            ..swage_storage::fixtures::new_session(topic_id)
        })
        .await
        .expect("worker session");
    let thread = swage_storage::store::ThreadStore::create(
        storage,
        swage_storage::NewThread {
            session_id: session.id,
            agent_id: BARE_WORKFLOW_AGENT_ID.into(),
        },
    )
    .await
    .expect("worker thread");
    storage
        .create_turn(swage_storage::NewTurn {
            thread_id: thread.id,
            agent_id: BARE_WORKFLOW_AGENT_ID.into(),
            state: TurnState::Processing,
            user_input: Some(input.into()),
            parent_turn_id: None,
        })
        .await
        .expect("worker turn")
        .id
}

async fn open_round(storage: &SqliteStorage, turn_id: Uuid, round: i64) {
    storage
        .open_provider_round(
            turn_id,
            round,
            serde_json::to_value(
                swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Outstanding,
            )
            .expect("outcome"),
        )
        .await
        .expect("open the round");
}

async fn settle_round(
    storage: &SqliteStorage,
    turn_id: Uuid,
    round: i64,
    outcome: swage_protocol::client::workflow::WorkflowProviderRoundOutcome,
) {
    storage
        .settle_provider_round(
            turn_id,
            round,
            serde_json::to_value(outcome).expect("outcome"),
        )
        .await
        .expect("settle the round");
}

fn attempt_on_turn(turn_id: Uuid) -> swage_protocol::client::workflow::WorkflowAgentCallResolution {
    swage_protocol::client::workflow::WorkflowAgentCallResolution {
        model: Some("scripted-model".into()),
        effort: None,
        session_id: None,
        thread_id: None,
        turn_id: Some(turn_id),
    }
}

fn only_agent_call(
    detail: &WorkflowRunDetailResponse,
) -> &swage_protocol::client::workflow::WorkflowAgentCallDetail {
    detail
        .journal
        .iter()
        .find_map(|entry| entry.agent_call.as_ref())
        .expect("the run detail carries an agent call")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dispatch_record_failure_prevents_the_provider_call() {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([scripted_text_output(
            "must not run",
        )])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let (storage, state, run) = worker_call_record_fixture(Arc::clone(&provider)).await;
    storage
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TEMP TRIGGER reject_worker_dispatch_record
                 BEFORE UPDATE OF dispatch_json ON workflow_journal
                 BEGIN SELECT RAISE(FAIL, 'injected dispatch record failure'); END;",
            )
        })
        .await
        .expect("install dispatch-record failure trigger");
    let (_, entry_id) = journalled_agent_call(&storage, run.topic_id).await;

    let error = run_ephemeral_agent_call(
        &state,
        run.id,
        run.topic_id,
        &[],
        &[],
        entry_id,
        worker_call_request("must be recorded before execution"),
        None,
        CancellationToken::new(),
    )
    .await
    .expect_err("a worker without a dispatch record must not run");

    assert!(
        error
            .to_string()
            .contains("injected dispatch record failure"),
        "the constitutive write failure is returned: {error}"
    );
    assert!(
        provider.requests.lock().expect("requests").is_empty(),
        "no paid provider work begins before its dispatch account exists"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_call_records_what_it_was_dispatched_with_and_what_it_resolved() {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            scripted_text_output_with_usage("worker output", 11, 7),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let (storage, state, run) = worker_call_record_fixture(provider).await;
    // The worker is placed where a real dispatch places one, so the
    // record is read back against a placement that was actually
    // resolved rather than against the absence of one.
    let placement_directory = tempfile::tempdir().expect("placement directory");
    let placement =
        granted_worker_placement(&storage, run.topic_id, placement_directory.path()).await;
    let (tx, task) = spawn_worker_call_host(state.clone(), &run);
    send_worker_call(
        &tx,
        worker_call_request("read the briefing"),
        Some(placement.clone()),
    )
    .await
    .expect("agent call");
    drop(tx);
    task.await.expect("host loop");

    let detail = get_run(State(state), Path(run.id)).await.expect("detail").0;
    let call = only_agent_call(&detail);
    let dispatch = call.dispatch.as_ref().expect("dispatch record");
    assert_eq!(dispatch.prompt, "read the briefing");
    assert_eq!(dispatch.agent_definition_id.as_deref(), Some("default"));
    assert_eq!(
        dispatch.requested_effort,
        Some(swage_protocol::client::settings::Effort::Low)
    );
    assert_eq!(
        dispatch.grants.as_deref(),
        Some(["file_read".to_string()].as_slice())
    );
    assert_eq!(
        dispatch.placement,
        Some(WorkflowWorkerPlacementRecord {
            path: placement.path.clone(),
            can_write: true,
        }),
        "the record carries where the worker was actually placed"
    );
    assert_eq!(
        dispatch.attempts.len(),
        1,
        "one attempt ran, so one is recorded"
    );
    let resolved = dispatch.attempts.last().expect("attempt record");
    assert!(resolved.model.is_some(), "the resolved model is recorded");
    assert_eq!(
        resolved.effort,
        Some(swage_protocol::client::settings::Effort::Low)
    );
    assert_eq!(resolved.turn_id, call.turn_id);
    assert_eq!(
        call.usage,
        WorkflowCallUsage::Known {
            input_tokens: 11,
            output_tokens: 7,
        }
    );
    assert_eq!(call.termination, WorkflowWorkerTermination::Settled);

    let rounds = call.provider_rounds.as_ref().expect("provider rounds");
    assert_eq!(rounds.len(), 1, "one provider request went out");
    assert!(
        rounds[0].completed_at.is_some(),
        "the round that came back is settled, not left outstanding"
    );
    assert_eq!(
        rounds[0].outcome,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Responded { tool_calls: 0 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_provider_failure_records_a_settled_failed_round() {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([vec![
            swage_core::StreamChunk::Error {
                message: "scripted provider failure".into(),
            },
        ]])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let (_storage, state, run) = worker_call_record_fixture(provider).await;
    let (tx, task) = spawn_worker_call_host(state.clone(), &run);
    let error = send_worker_call(&tx, worker_call_request("exercise provider failure"), None)
        .await
        .expect_err("the provider failure must fail the worker call");
    assert!(error.to_string().contains("scripted provider failure"));
    drop(tx);
    task.await.expect("host loop");

    let detail = get_run(State(state), Path(run.id)).await.expect("detail").0;
    let call = only_agent_call(&detail);
    assert_eq!(
        call.termination,
        WorkflowWorkerTermination::Failed {
            message: error.to_string(),
        }
    );
    let rounds = call.provider_rounds.as_ref().expect("provider rounds");
    assert_eq!(rounds.len(), 1, "one failed provider request was made");
    assert!(
        rounds[0].completed_at.is_some(),
        "the failed provider request must be settled"
    );
    assert_eq!(
        rounds[0].outcome,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Failed {
            message: "scripted provider failure".into(),
        }
    );
}

/// One journal entry, two turns: the attempt the run's end interrupted
/// and the attempt replay re-dispatched. The record has to carry both
/// attempts' rounds — reading only the settled turn hides everything the
/// interrupted attempt actually asked the provider for.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_dispatched_call_keeps_the_interrupted_attempts_rounds() {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let (storage, state, run) = worker_call_record_fixture(provider).await;
    let entry = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "agent_call".into(),
            args_hash: source_hash("interrupted then resumed"),
            request: serde_json::to_value(worker_call_request("survey the tree")).expect("request"),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("journal intent");
    storage
        .record_workflow_call_dispatch(
            entry.id,
            serde_json::to_value(WorkflowAgentCallDispatch {
                prompt: "survey the tree".into(),
                agent_definition_id: None,
                requested_model: None,
                requested_effort: None,
                placement: None,
                grants: None,
                attempts: Vec::new(),
            })
            .expect("dispatch record"),
        )
        .await
        .expect("dispatch record");

    // The interrupted attempt: one round that came back with a tool call,
    // and a second still out when the gateway went away.
    let interrupted = worker_turn(&storage, run.topic_id, "interrupted attempt").await;
    open_round(&storage, interrupted, 1).await;
    settle_round(
        &storage,
        interrupted,
        1,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Responded { tool_calls: 1 },
    )
    .await;
    open_round(&storage, interrupted, 2).await;
    record_agent_call_attempt(
        storage.as_ref(),
        entry.id,
        serde_json::to_value(attempt_on_turn(interrupted)).expect("attempt"),
    )
    .await
    .expect("first attempt");

    // Replay re-dispatches the same entry under a fresh turn.
    let resumed = worker_turn(&storage, run.topic_id, "resumed attempt").await;
    let interruption_not_before = chrono::Utc::now();
    record_agent_call_attempt(
        storage.as_ref(),
        entry.id,
        serde_json::to_value(attempt_on_turn(resumed)).expect("attempt"),
    )
    .await
    .expect("second attempt");
    open_round(&storage, resumed, 1).await;
    settle_round(
        &storage,
        resumed,
        1,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Responded { tool_calls: 0 },
    )
    .await;
    storage
        .complete_workflow_journal_entry(
            entry.id,
            serde_json::json!({
                "turn_id": resumed,
                "usage": { "availability": "known", "input_tokens": 5, "output_tokens": 2 },
            }),
        )
        .await
        .expect("settle the resumed call");

    let detail = get_run(State(state), Path(run.id)).await.expect("detail").0;
    let call = only_agent_call(&detail);
    let attempts = &call.dispatch.as_ref().expect("dispatch record").attempts;
    assert_eq!(
        attempts
            .iter()
            .map(|attempt| attempt.turn_id)
            .collect::<Vec<_>>(),
        vec![Some(interrupted), Some(resumed)],
        "both attempts stay on the entry, oldest first"
    );

    let rounds = call.provider_rounds.as_ref().expect("provider rounds");
    assert_eq!(
        rounds
            .iter()
            .map(|round| (round.attempt, round.round))
            .collect::<Vec<_>>(),
        vec![(1, 1), (1, 2), (2, 1)],
        "every attempt's rounds are projected, ordered by attempt then round"
    );
    assert_eq!(
        rounds[0].outcome,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Responded { tool_calls: 1 },
        "the interrupted attempt's settled round keeps what came back"
    );
    assert_eq!(
        rounds[1].outcome,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Interrupted,
        "the round the interruption caught reads interrupted, not still outstanding"
    );
    let interrupted_end = rounds[1]
        .completed_at
        .expect("the interrupted round ends when the re-dispatch made it unanswerable");
    assert!(
        interrupted_end >= interruption_not_before,
        "the interrupted round's end is the moment the interruption landed, \
         not the earlier request start"
    );
    assert_eq!(
        rounds[2].outcome,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Responded { tool_calls: 0 },
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_whose_provider_reports_no_usage_reads_unavailable_not_zero() {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([vec![
            swage_core::StreamChunk::Delta {
                content: "worker output".into(),
            },
            swage_core::StreamChunk::Done {
                finish_reason: None,
            },
        ]])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let (_storage, state, run) = worker_call_record_fixture(provider).await;
    let (tx, task) = spawn_worker_call_host(state.clone(), &run);
    send_worker_call(
        &tx,
        worker_call_request("work without reported usage"),
        None,
    )
    .await
    .expect("agent call");
    drop(tx);
    task.await.expect("host loop");

    let detail = get_run(State(state), Path(run.id)).await.expect("detail").0;
    assert_eq!(
        only_agent_call(&detail).usage,
        WorkflowCallUsage::Unavailable {
            reason: WorkflowUsageUnavailableReason::ProviderReportedNoUsage,
        },
        "a call the provider reported no usage for is unavailable, never zero spend"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_usage_closes_the_token_gate_and_marks_budget_readings_incomplete() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            vec![
                swage_core::StreamChunk::Delta {
                    content: "first answer".into(),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            scripted_text_output("illegally admitted second answer"),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        const first = await agent("provider omits usage", {agent: "default"});
        let refusal = null;
        try {
            await agent("must not bypass the token gate", {agent: "default"});
        } catch (error) {
            refusal = String(error);
        }
        return {first, refusal, snapshot: await budgetSnapshot()};
    "#;

    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(WorkflowBudget {
                max_agent_calls: Some(3),
                per_route: Default::default(),
                max_total_tokens: Some(12),
                reserved_closing_tokens: None,
                max_concurrent_agent_calls: Some(1),
            }),
        }),
    )
    .await
    .expect("start run");
    wait_for_terminal(&state, started.id).await;

    let run = storage
        .get_workflow_run(started.id)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(run.state, "completed", "run error: {:?}", run.error_message);
    let output = run.final_output.expect("workflow output");
    assert_eq!(output["first"], "first answer");
    assert!(
        output["refusal"]
            .as_str()
            .is_some_and(|error| error.contains("BudgetUsageUnavailableError")),
        "the next call is refused because the prior spend is unknown: {output}"
    );
    assert_eq!(output["snapshot"]["spent"], 0);
    assert_eq!(output["snapshot"]["remaining"], Value::Null);
    assert_eq!(output["snapshot"]["working_remaining"], Value::Null);
    assert_eq!(output["snapshot"]["closing_remaining"], Value::Null);
    assert_eq!(output["snapshot"]["usage_unavailable_calls"], 1);
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        1,
        "unknown spend must not admit another provider call"
    );

    let detail = get_run(State(state), Path(started.id))
        .await
        .expect("detail")
        .0;
    let terminal = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::TerminalBudget)
        .and_then(|entry| entry.result.as_ref())
        .expect("terminal budget reading");
    assert!(matches!(
        terminal,
        WorkflowJournalResult::TerminalBudget {
            spent: 0,
            remaining: None,
            closing_remaining: None,
            usage_unavailable_calls: 1,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_usage_closes_only_its_configured_route() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::from([
            vec![
                swage_core::StreamChunk::Delta {
                    content: "usage unavailable".into(),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            scripted_text_output_with_usage("other route", 1, 0),
            scripted_text_output_with_usage("fallback ledger", 1, 0),
        ])),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let mut config = fallback_test_config();
    config.models.entries.insert(
        "plain".into(),
        swage_config::ModelEndpointConfig {
            provider_id: "unconfigured-route".into(),
            model_id: "plain-wire".into(),
            ..Default::default()
        },
    );
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::clone(&provider) as Arc<dyn swage_provider::LlmProvider>,
        crate::test_support::with_mock_provider_entries(config),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let source = r#"
        const unavailable = await agent("provider omits usage", {
            agent: "default", model: "pinned", maxAttempts: 1
        });
        let refusal = null;
        try {
            await agent("same route must close", {
                agent: "default", model: "pinned", maxAttempts: 1
            });
        } catch (error) {
            refusal = String(error);
        }
        const other = await agent("another configured route", {
            agent: "default", model: "fallback", maxAttempts: 1
        });
        const fallback = await agent("the fallback ledger", {
            agent: "default", model: "plain", maxAttempts: 1
        });
        return {unavailable, refusal, other, fallback, snapshot: await budgetSnapshot()};
    "#;
    let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({
        "max_agent_calls": 5,
        "max_concurrent_agent_calls": 1,
        "max_total_tokens": 10,
        "reserved_closing_tokens": 2,
        "per_route": {
            "primary-route": {
                "max_total_tokens": 10,
                "reserved_closing_tokens": 2
            },
            "fallback-route": {
                "max_total_tokens": 10,
                "reserved_closing_tokens": 2
            }
        }
    }))
    .expect("route budget");
    let Json(started) = start_inline_run(
        State(state.clone()),
        Path(topic.id),
        Json(StartInlineWorkflowRunRequest {
            source: source.into(),
            args: Value::Null,
            budget: Some(budget),
        }),
    )
    .await
    .expect("start run");
    wait_for_terminal(&state, started.id).await;

    let Json(detail) = get_run(State(state), Path(started.id))
        .await
        .expect("run detail");
    assert_eq!(detail.run.state, WorkflowRunState::Completed);
    let output = detail.run.final_output.expect("workflow output");
    assert_eq!(output["unavailable"], "usage unavailable");
    assert!(
        output["refusal"]
            .as_str()
            .is_some_and(|error| error.contains("BudgetUsageUnavailableError")),
        "the route with unknown spend must refuse its next call: {output}"
    );
    assert_eq!(output["other"], "other route");
    assert_eq!(output["fallback"], "fallback ledger");
    assert_eq!(output["snapshot"]["spent"], 1);
    assert_eq!(output["snapshot"]["remaining"], 9);
    assert_eq!(output["snapshot"]["closing_remaining"], 2);
    assert_eq!(output["snapshot"]["usage_unavailable_calls"], 0);
    let routes = &output["snapshot"]["per_route"];
    assert_eq!(routes["primary-route"]["spent"], 0);
    assert_eq!(routes["primary-route"]["remaining"], Value::Null);
    assert_eq!(routes["primary-route"]["closing_remaining"], Value::Null);
    assert_eq!(routes["primary-route"]["usage_unavailable_calls"], 1);
    assert_eq!(routes["fallback-route"]["spent"], 1);
    assert_eq!(routes["fallback-route"]["remaining"], 9);
    assert_eq!(routes["fallback-route"]["closing_remaining"], 2);
    assert_eq!(routes["fallback-route"]["usage_unavailable_calls"], 0);
    assert_eq!(
        provider.requests.lock().expect("requests").len(),
        3,
        "only the unavailable route's second call is refused"
    );

    let terminal = detail
        .journal
        .iter()
        .find(|entry| entry.kind == WorkflowJournalEntryKind::TerminalBudget)
        .and_then(|entry| entry.result.as_ref())
        .expect("terminal budget reading");
    let terminal = serde_json::to_value(terminal).expect("terminal budget serialises");
    assert_eq!(terminal["spent"], 1);
    assert_eq!(terminal["remaining"], 9);
    assert_eq!(terminal["closing_remaining"], 2);
    assert_eq!(terminal["usage_unavailable_calls"], 0);
    assert_eq!(terminal["per_route"], *routes);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_the_run_cut_short_reads_cut_short_not_failed() {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let (storage, state, run) = worker_call_record_fixture(provider).await;
    // The entry stands for a worker still in flight when the run ended.
    let request = serde_json::to_value(worker_call_request("still working")).expect("request");
    let intent = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "agent_call".into(),
            args_hash: source_hash("still working"),
            request,
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("journal intent");
    // Cancel through the operator's own route rather than reaching for
    // the settling helper: a parked run is the shape where a worker is
    // genuinely still outstanding when the run is ended from outside.
    storage
        .update_workflow_run_state(run.id, "parked", None, None, None)
        .await
        .expect("park the run");
    let cancelled = cancel_run(State(state.clone()), Path(run.id))
        .await
        .expect("cancel the run")
        .0;
    assert_eq!(cancelled.state, WorkflowRunState::Cancelled);

    let settled = storage
        .list_workflow_journal(run.id)
        .await
        .expect("journal")
        .into_iter()
        .find(|entry| entry.id == intent.id)
        .expect("settled entry");
    assert_eq!(
        settled.status, "failed",
        "the stored status stays terminal so replay is unchanged"
    );

    let detail = get_run(State(state), Path(run.id)).await.expect("detail").0;
    let entry = detail
        .journal
        .iter()
        .find(|entry| entry.id == intent.id)
        .expect("wire entry");
    assert_eq!(
        entry.status,
        swage_protocol::client::workflow::WorkflowJournalEntryStatus::CutShort,
        "the run's own end is not the worker's failure"
    );
    assert_eq!(
        only_agent_call(&detail).termination,
        WorkflowWorkerTermination::CutShort {
            cause: WorkflowCutShortCause::RunCancelled,
            detail: Some(CANCELLED_JOURNAL_SETTLEMENT.into()),
        }
    );
    assert_eq!(
        only_agent_call(&detail).usage,
        WorkflowCallUsage::Unavailable {
            reason: WorkflowUsageUnavailableReason::CallDidNotSettle,
        }
    );
}

/// Opens the provider round and then blocks on a gate the test holds, so
/// the worker is demonstrably mid-round — a request went out and no
/// answer can arrive — when the run is cancelled underneath it.
struct GatedRoundProvider {
    round_opened: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl swage_provider::LlmProvider for GatedRoundProvider {
    fn model_id(&self) -> &str {
        "gated-round-model"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("workflow calls use streaming")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        let release = Arc::clone(&self.release);
        self.round_opened.notify_one();
        Box::pin(futures::stream::once(async move {
            release.notified().await;
            swage_core::StreamChunk::Done {
                finish_reason: None,
            }
        }))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_cancelled_mid_round_reads_cut_short_not_failed() {
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let topic = storage.general_scope().await.expect("topic");
    let source = r#"return await agent("work the run interrupts", {maxAttempts: 1});"#;
    let run = storage
        .create_workflow_run(NewWorkflowRun {
            source: Some(source.into()),
            source_hash: source_hash(source),
            budget: serde_json::to_value(WorkflowBudget {
                max_agent_calls: Some(1),
                per_route: Default::default(),
                max_total_tokens: Some(10),
                reserved_closing_tokens: Some(4),
                max_concurrent_agent_calls: Some(1),
            })
            .expect("budget"),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        })
        .await
        .expect("run");
    let round_opened = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(GatedRoundProvider {
            round_opened: Arc::clone(&round_opened),
            release: Arc::clone(&release),
        }) as Arc<dyn swage_provider::LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    launch_existing_run(state.clone(), run.clone(), source.into(), None)
        .await
        .expect("launch the run");
    // Cancel a worker that is genuinely running: the gate holds its
    // round open, so this is the live path rather than a run parked with
    // an outstanding entry nobody is working on.
    round_opened.notified().await;
    wait_for_open_worker_round(&storage).await;
    let cancellation_not_before = chrono::Utc::now();
    cancel_run(State(state.clone()), Path(run.id))
        .await
        .expect("cancel the running run");
    wait_for_terminal(&state, run.id).await;
    wait_for_worker_turn_interrupted(&storage).await;

    let detail = get_run(State(state), Path(run.id)).await.expect("detail").0;
    let entry = detail
        .journal
        .iter()
        .find(|entry| entry.agent_call.is_some())
        .expect("the cut-short worker's entry");
    assert_eq!(
        entry.status,
        swage_protocol::client::workflow::WorkflowJournalEntryStatus::CutShort,
        "the run's own end is not the worker's failure"
    );
    let call = only_agent_call(&detail);
    assert_eq!(
        call.termination,
        WorkflowWorkerTermination::CutShort {
            cause: WorkflowCutShortCause::RunCancelled,
            detail: Some(CANCELLED_JOURNAL_SETTLEMENT.into()),
        }
    );
    assert_eq!(
        call.usage,
        WorkflowCallUsage::Unavailable {
            reason: WorkflowUsageUnavailableReason::CallDidNotSettle,
        },
        "a round that never came back reports no spend it cannot know"
    );
    assert_eq!(
        call.dispatch
            .as_ref()
            .expect("the cut-short worker still left its dispatch record")
            .prompt,
        "work the run interrupts",
        "a worker cut short still accounts for what it was asked to do"
    );
    let rounds = call.provider_rounds.as_ref().expect("provider rounds");
    let caught = rounds
        .last()
        .expect("the round the cancel caught mid-flight");
    assert_eq!(
        caught.outcome,
        swage_protocol::client::workflow::WorkflowProviderRoundOutcome::Interrupted,
        "the run's end reaches the request it had out: nothing can answer it any more"
    );
    let caught_end = caught
        .completed_at
        .expect("the round the run's end caught still ends, at the moment it was cut short");
    assert!(
        caught_end >= cancellation_not_before,
        "the cut-short round's end is when the run ended, not the earlier request start"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_usage_record_outside_the_contract_is_rejected_not_passed_through() {
    let provider = Arc::new(ScriptedRecordingProvider {
        outputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let (storage, state, run) = worker_call_record_fixture(provider).await;
    let intent = storage
        .create_workflow_journal_intent(NewWorkflowJournalIntent {
            run_id: run.id,
            seq: 1,
            kind: "agent_call".into(),
            args_hash: source_hash("out of contract"),
            request: serde_json::json!({}),
            effect_kind: None,
            idempotency_key: None,
            target_kind: None,
        })
        .await
        .expect("journal intent");
    storage
        .complete_workflow_journal_entry(
            intent.id,
            serde_json::json!({ "usage": { "input_tokens": 3 } }),
        )
        .await
        .expect("stored blob");

    let error = get_run(State(state), Path(run.id))
        .await
        .expect_err("a usage record outside the contract must not pass through untyped");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside WorkflowCallUsage"),
        "error names the contract that was broken: {message}"
    );
    assert!(
        message.contains(&intent.id.to_string()),
        "error names the offending record: {message}"
    );
}
