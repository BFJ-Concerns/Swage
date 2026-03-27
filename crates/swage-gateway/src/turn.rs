use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_core::{ChatMessage, GatewayError, Role};
use swage_storage::model::{NewMessage, TurnState};

use swage_context::element::ContextElement;
use swage_context::history::HistoryElement;
use swage_context::memory::MemoryElement;
use swage_context::prompts::PromptsElement;
use swage_context::skills::{ActiveSkill, SkillsElement};
use swage_context::tokeniser::TiktokenTokeniser;
use swage_config::{HierarchyLevel, prompt_rule::PromptRule};

use swage_agent::{AgentBuilder, AgentConfig, AgentError, AgentEvent, TurnRequest};
use swage_tool::dispatch::ToolDispatcher;
use swage_tool::permission::RequireApprovalEntry as ToolRequireApprovalEntry;
use swage_tool::permission::PermissionConfig;
use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};
use swage_safety::LeakDetector;

use crate::skill::SkillManager;
use crate::tool_executor::{GatewayExecutionStrategy, SandboxedExecutor};

use crate::{
    state::AppState,
    subscription::SubscriptionState,
    websocket::ServerEvent,
};


// Gateway messaging integration: REST endpoints (POST /api/messages, POST /api/turns,
// GET /api/approvals, etc.) provide the programmatic interface. The MessageRouter,
// DelegationExecutor, and expiry are wired through the handler layer. Delegation
// interception in the streaming turn loop (agent-initiated delegation during tool
// dispatch) requires era 0.4's tool-as-delegation abstraction.
// (deferred to Phase 05). All messaging happens within the agent execution.
// No new WebSocket events in this phase (deferred to Phase 05).
// Success: delegation works end-to-end through the gateway, expired messages
// are cleaned up, single-agent conversations are unaffected.
// ---

/// Build the context assembly elements for a turn.
///
/// Constructs typed elements from the resolved turn inputs:
/// - `PromptsElement` — from the merged instruction string
/// - `SkillsElement`  — from the active skill set
/// - `MemoryElement`  — from pre-loaded project memory content (may be empty)
/// - `HistoryElement` — from the thread's message history
///
/// The returned vec is suitable for passing to `swage_context::assembly::assemble()`.
///
/// When instructions are empty the `PromptsElement` produces no messages.
/// When `memory_content` is empty the `MemoryElement` produces no messages
/// but is still present in the vec so custom assemblies can reference it.
fn build_assembly_elements(
    instructions: &str,
    active_skills: Vec<ActiveSkill>,
    history: Vec<ChatMessage>,
    memory_content: String,
    project_name: String,
) -> Vec<Box<dyn ContextElement>> {
    // Construct a single PromptRule from the resolved instructions string.
    // The config resolver has already merged hierarchy levels and assembled
    // the text — we wrap it as a single Global-level rule so the PromptsElement
    // produces the expected system message.
    let rules = if instructions.is_empty() {
        vec![]
    } else {
        vec![PromptRule {
            priority: 0,
            content: instructions.to_string(),
            source_level: HierarchyLevel::Global,
            file_path: PathBuf::from("resolved-instructions"),
        }]
    };

    vec![
        Box::new(PromptsElement::new(rules)),
        Box::new(SkillsElement::new(active_skills)),
        Box::new(MemoryElement::new(memory_content, project_name)),
        Box::new(HistoryElement::new(history)),
    ]
}

/// Load project memory content by reading all `.md` files from the project's
/// memory directory (`{swage_root}/projects/{project_slug}/memory/`).
///
/// Files are read in alphabetical filename order and joined with a blank line
/// between entries. Returns an empty string when the directory is absent or
/// contains no `.md` files, so callers receive a valid `String` in all cases.
fn load_project_memory(swage_root: &Path, project_slug: &str) -> String {
    let memory_dir = swage_root
        .join("projects")
        .join(project_slug)
        .join("memory");

    // TODO Synchronous filesystem reads in `load_project_memory` block async executor
    // priority: normal | tags: performance, review:branch-era-02-delivery
    // `std::fs::read_dir` and `std::fs::read_to_string` are synchronous and block the
    // Tokio worker thread for the duration of each disk operation. `load_project_memory`
    // is called inside the async `process_turn` path on every turn. For projects with
    // many or large memory files this degrades gateway responsiveness under load.
    // Fix: replace with `tokio::fs::read_dir` / `tokio::fs::read_to_string` or wrap
    // in `tokio::task::spawn_blocking`.
    // ---
    let entries = match std::fs::read_dir(&memory_dir) {
        Ok(rd) => rd,
        // Missing directory is the common case for new projects.
        Err(_) => return String::new(),
    };

    // Collect .md paths and sort alphabetically so loading order is stable.
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    paths.sort();

    let parts: Vec<String> = paths
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter(|s| !s.trim().is_empty())
        .collect();

    parts.join("\n\n")
}

