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
use swage_storage::WorkflowJournalRecord;
use swage_storage::{
    NewAuditEvent, NewSession, NewThread, NewWorkflowJournalIntent, NewWorkflowRun,
    NewWorkflowScript, ThreadStore, WorkflowRunRecord, WorkflowScriptRecord,
};
use swage_workflow::{
    AgentCallRequest, AgentCallResult, EffectPayload, EffectReceipt, EffectRecoveryHost,
    EngineLimits, ScriptCapabilities, WorkflowCallRequest, WorkflowEngine, WorkflowHost,
    journal_args_hash, recover_started_effect,
};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    error::ApiError,
    state::AppState,
    subscription::SubscriptionState,
    websocket::ServerEvent,
    workflow_delivery::{DeliveryCapability, capability_from_grant},
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
    Workflow {
        request: WorkflowCallRequest,
        cancellation: CancellationToken,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Timer {
        ms: u64,
        timer_id: u64,
        cancellation: CancellationToken,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    AbandonTimer {
        timer_id: u64,
    },
}

struct ChannelWorkflowHost {
    tx: mpsc::UnboundedSender<HostCommand>,
    spent_tokens: Arc<AtomicU64>,
    max_total_tokens: Option<u64>,
}

#[async_trait::async_trait]
impl WorkflowHost for ChannelWorkflowHost {
    fn record(&self, kind: &str, message: String) {
        let _ = self.tx.send(HostCommand::Record {
            kind: kind.into(),
            message,
        });
    }

    fn budget_spent(&self) -> u64 {
        self.spent_tokens.load(Ordering::SeqCst)
    }

    fn budget_remaining(&self) -> Option<u64> {
        self.max_total_tokens
            .map(|limit| limit.saturating_sub(self.budget_spent()))
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

    async fn run_workflow_call(
        &self,
        request: WorkflowCallRequest,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(HostCommand::Workflow {
                request,
                cancellation,
                reply,
            })
            .map_err(|_| "workflow host stopped".to_string())?;
        receive
            .await
            .map_err(|_| "workflow host dropped workflow reply".to_string())?
    }

    async fn timer(
        &self,
        ms: u64,
        timer_id: u64,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(HostCommand::Timer {
                ms,
                timer_id,
                cancellation,
                reply,
            })
            .map_err(|_| "workflow host stopped".to_string())?;
        receive
            .await
            .map_err(|_| "workflow host dropped timer reply".to_string())?
    }

    fn abandon_timer(&self, timer_id: u64) {
        let _ = self.tx.send(HostCommand::AbandonTimer { timer_id });
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

// Child workflows construct another host-command loop. Erase the loop future at
// that recursive boundary so rustc can use the explicit `Send` contract instead
// of recursively inspecting the async function's opaque type.
fn run_host_commands(
    state: AppState,
    run_id: Uuid,
    space_id: Uuid,
    delivery_grant: Option<Value>,
    budget: WorkflowBudget,
    replay: Vec<WorkflowJournalRecord>,
    agent_calls: Arc<AtomicU32>,
    spent_tokens: Arc<AtomicU64>,
    commands: mpsc::UnboundedReceiver<HostCommand>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(run_host_commands_inner(
        state,
        run_id,
        space_id,
        delivery_grant,
        budget,
        replay,
        agent_calls,
        spent_tokens,
        commands,
    ))
}

async fn run_host_commands_inner(
    state: AppState,
    run_id: Uuid,
    space_id: Uuid,
    delivery_grant: Option<Value>,
    budget: WorkflowBudget,
    replay: Vec<WorkflowJournalRecord>,
    agent_calls: Arc<AtomicU32>,
    spent_tokens: Arc<AtomicU64>,
    mut commands: mpsc::UnboundedReceiver<HostCommand>,
) {
    let effect_seq = Arc::new(AtomicU64::new(
        replay
            .iter()
            .filter(|entry| entry.kind == "effect" && entry.status == "completed")
            .count() as u64,
    ));
    let seq = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(
        budget.max_concurrent_agent_calls.unwrap_or(4).max(1) as usize,
    ));
    let replay = Arc::new(replay);
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
    // Pending wait() timers by script-assigned timer id. Abandonment cancels
    // the armed sleep so a beaten timer can never fire later and complete its
    // journal entry — a completed timer entry must always mean the timer won.
    let mut pending_timers: std::collections::HashMap<u64, CancellationToken> =
        std::collections::HashMap::new();
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
                        args_hash: journal_args_hash(&request),
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
                let current_seq = seq.load(Ordering::SeqCst);
                let existing_call = replay.get(current_seq as usize);
                if existing_call.is_none() {
                    if budget
                        .max_agent_calls
                        .is_some_and(|limit| agent_calls.load(Ordering::SeqCst) >= limit)
                    {
                        let _ =
                            reply.send(Err("BudgetExceededError: agent call limit reached".into()));
                        continue;
                    }
                    if budget
                        .max_total_tokens
                        .is_some_and(|limit| spent_tokens.load(Ordering::SeqCst) >= limit)
                    {
                        let _ = reply.send(Err("BudgetExceededError: token limit reached".into()));
                        continue;
                    }
                    agent_calls.fetch_add(1, Ordering::SeqCst);
                }
                seq.fetch_add(1, Ordering::SeqCst);
                let completed_replay =
                    existing_call.is_some_and(|entry| entry.status == "completed");
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
                    let hash = journal_args_hash(&request_json);
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
                    if !completed_replay && let Ok(result) = &result {
                        spent.fetch_add(
                            result.input_tokens + result.output_tokens,
                            Ordering::SeqCst,
                        );
                    }
                    let _ = reply.send(result);
                });
            }
            HostCommand::Workflow {
                request,
                cancellation,
                reply,
            } => {
                let current_seq = seq.fetch_add(1, Ordering::SeqCst);
                let request_json =
                    serde_json::to_value(&request).expect("workflow request serialises");
                let hash = journal_args_hash(&request_json);
                let existing = replay.get(current_seq as usize).cloned();
                let state = state.clone();
                // Spawned so a slow child run does not block the host loop —
                // log records and timer arms must keep flowing while a child
                // works. Seq was assigned at the channel boundary above.
                tokio::spawn(async move {
                    let result = match existing {
                        Some(entry) if entry.args_hash != hash => {
                            Err("NondeterministicReplayError".into())
                        }
                        Some(entry) if entry.status == "completed" => entry
                            .result
                            .clone()
                            .ok_or_else(|| "completed workflow entry has no result".to_string()),
                        Some(entry) => {
                            complete_workflow_call_entry(
                                &state,
                                run_id,
                                space_id,
                                entry.id,
                                current_seq,
                                request,
                                cancellation,
                            )
                            .await
                        }
                        None => {
                            match state
                                .storage
                                .create_workflow_journal_intent(NewWorkflowJournalIntent {
                                    run_id,
                                    seq: current_seq,
                                    kind: "workflow_call".into(),
                                    args_hash: hash,
                                    request: request_json,
                                    effect_kind: None,
                                    idempotency_key: None,
                                    target_kind: None,
                                })
                                .await
                            {
                                Ok(intent) => {
                                    complete_workflow_call_entry(
                                        &state,
                                        run_id,
                                        space_id,
                                        intent.id,
                                        current_seq,
                                        request,
                                        cancellation,
                                    )
                                    .await
                                }
                                Err(error) => Err(error.to_string()),
                            }
                        }
                    };
                    let _ = reply.send(result);
                });
            }
            HostCommand::AbandonTimer { timer_id } => {
                if let Some(token) = pending_timers.remove(&timer_id) {
                    token.cancel();
                }
            }
            HostCommand::Timer {
                ms,
                timer_id,
                cancellation,
                reply,
            } => {
                let abandon = CancellationToken::new();
                pending_timers.insert(timer_id, abandon.clone());
                let current_seq = seq.fetch_add(1, Ordering::SeqCst);
                let request_json = serde_json::json!({ "ms": ms });
                let hash = journal_args_hash(&request_json);
                let existing = replay.get(current_seq as usize).cloned();
                let state = state.clone();
                // A completed timer entry replays instantly, so a wait the
                // timer won wakes the same way on resume. A started entry is
                // re-armed whole: the original firing was never recorded, so
                // honesty is a fresh full wait, not a guessed remainder.
                tokio::spawn(async move {
                    let entry_id = match existing {
                        Some(entry) if entry.args_hash != hash => {
                            let _ = reply.send(Err("NondeterministicReplayError".into()));
                            return;
                        }
                        Some(entry) if entry.status == "completed" => {
                            let _ = reply.send(
                                entry
                                    .result
                                    .clone()
                                    .ok_or_else(|| "completed timer entry has no result".to_string()),
                            );
                            return;
                        }
                        Some(entry) => entry.id,
                        None => {
                            match state
                                .storage
                                .create_workflow_journal_intent(NewWorkflowJournalIntent {
                                    run_id,
                                    seq: current_seq,
                                    kind: "timer".into(),
                                    args_hash: hash,
                                    request: request_json,
                                    effect_kind: None,
                                    idempotency_key: None,
                                    target_kind: None,
                                })
                                .await
                            {
                                Ok(intent) => intent.id,
                                Err(error) => {
                                    let _ = reply.send(Err(error.to_string()));
                                    return;
                                }
                            }
                        }
                    };
                    tokio::select! {
                        () = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {}
                        () = abandon.cancelled() => {
                            // Beaten by a completion: the entry stays
                            // `started` and the reply is never consumed —
                            // wait() already resolved on the winner.
                            return;
                        }
                        () = cancellation.cancelled() => {
                            let _ = reply.send(Err("WorkflowCancelledError: timer cancelled".into()));
                            return;
                        }
                    }
                    let fired = serde_json::json!({ "fired": true });
                    let result = state
                        .storage
                        .complete_workflow_journal_entry(entry_id, fired.clone())
                        .await
                        .map(|_| fired)
                        .map_err(|error| error.to_string());
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
                let hash = journal_args_hash(&request_json);
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
                        let capability = match capability_from_grant(
                            Arc::clone(&state.storage),
                            delivery_grant.as_ref(),
                            payload.target.as_deref(),
                        )
                        .await
                        {
                            Ok(capability) => capability,
                            Err(error) => {
                                let _ = reply.send(Err(error));
                                continue;
                            }
                        };
                        complete_effect_entry(
                            &state,
                            capability,
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
                    let capability = match capability_from_grant(
                        Arc::clone(&state.storage),
                        delivery_grant.as_ref(),
                        payload.target.as_deref(),
                    )
                    .await
                    {
                        Ok(capability) => capability,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                    let effect_kind = match capability.effect_kind() {
                        swage_workflow::EffectKind::IdempotencyKey => "idempotency_key",
                        swage_workflow::EffectKind::CheckBeforeRedo => "check_before_redo",
                    };
                    match state
                        .storage
                        .create_workflow_journal_intent(NewWorkflowJournalIntent {
                            run_id,
                            seq: current_seq,
                            kind: "effect".into(),
                            args_hash: hash,
                            request: request_json,
                            effect_kind: Some(effect_kind.into()),
                            idempotency_key: Some(key),
                            target_kind: Some(capability.kind().into()),
                        })
                        .await
                    {
                        Ok(intent) => {
                            complete_effect_entry(
                                &state,
                                capability,
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

/// Runs a parent's `workflow()` call as a real child run and journals its
/// final output. Recovery is by parentage, not re-execution: a child the
/// parent already started is found via (parent_run_id, parent_seq) and its
/// recorded outcome is used — resumed if interrupted, never duplicated.
async fn complete_workflow_call_entry(
    state: &AppState,
    parent_run_id: Uuid,
    space_id: Uuid,
    entry_id: Uuid,
    parent_seq: u64,
    request: WorkflowCallRequest,
    cancellation: CancellationToken,
) -> Result<Value, String> {
    let script = state
        .storage
        .list_workflow_scripts(space_id)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|script| script.name == request.name)
        .ok_or_else(|| {
            format!(
                "WorkflowCompositionError: workflow script {:?} was not found",
                request.name
            )
        })?;
    let version = state
        .storage
        .get_workflow_script_head(script.id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "WorkflowCompositionError: workflow script has no version".to_string())?;
    let existing_child = state
        .storage
        .find_child_workflow_run(parent_run_id, parent_seq)
        .await
        .map_err(|error| error.to_string())?;
    let child = match existing_child {
        Some(child) => match child.state.as_str() {
            "completed" => child,
            "interrupted" => {
                let source = version.source.clone();
                let grant = script.delivery_grant.clone();
                state
                    .storage
                    .update_workflow_run_state(child.id, "running", None, None)
                    .await
                    .map_err(|error| error.to_string())?;
                let running = state
                    .storage
                    .get_workflow_run(child.id)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "child workflow run disappeared".to_string())?;
                launch_existing_run(state.clone(), running, source, grant)
                    .await
                    .map_err(|error| error.0.to_string())?;
                child
            }
            _ => child,
        },
        None => {
            let record = state
                .storage
                .create_workflow_run(NewWorkflowRun {
                    space_id,
                    parent_run_id: Some(parent_run_id),
                    parent_seq: Some(parent_seq),
                    script_version_id: Some(version.id),
                    source: None,
                    source_hash: source_hash(&version.source),
                    args: request.args.clone(),
                    budget: serde_json::to_value(WorkflowBudget::default())
                        .map_err(|error| error.to_string())?,
                    engine_version: "rquickjs-0.12.0".into(),
                })
                .await
                .map_err(|error| error.to_string())?;
            launch_existing_run(
                state.clone(),
                record.clone(),
                version.source.clone(),
                script.delivery_grant.clone(),
            )
            .await
            .map_err(|error| error.0.to_string())?;
            record
        }
    };
    let record = loop {
        let record = state
            .storage
            .get_workflow_run(child.id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "child workflow run disappeared".to_string())?;
        if record.state != "running" {
            break record;
        }
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            () = cancellation.cancelled() => {
                if let Some(token) = state.active_workflow_runs.lock().await.get(&child.id) {
                    token.cancel();
                }
                return Err("WorkflowCancelledError: parent run cancelled".into());
            }
        }
    };
    match record.state.as_str() {
        "completed" => {
            let output = record.final_output.unwrap_or(Value::Null);
            state
                .storage
                .complete_workflow_journal_entry(entry_id, output.clone())
                .await
                .map_err(|error| error.to_string())?;
            Ok(output)
        }
        other => Err(format!(
            "WorkflowCompositionError: child workflow run {} {}: {}",
            record.id,
            other,
            record.error_message.unwrap_or_default()
        )),
    }
}

async fn complete_effect_entry(
    state: &AppState,
    capability: Arc<dyn DeliveryCapability>,
    entry_id: Uuid,
    payload: EffectPayload,
    cancellation: CancellationToken,
    effect_seq: u64,
    key: Uuid,
) -> Result<EffectReceipt, String> {
    let request = serde_json::to_value(&payload).expect("effect payload serialises");
    let recovery_host = GatewayEffectRecoveryHost {
        capability: Arc::clone(&capability),
        payload,
        effect_seq,
        cancellation,
    };
    let receipt = recover_started_effect(capability.effect_kind(), key, &request, &recovery_host)
        .await
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
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

struct GatewayEffectRecoveryHost {
    capability: Arc<dyn DeliveryCapability>,
    payload: EffectPayload,
    effect_seq: u64,
    cancellation: CancellationToken,
}

#[async_trait::async_trait]
impl EffectRecoveryHost for GatewayEffectRecoveryHost {
    async fn resend(&self, key: Uuid, _request: &Value) -> Result<Value, String> {
        self.capability
            .deliver(
                key,
                self.payload.clone(),
                self.effect_seq,
                self.cancellation.clone(),
            )
            .await
            .and_then(|receipt| serde_json::to_value(receipt).map_err(|error| error.to_string()))
    }

    async fn probe(&self, key: Uuid, _request: &Value) -> Result<Option<Value>, String> {
        self.capability
            .probe(key, &self.payload)
            .await
            .and_then(|receipt| {
                receipt
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| error.to_string())
            })
    }
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
        script_id: record.script_id,
        script_name: record.script_name,
        script_version: record.script_version,
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
            parent_run_id: None,
            parent_seq: None,
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
            session_id: None,
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
    let replay = state
        .storage
        .list_workflow_journal(record.id)
        .await
        .map_err(internal)?;
    let initial_agent_calls = replay
        .iter()
        .filter(|entry| entry.kind == "agent_call")
        .count() as u32;
    let initial_spent_tokens = replay
        .iter()
        .filter(|entry| entry.kind == "agent_call" && entry.status == "completed")
        .filter_map(|entry| entry.result.as_ref())
        .map(|result| {
            result
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + result
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
        })
        .sum();
    let agent_calls = Arc::new(AtomicU32::new(initial_agent_calls));
    let spent_tokens = Arc::new(AtomicU64::new(initial_spent_tokens));
    let host_task = tokio::spawn(run_host_commands(
        state.clone(),
        record.id,
        space_id,
        delivery_grant.clone(),
        run_budget.clone(),
        replay,
        Arc::clone(&agent_calls),
        Arc::clone(&spent_tokens),
        host_rx,
    ));
    let host: Arc<dyn WorkflowHost> = Arc::new(ChannelWorkflowHost {
        tx: host_tx,
        spent_tokens,
        max_total_tokens: run_budget.max_total_tokens,
    });
    let args_json = serde_json::to_string(&record.args).map_err(internal)?;
    // Composition is a top-level capability only: a child run (one carrying
    // parentage) gets no `workflow()`, which bounds nesting to one level.
    let capabilities = ScriptCapabilities {
        delivery: delivery_grant.is_some(),
        composition: record.parent_run_id.is_none(),
    };
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
                    capabilities,
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
        // The engine drops its host sender before producing the result. Drain the
        // command loop so fire-and-forget log/phase records are durable before the
        // run becomes terminal and visible to trace readers.
        let _ = host_task.await;
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
                "workflow_call" => WorkflowJournalEntryKind::WorkflowCall,
                "timer" => WorkflowJournalEntryKind::Timer,
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

    struct RecordingDeliveryCapability {
        effect_kind: swage_workflow::EffectKind,
        probe_result: Option<EffectReceipt>,
        probes: AtomicUsize,
        deliveries: AtomicUsize,
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

    async fn started_effect_entry() -> (AppState, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                space_id: space.id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: None,
                source: Some("return null".into()),
                source_hash: source_hash("return null"),
                args: Value::Null,
                budget: serde_json::json!({}),
                engine_version: "test".into(),
            })
            .await
            .expect("run");
        let entry = storage
            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                run_id: run.id,
                seq: 0,
                kind: "effect".into(),
                args_hash: journal_args_hash(&Value::Null),
                request: Value::Null,
                effect_kind: Some("test".into()),
                idempotency_key: Some(Uuid::new_v4()),
                target_kind: Some("recording".into()),
            })
            .await
            .expect("started entry");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        (state, entry.id)
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
        let (state, entry_id) = started_effect_entry().await;
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
        let (state, entry_id) = started_effect_entry().await;
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
        let (state, entry_id) = started_effect_entry().await;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn saved_run_projects_script_version_and_parallel_failure_log() {
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
        let (script, version) = storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "trace-contract".into(),
                description: String::new(),
                source: "await parallel([()=>{throw new Error('branch failed')}]); return {spent:budget.spent(),remaining:budget.remaining()};".into(),
                delivery_grant: None,
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
                    max_total_tokens: Some(10),
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
        let space = storage.get_or_create_default().await.expect("space");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "child".into(),
                description: String::new(),
                source: "return { doubled: args.value * 2 };".into(),
                delivery_grant: None,
            })
            .await
            .expect("child script");
        let (parent, _) = storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "parent".into(),
                description: String::new(),
                source: "const child = await workflow(\"child\", { value: 21 }); return child;"
                    .into(),
                delivery_grant: None,
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
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_child_run_cannot_compose_further() {
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
        storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "grandchild".into(),
                description: String::new(),
                source: "return 1;".into(),
                delivery_grant: None,
            })
            .await
            .expect("grandchild script");
        storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "child".into(),
                description: String::new(),
                // In a child run `workflow` must be absent entirely.
                source: "return typeof workflow;".into(),
                delivery_grant: None,
            })
            .await
            .expect("child script");
        let (parent, _) = storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "parent".into(),
                description: String::new(),
                source: "return await workflow(\"child\");".into(),
                delivery_grant: None,
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
