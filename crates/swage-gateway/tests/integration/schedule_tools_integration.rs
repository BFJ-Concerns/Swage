//! Agent-facing schedule tools, end to end: an agent turn drives the
//! `schedule` and `wake_me_later` tools through the real dispatch pipeline,
//! the schedules it creates are attributed to the agent and fire on the tick,
//! and a permission grant that withholds the scheduling bundle blocks the
//! tools outright.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::Utc;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::change_case::{ChangeOperation, apply_change, undo_case};
use swage_gateway::service::ConversationService;
use swage_gateway::tool::schedule::{ScheduleTool, WakeMeLaterTool};
use swage_gateway::tool::{Tool, ToolDispatcher, ToolRegistry};
use swage_gateway::tool_executor::GatewayToolContext;
use swage_gateway::{schedule, server::build_router, state::AppState};
use swage_protocol::client::schedule::ScheduleStatus;
use swage_provider::LlmProvider;
use swage_storage::{
    NewAgentDefinition, NewSchedule, ScheduleRunState, SqliteStorage, UpdateSchedule,
};
use swage_tool::context::MockToolContext;
use swage_tool::permission::PermissionConfig;
use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};

/// Streams one fixed assistant turn per call — enough for an agent-created
/// schedule to fire and record a completed run.
struct TextProvider;

#[async_trait]
impl LlmProvider for TextProvider {
    fn model_id(&self) -> &str {
        "schedule-tools-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("schedule-tool tests declare no gate")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("schedule-tool tests do not use tool completion")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(vec![
            StreamChunk::Delta {
                content: "scheduled work done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]))
    }
}

fn test_state() -> AppState {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(TextProvider),
        swage_config::ResolvedConfig::default(),
        ToolDispatcher::allow_all(vec![]),
        None,
    )
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
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// The scheduling bundle as the config seeds it — the grant unit the tools
/// hang off.
fn scheduling_bundle() -> HashMap<String, Vec<String>> {
    let mut bundles = HashMap::new();
    bundles.insert(
        "scheduling".to_string(),
        vec!["schedule".to_string(), "wake_me_later".to_string()],
    );
    bundles
}

fn permissions(allow: &[&str], deny: &[&str]) -> PermissionConfig {
    PermissionConfig {
        allow: allow.iter().map(|entry| entry.to_string()).collect(),
        deny: deny.iter().map(|entry| entry.to_string()).collect(),
        bundles: scheduling_bundle(),
        require_approval: vec![],
    }
}

/// A dispatcher holding the two schedule tools, gated by `perms`.
fn dispatcher(perms: PermissionConfig) -> ToolDispatcher {
    let mut registry = ToolRegistry::default();
    registry.register_builtin(Arc::new(ScheduleTool)).unwrap();
    registry
        .register_builtin(Arc::new(WakeMeLaterTool))
        .unwrap();
    ToolDispatcher::new(
        Arc::new(registry),
        perms,
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(swage_safety::LeakDetector::default()),
        false,
    )
}

/// Build the host tool context the schedule tools need: it carries the live
/// gateway state and reports the calling agent and session.
fn host_context(state: &AppState, agent_id: &str, session_id: Uuid) -> GatewayToolContext {
    let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = MockToolContext::new(agent_id, session_id.to_string());
    GatewayToolContext::new(&ctx, ws_tx, state.storage.clone()).with_app_state(Some(state.clone()))
}

async fn tool_session(state: &AppState, space_id: Uuid) -> Uuid {
    ConversationService::new(state.storage.clone(), state.space_store.clone())
        .create_session_in_space_for_agent(space_id, "gateway", None)
        .await
        .unwrap()
        .id
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_created_schedule_is_attributed_and_fires() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["*"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, space.id).await);