/// Orchestrate a complete agent turn: user message → provider stream → storage.
///
/// # Flow
/// 1. Look up the session's default thread (created alongside the session).
/// 2. Guard: reject if a turn is already Processing for this thread.
/// 3. Resolve the project for the session; load per-project config and build ToolContext.
/// 4. Create a Turn record (state=Pending), immediately advance to Processing.
/// 5. Store the user message with thread_id and position.
/// 6. Build message history from the thread and send a streaming request to the provider.
/// 7. For each chunk: forward Token events to the WebSocket, accumulate content.
/// 8. On Done: store the assistant message, mark the turn Completed, send TurnComplete.
/// 9. On Cancel: store any partial response, mark the turn Interrupted, send Cancelled.
/// 10. On provider Error: mark the turn Failed, send an Error event.
/// 11. Cleanup: remove the CancellationToken from active_turns.
// Because process_turn is spawned as a tokio task, it won't inherit the
// HTTP request span. The instrument macro creates a self-contained "turn"
// span so all log entries within the turn share the same turn_id and
// session_id. turn_id is Empty at span creation and recorded once the
// Turn record exists.
#[tracing::instrument(
    name = "turn",
    skip(state, content, ws_tx, subscription),
    fields(turn_id = tracing::field::Empty),
)]
pub async fn process_turn(
    state: &AppState,
    session_id: Uuid,
    content: String,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
) -> Result<(), GatewayError> {
    // Step 1: Resolve the session's thread. The gateway creates one thread per
    // session in create_conversation / create_session_in_project.
    let threads = state.storage.list_by_session(session_id).await?;
    let thread = threads
        .into_iter()
        .next()
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let thread_id = thread.id;

    // Step 2: Guard — reject if a turn is already Processing for this thread.
    let existing_turns = state.storage.list_turns_by_thread(thread_id).await?;
    if existing_turns
        .iter()
        .any(|t| t.state == TurnState::Processing)
    {
        return Err(GatewayError::TurnInProgress { session_id });
    }

    // Step 3: Resolve the project for this session so we can load per-project config
    // and build the ToolContext passed to each tool invocation.
    let session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let project_id = session.project_id;

    let project = state
        .project_store
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
    let project_slug = project.slug.clone();

    // Load project-specific config (model, instructions, etc.). Falls back to
    // the startup config when no project-level config.toml exists, which is
    // the normal case for most projects.
    let config = match swage_config::load_resolved_config(&project_slug, "default") {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("failed to load project config for '{project_slug}', using startup config: {e}");
            state.config.clone()
        }
    };

    // Resolve the swage root once — skill discovery, project memory loading,
    // and assembly search-path construction all depend on it.
    //
    // Fail-open: a warning is logged and each consumer falls back gracefully.
    let swage_root_opt: Option<PathBuf> = match swage_config::discovery::resolve_root() {
        Ok(root) => Some(root),
        Err(e) => {
            tracing::warn!(
                error = %e.kind,
                "failed to resolve swage root; proceeding without skills, memory, or custom assembly"
            );
            None
        }
    };

    // Discover available skills for this project+agent context, then build the
    // active skill set by matching stored activation records against definitions.
    //
    // Missing skill definitions for recorded activations are silently skipped
    // by SkillManager::build_active_skills.
    let active_skills: Vec<ActiveSkill> = if let Some(root) = &swage_root_opt {
        let project_dir = root.join("projects").join(&project_slug);
        // The session's agent_id (typically "gateway") maps to the agents
        // subdirectory under the project.
        let agent_dir = project_dir.join("agents").join(&session.agent_id);
        let agent_dir_opt = if agent_dir.is_dir() { Some(agent_dir.as_path()) } else { None };
        let (available, _warnings) =
            SkillManager::discover_available(root, &project_dir, agent_dir_opt);

        // Ensure agent-level skills are activated for this session.
        // Idempotent — INSERT OR IGNORE in storage means repeated calls
        // on subsequent turns are harmless.
        if let Err(e) = SkillManager::auto_activate_agent_skills(
            session_id,
            &available,
            state.storage.as_ref(),
        )
        .await
        {
            tracing::warn!(
                error = ?e,
                "failed to auto-activate agent-level skills; proceeding"
            );
        }

        match SkillManager::build_active_skills(
            session_id,
            &available,
            state.storage.as_ref(),
        )
        .await
        {
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "failed to load active skills; proceeding without skills"
                );
                vec![]
            }
            Ok(infos) => infos
                .into_iter()
                .map(|info| ActiveSkill {
                    name: info.name,
                    instructions: info.instructions,
                    level: info.level,
                    is_agent_level: info.is_agent_level,
                    activation_order: info.activation_order as u32,
                })
                .collect(),
        }
    } else {
        vec![]
    };

    // Step 4: Set up the cancellation token before any turn processing begins.
    let cancel_token = CancellationToken::new();
    {
        let mut turns = state.active_turns.lock().await;
        turns.insert(session_id, cancel_token.clone());
    }

    // Step 5: Store the user message.
    // Position is the current count of messages in the thread.
    let user_position = state
        .storage
        .list_messages_by_thread(thread_id)
        .await
        .map(|msgs| msgs.len() as i32)
        .unwrap_or_else(|e| {
            tracing::warn!("failed to read message count for position, defaulting to 0: {e}");
            0
        });

    state
        .storage
        .create_message(NewMessage {
            thread_id,
            role: Role::User.to_string(),
            content: content.clone(),
            position: user_position,
            agent_id: "gateway".to_string(),
        })
        .await?;

    // Steps 6–7: Assemble context via the swage-context assembly engine.
    // Loads thread history, project memory, and constructs typed elements.
    // Runs fail-open assembly: custom Python configs when configured, built-in
    // default otherwise. The LLM provider is passed for two-phase compaction.
    let stored_history = state.storage.list_messages_by_thread(thread_id).await?;
    let history_messages: Vec<ChatMessage> = stored_history
        .into_iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            };
            ChatMessage::new(role, m.content)
        })
        .collect();

    // Load project memory from the filesystem. Returns an empty string when
    // no memory directory or files exist — MemoryElement handles empty content.
    let memory_content = swage_root_opt
        .as_deref()
        .map(|root| load_project_memory(root, &project_slug))
        .unwrap_or_default();

    // Build the assembly search hierarchy for locating Python assembly files.
    // Collected as owned PathBufs first so references can be formed safely
    // after the Vec is complete.
    let assembly_search_paths: Vec<(HierarchyLevel, PathBuf)> = swage_root_opt
        .as_ref()
        .map(|root| {
            let project_dir = root.join("projects").join(&project_slug);
            let agent_dir = project_dir.join("agents").join(&session.agent_id);
            let mut levels = vec![
                (HierarchyLevel::Global, root.clone()),
                (HierarchyLevel::Project, project_dir),
            ];
            // Only include the agent level when the directory exists;
            // a missing agent dir means no agent-specific assemblies.
            if agent_dir.is_dir() {
                levels.push((HierarchyLevel::Agent, agent_dir));
            }
            levels
        })
        .unwrap_or_default();
    let search_level_refs: Vec<(HierarchyLevel, &Path)> = assembly_search_paths
        .iter()
        .map(|(level, path)| (level.clone(), path.as_path()))
        .collect();

    let elements = build_assembly_elements(
        &config.instructions,
        active_skills,
        history_messages,
        memory_content,
        project.name.clone(),
    );
    let tokeniser = TiktokenTokeniser::new(&config.provider.model);

    // Run the fail-open assembly orchestrator. For "default" (or empty) assembly
    // names this delegates to default_assembly() synchronously. For any other
    // name it locates the corresponding .py file and executes it via PyO3.
    // On timeout or execution failure it falls back to default_assembly() and
    // records the error in the assembly report.
    let output = swage_context::assembly::assemble(
        &elements,
        &config.context,
        &tokeniser,
        &search_level_refs,
        state.provider.as_ref(),
        Some(&swage_python::ConcreteAssemblyRunner),
    )
    .await;
    let (assembled_messages, assembly_report) = (output.messages, output.report);

    tracing::debug!(
        total_tokens = assembly_report.total_tokens,
        elements = assembly_report.elements.len(),
        "context assembled"
    );

    // Serialise the assembly report for deferred emission.
    // The ContextAssembled event and snapshot persistence require a turn_id,
    // which is not available until the agent creates the Turn record and emits
    // TurnStarted. We prepare the report now and emit it in the event loop.
    let report_json = serde_json::to_value(&assembly_report)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to serialise assembly report");
            serde_json::json!({ "error": "serialisation failed" })
        });

    // Step 7: Resolve tool permissions from config cascade and build the
    // ToolDispatcher with the resolved permission set. When a sandbox is
    // available, inject SandboxedExecutor as the execution strategy so
    // tool calls are routed through bubblewrap transparently.
    //
    // Merge deny entries from the startup config (state.config) into the
    // per-turn permissions. The startup config represents the operator's
    // global constraints and may not exist in disk-loaded project configs.
    // Deny entries are additive — a tool denied by the startup config stays
    // denied regardless of what the project config allows.
    let mut deny = config.tools.permissions.deny.clone();
    for entry in &state.config.tools.permissions.deny {
        if !deny.contains(entry) {
            deny.push(entry.clone());
        }
    }
    let permissions = PermissionConfig {
        allow: config.tools.permissions.allow.clone(),
        deny,
        bundles: config.tools.permissions.bundles.clone(),
        require_approval: config.tools.permissions.require_approval
            .iter()
            .map(|entry| match entry {
                swage_config::model::RequireApprovalEntry::Simple(tool_name) => {
                    ToolRequireApprovalEntry {
                        tool_name: tool_name.clone(),
                        timeout_secs: None,
                        timeout_policy: None,
                    }
                }
                swage_config::model::RequireApprovalEntry::WithOptions { tool, timeout, timeout_policy } => {
                    ToolRequireApprovalEntry {
                        tool_name: tool.clone(),
                        timeout_secs: *timeout,
                        timeout_policy: timeout_policy.clone(),
                    }
                }
            })
            .collect(),
    };

    // Clone the existing registry from the startup dispatcher — the tools
    // themselves don't change, only the permission rules.
    let dispatcher = ToolDispatcher::new(
        state.tool_dispatcher.registry().clone(),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );
    // Build the composite execution strategy. GatewayExecutionStrategy scopes
    // PYTHON_CONTEXT for python_execute calls, and optionally wraps the
    // SandboxedExecutor for sandboxed tool execution.
    let inner_strategy: Option<Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>> =
        state.sandbox.as_ref().map(|sandbox| {
            Arc::new(SandboxedExecutor::new(sandbox.clone()))
                as Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>
        });
    let gateway_strategy = GatewayExecutionStrategy::new(
        inner_strategy,
        ws_tx.clone(),
    );
    let dispatcher = dispatcher.with_execution_strategy(Arc::new(gateway_strategy));

    // Step 8: Build the Agent that will execute the turn.
    // Wire output schema from config: if a schema is configured, pass it to the
    // agent so it validates the LLM response and retries on failure.
    let mut agent_config = AgentConfig::new(session.agent_id.clone());
    if let Some(schema) = config.output.schema.clone() {
        agent_config = agent_config.with_output_schema(schema, config.output.max_retries);
    }
    let agent = match AgentBuilder::new()
        .config(agent_config)
        .provider(state.provider.clone())
        .storage(state.storage.clone())
        .tool_dispatcher(dispatcher)
        .build()
    {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "failed to build agent for turn");
            let _ = ws_tx.send(ServerEvent::Error {
                code: "agent_construction_error".to_string(),
                message: e.to_string(),
            });
            // Cleanup cancellation token before returning.
            let mut turns = state.active_turns.lock().await;
            turns.remove(&session_id);
            return Err(GatewayError::WebSocketError(format!("agent construction failed: {e}")));
        }
    };

    // Step 9: Create the TurnRequest with assembled context and cancellation.
    let turn_request = TurnRequest {
        session_id,
        thread_id,
        user_message: content,
        assembled_context: assembled_messages,
        parent_turn_id: None,
        cancellation_token: cancel_token,
    };

    // Step 10: Create the event channel and spawn the agent's streaming loop.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Spawn the agent on a tokio task — it manages turn creation, state
    // transitions, tool dispatch, and message storage internally.
    let activity_tracker = state.activity_tracker.clone();
    let pricing_config = state.config.pricing.clone();
    let model_name = config.provider.model.clone();

    let agent_handle = tokio::spawn(async move {
        agent.process_turn_streaming(turn_request, event_tx).await
    });

    // Captures the turn_id emitted by TurnStarted so the agent result match
    // can use it for persistence and logging after the event loop finishes.
    let mut turn_id = Uuid::nil();

    // Step 11: Forward AgentEvent → ServerEvent. The loop runs until the
    // agent drops event_tx (channel closes) or the agent task completes.
    while let Some(event) = event_rx.recv().await {
        match event {
            AgentEvent::TurnStarted { turn_id: started_id } => {
                turn_id = started_id;
                // Record the turn_id in the tracing span now that the agent
                // has created the Turn record.
                tracing::Span::current().record("turn_id", turn_id.to_string());
                tracing::debug!("processing turn (agent-delegated)");

                // Persist the context snapshot now that we have a turn_id.
                if let Err(e) = state
                    .storage
                    .store_snapshot(turn_id, &report_json.to_string())
                    .await
                {
                    tracing::warn!(error = %e, "failed to persist context snapshot");
                }

                // Deliver the ContextAssembled event (subscription-gated).
                let assembled_event = ServerEvent::ContextAssembled {
                    turn_id,
                    report: report_json.clone(),
                };
                if subscription.lock().await.should_deliver(&assembled_event) {
                    let _ = ws_tx.send(assembled_event);
                }
            }
            AgentEvent::Token { content } => {
                activity_tracker.record_activity();
                let _ = ws_tx.send(ServerEvent::Token { content });
            }
            AgentEvent::ToolStart { call_id, tool_name, arguments } => {
                activity_tracker.record_activity();
                let tool_start = ServerEvent::ToolStart {
                    call_id,
                    tool: tool_name,
                    arguments,
                    parent_call_id: None,
                };
                if subscription.lock().await.should_deliver(&tool_start) {
                    let _ = ws_tx.send(tool_start);
                }
            }
            AgentEvent::ToolEnd { call_id, tool_name, result, status, duration_ms } => {
                let tool_end = ServerEvent::ToolEnd {
                    call_id,
                    tool: tool_name,
                    result,
                    status,
                    duration_ms: duration_ms.unwrap_or(0),
                    truncated: false,
                    total_bytes: None,
                    parent_call_id: None,
                };
                if subscription.lock().await.should_deliver(&tool_end) {
                    let _ = ws_tx.send(tool_end);
                }
            }
            AgentEvent::OutputSchemaRetry { turn_id, attempt, max_retries, errors } => {
                // Retry progress is observability noise — only deliver to context subscribers.
                let retry_event = ServerEvent::OutputSchemaRetry {
                    turn_id,
                    attempt,
                    max_retries,
                    errors: errors
                        .into_iter()
                        .map(|e| crate::websocket::SchemaValidationError {
                            path: e.path,
                            message: e.message,
                        })
                        .collect(),
                };
                if subscription.lock().await.should_deliver(&retry_event) {
                    let _ = ws_tx.send(retry_event);
                }
            }
            AgentEvent::OutputSchemaFailed { turn_id, attempts, errors } => {
                // Schema exhaustion is an error condition — always deliver regardless
                // of subscription state.
                let _ = ws_tx.send(ServerEvent::OutputSchemaFailed {
                    turn_id,
                    attempts,
                    errors: errors
                        .into_iter()
                        .map(|e| crate::websocket::SchemaValidationError {
                            path: e.path,
                            message: e.message,
                        })
                        .collect(),
                });
            }
            AgentEvent::TurnCompleted { turn_id, input_tokens, output_tokens, structured_output, .. } => {
                // Derive cost from the pricing config when available.
                let cost_microcents = pricing_config.compute_cost(
                    &model_name,
                    input_tokens,
                    output_tokens,
                );

                tracing::info!(
                    input_tokens = input_tokens,
                    output_tokens = output_tokens,
                    cost_microcents = ?cost_microcents,
                    "turn_complete",
                );

                // Update session activity timestamp.
                if let Err(e) = state.storage.update_activity(session_id).await {
                    tracing::warn!(error = %e, "failed to update session activity");
                }

                // Populate usage fields only for context-subscribed connections.
                let (ws_input, ws_output, ws_cost) =
                    if subscription.lock().await.is_subscribed(
                        crate::subscription::ObservabilityChannel::Context,
                    ) {
                        (Some(input_tokens), Some(output_tokens), cost_microcents)
                    } else {
                        (None, None, None)
                    };

                let _ = ws_tx.send(ServerEvent::TurnComplete {
                    turn_id,
                    input_tokens: ws_input,
                    output_tokens: ws_output,
                    cost_microcents: ws_cost,
                    structured_output: structured_output.clone(),
                });
            }
            AgentEvent::Error { message } => {
                let _ = ws_tx.send(ServerEvent::Error {
                    code: "agent_error".to_string(),
                    message,
                });
            }
            // Approval lifecycle events — route to the session-scoped ws_tx
            // so only the client connected to this agent sees the event.
            event @ (AgentEvent::ApprovalRequired { .. }
                | AgentEvent::ApprovalConfirmed { .. }
                | AgentEvent::ApprovalExpired { .. }) => {
                route_approval_agent_event(event, &ws_tx);
            }
        }
    }

    // Wait for the agent task to finish and surface any panic or error.
    match agent_handle.await {
        Ok(Ok(outbound)) => {
            // Agent completed successfully — persist structured_output if the
            // turn produced validated schema output.
            if let Some(ref output) = outbound.structured_output {
                use swage_storage::store::TurnStore;
                if let Err(e) = TurnStore::update_structured_output(
                    &*state.storage,
                    turn_id,
                    output,
                ).await {
                    // Non-fatal: the turn completed; only the persisted record is affected.
                    tracing::warn!(error = %e, %turn_id, "failed to persist structured_output");
                }
            }
        }
        Ok(Err(AgentError::Cancelled)) => {
            // Cancellation is cooperative — the agent already set
            // TurnState::Interrupted and ThreadState::Idle in storage.
            let _ = ws_tx.send(ServerEvent::Cancelled {});
        }
        Ok(Err(AgentError::OutputSchemaFailed { .. })) => {
            // The agent already emitted AgentEvent::OutputSchemaFailed through
            // the event channel (forwarded above). Log for server visibility but
            // do not send an additional error to the client.
            tracing::info!(%turn_id, "output schema validation exhausted all retries");
        }
        Ok(Err(agent_err)) => {
            // The agent already sent an Error event through the channel
            // for most failure paths. Log for server-side visibility but
            // avoid double-sending to the client — the event loop already
            // forwarded the AgentEvent::Error.
            tracing::error!(error = %agent_err, "agent turn failed");
        }
        Err(join_err) => {
            tracing::error!(error = %join_err, "agent task panicked");
            let _ = ws_tx.send(ServerEvent::Error {
                code: "internal_error".to_string(),
                message: "agent task panicked".to_string(),
            });
        }
    }

    // Cleanup: remove the cancellation token regardless of outcome.
    {
        let mut turns = state.active_turns.lock().await;
        turns.remove(&session_id);
    }

    Ok(())
}

