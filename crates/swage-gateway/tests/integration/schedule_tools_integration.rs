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
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::service::ConversationService;
use swage_gateway::tool::schedule::{ScheduleTool, WakeMeLaterTool};
use swage_gateway::tool::{Tool, ToolDispatcher, ToolRegistry};
use swage_gateway::tool_executor::GatewayToolContext;
use swage_gateway::{schedule, state::AppState};
use swage_protocol::client::delivery::DeliveryRoutingRequest;
use swage_protocol::client::schedule::ScheduleStatus;
use swage_provider::openai::OpenAiProvider;
use swage_provider::{LlmProvider, ProviderConfig};
use swage_storage::{ScheduleCreator, ScheduleRunState, SqliteStorage, UpdateSchedule};
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
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
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

async fn tool_session(state: &AppState, topic_id: Uuid) -> Uuid {
    ConversationService::new(state.storage.clone(), state.topic_store.clone())
        .create_session_in_topic_for_agent(topic_id, "gateway", None)
        .await
        .unwrap()
        .id
}

fn routing_for(label: &str, target: serde_json::Value) -> (serde_json::Value, serde_json::Value) {
    (
        json!({
            "home_target": null,
            "explicit_targets": {(label): target.clone()},
            "store_local_output": false,
        }),
        json!({
            "deliver_to_origin": false,
            "home_target": null,
            "explicit_targets": [target],
            "store_local_output": false,
        }),
    )
}

fn routing_cases() -> Vec<(&'static str, serde_json::Value, serde_json::Value)> {
    vec![
        (
            "channel",
            routing_for(
                "team_chat",
                json!({
                    "kind": "channel",
                    "integration": "dev_thread",
                    "account": "workspace-a",
                    "conversation": "room-1/topic-9",
                }),
            ),
        ),
        (
            "local storage",
            routing_for("archive", json!({"kind": "local_storage"})),
        ),
        (
            "webhook",
            routing_for(
                "automation",
                json!({
                    "kind": "webhook",
                    "url": "https://example.test/scheduled-output",
                }),
            ),
        ),
    ]
    .into_iter()
    .map(|(name, (input, persisted))| (name, input, persisted))
    .collect()
}