    // The agent creates a fresh-session interval schedule through the tool.
    let output = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create",
                "message": "sweep the inbox",
                "timing": "every",
                "every": "every 1 hours",
                "session": "fresh",
                "space_id": space.id,
                "agent_id": "gateway",
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let created = &output.result["schedule"];
    assert_eq!(created["creator"]["kind"], "agent");
    assert_eq!(created["creator"]["agent_id"], "researcher");
    let schedule_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    // Listing through the tool shows it with the same agent attribution.
    let listed = dispatcher
        .dispatch("schedule", json!({"action": "list"}), &ctx, "researcher")
        .await
        .unwrap();
    let schedules = listed.result["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0]["creator"]["agent_id"], "researcher");

    // It is due immediately after the interval, and fires like any schedule.
    let due = state
        .storage
        .get_schedule(schedule_id)
        .await
        .unwrap()
        .unwrap()
        .next_run_at
        .unwrap();
    let handles = schedule::tick_once(&state, due).await.unwrap();
    for handle in handles {
        handle.await.unwrap();
    }
    let runs = state.storage.list_schedule_runs(schedule_id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, ScheduleRunState::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduling_bundle_can_pause_and_resume_a_schedule_through_the_tool() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, space.id).await);

    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create",
                "message": "sweep the inbox",
                "timing": "every",
                "every": "every 1 hours",
                "session": "fresh",
                "space_id": space.id,
                "agent_id": "gateway",
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let schedule_id = created.result["schedule"]["id"].as_str().unwrap();

