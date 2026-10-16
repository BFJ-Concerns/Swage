#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    ApprovalTimeoutPolicy, CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{
    channel_gateway::{DeliveryAttribution, DeliveryTarget, deliver_to_targets},
    server::build_router,
    state::AppState,
};
use swage_provider::LlmProvider;
use swage_storage::{
    ApprovalKind, NewApprovalRequest, NewSchedule, NewScheduleRun, NewSession, NewThread, NewTurn,
    ScheduleCreator, ScheduleKind, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate,
    ScheduleStatus, SessionPolicy, SqliteStorage, TurnState,
    model::AppendMessage,
    store::{
        ApprovalStore, DeliveryStore, MessageStore, ScheduleStore, SessionStore, Storage,
        ThreadStore, TopicStore, TurnStore,
    },
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "operator-audit-usage-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("operator audit tests inspect persisted state only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("operator audit tests inspect persisted state only")
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

fn test_state(storage: Arc<SqliteStorage>) -> AppState {
    AppState::new(
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

async fn seed_session(storage: &Arc<SqliteStorage>) -> (Uuid, Uuid, Uuid) {
    let topic = storage.general_scope().await.unwrap();
    let session = storage
        .create_session(NewSession {
            metadata: Some(json!({"label": "Ops session"})),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .unwrap();
    let thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: "gateway".into(),
        },
    )
    .await
    .unwrap();
    (topic.id, session.id, thread.id)
}

#[tokio::test(flavor = "multi_thread")]
async fn delivery_record_write_failure_is_returned_without_masking_delivery_failure() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let missing_session = Uuid::new_v4();

    let record_error = deliver_to_targets(
        &state,
        missing_session,
        DeliveryAttribution::default(),
        &[DeliveryTarget::LocalStorage],
        "reply",
    )
    .await
    .expect_err("a rejected delivery record must fail the operation");
    assert!(
        matches!(record_error, swage_core::GatewayError::Storage(_)),
        "{record_error}"
    );

    let delivery_error = deliver_to_targets(
        &state,
        missing_session,
        DeliveryAttribution::default(),
        &[DeliveryTarget::Webhook {
            url: "not a URL".into(),
        }],
        "reply",
    )
    .await
    .expect_err("delivery and recording should both fail");
    assert!(
        delivery_error
            .to_string()
            .contains("invalid webhook target URL"),
        "{delivery_error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_search_queries_sessions_messages_approvals_runs_and_deliveries() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state.clone());
    let (_topic_id, session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("searchable user input".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let message = storage
        .append_message(AppendMessage {
            thread_id,
            attachments: Vec::new(),
            turn_id: Some(turn.id),
            role: swage_core::Role::User,
            origin: swage_storage::MessageOrigin::User,
            delivered: false,
            content: "message needle".into(),
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();
    let approval = storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: None,
            hook_name: None,
            prompt: "approval needle".into(),
            context: None,
            timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .await
        .unwrap();
    let schedule = storage
        .create_schedule(NewSchedule {
            id: Uuid::new_v4(),
            message: "schedule needle".into(),
            status: ScheduleStatus::Active,
            kind: ScheduleKind::Interval,
            expression: "every 1 minutes".into(),
            session_policy: SessionPolicy::Continuation { session_id },
            creator: ScheduleCreator::User,
            delivery_routing: json!({"store_local_output": true}),
            gate: None,
            next_run_at: None,
            last_run_at: None,
        })
        .await
        .unwrap();
    let schedule_run = storage
        .create_schedule_run(NewScheduleRun::pending(
            schedule.id,
            ScheduleRunTrigger::Manual,
            None,
        ))
        .await
        .unwrap();
    storage
        .update_schedule_run(
            schedule_run.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Completed),
                turn_id: Some(turn.id),
                output_text: Some("run output".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    deliver_to_targets(
        &state,
        session_id,
        Default::default(),
        &[DeliveryTarget::LocalStorage],
        "delivery needle",
    )
    .await
    .unwrap();

    let (session_status, session_body) =
        get_json(&router, "/api/operator/search?kind=session&q=Ops%20session").await;
    assert_eq!(session_status, StatusCode::OK);
    assert_eq!(session_body["results"][0]["id"], session_id.to_string());

    let (message_status, message_body) = get_json(
        &router,
        "/api/operator/search?kind=message&q=message%20needle",
    )
    .await;
    assert_eq!(message_status, StatusCode::OK);
    assert_eq!(message_body["results"][0]["id"], message.id.to_string());

    let (approval_status, approval_body) = get_json(
        &router,
        "/api/operator/search?kind=approval&q=approval%20needle",
    )
    .await;
    assert_eq!(approval_status, StatusCode::OK);
    assert_eq!(approval_body["results"][0]["id"], approval.id.to_string());

    let (run_status, run_body) = get_json(
        &router,
        "/api/operator/search?kind=schedule_run&q=schedule%20needle",
    )
    .await;
    assert_eq!(run_status, StatusCode::OK);
    assert_eq!(run_body["results"][0]["id"], schedule_run.id.to_string());

    let (delivery_status, delivery_body) = get_json(
        &router,
        "/api/operator/search?kind=delivery&q=delivery%20needle",
    )
    .await;
    assert_eq!(delivery_status, StatusCode::OK);
    assert_eq!(delivery_body["results"][0]["kind"], "delivery");
    assert_eq!(
        delivery_body["results"][0]["session_id"],
        session_id.to_string()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_trail_survives_restart_for_approvals_deliveries_and_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("operator-audit.db");
    let storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state.clone());
    let (_topic_id, session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::AwaitingApproval,
            user_input: Some("approval".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    let approval = storage
        .create_approval(NewApprovalRequest {
            turn_id: turn.id,
            agent_id: "gateway".into(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".into()),
            tool_arguments: None,
            hook_name: None,
            prompt: "approve me".into(),
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
    let (approval_status, _) = post_json(
        &router,
        &format!("/api/approvals/{}", approval.id),
        json!({"decision": "approved", "reason": "operator approved"}),
    )
    .await;
    assert_eq!(approval_status, StatusCode::OK);
    assert_eq!(
        decision.await.unwrap().decision,
        swage_agent::approval::ApprovalDecisionValue::Approved
    );

    let schedule = storage
        .create_schedule(NewSchedule {
            id: Uuid::new_v4(),
            message: "nightly".into(),
            status: ScheduleStatus::Active,
            kind: ScheduleKind::Interval,
            expression: "every 1 minutes".into(),
            session_policy: SessionPolicy::Continuation { session_id },
            creator: ScheduleCreator::User,
            delivery_routing: json!({"store_local_output": true}),
            gate: None,
            next_run_at: None,
            last_run_at: None,
        })
        .await
        .unwrap();
    let run = storage
        .create_schedule_run(NewScheduleRun::pending(
            schedule.id,
            ScheduleRunTrigger::Tick,
            None,
        ))
        .await
        .unwrap();
    storage
        .update_schedule_run(
            run.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Running),
                session_id: Some(session_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    storage
        .update_schedule_run(
            run.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Failed),
                error_message: Some("failed later".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    deliver_to_targets(
        &state,
        session_id,
        Default::default(),
        &[DeliveryTarget::Channel {
            integration: swage_gateway::channel_gateway::ExternalChannelIntegration::DevDm,
            account: "ops".into(),
            conversation: "alice".into(),
        }],
        "delivered after routing",
    )
    .await
    .unwrap();
    let deliveries = storage
        .list_delivery_records_by_session(session_id)
        .await
        .unwrap();
    let delivery = deliveries.last().unwrap();
    assert_eq!(delivery.status, "not_sent");
    assert_eq!(
        delivery.error.as_deref(),
        Some("development channel target is a deliberate no-op")
    );

    drop(router);
    drop(state);
    drop(storage);

    let restarted_storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
    let restarted_state = test_state(Arc::clone(&restarted_storage));
    let restarted_router = build_router(restarted_state);

    let (status, body) = get_json(
        &restarted_router,
        &format!("/api/operator/search?kind=audit_event&session_id={session_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let excerpts = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| {
            format!(
                "{} {}",
                result["title"].as_str().unwrap_or_default(),
                result["excerpt"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();

    assert!(
        excerpts.iter().any(|excerpt| {
            excerpt.contains("approval state pending -> approved")
                && excerpt.contains("operator approved")
        }),
        "expected approval audit event, got: {body:?}"
    );
    assert!(
        excerpts.iter().any(|excerpt| {
            excerpt.contains("schedule run state running -> failed")
                && excerpt.contains("\"from_state\":\"running\"")
        }),
        "expected schedule-run transition audit event, got: {body:?}"
    );
    assert!(
        excerpts.iter().any(|excerpt| {
            excerpt.contains("delivery recorded") && excerpt.contains("delivered after routing")
        }),
        "expected delivery audit event, got: {body:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_usage_reports_run_level_and_aggregate_costs() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = test_state(Arc::clone(&storage));
    let router = build_router(state);
    let (_topic_id, session_id, thread_id) = seed_session(&storage).await;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".into(),
            state: TurnState::Processing,
            user_input: Some("usage".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage.update_tokens(turn.id, 120, 45).await.unwrap();
    storage.update_cost(turn.id, 700).await.unwrap();
    storage
        .update_turn_state(turn.id, TurnState::Completed)
        .await
        .unwrap();

    let schedule = storage
        .create_schedule(NewSchedule {
            id: Uuid::new_v4(),
            message: "usage schedule".into(),
            status: ScheduleStatus::Active,
            kind: ScheduleKind::Interval,
            expression: "every 1 minutes".into(),
            session_policy: SessionPolicy::Continuation { session_id },
            creator: ScheduleCreator::User,
            delivery_routing: json!({"store_local_output": true}),
            gate: None,
            next_run_at: None,
            last_run_at: None,
        })
        .await
        .unwrap();
    let usage_run = storage
        .create_schedule_run(NewScheduleRun::pending(
            schedule.id,
            ScheduleRunTrigger::Manual,
            None,
        ))
        .await
        .unwrap();
    storage
        .update_schedule_run(
            usage_run.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Completed),
                turn_id: Some(turn.id),
                output_text: Some("done".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let (status, body) = get_json(
        &router,
        &format!(
            "/api/operator/usage?run_kind=user_turn&run_id={}&session_id={}&schedule_id={}",
            turn.id, session_id, schedule.id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["run"]["input_tokens"], 120);
    assert_eq!(body["run"]["output_tokens"], 45);
    assert_eq!(body["run"]["cost_microcents"], 700);
    assert_eq!(body["session"]["input_tokens"], 120);
    assert_eq!(body["session"]["output_tokens"], 45);
    assert_eq!(body["session"]["cost_microcents"], 700);
    assert_eq!(body["schedule"]["run_count"], 1);
    assert_eq!(body["schedule"]["input_tokens"], 120);
    assert_eq!(body["schedule"]["cost_microcents"], 700);
}
