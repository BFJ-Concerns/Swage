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
use swage_core::StorageError;
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

impl AppState {
    async fn create_workflow_journal_intent(
        &self,
        intent: NewWorkflowJournalIntent,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let entry = self.storage.create_workflow_journal_intent(intent).await?;
        if let Ok(wire) = journal_wire(entry.clone()) {
            let _ = self.broadcast.send(ServerEvent::WorkflowEntryStarted {
                event: swage_protocol::client::workflow::WorkflowEntryEvent {
                    run_id: entry.run_id,
                    entry: wire,
                },
            });
        }
        Ok(entry)
    }

    async fn complete_workflow_journal_entry(
        &self,
        id: Uuid,
        result: Value,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let entry = self
            .storage
            .complete_workflow_journal_entry(id, result)
            .await?;
        self.broadcast_terminal_workflow_entry(&entry);
        Ok(entry)
    }

    async fn create_failed_workflow_journal_entry(
        &self,
        intent: NewWorkflowJournalIntent,
        error: &str,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let entry = self
            .storage
            .create_failed_workflow_journal_entry(intent, error)
            .await?;
        self.broadcast_terminal_workflow_entry(&entry);
        Ok(entry)
    }

    async fn fail_workflow_journal_entry(
        &self,
        id: Uuid,
        error: &str,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let entry = self.storage.fail_workflow_journal_entry(id, error).await?;
        self.broadcast_terminal_workflow_entry(&entry);
        Ok(entry)
    }

    fn broadcast_terminal_workflow_entry(&self, entry: &WorkflowJournalRecord) {
        if let Ok(wire) = journal_wire(entry.clone()) {
            let _ = self.broadcast.send(ServerEvent::WorkflowEntryCompleted {
                event: swage_protocol::client::workflow::WorkflowEntryEvent {
                    run_id: entry.run_id,
                    entry: wire,
                },
            });
        }
    }
}

