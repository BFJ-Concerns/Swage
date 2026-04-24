use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_agent::{AgentBuilder, AgentError, AgentEvent, TurnRequest};
use swage_context::tokeniser::TiktokenTokeniser;
use swage_core::{ChatMessage, Role, StorageError};
use swage_hook::{HookEvent, HookOutcome, HookRegistry};
use swage_storage::model::{
    NewEdgeEvaluation, NewNodeExecution, NewNodeRetry, NewSession, NewThread, NewTopologyRun,
    NodeExecutionState, Team, TeamAgentDefinition, TeamTaskDefinition, TopologyNodeRecordKind,
    TopologyRunState,
};
use swage_storage::store::ThreadStore;
use swage_topology::{EdgeRouting, TopologyGraph, TopologyNode, TopologyNodeKind};

use crate::state::AppState;
use crate::turn::{
    GatewayDelegationHandler, build_agent_config, build_assembly_elements,
    build_gateway_dispatcher, load_bench_memory,
};
use crate::websocket::ServerEvent;

#[derive(Debug, Error)]
pub enum TopologyEngineError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Config(#[from] swage_config::ConfigError),
    #[error("topology hook blocked {event}: {reason}")]
    HookBlocked { event: String, reason: String },
    #[error("topology hook requested approval for {event}: {prompt}")]
    HookApprovalRequested { event: String, prompt: String },
    #[error("bench {bench_id} for team {team_id} was not found")]
    TeamBenchMissing { team_id: Uuid, bench_id: Uuid },
    #[error("session {session_id} was not found")]
    SessionMissing { session_id: Uuid },
    #[error("session {session_id} does not belong to team bench {bench_id}")]
    SessionBenchMismatch { session_id: Uuid, bench_id: Uuid },
    #[error("topology graph is invalid at runtime: {0}")]
    InvalidGraph(String),
}

#[derive(Debug, Clone)]
pub struct TopologyExecutionRequest {
    pub team: Team,
    pub graph: TopologyGraph,
    pub initial_input: serde_json::Value,
    pub session_id: Option<Uuid>,
    pub source_turn_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalNodeResult {
    pub node_id: String,
    pub output_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct TopologyExecutionResult {
    pub topology_run_id: Uuid,
    pub session_id: Uuid,
    pub final_output: serde_json::Value,
    pub terminal_results: Vec<TerminalNodeResult>,
}

#[derive(Clone)]
pub struct TopologyExecutor {
    state: AppState,
    hook_registry: Option<Arc<HookRegistry>>,
}

impl TopologyExecutor {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            hook_registry: None,
        }
    }

    pub fn with_hook_registry(mut self, hook_registry: Arc<HookRegistry>) -> Self {
        self.hook_registry = Some(hook_registry);
        self
    }