    let paused = dispatcher
        .dispatch(
            "schedule",
            json!({"action": "pause", "schedule_id": schedule_id}),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    assert_eq!(paused.result["schedule"]["status"], "paused");

    let resumed = dispatcher
        .dispatch(
            "schedule",
            json!({"action": "resume", "schedule_id": schedule_id}),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    assert_eq!(resumed.result["schedule"]["status"], "active");
}

#[tokio::test(flavor = "multi_thread")]
async fn model_schedule_mutations_record_state_diffs_and_provenance() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let session_id = tool_session(&state, space.id).await;
    let ctx = host_context(&state, "researcher", session_id);

    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "first task", "timing": "every",
                "every": "every 1 hours", "session": "fresh", "space_id": space.id,
                "agent_id": "gateway"
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let schedule_id = created.result["schedule"]["id"].as_str().unwrap();
    let create_version = created.result["change_case"]["version_id"]
        .as_str()
        .unwrap();

    let updated = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "update", "schedule_id": schedule_id, "message": "second task"
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let update_version = updated.result["change_case"]["version_id"]
        .as_str()
        .unwrap();
    let paused = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "pause", "schedule_id": schedule_id
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let pause_version = paused.result["change_case"]["version_id"].as_str().unwrap();
    let resumed = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "resume", "schedule_id": schedule_id
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let resume_version = resumed.result["change_case"]["version_id"]
        .as_str()
        .unwrap();
    let removed = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "remove", "schedule_id": schedule_id
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let delete_version = removed.result["change_case"]["version_id"]
        .as_str()
        .unwrap();

    let mut versions = state
        .storage
        .list_configuration_versions_for_entity("schedule", schedule_id)
        .await
        .unwrap();
    versions.reverse();
    assert_eq!(
        versions.len(),
        5,
        "blind-accept records every landed mutation"
    );
    assert_eq!(
        versions
            .iter()
            .map(|v| v.id.to_string())
            .collect::<Vec<_>>(),
        vec![
            create_version,
            update_version,
            pause_version,
            resume_version,
            delete_version
        ]
    );
    assert_eq!(versions[0].snapshot["message"], "first task");
    assert_eq!(versions[0].diff["message"]["from"], serde_json::Value::Null);
    assert_eq!(versions[0].diff["message"]["to"], "first task");
    assert_eq!(versions[1].snapshot["message"], "second task");
    assert_eq!(versions[1].diff["message"]["from"], "first task");
    assert_eq!(versions[1].diff["message"]["to"], "second task");
    assert_eq!(versions[2].diff["status"]["from"], "active");
    assert_eq!(versions[2].diff["status"]["to"], "paused");
    assert_eq!(versions[3].diff["status"]["from"], "paused");
    assert_eq!(versions[3].diff["status"]["to"], "active");
    assert!(versions[4].snapshot.is_null());
    assert_eq!(versions[4].diff["message"]["from"], "second task");
    assert_eq!(versions[4].diff["message"]["to"], serde_json::Value::Null);
    assert!(
        versions
            .iter()
            .all(|v| v.source == "schedule" && v.session_id == Some(session_id))
    );
    let space_versions = state
        .storage
        .list_configuration_versions(Some(space.id))
        .await
        .unwrap();
    assert_eq!(
        space_versions
            .iter()
            .filter(|version| version.entity_id == schedule_id)
            .count(),
        5,
        "space history includes the schedule removal as well as its earlier mutations"
    );

    let audit = state
        .storage
        .list_audit_events_by_session(session_id)
        .await
        .unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|event| event.entity_kind == "schedule")
            .count(),
        5
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_surface_restores_schedule_state_and_records_the_undo() {
    let state = test_state();
    let router = build_router(state.clone());
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let session_id = tool_session(&state, space.id).await;
    let ctx = host_context(&state, "researcher", session_id);

    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "original task", "timing": "every",
                "every": "every 1 hours", "session": "fresh", "space_id": space.id,
                "agent_id": "gateway"
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let schedule_id = created.result["schedule"]["id"].as_str().unwrap();
    let update = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "update", "schedule_id": schedule_id, "message": "changed task",
                "timing": "every", "every": "every 2 hours"
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let updated_case = update.result["change_case"]["version_id"].as_str().unwrap();

    let (status, undo) = post_json(
        &router,
        "/api/operator/configuration/undo",
        json!({"case_id": updated_case}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let restored = state
        .storage
        .get_schedule(schedule_id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.message, "original task",
        "the resource itself is restored"
    );
    assert_eq!(restored.expression, "every 1 hours");

    assert_eq!(
        undo["summary"],
        format!("undid configuration case {updated_case}: restored schedule {schedule_id}")
    );
    let undo_version = undo["version_id"].as_str().unwrap();
    let recorded = state
        .storage
        .get_configuration_version(undo_version.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recorded.operation_kind, "undo");
    assert_eq!(
        recorded.undone_version_id.unwrap().to_string(),
        updated_case
    );
    assert_eq!(recorded.diff["message"]["from"], "changed task");
    assert_eq!(recorded.diff["message"]["to"], "original task");
    assert_eq!(recorded.source, "operator_http");
    assert_eq!(recorded.session_id, None);
    let audit = state.storage.list_audit_events().await.unwrap();
    assert!(
        audit
            .iter()
            .any(|event| event.metadata["version_id"] == undo_version
                && event.summary == undo["summary"])
    );

    let removed = dispatcher
        .dispatch(
            "schedule",
            json!({"action": "remove", "schedule_id": schedule_id}),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let absence_version = removed.result["change_case"]["version_id"]
        .as_str()
        .unwrap();
    let (status, _) = post_json(
        &router,
        "/api/operator/configuration/undo",
        json!({"case_id": absence_version}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let restored = state
        .storage
        .get_schedule(schedule_id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.message, "original task");
}

#[tokio::test(flavor = "multi_thread")]
async fn undoing_schedule_creation_restores_absence_and_records_the_deletion() {
    let state = test_state();
    let router = build_router(state.clone());
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, space.id).await);
    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "temporary task", "timing": "every",
                "every": "every 1 hours", "session": "fresh", "space_id": space.id,
                "agent_id": "gateway"
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let schedule_id = created.result["schedule"]["id"].as_str().unwrap();
    let created_case = created.result["change_case"]["version_id"]
        .as_str()
        .unwrap();

    let (status, undo) = post_json(
        &router,
        "/api/operator/configuration/undo",
        json!({"case_id": created_case}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        state
            .storage
            .get_schedule(schedule_id.parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        undo["summary"],
        format!("undid configuration case {created_case}: removed schedule {schedule_id}")
    );

    let undo_version = undo["version_id"].as_str().unwrap();
    let recorded = state
        .storage
        .get_configuration_version(undo_version.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recorded.operation_kind, "undo");
    assert_eq!(
        recorded.undone_version_id.unwrap().to_string(),
        created_case
    );
    assert_eq!(recorded.diff["id"]["from"], schedule_id);
    assert!(recorded.diff["id"]["to"].is_null());
    assert!(recorded.snapshot.is_null());
    let audit = state.storage.list_audit_events().await.unwrap();
    assert!(audit.iter().any(|event| {
        event.metadata["version_id"] == undo_version && event.summary == undo["summary"]
    }));

    let (status, second_undo) = post_json(
        &router,
        "/api/operator/configuration/undo",
        json!({"case_id": created_case}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        second_undo["summary"],
        format!(
            "undid configuration case {created_case}: schedule {schedule_id} was already absent; nothing was removed"
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_definition_undo_uses_the_same_prior_state_and_prior_absence_rule() {
    let state = test_state();
    let original = NewAgentDefinition {
        id: "case-test-agent".into(),
        space_id: None,
        display_name: "Original".into(),
        description: "first state".into(),
        instructions: "Keep the first instructions".into(),
        model_id: None,
        tool_permissions: None,
    };
    let created = apply_change(
        &state,
        ChangeOperation::UpsertAgentDefinition {
            definition: original.clone(),
        },
        "test",
        None,
    )
    .await
    .unwrap();
    let mut updated = original.clone();
    updated.display_name = "Updated".into();
    updated.instructions = "Use the changed instructions".into();
    let update_case = apply_change(
        &state,
        ChangeOperation::UpsertAgentDefinition {
            definition: updated,
        },
        "test",
        None,
    )
    .await
    .unwrap();

    undo_case(&state, update_case.version_id, "operator_http", None)
        .await
        .unwrap();
    let restored = state
        .storage
        .list_agent_definitions(None)
        .await
        .unwrap()
        .into_iter()
        .find(|definition| definition.id == "case-test-agent")
        .unwrap();
    assert_eq!(restored.display_name, "Original");
    assert_eq!(restored.instructions, "Keep the first instructions");

    let undo = undo_case(&state, created.version_id, "operator_http", None)
        .await
        .unwrap();
    assert_eq!(
        undo.summary,
        format!(
            "undid configuration case {}: removed agent definition 'case-test-agent'",
            created.version_id
        )
    );
    assert!(
        !state
            .storage
            .list_agent_definitions(None)
            .await
            .unwrap()
            .into_iter()
            .any(|definition| definition.id == "case-test-agent")
    );

    let second_undo = undo_case(&state, created.version_id, "operator_http", None)
        .await
        .unwrap();
    assert_eq!(
        second_undo.summary,
        format!(
            "undid configuration case {}: agent definition 'case-test-agent' was already absent; nothing was removed",
            created.version_id
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn change_case_validation_rejects_invalid_schedule_before_any_write() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, space.id).await);
    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "valid task", "timing": "every",
                "every": "every 1 hours", "session": "fresh", "space_id": space.id,
                "agent_id": "gateway"
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let stored = state
        .storage
        .get_schedule(
            created.result["schedule"]["id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let invalid_id = Uuid::new_v4();
    let invalid = NewSchedule {
        id: invalid_id,
        message: stored.message,
        status: stored.status,
        kind: stored.kind,
        expression: "not an interval".into(),
        session_policy: stored.session_policy,
        creator: stored.creator,
        delivery_routing: stored.delivery_routing,
        gate: stored.gate,
        next_run_at: stored.next_run_at,
        last_run_at: stored.last_run_at,
    };
    assert!(
        apply_change(
            &state,
            ChangeOperation::CreateSchedule { schedule: invalid },
            "test",
            None
        )
        .await
        .is_err()
    );
    assert!(
        state
            .storage
            .get_schedule(invalid_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .storage
            .list_configuration_versions_for_entity("schedule", &invalid_id.to_string())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_schedule_edit_preserves_a_concurrent_tick_lifecycle() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let session_id = tool_session(&state, space.id).await;
    let ctx = host_context(&state, "researcher", session_id);
    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "before tick", "timing": "once",
                "at": Utc::now().to_rfc3339(), "session": "fresh", "space_id": space.id,
                "agent_id": "gateway"
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let schedule_id = created.result["schedule"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let stale = state
        .storage
        .get_schedule(schedule_id)
        .await
        .unwrap()
        .unwrap();
    let intended = NewSchedule {
        id: stale.id,
        message: "after tick".into(),
        status: stale.status,
        kind: stale.kind,
        expression: stale.expression,
        session_policy: stale.session_policy,
        creator: stale.creator,
        delivery_routing: stale.delivery_routing,
        gate: stale.gate,
        next_run_at: stale.next_run_at,
        last_run_at: stale.last_run_at,
    };

    // Model the lifecycle advance that can land after resolution but before
    // the configuration apply. The stale intended state must not replay the
    // old due instant or erase the tick timestamp.
    let tick_at = Utc::now();
    state
        .storage
        .update_schedule(
            schedule_id,
            UpdateSchedule {
                status: Some(ScheduleStatus::Completed),
                next_run_at: Some(None),
                last_run_at: Some(Some(tick_at)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    apply_change(
        &state,
        ChangeOperation::UpdateSchedule {
            schedule: intended,
            rearm: false,
        },
        "schedule",
        Some(session_id),
    )
    .await
    .unwrap();

    let stored = state
        .storage
        .get_schedule(schedule_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.message, "after tick");
    assert_eq!(stored.status, ScheduleStatus::Completed);
    assert_eq!(stored.next_run_at, None);
    assert_eq!(stored.last_run_at, Some(tick_at));
}

#[tokio::test(flavor = "multi_thread")]
async fn wake_me_later_schedules_a_one_off_continuation_of_the_calling_session() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let session = ConversationService::new(state.storage.clone(), state.space_store.clone())
        .create_session_in_space_for_agent(space.id, "gateway", None)
        .await
        .unwrap();
    let dispatcher = dispatcher(permissions(&["*"], &[]));
    let ctx = host_context(&state, "researcher", session.id);

    let output = dispatcher
        .dispatch(
            "wake_me_later",
            json!({"delay": "2 hours", "message": "check the build"}),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let schedule_id: Uuid = output.result["schedule_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let version_id: Uuid = output.result["change_case"]["version_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // The wake is a one-off schedule that continues this very session.
    let schedule = state
        .storage
        .get_schedule(schedule_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        schedule.session_policy,
        swage_storage::SessionPolicy::Continuation { session_id } if session_id == session.id
    ));
    assert!(matches!(
        schedule.creator,
        swage_storage::ScheduleCreator::Agent { agent_id } if agent_id == "researcher"
    ));
    assert!(matches!(
        schedule.kind,
        swage_storage::ScheduleKind::OneShot
    ));
    let version = state
        .storage
        .get_configuration_version(version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.space_id, Some(space.id));
    assert!(
        state
            .storage
            .list_configuration_versions(Some(space.id))
            .await
            .unwrap()
            .iter()
            .any(|version| version.id == version_id),
        "the continuation wake remains visible in its owning space's configuration history"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduling_bundle_is_the_grant_unit() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let ctx = host_context(&state, "researcher", tool_session(&state, space.id).await);

    // Granting only the scheduling bundle reaches both tools and nothing else.
    let granted = dispatcher(permissions(&["scheduling"], &[]));
    assert!(
        !ScheduleTool.parameters()["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action == "undo"),
        "the model-facing scheduling grant does not include undo"
    );
    assert!(
        granted
            .dispatch("schedule", json!({"action": "list"}), &ctx, "researcher")
            .await
            .is_ok()
    );

    // Withholding the bundle (a non-wildcard grant that omits it) blocks the
    // tools by default-deny.
    let ungranted = dispatcher(permissions(&["file_read"], &[]));
    assert!(matches!(
        ungranted
            .dispatch("schedule", json!({"action": "list"}), &ctx, "researcher")
            .await,
        Err(swage_core::ToolError::PermissionDenied { .. })
    ));

    // An explicit deny of the tool names overrides even a wildcard allow.
    let denied = dispatcher(permissions(&["*"], &["schedule", "wake_me_later"]));
    assert!(matches!(
        denied
            .dispatch(
                "wake_me_later",
                json!({"delay": "1 hours", "message": "x"}),
                &ctx,
                "researcher"
            )
            .await,
        Err(swage_core::ToolError::PermissionDenied { .. })
    ));
}