fn journal_wire(
    entry: WorkflowJournalRecord,
) -> Result<swage_protocol::client::workflow::WorkflowJournalEntry, String> {
    use swage_protocol::client::workflow::{
        WorkflowAgentCallDetail, WorkflowEffectDetail, WorkflowJournalEntry,
        WorkflowJournalEntryKind, WorkflowJournalEntryStatus,
    };
    let kind = match entry.kind.as_str() {
        "agent_call" => WorkflowJournalEntryKind::AgentCall,
        "effect" => WorkflowJournalEntryKind::Effect,
        "log" => WorkflowJournalEntryKind::Log,
        "phase" => WorkflowJournalEntryKind::Phase,
        "workflow_call" => WorkflowJournalEntryKind::WorkflowCall,
        "timer" => WorkflowJournalEntryKind::Timer,
        other => return Err(format!("unknown workflow entry kind: {other}")),
    };
    let status = match entry.status.as_str() {
        "started" => WorkflowJournalEntryStatus::Started,
        "completed" => WorkflowJournalEntryStatus::Completed,
        "failed" => WorkflowJournalEntryStatus::Failed,
        other => return Err(format!("unknown workflow entry status: {other}")),
    };
    let effect = entry
        .idempotency_key
        .map(|idempotency_key| WorkflowEffectDetail {
            effect_kind: entry.effect_kind.unwrap_or_default(),
            target_kind: entry.target_kind.unwrap_or_default(),
            idempotency_key,
        });
    let agent_call = (entry.kind == "agent_call").then(|| {
        let result = entry.result.as_ref();
        WorkflowAgentCallDetail {
            agent_id: entry
                .request
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            session_id: result
                .and_then(|v| v.get("session_id"))
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            thread_id: result
                .and_then(|v| v.get("thread_id"))
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            turn_id: result
                .and_then(|v| v.get("turn_id"))
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            input_tokens: result
                .and_then(|v| v.get("input_tokens"))
                .and_then(Value::as_u64),
            output_tokens: result
                .and_then(|v| v.get("output_tokens"))
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
}

#[derive(Default)]
struct TokenBudgetState {
    spent: u64,
    reserved: u64,
}

fn token_reservation_per_call(budget: &WorkflowBudget) -> Option<u64> {
    let limit = budget.max_total_tokens?;
    let calls = budget
        .max_agent_calls
        .filter(|calls| *calls != 0)
        .unwrap_or_else(|| budget.max_concurrent_agent_calls.unwrap_or(4).max(1));
    Some((limit / u64::from(calls)).max(1))
}

async fn record_agent_budget_refusal(
    state: &AppState,
    run_id: Uuid,
    seq: u64,
    request: &AgentCallRequest,
    error: &str,
) -> Result<(), String> {
    let request = serde_json::to_value(request).expect("agent request serialises");
    state
        .create_failed_workflow_journal_entry(
            NewWorkflowJournalIntent {
                run_id,
                seq,
                kind: "agent_call".into(),
                args_hash: journal_args_hash(&request),
                request,
                effect_kind: None,
                idempotency_key: None,
                target_kind: None,
            },
            error,
        )
        .await
        .map_err(|problem| problem.to_string())?;
    Ok(())
}

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
    ConfirmTimer {
        timer_id: u64,
        reply: oneshot::Sender<Result<(), String>>,
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

    async fn confirm_timer(&self, timer_id: u64) -> Result<(), String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(HostCommand::ConfirmTimer { timer_id, reply })
            .map_err(|_| "workflow host stopped".to_string())?;
        receive
            .await
            .map_err(|_| "workflow host dropped timer confirmation".to_string())?
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
    let request_schema = request.schema.clone();
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
    // The turn's failure must surface as this call's error — the shared turn
    // path reports failures over its event channel, which nobody holds here.
    // Falling through would hand the script raw text (or an empty string)
    // where its schema promised an object, silently.
    if turn.state != swage_storage::TurnState::Completed {
        return Err(format!(
            "AgentCallError: agent turn {} {}",
            turn.id,
            match turn.state {
                swage_storage::TurnState::Failed => "failed".to_string(),
                other => format!("ended {other:?}"),
            }
        ));
    }
    let output = match (&request_schema, turn.structured_output) {
        (Some(_), Some(structured)) => structured,
        (Some(_), None) => {
            // Schema demanded, none produced: the validated-object contract
            // cannot be met, and unvalidated text must not impersonate it.
            return Err(format!(
                "AgentCallError: agent turn {} produced no schema-valid output",
                turn.id
            ));
        }
        (None, structured) => {
            structured.unwrap_or_else(|| Value::String(turn.assistant_response.unwrap_or_default()))
        }
    };
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
    // Reserve one equal share of the run budget per declared call, or per
    // concurrency slot when no total call count is declared. This is an
    // estimate, not a provider cap: actual usage can exceed it. Admission and
    // settlement share one lock so actual spend replaces the reservation
    // without exposing a stale gap between separate counters.
    let token_reservation = token_reservation_per_call(&budget);
    let token_budget = Arc::new(Mutex::new(TokenBudgetState {
        spent: spent_tokens.load(Ordering::SeqCst),
        reserved: 0,
    }));
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
    // Pending wait() timers by script-assigned timer id. Firing never writes
    // the journal; the race's verdict does — ConfirmTimer completes the
    // entry (timer won), AbandonTimer cancels the armed sleep and leaves it
    // `started` (timer beaten) — so a completed timer entry always means the
    // timer won a wait, however close the race. The entry id is None when
    // replay found the entry already completed (confirm is then a no-op).
    let mut pending_timers: std::collections::HashMap<u64, CancellationToken> =
        std::collections::HashMap::new();
    let mut timer_entries: std::collections::HashMap<u64, Option<Uuid>> =
        std::collections::HashMap::new();
    while let Some(command) = commands.recv().await {
        match command {
            HostCommand::Record { kind, message } => {
                let request = serde_json::json!({ "message": message });
                let current_seq = seq.fetch_add(1, Ordering::SeqCst);
                // Replay check like every other journalled command: a resumed
                // script re-emits its logs, and the recorded row already
                // holds them — re-inserting would collide on (run_id, seq).
                if replay.get(current_seq as usize).is_some() {
                    continue;
                }
                if let Ok(intent) = state
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
                let mut call_reservation = 0;
                if existing_call.is_none() {
                    if budget
                        .max_agent_calls
                        .is_some_and(|limit| agent_calls.load(Ordering::SeqCst) >= limit)
                    {
                        // Replay admission counting recognises this prefix in `launch_existing_run`.
                        let error = "BudgetExceededError: agent call limit reached";
                        seq.fetch_add(1, Ordering::SeqCst);
                        let result = record_agent_budget_refusal(
                            &state,
                            run_id,
                            current_seq,
                            &request,
                            error,
                        )
                        .await
                        .map_or_else(Err, |_| Err(error.into()));
                        let _ = reply.send(result);
                        continue;
                    }
                }
                if let (Some(limit), Some(reservation)) =
                    (budget.max_total_tokens, token_reservation)
                    && existing_call.is_none_or(|entry| entry.status == "started")
                {
                    let mut accounting = token_budget.lock().await;
                    if existing_call.is_none() {
                        let available = limit
                            .saturating_sub(accounting.spent.saturating_add(accounting.reserved));
                        if available < reservation {
                            drop(accounting);
                            // Replay admission counting recognises this prefix in `launch_existing_run`.
                            let error = "BudgetExceededError: token limit reached";
                            seq.fetch_add(1, Ordering::SeqCst);
                            let result = record_agent_budget_refusal(
                                &state,
                                run_id,
                                current_seq,
                                &request,
                                error,
                            )
                            .await
                            .map_or_else(Err, |_| Err(error.into()));
                            let _ = reply.send(result);
                            continue;
                        }
                    }
                    // A started replay is executing real provider work again. Its
                    // original admission is already included in agent_calls, but
                    // its token estimate must be held again until this attempt
                    // settles so new calls cannot spend the same capacity.
                    accounting.reserved = accounting.reserved.saturating_add(reservation);
                    call_reservation = reservation;
                }
                if existing_call.is_none() {
                    agent_calls.fetch_add(1, Ordering::SeqCst);
                }
                seq.fetch_add(1, Ordering::SeqCst);
                let completed_replay =
                    existing_call.is_some_and(|entry| entry.status == "completed");
                let state = state.clone();
                let replay = Arc::clone(&replay);
                let spent = Arc::clone(&spent_tokens);
                let token_budget = Arc::clone(&token_budget);
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
                        } else if entry.status == "failed" {
                            // The original run observed this failure (and may
                            // have caught it); replay returns the recorded
                            // error rather than re-executing the call.
                            Err(entry
                                .error_message
                                .clone()
                                .unwrap_or_else(|| "recorded agent failure".to_string()))
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
                    let actual = if !completed_replay {
                        result
                            .as_ref()
                            .map(|result| result.input_tokens.saturating_add(result.output_tokens))
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let mut accounting = token_budget.lock().await;
                    accounting.spent = accounting.spent.saturating_add(actual);
                    spent.store(accounting.spent, Ordering::SeqCst);
                    accounting.reserved = accounting.reserved.saturating_sub(call_reservation);
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
                let current_seq = seq.fetch_add(1, Ordering::SeqCst);
                let request_json = serde_json::json!({ "ms": ms });
                let hash = journal_args_hash(&request_json);
                // A completed timer entry replays instantly, so a wait the
                // timer won wakes the same way on resume. A started entry is
                // re-armed whole: the original firing was never recorded, so
                // honesty is a fresh full wait, not a guessed remainder. The
                // intent is written here, in the loop, so timer_entries is
                // populated before any ConfirmTimer can arrive.
                let entry_id =
                    match replay.get(current_seq as usize) {
                        Some(entry) if entry.args_hash != hash => {
                            let _ = reply.send(Err("NondeterministicReplayError".into()));
                            continue;
                        }
                        Some(entry) if entry.status == "completed" => {
                            timer_entries.insert(timer_id, None);
                            let _ =
                                reply.send(entry.result.clone().ok_or_else(|| {
                                    "completed timer entry has no result".to_string()
                                }));
                            continue;
                        }
                        Some(entry) => entry.id,
                        None => {
                            match state
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
                                    continue;
                                }
                            }
                        }
                    };
                timer_entries.insert(timer_id, Some(entry_id));
                let abandon = CancellationToken::new();
                pending_timers.insert(timer_id, abandon.clone());
                // The firing only reports; it writes nothing. The journal
                // settles on the race's verdict via ConfirmTimer.
                tokio::spawn(async move {
                    tokio::select! {
                        () = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {
                            // Reporting only — the entry is completed by
                            // ConfirmTimer once the race declares this
                            // firing the winner. A firing the race ignores
                            // (its wait() already took a completion) leaves
                            // the entry `started`.
                            let _ = reply.send(Ok(serde_json::json!({ "fired": true })));
                        }
                        () = abandon.cancelled() => {}
                        () = cancellation.cancelled() => {
                            let _ = reply.send(Err("WorkflowCancelledError: timer cancelled".into()));
                        }
                    }
                });
            }
            HostCommand::ConfirmTimer { timer_id, reply } => {
                pending_timers.remove(&timer_id);
                let result = match timer_entries.get(&timer_id) {
                    // Replayed-completed: nothing to write.
                    Some(None) => Ok(()),
                    Some(Some(entry_id)) => state
                        .complete_workflow_journal_entry(
                            *entry_id,
                            serde_json::json!({"fired": true}),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string()),
                    None => Err("WaitTimerError: unknown timer id".into()),
                };
                let _ = reply.send(result);
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
                    } else if entry.status == "failed" {
                        // A terminal effect failure is part of the observed
                        // run. Only a started effect enters recovery.
                        Err(entry
                            .error_message
                            .clone()
                            .unwrap_or_else(|| "recorded effect failure".to_string()))
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
    match run_ephemeral_agent_call(state, space_id, request, cancellation).await {
        Ok(result) => {
            state
                .complete_workflow_journal_entry(
                    entry_id,
                    serde_json::to_value(&result).expect("agent result serialises"),
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(result)
        }
        // A failure the script observes (and may catch in parallel/pipeline)
        // must settle durably too, or replay treats the entry as live work
        // and re-executes the call — which may then succeed and change the
        // resumed path. Cancellation is the exception: an interrupted run's
        // in-flight calls are legitimately re-run on resume.
        Err(error) => {
            if !error.starts_with("WorkflowCancelledError") {
                let _ = state.fail_workflow_journal_entry(entry_id, &error).await;
            }
            Err(error)
        }
    }
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
                // Resume with the child's own recorded version, not the
                // script's current head: the record's script_version_id and
                // source_hash identify what ran, and a script update between
                // crash and resume must not swap the source underneath them.
                let recorded_version_id = child
                    .script_version_id
                    .ok_or_else(|| "child workflow run has no script version".to_string())?;
                let source = state
                    .storage
                    .get_workflow_script_version(recorded_version_id)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "child workflow script version is missing".to_string())?
                    .source;
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
            let parent_budget = state
                .storage
                .get_workflow_run(parent_run_id)
                .await
                .map_err(|error| error.to_string())?
                .map(|parent| parent.budget)
                .unwrap_or_else(|| serde_json::json!({}));
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
                    // The child inherits the parent's limits rather than
                    // running unbounded. Coarse — each child gets the full
                    // allowance again rather than drawing down a shared
                    // pool — but confinement flows down, which is what the
                    // budget contract promises.
                    budget: parent_budget,
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
    let result = recover_started_effect(capability.effect_kind(), key, &request, &recovery_host)
        .await
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()));
    match result {
        Ok(receipt) => {
            let serialised = serde_json::to_value(&receipt).expect("receipt serialises");
            match state
                .complete_workflow_journal_entry(entry_id, serialised)
                .await
            {
                Ok(_) => Ok(receipt),
                Err(error) => {
                    let error = error.to_string();
                    let _ = state.fail_workflow_journal_entry(entry_id, &error).await;
                    Err(error)
                }
            }
        }
        Err(error) if error.starts_with("WorkflowCancelledError") => Err(error),
        Err(error) => {
            let _ = state.fail_workflow_journal_entry(entry_id, &error).await;
            Err(error)
        }
    }
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
    // These prefixes are produced by both budget-refusal paths in the host-command loop.
    let initial_agent_calls = replay
        .iter()
        .filter(|entry| {
            entry.kind == "agent_call"
                && !entry
                    .error_message
                    .as_deref()
                    .is_some_and(|error| error.starts_with("BudgetExceededError:"))
        })
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
        .map(|entry| journal_wire(entry).map_err(internal))
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
        ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>>
        {
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
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
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
        let space = storage.get_or_create_default().await.expect("space");
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_host_commands(
            state,
            run_id,
            space.id,
            None,
            WorkflowBudget {
                max_agent_calls: None,
                max_total_tokens: None,
                max_concurrent_agent_calls: None,
            },
            replay,
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU64::new(0)),
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
        let space = storage.get_or_create_default().await.expect("space");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let mut events = state.broadcast.subscribe();
        let Json(started) = start_inline_run(
            State(state.clone()),
            Path(space.id),
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
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].id, observed[1].id);
        let Json(detail) = get_run(State(state.clone()), Path(started.id))
            .await
            .expect("detail");
        assert_eq!(observed[1].status, detail.journal[0].status);
        assert_eq!(
            observed[1].completed_order,
            detail.journal[0].completed_order
        );

        let failed = state
            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                run_id: started.id,
                seq: 1,
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
    async fn token_reservation_refuses_a_concurrent_call_at_admission() {
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
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::PendingProvider),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_host_commands(
            state,
            run.id,
            space.id,
            None,
            WorkflowBudget {
                max_agent_calls: None,
                max_total_tokens: Some(1_000_000),
                max_concurrent_agent_calls: Some(2),
            },
            vec![],
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU64::new(0)),
            rx,
        ));
        let request = AgentCallRequest {
            prompt: "hold capacity".into(),
            agent: "default".into(),
            model: None,
            schema: None,
            max_attempts: 1,
            label: None,
        };
        let first_cancellation = CancellationToken::new();
        let (first_reply, first_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: request.clone(),
            cancellation: first_cancellation.clone(),
            reply: first_reply,
        })
        .expect("first call");
        let second_cancellation = CancellationToken::new();
        let (second_reply, second_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: request.clone(),
            cancellation: second_cancellation.clone(),
            reply: second_reply,
        })
        .expect("second call");
        let (third_reply, third_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request,
            cancellation: CancellationToken::new(),
            reply: third_reply,
        })
        .expect("third call");
        assert_eq!(
            third_receive.await.expect("reply").expect_err("refused"),
            "BudgetExceededError: token limit reached"
        );
        let journal = storage
            .list_workflow_journal(run.id)
            .await
            .expect("journal");
        assert_eq!(journal.len(), 3);
        assert_eq!(journal[2].status, "failed");
        assert_eq!(
            journal[2].error_message.as_deref(),
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
        assert!(!first_error.starts_with("BudgetExceededError:"));
        assert!(!second_error.starts_with("BudgetExceededError:"));
        drop(tx);
        task.await.expect("host loop");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn token_reservation_settlement_releases_estimate_and_retains_actual_spend() {
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
        let pending_call_started = Arc::new(Notify::new());
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
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
            space.id,
            None,
            WorkflowBudget {
                max_agent_calls: None,
                max_total_tokens: Some(1_000_000),
                max_concurrent_agent_calls: Some(2),
            },
            vec![],
            Arc::new(AtomicU32::new(0)),
            Arc::clone(&spent),
            rx,
        ));
        let request = AgentCallRequest {
            prompt: "account usage".into(),
            agent: "default".into(),
            model: None,
            schema: None,
            max_attempts: 1,
            label: None,
        };
        let (first_reply, first_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: request.clone(),
            cancellation: CancellationToken::new(),
            reply: first_reply,
        })
        .expect("settling call");
        let settled = first_receive
            .await
            .expect("first reply")
            .expect("first call");
        assert_eq!(settled.input_tokens + settled.output_tokens, 400_000);
        assert_eq!(spent.load(Ordering::SeqCst), 400_000);

        let pending_cancellation = CancellationToken::new();
        let (second_reply, second_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: request.clone(),
            cancellation: pending_cancellation.clone(),
            reply: second_reply,
        })
        .expect("call admitted after settlement");
        pending_call_started.notified().await;
        let (third_reply, third_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request,
            cancellation: CancellationToken::new(),
            reply: third_reply,
        })
        .expect("call competing with retained spend");
        assert_eq!(
            third_receive
                .await
                .expect("third reply")
                .expect_err("refused"),
            "BudgetExceededError: token limit reached"
        );
        pending_cancellation.cancel();
        assert!(second_receive.await.expect("second reply").is_err());
        drop(tx);
        task.await.expect("host loop");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replayed_started_calls_hold_token_reservations_until_settlement() {
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
        let request = AgentCallRequest {
            prompt: "resume work".into(),
            agent: "default".into(),
            model: None,
            schema: None,
            max_attempts: 1,
            label: None,
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
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::PendingProvider),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_host_commands(
            state,
            run.id,
            space.id,
            None,
            WorkflowBudget {
                max_agent_calls: None,
                max_total_tokens: Some(1_000_000),
                max_concurrent_agent_calls: Some(2),
            },
            replay,
            Arc::new(AtomicU32::new(2)),
            Arc::new(AtomicU64::new(0)),
            rx,
        ));
        let first_cancellation = CancellationToken::new();
        let (first_reply, first_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: request.clone(),
            cancellation: first_cancellation.clone(),
            reply: first_reply,
        })
        .expect("first replay");
        let second_cancellation = CancellationToken::new();
        let (second_reply, second_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request: request.clone(),
            cancellation: second_cancellation.clone(),
            reply: second_reply,
        })
        .expect("second replay");
        let (third_reply, third_receive) = oneshot::channel();
        tx.send(HostCommand::Agent {
            request,
            cancellation: CancellationToken::new(),
            reply: third_reply,
        })
        .expect("new call");
        assert_eq!(
            third_receive
                .await
                .expect("third reply")
                .expect_err("refused"),
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
    async fn resumed_run_excludes_journalled_refusal_from_agent_call_count() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        let source = "try { await agent('must not execute', {agent:'default'}); } catch (_) {} return await agent('does execute', {agent:'default'});";
        let budget = WorkflowBudget {
            max_agent_calls: Some(1),
            max_total_tokens: Some(1_000_000),
            max_concurrent_agent_calls: Some(4),
        };
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                space_id: space.id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: None,
                source: Some(source.into()),
                source_hash: source_hash(source),
                args: Value::Null,
                budget: serde_json::to_value(&budget).expect("budget"),
                engine_version: "test".into(),
            })
            .await
            .expect("run");
        let request = AgentCallRequest {
            prompt: "must not execute".into(),
            agent: "default".into(),
            model: None,
            schema: None,
            max_attempts: 1,
            label: None,
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
            .fail_workflow_journal_entry(
                refused.id,
                "BudgetExceededError: agent call limit reached",
            )
            .await
            .expect("refuse call");
        storage
            .update_workflow_run_state(run.id, "interrupted", None, None)
            .await
            .expect("interrupt run");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
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
        assert_eq!(journal.len(), 2);
        assert_eq!(journal[0].id, refused.id);
        assert_eq!(journal[1].status, "completed");
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

        // Genuine replay: interrupt the finished run and resume it. The
        // completed timer entry must replay instantly to the same winner,
        // adding no second timer entry to the journal.
        storage
            .update_workflow_run_state(started.id, "interrupted", None, None)
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
