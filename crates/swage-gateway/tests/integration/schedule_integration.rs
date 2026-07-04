//! End-to-end coverage for the unified schedule domain: every kind fires,
//! both session policies execute, the gate decides both ways, overlap is
//! skipped deliberately, deliveries are recorded, and restart recovery
//! repairs orphaned runs.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{schedule, server::build_router, state::AppState};
use swage_protocol::client::delivery::{DeliveryRoutingRequest, DeliveryTarget};
use swage_protocol::client::schedule::{
    CreateScheduleRequest, GateConfig, GateVerdict, ScheduleSpec, SessionPolicy,
};
use swage_provider::LlmProvider;
use swage_storage::{
    NewScheduleRun, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate, SqliteStorage,
};

/// Provider double serving both halves of the schedule domain: queued
/// stream rounds feed agent turns, queued completion replies feed the
/// gate. Completion requests are recorded so tests can assert what the
/// gate actually asked.
struct MockProvider {
    stream_rounds: Mutex<VecDeque<Vec<StreamChunk>>>,
    completion_replies: Mutex<VecDeque<String>>,
    completion_requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl MockProvider {
    fn new(stream_rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            stream_rounds: Mutex::new(stream_rounds.into()),
            completion_replies: Mutex::new(VecDeque::new()),
            completion_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_completions(mut self, replies: Vec<&str>) -> Self {
        self.completion_replies = Mutex::new(replies.into_iter().map(String::from).collect());
        self
    }

    fn completion_requests(&self) -> Arc<Mutex<Vec<CompletionRequest>>> {
        Arc::clone(&self.completion_requests)
    }

    fn text_round(content: &str) -> Vec<StreamChunk> {
        vec![
            StreamChunk::Delta {
                content: content.into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "schedule-mock"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        self.completion_requests.lock().unwrap().push(request);
        let reply = self.completion_replies.lock().unwrap().pop_front().ok_or(
            ProviderError::InvalidRequest {
                message: "no completion reply queued".into(),
            },
        )?;
        Ok(CompletionResponse {
            content: reply,
            input_tokens: 10,
            output_tokens: 10,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("schedule tests do not use tool completion")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = self
            .stream_rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                vec![StreamChunk::Done {
                    finish_reason: None,
                }]
            });
        Box::pin(futures::stream::iter(chunks))
    }
}

/// The gate model as it appears in the configured catalogue.
const GATE_MODEL: &str = "gate-small";

fn config_with_gate_model() -> swage_config::ResolvedConfig {
    let mut config = swage_config::ResolvedConfig::default();
    config.models.entries.insert(
        GATE_MODEL.to_string(),
        swage_config::ModelEndpointConfig {
            provider_id: "mock".into(),
            model_id: "gate-small-model".into(),
            aliases: vec!["cheap".into()],
            context_window: None,
            generation_preset: None,
            capabilities: Default::default(),
        },
    );
    config
}

fn test_state(db_path: &Path, provider: impl LlmProvider + 'static) -> AppState {
    test_state_with_config(db_path, provider, swage_config::ResolvedConfig::default())
}

fn test_state_with_config(
    db_path: &Path,
    provider: impl LlmProvider + 'static,
    config: swage_config::ResolvedConfig,
) -> AppState {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider),
        config,
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

async fn request_json(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = json_response(response).await;
    (status, body)
}

async fn create_conversation(router: &Router) -> Uuid {
    let (status, body) = request_json(router, "POST", "/api/conversations", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().parse().unwrap()
}

fn continuation(session_id: Uuid) -> SessionPolicy {
    SessionPolicy::Continuation { session_id }
}

fn create_request(
    message: &str,
    spec: ScheduleSpec,
    session: SessionPolicy,
) -> CreateScheduleRequest {
    CreateScheduleRequest {
        message: message.into(),
        spec,
        session,
        routing: None,
        gate: None,
    }
}

// ---------------------------------------------------------------------------
// API lifecycle
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn schedule_lifecycle_create_edit_pause_resume_trigger_delete() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("lifecycle.db"),
        MockProvider::new(vec![MockProvider::text_round("manual result")]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    // Create.
    let (status, created) = request_json(
        &router,
        "POST",
        "/api/schedules",
        Some(json!({
            "message": "initial",
            "spec": {"kind": "interval", "every": "every 1 hours"},
            "session": {"policy": "continuation", "session_id": session_id},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["status"], "active");
    assert_eq!(created["spec"]["kind"], "interval");
    assert_eq!(created["session"]["policy"], "continuation");
    // Local-record routing is the default for origin-less schedules.
    assert_eq!(created["routing"]["store_local_output"], true);
    assert_eq!(created["routing"]["deliver_to_origin"], false);
    let schedule_id = created["id"].as_str().unwrap().to_string();

    // Invalid specs are rejected outright.
    let (status, _) = request_json(
        &router,
        "POST",
        "/api/schedules",
        Some(json!({
            "message": "bad",
            "spec": {"kind": "interval", "every": "whenever"},
            "session": {"policy": "continuation", "session_id": session_id},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Edit message and spec.
    let (status, updated) = request_json(
        &router,
        "PUT",
        &format!("/api/schedules/{schedule_id}"),
        Some(json!({
            "message": "edited",
            "spec": {"kind": "interval", "every": "every 2 hours"},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["message"], "edited");
    assert_eq!(updated["spec"]["every"], "every 2 hours");

    // Pause parks it; resume recomputes the next occurrence.
    let (status, paused) = request_json(
        &router,
        "POST",
        &format!("/api/schedules/{schedule_id}/pause"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(paused["status"], "paused");
    let (status, resumed) = request_json(
        &router,
        "POST",
        &format!("/api/schedules/{schedule_id}/resume"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resumed["status"], "active");
    assert!(resumed["next_run_at"].is_string());

    // Manual trigger executes immediately and returns the recorded run.
    let (status, triggered) = request_json(
        &router,
        "POST",
        &format!("/api/schedules/{schedule_id}/trigger"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(triggered["run"]["state"], "completed");
    assert_eq!(triggered["run"]["trigger"], "manual");
    assert_eq!(triggered["run"]["output_text"], "manual result");
    assert_eq!(
        triggered["run"]["session_id"].as_str().unwrap(),
        session_id.to_string()
    );

    let (status, runs) = request_json(
        &router,
        "GET",
        &format!("/api/schedules/{schedule_id}/runs"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);

    // Delete removes the schedule and its history.
    let (status, _) = request_json(
        &router,
        "DELETE",
        &format!("/api/schedules/{schedule_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, listed) = request_json(&router, "GET", "/api/schedules", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["schedules"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// Kinds firing on tick
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn each_schedule_kind_fires_on_tick_and_advances() {
    // Each kind gets its own state so tick timing is deterministic — a
    // shared clock would let a near cron boundary race the assertions.
    let specs = [
        (
            "one_shot",
            ScheduleSpec::OneShot {
                at: (Utc::now() - Duration::seconds(5)).to_rfc3339(),
            },
        ),
        (
            "interval",
            ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
        ),
        (
            "cron",
            ScheduleSpec::Cron {
                cron: "0/5 * * * * * *".into(),
            },
        ),
    ];

    for (kind, spec) in specs {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(
            &dir.path().join(format!("{kind}.db")),
            MockProvider::new(vec![MockProvider::text_round("tick output")]),
        );
        let router = build_router(state.clone());
        let created = schedule::create_schedule(
            &state,
            create_request(
                "fire",
                spec,
                continuation(create_conversation(&router).await),
            ),
        )
        .await
        .unwrap();

        let due_at = created
            .next_run_at
            .expect("every kind computes a first occurrence");
        let executed = schedule::tick_once(&state, due_at).await.unwrap();
        assert_eq!(executed, 1, "{kind} should fire when its occurrence is due");

        let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
        assert_eq!(runs.len(), 1, "{kind} should have recorded one run");
        assert_eq!(runs[0].state, ScheduleRunState::Completed);
        assert_eq!(runs[0].trigger, ScheduleRunTrigger::Tick);
        assert_eq!(runs[0].output_text.as_deref(), Some("tick output"));
        assert!(runs[0].scheduled_for.is_some());

        // One-shots complete; recurring kinds advance to a next occurrence.
        let after = state
            .storage
            .get_schedule(created.id)
            .await
            .unwrap()
            .unwrap();
        if kind == "one_shot" {
            assert_eq!(after.status.to_string(), "completed");
            assert!(after.next_run_at.is_none());
        } else {
            assert_eq!(after.status.to_string(), "active");
            let next = after.next_run_at.expect("recurring kinds advance");
            assert!(next > due_at, "{kind} next occurrence should move forward");
        }
    }
}

// ---------------------------------------------------------------------------
// Session policies
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn fresh_policy_creates_a_session_per_run_and_seeds_previous_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("fresh.db"),
        MockProvider::new(vec![
            MockProvider::text_round("first outcome"),
            MockProvider::text_round("second outcome"),
        ]),
    );
    let bench = state.bench_store.get_or_create_default().await.unwrap();

    let created = schedule::create_schedule(
        &state,
        create_request(
            "Check the queue.",
            ScheduleSpec::Interval {
                every: "every 1 hours".into(),
            },
            SessionPolicy::Fresh {
                bench_id: bench.id,
                agent_id: "gateway".into(),
            },
        ),
    )
    .await
    .unwrap();

    let first = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    let second = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;

    // Each run got its own session, recorded on the run row.
    let first_session = first.session_id.expect("first run records its session");
    let second_session = second.session_id.expect("second run records its session");
    assert_ne!(
        first_session, second_session,
        "fresh policy must not reuse sessions"
    );

    // The first run opens with no history; the second carries the first
    // run's outcome into its deliberately assembled opening message.
    let first_turn = state
        .storage
        .get_turn(first.turn_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    let first_input = first_turn.user_input.unwrap();
    assert!(first_input.starts_with("Check the queue."));
    assert!(first_input.contains("first run of this schedule"));

    let second_turn = state
        .storage
        .get_turn(second.turn_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    let second_input = second_turn.user_input.unwrap();
    assert!(second_input.contains("## Previous run outcome"));
    assert!(second_input.contains("first outcome"));

    // The created sessions belong to the schedule's target agent.
    let session = state
        .storage
        .get_session(first_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.agent_id, "gateway");
    assert_eq!(session.bench_id, bench.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn continuation_policy_reuses_the_named_session() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("continuation.db"),
        MockProvider::new(vec![
            MockProvider::text_round("beat one"),
            MockProvider::text_round("beat two"),
        ]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let created = schedule::create_schedule(
        &state,
        create_request(
            "Anything need attention?",
            ScheduleSpec::Interval {
                every: "every 15 minutes".into(),
            },
            continuation(session_id),
        ),
    )
    .await
    .unwrap();

    let first = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    let second = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    assert_eq!(first.session_id, Some(session_id));
    assert_eq!(second.session_id, Some(session_id));

    // Both turns landed in the same session's thread, as the session's
    // own agent.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    assert_eq!(threads.len(), 1);
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    assert_eq!(turns.len(), 2);
    let session = state
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(turns.iter().all(|turn| turn.agent_id == session.agent_id));
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn gate_skip_verdict_records_the_run_without_waking_the_main_model() {
    let dir = tempfile::tempdir().unwrap();
    let provider = MockProvider::new(vec![]).with_completions(vec![
        r#"{"decision": "skip", "reasoning": "nothing has changed since the last run"}"#,
    ]);
    let completion_requests = provider.completion_requests();
    let state = test_state_with_config(
        &dir.path().join("gate-skip.db"),
        provider,
        config_with_gate_model(),
    );
    let bench = state.bench_store.get_or_create_default().await.unwrap();

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Summarise overnight alerts.".into(),
            spec: ScheduleSpec::Interval {
                every: "every 30 minutes".into(),
            },
            session: SessionPolicy::Fresh {
                bench_id: bench.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: GATE_MODEL.into(),
                criteria: Some("Run only when there are unreviewed alerts.".into()),
            }),
        },
    )
    .await
    .unwrap();

    let run = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;

    // The run is recorded, not silent: skipped, reason gate, verdict and
    // reasoning attached.
    assert_eq!(run.state.to_string(), "skipped");
    assert_eq!(run.skip_reason.unwrap().to_string(), "gate");
    let gate = run.gate.expect("gate decision recorded on the run");
    assert_eq!(gate.verdict, GateVerdict::Skip);
    assert_eq!(gate.reasoning, "nothing has changed since the last run");
    assert_eq!(gate.model, GATE_MODEL);

    // The main model never woke: no session, no turn.
    assert!(run.session_id.is_none());
    assert!(run.turn_id.is_none());

    // The gate saw the task, the criteria, and asked with the resolved
    // provider-facing model id.
    let requests = completion_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model.as_deref(), Some("gate-small-model"));
    let prompt = requests[0].messages[0].text_content();
    assert!(prompt.contains("Summarise overnight alerts."));
    assert!(prompt.contains("Run only when there are unreviewed alerts."));
    drop(requests);

    // The decision is auditable: an explicit gate event plus the state
    // transition the SQL trigger recorded.
    let events = state.storage.list_audit_events().await.unwrap();
    let gate_event = events
        .iter()
        .find(|event| event.event_kind == "gate_decision")
        .expect("gate decision audit event");
    assert_eq!(gate_event.entity_id, run.id.to_string());
    assert_eq!(gate_event.metadata["verdict"], "skip");
    assert!(
        events.iter().any(|event| {
            event.entity_kind == "schedule_run"
                && event.event_kind == "state_transition"
                && event.metadata["to_state"] == "skipped"
                && event.metadata["skip_reason"] == "gate"
        }),
        "state transition to skipped should be audited"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_run_verdict_proceeds_to_execution() {
    let dir = tempfile::tempdir().unwrap();
    let provider = MockProvider::new(vec![MockProvider::text_round("alert summary")])
        .with_completions(vec![
            r#"{"decision": "run", "reasoning": "three unreviewed alerts"}"#,
        ]);
    let state = test_state_with_config(
        &dir.path().join("gate-run.db"),
        provider,
        config_with_gate_model(),
    );
    let bench = state.bench_store.get_or_create_default().await.unwrap();

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Summarise overnight alerts.".into(),
            spec: ScheduleSpec::Interval {
                every: "every 30 minutes".into(),
            },
            session: SessionPolicy::Fresh {
                bench_id: bench.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: GATE_MODEL.into(),
                criteria: None,
            }),
        },
    )
    .await
    .unwrap();

    let run = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    assert_eq!(run.state.to_string(), "completed");
    assert_eq!(run.output_text.as_deref(), Some("alert summary"));
    let gate = run.gate.expect("gate decision recorded even when running");
    assert_eq!(gate.verdict, GateVerdict::Run);
    assert_eq!(gate.reasoning, "three unreviewed alerts");
    assert!(run.session_id.is_some());
    assert!(run.turn_id.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_failure_fails_the_run_instead_of_waking_the_main_model() {
    let dir = tempfile::tempdir().unwrap();
    // No completion replies queued: the gate call errors.
    let provider = MockProvider::new(vec![]);
    let state = test_state_with_config(
        &dir.path().join("gate-error.db"),
        provider,
        config_with_gate_model(),
    );
    let bench = state.bench_store.get_or_create_default().await.unwrap();

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Summarise overnight alerts.".into(),
            spec: ScheduleSpec::Interval {
                every: "every 30 minutes".into(),
            },
            session: SessionPolicy::Fresh {
                bench_id: bench.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: GATE_MODEL.into(),
                criteria: None,
            }),
        },
    )
    .await
    .unwrap();

    let result = schedule::trigger_schedule(&state, created.id).await;
    assert!(result.is_err(), "gate failure should surface as an error");

    let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, ScheduleRunState::Failed);
    assert!(
        runs[0]
            .error_message
            .as_deref()
            .unwrap()
            .contains("gate check failed")
    );
    assert!(runs[0].turn_id.is_none(), "main model must stay asleep");
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_model_must_exist_in_the_catalogue() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("gate-unknown.db"),
        MockProvider::new(vec![]),
    );
    let bench = state.bench_store.get_or_create_default().await.unwrap();

    let result = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "gated".into(),
            spec: ScheduleSpec::Interval {
                every: "every 1 hours".into(),
            },
            session: SessionPolicy::Fresh {
                bench_id: bench.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: "no-such-model".into(),
                criteria: None,
            }),
        },
    )
    .await;
    assert!(
        result.is_err(),
        "unknown gate model should be rejected at creation"
    );
}

// ---------------------------------------------------------------------------
// Overlap
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn due_run_meeting_an_executing_predecessor_is_skipped_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir.path().join("overlap.db"), MockProvider::new(vec![]));
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let created = schedule::create_schedule(
        &state,
        create_request(
            "long task",
            ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            continuation(session_id),
        ),
    )
    .await
    .unwrap();

    // A predecessor is still executing.
    let predecessor = state
        .storage
        .create_schedule_run(NewScheduleRun {
            schedule_id: created.id,
            trigger: ScheduleRunTrigger::Tick,
            scheduled_for: Some(Utc::now()),
        })
        .await
        .unwrap();
    state
        .storage
        .update_schedule_run(
            predecessor.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let run = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    assert_eq!(run.state.to_string(), "skipped");
    assert_eq!(run.skip_reason.unwrap().to_string(), "overlap");
    assert!(run.turn_id.is_none());

    // The predecessor is untouched.
    let predecessor = state
        .storage
        .get_schedule_run(predecessor.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(predecessor.state, ScheduleRunState::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_continuation_session_skips_the_run_deliberately() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir.path().join("busy.db"), MockProvider::new(vec![]));
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    // A user turn is mid-flight in the target session.
    swage_gateway::automated_turn::create_placeholder_turn(
        &state,
        session_id,
        "gateway",
        "user is mid-conversation",
    )
    .await
    .unwrap();

    let created = schedule::create_schedule(
        &state,
        create_request(
            "check in",
            ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            continuation(session_id),
        ),
    )
    .await
    .unwrap();

    let run = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    assert_eq!(run.state.to_string(), "skipped");
    assert_eq!(run.skip_reason.unwrap().to_string(), "overlap");
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn completed_runs_deliver_to_local_and_channel_targets() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("delivery.db"),
        MockProvider::new(vec![MockProvider::text_round("scheduled reply")]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "deliver me".into(),
            spec: ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            session: continuation(session_id),
            routing: Some(DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: vec![DeliveryTarget::Channel {
                    integration:
                        swage_protocol::client::delivery::ExternalChannelIntegration::DevThread,
                    account: "workspace-a".into(),
                    conversation: "room-1/topic-9".into(),
                }],
                store_local_output: true,
            }),
            gate: None,
        },
    )
    .await
    .unwrap();

    schedule::tick_once(&state, created.next_run_at.unwrap())
        .await
        .unwrap();

    let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].output_text.as_deref(), Some("scheduled reply"));

    let (_, delivery_log) = request_json(
        &router,
        "GET",
        &format!("/api/deliveries/log?session_id={session_id}"),
        None,
    )
    .await;
    let deliveries = delivery_log["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0]["target"]["kind"], "channel");
    assert_eq!(deliveries[1]["target"]["kind"], "local_storage");
    assert!(
        deliveries
            .iter()
            .all(|delivery| delivery["content"] == "scheduled reply")
    );
}

// ---------------------------------------------------------------------------
// Restart recovery
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn restart_recovery_fails_runs_orphaned_by_a_previous_process() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir.path().join("recovery.db"), MockProvider::new(vec![]));
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let created = schedule::create_schedule(
        &state,
        create_request(
            "interrupted work",
            ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            continuation(session_id),
        ),
    )
    .await
    .unwrap();

    // Two runs left mid-flight by an unclean shutdown.
    let pending = state
        .storage
        .create_schedule_run(NewScheduleRun {
            schedule_id: created.id,
            trigger: ScheduleRunTrigger::Tick,
            scheduled_for: Some(Utc::now()),
        })
        .await
        .unwrap();
    let running = state
        .storage
        .create_schedule_run(NewScheduleRun {
            schedule_id: created.id,
            trigger: ScheduleRunTrigger::Tick,
            scheduled_for: Some(Utc::now()),
        })
        .await
        .unwrap();
    state
        .storage
        .update_schedule_run(
            running.id,
            ScheduleRunUpdate {
                state: Some(ScheduleRunState::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let counts = swage_gateway::shutdown::recover_orphaned_background_runs(&*state.storage)
        .await
        .unwrap();
    assert_eq!(counts.schedule_runs_failed, 2);

    for run_id in [pending.id, running.id] {
        let run = state
            .storage
            .get_schedule_run(run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert!(
            run.error_message
                .as_deref()
                .unwrap()
                .contains("gateway stopped")
        );
        assert!(run.completed_at.is_some());
    }
}