/// Cancel the active turn for a session.
///
/// Returns an error if there is no active turn to cancel.
pub async fn cancel_turn(state: &AppState, session_id: Uuid) -> Result<(), GatewayError> {
    let turns = state.active_turns.lock().await;
    match turns.get(&session_id) {
        Some(token) => {
            token.cancel();
            Ok(())
        }
        None => Err(GatewayError::WebSocketError(format!(
            "no active turn for session {session_id}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Approval event routing
// ---------------------------------------------------------------------------

/// Forward an approval lifecycle `AgentEvent` to the WebSocket sender for
/// this session.
///
/// Approval events are session-scoped: they go only to the client connected
/// to the agent that raised the gate. Calling `state.broadcast.send` instead
/// would deliver to every connected client — this function deliberately uses
/// the per-turn `ws_tx` channel to preserve that isolation.
///
/// Returns `true` when the event was an approval variant and was forwarded;
/// `false` for all other event types.
pub(crate) fn route_approval_agent_event(
    event: AgentEvent,
    ws_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) -> bool {
    match event {
        AgentEvent::ApprovalRequired {
            approval_id,
            turn_id,
            agent_id,
            kind,
            tool_name,
            tool_arguments,
            hook_name: _, // hook identity is internal — the approval UI doesn't need it
            prompt,
            context,
            timeout_at,
            timeout_policy,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalRequired {
                approval_id: approval_id.to_string(),
                turn_id: turn_id.to_string(),
                agent_id,
                kind,
                tool_name,
                tool_arguments,
                prompt,
                context,
                timeout_at,
                timeout_policy,
            });
            true
        }
        AgentEvent::ApprovalConfirmed { approval_id, decision, reason } => {
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval_id.to_string(),
                decision,
                reason,
            });
            true
        }
        AgentEvent::ApprovalExpired { approval_id, timeout_policy } => {
            let _ = ws_tx.send(ServerEvent::ApprovalExpired {
                approval_id: approval_id.to_string(),
                timeout_policy,
            });
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        collections::HashMap,
        pin::Pin,
        sync::Arc,
    };

    use async_trait::async_trait;
    use futures::stream;
    use tokio::sync::{Mutex, mpsc};

    use swage_core::{
        CompletionRequest, CompletionResponse, FinishReason,
        ProviderError, StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;
    use swage_core::StorageError;
    use swage_storage::{
        model::{
            ActiveSkillRecord, ApprovalKind, ApprovalRequest, ApprovalState, Message,
            NewApprovalRequest, NewMessage, NewProject, NewSession, NewThread, NewToolCall, NewTurn,
            Project, Session, Thread, ThreadState, ToolCallRecord, Turn, TurnState,
        },
        store::{
            ApprovalStore, ContextSnapshotStore, MessageStore, ProjectStore, SessionStore,
            SkillActivationStore, ThreadStore, ToolCallStore, TurnStore,
        },
    };


    use swage_agent::AgentEvent;

    use super::*;
    use crate::{state::AppState, subscription::SubscriptionState, websocket::ServerEvent};

    // -----------------------------------------------------------------------
    // SWAGE_HOME isolation for config tests
    // -----------------------------------------------------------------------

    /// Serialise tests that mutate SWAGE_HOME — it's process-wide state.
    static SWAGE_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard that points SWAGE_HOME at a non-existent path so the gateway
    /// falls back to the injected `state.config` instead of loading from disk.
    struct SwageHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl SwageHomeGuard {
        fn isolate() -> Self {
            let lock = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // Safety: test-only env mutation; set_var is unsafe in edition 2024.
            unsafe { std::env::set_var("SWAGE_HOME", "/tmp/swage_nonexistent_test_root") };
            Self { _lock: lock }
        }
    }

    impl Drop for SwageHomeGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("SWAGE_HOME") };
        }
    }

    // -----------------------------------------------------------------------
    // Mock LLM providers
    // -----------------------------------------------------------------------

    /// A mock provider that streams a fixed sequence of chunks.
    pub(crate) struct MockProvider {
        chunks: std::sync::Mutex<Vec<StreamChunk>>,
    }

    impl MockProvider {
        pub(crate) fn new(chunks: Vec<StreamChunk>) -> Self {
            Self {
                chunks: std::sync::Mutex::new(chunks),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn model_id(&self) -> &str { "mock-model" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in streaming tests")
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not used in streaming tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            let chunks = {
                let mut lock = self.chunks.lock().expect("mock lock poisoned");
                std::mem::take(&mut *lock)
            };
            Box::pin(stream::iter(chunks))
        }
    }

    /// A mock provider supporting multiple streaming rounds.
    pub(crate) struct MultiRoundMockProvider {
        rounds: std::sync::Mutex<std::collections::VecDeque<Vec<StreamChunk>>>,
    }

    impl MultiRoundMockProvider {
        pub(crate) fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
            Self {
                rounds: std::sync::Mutex::new(rounds.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MultiRoundMockProvider {
        fn model_id(&self) -> &str { "mock-multi-round" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in multi-round tests")
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not used in multi-round tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            let chunks = self
                .rounds
                .lock()
                .expect("mock lock poisoned")
                .pop_front()
                .unwrap_or_default();
            Box::pin(stream::iter(chunks))
        }
    }

    /// A mock provider whose stream blocks forever.
    pub(crate) struct PendingProvider;

    #[async_trait]
    impl LlmProvider for PendingProvider {
        fn model_id(&self) -> &str { "pending-model" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in cancellation tests")
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not used in cancellation tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::pending())
        }
    }

    // -----------------------------------------------------------------------
    // Mock storage — in-memory Vec/HashMap backed, usable in tests.
    // -----------------------------------------------------------------------

    /// In-memory storage for testing.
    pub(crate) struct MockStorage {
        pub(crate) db: Mutex<InMemoryDb>,
    }

    #[derive(Default)]
    pub(crate) struct InMemoryDb {
        pub(crate) projects: HashMap<Uuid, Project>,
        pub(crate) sessions: HashMap<Uuid, Session>,
        pub(crate) threads: HashMap<Uuid, Thread>,
        pub(crate) turns: HashMap<Uuid, Turn>,
        pub(crate) messages: Vec<Message>,
        pub(crate) tool_calls: Vec<ToolCallRecord>,
        pub(crate) snapshots: HashMap<Uuid, String>,
        pub(crate) approvals: HashMap<Uuid, ApprovalRequest>,
    }

    impl MockStorage {
        pub(crate) fn new() -> Self {
            Self {
                db: Mutex::new(InMemoryDb::default()),
            }
        }
    }

    #[async_trait]
    impl ProjectStore for MockStorage {
        async fn create(&self, project: NewProject) -> Result<Project, StorageError> {
            let p = Project {
                id: Uuid::new_v4(),
                name: project.name,
                slug: project.slug,
                workspace_path: project.workspace_path,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.db.lock().await.projects.insert(p.id, p.clone());
            Ok(p)
        }

        async fn get(&self, id: Uuid) -> Result<Option<Project>, StorageError> {
            Ok(self.db.lock().await.projects.get(&id).cloned())
        }

        async fn get_by_name(&self, name: &str) -> Result<Option<Project>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .projects
                .values()
                .find(|p| p.name == name)
                .cloned())
        }

        async fn get_or_create_default(&self) -> Result<Project, StorageError> {
            if let Some(p) = self.get_by_name("default").await? {
                return Ok(p);
            }
            ProjectStore::create(self, NewProject {
                name: "default".to_string(),
                slug: "default".to_string(),
                workspace_path: None,
            })
            .await
        }

        async fn list_all(&self) -> Result<Vec<Project>, StorageError> {
            let mut projects: Vec<Project> =
                self.db.lock().await.projects.values().cloned().collect();
            projects.sort_by_key(|p| p.created_at);
            Ok(projects)
        }

        async fn get_by_slug(&self, slug: &str) -> Result<Option<Project>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .projects
                .values()
                .find(|p| p.slug == slug)
                .cloned())
        }

        async fn update(
            &self,
            id: Uuid,
            name: Option<String>,
            slug: Option<String>,
            workspace_path: Option<String>,
        ) -> Result<Project, StorageError> {
            let mut db = self.db.lock().await;
            match db.projects.get_mut(&id) {
                None => Err(StorageError::NotFound { entity: "Project".into(), id: id.to_string() }),
                Some(p) => {
                    if let Some(n) = name { p.name = n; }
                    if let Some(s) = slug { p.slug = s; }
                    if let Some(wp) = workspace_path { p.workspace_path = Some(wp); }
                    p.updated_at = chrono::Utc::now();
                    Ok(p.clone())
                }
            }
        }

        async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
            let mut db = self.db.lock().await;
            match db.projects.get(&id) {
                None => Ok(false),
                Some(p) if p.name == "default" => Err(StorageError::ConstraintViolation(
                    "cannot delete the default project".into(),
                )),
                _ => Ok(db.projects.remove(&id).is_some()),
            }
        }
    }

    #[async_trait]
    impl SessionStore for MockStorage {
        async fn create_session(&self, session: NewSession) -> Result<Session, StorageError> {
            let s = Session {
                id: Uuid::new_v4(),
                project_id: session.project_id,
                agent_id: session.agent_id,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.db.lock().await.sessions.insert(s.id, s.clone());
            Ok(s)
        }

        async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
            Ok(self.db.lock().await.sessions.get(&id).cloned())
        }

        async fn list_by_project(
            &self,
            project_id: Uuid,
        ) -> Result<Vec<Session>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .sessions
                .values()
                .filter(|s| s.project_id == project_id)
                .cloned()
                .collect())
        }

        async fn list_sessions_by_agent_id(&self, agent_id: &str) -> Result<Vec<Session>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .sessions
                .values()
                .filter(|s| s.agent_id == agent_id)
                .cloned()
                .collect())
        }

        async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError> {
            Ok(self.db.lock().await.sessions.remove(&id).is_some())
        }

        async fn update_activity(&self, id: Uuid) -> Result<(), StorageError> {
            if let Some(session) = self.db.lock().await.sessions.get_mut(&id) {
                session.updated_at = chrono::Utc::now();
            }
            Ok(())
        }
    }

    #[async_trait]
    impl ThreadStore for MockStorage {
        async fn create(&self, thread: NewThread) -> Result<Thread, StorageError> {
            let t = Thread {
                id: Uuid::new_v4(),
                session_id: thread.session_id,
                agent_id: thread.agent_id,
                state: ThreadState::Idle,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.db.lock().await.threads.insert(t.id, t.clone());
            Ok(t)
        }

        async fn get(&self, id: Uuid) -> Result<Option<Thread>, StorageError> {
            Ok(self.db.lock().await.threads.get(&id).cloned())
        }

        async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<Thread>, StorageError> {
            let mut threads: Vec<Thread> = self
                .db
                .lock()
                .await
                .threads
                .values()
                .filter(|t| t.session_id == session_id)
                .cloned()
                .collect();
            threads.sort_by_key(|t| t.created_at);
            Ok(threads)
        }

        async fn list_threads_by_agent_id(&self, agent_id: &str) -> Result<Vec<Thread>, StorageError> {
            let mut threads: Vec<Thread> = self
                .db
                .lock()
                .await
                .threads
                .values()
                .filter(|t| t.agent_id == agent_id)
                .cloned()
                .collect();
            threads.sort_by_key(|t| t.created_at);
            Ok(threads)
        }

        async fn update_state(&self, id: Uuid, state: ThreadState) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            match db.threads.get_mut(&id) {
                None => Err(StorageError::NotFound { entity: "Thread".into(), id: id.to_string() }),
                Some(t) => {
                    if !t.state.can_transition_to(&state) {
                        return Err(StorageError::ConstraintViolation(format!(
                            "invalid thread state transition: {:?} → {:?}",
                            t.state, state
                        )));
                    }
                    t.state = state;
                    t.updated_at = chrono::Utc::now();
                    Ok(())
                }
            }
        }

        async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
            Ok(self.db.lock().await.threads.remove(&id).is_some())
        }
    }

    #[async_trait]
    impl TurnStore for MockStorage {
        async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError> {
            let t = Turn {
                id: Uuid::new_v4(),
                thread_id: turn.thread_id,
                agent_id: turn.agent_id,
                state: turn.state,
                user_input: None,
                assistant_response: None,
                input_tokens: None,
                output_tokens: None,
                cost_microcents: None,
                structured_output: None,
                parent_turn_id: turn.parent_turn_id,
                created_at: chrono::Utc::now(),
                completed_at: None,
            };
            self.db.lock().await.turns.insert(t.id, t.clone());
            Ok(t)
        }

        async fn update_structured_output(
            &self,
            id: Uuid,
            output: &serde_json::Value,
        ) -> Result<(), StorageError> {
            if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
                t.structured_output = Some(output.clone());
            }
            Ok(())
        }

        async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
            Ok(self.db.lock().await.turns.get(&id).cloned())
        }

        async fn list_turns_by_thread(
            &self,
            thread_id: Uuid,
        ) -> Result<Vec<Turn>, StorageError> {
            let mut turns: Vec<Turn> = self
                .db
                .lock()
                .await
                .turns
                .values()
                .filter(|t| t.thread_id == thread_id)
                .cloned()
                .collect();
            turns.sort_by_key(|t| t.created_at);
            Ok(turns)
        }

        async fn update_turn_state(
            &self,
            id: Uuid,
            state: TurnState,
        ) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            if let Some(turn) = db.turns.get_mut(&id) {
                if matches!(state, TurnState::Completed | TurnState::Failed | TurnState::Interrupted) {
                    turn.completed_at = Some(chrono::Utc::now());
                }
                turn.state = state;
            }
            Ok(())
        }

        async fn find_in_progress_by_agent(
            &self,
            agent_id: &str,
        ) -> Result<Vec<Turn>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .turns
                .values()
                .filter(|t| {
                    t.agent_id == agent_id
                        && matches!(t.state, TurnState::Pending | TurnState::Processing)
                })
                .cloned()
                .collect())
        }

        async fn update_response(&self, id: Uuid, response: &str) -> Result<(), StorageError> {
            if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
                t.assistant_response = Some(response.to_string());
            }
            Ok(())
        }

        async fn update_tokens(
            &self,
            id: Uuid,
            input: i64,
            output: i64,
        ) -> Result<(), StorageError> {
            if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
                t.input_tokens = Some(input);
                t.output_tokens = Some(output);
            }
            Ok(())
        }

        async fn update_cost(&self, id: Uuid, cost: i64) -> Result<(), StorageError> {
            if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
                t.cost_microcents = Some(cost);
            }
            Ok(())
        }
    }

    #[async_trait]
    impl ContextSnapshotStore for MockStorage {
        async fn store_snapshot(
            &self,
            turn_id: Uuid,
            report_json: &str,
        ) -> Result<(), StorageError> {
            self.db.lock().await.snapshots.insert(turn_id, report_json.to_string());
            Ok(())
        }

        async fn get_snapshot(&self, turn_id: Uuid) -> Result<Option<String>, StorageError> {
            Ok(self.db.lock().await.snapshots.get(&turn_id).cloned())
        }
    }

    #[async_trait]
    impl MessageStore for MockStorage {
        async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError> {
            let m = Message {
                id: Uuid::new_v4(),
                thread_id: message.thread_id,
                role: message.role,
                content: message.content,
                position: message.position,
                agent_id: message.agent_id,
                tool_call_id: None,
                tool_name: None,
                created_at: chrono::Utc::now(),
            };
            self.db.lock().await.messages.push(m.clone());
            Ok(m)
        }

        async fn list_messages_by_thread(
            &self,
            thread_id: Uuid,
        ) -> Result<Vec<Message>, StorageError> {
            let db = self.db.lock().await;
            let mut msgs: Vec<Message> = db
                .messages
                .iter()
                .filter(|m| m.thread_id == thread_id)
                .cloned()
                .collect();
            msgs.sort_by_key(|m| m.position);
            Ok(msgs)
        }

        async fn get_latest_for_session(
            &self,
            session_id: Uuid,
        ) -> Result<Option<Message>, StorageError> {
            let db = self.db.lock().await;
            // Collect thread_ids belonging to this session.
            let thread_ids: std::collections::HashSet<Uuid> = db
                .threads
                .values()
                .filter(|t| t.session_id == session_id)
                .map(|t| t.id)
                .collect();
            // Return the message with the highest position across all threads.
            Ok(db
                .messages
                .iter()
                .filter(|m| thread_ids.contains(&m.thread_id))
                .max_by_key(|m| (m.created_at, m.position))
                .cloned())
        }

        async fn get_message(&self, id: Uuid) -> Result<Option<Message>, StorageError> {
            Ok(self.db.lock().await.messages.iter().find(|m| m.id == id).cloned())
        }

        async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError> {
            self.db.lock().await.messages.retain(|m| m.thread_id != thread_id);
            Ok(())
        }
    }

    #[async_trait]
    impl ToolCallStore for MockStorage {
        async fn create_tool_call(
            &self,
            tool_call: NewToolCall,
        ) -> Result<ToolCallRecord, StorageError> {
            let record = ToolCallRecord {
                id: Uuid::new_v4(),
                turn_id: tool_call.turn_id,
                agent_id: tool_call.agent_id,
                call_id: tool_call.call_id,
                tool: tool_call.tool,
                arguments: tool_call.arguments,
                result: tool_call.result,
                status: tool_call.status,
                duration_ms: tool_call.duration_ms,
                truncated: tool_call.truncated,
                total_bytes: tool_call.total_bytes,
                position: tool_call.position,
                created_at: chrono::Utc::now(),
                parent_call_id: tool_call.parent_call_id,
            };
            self.db.lock().await.tool_calls.push(record.clone());
            Ok(record)
        }

        async fn get_tool_call(
            &self,
            id: Uuid,
        ) -> Result<Option<ToolCallRecord>, StorageError> {
            let db = self.db.lock().await;
            Ok(db.tool_calls.iter().find(|tc| tc.id == id).cloned())
        }

        async fn update_tool_call_result(
            &self,
            call_id: &str,
            turn_id: Uuid,
            result: &str,
            status: &str,
            duration_ms: i64,
            truncated: bool,
            total_bytes: Option<i64>,
        ) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            if let Some(tc) = db
                .tool_calls
                .iter_mut()
                .find(|tc| tc.call_id == call_id && tc.turn_id == turn_id)
            {
                tc.result = Some(result.to_string());
                tc.status = Some(status.to_string());
                tc.duration_ms = Some(duration_ms);
                tc.truncated = truncated;
                tc.total_bytes = total_bytes;
            }
            Ok(())
        }

        async fn list_tool_calls_by_turn(
            &self,
            turn_id: Uuid,
        ) -> Result<Vec<ToolCallRecord>, StorageError> {
            let db = self.db.lock().await;
            let mut calls: Vec<ToolCallRecord> = db
                .tool_calls
                .iter()
                .filter(|tc| tc.turn_id == turn_id)
                .cloned()
                .collect();
            calls.sort_by_key(|tc| tc.position);
            Ok(calls)
        }
    }

    // Stub: skill activation is not exercised in gateway turn tests.
    #[async_trait]
    impl SkillActivationStore for MockStorage {
        async fn activate_skill(&self, _session_id: Uuid, _skill_name: &str) -> Result<(), StorageError> {
            Ok(())
        }
        async fn deactivate_skill(&self, _session_id: Uuid, _skill_name: &str) -> Result<bool, StorageError> {
            Ok(false)
        }
        async fn list_active_skills(&self, _session_id: Uuid) -> Result<Vec<ActiveSkillRecord>, StorageError> {
            Ok(vec![])
        }
        async fn clear_active_skills(&self, _session_id: Uuid) -> Result<(), StorageError> {
            Ok(())
        }
    }

    #[async_trait]
    impl ApprovalStore for MockStorage {
        async fn create_approval(
            &self,
            approval: NewApprovalRequest,
        ) -> Result<ApprovalRequest, StorageError> {
            let approval_req = ApprovalRequest {
                id: Uuid::new_v4(),
                turn_id: approval.turn_id,
                agent_id: approval.agent_id,
                kind: approval.kind,
                tool_name: approval.tool_name,
                tool_arguments: approval.tool_arguments,
                hook_name: approval.hook_name,
                prompt: approval.prompt,
                context: approval.context,
                timeout_at: approval.timeout_at,
                timeout_policy: approval.timeout_policy,
                state: ApprovalState::Pending,
                decision_reason: None,
                decided_at: None,
                created_at: chrono::Utc::now(),
            };
            self.db.lock().await.approvals.insert(approval_req.id, approval_req.clone());
            Ok(approval_req)
        }

        async fn get_approval(&self, id: Uuid) -> Result<Option<ApprovalRequest>, StorageError> {
            Ok(self.db.lock().await.approvals.get(&id).cloned())
        }

        async fn list_pending_by_agent(
            &self,
            agent_id: &str,
        ) -> Result<Vec<ApprovalRequest>, StorageError> {
            let db = self.db.lock().await;
            let mut result: Vec<_> = db
                .approvals
                .values()
                .filter(|a| a.agent_id == agent_id && a.state == ApprovalState::Pending)
                .cloned()
                .collect();
            result.sort_by_key(|a| a.created_at);
            Ok(result)
        }

        async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
            let db = self.db.lock().await;
            let mut result: Vec<_> = db
                .approvals
                .values()
                .filter(|a| a.state == ApprovalState::Pending)
                .cloned()
                .collect();
            result.sort_by_key(|a| a.created_at);
            Ok(result)
        }

        async fn update_approval_state(
            &self,
            id: Uuid,
            state: ApprovalState,
            reason: Option<&str>,
        ) -> Result<bool, StorageError> {
            let mut db = self.db.lock().await;
            match db.approvals.get_mut(&id) {
                None => Ok(false),
                Some(approval) => {
                    if approval.state != ApprovalState::Pending {
                        return Ok(false);
                    }
                    approval.state = state;
                    approval.decision_reason = reason.map(|s| s.to_string());
                    approval.decided_at = Some(chrono::Utc::now());
                    Ok(true)
                }
            }
        }

        async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError> {
            let mut db = self.db.lock().await;
            let expired_ids: Vec<_> = db
                .approvals
                .iter()
                .filter(|(_, a)| !db.turns.contains_key(&a.turn_id))
                .map(|(id, _)| *id)
                .collect();
            let count = expired_ids.len() as u64;
            for id in expired_ids {
                if let Some(approval) = db.approvals.get_mut(&id) {
                    if approval.state == ApprovalState::Pending {
                        approval.state = ApprovalState::Expired;
                        approval.decided_at = Some(chrono::Utc::now());
                    }
                }
            }
            Ok(count)
        }
    }

    #[async_trait]
    impl swage_storage::store::AgentMessageStore for MockStorage {
        async fn create_agent_message(
            &self,
            _: swage_storage::model::NewAgentMessage,
        ) -> Result<swage_storage::model::AgentMessage, StorageError> {
            unimplemented!()
        }
        async fn get_agent_message(
            &self,
            _: Uuid,
        ) -> Result<Option<swage_storage::model::AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_for_agent(
            &self,
            _: &str,
        ) -> Result<Vec<swage_storage::model::AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn update_agent_message_status(
            &self,
            _: Uuid,
            _: swage_core::MessageStatus,
            _: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn list_by_correlation(
            &self,
            _: Uuid,
        ) -> Result<Vec<swage_storage::model::AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn expire_stale_messages(
            &self,
            _: std::time::Duration,
        ) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build an AppState with a pre-populated session and its default thread.
    ///
    /// Returns the state and the session_id. The thread is created automatically
    /// so that `process_turn` can look it up via `list_by_session`.
    pub(crate) async fn make_state_with_session(
        provider_chunks: Vec<StreamChunk>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        // Create the default project via ProjectStore.
        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        // Create a session.
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        // Create the default thread for this session.
        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create default thread");

        let provider = Arc::new(MockProvider::new(provider_chunks));
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };
        (state, session_id)
    }

    /// Like `make_state_with_session` but accepts an explicit `ResolvedConfig`.
    ///
    /// Used for tests that need non-default config (e.g. output schema configured).
    pub(crate) async fn make_state_with_config(
        provider_chunks: Vec<StreamChunk>,
        config: swage_config::ResolvedConfig,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create default thread");

        let provider = Arc::new(MockProvider::new(provider_chunks));
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config,
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };
        (state, session_id)
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn full_turn_stores_messages_and_completes() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Delta { content: ", world".to_string() },
            StreamChunk::Delta { content: "!".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete without error");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let token_contents: Vec<String> = events
            .iter()
            .filter_map(|e| {
                if let ServerEvent::Token { content } = e {
                    Some(content.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(token_contents, ["Hello", ", world", "!"]);
        assert!(events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. })));

        // Get thread_id and verify messages stored in the DB.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let messages = state.storage.list_messages_by_thread(thread_id).await.unwrap();
        assert_eq!(messages.len(), 2, "user message + assistant message");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hi");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "Hello, world!");

        // Verify the turn was marked Completed.
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].state, TurnState::Completed);
    }

    #[tokio::test]
    async fn cancelled_turn_stores_partial_and_marks_interrupted() {
        let storage = Arc::new(MockStorage::new());

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create default thread");

        let state = Arc::new(AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: Arc::new(PendingProvider) as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        });

        let (tx, mut rx) = mpsc::unbounded_channel();

        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            process_turn(&*state_clone, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await
        });

        for _ in 0..50 {
            tokio::task::yield_now().await;
            if state.active_turns.lock().await.contains_key(&session_id) {
                break;
            }
        }

        assert!(
            state.active_turns.lock().await.contains_key(&session_id),
            "turn should be active before cancel"
        );

        cancel_turn(&state, session_id)
            .await
            .expect("cancel should succeed");

        handle.await.expect("task should not panic").expect("process_turn ok");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| matches!(e, ServerEvent::Cancelled {})),
            "expected Cancelled event"
        );

        // Verify turn is Interrupted in storage.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns[0].state, TurnState::Interrupted);
    }

    #[tokio::test]
    async fn provider_error_marks_turn_failed() {
        let (state, session_id) = make_state_with_session(vec![StreamChunk::Error {
            message: "provider overloaded".to_string(),
        }])
        .await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("error is surfaced via event, not Err return");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        // Agent delegates error categorisation — provider errors surface as agent_error.
        assert!(events.iter().any(|e| matches!(
            e,
            ServerEvent::Error { code, .. } if code == "agent_error"
        )));

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns[0].state, TurnState::Failed);
    }

    #[tokio::test]
    async fn concurrent_turn_rejected_with_turn_in_progress() {
        // Manually create a Processing turn to simulate an active turn.
        let (state, session_id) = make_state_with_session(vec![]).await;

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        // Create a turn and advance it to Processing.
        let turn = state
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                parent_turn_id: None,
            })
            .await
            .unwrap();
        state
            .storage
            .update_turn_state(turn.id, TurnState::Processing)
            .await
            .unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = process_turn(&state, session_id, "Another message".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await;

        assert!(
            matches!(result, Err(GatewayError::TurnInProgress { .. })),
            "expected TurnInProgress, got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Mock tool for the execution loop
    // -----------------------------------------------------------------------

    struct EchoMockTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for EchoMockTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "Echo the input back" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            })
        }
        async fn execute(
            &self,
            arguments: serde_json::Value,
            _ctx: &dyn swage_tool::context::ToolContext,
        ) -> Result<serde_json::Value, swage_core::ToolError> {
            let text = arguments["text"].as_str().unwrap_or("").to_string();
            Ok(serde_json::json!({"output": text}))
        }
    }

    /// Build an AppState with a multi-round provider and an EchoMockTool.
    async fn make_state_with_tools(
        rounds: Vec<Vec<StreamChunk>>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create default thread");

        let provider = Arc::new(MultiRoundMockProvider::new(rounds));

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![
                Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>,
            ])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };
        (state, session_id)
    }

    // -----------------------------------------------------------------------
    // Tool execution loop tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn tool_call_sends_tool_start_and_tool_end_events() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "The tool said: hello".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use echo".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolStart { tool, call_id, .. }
                if tool == "echo" && call_id == "call_1"
            )),
            "expected ToolStart event, got: {events:?}"
        );

        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolEnd { tool, call_id, status, .. }
                if tool == "echo" && call_id == "call_1" && status == "success"
            )),
            "expected ToolEnd event, got: {events:?}"
        );

        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::Token { content } if content.contains("hello")
            )),
            "expected token with 'hello', got: {events:?}"
        );

        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
    }

    #[tokio::test]
    async fn tool_call_stores_tool_call_record() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_store".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "stored" }),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Store this".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 1, "expected 1 tool call record");
        assert_eq!(tool_calls[0].call_id, "call_store");
        assert_eq!(tool_calls[0].tool, "echo");
        assert!(tool_calls[0].result.is_some(), "tool call should have a result");
        // The agent serialises the full ToolOutput.result JSON as the stored string.
        assert_eq!(tool_calls[0].result.as_deref(), Some("{\"output\":\"stored\"}"));
    }

    #[tokio::test]
    async fn unknown_tool_returns_error_to_model() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_unknown".into(),
                name: "nonexistent_tool".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "Sorry, that tool doesn't exist.".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use unknown".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should still complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolEnd { status, .. } if status == "error"
            )),
            "expected ToolEnd with error status, got: {events:?}"
        );

        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
    }

    #[tokio::test]
    async fn multi_round_tool_calling() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "first" }),
            },
            StreamChunk::ToolUse {
                call_id: "c2".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "second" }),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::ToolUse {
                call_id: "c3".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "third" }),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_3 = vec![
            StreamChunk::Delta { content: "All done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let (state, session_id) =
            make_state_with_tools(vec![round_1, round_2, round_3]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Multi-round".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let tool_starts: Vec<_> = events.iter().filter(|e| matches!(e, ServerEvent::ToolStart { .. })).collect();
        let tool_ends: Vec<_> = events.iter().filter(|e| matches!(e, ServerEvent::ToolEnd { .. })).collect();
        assert_eq!(tool_starts.len(), 3, "expected 3 ToolStart events");
        assert_eq!(tool_ends.len(), 3, "expected 3 ToolEnd events");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 3, "expected 3 tool call records");

        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));
    }

    // -----------------------------------------------------------------------
    // Project context and config resolution tests
    // -----------------------------------------------------------------------

    struct CapturingProvider {
        captured: Arc<std::sync::Mutex<Option<CompletionRequest>>>,
        chunks: Vec<StreamChunk>,
    }

    impl CapturingProvider {
        fn new(chunks: Vec<StreamChunk>) -> (Self, Arc<std::sync::Mutex<Option<CompletionRequest>>>) {
            let captured = Arc::new(std::sync::Mutex::new(None));
            (
                Self { captured: Arc::clone(&captured), chunks },
                captured,
            )
        }
    }

    #[async_trait]
    impl LlmProvider for CapturingProvider {
        fn model_id(&self) -> &str { "capturing-model" }

        async fn complete(
            &self, _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!()
        }

        async fn complete_with_tools(
            &self, _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!()
        }

        fn stream(&self, request: CompletionRequest) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            *self.captured.lock().unwrap() = Some(request);
            Box::pin(stream::iter(self.chunks.clone()))
        }
    }

    struct ContextCaptureTool {
        captured: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for ContextCaptureTool {
        fn name(&self) -> &str { "capture_context" }
        fn description(&self) -> &str { "Captures the working_directory from the tool context" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn execute(
            &self,
            _: serde_json::Value,
            ctx: &dyn swage_tool::context::ToolContext,
        ) -> Result<serde_json::Value, swage_core::ToolError> {
            *self.captured.lock().unwrap() = Some(ctx.working_directory().to_path_buf());
            Ok(serde_json::json!({"output": "captured"}))
        }
    }

    /// Create a storage with a session + thread and return (storage, session_id).
    async fn make_storage_with_session() -> (Arc<MockStorage>, Uuid) {
        let storage = Arc::new(MockStorage::new());
        let project = storage.get_or_create_default().await.expect("project");
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("session");
        let session_id = session.id;
        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("thread");
        (storage, session_id)
    }

    #[tokio::test]
    async fn process_turn_passes_tool_context_to_tools() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let capture_tool = Arc::new(ContextCaptureTool { captured: Arc::clone(&captured) });

        let (storage, session_id) = make_storage_with_session().await;

        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "cc_1".into(),
                name: "capture_context".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![StreamChunk::Delta { content: "done".into() }, StreamChunk::Done { finish_reason: None }];

        let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![
                capture_tool as Arc<dyn crate::tool::Tool>,
            ])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "capture".into(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await.expect("turn ok");

        let working_dir = captured
            .lock()
            .unwrap()
            .take()
            .expect("ToolContext should have been passed during tool execution");

        assert!(working_dir.is_absolute(), "working_directory should be absolute: {working_dir:?}");
    }

    #[tokio::test]
    async fn process_turn_resolves_config_per_project_and_completes_turn() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".into() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (provider, captured_request) = CapturingProvider::new(chunks);

        let storage = Arc::new(MockStorage::new());
        let project = ProjectStore::create(&*storage, swage_storage::model::NewProject {
            name: "Test Config Project".to_string(),
            slug: "test-config-resolution-no-project-toml".to_string(),
            workspace_path: None,
        })
        .await
        .expect("create test project");
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("session");
        let session_id = session.id;
        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("thread");

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: Arc::new(provider) as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "hi".into(), tx, Arc::new(Mutex::new(SubscriptionState::new(true)))).await.expect("turn ok");

        // Verify the provider was invoked — the agent delegates model selection
        // to the provider via model_id() rather than setting CompletionRequest.model.
        let req = captured_request.lock().unwrap().take();
        assert!(req.is_some(), "provider should have been invoked for config-resolved turn");
    }

    #[tokio::test]
    async fn text_only_turn_still_works_with_tool_registry() {
        let (state, session_id) = make_state_with_tools(vec![vec![
            StreamChunk::Delta { content: "Just text".into() },
            StreamChunk::Done { finish_reason: None },
        ]]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hello".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, ServerEvent::Token { content } if content == "Just text")));
        assert!(events.iter().any(|e| matches!(e, ServerEvent::TurnComplete { .. })));

        assert!(!events.iter().any(|e| matches!(e, ServerEvent::ToolStart { .. })));
    }

    // -----------------------------------------------------------------------
    // ToolContext propagation tests
    // -----------------------------------------------------------------------

    struct WorkingDirReadTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for WorkingDirReadTool {
        fn name(&self) -> &str { "working_dir_read" }
        fn description(&self) -> &str { "Returns the working_directory from the ToolContext" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            ctx: &dyn swage_tool::context::ToolContext,
        ) -> Result<serde_json::Value, swage_core::ToolError> {
            let path = ctx.working_directory().to_string_lossy().into_owned();
            Ok(serde_json::json!({"output": path}))
        }
    }

    #[tokio::test]
    async fn tool_context_working_directory_is_passed_during_tool_execution() {
        let storage = Arc::new(MockStorage::new());
        let project = ProjectStore::create(&*storage, NewProject {
            name: "my-project".to_string(),
            slug: "my-project".to_string(),
            workspace_path: None,
        })
        .await
        .expect("create project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("thread");

        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "ctx-check".into(),
                name: "working_dir_read".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![
                Arc::new(WorkingDirReadTool) as Arc<dyn crate::tool::Tool>,
            ])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(&state, session_id, "Check context".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state.storage.list_tool_calls_by_turn(turns[0].id).await.unwrap();
        assert_eq!(tool_calls.len(), 1);
        // The agent stores the full JSON-serialised tool output.
        let result = tool_calls[0].result.as_deref().unwrap_or("");
        let result_json: serde_json::Value = serde_json::from_str(result)
            .expect("tool result should be valid JSON");
        let path_str = result_json["output"].as_str()
            .expect("output field should be a string");
        assert!(
            std::path::Path::new(path_str).is_absolute(),
            "working_directory should be an absolute path, got: {path_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Sandbox dispatch routing tests
    // -----------------------------------------------------------------------

    /// Records calls and returns a predefined result.
    struct MockSandbox {
        calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
        result: std::sync::Mutex<Option<Result<swage_sandbox::ToolCallResult, swage_core::SandboxError>>>,
    }

    impl MockSandbox {
        fn returning(result: Result<swage_sandbox::ToolCallResult, swage_core::SandboxError>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(vec![])),
                result: std::sync::Mutex::new(Some(result)),
            }
        }
    }

    #[async_trait]
    impl swage_sandbox::Sandbox for MockSandbox {
        async fn spawn_agent(&mut self) -> Result<(), swage_core::SandboxError> {
            Ok(())
        }

        async fn execute_tool(
            &self,
            name: &str,
            params: serde_json::Value,
        ) -> Result<swage_sandbox::ToolCallResult, swage_core::SandboxError> {
            self.calls.lock().await.push((name.to_string(), params));
            self.result.lock().unwrap().take().expect("execute_tool called more than once")
        }

        async fn shutdown(&mut self) -> Result<(), swage_core::SandboxError> {
            Ok(())
        }
    }

    /// Minimal tool stub for sandbox tests — registered so dispatch can find
    /// the tool, but execution is intercepted by SandboxedExecutor before
    /// reaching this implementation.
    struct StubTool { tool_name: &'static str }

    #[async_trait::async_trait]
    impl crate::tool::Tool for StubTool {
        fn name(&self) -> &str { self.tool_name }
        fn description(&self) -> &str { "stub for sandbox routing tests" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "additionalProperties": true})
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _ctx: &dyn swage_tool::context::ToolContext,
        ) -> Result<serde_json::Value, swage_core::ToolError> {
            // Should never be reached when SandboxedExecutor is the strategy.
            Err(swage_core::ToolError::ExecutionFailed {
                name: self.tool_name.to_string(),
                message: "direct execution not supported in sandbox mode".into(),
            })
        }
    }

    /// Build an AppState with an active sandbox and specified tools registered.
    ///
    /// Tools are registered in the dispatcher so the dispatch pipeline can find
    /// them (step 1). The SandboxedExecutor strategy intercepts execution at
    /// step 6, routing calls through the sandbox instead of executing locally.
    async fn make_state_with_sandbox(
        rounds: Vec<Vec<StreamChunk>>,
        sandbox: Arc<dyn swage_sandbox::Sandbox>,
        tools: Vec<Arc<dyn crate::tool::Tool>>,
    ) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        let project = storage.get_or_create_default().await.expect("project");
        let session = storage
            .create_session(NewSession { project_id: project.id, agent_id: "gateway".to_string() })
            .await
            .expect("session");
        let session_id = session.id;

        ThreadStore::create(&*storage, NewThread { session_id, agent_id: "gateway".to_string() })
            .await
            .expect("thread");

        let provider = Arc::new(MultiRoundMockProvider::new(rounds));
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(tools)),
            sandbox: Some(sandbox),
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: { let (tx, _) = tokio::sync::broadcast::channel(16); tx },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(Mutex::new(HashMap::new())),
        };
        (state, session_id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sandbox_routes_tool_calls_through_sandbox_when_present() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "sb_call".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "sandbox_hi"}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Ok(
            swage_sandbox::ToolCallResult::success(serde_json::json!("sandbox_output"), 5),
        )));
        // Register the tool so dispatch step 1 (lookup) succeeds; execution
        // is intercepted at step 6 by SandboxedExecutor.
        let tools: Vec<Arc<dyn crate::tool::Tool>> = vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>,
        ];
        let (state, session_id) = make_state_with_sandbox(vec![round_1, round_2], sandbox, tools).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use echo".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        // The empty direct dispatcher would produce status="error". Only the
        // sandbox path can produce status="success" here.
        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolEnd { call_id, status, .. }
                if call_id == "sb_call" && status == "success"
            )),
            "sandbox tool call should succeed; got events: {events:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sandbox_ipc_error_produces_tool_end_error_with_communication_message() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "err_call".into(),
                name: "broken".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Err(
            swage_core::SandboxError::IpcError { message: "pipe broken".to_string() },
        )));
        let tools: Vec<Arc<dyn crate::tool::Tool>> = vec![
            Arc::new(StubTool { tool_name: "broken" }) as Arc<dyn crate::tool::Tool>,
        ];
        let (state, session_id) = make_state_with_sandbox(vec![round_1, round_2], sandbox, tools).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use broken".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let tool_end = events
            .iter()
            .find(|e| matches!(e, ServerEvent::ToolEnd { call_id, .. } if call_id == "err_call"))
            .expect("expected ToolEnd for err_call");

        match tool_end {
            ServerEvent::ToolEnd { status, result, .. } => {
                assert_eq!(status, "error", "IPC error must produce error status");
                let msg = result.as_str().unwrap_or("");
                assert!(
                    msg.contains("sandbox communication error"),
                    "result should contain 'sandbox communication error', got: {msg}"
                );
            }
            _ => panic!("expected ToolEnd"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sandbox_tool_error_field_produces_tool_end_error() {
        // ToolCallResult.error = Some(...) means the sandboxed tool itself failed.
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "fail_call".into(),
                name: "fail_tool".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done { finish_reason: None },
        ];
        let round_2 = vec![
            StreamChunk::Delta { content: "done".into() },
            StreamChunk::Done { finish_reason: None },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Ok(
            swage_sandbox::ToolCallResult::failed("tool execution failed inside sandbox", 10),
        )));
        let tools: Vec<Arc<dyn crate::tool::Tool>> = vec![
            Arc::new(StubTool { tool_name: "fail_tool" }) as Arc<dyn crate::tool::Tool>,
        ];
        let (state, session_id) = make_state_with_sandbox(vec![round_1, round_2], sandbox, tools).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Use fail_tool".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let tool_end = events
            .iter()
            .find(|e| matches!(e, ServerEvent::ToolEnd { call_id, .. } if call_id == "fail_call"))
            .expect("expected ToolEnd for fail_call");

        match tool_end {
            ServerEvent::ToolEnd { status, result, .. } => {
                assert_eq!(status, "error");
                let msg = result.as_str().unwrap_or("");
                assert!(
                    msg.contains("tool execution failed"),
                    "result should contain the sandbox error message, got: {msg}"
                );
            }
            _ => panic!("expected ToolEnd"),
        }
    }

    // -----------------------------------------------------------------------
    // Turn ID correlation span test
    // -----------------------------------------------------------------------

    /// The `process_turn` function creates a tracing span named "turn" carrying
    /// `turn_id` and `session_id` fields so that all log entries within the turn
    /// are correlated by the same identifiers.
    ///
    /// Uses a custom `Layer` that records span metadata to an in-process buffer,
    /// avoiding external log capture and filesystem dependencies.
    #[test]
    fn process_turn_creates_correlation_span() {
        use std::sync::{Arc as StdArc, Mutex as StdMutex};
        use tracing_subscriber::{layer::SubscriberExt, Registry};

        // Accumulates (span_name, [field_names]) for every span created
        // during process_turn. Shared between the subscriber and the assertions.
        let recorded: StdArc<StdMutex<Vec<(String, Vec<String>)>>> =
            StdArc::new(StdMutex::new(Vec::new()));
        let captured = StdArc::clone(&recorded);

        // Minimal Layer that records span names and field names on creation.
        struct SpanFieldRecorder(StdArc<StdMutex<Vec<(String, Vec<String>)>>>);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanFieldRecorder {
            fn on_new_span(
                &self,
                attrs: &tracing::span::Attributes<'_>,
                _id: &tracing::span::Id,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let name = attrs.metadata().name().to_string();
                let mut fields = Vec::new();

                // Collect field names from metadata — this captures both
                // value-filled and Empty (deferred) fields, so turn_id is
                // visible even though it's recorded after span creation.
                attrs.metadata().fields().iter().for_each(|f| {
                    fields.push(f.name().to_string());
                });
                self.0.lock().unwrap().push((name, fields));
            }
        }

        let subscriber = Registry::default().with(SpanFieldRecorder(captured));

        // Scope the subscriber around the entire async execution via with_default.
        // This is more reliable than set_default inside #[tokio::test] because
        // with_default brackets the runtime creation itself, ensuring the subscriber
        // is active when tracing rebuilds callsite interest caches (which happens
        // when SCOPED_COUNT transitions 0→1). A gap between set_default and
        // process_turn can let concurrent tests drain SCOPED_COUNT to 0, causing
        // tracing to rebuild with a noop subscriber and cache "never" interest —
        // so spans are silently dropped before reaching our recorder.
        tracing::subscriber::with_default(subscriber, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let chunks = vec![
                        StreamChunk::Delta { content: "ok".to_string() },
                        StreamChunk::Done { finish_reason: None },
                    ];
                    let (state, session_id) = make_state_with_session(chunks).await;
                    let (tx, _rx) = mpsc::unbounded_channel();

                    // Re-seed callsite interest immediately before process_turn.
                    // In a parallel test run, concurrent tests can trigger
                    // rebuild_interest_cache() on their own threads (when their
                    // scoped subscriber guards drop), resetting our callsite's
                    // interest to "never". Calling it here — with our subscriber
                    // still active — re-establishes "sometimes" interest just
                    // before the span would be created.
                    tracing::callsite::rebuild_interest_cache();

                    process_turn(&state, session_id, "test".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
                        .await
                        .expect("turn should complete");
                });
        });

        let spans = recorded.lock().unwrap();
        let turn_spans: Vec<_> = spans.iter().filter(|(name, _)| name == "turn").collect();

        assert!(
            !turn_spans.is_empty(),
            "expected a 'turn' span during process_turn; saw spans: {:?}",
            spans.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        );

        let (_, fields) = &turn_spans[0];
        assert!(
            fields.contains(&"turn_id".to_string()),
            "turn span should have 'turn_id' field, got: {fields:?}",
        );
        assert!(
            fields.contains(&"session_id".to_string()),
            "turn span should have 'session_id' field, got: {fields:?}",
        );
    }

    // -----------------------------------------------------------------------
    // Token usage structured log tests
    // -----------------------------------------------------------------------

    /// Verifies that process_turn emits an INFO-level tracing event at turn
    /// completion that includes input_tokens, output_tokens, and
    /// cost_microcents as structured fields. This event extends the turn span
    /// with cost-visibility for operators reading JSON logs.
    #[test]
    fn process_turn_logs_token_usage_at_completion() {
        use std::sync::{Arc as StdArc, Mutex as StdMutex};
        use swage_core::TokenUsage;
        use tracing_subscriber::{layer::SubscriberExt, Registry};

        // Accumulates (level_str, [field_names]) for every event emitted
        // during process_turn. Shared between the layer and the assertions.
        let recorded: StdArc<StdMutex<Vec<(String, Vec<String>)>>> =
            StdArc::new(StdMutex::new(Vec::new()));
        let captured = StdArc::clone(&recorded);

        struct EventFieldRecorder(StdArc<StdMutex<Vec<(String, Vec<String>)>>>);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventFieldRecorder {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let level = event.metadata().level().to_string();
                let fields: Vec<String> = event
                    .metadata()
                    .fields()
                    .iter()
                    .map(|f| f.name().to_string())
                    .collect();
                self.0.lock().unwrap().push((level, fields));
            }
        }

        let subscriber = Registry::default().with(EventFieldRecorder(captured));

        tracing::subscriber::with_default(subscriber, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let chunks = vec![
                        StreamChunk::Delta { content: "ok".to_string() },
                        StreamChunk::Usage(TokenUsage { input_tokens: 100, output_tokens: 50 }),
                        StreamChunk::Done { finish_reason: None },
                    ];
                    let (state, session_id) = make_state_with_session(chunks).await;
                    let (tx, _rx) = mpsc::unbounded_channel();

                    tracing::callsite::rebuild_interest_cache();

                    process_turn(
                        &state,
                        session_id,
                        "test".to_string(),
                        tx,
                        Arc::new(Mutex::new(SubscriptionState::new(true))),
                    )
                    .await
                    .expect("turn should complete");
                });
        });

        let events = recorded.lock().unwrap();

        // Find an INFO event that carries token usage fields.
        let usage_events: Vec<_> = events
            .iter()
            .filter(|(level, fields)| {
                level == "INFO" && fields.contains(&"input_tokens".to_string())
            })
            .collect();

        assert!(
            !usage_events.is_empty(),
            "expected an INFO event with input_tokens field; got events: {events:?}",
        );

        let (_, fields) = &usage_events[0];
        assert!(
            fields.contains(&"output_tokens".to_string()),
            "usage event should have output_tokens field, got: {fields:?}",
        );
        assert!(
            fields.contains(&"cost_microcents".to_string()),
            "usage event should have cost_microcents field, got: {fields:?}",
        );
    }

    // -----------------------------------------------------------------------
    // Context assembly tests
    // -----------------------------------------------------------------------

    // Imports from super::* via `use super::*` — ContextElement, HistoryElement,
    // PromptsElement, TiktokenTokeniser, PromptRule are all available.

    /// Assembly from a PromptRule and HistoryElement produces a system message
    /// first, followed by history, with a report showing non-zero token counts.
    #[test]
    fn assembly_with_instructions_produces_system_message_first() {
        let rule = PromptRule {
            priority: 0,
            content: "You are a helpful coding assistant.".to_string(),
            source_level: swage_config::HierarchyLevel::Global,
            file_path: std::path::PathBuf::from("test.md"),
        };
        let prompts = PromptsElement::new(vec![rule]);
        let history = HistoryElement::new(vec![
            ChatMessage::new(Role::User, "Hello"),
        ]);

        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(prompts),
            Box::new(history),
        ];

        let config = swage_config::ContextConfig::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

        let output = swage_context::assembly::default_assembly(
            &elements, &config, &tokeniser,
        );

        // System message from instructions appears first.
        assert_eq!(
            output.messages[0].role, Role::System,
            "first message should be system instructions"
        );
        // User message from history follows.
        assert_eq!(
            output.messages[1].role, Role::User,
            "second message should be user history"
        );

        // Report should have non-zero token counts for both elements.
        let prompts_report = output.report.elements.iter()
            .find(|e| e.name == "prompts")
            .expect("report should include prompts element");
        assert!(
            prompts_report.token_count > 0,
            "prompts element should have non-zero token count"
        );

        let history_report = output.report.elements.iter()
            .find(|e| e.name == "history")
            .expect("report should include history element");
        assert!(
            history_report.token_count > 0,
            "history element should have non-zero token count"
        );
    }

    /// Assembly with empty instructions produces no system message — only
    /// history messages.
    #[test]
    fn assembly_without_instructions_omits_system_message() {
        let prompts = PromptsElement::new(vec![]);
        let history = HistoryElement::new(vec![
            ChatMessage::new(Role::User, "Hello"),
        ]);

        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(prompts),
            Box::new(history),
        ];

        let config = swage_config::ContextConfig::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

        let output = swage_context::assembly::default_assembly(
            &elements, &config, &tokeniser,
        );

        // No system message when instructions are empty.
        assert!(
            output.messages.iter().all(|m| m.role != Role::System),
            "expected no system message when prompts are empty"
        );
        assert_eq!(output.messages.len(), 1, "should only contain the user message");
    }

    /// Constructing a PromptRule from the config.instructions string produces
    /// a PromptsElement that generates equivalent output to the old flat prepend.
    #[test]
    fn prompt_rule_from_instructions_string_matches_flat_prepend() {
        let instructions = "You are a helpful assistant.\n\nBe concise.";

        // Old approach: flat system message prepend.
        let flat_system = ChatMessage::new(Role::System, instructions);

        // New approach: PromptRule → PromptsElement → assembly.
        let rule = PromptRule {
            priority: 0,
            content: instructions.to_string(),
            source_level: swage_config::HierarchyLevel::Global,
            file_path: std::path::PathBuf::from("instructions"),
        };
        let prompts = PromptsElement::new(vec![rule]);
        let content = prompts.content();

        assert_eq!(content.len(), 1, "should produce exactly one system message");
        assert_eq!(content[0].role, flat_system.role);
        // Content should be identical — single rule, no joining needed.
        assert_eq!(content[0].content, flat_system.content);
    }

    /// The build_assembly_elements helper constructs the correct elements for
    /// the turn's context assembly. This function is tested directly to verify
    /// the wiring before it's used in process_turn.
    #[test]
    fn build_assembly_elements_creates_prompts_skills_memory_and_history() {
        let instructions = "Be helpful.";
        let history = vec![
            ChatMessage::new(Role::User, "Hi"),
            ChatMessage::new(Role::Assistant, "Hello!"),
        ];

        let elements = build_assembly_elements(
            instructions,
            vec![],
            history,
            String::new(),
            "My Project".to_string(),
        );

        // Should produce four elements: prompts, skills (empty), memory, and history.
        assert_eq!(elements.len(), 4);

        let names: Vec<&str> = elements.iter().map(|e| e.name()).collect();
        assert!(names.contains(&"prompts"), "should include prompts element");
        assert!(names.contains(&"skills"), "should include skills element");
        assert!(names.contains(&"memory"), "should include memory element");
        assert!(names.contains(&"history"), "should include history element");
    }

    /// When instructions are empty, build_assembly_elements still includes the
    /// prompts element (which will produce no system message), the skills
    /// element, memory, and history.
    #[test]
    fn build_assembly_elements_with_empty_instructions() {
        let elements = build_assembly_elements(
            "",
            vec![],
            vec![ChatMessage::new(Role::User, "Hello")],
            String::new(),
            "My Project".to_string(),
        );

        assert_eq!(elements.len(), 4);
        // PromptsElement with empty rules produces no messages.
        let prompts = elements.iter().find(|e| e.name() == "prompts").unwrap();
        assert!(prompts.content().is_empty());
    }

    /// When memory_content is non-empty, the memory element produces a system
    /// message containing that content.
    #[test]
    fn build_assembly_elements_memory_element_contains_provided_content() {
        let elements = build_assembly_elements(
            "Be helpful.",
            vec![],
            vec![],
            "Important project context".to_string(),
            "My Project".to_string(),
        );

        let memory = elements.iter().find(|e| e.name() == "memory").unwrap();
        let messages = memory.content();
        assert_eq!(messages.len(), 1, "non-empty memory should produce one message");
        // content is Vec<ContentPart> — extract the text to check the value.
        let text = messages[0].content.iter().find_map(|p| {
            if let swage_core::ContentPart::Text { text } = p { Some(text.as_str()) } else { None }
        });
        assert!(
            text.is_some_and(|t| t.contains("Important project context")),
            "memory message should contain the provided content"
        );
    }

    /// load_project_memory returns an empty string when the project has no
    /// memory directory.
    #[test]
    fn load_project_memory_returns_empty_when_dir_absent() {
        let dir = tempfile::tempdir().unwrap();
        // No memory directory created — the helper should not panic.
        let result = load_project_memory(dir.path(), "no-such-project");
        assert!(result.is_empty(), "should return empty string when memory dir is absent");
    }

    /// load_project_memory reads all .md files from the memory directory and
    /// joins them in alphabetical filename order.
    #[test]
    fn load_project_memory_concatenates_md_files_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir.path().join("projects").join("my-project").join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();

        // b_notes sorts after a_context.
        std::fs::write(memory_dir.join("b_notes.md"), "Second file content").unwrap();
        std::fs::write(memory_dir.join("a_context.md"), "First file content").unwrap();
        // Non-.md file must not be included.
        std::fs::write(memory_dir.join("other.txt"), "Should be ignored").unwrap();

        let result = load_project_memory(dir.path(), "my-project");

        assert!(result.contains("First file content"), "should include first .md file");
        assert!(result.contains("Second file content"), "should include second .md file");
        assert!(!result.contains("Should be ignored"), "should not include non-.md files");

        // a_context.md sorts before b_notes.md alphabetically.
        let first_pos = result.find("First file content").unwrap();
        let second_pos = result.find("Second file content").unwrap();
        assert!(first_pos < second_pos, "files should appear in alphabetical order");
    }

    /// load_project_memory skips files with only whitespace content.
    #[test]
    fn load_project_memory_skips_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir.path().join("projects").join("my-project").join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();

        std::fs::write(memory_dir.join("empty.md"), "   \n  ").unwrap();
        std::fs::write(memory_dir.join("content.md"), "Real content").unwrap();

        let result = load_project_memory(dir.path(), "my-project");

        assert!(result.contains("Real content"));
        assert!(!result.contains("   "), "whitespace-only files should be excluded");
    }

    // -----------------------------------------------------------------------
    // Token usage persistence tests
    // -----------------------------------------------------------------------

    /// When the provider returns a Usage chunk, the turn record should have
    /// non-null input_tokens and output_tokens in storage after completion.
    #[tokio::test]
    async fn token_usage_persisted_to_turn_record() {
        use swage_core::TokenUsage;

        let chunks = vec![
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Usage(TokenUsage { input_tokens: 42, output_tokens: 17 }),
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns.len(), 1);

        let turn = &turns[0];
        assert_eq!(
            turn.input_tokens, Some(42),
            "input_tokens should be persisted from provider usage"
        );
        assert_eq!(
            turn.output_tokens, Some(17),
            "output_tokens should be persisted from provider usage"
        );
    }

    /// When the provider does not return a Usage chunk, token fields should
    /// remain NULL (None) in the turn record.
    #[tokio::test]
    async fn token_usage_remains_null_when_provider_sends_no_usage() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(&state, session_id, "Hi".to_string(), tx, Arc::new(Mutex::new(SubscriptionState::new(true))))
            .await
            .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();

        // The agent always persists token counts — zero when the provider
        // sends no Usage chunk (the old gateway left them NULL, the agent
        // stores the accumulated total which starts at 0).
        let turn = &turns[0];
        assert_eq!(
            turn.input_tokens, Some(0),
            "input_tokens should be zero when no usage reported"
        );
        assert_eq!(
            turn.output_tokens, Some(0),
            "output_tokens should be zero when no usage reported"
        );
    }

    // -----------------------------------------------------------------------
    // ContextAssembled event and snapshot persistence tests
    // -----------------------------------------------------------------------

    /// ContextAssembled must be the first event before any Token events.
    #[tokio::test]
    async fn context_assembled_precedes_first_token() {
        let chunks = vec![
            StreamChunk::Delta { content: "Hello".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
        )
        .await
        .expect("turn should complete");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Find positions of ContextAssembled and first Token.
        let assembled_pos = events
            .iter()
            .position(|e| matches!(e, ServerEvent::ContextAssembled { .. }))
            .expect("expected a ContextAssembled event");
        let first_token_pos = events
            .iter()
            .position(|e| matches!(e, ServerEvent::Token { .. }))
            .expect("expected at least one Token event");

        assert!(
            assembled_pos < first_token_pos,
            "ContextAssembled (pos {assembled_pos}) must precede first Token (pos {first_token_pos})"
        );

        // Verify the event carries the correct turn_id.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let turns = state
            .storage
            .list_turns_by_thread(threads[0].id)
            .await
            .unwrap();
        let turn_id = turns[0].id;

        match &events[assembled_pos] {
            ServerEvent::ContextAssembled {
                turn_id: event_turn_id,
                report,
            } => {
                assert_eq!(*event_turn_id, turn_id, "turn_id should match");
                // Report should have an "elements" array from the assembly engine.
                assert!(
                    report.get("elements").is_some(),
                    "report should contain elements key"
                );
            }
            other => panic!("expected ContextAssembled, got: {other:?}"),
        }
    }

    /// The assembly report should be persisted as a context snapshot.
    #[tokio::test]
    async fn assembly_report_persisted_as_context_snapshot() {
        use swage_storage::store::ContextSnapshotStore;

        let chunks = vec![
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
        )
        .await
        .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let turns = state
            .storage
            .list_turns_by_thread(threads[0].id)
            .await
            .unwrap();
        let turn_id = turns[0].id;

        let snapshot = state
            .storage
            .get_snapshot(turn_id)
            .await
            .expect("get_snapshot should succeed");

        assert!(
            snapshot.is_some(),
            "context snapshot should be persisted for the turn"
        );

        // Verify it parses as valid JSON with the expected structure.
        let parsed: serde_json::Value =
            serde_json::from_str(&snapshot.unwrap()).expect("snapshot should be valid JSON");
        assert!(
            parsed.get("elements").is_some(),
            "snapshot should contain elements key"
        );
    }

    /// When subscription is off, ContextAssembled is not delivered but
    /// snapshot persistence still happens.
    #[tokio::test]
    async fn context_assembled_not_sent_when_unsubscribed() {
        use swage_storage::store::ContextSnapshotStore;

        let chunks = vec![
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = false, no channels subscribed.
        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
        )
        .await
        .expect("turn should complete");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // No ContextAssembled in the event stream.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ServerEvent::ContextAssembled { .. })),
            "ContextAssembled should not be sent when unsubscribed"
        );

        // But snapshot should still be persisted.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let turns = state
            .storage
            .list_turns_by_thread(threads[0].id)
            .await
            .unwrap();
        let snapshot = state
            .storage
            .get_snapshot(turns[0].id)
            .await
            .expect("get_snapshot should succeed");
        assert!(
            snapshot.is_some(),
            "snapshot should still be persisted even without subscription"
        );
    }

    // -----------------------------------------------------------------------
    // TurnComplete usage field tests
    // -----------------------------------------------------------------------

    /// When subscribed to context channel, TurnComplete includes token usage.
    #[tokio::test]
    async fn turn_complete_includes_usage_when_context_subscribed() {
        use swage_core::TokenUsage;

        let chunks = vec![
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Usage(TokenUsage { input_tokens: 100, output_tokens: 50 }),
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = true means all channels are subscribed.
        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
        )
        .await
        .expect("turn should complete");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        let complete = events
            .iter()
            .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
            .expect("expected TurnComplete event");

        match complete {
            ServerEvent::TurnComplete {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(*input_tokens, Some(100), "input_tokens should be populated");
                assert_eq!(*output_tokens, Some(50), "output_tokens should be populated");
            }
            other => panic!("expected TurnComplete, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Output schema wiring tests
    // -----------------------------------------------------------------------

    /// When the config has an output schema and the provider returns valid JSON,
    /// TurnComplete carries structured_output and the turn record is persisted.
    #[tokio::test]
    async fn turn_complete_carries_structured_output_when_schema_passes() {
        let _guard = SwageHomeGuard::isolate();
        use swage_core::FinishReason;
        use swage_config::OutputConfig;

        let chunks = vec![
            StreamChunk::Delta { content: r#"{"result":"ok"}"#.to_string() },
            StreamChunk::Done { finish_reason: Some(FinishReason::Stop) },
        ];

        // Build a config with an output schema requiring {"result": <string>}.
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "result": { "type": "string" } },
            "required": ["result"]
        });
        let mut config = swage_config::ResolvedConfig::default();
        config.output = OutputConfig { schema: Some(schema), max_retries: 1 };

        let (state, session_id) = make_state_with_config(chunks, config).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            "give me json".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
        )
        .await
        .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let complete = events
            .iter()
            .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
            .expect("expected TurnComplete");

        match complete {
            ServerEvent::TurnComplete { structured_output, .. } => {
                assert!(
                    structured_output.is_some(),
                    "TurnComplete should carry structured_output when schema passes"
                );
                assert_eq!(
                    structured_output.as_ref().unwrap()["result"],
                    "ok"
                );
            }
            other => panic!("expected TurnComplete, got: {other:?}"),
        }
    }

    /// After a valid schema turn, the turn record in storage has structured_output.
    #[tokio::test]
    async fn structured_output_persisted_on_turn_after_schema_validation() {
        let _guard = SwageHomeGuard::isolate();
        use swage_core::FinishReason;
        use swage_config::OutputConfig;
        use swage_storage::store::TurnStore;

        let chunks = vec![
            StreamChunk::Delta { content: r#"{"x":42}"#.to_string() },
            StreamChunk::Done { finish_reason: Some(FinishReason::Stop) },
        ];

        let schema = serde_json::json!({
            "type": "object",
            "properties": { "x": { "type": "integer" } },
            "required": ["x"]
        });
        let mut config = swage_config::ResolvedConfig::default();
        config.output = OutputConfig { schema: Some(schema), max_retries: 1 };

        let (state, session_id) = make_state_with_config(chunks, config).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            "give me x".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
        )
        .await
        .expect("turn should complete");

        // Drain events to let the turn finish.
        while rx.try_recv().is_ok() {}

        // Look up the turn in storage and verify structured_output was persisted.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = TurnStore::list_turns_by_thread(&*state.storage, thread_id).await.unwrap();
        assert_eq!(turns.len(), 1);
        assert!(
            turns[0].structured_output.is_some(),
            "turn record should have structured_output persisted"
        );
        assert_eq!(turns[0].structured_output.as_ref().unwrap()["x"], 42);
    }

    /// When NOT subscribed to context channel, TurnComplete omits usage fields.
    #[tokio::test]
    async fn turn_complete_omits_usage_when_not_subscribed() {
        use swage_core::TokenUsage;

        let chunks = vec![
            StreamChunk::Delta { content: "Hi".to_string() },
            StreamChunk::Usage(TokenUsage { input_tokens: 100, output_tokens: 50 }),
            StreamChunk::Done { finish_reason: None },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = false, no subscriptions.
        process_turn(
            &state,
            session_id,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
        )
        .await
        .expect("turn should complete");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        let complete = events
            .iter()
            .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
            .expect("expected TurnComplete event");

        match complete {
            ServerEvent::TurnComplete {
                input_tokens,
                output_tokens,
                cost_microcents,
                ..
            } => {
                assert_eq!(*input_tokens, None, "input_tokens should be None when unsubscribed");
                assert_eq!(*output_tokens, None, "output_tokens should be None when unsubscribed");
                assert_eq!(*cost_microcents, None, "cost_microcents should be None when unsubscribed");
            }
            other => panic!("expected TurnComplete, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Approval event routing tests [byi8]
    // -----------------------------------------------------------------------

    #[test]
    fn approval_required_routes_to_ws_tx() {
        // When the agent emits ApprovalRequired, the session-scoped ws_tx
        // should receive the corresponding ServerEvent — not the broadcast channel.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let approval_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();

        route_approval_agent_event(
            AgentEvent::ApprovalRequired {
                approval_id,
                turn_id,
                agent_id: "agent-1".to_string(),
                kind: "tool".to_string(),
                tool_name: Some("shell".to_string()),
                tool_arguments: Some(serde_json::json!({"cmd": "ls"})),
                hook_name: None,
                prompt: "Allow shell?".to_string(),
                context: None,
                timeout_at: "2026-01-01T00:00:00Z".to_string(),
                timeout_policy: "deny".to_string(),
            },
            &tx,
        );

        match rx.try_recv().expect("ws_tx must receive the event") {
            ServerEvent::ApprovalRequired { approval_id: aid, turn_id: tid, agent_id, kind, tool_name, .. } => {
                assert_eq!(aid, approval_id.to_string());
                assert_eq!(tid, turn_id.to_string());
                assert_eq!(agent_id, "agent-1");
                assert_eq!(kind, "tool");
                assert_eq!(tool_name, Some("shell".to_string()));
            }
            other => panic!("expected ApprovalRequired, got: {other:?}"),
        }
    }

    #[test]
    fn approval_confirmed_routes_to_ws_tx() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let approval_id = Uuid::new_v4();

        route_approval_agent_event(
            AgentEvent::ApprovalConfirmed {
                approval_id,
                decision: "approved".to_string(),
                reason: Some("looks safe".to_string()),
            },
            &tx,
        );

        match rx.try_recv().expect("ws_tx must receive the event") {
            ServerEvent::ApprovalConfirmed { approval_id: aid, decision, reason } => {
                assert_eq!(aid, approval_id.to_string());
                assert_eq!(decision, "approved");
                assert_eq!(reason, Some("looks safe".to_string()));
            }
            other => panic!("expected ApprovalConfirmed, got: {other:?}"),
        }
    }

    #[test]
    fn approval_expired_routes_to_ws_tx() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let approval_id = Uuid::new_v4();

        route_approval_agent_event(
            AgentEvent::ApprovalExpired {
                approval_id,
                timeout_policy: "deny".to_string(),
            },
            &tx,
        );

        match rx.try_recv().expect("ws_tx must receive the event") {
            ServerEvent::ApprovalExpired { approval_id: aid, timeout_policy } => {
                assert_eq!(aid, approval_id.to_string());
                assert_eq!(timeout_policy, "deny");
            }
            other => panic!("expected ApprovalExpired, got: {other:?}"),
        }
    }

    #[test]
    fn approval_events_routed_to_ws_tx_not_broadcast() {
        // Two separate mpsc channels simulating two different sessions.
        // Only the channel passed to route_approval_agent_event should receive
        // the event — the other remains empty, proving session-scoped routing.
        let (tx_a, mut rx_a) = mpsc::unbounded_channel::<ServerEvent>();
        let (tx_b, mut rx_b) = mpsc::unbounded_channel::<ServerEvent>();

        route_approval_agent_event(
            AgentEvent::ApprovalRequired {
                approval_id: Uuid::new_v4(),
                turn_id: Uuid::new_v4(),
                agent_id: "agent-a".to_string(),
                kind: "tool".to_string(),
                tool_name: None,
                tool_arguments: None,
                hook_name: None,
                prompt: "Allow?".to_string(),
                context: None,
                timeout_at: "2026-01-01T00:00:00Z".to_string(),
                timeout_policy: "deny".to_string(),
            },
            &tx_a, // only agent-a's channel
        );

        // agent-a's client should receive the event.
        assert!(
            rx_a.try_recv().is_ok(),
            "agent-a's ws_tx must receive the ApprovalRequired event"
        );
        // agent-b's client must NOT receive it — routing is session-scoped.
        assert!(
            rx_b.try_recv().is_err(),
            "agent-b's ws_tx must not receive agent-a's approval event"
        );
    }
}