#[test]
fn provider_facing_schedule_schema_preserves_delivery_target_vocabulary() {
    let provider =
        OpenAiProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
    let request = CompletionRequest {
        effort: None,
        messages: vec![],
        model: None,
        max_tokens: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![json!({
            "name": ScheduleTool.name(),
            "description": ScheduleTool.description(),
            "parameters": ScheduleTool.parameters(),
        })],
    };

    let snapshot = provider.provider_request_snapshot(&request, false);
    let routing = &snapshot["body"]["tools"][0]["function"]["parameters"]["properties"]["routing"];
    let home = &routing["properties"]["home_target"];
    let additional = &routing["properties"]["explicit_targets"]["additionalProperties"];

    for target in [home, additional] {
        assert_eq!(
            target["properties"]["kind"]["enum"],
            json!(["channel", "local_storage", "webhook"])
        );
        assert_eq!(
            target["properties"]["integration"]["enum"],
            json!(["dev_dm", "dev_thread", "telegram"])
        );
        for field in ["account", "conversation", "url"] {
            assert_eq!(target["properties"][field]["type"], "string");
        }
        let description = target["description"]
            .as_str()
            .expect("projected delivery target carries its stripped oneOf requirements");
        for required in [
            "`kind` must be \"channel\"",
            "requires `kind`, `integration`, `account`, `conversation`",
            "`kind` must be \"local_storage\"",
            "`kind` must be \"webhook\"",
            "requires `kind`, `url`",
        ] {
            assert!(
                description.contains(required),
                "projected target description omits {required}: {description}"
            );
        }
    }
    assert!(
        home["description"]
            .as_str()
            .expect("nullable projected target carries a description")
            .contains("is `null`"),
        "nullable target summary must include its null alternative"
    );
    assert!(
        routing["properties"]["explicit_targets"]
            .get("items")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_routing_keeps_strict_local_target_validation() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, topic.id).await);
    let over_specified_webhook = json!({
        "kind": "webhook",
        "url": "https://example.test/scheduled-output",
        "account": "ignored-by-serde"
    });

    serde_json::from_value::<DeliveryRoutingRequest>(json!({
        "deliver_to_origin": false,
        "explicit_targets": [over_specified_webhook.clone()]
    }))
    .expect("serde alone accepts fields that do not belong to the selected target variant");

    let rejected = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create",
                "message": "deliver to an over-specified webhook",
                "timing": "every",
                "every": "every 1 hours",
                "session": "fresh",
                "topic_id": topic.id,
                "agent_id": "gateway",
                "routing": {
                    "explicit_targets": {
                        "automation": over_specified_webhook
                    }
                }
            }),
            &ctx,
            "researcher",
        )
        .await;

    assert!(matches!(
        rejected,
        Err(swage_core::ToolError::InvalidParameters { .. })
    ));
    assert!(state.storage.list_schedules().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_can_create_schedules_with_each_delivery_route() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, topic.id).await);

    for (name, routing, persisted_routing) in routing_cases() {
        let created = dispatcher
            .dispatch(
                "schedule",
                json!({
                    "action": "create",
                    "message": format!("deliver through {name}"),
                    "timing": "every",
                    "every": "every 1 hours",
                    "session": "fresh",
                    "topic_id": topic.id,
                    "agent_id": "gateway",
                    "routing": routing,
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
        let stored = state
            .storage
            .get_schedule(schedule_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.delivery_routing, persisted_routing,
            "the {name} route is persisted from the tool call"
        );
        assert!(matches!(
            stored.creator,
            ScheduleCreator::Agent { ref agent_id } if agent_id == "researcher"
        ));
    }

    let omitted = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create",
                "message": "keep the default route",
                "timing": "every",
                "every": "every 1 hours",
                "session": "fresh",
                "topic_id": topic.id,
                "agent_id": "gateway",
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    assert_eq!(
        omitted.result["schedule"]["routing"],
        json!({
            "deliver_to_origin": false,
            "home_target": null,
            "explicit_targets": [],
            "store_local_output": true,
        }),
        "omitting routing retains the service's local-only create default"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_can_update_schedules_to_each_delivery_route() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, topic.id).await);
    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create",
                "message": "change my route",
                "timing": "every",
                "every": "every 1 hours",
                "session": "fresh",
                "topic_id": topic.id,
                "agent_id": "gateway",
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let schedule_id = created.result["schedule"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    for (name, routing, persisted_routing) in routing_cases() {
        dispatcher
            .dispatch(
                "schedule",
                json!({
                    "action": "update",
                    "schedule_id": schedule_id,
                    "routing": routing,
                }),
                &ctx,
                "researcher",
            )
            .await
            .unwrap();
        let stored = state
            .storage
            .get_schedule(schedule_id.parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.delivery_routing, persisted_routing,
            "the {name} route is persisted from the tool update"
        );
    }

    let before_omission = state
        .storage
        .get_schedule(schedule_id.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .delivery_routing;
    dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "update",
                "schedule_id": schedule_id,
                "message": "route unchanged",
            }),
            &ctx,
            "researcher",
        )
        .await
        .unwrap();
    let after_omission = state
        .storage
        .get_schedule(schedule_id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_omission.delivery_routing, before_omission);
    assert!(matches!(
        after_omission.creator,
        ScheduleCreator::Agent { ref agent_id } if agent_id == "researcher"
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_created_schedule_is_attributed_and_fires() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["*"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, topic.id).await);

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
                "topic_id": topic.id,
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
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, topic.id).await);

    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create",
                "message": "sweep the inbox",
                "timing": "every",
                "every": "every 1 hours",
                "session": "fresh",
                "topic_id": topic.id,
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
async fn model_schedule_mutations_record_prior_and_new_state_and_provenance() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let session_id = tool_session(&state, topic.id).await;
    let ctx = host_context(&state, "researcher", session_id);

    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "first task", "timing": "every",
                "every": "every 1 hours", "session": "fresh", "topic_id": topic.id,
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
        .to_owned();
    let schedule_id = schedule_id.as_str();

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
    assert_eq!(updated.result["schedule"]["message"], "second task");
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
    assert_eq!(paused.result["schedule"]["status"], "paused");
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
    assert_eq!(resumed.result["schedule"]["status"], "active");
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
    assert_eq!(removed.result["removed"], true);

    // Every landed mutation leaves one configuration_change audit event
    // carrying the whole prior and new state under a named action.
    let audit = state
        .storage
        .list_audit_events_by_session(session_id)
        .await
        .unwrap();
    let changes: Vec<_> = audit
        .iter()
        .filter(|event| {
            event.event_kind == "configuration_change" && event.entity_id == schedule_id
        })
        .collect();
    assert_eq!(changes.len(), 5, "every landed mutation is recorded");

    let by_action = |action: &str| {
        changes
            .iter()
            .find(|event| event.metadata["action"] == action)
            .unwrap_or_else(|| panic!("no recorded {action}"))
    };
    let create = by_action("create_schedule");
    assert!(create.metadata["prior_state"].is_null());
    assert_eq!(create.metadata["new_state"]["message"], "first task");
    let update = by_action("update_schedule");
    assert_eq!(update.metadata["prior_state"]["message"], "first task");
    assert_eq!(update.metadata["new_state"]["message"], "second task");
    let pause = by_action("pause_schedule");
    assert_eq!(pause.metadata["prior_state"]["status"], "active");
    assert_eq!(pause.metadata["new_state"]["status"], "paused");
    let resume = by_action("resume_schedule");
    assert_eq!(resume.metadata["prior_state"]["status"], "paused");
    assert_eq!(resume.metadata["new_state"]["status"], "active");
    let delete = by_action("delete_schedule");
    assert_eq!(delete.metadata["prior_state"]["message"], "second task");
    assert!(delete.metadata["new_state"].is_null());

    assert!(
        changes
            .iter()
            .all(|event| event.metadata["source"] == "schedule"
                && event.session_id == Some(session_id)),
        "provenance names the tool surface and the authoring session"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_schedule_is_rejected_before_any_write() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let ctx = host_context(&state, "researcher", tool_session(&state, topic.id).await);

    let rejected = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "broken task", "timing": "every",
                "every": "not an interval", "session": "fresh", "topic_id": topic.id,
                "agent_id": "gateway"
            }),
            &ctx,
            "researcher",
        )
        .await;
    assert!(rejected.is_err(), "an invalid interval never lands");

    // Nothing was written: no schedule row and no recorded change.
    assert!(state.storage.list_schedules().await.unwrap().is_empty());
    let audits = state.storage.list_audit_events().await.unwrap();
    assert!(
        !audits
            .iter()
            .any(|event| event.event_kind == "configuration_change"
                && event.entity_kind == "schedule"),
        "a rejected mutation leaves no configuration_change record"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_schedule_edit_preserves_a_concurrent_tick_lifecycle() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let dispatcher = dispatcher(permissions(&["scheduling"], &[]));
    let session_id = tool_session(&state, topic.id).await;
    let ctx = host_context(&state, "researcher", session_id);
    let created = dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "create", "message": "before tick", "timing": "once",
                "at": Utc::now().to_rfc3339(), "session": "fresh", "topic_id": topic.id,
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
    // The scheduler's tick advances lifecycle state the edit must not
    // replay: an edit without a timing change leaves status, the due
    // instant, and the tick timestamp untouched.
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
    dispatcher
        .dispatch(
            "schedule",
            json!({
                "action": "update",
                "schedule_id": schedule_id.to_string(),
                "message": "after tick"
            }),
            &ctx,
            "researcher",
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
    let topic = state.topic_store.general_scope().await.unwrap();
    let session = ConversationService::new(state.storage.clone(), state.topic_store.clone())
        .create_session_in_topic_for_agent(topic.id, "gateway", None)
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
    // The wake is recorded like any other configuration change, attributed
    // to the tool surface and the calling session.
    let audits = state
        .storage
        .list_audit_events_by_session(session.id)
        .await
        .unwrap();
    assert!(
        audits.iter().any(|event| {
            event.event_kind == "configuration_change"
                && event.entity_id == schedule_id.to_string()
                && event.metadata["action"] == "create_schedule"
                && event.metadata["source"] == "wake_me_later"
        }),
        "the continuation wake leaves a configuration_change record"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduling_bundle_is_the_grant_unit() {
    let state = test_state();
    let topic = state.topic_store.general_scope().await.unwrap();
    let ctx = host_context(&state, "researcher", tool_session(&state, topic.id).await);

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
