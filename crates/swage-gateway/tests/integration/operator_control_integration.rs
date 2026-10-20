#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    ApprovalTimeoutPolicy, CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{
    channel_gateway::{ChannelOrigin, DeliveryTarget},
    device::{RegisterDeviceRequest, register_remote_device},
    schedule::create_schedule,
    secret::CreateSecretRequest,
    server::build_router,
    state::AppState,
};
use swage_protocol::client::schedule::{
    CreateScheduleRequest, ScheduleCreator, ScheduleSpec, SessionPolicy,
};
use swage_provider::LlmProvider;
use swage_storage::{
    ApprovalKind, NewApprovalRequest, NewDeliveryRecord, NewTurn, SqliteStorage, TurnState,
    store::{Storage, TopicStore},
};

use crate::support::ProcessEnvGuard;

fn with_secret_env() -> ProcessEnvGuard {
    let mut guard = ProcessEnvGuard::new();
    guard.set_managed_secret_key(17);
    guard
}

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "operator-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("operator tests use HTTP control paths only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("operator tests use HTTP control paths only")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(vec![StreamChunk::Done {
            finish_reason: None,
        }]))
    }
}

fn test_state() -> AppState {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    AppState::for_test(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    )
}

async fn json_response(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    }
}

async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

async fn post_json(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

async fn request_json(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

async fn create_conversation(router: &Router) -> Uuid {
    let (status, body) = post_json(router, "/api/conversations", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_definition_http_crud_records_every_mutation() {
    let state = test_state();
    let router = build_router(state.clone());
    let create = json!({
        "id": "researcher",
        "topic_id": null,
        "display_name": "Researcher",
        "description": "Finds evidence",
        "instructions": "Gather and cite evidence.",
        "model_id": null,
        "tool_permissions": {"allow": ["knowledge_search"], "deny": [], "bundles": {}},
        "skills": ["source-checking"],
        "output_schema": {"type": "object", "required": ["finding"]},
        "output_max_retries": 2
    });

    let (status, created) = post_json(&router, "/api/operator/agent-definitions", create).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["agent_id"], "researcher");

    let (status, listed) = get_json(&router, "/api/operator/agent-definitions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["definitions"][0]["id"], "researcher");
    assert_eq!(listed["definitions"][0]["skills"][0], "source-checking");
    assert_eq!(listed["definitions"][0]["output_max_retries"], 2);

    let update = json!({
        "topic_id": null,
        "display_name": "Research Lead",
        "description": "Finds decisive evidence",
        "instructions": "Gather, verify, and cite evidence.",
        "model_id": null,
        "tool_permissions": null,
        "skills": ["source-checking"],
        "output_schema": {"type": "object", "required": ["finding", "source"]},
        "output_max_retries": 1
    });
    let (status, updated) = request_json(
        &router,
        "PUT",
        "/api/operator/agent-definitions/researcher",
        Some(update),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");

    // Every mutation leaves a configuration_change audit event naming what
    // changed and what it replaced — the whole prior and new state.
    let audits = state.storage.list_audit_events().await.unwrap();
    let changes: Vec<_> = audits
        .iter()
        .filter(|event| {
            event.event_kind == "configuration_change" && event.entity_id == "researcher@global"
        })
        .collect();
    assert_eq!(changes.len(), 2, "create and update each leave a record");
    let update_event = changes
        .iter()
        .find(|event| {
            event.metadata["action"] == "upsert_agent_definition"
                && !event.metadata["prior_state"].is_null()
        })
        .expect("the update's record carries the state it replaced");
    assert_eq!(
        update_event.metadata["prior_state"]["display_name"],
        "Researcher"
    );
    assert_eq!(
        update_event.metadata["new_state"]["display_name"],
        "Research Lead"
    );
    assert_eq!(update_event.metadata["source"], "operator_http");

    let (status, deleted) = request_json(
        &router,
        "DELETE",
        "/api/operator/agent-definitions/researcher",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let (status, listed) = get_json(&router, "/api/operator/agent-definitions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["definitions"], json!([]));

    let audits = state.storage.list_audit_events().await.unwrap();
    let delete_event = audits
        .iter()
        .find(|event| {
            event.event_kind == "configuration_change"
                && event.metadata["action"] == "delete_agent_definition"
        })
        .expect("the delete leaves a record");
    assert_eq!(
        delete_event.metadata["prior_state"]["display_name"],
        "Research Lead"
    );
    assert!(delete_event.metadata["new_state"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn effective_agent_definition_endpoint_selects_the_requested_topic() {
    let router = build_router(test_state());
    let (_, current) = post_json(&router, "/api/topics", json!({ "name": "Current" })).await;
    let (_, other) = post_json(&router, "/api/topics", json!({ "name": "Other" })).await;
    let current_id = current["id"].as_str().unwrap();
    let other_id = other["id"].as_str().unwrap();

    for definition in [
        json!({
            "id": "shared", "topic_id": null, "display_name": "Global shared",
            "description": "", "instructions": "global", "model_id": null,
            "tool_permissions": null, "skills": [], "output_schema": null,
            "output_max_retries": null
        }),
        json!({
            "id": "shared", "topic_id": current_id, "display_name": "Topic shared",
            "description": "", "instructions": "topic winner", "model_id": null,
            "tool_permissions": null, "skills": [], "output_schema": null,
            "output_max_retries": null
        }),
        json!({
            "id": "foreign", "topic_id": other_id, "display_name": "Foreign",
            "description": "", "instructions": "other topic", "model_id": null,
            "tool_permissions": null, "skills": [], "output_schema": null,
            "output_max_retries": null
        }),
        json!({
            "id": "researcher", "topic_id": null, "display_name": "Authored researcher",
            "description": "", "instructions": "authored winner", "model_id": null,
            "tool_permissions": null, "skills": [], "output_schema": null,
            "output_max_retries": null
        }),
    ] {
        let (status, body) =
            post_json(&router, "/api/operator/agent-definitions", definition).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let (status, body) = get_json(
        &router,
        &format!("/api/topics/{current_id}/agent-definitions"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let definitions = body["definitions"].as_array().unwrap();
    assert_eq!(definitions.len(), 4, "effective winners: {body}");
    assert_eq!(definitions[0]["id"], "gateway");
    assert_eq!(definitions[0]["source_scope"], "built_in");
    assert_eq!(definitions[1]["id"], "memory-curator");
    assert_eq!(definitions[1]["source_scope"], "built_in");
    assert_eq!(definitions[1]["selectable"], false);
    assert_eq!(definitions[2]["id"], "researcher");
    assert_eq!(definitions[2]["instructions"], "authored winner");
    assert_eq!(definitions[2]["source_scope"], "global");
    assert_eq!(definitions[3]["id"], "shared");
    assert_eq!(definitions[3]["instructions"], "topic winner");
    assert_eq!(definitions[3]["source_scope"], "topic");
    assert!(
        definitions
            .iter()
            .all(|definition| definition["id"] != "foreign")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_overview_lists_sessions_jobs_approvals_channels_and_devices() {
    let _guard = with_secret_env();
    let state = test_state();
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    state.active_connections.lock().await.insert(session_id);
    state
        .active_turns
        .lock()
        .await
        .insert(session_id, CancellationToken::new());

    create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Nightly sweep".into(),
            spec: ScheduleSpec::Interval {
                every: "every 5 minutes".into(),
            },
            session: SessionPolicy::Continuation { session_id },
            routing: None,
            gate: None,
        },
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    let thread_id = state.storage.list_by_session(session_id).await.unwrap()[0].id;
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("deploy".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: Some(json!({"cmd": "deploy"})),
            hook_name: None,
            prompt: "Allow deploy".into(),
            context: None,
            timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .await
        .unwrap();

    register_remote_device(
        &state,
        RegisterDeviceRequest {
            device_id: "docker-a".into(),
            label: Some("Docker A".into()),
            kind: "docker".into(),
            shared_token_secret_ref: state
                .secret_manager
                .create_secret(CreateSecretRequest {
                    name: "Operator device token".into(),
                    material_kind: "device_credential".into(),
                    owner_kind: "service".into(),
                    backend_kind: "managed".into(),
                    scope_kind: "gateway".into(),
                    scope_id: None,
                    metadata: None,
                    value: Some("secret".into()),
                    external_reference: None,
                })
                .await
                .unwrap()
                .reference,
            workspace_root: "/srv/docker".into(),
            workspace_roots: vec![],
        },
    )
    .await
    .unwrap();
    swage_gateway::device::disconnect_device(&state, "docker-a").await;

    state.channel_routes.lock().await.insert(
        ChannelOrigin {
            integration: swage_gateway::channel_gateway::ExternalChannelIntegration::DevDm,
            account: "ops".into(),
            conversation: "alice".into(),
        },
        session_id,
    );
    state
        .storage
        .create_delivery_record(NewDeliveryRecord {
            session_id: Some(session_id),
            run_id: None,
            turn_id: None,
            target: serde_json::to_value(DeliveryTarget::Channel {
                integration: swage_gateway::channel_gateway::ExternalChannelIntegration::DevDm,
                account: "ops".into(),
                conversation: "alice".into(),
            })
            .unwrap(),
            content: "Delivered".into(),
            status: "sent".into(),
            error: None,
        })
        .await
        .unwrap();

    let (status, body) = get_json(&router, "/api/operator/overview").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["gateway"]["health"], "degraded");
    assert_eq!(body["sessions"][0]["id"], session_id.to_string());
    assert_eq!(body["sessions"][0]["has_active_connection"], true);
    assert_eq!(body["sessions"][0]["has_running_turn"], true);
    assert_eq!(body["approvals"][0]["tool_name"], "shell");
    assert_eq!(
        body["schedules"]["schedules"][0]["message"],
        "Nightly sweep"
    );
    assert_eq!(body["channels"]["channels"][0]["integration"], "dev_dm");
    assert_eq!(body["channels"]["channels"][0]["status"], "active");
    let docker_device = body["devices"]
        .as_array()
        .and_then(|devices| {
            devices
                .iter()
                .find(|device| device["device_id"] == json!("docker-a"))
        })
        .expect("expected docker-a device in operator overview");
    assert_eq!(docker_device["state"], "disconnected");

    let (channel_status, channel_body) = get_json(&router, "/api/channels").await;
    assert_eq!(channel_status, StatusCode::OK);
    assert_eq!(channel_body["channels"][0]["integration"], "dev_dm");
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_controls_manage_approvals_and_schedules_without_full_restart() {
    let state = test_state();
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let scheduled = create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Digest".into(),
            spec: ScheduleSpec::Interval {
                every: "every 10 minutes".into(),
            },
            session: SessionPolicy::Continuation { session_id },
            routing: None,
            gate: None,
        },
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    let thread_id = state.storage.list_by_session(session_id).await.unwrap()[0].id;
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("danger".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let approval = state
        .storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: Some(json!({"cmd": "danger"})),
            hook_name: None,
            prompt: "Allow danger".into(),
            context: None,
            timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .await
        .unwrap();
    // A decision settles only a gate that is still waiting for it.
    let (waiter, decision) = tokio::sync::oneshot::channel();
    state
        .approval_channels
        .lock()
        .unwrap()
        .insert(approval.id, waiter.into());

    let (pause_status, pause_body) = post_json(
        &router,
        &format!("/api/schedules/{}/pause", scheduled.id),
        json!({}),
    )
    .await;
    assert_eq!(pause_status, StatusCode::OK);
    assert_eq!(pause_body["status"], "paused");

    let (resume_status, resume_body) = post_json(
        &router,
        &format!("/api/schedules/{}/resume", scheduled.id),
        json!({}),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK);
    assert_eq!(resume_body["status"], "active");

    let (reject_status, reject_body) = post_json(
        &router,
        &format!("/api/approvals/{}", approval.id),
        json!({
            "decision": "rejected",
            "reason": "operator review"
        }),
    )
    .await;
    assert_eq!(reject_status, StatusCode::OK);
    assert_eq!(reject_body["decision"], "rejected");
    assert_eq!(
        decision.await.unwrap().decision,
        swage_agent::approval::ApprovalDecisionValue::Rejected
    );
}

async fn operator_child(
    state: &AppState,
    origin: swage_storage::SessionOrigin,
    turn_state: TurnState,
) -> swage_storage::Turn {
    let topic = state.topic_store.general_scope().await.unwrap();
    let session = state
        .storage
        .create_session(swage_storage::NewSession {
            topic_id: topic.id,
            agent_id: "gateway".into(),
            origin,
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .unwrap();
    let thread = state
        .storage
        .create(swage_storage::NewThread {
            session_id: session.id,
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();
    state
        .storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".into(),
            state: turn_state,
            user_input: Some("Work".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_counts_processing_detached_children_without_a_foreground_token() {
    let state = test_state();
    let router = build_router(state.clone());
    let parent = create_conversation(&router).await;
    let child = operator_child(
        &state,
        swage_storage::SessionOrigin::Delegation {
            parent_session_id: parent,
        },
        TurnState::Processing,
    )
    .await;
    let child_session = state
        .storage
        .get(child.thread_id)
        .await
        .unwrap()
        .unwrap()
        .session_id;
    let (_, body) = get_json(&router, "/api/operator/overview").await;
    let row = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == child_session.to_string())
        .unwrap();
    assert_eq!(row["has_running_turn"], true, "{body}");
    assert_eq!(body["gateway"]["active_turn_count"], 1, "{body}");
    state
        .storage
        .update_turn_state(child.id, TurnState::Completed)
        .await
        .unwrap();
    let (_, settled) = get_json(&router, "/api/operator/overview").await;
    assert_eq!(settled["gateway"]["active_turn_count"], 0);
}

async fn operator_run(state: &AppState, parent: Option<Uuid>) -> swage_storage::WorkflowRunRecord {
    state
        .storage
        .create_workflow_run(swage_storage::NewWorkflowRun {
            topic_id: state.topic_store.general_scope().await.unwrap().id,
            parent_run_id: None,
            parent_seq: None,
            dispatching_session_id: parent,
            script_version_id: None,
            inline_workflow_id: None,
            inline_workflow_name: Some("Sweep".into()),
            inline_workflow_version: None,
            carrying_skill: None,
            task: None,
            source: Some("await swage.timer('1h');".into()),
            source_hash: "operator-test".into(),
            args: json!({}),
            budget: json!({}),
            engine_version: "test".into(),
            dispatch_bound_agent_ids: vec![],
            dispatch_bound_task_definition_ids: vec![],
            workflow_call_tools: None,
            inherited_worker_placement: None,
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_uses_the_run_accounts_failed_and_cut_short_partition() {
    let state = test_state();
    let router = build_router(state.clone());
    let run = operator_run(&state, None).await;
    let mut workers = json!({"done": [], "failed": [], "cut_short": [], "unsettled": []});
    let mut expected = Vec::new();
    for (seq, partition, ending) in [(1, "failed", "failed"), (2, "cut_short", "cancelled")] {
        let turn = operator_child(
            &state,
            swage_storage::SessionOrigin::WorkflowWorker { run_id: run.id },
            TurnState::Interrupted,
        )
        .await;
        let session_id = state
            .storage
            .get(turn.thread_id)
            .await
            .unwrap()
            .unwrap()
            .session_id;
        let entry = state
            .storage
            .create_workflow_journal_intent(swage_storage::NewWorkflowJournalIntent {
                run_id: run.id,
                seq,
                kind: "agent_call".into(),
                args_hash: seq.to_string(),
                request: json!({"agent": "gateway"}),
                effect_kind: None,
                idempotency_key: None,
                target_kind: None,
            })
            .await
            .unwrap();
        state
            .storage
            .record_workflow_call_dispatch(
                entry.id,
                json!({"prompt": "Work", "attempts": [{
                    "cause": {"cause": "first"}, "opened_at": chrono::Utc::now(),
                    "session_id": session_id, "thread_id": turn.thread_id, "turn_id": turn.id
                }]}),
            )
            .await
            .unwrap();
        state
            .storage
            .fail_workflow_journal_entry(entry.id, "worker stopped")
            .await
            .unwrap();
        workers[partition]
            .as_array_mut()
            .unwrap()
            .push(json!({"journal_entry_id": entry.id, "seq": seq,
            "redispatched_after_interruption": false}));
        expected.push((session_id, ending));
    }
    state.storage.set_workflow_run_terminal_account(run.id, json!({
        "ending": {"ending": "cancelled"}, "workers": workers, "settled_at": chrono::Utc::now()
    })).await.unwrap();
    state
        .storage
        .update_workflow_run_state(run.id, "cancelled", None, None, None)
        .await
        .unwrap();
    let (_, body) = get_json(&router, "/api/operator/overview").await;
    for (session_id, ending) in expected {
        let row = body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == session_id.to_string())
            .unwrap();
        assert_eq!(row["ending"], ending, "{body}");
        assert_eq!(row["has_interrupted_turn"], false, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_lists_live_runs_and_attributes_children() {
    let state = test_state();
    let router = build_router(state.clone());
    let parent = create_conversation(&router).await;
    let running = operator_run(&state, Some(parent)).await;
    let parked = operator_run(&state, Some(parent)).await;
    state
        .storage
        .update_workflow_run_state(parked.id, "parked", None, None, None)
        .await
        .unwrap();
    let terminal = operator_run(&state, Some(parent)).await;
    state
        .storage
        .update_workflow_run_state(terminal.id, "completed", None, None, None)
        .await
        .unwrap();
    let child = operator_child(
        &state,
        swage_storage::SessionOrigin::Delegation {
            parent_session_id: parent,
        },
        TurnState::Processing,
    )
    .await;
    let child_id = state
        .storage
        .get(child.thread_id)
        .await
        .unwrap()
        .unwrap()
        .session_id;
    let (_, body) = get_json(&router, "/api/operator/overview").await;
    let runs = body["workflow_runs"]
        .as_array()
        .expect("live workflow runs belong on the fleet");
    assert_eq!(runs.len(), 2, "{body}");
    for id in [running.id, parked.id] {
        let row = runs.iter().find(|row| row["id"] == id.to_string()).unwrap();
        assert_eq!(row["parent_session_id"], parent.to_string());
        assert_eq!(row["agent_id"], "gateway");
    }
    let row = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == child_id.to_string())
        .unwrap();
    assert_eq!(row["parent_session_id"], parent.to_string(), "{row}");
    assert_eq!(row["agent_id"], "gateway");
}