    pub async fn execute(
        &self,
        request: TopologyExecutionRequest,
    ) -> Result<TopologyExecutionResult, TopologyEngineError> {
        let bench = self
            .state
            .bench_store
            .get(request.team.bench_id)
            .await?
            .ok_or(TopologyEngineError::TeamBenchMissing {
                team_id: request.team.id,
                bench_id: request.team.bench_id,
            })?;

        let session_id = self
            .ensure_run_session(&request.team, request.session_id)
            .await?;

        let topology_run = self
            .state
            .storage
            .create_topology_run(NewTopologyRun {
                team_id: request.team.id,
                session_id,
                source_turn_id: request.source_turn_id,
                state: TopologyRunState::Running,
                final_output: None,
                error_message: None,
            })
            .await?;

        if let Err(error) = dispatch_hook(
            self.hook_registry.as_ref(),
            HookEvent::TopologyStart {
                topology_run_id: topology_run.id.to_string(),
                team_id: request.team.id.to_string(),
                session_id: session_id.to_string(),
            },
        )
        .await
        {
            let error_message = error.to_string();
            self.state
                .storage
                .update_topology_run(
                    topology_run.id,
                    TopologyRunState::Failed,
                    None,
                    Some(&error_message),
                )
                .await?;
            broadcast_topology_event(
                &self.state,
                ServerEvent::TopologyRunCompleted {
                    session_id,
                    topology_run_id: topology_run.id,
                    success: false,
                    final_output: None,
                },
            );
            return Err(error);
        }
        broadcast_topology_event(
            &self.state,
            ServerEvent::TopologyRunStarted {
                session_id,
                topology_run_id: topology_run.id,
                team_id: request.team.id,
            },
        );

        let node_map: HashMap<String, TopologyNode> = request
            .graph
            .nodes
            .iter()
            .cloned()
            .map(|node| (node.id.clone(), node))
            .collect();
        let concurrency_limit = request
            .graph
            .budget
            .max_concurrency
            .map(|value| value.max(1) as usize)
            .unwrap_or(usize::MAX);
        let run_cancel_token = CancellationToken::new();
        let mut visit_counts: HashMap<String, u32> = HashMap::new();
        let mut pending = VecDeque::from([PendingActivation {
            node_id: request.graph.entry_node_id.clone(),
            input_json: request.initial_input,
        }]);
        let mut active = JoinSet::new();
        let mut terminal_results = Vec::new();
        let mut run_failed = None::<String>;
        let mut total_tokens_consumed = 0u64;

        while !pending.is_empty() || !active.is_empty() {
            while active.len() < concurrency_limit && !pending.is_empty() {
                let activation = pending.pop_front().expect("pending activation exists");
                let node = node_map.get(&activation.node_id).cloned().ok_or_else(|| {
                    TopologyEngineError::InvalidGraph(format!(
                        "node '{}' disappeared during execution",
                        activation.node_id
                    ))
                })?;

                let visit_count = visit_counts.entry(node.id.clone()).or_default();
                *visit_count += 1;
                if let Some(limit) = node.iteration_limit {
                    if *visit_count > limit {
                        run_failed = Some(format!(
                            "node '{}' exceeded iteration limit {}",
                            node.id, limit
                        ));
                        run_cancel_token.cancel();
                        break;
                    }
                }

                let state = self.state.clone();
                let team = request.team.clone();
                let bench = bench.clone();
                let topology_run_id = topology_run.id;
                let cancel_token = run_cancel_token.child_token();
                let hook_registry = self.hook_registry.clone();
                active.spawn(async move {
                    execute_node(
                        state,
                        bench,
                        team,
                        session_id,
                        topology_run_id,
                        node,
                        activation.input_json,
                        hook_registry,
                        cancel_token,
                    )
                    .await
                });
            }

            if run_failed.is_some() {
                break;
            }

            let Some(joined) = active.join_next().await else {
                continue;
            };

            let completed = match joined {
                Ok(result) => match result {
                    Ok(output) => output,
                    Err(error) => {
                        run_failed = Some(error.to_string());
                        run_cancel_token.cancel();
                        break;
                    }
                },
                Err(join_error) => {
                    run_failed = Some(format!("topology node task panicked: {join_error}"));
                    run_cancel_token.cancel();
                    break;
                }
            };

            if let Some(max_total_tokens) = request.graph.budget.max_total_tokens {
                let node_tokens = completed.input_tokens as u64 + completed.output_tokens as u64;
                total_tokens_consumed =
                    total_tokens_consumed
                        .checked_add(node_tokens)
                        .ok_or_else(|| {
                            TopologyEngineError::InvalidGraph(
                                "node token accounting overflowed unexpectedly".into(),
                            )
                        })?;

                if total_tokens_consumed > max_total_tokens {
                    run_failed = Some(format!(
                        "topology token budget exceeded: {} > {}",
                        total_tokens_consumed, max_total_tokens
                    ));
                    run_cancel_token.cancel();
                    break;
                }
            }

            let outgoing_edges: Vec<_> = request
                .graph
                .edges
                .iter()
                .filter(|edge| edge.source_node_id == completed.node_id)
                .cloned()
                .collect();

            let next_activations = match evaluate_edges(
                &self.state,
                topology_run.id,
                &request.graph,
                &completed,
                outgoing_edges,
                self.hook_registry.as_ref(),
            )
            .await
            {
                Ok(next_activations) => next_activations,
                Err(error) => {
                    run_failed = Some(error.to_string());
                    run_cancel_token.cancel();
                    break;
                }
            };

            if next_activations.is_empty() {
                terminal_results.push(TerminalNodeResult {
                    node_id: completed.node_id,
                    output_json: completed.output_json,
                });
            } else {
                for activation in next_activations {
                    pending.push_back(activation);
                }
            }
        }

        run_cancel_token.cancel();
        while active.join_next().await.is_some() {}

        if let Some(error_message) = run_failed {
            self.state
                .storage
                .update_topology_run(
                    topology_run.id,
                    TopologyRunState::Failed,
                    None,
                    Some(&error_message),
                )
                .await?;
            if let Err(error) = dispatch_hook(
                self.hook_registry.as_ref(),
                HookEvent::TopologyEnd {
                    topology_run_id: topology_run.id.to_string(),
                    team_id: request.team.id.to_string(),
                    session_id: session_id.to_string(),
                    success: false,
                },
            )
            .await
            {
                tracing::warn!(error = %error, "topology end hook outcome could not be enforced after failure");
            }
            broadcast_topology_event(
                &self.state,
                ServerEvent::TopologyRunCompleted {
                    session_id,
                    topology_run_id: topology_run.id,
                    success: false,
                    final_output: None,
                },
            );
            return Err(TopologyEngineError::InvalidGraph(error_message));
        }

        let final_output = match terminal_results.as_slice() {
            [single] => single.output_json.clone(),
            many => serde_json::Value::Array(
                many.iter()
                    .map(|result| {
                        serde_json::json!({
                            "node_id": result.node_id,
                            "output": result.output_json,
                        })
                    })
                    .collect(),
            ),
        };

        self.state
            .storage
            .update_topology_run(
                topology_run.id,
                TopologyRunState::Completed,
                Some(&final_output),
                None,
            )
            .await?;

        if let Err(error) = dispatch_hook(
            self.hook_registry.as_ref(),
            HookEvent::TopologyEnd {
                topology_run_id: topology_run.id.to_string(),
                team_id: request.team.id.to_string(),
                session_id: session_id.to_string(),
                success: true,
            },
        )
        .await
        {
            tracing::warn!(error = %error, "topology end hook outcome could not be enforced after success");
        }
        broadcast_topology_event(
            &self.state,
            ServerEvent::TopologyRunCompleted {
                session_id,
                topology_run_id: topology_run.id,
                success: true,
                final_output: Some(final_output.clone()),
            },
        );

        Ok(TopologyExecutionResult {
            topology_run_id: topology_run.id,
            session_id,
            final_output,
            terminal_results,
        })
    }

    async fn ensure_run_session(
        &self,
        team: &Team,
        session_id: Option<Uuid>,
    ) -> Result<Uuid, TopologyEngineError> {
        match session_id {
            Some(session_id) => {
                let session = self
                    .state
                    .storage
                    .get_session(session_id)
                    .await?
                    .ok_or(TopologyEngineError::SessionMissing { session_id })?;
                if session.bench_id != team.bench_id {
                    return Err(TopologyEngineError::SessionBenchMismatch {
                        session_id,
                        bench_id: team.bench_id,
                    });
                }
                Ok(session_id)
            }
            None => Ok(self
                .state
                .storage
                .create_session(NewSession {
                    bench_id: team.bench_id,
                    agent_id: team.entry_agent_id.clone(),
                    metadata: None,
                })
                .await?
                .id),
        }
    }
}

#[derive(Debug)]
struct PendingActivation {
    node_id: String,
    input_json: serde_json::Value,
}

#[derive(Debug)]
struct NodeCompletion {
    node_id: String,
    node_execution_id: Uuid,
    output_json: serde_json::Value,
    input_tokens: u32,
    output_tokens: u32,
}

