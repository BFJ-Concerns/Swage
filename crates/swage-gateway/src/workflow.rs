//! Durable workflow HTTP surface and detached engine supervision.

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use swage_protocol::client::workflow::{
    CreateWorkflowScriptRequest, StartInlineWorkflowRunRequest, StartWorkflowRunRequest,
    UpdateWorkflowScriptRequest, WorkflowBudget, WorkflowRun, WorkflowRunDetailResponse,
    WorkflowRunListResponse, WorkflowRunState, WorkflowScript, WorkflowScriptListResponse,
};
use swage_storage::{
    NewAuditEvent, NewSession, NewThread, NewWorkflowJournalIntent, NewWorkflowRun,
    NewWorkflowScript, ThreadStore, WorkflowRunRecord, WorkflowScriptRecord,
};
use swage_workflow::{
    AgentCallRequest, AgentCallResult, EffectPayload, EffectReceipt, EngineLimits, JournalReplay,
    WorkflowEngine, WorkflowHost,
};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    error::ApiError, state::AppState, subscription::SubscriptionState, websocket::ServerEvent,
};

#[async_trait::async_trait]
pub trait WorkflowLauncher: Send + Sync {
    async fn start_by_name(
        &self,
        session_id: Uuid,
        name: &str,
        args: Value,
        budget: Option<WorkflowBudget>,
        wait: bool,
    ) -> Result<Value, String>;
}

pub struct GatewayWorkflowLauncher {
    state: AppState,
}

impl GatewayWorkflowLauncher {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait::async_trait]
impl WorkflowLauncher for GatewayWorkflowLauncher {
    async fn start_by_name(
        &self,
        session_id: Uuid,
        name: &str,
        args: Value,
        budget: Option<WorkflowBudget>,
        wait: bool,
    ) -> Result<Value, String> {
        let session = self
            .state
            .storage
            .get_session(session_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("session {session_id} was not found"))?;
        let script = self
            .state
            .storage
            .list_workflow_scripts(session.space_id)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|script| script.name == name)
            .ok_or_else(|| format!("workflow script {name:?} was not found"))?;
        let version = self
            .state
            .storage
            .get_workflow_script_head(script.id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "workflow script has no version".to_string())?;
        let run = start_run(
            self.state.clone(),
            session.space_id,
            Some(version.id),
            version.source,
            args,
            budget.unwrap_or_default(),
            script.delivery_grant,
        )
        .await
        .map_err(|error| error.0.to_string())?;
        if !wait {
            return serde_json::to_value(run).map_err(|error| error.to_string());
        }
        let record = tokio::time::timeout(std::time::Duration::from_secs(600), async {
            loop {
                let record = self
                    .state
                    .storage
                    .get_workflow_run(run.id)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "workflow run disappeared".to_string())?;
                if record.state != "running" {
                    return Ok::<_, String>(record);
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "workflow wait timed out".to_string())??;
        serde_json::to_value(run_wire(record).map_err(|error| error.0.to_string())?)
            .map_err(|error| error.to_string())
    }
}

enum HostCommand {
    Record {
        kind: String,
        message: String,
    },
    Agent {
        request: AgentCallRequest,
        cancellation: CancellationToken,
        reply: oneshot::Sender<Result<AgentCallResult, String>>,
    },
    Deliver {
        payload: EffectPayload,
        cancellation: CancellationToken,
        reply: oneshot::Sender<Result<EffectReceipt, String>>,
    },
}

struct ChannelWorkflowHost {
    tx: mpsc::UnboundedSender<HostCommand>,
}

#[async_trait::async_trait]
impl WorkflowHost for ChannelWorkflowHost {
    fn record(&self, kind: &str, message: String) {
        let _ = self.tx.send(HostCommand::Record {
            kind: kind.into(),
            message,
        });
    }

