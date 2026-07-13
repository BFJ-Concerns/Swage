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
use serde_json::json;
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::service::ConversationService;
use swage_gateway::tool::schedule::{ScheduleTool, WakeMeLaterTool};
use swage_gateway::tool::{ToolDispatcher, ToolRegistry};
use swage_gateway::tool_executor::GatewayToolContext;
use swage_gateway::{schedule, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{ScheduleRunState, SqliteStorage};
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

#[tokio::test(flavor = "multi_thread")]
async fn agent_created_schedule_is_attributed_and_fires() {
    let state = test_state();
    let space = state.space_store.get_or_create_default().await.unwrap();
    let dispatcher = dispatcher(permissions(&["*"], &[]));
    let ctx = host_context(&state, "researcher", Uuid::new_v4());

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
    let ctx = host_context(&state, "researcher", Uuid::new_v4());

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
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduling_bundle_is_the_grant_unit() {
    let state = test_state();
    let ctx = host_context(&state, "researcher", Uuid::new_v4());

    // Granting only the scheduling bundle reaches both tools and nothing else.
    let granted = dispatcher(permissions(&["scheduling"], &[]));
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