async fn execute_node(
    state: AppState,
    bench: swage_storage::model::Bench,
    team: Team,
    session_id: Uuid,
    topology_run_id: Uuid,
    node: TopologyNode,
    input_json: serde_json::Value,
    hook_registry: Option<Arc<HookRegistry>>,
    cancel_token: CancellationToken,
) -> Result<NodeCompletion, TopologyEngineError> {
    let node_execution = state
        .storage
        .create_node_execution(NewNodeExecution {
            topology_run_id,
            node_id: node.id.clone(),
            node_kind: match node.kind {
                TopologyNodeKind::Llm => TopologyNodeRecordKind::Llm,
                TopologyNodeKind::Deterministic => TopologyNodeRecordKind::Deterministic,
                TopologyNodeKind::Continuation => TopologyNodeRecordKind::Continuation,
            },
            agent_id: node.agent_id.clone(),
            task_name: node.task_name.clone(),
            state: NodeExecutionState::Running,
            input_json: Some(input_json.clone()),
            output_json: None,
            error_message: None,
        })
        .await?;

    dispatch_hook(
        hook_registry.as_ref(),
        HookEvent::NodeStart {
            topology_run_id: topology_run_id.to_string(),
            node_execution_id: node_execution.id.to_string(),
            node_id: node.id.clone(),
            agent_id: node.agent_id.clone(),
        },
    )
    .await?;
    broadcast_topology_event(
        &state,
        ServerEvent::TopologyNodeStarted {
            session_id,
            topology_run_id,
            node_execution_id: node_execution.id,
            node_id: node.id.clone(),
        },
    );

    let result = match node.kind {
        TopologyNodeKind::Deterministic => execute_deterministic_node(&node, &input_json),
        TopologyNodeKind::Llm | TopologyNodeKind::Continuation => {
            let agent_id = node
                .agent_id
                .clone()
                .unwrap_or_else(|| team.entry_agent_id.clone());
            execute_agent_node(
                &state,
                &bench,
                &team,
                session_id,
                &node,
                &agent_id,
                &input_json,
                cancel_token,
            )
            .await
        }
    };

    match result {
        Ok(node_result) => {
            state
                .storage
                .update_node_execution(
                    node_execution.id,
                    NodeExecutionState::Completed,
                    Some(&node_result.output_json),
                    None,
                )
                .await?;

            for retry in node_result.retries {
                state
                    .storage
                    .create_node_retry(NewNodeRetry {
                        node_execution_id: node_execution.id,
                        attempt_number: retry.attempt_number,
                        failure_kind: retry.failure_kind,
                        error_message: retry.error_message,
                        recovery_guidance: retry.recovery_guidance,
                    })
                    .await?;
            }

            dispatch_hook(
                hook_registry.as_ref(),
                HookEvent::NodeEnd {
                    topology_run_id: topology_run_id.to_string(),
                    node_execution_id: node_execution.id.to_string(),
                    node_id: node.id.clone(),
                    success: true,
                    output: Some(node_result.output_json.clone()),
                },
            )
            .await?;
            broadcast_topology_event(
                &state,
                ServerEvent::TopologyNodeCompleted {
                    session_id,
                    topology_run_id,
                    node_execution_id: node_execution.id,
                    node_id: node.id.clone(),
                    success: true,
                },
            );

            Ok(NodeCompletion {
                node_id: node.id,
                node_execution_id: node_execution.id,
                output_json: node_result.output_json,
                input_tokens: node_result.input_tokens,
                output_tokens: node_result.output_tokens,
            })
        }
        Err(error) => {
            let node_id = node.id.clone();
            state
                .storage
                .update_node_execution(
                    node_execution.id,
                    NodeExecutionState::Failed,
                    None,
                    Some(&error.to_string()),
                )
                .await?;
            dispatch_hook(
                hook_registry.as_ref(),
                HookEvent::NodeEnd {
                    topology_run_id: topology_run_id.to_string(),
                    node_execution_id: node_execution.id.to_string(),
                    node_id: node_id.clone(),
                    success: false,
                    output: None,
                },
            )
            .await?;
            broadcast_topology_event(
                &state,
                ServerEvent::TopologyNodeCompleted {
                    session_id,
                    topology_run_id,
                    node_execution_id: node_execution.id,
                    node_id,
                    success: false,
                },
            );
            Err(error)
        }
    }
}

#[derive(Debug)]
struct AgentNodeResult {
    output_json: serde_json::Value,
    input_tokens: u32,
    output_tokens: u32,
    retries: Vec<NodeRetryRecord>,
}

#[derive(Debug)]
struct NodeRetryRecord {
    attempt_number: i32,
    failure_kind: String,
    error_message: String,
    recovery_guidance: Option<String>,
}