    async fn run_agent_call(
        &self,
        request: AgentCallRequest,
        cancellation: CancellationToken,
    ) -> Result<AgentCallResult, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(HostCommand::Agent {
                request,
                cancellation,
                reply,
            })
            .map_err(|_| "workflow host stopped".to_string())?;
        receive
            .await
            .map_err(|_| "workflow host dropped agent reply".to_string())?
    }

    async fn deliver(
        &self,
        payload: EffectPayload,
        cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(HostCommand::Deliver {
                payload,
                cancellation,
                reply,
            })
            .map_err(|_| "workflow host stopped".to_string())?;
        receive
            .await
            .map_err(|_| "workflow host dropped delivery reply".to_string())?
    }
}

async fn run_ephemeral_agent_call(
    state: &AppState,
    space_id: Uuid,
    request: AgentCallRequest,
    cancellation: CancellationToken,
) -> Result<AgentCallResult, String> {
    let session = state
        .storage
        .create_session(NewSession {
            space_id,
            agent_id: request.agent.clone(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .map_err(|error| error.to_string())?;
    if let Some(model) = request.model.as_deref() {
        state
            .storage
            .set_session_selected_model(session.id, Some(model.to_string()))
            .await
            .map_err(|error| error.to_string())?;
    }
    let thread = ThreadStore::create(
        state.storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: request.agent,
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let (events, _events_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let processing = crate::turn::process_turn_with_existing_turn(
        state,
        session.id,
        Some(thread.id),
        request.prompt,
        events,
        Arc::new(Mutex::new(SubscriptionState::new(false))),
        request.schema,
        Some(request.max_attempts.saturating_sub(1)),
        None,
        crate::turn::TurnAttendance::Unattended,
    );
    tokio::pin!(processing);
    tokio::select! {
        result = &mut processing => result.map_err(|error| error.to_string())?,
        () = cancellation.cancelled() => {
            crate::turn::cancel_turn(state, session.id).await.map_err(|error| error.to_string())?;
            return Err("WorkflowCancelledError".into());
        }
    }
    let turn = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .max_by_key(|turn| turn.created_at)
        .ok_or_else(|| "agent call produced no turn".to_string())?;
    let output = turn
        .structured_output
        .unwrap_or_else(|| Value::String(turn.assistant_response.unwrap_or_default()));
    Ok(AgentCallResult {
        output,
        input_tokens: turn.input_tokens.unwrap_or(0).max(0) as u64,
        output_tokens: turn.output_tokens.unwrap_or(0).max(0) as u64,
        session_id: Some(session.id),
        thread_id: Some(thread.id),
        turn_id: Some(turn.id),
    })
}

async fn deliver_webhook(
    grant: Option<&Value>,
    payload: EffectPayload,
    cancellation: CancellationToken,
    effect_seq: u64,
    key: Uuid,
) -> Result<EffectReceipt, String> {
    let targets = grant
        .and_then(Value::as_array)
        .ok_or_else(|| "DeliveryNotGrantedError".to_string())?;
    let target = targets
        .iter()
        .find(|target| {
            target.get("kind").and_then(Value::as_str) == Some("webhook")
                && payload
                    .target
                    .as_deref()
                    .is_none_or(|name| target.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| "DeliveryNotGrantedError".to_string())?;
    let url = target
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "webhook grant is missing url".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let request = client.post(url).header("Idempotency-Key", key.to_string()).json(&serde_json::json!({ "effect_id": key, "content": payload.content, "metadata": payload.metadata }));
    let response = tokio::select! { response = request.send() => response.map_err(|error| error.to_string())?, () = cancellation.cancelled() => return Err("WorkflowCancelledError".into()) };
    if !response.status().is_success() {
        return Err(format!("webhook returned {}", response.status()));
    }
    Ok(EffectReceipt {
        delivered: true,
        target: "webhook".into(),
        effect_seq,
    })
}

async fn run_host_commands(
    state: AppState,
    run_id: Uuid,
    space_id: Uuid,
    delivery_grant: Option<Value>,
    budget: WorkflowBudget,
    mut commands: mpsc::UnboundedReceiver<HostCommand>,
) {
    let effect_seq = Arc::new(AtomicU64::new(0));
    let seq = Arc::new(AtomicU64::new(0));
    let agent_calls = Arc::new(AtomicU32::new(0));
    let spent_tokens = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(
        budget.max_concurrent_agent_calls.unwrap_or(4).max(1) as usize,
    ));
    let replay = Arc::new(
        state
            .storage
            .list_workflow_journal(run_id)
            .await
            .unwrap_or_default(),
    );
    // Only agent calls can be outstanding together. Map their persisted completion
    // order to a dense rank so replay releases parallel promises in exactly the
    // interleaving observed by the original run, irrespective of unrelated log rows.
    let mut replay_agent_orders = replay
        .iter()
        .filter(|entry| entry.kind == "agent_call" && entry.status == "completed")
        .filter_map(|entry| entry.completed_order.map(|order| (entry.seq, order)))
        .collect::<Vec<_>>();
    replay_agent_orders.sort_by_key(|(_, order)| *order);
    let replay_agent_ranks = Arc::new(
        replay_agent_orders
            .into_iter()
            .enumerate()
            .map(|(rank, (sequence, _))| (sequence, rank as u64))
            .collect::<std::collections::HashMap<_, _>>(),
    );
    let replay_release_rank = Arc::new(AtomicU64::new(0));
    let replay_release_notify = Arc::new(Notify::new());
    while let Some(command) = commands.recv().await {
        match command {
            HostCommand::Record { kind, message } => {
                let request = serde_json::json!({ "message": message });
                let current_seq = seq.fetch_add(1, Ordering::SeqCst);
                if let Ok(intent) = state
                    .storage
                    .create_workflow_journal_intent(NewWorkflowJournalIntent {
                        run_id,
                        seq: current_seq,
                        kind,
                        args_hash: JournalReplay::args_hash(&request),
                        request,
                        effect_kind: None,
                        idempotency_key: None,
                        target_kind: None,
                    })
                    .await
                {
                    let _ = state
                        .storage
                        .complete_workflow_journal_entry(intent.id, Value::Null)
                        .await;
                }
            }
            HostCommand::Agent {
                request,
                cancellation,
                reply,
            } => {
                if budget
                    .max_agent_calls
                    .is_some_and(|limit| agent_calls.fetch_add(1, Ordering::SeqCst) >= limit)
                {
                    let _ = reply.send(Err("BudgetExceededError: agent call limit reached".into()));
                    continue;
                }
                if budget
                    .max_total_tokens
                    .is_some_and(|limit| spent_tokens.load(Ordering::SeqCst) >= limit)
                {
                    let _ = reply.send(Err("BudgetExceededError: token limit reached".into()));
                    continue;
                }
                let state = state.clone();
                let replay = Arc::clone(&replay);
                let spent = Arc::clone(&spent_tokens);
                let semaphore = Arc::clone(&semaphore);
                let replay_agent_ranks = Arc::clone(&replay_agent_ranks);
                let replay_release_rank = Arc::clone(&replay_release_rank);
                let replay_release_notify = Arc::clone(&replay_release_notify);
                // The channel is the deterministic boundary. Assigning sequence
                // numbers inside spawned tasks would make scheduler timing part of
                // the journal, which is an especially unhelpful kind of experiment.
                let current_seq = seq.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let _permit = match semaphore.acquire().await {
                        Ok(permit) => permit,
                        Err(_) => {
                            let _ = reply.send(Err("workflow concurrency gate closed".into()));
                            return;
                        }
                    };
                    let request_json =
                        serde_json::to_value(&request).expect("agent request serialises");
                    let hash = JournalReplay::args_hash(&request_json);
                    let existing = replay.get(current_seq as usize);
                    let result = if let Some(entry) = existing {
                        if entry.args_hash != hash {
                            Err("NondeterministicReplayError".into())
                        } else if entry.status == "completed" {
                            let result = entry
                                .result
                                .clone()
                                .ok_or_else(|| "completed agent entry has no result".to_string())
                                .and_then(|value| {
                                    serde_json::from_value(value).map_err(|error| error.to_string())
                                });
                            if let Some(rank) = replay_agent_ranks.get(&current_seq).copied() {
                                await_replay_release(
                                    rank,
                                    &replay_release_rank,
                                    &replay_release_notify,
                                )
                                .await;
                            }
                            result
                        } else {
                            complete_agent_entry(&state, space_id, entry.id, request, cancellation)
                                .await
                        }
                    } else {
                        match state
                            .storage
                            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                                run_id,
                                seq: current_seq,
                                kind: "agent_call".into(),
                                args_hash: hash,
                                request: request_json,
                                effect_kind: None,
                                idempotency_key: None,
                                target_kind: None,
                            })
                            .await
                        {
                            Ok(intent) => {
                                complete_agent_entry(
                                    &state,
                                    space_id,
                                    intent.id,
                                    request,
                                    cancellation,
                                )
                                .await
                            }
                            Err(error) => Err(error.to_string()),
                        }
                    };
                    if let Ok(result) = &result {
                        spent.fetch_add(
                            result.input_tokens + result.output_tokens,
                            Ordering::SeqCst,
                        );
                    }
                    let _ = reply.send(result);
                });
            }
            HostCommand::Deliver {
                payload,
                cancellation,
                reply,
            } => {
                let request_json =
                    serde_json::to_value(&payload).expect("effect payload serialises");
                let hash = JournalReplay::args_hash(&request_json);
                let current_seq = seq.fetch_add(1, Ordering::SeqCst);
                let current_effect = effect_seq.load(Ordering::SeqCst);
                let existing = replay.get(current_seq as usize);
                let result = if let Some(entry) = existing {
                    if entry.args_hash != hash {
                        Err("NondeterministicReplayError".into())
                    } else if entry.status == "completed" {
                        entry
                            .result
                            .clone()
                            .ok_or_else(|| "completed effect entry has no result".to_string())
                            .and_then(|value| {
                                serde_json::from_value(value).map_err(|error| error.to_string())
                            })
                    } else if let Some(key) = entry.idempotency_key {
                        complete_effect_entry(
                            &state,
                            delivery_grant.as_ref(),
                            entry.id,
                            payload,
                            cancellation,
                            current_effect,
                            key,
                        )
                        .await
                    } else {
                        Err("effect intent has no idempotency key".into())
                    }
                } else {
                    let key = Uuid::new_v4();
                    match state
                        .storage
                        .create_workflow_journal_intent(NewWorkflowJournalIntent {
                            run_id,
                            seq: current_seq,
                            kind: "effect".into(),
                            args_hash: hash,
                            request: request_json,
                            effect_kind: Some("idempotency_key".into()),
                            idempotency_key: Some(key),
                            target_kind: Some("webhook".into()),
                        })
                        .await
                    {
                        Ok(intent) => {
                            complete_effect_entry(
                                &state,
                                delivery_grant.as_ref(),
                                intent.id,
                                payload,
                                cancellation,
                                current_effect,
                                key,
                            )
                            .await
                        }
                        Err(error) => Err(error.to_string()),
                    }
                };
                if result.is_ok() {
                    effect_seq.fetch_add(1, Ordering::SeqCst);
                }
                let _ = reply.send(result);
            }
        }
    }
}

async fn await_replay_release(rank: u64, next: &AtomicU64, notify: &Notify) {
    loop {
        if next
            .compare_exchange(rank, rank + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            notify.notify_waiters();
            return;
        }
        notify.notified().await;
    }
}

async fn complete_agent_entry(
    state: &AppState,
    space_id: Uuid,
    entry_id: Uuid,
    request: AgentCallRequest,
    cancellation: CancellationToken,
) -> Result<AgentCallResult, String> {
    let result = run_ephemeral_agent_call(state, space_id, request, cancellation).await?;
    state
        .storage
        .complete_workflow_journal_entry(
            entry_id,
            serde_json::to_value(&result).expect("agent result serialises"),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(result)
}

async fn complete_effect_entry(
    state: &AppState,
    grant: Option<&Value>,
    entry_id: Uuid,
    payload: EffectPayload,
    cancellation: CancellationToken,
    effect_seq: u64,
    key: Uuid,
) -> Result<EffectReceipt, String> {
    let receipt = deliver_webhook(grant, payload, cancellation, effect_seq, key).await?;
    state
        .storage
        .complete_workflow_journal_entry(
            entry_id,
            serde_json::to_value(&receipt).expect("receipt serialises"),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(receipt)
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::internal(error.to_string())
}

fn script_wire(record: WorkflowScriptRecord, head_version: u32) -> WorkflowScript {
    WorkflowScript {
        id: record.id,
        space_id: record.space_id,
        name: record.name,
        description: record.description,
        head_version,
        has_delivery_grant: record.delivery_grant.is_some(),
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

fn run_state(state: &str) -> Result<WorkflowRunState, ApiError> {
    match state {
        "running" => Ok(WorkflowRunState::Running),
        "completed" => Ok(WorkflowRunState::Completed),
        "failed" => Ok(WorkflowRunState::Failed),
        "cancelled" => Ok(WorkflowRunState::Cancelled),
        "interrupted" => Ok(WorkflowRunState::Interrupted),
        other => Err(internal(format!("unknown workflow run state: {other}"))),
    }
}

fn budget_wire(value: &Value) -> WorkflowBudget {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

fn run_wire(record: WorkflowRunRecord) -> Result<WorkflowRun, ApiError> {
    Ok(WorkflowRun {
        id: record.id,
        space_id: record.space_id,
        script_id: None,
        script_name: None,
        script_version: None,
        session_id: record.session_id,
        state: run_state(&record.state)?,
        args: record.args,
        budget: budget_wire(&record.budget),
        final_output: record.final_output,
        error_message: record.error_message,
        created_at: record.created_at,
        completed_at: record.completed_at,
    })
}

pub async fn create_script(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
    Json(request): Json<CreateWorkflowScriptRequest>,
) -> Result<Json<WorkflowScript>, ApiError> {
    state
        .space_store
        .get(space_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("space", space_id))?;
    let (record, version) = state
        .storage
        .create_workflow_script(NewWorkflowScript {
            space_id,
            name: request.name,
            description: request.description,
            source: request.source,
            delivery_grant: request.delivery_grant,
        })
        .await
        .map_err(internal)?;
    Ok(Json(script_wire(record, version.version)))
}

pub async fn list_scripts(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
) -> Result<Json<WorkflowScriptListResponse>, ApiError> {
    let records = state
        .storage
        .list_workflow_scripts(space_id)
        .await
        .map_err(internal)?;
    let mut scripts = Vec::with_capacity(records.len());
    for record in records {
        let head = state
            .storage
            .get_workflow_script_head(record.id)
            .await
            .map_err(internal)?
            .ok_or_else(|| internal("workflow script has no version"))?;
        scripts.push(script_wire(record, head.version));
    }
    Ok(Json(WorkflowScriptListResponse { scripts }))
}

pub async fn get_script(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowScript>, ApiError> {
    let record = state
        .storage
        .get_workflow_script(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("workflow script", id))?;
    let head = state
        .storage
        .get_workflow_script_head(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("workflow script has no version"))?;
    Ok(Json(script_wire(record, head.version)))
}

pub async fn update_script(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateWorkflowScriptRequest>,
) -> Result<Json<WorkflowScript>, ApiError> {
    let _record = state
        .storage
        .get_workflow_script(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("workflow script", id))?;
    if request.description.is_some() || request.delivery_grant.is_some() {
        state
            .storage
            .update_workflow_script(id, request.description.as_deref(), request.delivery_grant)
            .await
            .map_err(internal)?;
    }
    if let Some(source) = request.source {
        state
            .storage
            .append_workflow_script_version(id, &source)
            .await
            .map_err(internal)?;
    }
    get_script(State(state), Path(id)).await
}

fn source_hash(source: &str) -> String {
    Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn start_run(
    state: AppState,
    space_id: Uuid,
    script_version_id: Option<Uuid>,
    source: String,
    args: Value,
    budget: WorkflowBudget,
    delivery_grant: Option<Value>,
) -> Result<WorkflowRun, ApiError> {
    // Each run owns a QuickJS runtime and an OS thread. Keep the default bound
    // explicit until workflow settings join the resolved configuration model.
    if state.active_workflow_runs.lock().await.len() >= 4 {
        return Err(ApiError::conflict(
            "workflow run limit reached (maximum 4 active runs)".into(),
        ));
    }
    let record = state
        .storage
        .create_workflow_run(NewWorkflowRun {
            space_id,
            script_version_id,
            source: script_version_id.is_none().then(|| source.clone()),
            source_hash: source_hash(&source),
            args,
            budget: serde_json::to_value(budget).map_err(internal)?,
            engine_version: "rquickjs-0.12.0".into(),
        })
        .await
        .map_err(internal)?;
    launch_existing_run(state, record, source, delivery_grant).await
}

async fn launch_existing_run(
    state: AppState,
    record: WorkflowRunRecord,
    source: String,
    delivery_grant: Option<Value>,
) -> Result<WorkflowRun, ApiError> {
    let space_id = record.space_id;
    let started = run_wire(record.clone())?;
    let _ = state.broadcast.send(ServerEvent::WorkflowRunStarted {
        event: swage_protocol::client::workflow::WorkflowRunStartedEvent {
            run: started.clone(),
        },
    });
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: record.session_id,
            entity_kind: "workflow_run".into(),
            entity_id: record.id.to_string(),
            event_kind: "started".into(),
            summary: "Workflow run started".into(),
            metadata: serde_json::json!({"space_id":record.space_id}),
        })
        .await
        .map_err(internal)?;
    let cancellation = CancellationToken::new();
    state
        .active_workflow_runs
        .lock()
        .await
        .insert(record.id, cancellation.clone());
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let (host_tx, host_rx) = mpsc::unbounded_channel();
    let run_budget = budget_wire(&record.budget);
    tokio::spawn(run_host_commands(
        state.clone(),
        record.id,
        space_id,
        delivery_grant.clone(),
        run_budget,
        host_rx,
    ));
    let host: Arc<dyn WorkflowHost> = Arc::new(ChannelWorkflowHost { tx: host_tx });
    let args_json = serde_json::to_string(&record.args).map_err(internal)?;
    let delivery_granted = delivery_grant.is_some();
    std::thread::Builder::new()
        .name(format!("workflow-{}", record.id))
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("workflow runtime");
            let result =
                runtime.block_on(WorkflowEngine::new(EngineLimits::default()).execute_hosted(
                    &source,
                    &args_json,
                    cancellation,
                    Some(host),
                    delivery_granted,
                ));
            let _ = result_tx.send(result);
        })
        .map_err(internal)?;
    let storage = Arc::clone(&state.storage);
    let active = Arc::clone(&state.active_workflow_runs);
    let run_id = record.id;
    let broadcast = state.broadcast.clone();
    tokio::spawn(async move {
        let (run_state, output, error) = match result_rx.await {
            Ok(Ok(json)) => match serde_json::from_str(&json) {
                Ok(value) => ("completed", Some(value), None),
                Err(problem) => ("failed", None, Some(problem.to_string())),
            },
            Ok(Err(swage_workflow::WorkflowEngineError::Cancelled)) => ("cancelled", None, None),
            Ok(Err(problem)) => ("failed", None, Some(problem.to_string())),
            Err(problem) => ("failed", None, Some(problem.to_string())),
        };
        let _ = storage
            .update_workflow_run_state(run_id, run_state, output, error.as_deref())
            .await;
        let _ = storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id: None,
                entity_kind: "workflow_run".into(),
                entity_id: run_id.to_string(),
                event_kind: run_state.into(),
                summary: format!("Workflow run {run_state}"),
                metadata: serde_json::json!({}),
            })
            .await;
        if let Ok(Some(record)) = storage.get_workflow_run(run_id).await
            && let Ok(run) = run_wire(record)
        {
            let _ = broadcast.send(ServerEvent::WorkflowRunCompleted {
                event: swage_protocol::client::workflow::WorkflowRunCompletedEvent { run },
            });
        }
        active.lock().await.remove(&run_id);
    });
    Ok(started)
}

pub async fn start_inline_run(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
    Json(request): Json<StartInlineWorkflowRunRequest>,
) -> Result<Json<WorkflowRun>, ApiError> {
    state
        .space_store
        .get(space_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("space", space_id))?;
    Ok(Json(
        start_run(
            state,
            space_id,
            None,
            request.source,
            request.args,
            request.budget.unwrap_or_default(),
            None,
        )
        .await?,
    ))
}

pub async fn start_saved_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<StartWorkflowRunRequest>,
) -> Result<Json<WorkflowRun>, ApiError> {
    let script = state
        .storage
        .get_workflow_script(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("workflow script", id))?;
    let version = state
        .storage
        .get_workflow_script_head(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("workflow script has no version"))?;
    Ok(Json(
        start_run(
            state,
            script.space_id,
            Some(version.id),
            version.source,
            request.args,
            request.budget.unwrap_or_default(),
            script.delivery_grant,
        )
        .await?,
    ))
}

pub async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowRunDetailResponse>, ApiError> {
    let run = state
        .storage
        .get_workflow_run(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("workflow run", id))?;
    let journal = state
        .storage
        .list_workflow_journal(id)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|entry| {
            use swage_protocol::client::workflow::{
                WorkflowEffectDetail, WorkflowJournalEntry, WorkflowJournalEntryKind,
                WorkflowJournalEntryStatus,
            };
            let kind = match entry.kind.as_str() {
                "agent_call" => WorkflowJournalEntryKind::AgentCall,
                "effect" => WorkflowJournalEntryKind::Effect,
                "log" => WorkflowJournalEntryKind::Log,
                "phase" => WorkflowJournalEntryKind::Phase,
                other => return Err(internal(format!("unknown workflow entry kind: {other}"))),
            };
            let status = match entry.status.as_str() {
                "started" => WorkflowJournalEntryStatus::Started,
                "completed" => WorkflowJournalEntryStatus::Completed,
                "failed" => WorkflowJournalEntryStatus::Failed,
                other => return Err(internal(format!("unknown workflow entry status: {other}"))),
            };
            let effect = entry
                .idempotency_key
                .map(|idempotency_key| WorkflowEffectDetail {
                    effect_kind: entry.effect_kind.unwrap_or_default(),
                    target_kind: entry.target_kind.unwrap_or_default(),
                    idempotency_key,
                });
            let agent_call = (entry.kind == "agent_call").then(|| {
                use swage_protocol::client::workflow::WorkflowAgentCallDetail;
                let result = entry.result.as_ref();
                WorkflowAgentCallDetail {
                    agent_id: entry
                        .request
                        .get("agent")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    session_id: result
                        .and_then(|value| value.get("session_id"))
                        .and_then(|value| serde_json::from_value(value.clone()).ok()),
                    thread_id: result
                        .and_then(|value| value.get("thread_id"))
                        .and_then(|value| serde_json::from_value(value.clone()).ok()),
                    turn_id: result
                        .and_then(|value| value.get("turn_id"))
                        .and_then(|value| serde_json::from_value(value.clone()).ok()),
                    input_tokens: result
                        .and_then(|value| value.get("input_tokens"))
                        .and_then(Value::as_u64),
                    output_tokens: result
                        .and_then(|value| value.get("output_tokens"))
                        .and_then(Value::as_u64),
                }
            });
            let label = entry
                .request
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok(WorkflowJournalEntry {
                id: entry.id,
                seq: entry.seq,
                completed_order: entry.completed_order,
                kind,
                status,
                label,
                request: entry.request,
                result: entry.result,
                error_message: entry.error_message,
                agent_call,
                effect,
                started_at: entry.started_at,
                completed_at: entry.completed_at,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(WorkflowRunDetailResponse {
        run: run_wire(run)?,
        journal,
    }))
}

pub async fn list_runs(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
) -> Result<Json<WorkflowRunListResponse>, ApiError> {
    let runs = state
        .storage
        .list_workflow_runs(space_id)
        .await
        .map_err(internal)?
        .into_iter()
        .map(run_wire)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(WorkflowRunListResponse { runs }))
}

pub async fn cancel_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowRun>, ApiError> {
    if let Some(token) = state.active_workflow_runs.lock().await.get(&id).cloned() {
        token.cancel();
    }
    let run = state
        .storage
        .get_workflow_run(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("workflow run", id))?;
    Ok(Json(run_wire(run)?))
}

pub async fn resume_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowRun>, ApiError> {
    let run = state
        .storage
        .get_workflow_run(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("workflow run", id))?;
    if run.state != "interrupted" {
        return Err(ApiError::conflict(
            "only interrupted workflow runs can resume".into(),
        ));
    }
    let source = match (&run.source, run.script_version_id) {
        (Some(source), _) => source.clone(),
        (None, Some(version_id)) => {
            state
                .storage
                .get_workflow_script_version(version_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| internal("workflow script version is missing"))?
                .source
        }
        (None, None) => return Err(internal("workflow run has no source")),
    };
    let grant = if let Some(version_id) = run.script_version_id {
        let version = state
            .storage
            .get_workflow_script_version(version_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| internal("workflow script version is missing"))?;
        state
            .storage
            .get_workflow_script(version.script_id)
            .await
            .map_err(internal)?
            .and_then(|script| script.delivery_grant)
    } else {
        None
    };
    state
        .storage
        .update_workflow_run_state(id, "running", None, None)
        .await
        .map_err(internal)?;
    let running = state
        .storage
        .get_workflow_run(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("workflow run", id))?;
    Ok(Json(
        launch_existing_run(state, running, source, grant).await?,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use swage_storage::{SpaceStore, SqliteStorage, Storage, WorkflowStore};

    #[tokio::test(flavor = "multi_thread")]
    async fn inline_run_returns_detached_and_completes_from_recorded_args() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );

        let Json(started) = start_inline_run(
            State(state.clone()),
            Path(space.id),
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
        let space = storage.get_or_create_default().await.expect("space");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let (script, _) = storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "deliver-once".into(),
                description: String::new(),
                source: "return await deliver({ content: args.content })".into(),
                delivery_grant: Some(
                    serde_json::json!([{"kind":"webhook","url":format!("http://{address}/post")}]),
                ),
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
            .update_workflow_run_state(started.id, "interrupted", None, None)
            .await
            .expect("simulate restart");
        let _resumed = resume_run(State(state.clone()), Path(started.id))
            .await
            .expect("resume");
        wait_for_terminal(&state, started.id).await;
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    }

    async fn wait_for_terminal(state: &AppState, id: Uuid) {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
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
}
