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

use futures::StreamExt as _;
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use tokio::sync::oneshot;

use swage_gateway::{schedule, server::build_router, state::AppState};
use swage_protocol::client::delivery::{DeliveryRoutingRequest, DeliveryTarget};
use swage_protocol::client::page::CreatePageRequest;
use swage_protocol::client::schedule::{
    CreateScheduleRequest, GateConfig, GateVerdict, ScheduleCreator, ScheduleSpec, ScheduleStatus,
    SessionPolicy, SkipReason, UpdateScheduleRequest,
};
use swage_provider::LlmProvider;
use swage_storage::{
    NewScheduleRun, NewSpace, ScheduleRunState, ScheduleRunTrigger, ScheduleRunUpdate,
    SqliteStorage,
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
            default_effort: None,
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
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
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

/// Drive one tick to quiescence: `tick_once` now spawns each due run rather
/// than awaiting it, so a test that reads run rows must first await the
/// spawned handles. Returns how many runs the tick spawned.
async fn tick_to_completion(state: &AppState, now: chrono::DateTime<Utc>) -> usize {
    let handles = schedule::tick_once(state, now).await.unwrap();
    let spawned = handles.len();
    for handle in handles {
        handle.await.unwrap();
    }
    spawned
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

    // Every operator mutation leaves a configuration_change audit event
    // with operator provenance, each carrying the prior and new state.
    let configuration_events = state
        .storage
        .list_audit_events()
        .await
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.entity_kind == "schedule"
                && event.entity_id == schedule_id
                && event.event_kind == "configuration_change"
        })
        .collect::<Vec<_>>();
    assert_eq!(configuration_events.len(), 5);
    let mut actions = configuration_events
        .iter()
        .map(|event| event.metadata["action"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    actions.sort();
    assert_eq!(
        actions,
        vec![
            "create_schedule",
            "delete_schedule",
            "pause_schedule",
            "resume_schedule",
            "update_schedule",
        ]
    );
    assert!(configuration_events.iter().all(|event| {
        event.metadata["source"] == "operator_http"
            && (!event.metadata["prior_state"].is_null() || !event.metadata["new_state"].is_null())
    }));
    let delete_event = configuration_events
        .iter()
        .find(|event| event.metadata["action"] == "delete_schedule")
        .unwrap();
    assert!(
        !delete_event.metadata["prior_state"].is_null()
            && delete_event.metadata["new_state"].is_null(),
        "the deletion records the state it removed"
    );
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
            ScheduleCreator::User,
        )
        .await
        .unwrap();

        let due_at = created
            .next_run_at
            .expect("every kind computes a first occurrence");
        let executed = tick_to_completion(&state, due_at).await;
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

/// Provider that stalls any turn whose input contains a marker until the
/// test releases a gate, and streams a fixed reply for every other turn. It
/// proves the tick executes runs concurrently: a stalled run cannot hold up
/// a sibling due at the same instant.
struct HeadOfLineProvider {
    gate: Arc<tokio::sync::Semaphore>,
    stall_marker: &'static str,
}

#[async_trait]
impl LlmProvider for HeadOfLineProvider {
    fn model_id(&self) -> &str {
        "head-of-line-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("head-of-line test declares no gate")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("head-of-line test does not use tool completion")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let stalls = request
            .messages
            .iter()
            .any(|message| message.text_content().contains(self.stall_marker));
        if !stalls {
            return Box::pin(futures::stream::iter(MockProvider::text_round("fast done")));
        }
        // Wait on the gate before producing any output, so this run stays
        // in flight until the test releases a permit.
        let gate = Arc::clone(&self.gate);
        Box::pin(
            futures::stream::once(async move {
                let _permit = gate.acquire().await.expect("gate open");
                futures::stream::iter(MockProvider::text_round("slow done"))
            })
            .flatten(),
        )
    }
}

/// Poll a schedule's newest run until it reaches a terminal state, or panic
/// on timeout. Used to observe one run finishing while another is stalled.
async fn await_terminal_run(state: &AppState, schedule_id: Uuid) -> swage_storage::ScheduleRun {
    // This is a harness safety ceiling, not a product latency guarantee. A
    // full parallel test run can leave an otherwise immediate mock turn
    // waiting for CPU long enough to cross the old five-second deadline.
    let deadline = std::time::Duration::from_secs(45);
    let started = std::time::Instant::now();
    loop {
        if let Some(run) = state
            .storage
            .list_schedule_runs(schedule_id)
            .await
            .unwrap()
            .into_iter()
            .next_back()
        {
            let terminal = matches!(
                run.state,
                ScheduleRunState::Completed
                    | ScheduleRunState::CompletedEmpty
                    | ScheduleRunState::Failed
                    | ScheduleRunState::Skipped
            );
            if terminal {
                return run;
            }
        }
        assert!(
            started.elapsed() < deadline,
            "run for schedule {schedule_id} never reached a terminal state"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_due_schedules_do_not_head_of_line_block() {
    let dir = tempfile::tempdir().unwrap();
    let due_at = Utc::now() - Duration::seconds(5);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let state = test_state(
        &dir.path().join("head-of-line.db"),
        HeadOfLineProvider {
            gate: Arc::clone(&gate),
            stall_marker: "STALL",
        },
    );
    let router = build_router(state.clone());
    let slow_session = create_conversation(&router).await;
    let fast_session = create_conversation(&router).await;

    // Two one-shot schedules due at the same instant: the slow one's turn
    // blocks on the gate, the fast one streams straight through.
    let slow = schedule::create_schedule(
        &state,
        create_request(
            "STALL until released",
            ScheduleSpec::OneShot {
                at: due_at.to_rfc3339(),
            },
            continuation(slow_session),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();
    let fast = schedule::create_schedule(
        &state,
        create_request(
            "run promptly",
            ScheduleSpec::OneShot {
                at: due_at.to_rfc3339(),
            },
            continuation(fast_session),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    // tick_once returns as soon as both runs are spawned; it does not wait
    // for them. A serial tick would block here inside the slow run.
    let handles = schedule::tick_once(&state, Utc::now()).await.unwrap();
    assert_eq!(handles.len(), 2);

    // The fast schedule completes while the slow one is still gated — the
    // proof that the slow run did not hold up the tick.
    let fast_run = await_terminal_run(&state, fast.id).await;
    assert_eq!(fast_run.state, ScheduleRunState::Completed);
    assert_eq!(fast_run.output_text.as_deref(), Some("fast done"));

    let slow_run = state
        .storage
        .list_schedule_runs(slow.id)
        .await
        .unwrap()
        .pop()
        .expect("slow run row exists");
    assert!(
        matches!(
            slow_run.state,
            ScheduleRunState::Pending | ScheduleRunState::Running
        ),
        "slow run should still be in flight while the fast run has finished, was {:?}",
        slow_run.state
    );

    // Release the slow run and let both spawned tasks finish cleanly.
    gate.add_permits(1);
    for handle in handles {
        handle.await.unwrap();
    }
    let slow_run = await_terminal_run(&state, slow.id).await;
    assert_eq!(slow_run.state, ScheduleRunState::Completed);
    assert_eq!(slow_run.output_text.as_deref(), Some("slow done"));
}

/// Panics inside `complete` — the gate call, which the run awaits directly in
/// its own spawned task (unlike `stream`, which the turn engine drives on a
/// nested task). Proves a *panicking* run, not merely a failing one, is caught
/// on the schedule's join handle without taking the tick down. `stream` serves
/// a clean reply for the ungated sibling.
struct PanicProvider;

#[async_trait]
impl LlmProvider for PanicProvider {
    fn model_id(&self) -> &str {
        "panic-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        panic!("injected panic inside a scheduled run's gate check");
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("panic test does not use tool completion")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(MockProvider::text_round(
            "survivor done",
        )))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_spawned_run_is_caught_and_the_tick_survives() {
    let dir = tempfile::tempdir().unwrap();
    let due_at = Utc::now() - Duration::seconds(5);
    let state = test_state_with_config(
        &dir.path().join("panic.db"),
        PanicProvider,
        config_with_gate_model(),
    );
    let router = build_router(state.clone());
    let panic_session = create_conversation(&router).await;
    let survivor_session = create_conversation(&router).await;

    // The panicking schedule declares a gate, so its run calls the (panicking)
    // gate model inline before any turn machinery.
    let panicking = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            gate: Some(GateConfig {
                model: GATE_MODEL.into(),
                criteria: None,
            }),
            ..create_request(
                "this run panics in the gate",
                ScheduleSpec::OneShot {
                    at: due_at.to_rfc3339(),
                },
                continuation(panic_session),
            )
        },
        ScheduleCreator::User,
    )
    .await
    .unwrap();
    let survivor = schedule::create_schedule(
        &state,
        create_request(
            "this run just works",
            ScheduleSpec::OneShot {
                at: due_at.to_rfc3339(),
            },
            continuation(survivor_session),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    // tick_once returns without panicking, even though one of the runs it
    // spawned will panic — a panic in a run does not take the tick down.
    let handles = schedule::tick_once(&state, Utc::now()).await.unwrap();
    assert_eq!(handles.len(), 2);

    // Exactly one join handle carries a panic (the mechanism the loop reaps to
    // surface it); the other completes cleanly.
    let mut panicked = 0;
    let mut completed = 0;
    for handle in handles {
        match handle.await {
            Err(join_error) if join_error.is_panic() => panicked += 1,
            Ok(()) => completed += 1,
            Err(other) => panic!("unexpected join error: {other}"),
        }
    }
    assert_eq!(
        panicked, 1,
        "the panicking run's handle must report a panic"
    );
    assert_eq!(completed, 1, "the sibling run must finish cleanly");

    // The surviving schedule ran to completion; the panicking one's run row
    // was left non-terminal (the task died mid-run) — recovered at next
    // startup, exactly like a crash.
    let survivor_run = await_terminal_run(&state, survivor.id).await;
    assert_eq!(survivor_run.state, ScheduleRunState::Completed);
    assert_eq!(survivor_run.output_text.as_deref(), Some("survivor done"));
    let panic_runs = state
        .storage
        .list_schedule_runs(panicking.id)
        .await
        .unwrap();
    assert_eq!(panic_runs.len(), 1);
    assert!(matches!(
        panic_runs[0].state,
        ScheduleRunState::Pending | ScheduleRunState::Running
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_continues_after_a_due_schedule_run_fails() {
    let dir = tempfile::tempdir().unwrap();
    let due_at = Utc::now() - Duration::seconds(5);
    let state = test_state(
        &dir.path().join("tick-continues-after-failure.db"),
        MockProvider::new(vec![
            vec![StreamChunk::Error {
                message: "provider down".into(),
            }],
            MockProvider::text_round("later schedule still ran"),
        ]),
    );
    let router = build_router(state.clone());
    let first_session = create_conversation(&router).await;
    let second_session = create_conversation(&router).await;

    let first = schedule::create_schedule(
        &state,
        create_request(
            "this run will fail",
            ScheduleSpec::OneShot {
                at: due_at.to_rfc3339(),
            },
            continuation(first_session),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();
    let second = schedule::create_schedule(
        &state,
        create_request(
            "this run must still happen",
            ScheduleSpec::OneShot {
                at: due_at.to_rfc3339(),
            },
            continuation(second_session),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    let executed = tick_to_completion(&state, Utc::now()).await;
    assert_eq!(
        executed, 2,
        "one failing due schedule must not abort the rest of the tick"
    );

    // Runs are spawned concurrently, so which of the two schedules pops the
    // failing provider round is a race — the guarantee under test is
    // isolation, not order: exactly one run fails and the other completes,
    // each with a single Tick-triggered run.
    let first_runs = state.storage.list_schedule_runs(first.id).await.unwrap();
    let second_runs = state.storage.list_schedule_runs(second.id).await.unwrap();
    assert_eq!(first_runs.len(), 1);
    assert_eq!(second_runs.len(), 1);
    let (failed, completed) = match first_runs[0].state {
        ScheduleRunState::Failed => (&first_runs[0], &second_runs[0]),
        _ => (&second_runs[0], &first_runs[0]),
    };
    assert_eq!(failed.trigger, ScheduleRunTrigger::Tick);
    assert_eq!(failed.state, ScheduleRunState::Failed);
    assert!(failed.error_message.is_some());
    assert!(failed.completed_at.is_some());
    let deliveries = state.storage.list_delivery_records().await.unwrap();
    let no_delivery = deliveries
        .iter()
        .find(|delivery| delivery.run_id == Some(failed.id))
        .expect("a run failing before delivery records why it made no delivery");
    assert_eq!(no_delivery.status, "not_delivered");
    assert_eq!(no_delivery.error, failed.error_message);
    assert_eq!(completed.trigger, ScheduleRunTrigger::Tick);
    assert_eq!(completed.state, ScheduleRunState::Completed);
    assert_eq!(
        completed.output_text.as_deref(),
        Some("later schedule still ran")
    );

    for schedule_id in [first.id, second.id] {
        let schedule = state
            .storage
            .get_schedule(schedule_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(schedule.status, ScheduleStatus::Completed);
        assert!(schedule.next_run_at.is_none());
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
    let viewing_space = state.space_store.get_or_create_default().await.unwrap();
    let target_space = state
        .space_store
        .create(swage_storage::NewSpace {
            name: "Scheduled work target".into(),
            slug: "scheduled-work-target".into(),
        })
        .await
        .unwrap();
    assert_ne!(viewing_space.id, target_space.id);

    let created = schedule::create_schedule(
        &state,
        create_request(
            "Check the queue.",
            ScheduleSpec::Interval {
                every: "every 1 hours".into(),
            },
            SessionPolicy::Fresh {
                space_id: target_space.id,
                agent_id: "gateway".into(),
            },
        ),
        ScheduleCreator::User,
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
    assert_eq!(session.space_id, target_space.id);
    assert_ne!(session.space_id, viewing_space.id);
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
        ScheduleCreator::User,
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
    let space = state.space_store.get_or_create_default().await.unwrap();

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Summarise overnight alerts.".into(),
            spec: ScheduleSpec::Interval {
                every: "every 30 minutes".into(),
            },
            session: SessionPolicy::Fresh {
                space_id: space.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: GATE_MODEL.into(),
                criteria: Some("Run only when there are unreviewed alerts.".into()),
            }),
        },
        ScheduleCreator::User,
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
    let deliveries = state.storage.list_delivery_records().await.unwrap();
    let no_delivery = deliveries
        .iter()
        .find(|delivery| delivery.run_id == Some(run.id))
        .expect("a gated-out run records why it made no delivery");
    assert_eq!(no_delivery.status, "not_delivered");
    assert_eq!(no_delivery.session_id, None);
    assert_eq!(no_delivery.target["kind"], "no_delivery");
    assert!(no_delivery.error.as_deref().unwrap().contains("no work"));

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
    let space = state.space_store.get_or_create_default().await.unwrap();

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Summarise overnight alerts.".into(),
            spec: ScheduleSpec::Interval {
                every: "every 30 minutes".into(),
            },
            session: SessionPolicy::Fresh {
                space_id: space.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: GATE_MODEL.into(),
                criteria: None,
            }),
        },
        ScheduleCreator::User,
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
    let space = state.space_store.get_or_create_default().await.unwrap();

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "Summarise overnight alerts.".into(),
            spec: ScheduleSpec::Interval {
                every: "every 30 minutes".into(),
            },
            session: SessionPolicy::Fresh {
                space_id: space.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: GATE_MODEL.into(),
                criteria: None,
            }),
        },
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    // A gate transport failure is a run failure, not a raised error: the
    // trigger returns the recorded failed run (item 4's contract), with the
    // main model left asleep.
    let run = schedule::trigger_schedule(&state, created.id)
        .await
        .expect("a failed run is returned, not raised")
        .run;
    assert_eq!(run.state, ScheduleRunState::Failed);
    assert!(run.completed_at.is_some());
    assert!(
        run.error_message
            .as_deref()
            .unwrap()
            .contains("gate check failed")
    );
    assert!(run.turn_id.is_none(), "main model must stay asleep");

    let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
    assert_eq!(runs.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_model_must_exist_in_the_catalogue() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("gate-unknown.db"),
        MockProvider::new(vec![]),
    );
    let space = state.space_store.get_or_create_default().await.unwrap();

    let result = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "gated".into(),
            spec: ScheduleSpec::Interval {
                every: "every 1 hours".into(),
            },
            session: SessionPolicy::Fresh {
                space_id: space.id,
                agent_id: "gateway".into(),
            },
            routing: None,
            gate: Some(GateConfig {
                model: "no-such-model".into(),
                criteria: None,
            }),
        },
        ScheduleCreator::User,
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
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    // A predecessor is still executing.
    let predecessor = state
        .storage
        .create_schedule_run(NewScheduleRun::pending(
            created.id,
            ScheduleRunTrigger::Tick,
            Some(Utc::now()),
        ))
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
        ScheduleCreator::User,
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
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    tick_to_completion(&state, created.next_run_at.unwrap()).await;

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
    assert!(deliveries.iter().all(|delivery| {
        delivery["run_id"] == runs[0].id.to_string()
            && delivery["turn_id"] == runs[0].turn_id.unwrap().to_string()
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_runs_deliver_to_a_page_through_the_page_substrate() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("page-delivery.db"),
        MockProvider::new(vec![MockProvider::text_round("scheduled page output")]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;
    let space = state.space_store.get_or_create_default().await.unwrap();
    let page = swage_gateway::page::create_page(
        &state,
        space.id,
        CreatePageRequest {
            title: "Schedule output".into(),
        },
    )
    .await
    .unwrap();
    let mut events = state.broadcast.subscribe();

    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            routing: Some(DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: vec![DeliveryTarget::Page { page_id: page.id }],
                store_local_output: false,
            }),
            ..create_request(
                "write the scheduled output",
                ScheduleSpec::Interval {
                    every: "every 5 seconds".into(),
                },
                continuation(session_id),
            )
        },
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    tick_to_completion(&state, created.next_run_at.unwrap()).await;

    let modules = state.storage.list_page_modules(page.id).await.unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].source, "scheduled page output");
    let event = events.recv().await.unwrap();
    assert!(matches!(
        event,
        swage_protocol::client::websocket::ServerEvent::PageUpdated {
            page_id,
            module_id: Some(_),
        } if page_id == page.id
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn page_delivery_rejects_a_page_outside_the_schedules_space() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("cross-space-page.db"),
        MockProvider::new(vec![]),
    );
    let default = state.space_store.get_or_create_default().await.unwrap();
    let other = state
        .space_store
        .create(NewSpace {
            name: "Elsewhere".into(),
            slug: "elsewhere".parse().unwrap(),
        })
        .await
        .unwrap();
    let page = swage_gateway::page::create_page(
        &state,
        other.id,
        CreatePageRequest {
            title: "Other space".into(),
        },
    )
    .await
    .unwrap();

    let error = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            message: "write elsewhere".into(),
            spec: ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            session: SessionPolicy::Fresh {
                space_id: default.id,
                agent_id: "gateway".into(),
            },
            routing: Some(DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: vec![DeliveryTarget::Page { page_id: page.id }],
                store_local_output: false,
            }),
            gate: None,
        },
        ScheduleCreator::User,
    )
    .await
    .expect_err("cross-space page delivery must be refused at schedule creation");
    assert!(error.to_string().contains("schedule's space"));
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_successful_run_records_a_distinct_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("empty-run.db"),
        MockProvider::new(vec![MockProvider::text_round("")]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let created = schedule::create_schedule(
        &state,
        create_request(
            "produce no visible text",
            ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            continuation(session_id),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    tick_to_completion(&state, created.next_run_at.unwrap()).await;

    let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, ScheduleRunState::CompletedEmpty);
    assert_eq!(runs[0].output_text.as_deref(), Some(""));
    assert!(runs[0].completed_at.is_some());

    let api = schedule::list_schedule_runs(&state, created.id)
        .await
        .unwrap();
    assert_eq!(api.runs[0].state.to_string(), "completed_empty");
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

    // Two schedules, each left with a run mid-flight by an unclean
    // shutdown. They sit on separate schedules because the one-in-flight
    // guard permits only one non-terminal run per schedule; recovery sweeps
    // the whole table regardless.
    let make_schedule = || {
        schedule::create_schedule(
            &state,
            create_request(
                "interrupted work",
                ScheduleSpec::Interval {
                    every: "every 5 seconds".into(),
                },
                continuation(session_id),
            ),
            ScheduleCreator::User,
        )
    };
    let schedule_a = make_schedule().await.unwrap();
    let schedule_b = make_schedule().await.unwrap();

    let pending = state
        .storage
        .create_schedule_run(NewScheduleRun::pending(
            schedule_a.id,
            ScheduleRunTrigger::Tick,
            Some(Utc::now()),
        ))
        .await
        .unwrap();
    let running = state
        .storage
        .create_schedule_run(NewScheduleRun::pending(
            schedule_b.id,
            ScheduleRunTrigger::Tick,
            Some(Utc::now()),
        ))
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

// ---------------------------------------------------------------------------
// One-shot lifecycle (item 1)
// ---------------------------------------------------------------------------

/// A one-shot request at a given moment.
fn one_shot_request(session_id: Uuid, at: chrono::DateTime<Utc>) -> CreateScheduleRequest {
    create_request(
        "one and done",
        ScheduleSpec::OneShot {
            at: at.to_rfc3339(),
        },
        continuation(session_id),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn paused_future_one_shot_resumes_and_still_fires() {
    // The regression: pausing then resuming a one-shot whose moment is still
    // ahead used to complete it, silently cancelling a pending run. It must
    // resume active and keep its moment.
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("resume-future.db"),
        MockProvider::new(vec![]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let moment = Utc::now() + Duration::hours(1);
    let created = schedule::create_schedule(
        &state,
        one_shot_request(session_id, moment),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    schedule::pause_schedule(&state, created.id).await.unwrap();
    let resumed = schedule::resume_schedule(&state, created.id).await.unwrap();

    assert_eq!(resumed.status, ScheduleStatus::Active);
    assert_eq!(
        resumed.next_run_at.map(|t| t.to_rfc3339()),
        Some(moment.to_rfc3339()),
        "a future one-shot keeps its original moment on resume"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn paused_lapsed_one_shot_resumes_completed_without_firing() {
    // A never-fired one-shot whose moment passed while paused completes on
    // resume — deliberately, without a surprise late run.
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("resume-lapsed.db"),
        MockProvider::new(vec![]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let lapsed = Utc::now() - Duration::hours(1);
    let created = schedule::create_schedule(
        &state,
        one_shot_request(session_id, lapsed),
        ScheduleCreator::User,
    )
    .await
    .unwrap();
    schedule::pause_schedule(&state, created.id).await.unwrap();

    let resumed = schedule::resume_schedule(&state, created.id).await.unwrap();
    assert_eq!(resumed.status, ScheduleStatus::Completed);
    assert!(resumed.next_run_at.is_none());

    let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
    assert!(
        runs.is_empty(),
        "resuming a lapsed one-shot must not fire it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fired_one_shot_never_refires_via_pause_resume_or_unchanged_update() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("fired-oneshot.db"),
        MockProvider::new(vec![MockProvider::text_round("first")]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let moment = Utc::now() - Duration::seconds(1);
    let created = schedule::create_schedule(
        &state,
        one_shot_request(session_id, moment),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    // Fire it.
    tick_to_completion(&state, Utc::now()).await;
    let after_fire = state
        .storage
        .get_schedule(created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_fire.status, ScheduleStatus::Completed);
    assert_eq!(
        state
            .storage
            .list_schedule_runs(created.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // Pause/resume must not revive a fired one-shot.
    schedule::pause_schedule(&state, created.id).await.unwrap();
    let resumed = schedule::resume_schedule(&state, created.id).await.unwrap();
    assert_eq!(resumed.status, ScheduleStatus::Completed);

    // An update that leaves the moment unchanged must not revive it either.
    schedule::update_schedule(
        &state,
        created.id,
        UpdateScheduleRequest {
            message: Some("still done".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    tick_to_completion(&state, Utc::now()).await;
    assert_eq!(
        state
            .storage
            .list_schedule_runs(created.id)
            .await
            .unwrap()
            .len(),
        1,
        "a fired one-shot must never fire again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn updating_one_shot_moment_to_future_rearms_and_fires_once_more() {
    // The one sanctioned re-arm: fired -> update to a future moment ->
    // fires exactly once more.
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("rearm-oneshot.db"),
        MockProvider::new(vec![
            MockProvider::text_round("first"),
            MockProvider::text_round("second"),
        ]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let created = schedule::create_schedule(
        &state,
        one_shot_request(session_id, Utc::now() - Duration::seconds(1)),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    tick_to_completion(&state, Utc::now()).await;
    assert_eq!(
        state
            .storage
            .get_schedule(created.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ScheduleStatus::Completed
    );

    // Re-arm to a future moment: revives from completed.
    let rearm_at = Utc::now() + Duration::seconds(1);
    let rearmed = schedule::update_schedule(
        &state,
        created.id,
        UpdateScheduleRequest {
            spec: Some(ScheduleSpec::OneShot {
                at: rearm_at.to_rfc3339(),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rearmed.status, ScheduleStatus::Active);
    assert_eq!(
        rearmed.next_run_at.map(|t| t.to_rfc3339()),
        Some(rearm_at.to_rfc3339())
    );

    // The re-armed moment fires once more, then the schedule is spent again.
    tick_to_completion(&state, rearm_at).await;
    let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
    assert_eq!(runs.len(), 2, "re-armed one-shot fires exactly once more");
    assert!(
        runs.iter()
            .all(|run| run.state == ScheduleRunState::Completed)
    );
    assert_eq!(
        state
            .storage
            .get_schedule(created.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ScheduleStatus::Completed
    );
}

// ---------------------------------------------------------------------------
// Terminal run state on every failure seam (item 2) + trigger contract (item 4)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn trigger_returns_the_recorded_failed_run_over_http() {
    // The endpoint's contract: a valid request returns the recorded run in
    // whatever terminal state it reached — a failed run comes back 202 with
    // its failure in the body, never as a 4xx (item 4). The run here fails
    // at delivery, after producing output.
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("trigger-failed.db"),
        MockProvider::new(vec![MockProvider::text_round("produced output")]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;
    let (_, created) = request_json(
        &router,
        "POST",
        "/api/schedules",
        Some(json!({
            "message": "undeliverable",
            "spec": {"kind": "interval", "every": "every 5 seconds"},
            "session": {"policy": "continuation", "session_id": session_id},
            "routing": {
                "deliver_to_origin": false,
                "home_target": null,
                "explicit_targets": [{"kind": "webhook", "url": "not-a-valid-url"}],
                "store_local_output": false
            },
        })),
    )
    .await;
    let schedule_id = created["id"].as_str().unwrap();

    let (status, body) = request_json(
        &router,
        "POST",
        &format!("/api/schedules/{schedule_id}/trigger"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a valid request returns the run even when the run failed"
    );
    assert_eq!(body["run"]["state"], "failed");
    assert!(body["run"]["error_message"].is_string());
    assert!(body["run"]["completed_at"].is_string());
    // The output the turn produced survives on the failed run.
    assert_eq!(body["run"]["output_text"], "produced output");
}

#[tokio::test(flavor = "multi_thread")]
async fn delivery_failure_preserves_output_on_the_failed_run() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(
        &dir.path().join("delivery-fail.db"),
        MockProvider::new(vec![MockProvider::text_round("hard-won output")]),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    // Route to a webhook whose URL cannot even be parsed: delivery fails
    // after the turn has already produced output.
    let created = schedule::create_schedule(
        &state,
        CreateScheduleRequest {
            routing: Some(DeliveryRoutingRequest {
                deliver_to_origin: false,
                home_target: None,
                explicit_targets: vec![DeliveryTarget::Webhook {
                    url: "not-a-valid-url".into(),
                }],
                store_local_output: false,
            }),
            ..create_request(
                "undeliverable",
                ScheduleSpec::Interval {
                    every: "every 5 seconds".into(),
                },
                continuation(session_id),
            )
        },
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    let run = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    assert_eq!(run.state, ScheduleRunState::Failed);
    assert_eq!(
        run.output_text.as_deref(),
        Some("hard-won output"),
        "output the turn produced stays on the failed run"
    );
    assert!(
        run.error_message
            .as_deref()
            .unwrap()
            .contains("delivery failed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_on_unknown_schedule_is_a_request_error() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir.path().join("unknown.db"), MockProvider::new(vec![]));
    let router = build_router(state.clone());

    let (status, _) = request_json(
        &router,
        "POST",
        &format!("/api/schedules/{}/trigger", Uuid::new_v4()),
        None,
    )
    .await;
    assert!(
        status.is_client_error(),
        "unknown schedule is a 4xx, got {status}"
    );
}

// ---------------------------------------------------------------------------
// Atomic overlap guard, concurrent racers (item 3)
// ---------------------------------------------------------------------------

/// A provider whose single stream emits one delta, signals that it has
/// started, then blocks until released — so a run can be pinned in-flight
/// while a racing trigger observes the overlap.
struct StallProvider {
    started: Mutex<Option<oneshot::Sender<()>>>,
    release: Mutex<Option<oneshot::Receiver<()>>>,
    output: String,
}

impl StallProvider {
    fn new(output: &str, started: oneshot::Sender<()>, release: oneshot::Receiver<()>) -> Self {
        Self {
            started: Mutex::new(Some(started)),
            release: Mutex::new(Some(release)),
            output: output.into(),
        }
    }
}

#[async_trait]
impl LlmProvider for StallProvider {
    fn model_id(&self) -> &str {
        "stall-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("overlap tests do not gate")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("overlap tests do not use tool completion")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        let release = self.release.lock().unwrap().take();
        let output = self.output.clone();
        Box::pin(
            futures::stream::once(async move { StreamChunk::Delta { content: output } }).chain(
                futures::stream::once(async move {
                    if let Some(release) = release {
                        let _ = release.await;
                    }
                    StreamChunk::Done {
                        finish_reason: Some(FinishReason::Stop),
                    }
                }),
            ),
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_manual_triggers_run_one_and_skip_the_overlap_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let state = test_state(
        &dir.path().join("race.db"),
        StallProvider::new("held output", started_tx, release_rx),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;
    let created = schedule::create_schedule(
        &state,
        create_request(
            "racing",
            ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            continuation(session_id),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();

    // The winner claims the in-flight slot and stalls mid-turn.
    let winner_state = state.clone();
    let winner_id = created.id;
    let winner =
        tokio::spawn(async move { schedule::trigger_schedule(&winner_state, winner_id).await });
    started_rx
        .await
        .expect("winner's turn should start before the racer triggers");

    // The racer trips the atomic guard and records a clean overlap skip.
    let loser = schedule::trigger_schedule(&state, created.id)
        .await
        .unwrap()
        .run;
    assert_eq!(loser.state, ScheduleRunState::Skipped);
    assert_eq!(loser.skip_reason, Some(SkipReason::Overlap));
    assert!(loser.turn_id.is_none());
    let message = loser.error_message.as_deref().unwrap_or_default();
    assert!(!message.is_empty(), "overlap skip carries a clean message");
    assert!(
        !message.contains("UNIQUE constraint"),
        "overlap skips must never leak raw constraint text, got: {message}"
    );

    // Release the winner: it completes normally.
    release_tx.send(()).unwrap();
    let winner_run = winner.await.unwrap().unwrap().run;
    assert_eq!(winner_run.state, ScheduleRunState::Completed);
    assert_eq!(winner_run.output_text.as_deref(), Some("held output"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_racing_an_in_flight_manual_run_records_a_clean_overlap_skip() {
    // The other live interleaving: the serial tick loop firing a due schedule
    // while a manual trigger already holds it in flight. From the index's
    // side this is the same insert-against-a-non-terminal-row collision as
    // manual-vs-manual — the trigger kind is irrelevant to the guard — so the
    // tick's occurrence records a terminal overlap skip rather than executing
    // a second time.
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let state = test_state(
        &dir.path().join("race-tick.db"),
        StallProvider::new("held output", started_tx, release_rx),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;
    let created = schedule::create_schedule(
        &state,
        create_request(
            "racing the tick",
            ScheduleSpec::Interval {
                every: "every 5 seconds".into(),
            },
            continuation(session_id),
        ),
        ScheduleCreator::User,
    )
    .await
    .unwrap();
    let due_at = created
        .next_run_at
        .expect("an interval has a first occurrence");

    // A manual trigger claims the in-flight slot and stalls mid-turn.
    let winner_state = state.clone();
    let winner_id = created.id;
    let winner =
        tokio::spawn(async move { schedule::trigger_schedule(&winner_state, winner_id).await });
    started_rx
        .await
        .expect("the manual turn should start before the tick fires");

    // The tick finds the schedule due, tries to run it, and trips the guard.
    let executed = tick_to_completion(&state, due_at).await;
    assert_eq!(executed, 1);

    let runs = state.storage.list_schedule_runs(created.id).await.unwrap();
    let skip = runs
        .iter()
        .find(|run| run.trigger == ScheduleRunTrigger::Tick)
        .expect("the tick recorded a run");
    assert_eq!(skip.state, ScheduleRunState::Skipped);
    assert_eq!(skip.skip_reason, Some(SkipReason::Overlap));
    assert!(skip.turn_id.is_none());
    let message = skip.error_message.as_deref().unwrap_or_default();
    assert!(
        !message.is_empty() && !message.contains("UNIQUE constraint"),
        "overlap skips must never leak raw constraint text, got: {message}"
    );
    let deliveries = state.storage.list_delivery_records().await.unwrap();
    let no_delivery = deliveries
        .iter()
        .find(|delivery| delivery.run_id == Some(skip.id))
        .expect("an overlap-skipped run records why it made no delivery");
    assert_eq!(no_delivery.status, "not_delivered");
    assert!(no_delivery.error.as_deref().unwrap().contains("in flight"));

    // Release the winning manual run: it completes normally.
    release_tx.send(()).unwrap();
    let winner_run = winner.await.unwrap().unwrap().run;
    assert_eq!(winner_run.state, ScheduleRunState::Completed);
}

// ---------------------------------------------------------------------------
// Gate-update tri-state wire contract (item 6)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn gate_update_tristate_round_trips_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state_with_config(
        &dir.path().join("gate-tristate.db"),
        MockProvider::new(vec![]),
        config_with_gate_model(),
    );
    let router = build_router(state.clone());
    let session_id = create_conversation(&router).await;

    let (status, created) = request_json(
        &router,
        "POST",
        "/api/schedules",
        Some(json!({
            "message": "clearable gate",
            "spec": {"kind": "interval", "every": "every 5 seconds"},
            "session": {"policy": "continuation", "session_id": session_id},
            "gate": {"model": GATE_MODEL, "criteria": "only when there is work"},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["gate"]["model"], GATE_MODEL);
    let schedule_id = created["id"].as_str().unwrap().to_string();

    // Absent gate field leaves it untouched.
    let (status, unchanged) = request_json(
        &router,
        "PUT",
        &format!("/api/schedules/{schedule_id}"),
        Some(json!({"message": "renamed"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unchanged["message"], "renamed");
    assert_eq!(unchanged["gate"]["model"], GATE_MODEL);

    // Clear removes it.
    let (status, cleared) = request_json(
        &router,
        "PUT",
        &format!("/api/schedules/{schedule_id}"),
        Some(json!({"gate": {"action": "clear"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(cleared["gate"].is_null());

    // Set installs a fresh gate, criteria and all.
    let (status, set) = request_json(
        &router,
        "PUT",
        &format!("/api/schedules/{schedule_id}"),
        Some(json!({"gate": {"action": "set", "model": GATE_MODEL, "criteria": "revised"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(set["gate"]["model"], GATE_MODEL);
    assert_eq!(set["gate"]["criteria"], "revised");
}