async fn execute_agent_node(
    state: &AppState,
    bench: &swage_storage::model::Bench,
    team: &Team,
    session_id: Uuid,
    node: &TopologyNode,
    agent_id: &str,
    input_json: &serde_json::Value,
    cancel_token: CancellationToken,
) -> Result<AgentNodeResult, TopologyEngineError> {
    let config = resolve_node_config(state, bench, team, agent_id, node.task_name.as_deref())?;
    let rendered_input = render_node_input(node, input_json);

    let thread = ThreadStore::create(
        state.storage.as_ref(),
        NewThread {
            session_id,
            agent_id: agent_id.to_string(),
        },
    )
    .await?;

    state
        .storage
        .append_message(swage_storage::model::AppendMessage {
            thread_id: thread.id,
            turn_id: None,
            role: Role::User,
            content: rendered_input.clone(),
            agent_id: agent_id.to_string(),
        })
        .await?;
    let stored_history = state.storage.list_messages_by_thread(thread.id).await?;
    let history_messages: Vec<ChatMessage> = stored_history
        .into_iter()
        .map(|message| ChatMessage::new(message.role, message.content))
        .collect();

    let swage_root_opt: Option<PathBuf> = swage_config::discovery::resolve_root().ok();
    let memory_content = match swage_root_opt.as_deref() {
        Some(root) => load_bench_memory(root, &bench.slug).await,
        None => String::new(),
    };

    let assembly_search_paths: Vec<(swage_config::HierarchyLevel, PathBuf)> = swage_root_opt
        .as_ref()
        .map(|root| {
            let bench_dir = root.join("benches").join(&bench.slug);
            let agent_dir = bench_dir.join("agents").join(agent_id);
            let mut levels = vec![
                (swage_config::HierarchyLevel::Global, root.clone()),
                (swage_config::HierarchyLevel::Bench, bench_dir),
            ];
            if agent_dir.is_dir() {
                levels.push((swage_config::HierarchyLevel::Agent, agent_dir));
            }
            levels
        })
        .unwrap_or_default();
    let search_level_refs: Vec<(swage_config::HierarchyLevel, &Path)> = assembly_search_paths
        .iter()
        .map(|(level, path)| (level.clone(), path.as_path()))
        .collect();

    let elements = build_assembly_elements(
        &config.instructions,
        vec![],
        history_messages,
        memory_content,
        bench.name.clone(),
    );
    let tokeniser = TiktokenTokeniser::new(&config.provider.model);
    let assembly_runner =
        swage_python::ConfiguredAssemblyRunner::new(config.tools.python_execute.clone());
    let output = swage_context::assembly::assemble(
        &elements,
        &config.context,
        &tokeniser,
        &search_level_refs,
        state.provider.as_ref(),
        Some(&assembly_runner),
    )
    .await;

    let (silent_ws_tx, _silent_ws_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let dispatcher = build_gateway_dispatcher(state, &config, None, silent_ws_tx).await;
    let agent_config =
        build_agent_config(agent_id.to_string(), &config, node.output_schema.clone());
    let mut agent_builder = AgentBuilder::new()
        .config(agent_config)
        .provider(state.provider.clone())
        .storage(state.storage.clone())
        .tool_dispatcher(dispatcher)
        .approval_registrar(crate::turn::build_approval_registrar(
            state.approval_channels.clone(),
        ))
        .delegation_handler(Arc::new(GatewayDelegationHandler::new(state.clone())))
        .context_transfer_config(config.communication.context_transfer.clone());
    if let Some(hook_registry) = state.hook_registry.clone() {
        agent_builder = agent_builder.hook_registry(hook_registry);
    }
    let agent = agent_builder.build()?;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let outbound = agent
        .process_turn_streaming(
            TurnRequest {
                execution: swage_agent::context::TurnExecutionContext {
                    session_id,
                    thread_id: thread.id,
                    turn_id: Uuid::new_v4(),
                    bench_id: Some(bench.id),
                },
                user_message: rendered_input,
                assembled_context: output.messages,
                parent_turn_id: None,
                existing_turn_id: None,
                cancellation_token: cancel_token,
            },
            event_tx,
        )
        .await;

    let mut turn_id = None;
    let mut retries = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        match event {
            AgentEvent::TurnStarted {
                turn_id: current_turn_id,
            } => turn_id = Some(current_turn_id),
            AgentEvent::OutputSchemaRetry {
                attempt, errors, ..
            } => {
                retries.push(NodeRetryRecord {
                    attempt_number: attempt as i32,
                    failure_kind: "output_schema".into(),
                    error_message: errors
                        .into_iter()
                        .map(|error| {
                            if error.path.is_empty() {
                                error.message
                            } else {
                                format!("{}: {}", error.path, error.message)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("; "),
                    recovery_guidance: Some("Return valid JSON matching the node schema".into()),
                });
            }
            _ => {}
        }
    }

    let outbound = outbound?;
    if let Some(turn_id) = turn_id {
        if let Some(ref output) = outbound.structured_output {
            state
                .storage
                .update_structured_output(turn_id, output)
                .await?;
        }
        if let Some(cost) = state.config.pricing.compute_cost(
            &config.provider.model,
            outbound.input_tokens,
            outbound.output_tokens,
        ) {
            state.storage.update_cost(turn_id, cost).await?;
        }
    }
    state.storage.update_activity(session_id).await?;

    Ok(AgentNodeResult {
        output_json: outbound
            .structured_output
            .clone()
            .or_else(|| serde_json::from_str::<serde_json::Value>(&outbound.response).ok())
            .unwrap_or_else(|| serde_json::json!({ "response": outbound.response })),
        input_tokens: outbound.input_tokens,
        output_tokens: outbound.output_tokens,
        retries,
    })
}

fn execute_deterministic_node(
    node: &TopologyNode,
    input_json: &serde_json::Value,
) -> Result<AgentNodeResult, TopologyEngineError> {
    let handler_name = node.handler_name.as_deref().ok_or_else(|| {
        TopologyEngineError::InvalidGraph(format!(
            "deterministic node '{}' is missing a handler_name",
            node.id
        ))
    })?;

    let output_json = match handler_name {
        "identity" => input_json.clone(),
        "collect-results" => match input_json {
            serde_json::Value::Array(items) => serde_json::json!({ "results": items }),
            other => serde_json::json!({ "result": other }),
        },
        other => {
            return Err(TopologyEngineError::InvalidGraph(format!(
                "unknown deterministic handler '{other}'"
            )));
        }
    };

    Ok(AgentNodeResult {
        output_json,
        input_tokens: 0,
        output_tokens: 0,
        retries: vec![],
    })
}

fn resolve_node_config(
    state: &AppState,
    bench: &swage_storage::model::Bench,
    team: &Team,
    agent_id: &str,
    task_name: Option<&str>,
) -> Result<swage_config::ResolvedConfig, TopologyEngineError> {
    let mut config = load_topology_node_config(state, bench, team, agent_id)?;

    if let Some(agent) = team
        .agents
        .iter()
        .find(|candidate| candidate.id == agent_id)
    {
        config = apply_team_agent_definition(config, agent);
    }
    if let Some(task_name) = task_name {
        if let Some(task) = team
            .tasks
            .iter()
            .find(|candidate| candidate.name == task_name)
        {
            config = team_task_to_config(task).apply_to(config);
        }
    }

    Ok(config)
}

fn load_topology_node_config(
    state: &AppState,
    bench: &swage_storage::model::Bench,
    team: &Team,
    agent_id: &str,
) -> Result<swage_config::ResolvedConfig, TopologyEngineError> {
    let Ok(root) = swage_config::discovery::resolve_root() else {
        return Ok(state.config.clone());
    };

    let bench_dir = root.join("benches").join(&bench.slug);
    let team_dir = bench_dir.join("teams").join(&team.name);
    let agent_dir = bench_dir.join("agents").join(agent_id);

    let has_filesystem_config = bench_dir.join("config.toml").exists()
        || team_dir.join("config.toml").exists()
        || agent_dir.join("config.toml").exists();

    if !has_filesystem_config {
        return Ok(state.config.clone());
    }

    swage_config::load_resolved_config_from_with_team(
        &root,
        &bench.slug,
        Some(&team.name),
        agent_id,
    )
    .map_err(TopologyEngineError::from)
}

fn render_node_input(node: &TopologyNode, input_json: &serde_json::Value) -> String {
    let task_prefix = node
        .task_name
        .as_ref()
        .map(|task_name| format!("Task: {task_name}\n\n"))
        .unwrap_or_default();

    match input_json {
        serde_json::Value::String(text) if task_prefix.is_empty() => text.clone(),
        serde_json::Value::String(text) => format!("{task_prefix}Input:\n{text}"),
        value => format!(
            "{task_prefix}Input JSON:\n{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        ),
    }
}

fn apply_team_agent_definition(
    mut config: swage_config::ResolvedConfig,
    agent: &TeamAgentDefinition,
) -> swage_config::ResolvedConfig {
    if let Some(model) = agent.model.as_deref() {
        config.provider.model = model.to_string();
    }
    if !agent.instructions.trim().is_empty() {
        config.instructions = match config.instructions.trim().is_empty() {
            true => agent.instructions.clone(),
            false => format!("{}\n\n{}", config.instructions, agent.instructions),
        };
    }
    config
}

fn team_task_to_config(task: &TeamTaskDefinition) -> swage_config::TaskDefinition {
    swage_config::TaskDefinition {
        name: task.name.clone(),
        description: task.description.clone(),
        instructions: task.instructions.clone(),
        model: task.model.clone(),
        output_schema: task.output_schema.clone(),
        max_output_retries: task.max_output_retries,
        tool_allow: task.tool_allow.clone(),
        tool_deny: task.tool_deny.clone(),
        tool_bundles: None,
        tool_require_approval: None,
    }
}

async fn evaluate_edges(
    state: &AppState,
    topology_run_id: Uuid,
    graph: &TopologyGraph,
    completed: &NodeCompletion,
    edges: Vec<swage_topology::TopologyEdge>,
    hook_registry: Option<&Arc<HookRegistry>>,
) -> Result<Vec<PendingActivation>, TopologyEngineError> {
    let mut activations = Vec::new();
    let max_fan_out = graph.budget.max_fan_out.unwrap_or(u32::MAX) as usize;

    for edge in edges {
        match &edge.routing {
            EdgeRouting::Unconditional | EdgeRouting::Fork => {
                state
                    .storage
                    .create_edge_evaluation(NewEdgeEvaluation {
                        topology_run_id,
                        source_node_execution_id: completed.node_execution_id,
                        edge_id: edge.id.clone(),
                        target_node_id: edge.target_node_id.clone(),
                        condition_expression: None,
                        matched: true,
                        payload_json: Some(completed.output_json.clone()),
                    })
                    .await?;
                activations.push(PendingActivation {
                    node_id: edge.target_node_id.clone(),
                    input_json: completed.output_json.clone(),
                });
            }
            EdgeRouting::Condition { field_path, equals } => {
                let actual = extract_field_value(&completed.output_json, field_path);
                let matched = actual == Some(equals);
                state
                    .storage
                    .create_edge_evaluation(NewEdgeEvaluation {
                        topology_run_id,
                        source_node_execution_id: completed.node_execution_id,
                        edge_id: edge.id.clone(),
                        target_node_id: edge.target_node_id.clone(),
                        condition_expression: Some(format!("{field_path} == {equals}")),
                        matched,
                        payload_json: actual.cloned(),
                    })
                    .await?;
                if matched {
                    activations.push(PendingActivation {
                        node_id: edge.target_node_id.clone(),
                        input_json: completed.output_json.clone(),
                    });
                }
            }
            EdgeRouting::FanOut { items_field } => {
                let Some(items) = extract_field_value(&completed.output_json, items_field)
                    .and_then(|value| value.as_array())
                else {
                    return Err(TopologyEngineError::InvalidGraph(format!(
                        "fan-out edge '{}' expected array field '{}' at runtime",
                        edge.id, items_field
                    )));
                };

                if items.len() > max_fan_out {
                    return Err(TopologyEngineError::InvalidGraph(format!(
                        "fan-out edge '{}' produced {} items, exceeding limit {}",
                        edge.id,
                        items.len(),
                        max_fan_out
                    )));
                }

                if items.is_empty() {
                    state
                        .storage
                        .create_edge_evaluation(NewEdgeEvaluation {
                            topology_run_id,
                            source_node_execution_id: completed.node_execution_id,
                            edge_id: edge.id.clone(),
                            target_node_id: edge.target_node_id.clone(),
                            condition_expression: Some(format!("fan_out {items_field}")),
                            matched: false,
                            payload_json: Some(serde_json::Value::Array(vec![])),
                        })
                        .await?;
                }

                dispatch_hook(
                    hook_registry,
                    HookEvent::FanOutSpawn {
                        topology_run_id: topology_run_id.to_string(),
                        source_node_execution_id: completed.node_execution_id.to_string(),
                        edge_id: edge.id.clone(),
                        branch_count: items.len() as u32,
                    },
                )
                .await?;

                for item in items {
                    state
                        .storage
                        .create_edge_evaluation(NewEdgeEvaluation {
                            topology_run_id,
                            source_node_execution_id: completed.node_execution_id,
                            edge_id: edge.id.clone(),
                            target_node_id: edge.target_node_id.clone(),
                            condition_expression: Some(format!("fan_out {items_field}")),
                            matched: true,
                            payload_json: Some(item.clone()),
                        })
                        .await?;
                    activations.push(PendingActivation {
                        node_id: edge.target_node_id.clone(),
                        input_json: item.clone(),
                    });
                }

                dispatch_hook(
                    hook_registry,
                    HookEvent::FanOutCollect {
                        topology_run_id: topology_run_id.to_string(),
                        source_node_execution_id: completed.node_execution_id.to_string(),
                        edge_id: edge.id.clone(),
                        branch_count: items.len() as u32,
                    },
                )
                .await?;
            }
        }
    }

    Ok(activations)
}

fn extract_field_value<'a>(
    value: &'a serde_json::Value,
    field_path: &str,
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in field_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

async fn dispatch_hook(
    hook_registry: Option<&Arc<HookRegistry>>,
    event: HookEvent,
) -> Result<(), TopologyEngineError> {
    let Some(hook_registry) = hook_registry else {
        return Ok(());
    };
    let event_name = event.event_name().to_string();
    match hook_registry.dispatch(event).await {
        HookOutcome::Continue => Ok(()),
        HookOutcome::ContinueModified(modified) => {
            tracing::warn!(
                original_event = event_name,
                modified_event = modified.event_name(),
                "topology hooks returned a modified event that the executor does not apply"
            );
            Ok(())
        }
        HookOutcome::Block(reason) => Err(TopologyEngineError::HookBlocked {
            event: event_name,
            reason,
        }),
        HookOutcome::RequestApproval { prompt, .. } => {
            Err(TopologyEngineError::HookApprovalRequested {
                event: event_name,
                prompt,
            })
        }
    }
}

fn broadcast_topology_event(state: &AppState, event: ServerEvent) {
    let _ = state.broadcast.send(event);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use async_trait::async_trait;
    use swage_hook::{Hook, HookOutcome, HookRegistration, HookRegistry};
    use swage_storage::model::{NewBench, NewTeam};
    use swage_storage::store::BenchStore;
    use swage_storage::store::TeamStore;

    use super::*;
    use crate::lifecycle::ActivityTracker;
    use crate::turn::tests::{MockProvider, MockStorage, MultiRoundMockProvider};

    fn team_agents(ids: &[&str]) -> Vec<swage_storage::model::TeamAgentDefinition> {
        ids.iter()
            .map(|id| swage_storage::model::TeamAgentDefinition {
                id: (*id).to_string(),
                display_name: (*id).to_string(),
                description: String::new(),
                instructions: String::new(),
                model: None,
            })
            .collect()
    }

    fn team_tasks(names: &[&str]) -> Vec<swage_storage::model::TeamTaskDefinition> {
        names
            .iter()
            .map(|name| swage_storage::model::TeamTaskDefinition {
                name: (*name).to_string(),
                description: String::new(),
                instructions: format!("Task {name}"),
                model: None,
                output_schema: None,
                max_output_retries: None,
                tool_allow: None,
                tool_deny: None,
            })
            .collect()
    }

    fn make_state(provider: Arc<dyn swage_provider::LlmProvider>) -> (AppState, Arc<MockStorage>) {
        let storage = Arc::new(MockStorage::new());
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            bench_store: Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
            provider,
            voice_provider: Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                "voice is unavailable for tests",
            )) as Arc<dyn swage_provider::VoiceProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            hook_registry: None,
            activity_tracker: Arc::new(ActivityTracker::new()),
            broadcast,
            shutdown_signal: CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            presence: Arc::new(Mutex::new(crate::presence::PresenceRuntime::default())),
            execution_hosts: Arc::new(Mutex::new(
                crate::execution_host::ExecutionHostRuntime::default(),
            )),
            pairing_codes: Arc::new(Mutex::new(HashMap::new())),
            secret_manager: Arc::new(crate::secret::SecretManager::new(
                Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
            )),
            workflow_capabilities: crate::capabilities::WorkflowCapabilities::default(),
        };
        (state, storage)
    }

    fn schema(properties: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": properties,
        })
    }

    struct RecordingHook {
        events: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Hook for RecordingHook {
        fn name(&self) -> &str {
            "recording-topology-hook"
        }

        async fn execute(&self, event: &HookEvent) -> HookOutcome {
            self.events
                .lock()
                .unwrap()
                .push(event.event_name().to_string());
            HookOutcome::Continue
        }
    }

    struct BlockOnEventHook {
        event_name: &'static str,
    }

    #[async_trait]
    impl Hook for BlockOnEventHook {
        fn name(&self) -> &str {
            "block-topology-event"
        }

        async fn execute(&self, event: &HookEvent) -> HookOutcome {
            if event.event_name() == self.event_name {
                HookOutcome::Block(format!("blocked {}", self.event_name))
            } else {
                HookOutcome::Continue
            }
        }
    }

    struct ApprovalOnEventHook {
        event_name: &'static str,
    }

    #[async_trait]
    impl Hook for ApprovalOnEventHook {
        fn name(&self) -> &str {
            "approve-topology-event"
        }

        async fn execute(&self, event: &HookEvent) -> HookOutcome {
            if event.event_name() == self.event_name {
                HookOutcome::RequestApproval {
                    prompt: format!("approve {}", self.event_name),
                    context: None,
                    timeout: None,
                }
            } else {
                HookOutcome::Continue
            }
        }
    }

    #[tokio::test]
    async fn executes_conditional_topology_and_persists_trace() {
        let provider = Arc::new(MultiRoundMockProvider::new(vec![
            vec![
                swage_core::StreamChunk::Delta {
                    content: "{\"decision\":\"review\",\"draft\":\"hello\"}".into(),
                },
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 10,
                    output_tokens: 6,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: Some(swage_core::FinishReason::Stop),
                },
            ],
            vec![
                swage_core::StreamChunk::Delta {
                    content: "{\"result\":\"approved\"}".into(),
                },
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 8,
                    output_tokens: 4,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: Some(swage_core::FinishReason::Stop),
                },
            ],
        ]));
        let (state, storage) = make_state(provider);
        let bench = BenchStore::create(
            storage.as_ref(),
            NewBench {
                name: "topology-bench".into(),
                slug: "topology-bench".into(),
            },
        )
        .await
        .unwrap();
        let team = storage
            .create_team(NewTeam {
                bench_id: bench.id,
                name: "analysis".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner", "reviewer"]),
                tasks: team_tasks(&["draft-plan", "review-plan"]),
            })
            .await
            .unwrap();
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: swage_topology::TopologyBudget {
                max_fan_out: Some(4),
                max_concurrency: Some(2),
                max_total_tokens: Some(100),
            },
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm,
                    agent_id: Some("planner".into()),
                    task_name: Some("draft-plan".into()),
                    handler_name: None,
                    output_schema: Some(schema(serde_json::json!({
                        "decision": {"type": "string"},
                        "draft": {"type": "string"}
                    }))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm,
                    agent_id: Some("reviewer".into()),
                    task_name: Some("review-plan".into()),
                    handler_name: None,
                    output_schema: Some(schema(serde_json::json!({
                        "result": {"type": "string"}
                    }))),
                    iteration_limit: None,
                },
            ],
            edges: vec![swage_topology::TopologyEdge {
                id: "plan-to-review".into(),
                source_node_id: "plan".into(),
                target_node_id: "review".into(),
                routing: EdgeRouting::Condition {
                    field_path: "decision".into(),
                    equals: serde_json::json!("review"),
                },
            }],
        };

        let executor = TopologyExecutor::new(state.clone());
        let result = executor
            .execute(TopologyExecutionRequest {
                team: team.clone(),
                graph,
                initial_input: serde_json::json!({"message": "hello"}),
                session_id: None,
                source_turn_id: None,
            })
            .await
            .unwrap();

        assert_eq!(
            result.final_output,
            serde_json::json!({"result": "approved"})
        );
        let runs = state
            .storage
            .list_topology_runs_by_team(team.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, TopologyRunState::Completed);
        let executions = state
            .storage
            .list_node_executions_by_run(result.topology_run_id)
            .await
            .unwrap();
        assert_eq!(executions.len(), 2);
        let edges = state
            .storage
            .list_edge_evaluations_by_run(result.topology_run_id)
            .await
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert!(edges[0].matched);
    }

    #[tokio::test]
    async fn fan_out_limit_failure_marks_run_failed() {
        let provider = Arc::new(MockProvider::new(vec![
            swage_core::StreamChunk::Delta {
                content: "{\"items\":[1,2,3]}".into(),
            },
            swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: 5,
                output_tokens: 5,
            }),
            swage_core::StreamChunk::Done {
                finish_reason: Some(swage_core::FinishReason::Stop),
            },
        ]));
        let (state, storage) = make_state(provider);
        let bench = BenchStore::create(
            storage.as_ref(),
            NewBench {
                name: "fanout-bench".into(),
                slug: "fanout-bench".into(),
            },
        )
        .await
        .unwrap();
        let team = storage
            .create_team(NewTeam {
                bench_id: bench.id,
                name: "fanout-team".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner", "worker"]),
                tasks: team_tasks(&["draft-plan", "review-plan"]),
            })
            .await
            .unwrap();
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: swage_topology::TopologyBudget {
                max_fan_out: Some(2),
                max_concurrency: Some(2),
                max_total_tokens: Some(100),
            },
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm,
                    agent_id: Some("planner".into()),
                    task_name: Some("draft-plan".into()),
                    handler_name: None,
                    output_schema: Some(schema(serde_json::json!({
                        "items": {"type": "array"}
                    }))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "worker".into(),
                    kind: TopologyNodeKind::Llm,
                    agent_id: Some("worker".into()),
                    task_name: Some("review-plan".into()),
                    handler_name: None,
                    output_schema: Some(schema(serde_json::json!({
                        "result": {"type": "string"}
                    }))),
                    iteration_limit: None,
                },
            ],
            edges: vec![swage_topology::TopologyEdge {
                id: "fan".into(),
                source_node_id: "plan".into(),
                target_node_id: "worker".into(),
                routing: EdgeRouting::FanOut {
                    items_field: "items".into(),
                },
            }],
        };

        let executor = TopologyExecutor::new(state.clone());
        let error = executor
            .execute(TopologyExecutionRequest {
                team: team.clone(),
                graph,
                initial_input: serde_json::json!({"message": "hello"}),
                session_id: None,
                source_turn_id: None,
            })
            .await
            .expect_err("fan-out should fail");

        assert!(error.to_string().contains("fan-out"));
        let runs = state
            .storage
            .list_topology_runs_by_team(team.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, TopologyRunState::Failed);
    }

    #[tokio::test]
    async fn iteration_limit_failure_marks_run_failed_and_skips_later_pending_work() {
        let provider = Arc::new(MockProvider::new(vec![swage_core::StreamChunk::Done {
            finish_reason: Some(swage_core::FinishReason::Stop),
        }]));
        let (state, storage) = make_state(provider);
        let bench = BenchStore::create(
            storage.as_ref(),
            NewBench {
                name: "iteration-limit-bench".into(),
                slug: "iteration-limit-bench".into(),
            },
        )
        .await
        .unwrap();
        let team = storage
            .create_team(NewTeam {
                bench_id: bench.id,
                name: "iteration-limit-team".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: vec![],
            })
            .await
            .unwrap();
        let graph = TopologyGraph {
            entry_node_id: "loop".into(),
            budget: swage_topology::TopologyBudget {
                max_fan_out: Some(2),
                max_concurrency: Some(1),
                max_total_tokens: Some(100),
            },
            nodes: vec![
                TopologyNode {
                    id: "loop".into(),
                    kind: TopologyNodeKind::Deterministic,
                    agent_id: None,
                    task_name: None,
                    handler_name: Some("identity".into()),
                    output_schema: None,
                    iteration_limit: Some(1),
                },
                TopologyNode {
                    id: "later".into(),
                    kind: TopologyNodeKind::Deterministic,
                    agent_id: None,
                    task_name: None,
                    handler_name: Some("identity".into()),
                    output_schema: None,
                    iteration_limit: None,
                },
            ],
            edges: vec![
                swage_topology::TopologyEdge {
                    id: "loop-again".into(),
                    source_node_id: "loop".into(),
                    target_node_id: "loop".into(),
                    routing: EdgeRouting::Unconditional,
                },
                swage_topology::TopologyEdge {
                    id: "loop-to-later".into(),
                    source_node_id: "loop".into(),
                    target_node_id: "later".into(),
                    routing: EdgeRouting::Unconditional,
                },
            ],
        };

        let executor = TopologyExecutor::new(state.clone());
        let error = executor
            .execute(TopologyExecutionRequest {
                team: team.clone(),
                graph,
                initial_input: serde_json::json!({"message": "hello"}),
                session_id: None,
                source_turn_id: None,
            })
            .await
            .expect_err("the second loop activation should exceed the iteration limit");

        assert!(error.to_string().contains("exceeded iteration limit"));
        let runs = state
            .storage
            .list_topology_runs_by_team(team.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, TopologyRunState::Failed);
        assert!(runs[0].final_output.is_none());
        assert!(
            runs[0]
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("exceeded iteration limit"))
        );

        let executions = state
            .storage
            .list_node_executions_by_run(runs[0].id)
            .await
            .unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].node_id, "loop");
        assert_eq!(executions[0].state, NodeExecutionState::Completed);
        assert!(
            executions
                .iter()
                .all(|execution| execution.node_id != "later"),
            "pending work queued after the cyclic activation must not execute"
        );
    }

    #[tokio::test]
    async fn token_budget_failure_marks_run_failed_without_final_output() {
        let provider = Arc::new(MultiRoundMockProvider::new(vec![
            vec![
                swage_core::StreamChunk::Delta {
                    content: "{\"decision\":\"review\"}".into(),
                },
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 4,
                    output_tokens: 2,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: Some(swage_core::FinishReason::Stop),
                },
            ],
            vec![
                swage_core::StreamChunk::Delta {
                    content: "{\"result\":\"approved\"}".into(),
                },
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 4,
                    output_tokens: 2,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: Some(swage_core::FinishReason::Stop),
                },
            ],
        ]));
        let (state, storage) = make_state(provider);
        let bench = BenchStore::create(
            storage.as_ref(),
            NewBench {
                name: "token-budget-bench".into(),
                slug: "token-budget-bench".into(),
            },
        )
        .await
        .unwrap();
        let team = storage
            .create_team(NewTeam {
                bench_id: bench.id,
                name: "token-budget-team".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner", "reviewer"]),
                tasks: team_tasks(&["draft-plan", "review-plan"]),
            })
            .await
            .unwrap();
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: swage_topology::TopologyBudget {
                max_fan_out: Some(1),
                max_concurrency: Some(1),
                max_total_tokens: Some(10),
            },
            nodes: vec![
                TopologyNode {
                    id: "plan".into(),
                    kind: TopologyNodeKind::Llm,
                    agent_id: Some("planner".into()),
                    task_name: Some("draft-plan".into()),
                    handler_name: None,
                    output_schema: Some(schema(serde_json::json!({
                        "decision": {"type": "string"}
                    }))),
                    iteration_limit: None,
                },
                TopologyNode {
                    id: "review".into(),
                    kind: TopologyNodeKind::Llm,
                    agent_id: Some("reviewer".into()),
                    task_name: Some("review-plan".into()),
                    handler_name: None,
                    output_schema: Some(schema(serde_json::json!({
                        "result": {"type": "string"}
                    }))),
                    iteration_limit: None,
                },
            ],
            edges: vec![swage_topology::TopologyEdge {
                id: "plan-to-review".into(),
                source_node_id: "plan".into(),
                target_node_id: "review".into(),
                routing: EdgeRouting::Condition {
                    field_path: "decision".into(),
                    equals: serde_json::json!("review"),
                },
            }],
        };

        let executor = TopologyExecutor::new(state.clone());
        let error = executor
            .execute(TopologyExecutionRequest {
                team: team.clone(),
                graph,
                initial_input: serde_json::json!({"message": "hello"}),
                session_id: None,
                source_turn_id: None,
            })
            .await
            .expect_err("the second node should push the run over the token budget");

        assert!(error.to_string().contains("token budget exceeded"));
        let runs = state
            .storage
            .list_topology_runs_by_team(team.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, TopologyRunState::Failed);
        assert!(runs[0].final_output.is_none());
        assert!(
            runs[0]
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("token budget exceeded"))
        );

        let executions = state
            .storage
            .list_node_executions_by_run(runs[0].id)
            .await
            .unwrap();
        assert_eq!(executions.len(), 2);
        assert_eq!(
            executions
                .iter()
                .map(|execution| (&execution.node_id, &execution.state))
                .collect::<Vec<_>>(),
            vec![
                (&"plan".to_string(), &NodeExecutionState::Completed),
                (&"review".to_string(), &NodeExecutionState::Completed),
            ]
        );
    }

    #[tokio::test]
    async fn topology_lifecycle_hooks_fire_for_run_and_nodes() {
        let provider = Arc::new(MultiRoundMockProvider::new(vec![vec![
            swage_core::StreamChunk::Delta {
                content: "{\"result\":\"ok\"}".into(),
            },
            swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
            }),
            swage_core::StreamChunk::Done {
                finish_reason: Some(swage_core::FinishReason::Stop),
            },
        ]]));
        let (state, storage) = make_state(provider);
        let bench = BenchStore::create(
            storage.as_ref(),
            NewBench {
                name: "hook-bench".into(),
                slug: "hook-bench".into(),
            },
        )
        .await
        .unwrap();
        let team = storage
            .create_team(NewTeam {
                bench_id: bench.id,
                name: "hook-team".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: team_tasks(&["draft-plan"]),
            })
            .await
            .unwrap();
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: swage_topology::TopologyBudget {
                max_fan_out: Some(1),
                max_concurrency: Some(1),
                max_total_tokens: Some(20),
            },
            nodes: vec![TopologyNode {
                id: "plan".into(),
                kind: TopologyNodeKind::Llm,
                agent_id: Some("planner".into()),
                task_name: Some("draft-plan".into()),
                handler_name: None,
                output_schema: Some(schema(serde_json::json!({
                    "result": {"type": "string"}
                }))),
                iteration_limit: None,
            }],
            edges: vec![],
        };

        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(HookRegistration::new(Arc::new(RecordingHook {
            events: recorded.clone(),
        })));

        let executor =
            TopologyExecutor::new(state.clone()).with_hook_registry(Arc::new(hook_registry));
        executor
            .execute(TopologyExecutionRequest {
                team,
                graph,
                initial_input: serde_json::json!({"message": "hello"}),
                session_id: None,
                source_turn_id: None,
            })
            .await
            .unwrap();

        let events = recorded.lock().unwrap().clone();
        assert!(events.contains(&"TopologyStart".to_string()));
        assert!(events.contains(&"NodeStart".to_string()));
        assert!(events.contains(&"NodeEnd".to_string()));
        assert!(events.contains(&"TopologyEnd".to_string()));
    }

    #[tokio::test]
    async fn blocking_topology_start_hook_fails_the_run() {
        let provider = Arc::new(MockProvider::new(vec![swage_core::StreamChunk::Done {
            finish_reason: Some(swage_core::FinishReason::Stop),
        }]));
        let (state, storage) = make_state(provider);
        let bench = BenchStore::create(
            storage.as_ref(),
            NewBench {
                name: "blocked-topology-bench".into(),
                slug: "blocked-topology-bench".into(),
            },
        )
        .await
        .unwrap();
        let team = storage
            .create_team(NewTeam {
                bench_id: bench.id,
                name: "hook-team".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: team_tasks(&["draft-plan"]),
            })
            .await
            .unwrap();
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: swage_topology::TopologyBudget {
                max_fan_out: Some(1),
                max_concurrency: Some(1),
                max_total_tokens: Some(20),
            },
            nodes: vec![TopologyNode {
                id: "plan".into(),
                kind: TopologyNodeKind::Llm,
                agent_id: Some("planner".into()),
                task_name: Some("draft-plan".into()),
                handler_name: None,
                output_schema: Some(schema(serde_json::json!({
                    "result": {"type": "string"}
                }))),
                iteration_limit: None,
            }],
            edges: vec![],
        };

        let mut hook_registry = HookRegistry::new();
        hook_registry.register(HookRegistration::new(Arc::new(BlockOnEventHook {
            event_name: "TopologyStart",
        })));

        let executor =
            TopologyExecutor::new(state.clone()).with_hook_registry(Arc::new(hook_registry));
        let error = executor
            .execute(TopologyExecutionRequest {
                team: team.clone(),
                graph,
                initial_input: serde_json::json!({"message": "hello"}),
                session_id: None,
                source_turn_id: None,
            })
            .await
            .expect_err("TopologyStart block should fail the run");

        assert!(matches!(error, TopologyEngineError::HookBlocked { .. }));
        let runs = state
            .storage
            .list_topology_runs_by_team(team.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, TopologyRunState::Failed);
    }

    #[tokio::test]
    async fn approval_requesting_node_start_hook_fails_the_run() {
        let provider = Arc::new(MockProvider::new(vec![
            swage_core::StreamChunk::Delta {
                content: "{\"result\":\"ok\"}".into(),
            },
            swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
            }),
            swage_core::StreamChunk::Done {
                finish_reason: Some(swage_core::FinishReason::Stop),
            },
        ]));
        let (state, storage) = make_state(provider);
        let bench = BenchStore::create(
            storage.as_ref(),
            NewBench {
                name: "approval-topology-bench".into(),
                slug: "approval-topology-bench".into(),
            },
        )
        .await
        .unwrap();
        let team = storage
            .create_team(NewTeam {
                bench_id: bench.id,
                name: "hook-team".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: team_tasks(&["draft-plan"]),
            })
            .await
            .unwrap();
        let graph = TopologyGraph {
            entry_node_id: "plan".into(),
            budget: swage_topology::TopologyBudget {
                max_fan_out: Some(1),
                max_concurrency: Some(1),
                max_total_tokens: Some(20),
            },
            nodes: vec![TopologyNode {
                id: "plan".into(),
                kind: TopologyNodeKind::Llm,
                agent_id: Some("planner".into()),
                task_name: Some("draft-plan".into()),
                handler_name: None,
                output_schema: Some(schema(serde_json::json!({
                    "result": {"type": "string"}
                }))),
                iteration_limit: None,
            }],
            edges: vec![],
        };

        let mut hook_registry = HookRegistry::new();
        hook_registry.register(HookRegistration::new(Arc::new(ApprovalOnEventHook {
            event_name: "NodeStart",
        })));

        let executor =
            TopologyExecutor::new(state.clone()).with_hook_registry(Arc::new(hook_registry));
        let error = executor
            .execute(TopologyExecutionRequest {
                team: team.clone(),
                graph,
                initial_input: serde_json::json!({"message": "hello"}),
                session_id: None,
                source_turn_id: None,
            })
            .await
            .expect_err("NodeStart approval request should fail the run");

        assert!(matches!(
            error,
            TopologyEngineError::InvalidGraph(ref message)
                if message.contains("topology hook requested approval")
        ));
        let runs = state
            .storage
            .list_topology_runs_by_team(team.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, TopologyRunState::Failed);
    }
}
