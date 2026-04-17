use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_core::{ChatMessage, GatewayError, Role};
use swage_hook::{HookEvent, HookOutcome, HookRegistry};
use swage_storage::model::{AppendMessage, InboundAudit, NewAgentMessage, NewTurn, TurnState};

use swage_config::{HierarchyLevel, prompt_rule::PromptRule};
use swage_context::element::ContextElement;
use swage_context::history::HistoryElement;
use swage_context::memory::MemoryElement;
use swage_context::prompts::PromptsElement;
use swage_context::skills::{ActiveSkill, SkillsElement};
use swage_context::tokeniser::TiktokenTokeniser;

use swage_agent::{
    AgentBuilder, AgentConfig, AgentError, AgentEvent, DelegationExecutor, DelegationHandler,
    MessageRouter, TurnRequest,
};
use swage_safety::LeakDetector;
use swage_tool::dispatch::ToolDispatcher;
use swage_tool::permission::PermissionConfig;
use swage_tool::permission::RequireApprovalEntry as ToolRequireApprovalEntry;
use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};

use crate::skill::SkillManager;
use crate::tool_executor::{GatewayExecutionStrategy, SandboxedExecutor};

use crate::{state::AppState, subscription::SubscriptionState, websocket::ServerEvent};
// Gateway messaging spans three surfaces: REST endpoints for durable messages
// and turn submission, session-scoped WebSocket events for live approval and
// delegation updates, and in-process routing between running agents.

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
pub(crate) fn build_assembly_elements(
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
pub(crate) async fn load_project_memory(swage_root: &Path, project_slug: &str) -> String {
    let memory_dir = swage_root
        .join("projects")
        .join(project_slug)
        .join("memory");

    let mut entries = match tokio::fs::read_dir(&memory_dir).await {
        Ok(rd) => rd,
        // Missing directory is the common case for new projects.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
        Err(error) => {
            tracing::warn!(
                path = %memory_dir.display(),
                %error,
                "failed to read project memory directory"
            );
            return String::new();
        }
    };

    // Collect .md paths and sort alphabetically so loading order is stable.
    let mut paths: Vec<PathBuf> = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                    paths.push(path);
                }
            }
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    path = %memory_dir.display(),
                    %error,
                    "failed to enumerate project memory directory"
                );
                return String::new();
            }
        }
    }
    paths.sort();

    let mut parts = Vec::new();
    for path in paths {
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                if !content.trim().is_empty() {
                    parts.push(content);
                }
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to read project memory file");
            }
        }
    }

    parts.join("\n\n")
}

pub(crate) fn build_approval_registrar(
    approval_channels: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                Uuid,
                tokio::sync::oneshot::Sender<swage_agent::ApprovalDecision>,
            >,
        >,
    >,
) -> swage_agent::ApprovalChannelRegistrar {
    Arc::new(move |approval_id, sender| {
        approval_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(approval_id, sender);
    })
}

fn build_permission_config(
    config: &swage_config::ResolvedConfig,
    startup_config: &swage_config::ResolvedConfig,
) -> PermissionConfig {
    let mut deny = config.tools.permissions.deny.clone();
    for entry in &startup_config.tools.permissions.deny {
        if !deny.contains(entry) {
            deny.push(entry.clone());
        }
    }

    PermissionConfig {
        allow: config.tools.permissions.allow.clone(),
        deny,
        bundles: config.tools.permissions.bundles.clone(),
        require_approval: config
            .tools
            .permissions
            .require_approval
            .iter()
            .map(|entry| match entry {
                swage_config::model::RequireApprovalEntry::Simple(tool_name) => {
                    ToolRequireApprovalEntry {
                        tool_name: tool_name.clone(),
                        timeout_secs: Some(config.hitl.timeout),
                        timeout_policy: Some(config.hitl.timeout_policy.clone()),
                    }
                }
                swage_config::model::RequireApprovalEntry::WithOptions {
                    tool,
                    timeout,
                    timeout_policy,
                } => ToolRequireApprovalEntry {
                    tool_name: tool.clone(),
                    timeout_secs: Some(timeout.unwrap_or(config.hitl.timeout)),
                    timeout_policy: Some(
                        timeout_policy
                            .clone()
                            .unwrap_or_else(|| config.hitl.timeout_policy.clone()),
                    ),
                },
            })
            .collect(),
    }
}

pub(crate) fn build_gateway_dispatcher(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
) -> ToolDispatcher {
    let permissions = build_permission_config(config, &state.config);

    let dispatcher = ToolDispatcher::new(
        state.tool_dispatcher.registry().clone(),
        permissions,
        Arc::new(RateLimiter::new()),
        RateLimitConfig::default(),
        Arc::new(LeakDetector::default()),
        false,
    );

    let inner_strategy: Option<Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>> =
        state.sandbox.as_ref().map(|sandbox| {
            Arc::new(SandboxedExecutor::new(sandbox.clone()))
                as Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>
        });

    dispatcher.with_execution_strategy(Arc::new(GatewayExecutionStrategy::new(
        inner_strategy,
        ws_tx,
    )))
}

pub(crate) fn build_agent_config(
    agent_id: String,
    config: &swage_config::ResolvedConfig,
    output_schema_override: Option<serde_json::Value>,
) -> AgentConfig {
    let mut agent_config = AgentConfig::new(agent_id);
    if let Some(schema) = output_schema_override.or_else(|| config.output.schema.clone()) {
        agent_config = agent_config.with_output_schema(schema, config.output.max_retries);
    }
    agent_config
}

fn load_session_config(
    state: &AppState,
    project_slug: &str,
    agent_id: &str,
) -> Result<swage_config::ResolvedConfig, GatewayError> {
    let Ok(root) = swage_config::discovery::resolve_root() else {
        return Ok(state.config.clone());
    };

    let project_dir = root.join("projects").join(project_slug);
    let project_config_exists = project_dir.join("config.toml").exists();
    let agent_config_exists = project_dir
        .join("agents")
        .join(agent_id)
        .join("config.toml")
        .exists();

    if !project_config_exists && !agent_config_exists {
        return Ok(state.config.clone());
    }

    swage_config::load_resolved_config_from(&root, project_slug, agent_id)
        .map_err(|error| GatewayError::ConfigError(error.to_string()))
}

#[derive(Debug, Clone)]
pub(crate) struct BeforeInboundDispatchResult {
    pub canonical_content: String,
    pub modified_by_hooks: Vec<String>,
}

fn map_hook_block(reason: String) -> GatewayError {
    GatewayError::ConfigError(format!(
        "BeforeInbound hook blocked inbound content: {reason}"
    ))
}

pub(crate) async fn dispatch_before_inbound(
    hook_registry: Option<&Arc<HookRegistry>>,
    raw_content: &str,
) -> Result<BeforeInboundDispatchResult, GatewayError> {
    let Some(hook_registry) = hook_registry else {
        return Ok(BeforeInboundDispatchResult {
            canonical_content: raw_content.to_string(),
            modified_by_hooks: Vec::new(),
        });
    };

    let traced = hook_registry
        .dispatch_traced(HookEvent::BeforeInbound {
            content: raw_content.to_string(),
        })
        .await;

    match traced.outcome {
        HookOutcome::Continue => Ok(BeforeInboundDispatchResult {
            canonical_content: raw_content.to_string(),
            modified_by_hooks: traced.modified_by_hooks,
        }),
        HookOutcome::ContinueModified(modified) => match *modified {
            HookEvent::BeforeInbound { content } => Ok(BeforeInboundDispatchResult {
                canonical_content: content,
                modified_by_hooks: traced.modified_by_hooks,
            }),
            _ => Ok(BeforeInboundDispatchResult {
                canonical_content: raw_content.to_string(),
                modified_by_hooks: traced.modified_by_hooks,
            }),
        },
        HookOutcome::Block(reason) => Err(map_hook_block(reason)),
        HookOutcome::RequestApproval { .. } => Err(GatewayError::ConfigError(
            "BeforeInbound hooks cannot request approval".to_string(),
        )),
    }
}

fn resolve_swage_root() -> Option<PathBuf> {
    match swage_config::discovery::resolve_root() {
        Ok(root) => Some(root),
        Err(error) => {
            tracing::warn!(
                error = %error.kind,
                "failed to resolve swage root; proceeding without skills, memory, or custom assembly"
            );
            None
        }
    }
}

#[derive(Clone)]
pub(crate) struct GatewayDelegationHandler {
    state: AppState,
    active_delegations: swage_agent::ActiveDelegations,
}

impl GatewayDelegationHandler {
    pub(crate) fn new(state: AppState) -> Self {
        Self {
            state,
            active_delegations: swage_agent::active_delegations(),
        }
    }
}

#[async_trait::async_trait]
impl DelegationHandler for GatewayDelegationHandler {
    async fn handle_delegation(
        &self,
        target_agent_id: &str,
        task: &str,
        context: Option<serde_json::Value>,
        sender_agent_id: &str,
        sender_turn_id: Uuid,
        project_id: Uuid,
        cancel_token: CancellationToken,
    ) -> Result<serde_json::Value, AgentError> {
        let project = self
            .state
            .project_store
            .get(project_id)
            .await?
            .ok_or_else(|| {
                AgentError::InvalidConfig(format!(
                    "delegation project {project_id} no longer exists"
                ))
            })?;

        let target_resolved = swage_config::load_resolved_config(&project.slug, target_agent_id)
            .map_err(|error| {
                AgentError::InvalidConfig(format!(
                    "failed to load config for delegated agent '{target_agent_id}' in project '{}': {error}",
                    project.slug
                ))
            })?;

        let target_agent_config =
            build_agent_config(target_agent_id.to_string(), &target_resolved, None);
        let target_context_transfer_config = target_resolved.communication.context_transfer.clone();

        // Delegated child agents execute within the gateway runtime, but their
        // internal tool activity is not surfaced on the parent conversation's
        // WebSocket stream.
        let (silent_ws_tx, _silent_ws_rx) = mpsc::unbounded_channel();
        let target_dispatcher =
            build_gateway_dispatcher(&self.state, &target_resolved, silent_ws_tx);

        let router = Arc::new(MessageRouter::new(self.state.storage.clone(), None));
        let executor = DelegationExecutor {
            storage: self.state.storage.clone(),
            provider: self.state.provider.clone(),
            hook_registry: self.state.hook_registry.clone(),
            leak_detector: None,
            approval_registrar: Some(build_approval_registrar(
                self.state.approval_channels.clone(),
            )),
            delegation_handler: Some(Arc::new(self.clone())),
            router: router.clone(),
            active_delegations: self.active_delegations.clone(),
        };

        let mut payload = serde_json::json!({
            "task": task,
        });
        if let Some(context) = context {
            payload["context"] = context;
        }

        let delegate_msg = router
            .send_message(NewAgentMessage {
                sender_agent_id: sender_agent_id.to_string(),
                recipient_agent_id: target_agent_id.to_string(),
                kind: swage_core::MessageKind::Delegate,
                correlation_id: None,
                payload,
            })
            .await?;

        router.deliver_message(&delegate_msg).await?;

        let child_cancel_token = cancel_token.child_token();
        let result_msg = match executor
            .execute_delegation(
                &delegate_msg,
                sender_turn_id,
                project_id,
                target_agent_config,
                target_context_transfer_config,
                Some(target_dispatcher),
                child_cancel_token,
            )
            .await
        {
            Ok(result_msg) => result_msg,
            Err(error) => {
                let _ = router
                    .fail_message(delegate_msg.id, &error.to_string())
                    .await;
                return Err(error);
            }
        };

        router.deliver_message(&result_msg).await?;
        Ok(result_msg.payload.clone())
    }
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
pub async fn process_turn(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    content: String,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn(
        state,
        session_id,
        target_thread_id,
        content,
        ws_tx,
        subscription,
        output_schema_override,
        None,
    )
    .await
}

#[tracing::instrument(
    name = "turn",
    skip(state, content, ws_tx, subscription),
    fields(turn_id = tracing::field::Empty),
)]
pub(crate) async fn process_turn_with_existing_turn(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    content: String,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    existing_turn_id: Option<Uuid>,
) -> Result<(), GatewayError> {
    // Step 1: Resolve the project and target thread for this session.
    let session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let thread = crate::thread_selection::resolve_thread_for_session(
        state.storage.as_ref(),
        session_id,
        target_thread_id,
    )
    .await?;
    let thread_id = thread.id;

    // Step 2: Guard — reject if a turn is already active for this thread.
    // Pending, Processing, AwaitingApproval, and Delegating all represent
    // in-progress work; allowing a second turn would interleave history.
    let existing_turns = state.storage.list_turns_by_thread(thread_id).await?;
    if existing_turns.iter().any(|t| {
        if existing_turn_id.is_some_and(|current_turn_id| t.id == current_turn_id) {
            return false;
        }
        matches!(
            t.state,
            TurnState::Pending
                | TurnState::Processing
                | TurnState::AwaitingApproval
                | TurnState::Delegating
        )
    }) {
        return Err(GatewayError::TurnInProgress { session_id });
    }

    // Step 3: Resolve the project for this session so we can load per-project config
    // and build the ToolContext passed to each tool invocation.
    let project_id = session.project_id;

    let project = state
        .project_store
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
    let project_slug = project.slug.clone();

    // Load project-specific config. Missing project config falls back to the
    // startup config, but present invalid config fails the turn before any
    // user message is written.
    let config = load_session_config(state, &project_slug, &session.agent_id)?;

    // Resolve the swage root once — skill discovery, project memory loading,
    // and assembly search-path construction all depend on it.
    //
    // Fail-open: a warning is logged and each consumer falls back gracefully.
    let swage_root_opt = resolve_swage_root();

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
        let agent_dir_opt = if agent_dir.is_dir() {
            Some(agent_dir.as_path())
        } else {
            None
        };
        let (available, _warnings) =
            SkillManager::discover_available(root, &project_dir, agent_dir_opt);

        // Ensure agent-level skills are activated for this session.
        // Idempotent — INSERT OR IGNORE in storage means repeated calls
        // on subsequent turns are harmless.
        if let Err(e) =
            SkillManager::auto_activate_agent_skills(session_id, &available, state.storage.as_ref())
                .await
        {
            tracing::warn!(
                error = ?e,
                "failed to auto-activate agent-level skills; proceeding"
            );
        }

        match SkillManager::build_active_skills(session_id, &available, state.storage.as_ref())
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

    // Step 4: Ensure the turn exists before any hook execution so the raw
    // input is recorded durably even when inbound processing blocks the turn.
    let prepared_turn_id = match existing_turn_id {
        Some(turn_id) => turn_id,
        None => {
            state
                .storage
                .create_turn(NewTurn {
                    thread_id,
                    agent_id: session.agent_id.clone(),
                    state: TurnState::Pending,
                    user_input: Some(content.clone()),
                    parent_turn_id: None,
                })
                .await?
                .id
        }
    };

    // Step 5: Set up the cancellation token before any turn processing begins.
    let cancel_token = CancellationToken::new();
    {
        let mut turns = state.active_turns.lock().await;
        turns.insert(session_id, cancel_token.clone());
    }

    // Step 6: Canonicalise inbound content before persisting any thread message.
    let before_inbound = match dispatch_before_inbound(state.hook_registry.as_ref(), &content).await
    {
        Ok(result) => result,
        Err(error) => {
            state
                .storage
                .update_turn_state(prepared_turn_id, TurnState::Failed)
                .await?;
            let mut turns = state.active_turns.lock().await;
            turns.remove(&session_id);
            return Err(error);
        }
    };

    if !before_inbound.modified_by_hooks.is_empty() {
        state
            .storage
            .update_inbound_audit(
                prepared_turn_id,
                &InboundAudit {
                    canonical_input: before_inbound.canonical_content.clone(),
                    modified_by_hooks: before_inbound.modified_by_hooks.clone(),
                },
            )
            .await?;
    }

    state
        .storage
        .append_message(AppendMessage {
            thread_id,
            turn_id: Some(prepared_turn_id),
            role: Role::User,
            content: before_inbound.canonical_content.clone(),
            agent_id: session.agent_id.clone(),
        })
        .await?;

    // Steps 8–9: Assemble context via the swage-context assembly engine.
    // Loads thread history, project memory, and constructs typed elements.
    // Runs fail-open assembly: custom Python configs when configured, built-in
    // default otherwise. The LLM provider is passed for two-phase compaction.
    let stored_history = state.storage.list_messages_by_thread(thread_id).await?;
    let history_messages: Vec<ChatMessage> = stored_history
        .into_iter()
        .map(|m| ChatMessage::new(m.role, m.content))
        .collect();

    // Load project memory from the filesystem. Returns an empty string when
    // no memory directory or files exist — MemoryElement handles empty content.
    let memory_content = match swage_root_opt.as_deref() {
        Some(root) => load_project_memory(root, &project_slug).await,
        None => String::new(),
    };

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

    // Run the fail-open assembly orchestrator. For "default" (or empty)
    // assembly names this delegates to default_assembly() synchronously.
    // Named Python assemblies now execute in a child worker instead of an
    // embedded interpreter.
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
    let report_json = serde_json::to_value(&assembly_report).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to serialise assembly report");
        serde_json::json!({ "error": "serialisation failed" })
    });

    // Step 9: Resolve tool permissions from config cascade and build the
    // ToolDispatcher with the resolved permission set. When a sandbox is
    // available, inject SandboxedExecutor as the execution strategy so tool
    // calls are routed through bubblewrap transparently.
    let dispatcher = build_gateway_dispatcher(state, &config, ws_tx.clone());

    // Step 10: Build the Agent that will execute the turn.
    let agent_config =
        build_agent_config(session.agent_id.clone(), &config, output_schema_override);
    let mut agent_builder = AgentBuilder::new()
        .config(agent_config)
        .provider(state.provider.clone())
        .storage(state.storage.clone())
        .tool_dispatcher(dispatcher)
        .approval_registrar(build_approval_registrar(state.approval_channels.clone()))
        .delegation_handler(Arc::new(GatewayDelegationHandler::new(state.clone())))
        .context_transfer_config(config.communication.context_transfer.clone());
    if let Some(hook_registry) = state.hook_registry.clone() {
        agent_builder = agent_builder.hook_registry(hook_registry);
    }
    let agent = match agent_builder.build() {
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
            return Err(GatewayError::WebSocketError(format!(
                "agent construction failed: {e}"
            )));
        }
    };

    // Step 11: Create the TurnRequest with assembled context and cancellation.
    let turn_request = TurnRequest {
        execution: swage_agent::context::TurnExecutionContext {
            session_id,
            thread_id,
            turn_id: prepared_turn_id,
            project_id: Some(project_id),
        },
        user_message: before_inbound.canonical_content.clone(),
        assembled_context: assembled_messages,
        parent_turn_id: None,
        existing_turn_id: Some(prepared_turn_id),
        cancellation_token: cancel_token,
    };

    // Step 12: Create the event channel and spawn the agent's streaming loop.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Spawn the agent on a tokio task — it manages turn creation, state
    // transitions, tool dispatch, and message storage internally.
    let activity_tracker = state.activity_tracker.clone();
    let pricing_config = state.config.pricing.clone();
    let model_name = config.provider.model.clone();

    let agent_handle =
        tokio::spawn(async move { agent.process_turn_streaming(turn_request, event_tx).await });

    // Captures the turn_id emitted by TurnStarted so the agent result match
    // can use it for persistence and logging after the event loop finishes.
    let mut turn_id: Option<Uuid> = None;

    // Step 11: Forward AgentEvent → ServerEvent. The loop runs until the
    // agent drops event_tx (channel closes) or the agent task completes.
    while let Some(event) = event_rx.recv().await {
        match event {
            AgentEvent::TurnStarted {
                turn_id: started_id,
            } => {
                turn_id = Some(started_id);
                // Record the turn_id in the tracing span now that the agent
                // has created the Turn record.
                tracing::Span::current().record("turn_id", started_id.to_string());
                tracing::debug!("processing turn (agent-delegated)");

                // Persist the context snapshot now that we have a turn_id.
                if let Err(e) = state
                    .storage
                    .store_snapshot(started_id, &report_json.to_string())
                    .await
                {
                    tracing::warn!(error = %e, "failed to persist context snapshot");
                }

                // Deliver the ContextAssembled event (subscription-gated).
                let assembled_event = ServerEvent::ContextAssembled {
                    turn_id: started_id,
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
            AgentEvent::ToolStart {
                call_id,
                tool_name,
                arguments,
            } => {
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
            AgentEvent::ToolEnd {
                call_id,
                tool_name,
                result,
                status,
                duration_ms,
            } => {
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
            AgentEvent::OutputSchemaRetry {
                turn_id,
                attempt,
                max_retries,
                errors,
            } => {
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
            AgentEvent::OutputSchemaFailed {
                turn_id,
                attempts,
                errors,
            } => {
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
            AgentEvent::TurnCompleted {
                turn_id,
                input_tokens,
                output_tokens,
                structured_output,
                ..
            } => {
                // Derive cost from the pricing config when available.
                let cost_microcents =
                    pricing_config.compute_cost(&model_name, input_tokens, output_tokens);

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
                let (ws_input, ws_output, ws_cost) = if subscription
                    .lock()
                    .await
                    .is_subscribed(crate::subscription::ObservabilityChannel::Context)
                {
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
            AgentEvent::DelegationStarted {
                target_agent_id,
                task: _,
            } => {
                // Tool-as-delegation started — emit as a delegation event.
                // We use turn_id for parent_turn_id; child_turn_id is unknown
                // at this point (the child turn hasn't been created yet).
                let _ = ws_tx.send(ServerEvent::DelegationStarted {
                    parent_turn_id: turn_id.unwrap_or(prepared_turn_id),
                    child_turn_id: None,
                    target_agent_id,
                });
            }
            AgentEvent::DelegationCompleted {
                target_agent_id,
                success,
            } => {
                let _ = ws_tx.send(ServerEvent::DelegationCompleted {
                    parent_turn_id: turn_id.unwrap_or(prepared_turn_id),
                    child_turn_id: None,
                    target_agent_id,
                    success,
                });
            }
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
                if let Err(e) =
                    TurnStore::update_structured_output(&*state.storage, outbound.turn_id, output)
                        .await
                {
                    // Non-fatal: the turn completed; only the persisted record is affected.
                    tracing::warn!(
                        error = %e,
                        turn_id = %outbound.turn_id,
                        "failed to persist structured_output"
                    );
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
            tracing::info!(turn_id = ?turn_id, "output schema validation exhausted all retries");
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
        AgentEvent::ApprovalConfirmed {
            approval_id,
            decision,
            reason,
        } => {
            let _ = ws_tx.send(ServerEvent::ApprovalConfirmed {
                approval_id: approval_id.to_string(),
                decision,
                reason,
            });
            true
        }
        AgentEvent::ApprovalExpired {
            approval_id,
            timeout_policy,
        } => {
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
    use std::{collections::HashMap, pin::Pin, sync::Arc};

    use async_trait::async_trait;
    use futures::stream;
    use tokio::sync::{Mutex, Notify, mpsc};

    use swage_core::StorageError;
    use swage_core::{
        CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
        ToolCompletionResponse,
    };
    use swage_hook::{Hook, HookOutcome, HookRegistration, HookRegistry};
    use swage_provider::LlmProvider;
    use swage_storage::{
        model::{
            ActiveSkillRecord, AgentMessage, AppendMessage, ApprovalRequest, ApprovalState,
            EdgeEvaluation, Message, NewAgentMessage, NewApprovalRequest, NewEdgeEvaluation,
            NewMessage, NewNodeExecution, NewNodeRetry, NewProject, NewSession, NewTeam, NewThread,
            NewToolCall, NewTopologyRun, NewTurn, NodeExecution, NodeExecutionState, NodeRetry,
            Project, Session, Team, Thread, ThreadState, ToolCallRecord, TopologyRun,
            TopologyRunState, Turn, TurnState,
        },
        store::{
            ApprovalStore, ContextSnapshotStore, MessageStore, ProjectStore, SessionStore,
            SkillActivationStore, TeamStore, ThreadStore, ToolCallStore, TopologyTraceStore,
            TurnStore,
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
        fn set_to(path: impl AsRef<std::path::Path>) -> Self {
            let lock = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            unsafe { std::env::set_var("SWAGE_HOME", path.as_ref()) };
            Self { _lock: lock }
        }

        fn isolate() -> Self {
            Self::set_to("/tmp/swage_nonexistent_test_root")
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
        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in streaming tests")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
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
        fn model_id(&self) -> &str {
            "mock-multi-round"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in multi-round tests")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
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
        fn model_id(&self) -> &str {
            "pending-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("not used in cancellation tests")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
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

    struct BlockInboundHook;

    #[async_trait]
    impl Hook for BlockInboundHook {
        fn name(&self) -> &str {
            "block-inbound"
        }

        async fn execute(&self, _event: &HookEvent) -> HookOutcome {
            HookOutcome::Block("blocked by test hook".to_string())
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
        pub(crate) teams: HashMap<Uuid, Team>,
        pub(crate) thread_branches: Vec<swage_storage::ThreadBranch>,
        pub(crate) topology_runs: HashMap<Uuid, TopologyRun>,
        pub(crate) node_executions: HashMap<Uuid, NodeExecution>,
        pub(crate) edge_evaluations: Vec<EdgeEvaluation>,
        pub(crate) node_retries: Vec<NodeRetry>,
        pub(crate) approval_memory: std::collections::HashSet<(Uuid, String, String)>,
        pub(crate) sessions: HashMap<Uuid, Session>,
        pub(crate) threads: HashMap<Uuid, Thread>,
        pub(crate) turns: HashMap<Uuid, Turn>,
        pub(crate) messages: Vec<Message>,
        pub(crate) tool_calls: Vec<ToolCallRecord>,
        pub(crate) snapshots: HashMap<Uuid, String>,
        pub(crate) approvals: HashMap<Uuid, ApprovalRequest>,
        pub(crate) agent_messages: HashMap<Uuid, AgentMessage>,
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
                is_default: false,
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
            if let Some(p) = self
                .db
                .lock()
                .await
                .projects
                .values()
                .find(|project| project.is_default)
                .cloned()
            {
                return Ok(p);
            }
            let project = Project {
                id: Uuid::new_v4(),
                name: "default".to_string(),
                is_default: true,
                slug: "default".to_string(),
                workspace_path: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.db
                .lock()
                .await
                .projects
                .insert(project.id, project.clone());
            Ok(project)
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
                None => Err(StorageError::NotFound {
                    entity: "Project".into(),
                    id: id.to_string(),
                }),
                Some(p) => {
                    if let Some(n) = name {
                        p.name = n;
                    }
                    if let Some(s) = slug {
                        p.slug = s;
                    }
                    if let Some(wp) = workspace_path {
                        p.workspace_path = Some(wp);
                    }
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
    impl TeamStore for MockStorage {
        async fn create_team(&self, team: NewTeam) -> Result<Team, StorageError> {
            let now = chrono::Utc::now();
            let created = Team {
                id: Uuid::new_v4(),
                project_id: team.project_id,
                name: team.name,
                description: team.description,
                entry_agent_id: team.entry_agent_id,
                members: team.members,
                created_at: now,
                updated_at: now,
            };
            self.db
                .lock()
                .await
                .teams
                .insert(created.id, created.clone());
            Ok(created)
        }

        async fn get_team(&self, id: Uuid) -> Result<Option<Team>, StorageError> {
            Ok(self.db.lock().await.teams.get(&id).cloned())
        }

        async fn get_team_by_name(
            &self,
            project_id: Uuid,
            name: &str,
        ) -> Result<Option<Team>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .teams
                .values()
                .find(|team| team.project_id == project_id && team.name == name)
                .cloned())
        }

        async fn list_teams_by_project(&self, project_id: Uuid) -> Result<Vec<Team>, StorageError> {
            let mut teams: Vec<Team> = self
                .db
                .lock()
                .await
                .teams
                .values()
                .filter(|team| team.project_id == project_id)
                .cloned()
                .collect();
            teams.sort_by_key(|team| team.created_at);
            Ok(teams)
        }

        async fn update_team(
            &self,
            id: Uuid,
            name: Option<String>,
            description: Option<String>,
            entry_agent_id: Option<String>,
            members: Option<Vec<String>>,
        ) -> Result<Team, StorageError> {
            let mut db = self.db.lock().await;
            match db.teams.get_mut(&id) {
                None => Err(StorageError::NotFound {
                    entity: "Team".into(),
                    id: id.to_string(),
                }),
                Some(team) => {
                    if let Some(name) = name {
                        team.name = name;
                    }
                    if let Some(description) = description {
                        team.description = description;
                    }
                    if let Some(entry_agent_id) = entry_agent_id {
                        team.entry_agent_id = entry_agent_id;
                    }
                    if let Some(members) = members {
                        team.members = members;
                    }
                    team.updated_at = chrono::Utc::now();
                    Ok(team.clone())
                }
            }
        }

        async fn delete_team(&self, id: Uuid) -> Result<bool, StorageError> {
            Ok(self.db.lock().await.teams.remove(&id).is_some())
        }
    }

    #[async_trait]
    impl TopologyTraceStore for MockStorage {
        async fn create_topology_run(
            &self,
            run: NewTopologyRun,
        ) -> Result<TopologyRun, StorageError> {
            let now = chrono::Utc::now();
            let created = TopologyRun {
                id: Uuid::new_v4(),
                team_id: run.team_id,
                session_id: run.session_id,
                source_turn_id: run.source_turn_id,
                state: run.state.clone(),
                final_output: run.final_output,
                error_message: run.error_message,
                created_at: now,
                completed_at: if matches!(
                    run.state,
                    TopologyRunState::Completed
                        | TopologyRunState::Failed
                        | TopologyRunState::Interrupted
                ) {
                    Some(now)
                } else {
                    None
                },
            };
            self.db
                .lock()
                .await
                .topology_runs
                .insert(created.id, created.clone());
            Ok(created)
        }

        async fn get_topology_run(&self, id: Uuid) -> Result<Option<TopologyRun>, StorageError> {
            Ok(self.db.lock().await.topology_runs.get(&id).cloned())
        }

        async fn list_topology_runs_by_team(
            &self,
            team_id: Uuid,
        ) -> Result<Vec<TopologyRun>, StorageError> {
            let mut runs: Vec<TopologyRun> = self
                .db
                .lock()
                .await
                .topology_runs
                .values()
                .filter(|run| run.team_id == team_id)
                .cloned()
                .collect();
            runs.sort_by_key(|run| run.created_at);
            Ok(runs)
        }

        async fn list_topology_runs_by_session(
            &self,
            session_id: Uuid,
        ) -> Result<Vec<TopologyRun>, StorageError> {
            let mut runs: Vec<TopologyRun> = self
                .db
                .lock()
                .await
                .topology_runs
                .values()
                .filter(|run| run.session_id == session_id)
                .cloned()
                .collect();
            runs.sort_by_key(|run| run.created_at);
            Ok(runs)
        }

        async fn update_topology_run(
            &self,
            id: Uuid,
            state: TopologyRunState,
            final_output: Option<&serde_json::Value>,
            error_message: Option<&str>,
        ) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            let run = db
                .topology_runs
                .get_mut(&id)
                .ok_or(StorageError::NotFound {
                    entity: "TopologyRun".into(),
                    id: id.to_string(),
                })?;
            run.state = state.clone();
            if let Some(final_output) = final_output {
                run.final_output = Some(final_output.clone());
            }
            if let Some(error_message) = error_message {
                run.error_message = Some(error_message.to_string());
            }
            if matches!(
                state,
                TopologyRunState::Completed
                    | TopologyRunState::Failed
                    | TopologyRunState::Interrupted
            ) {
                run.completed_at = Some(chrono::Utc::now());
            }
            Ok(())
        }

        async fn create_node_execution(
            &self,
            execution: NewNodeExecution,
        ) -> Result<NodeExecution, StorageError> {
            let now = chrono::Utc::now();
            let created = NodeExecution {
                id: Uuid::new_v4(),
                topology_run_id: execution.topology_run_id,
                node_id: execution.node_id,
                node_kind: execution.node_kind,
                agent_id: execution.agent_id,
                task_name: execution.task_name,
                state: execution.state.clone(),
                input_json: execution.input_json,
                output_json: execution.output_json,
                error_message: execution.error_message,
                created_at: now,
                completed_at: if matches!(
                    execution.state,
                    NodeExecutionState::Completed
                        | NodeExecutionState::Failed
                        | NodeExecutionState::Skipped
                ) {
                    Some(now)
                } else {
                    None
                },
            };
            self.db
                .lock()
                .await
                .node_executions
                .insert(created.id, created.clone());
            Ok(created)
        }

        async fn get_node_execution(
            &self,
            id: Uuid,
        ) -> Result<Option<NodeExecution>, StorageError> {
            Ok(self.db.lock().await.node_executions.get(&id).cloned())
        }

        async fn list_node_executions_by_run(
            &self,
            topology_run_id: Uuid,
        ) -> Result<Vec<NodeExecution>, StorageError> {
            let mut executions: Vec<NodeExecution> = self
                .db
                .lock()
                .await
                .node_executions
                .values()
                .filter(|execution| execution.topology_run_id == topology_run_id)
                .cloned()
                .collect();
            executions.sort_by_key(|execution| execution.created_at);
            Ok(executions)
        }

        async fn update_node_execution(
            &self,
            id: Uuid,
            state: NodeExecutionState,
            output_json: Option<&serde_json::Value>,
            error_message: Option<&str>,
        ) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            let execution = db
                .node_executions
                .get_mut(&id)
                .ok_or(StorageError::NotFound {
                    entity: "NodeExecution".into(),
                    id: id.to_string(),
                })?;
            execution.state = state.clone();
            if let Some(output_json) = output_json {
                execution.output_json = Some(output_json.clone());
            }
            if let Some(error_message) = error_message {
                execution.error_message = Some(error_message.to_string());
            }
            if matches!(
                state,
                NodeExecutionState::Completed
                    | NodeExecutionState::Failed
                    | NodeExecutionState::Skipped
            ) {
                execution.completed_at = Some(chrono::Utc::now());
            }
            Ok(())
        }

        async fn create_edge_evaluation(
            &self,
            evaluation: NewEdgeEvaluation,
        ) -> Result<EdgeEvaluation, StorageError> {
            let created = EdgeEvaluation {
                id: Uuid::new_v4(),
                topology_run_id: evaluation.topology_run_id,
                source_node_execution_id: evaluation.source_node_execution_id,
                edge_id: evaluation.edge_id,
                target_node_id: evaluation.target_node_id,
                condition_expression: evaluation.condition_expression,
                matched: evaluation.matched,
                payload_json: evaluation.payload_json,
                created_at: chrono::Utc::now(),
            };
            self.db.lock().await.edge_evaluations.push(created.clone());
            Ok(created)
        }

        async fn list_edge_evaluations_by_run(
            &self,
            topology_run_id: Uuid,
        ) -> Result<Vec<EdgeEvaluation>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .edge_evaluations
                .iter()
                .filter(|evaluation| evaluation.topology_run_id == topology_run_id)
                .cloned()
                .collect())
        }

        async fn create_node_retry(&self, retry: NewNodeRetry) -> Result<NodeRetry, StorageError> {
            let created = NodeRetry {
                id: Uuid::new_v4(),
                node_execution_id: retry.node_execution_id,
                attempt_number: retry.attempt_number,
                failure_kind: retry.failure_kind,
                error_message: retry.error_message,
                recovery_guidance: retry.recovery_guidance,
                created_at: chrono::Utc::now(),
            };
            self.db.lock().await.node_retries.push(created.clone());
            Ok(created)
        }

        async fn list_node_retries(
            &self,
            node_execution_id: Uuid,
        ) -> Result<Vec<NodeRetry>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .node_retries
                .iter()
                .filter(|retry| retry.node_execution_id == node_execution_id)
                .cloned()
                .collect())
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
                metadata: session.metadata,
            };
            self.db.lock().await.sessions.insert(s.id, s.clone());
            Ok(s)
        }

        async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
            Ok(self.db.lock().await.sessions.get(&id).cloned())
        }

        async fn list_by_project(&self, project_id: Uuid) -> Result<Vec<Session>, StorageError> {
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

        async fn list_sessions_by_agent_id(
            &self,
            agent_id: &str,
        ) -> Result<Vec<Session>, StorageError> {
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

        async fn list_threads_by_agent_id(
            &self,
            agent_id: &str,
        ) -> Result<Vec<Thread>, StorageError> {
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
                None => Err(StorageError::NotFound {
                    entity: "Thread".into(),
                    id: id.to_string(),
                }),
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

        async fn create_branch(
            &self,
            parent_thread_id: Uuid,
            branched_from_message_id: Uuid,
        ) -> Result<swage_storage::ThreadBranch, StorageError> {
            let mut db = self.db.lock().await;
            let parent =
                db.threads
                    .get(&parent_thread_id)
                    .cloned()
                    .ok_or(StorageError::NotFound {
                        entity: "Thread".into(),
                        id: parent_thread_id.to_string(),
                    })?;
            let source_position = db
                .messages
                .iter()
                .find(|message| {
                    message.id == branched_from_message_id && message.thread_id == parent_thread_id
                })
                .map(|message| message.position)
                .ok_or(StorageError::NotFound {
                    entity: "Message".into(),
                    id: branched_from_message_id.to_string(),
                })?;
            let branch = swage_storage::ThreadBranch {
                child_thread_id: Uuid::new_v4(),
                parent_thread_id,
                branched_from_message_id,
                created_at: chrono::Utc::now(),
            };
            db.threads.insert(
                branch.child_thread_id,
                Thread {
                    id: branch.child_thread_id,
                    session_id: parent.session_id,
                    agent_id: parent.agent_id,
                    state: ThreadState::Idle,
                    created_at: branch.created_at,
                    updated_at: branch.created_at,
                },
            );
            let messages_to_copy: Vec<_> = db
                .messages
                .iter()
                .filter(|message| {
                    message.thread_id == parent_thread_id && message.position <= source_position
                })
                .cloned()
                .collect();
            for message in messages_to_copy {
                let message_id = Uuid::new_v4();
                db.messages.push(swage_storage::Message {
                    id: message_id,
                    thread_id: branch.child_thread_id,
                    turn_id: message.turn_id,
                    role: message.role,
                    content: message.content,
                    position: message.position,
                    agent_id: message.agent_id,
                    tool_call_id: message.tool_call_id,
                    tool_name: message.tool_name,
                    created_at: message.created_at,
                });
            }
            db.thread_branches.push(branch.clone());
            Ok(branch)
        }

        async fn list_branches_by_session(
            &self,
            session_id: Uuid,
        ) -> Result<Vec<swage_storage::ThreadBranch>, StorageError> {
            let db = self.db.lock().await;
            Ok(db
                .thread_branches
                .iter()
                .filter(|branch| {
                    db.threads
                        .get(&branch.child_thread_id)
                        .map(|thread| thread.session_id == session_id)
                        .unwrap_or(false)
                })
                .cloned()
                .collect())
        }

        async fn get_branch(
            &self,
            child_thread_id: Uuid,
        ) -> Result<Option<swage_storage::ThreadBranch>, StorageError> {
            Ok(self
                .db
                .lock()
                .await
                .thread_branches
                .iter()
                .find(|branch| branch.child_thread_id == child_thread_id)
                .cloned())
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
                user_input: turn.user_input,
                inbound_audit: None,
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

        async fn list_turns_by_thread(&self, thread_id: Uuid) -> Result<Vec<Turn>, StorageError> {
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

        async fn update_turn_state(&self, id: Uuid, state: TurnState) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            if let Some(turn) = db.turns.get_mut(&id) {
                if matches!(
                    state,
                    TurnState::Completed | TurnState::Failed | TurnState::Interrupted
                ) {
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
                        && matches!(
                            t.state,
                            TurnState::Pending
                                | TurnState::Processing
                                | TurnState::AwaitingApproval
                                | TurnState::Delegating
                        )
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

        async fn update_inbound_audit(
            &self,
            id: Uuid,
            audit: &swage_storage::InboundAudit,
        ) -> Result<(), StorageError> {
            if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
                t.inbound_audit = Some(audit.clone());
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
            self.db
                .lock()
                .await
                .snapshots
                .insert(turn_id, report_json.to_string());
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
                turn_id: message.turn_id,
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

        async fn append_message(&self, message: AppendMessage) -> Result<Message, StorageError> {
            let mut db = self.db.lock().await;
            let position = db
                .messages
                .iter()
                .filter(|existing| existing.thread_id == message.thread_id)
                .map(|existing| existing.position)
                .max()
                .map_or(0, |max_position| max_position + 1);
            let created = Message {
                id: Uuid::new_v4(),
                thread_id: message.thread_id,
                turn_id: message.turn_id,
                role: message.role,
                content: message.content,
                position,
                agent_id: message.agent_id,
                tool_call_id: None,
                tool_name: None,
                created_at: chrono::Utc::now(),
            };
            db.messages.push(created.clone());
            Ok(created)
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
            Ok(self
                .db
                .lock()
                .await
                .messages
                .iter()
                .find(|m| m.id == id)
                .cloned())
        }

        async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError> {
            self.db
                .lock()
                .await
                .messages
                .retain(|m| m.thread_id != thread_id);
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

        async fn get_tool_call(&self, id: Uuid) -> Result<Option<ToolCallRecord>, StorageError> {
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
        async fn activate_skill(
            &self,
            _session_id: Uuid,
            _skill_name: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }
        async fn deactivate_skill(
            &self,
            _session_id: Uuid,
            _skill_name: &str,
        ) -> Result<bool, StorageError> {
            Ok(false)
        }
        async fn list_active_skills(
            &self,
            _session_id: Uuid,
        ) -> Result<Vec<ActiveSkillRecord>, StorageError> {
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
            self.db
                .lock()
                .await
                .approvals
                .insert(approval_req.id, approval_req.clone());
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

        async fn list_pending_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
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

        async fn remember_approval(
            &self,
            session_id: Uuid,
            tool_name: &str,
            request_fingerprint: &str,
        ) -> Result<(), StorageError> {
            self.db.lock().await.approval_memory.insert((
                session_id,
                tool_name.to_string(),
                request_fingerprint.to_string(),
            ));
            Ok(())
        }

        async fn has_approval_memory(
            &self,
            session_id: Uuid,
            tool_name: &str,
            request_fingerprint: &str,
        ) -> Result<bool, StorageError> {
            Ok(self.db.lock().await.approval_memory.contains(&(
                session_id,
                tool_name.to_string(),
                request_fingerprint.to_string(),
            )))
        }
    }

    #[async_trait]
    impl swage_storage::store::AgentMessageStore for MockStorage {
        async fn create_agent_message(
            &self,
            msg: NewAgentMessage,
        ) -> Result<AgentMessage, StorageError> {
            msg.validate()?;

            let now = chrono::Utc::now();
            let record = AgentMessage {
                id: Uuid::new_v4(),
                sender_agent_id: msg.sender_agent_id,
                recipient_agent_id: msg.recipient_agent_id,
                kind: msg.kind,
                correlation_id: msg.correlation_id,
                payload: msg.payload,
                status: swage_core::MessageStatus::Pending,
                failure_reason: None,
                created_at: now,
                delivered_at: None,
            };
            self.db
                .lock()
                .await
                .agent_messages
                .insert(record.id, record.clone());
            Ok(record)
        }
        async fn get_agent_message(&self, id: Uuid) -> Result<Option<AgentMessage>, StorageError> {
            Ok(self.db.lock().await.agent_messages.get(&id).cloned())
        }
        async fn list_pending_for_agent(
            &self,
            agent_id: &str,
        ) -> Result<Vec<AgentMessage>, StorageError> {
            let mut messages: Vec<_> = self
                .db
                .lock()
                .await
                .agent_messages
                .values()
                .filter(|message| {
                    message.recipient_agent_id == agent_id
                        && message.status == swage_core::MessageStatus::Pending
                })
                .cloned()
                .collect();
            messages.sort_by_key(|message| message.created_at);
            Ok(messages)
        }
        async fn update_agent_message_status(
            &self,
            id: Uuid,
            status: swage_core::MessageStatus,
            failure_reason: Option<&str>,
        ) -> Result<bool, StorageError> {
            let mut db = self.db.lock().await;
            let Some(message) = db.agent_messages.get_mut(&id) else {
                return Ok(false);
            };
            if message.status != swage_core::MessageStatus::Pending {
                return Ok(false);
            }

            message.status = status.clone();
            message.failure_reason = failure_reason.map(str::to_string);
            message.delivered_at = if status == swage_core::MessageStatus::Delivered {
                Some(chrono::Utc::now())
            } else {
                None
            };
            Ok(true)
        }
        async fn update_agent_message_payload(
            &self,
            id: Uuid,
            payload: serde_json::Value,
        ) -> Result<(), StorageError> {
            let mut db = self.db.lock().await;
            let message = db
                .agent_messages
                .get_mut(&id)
                .ok_or_else(|| StorageError::NotFound {
                    entity: "agent_message".into(),
                    id: id.to_string(),
                })?;
            message.payload = payload;
            Ok(())
        }
        async fn list_by_correlation(
            &self,
            correlation_id: Uuid,
        ) -> Result<Vec<AgentMessage>, StorageError> {
            let mut messages: Vec<_> = self
                .db
                .lock()
                .await
                .agent_messages
                .values()
                .filter(|message| message.correlation_id == Some(correlation_id))
                .cloned()
                .collect();
            messages.sort_by_key(|message| message.created_at);
            Ok(messages)
        }
        async fn expire_stale_messages(
            &self,
            max_age: std::time::Duration,
        ) -> Result<u64, StorageError> {
            let now = chrono::Utc::now();
            let mut db = self.db.lock().await;
            let mut expired = 0;
            for message in db.agent_messages.values_mut() {
                if message.status != swage_core::MessageStatus::Pending {
                    continue;
                }
                let Ok(age) = now.signed_duration_since(message.created_at).to_std() else {
                    continue;
                };
                if age > max_age {
                    message.status = swage_core::MessageStatus::Failed;
                    message.failure_reason = Some("delivery timeout".into());
                    message.delivered_at = None;
                    expired += 1;
                }
            }
            Ok(expired)
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
                metadata: None,
            })
            .await
            .expect("create session");
        let session_id = session.id;

        // Create the default thread for this session.
        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
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
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
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
                metadata: None,
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
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
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
        };
        (state, session_id)
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn full_turn_stores_messages_and_completes() {
        let chunks = vec![
            StreamChunk::Delta {
                content: "Hello".to_string(),
            },
            StreamChunk::Delta {
                content: ", world".to_string(),
            },
            StreamChunk::Delta {
                content: "!".to_string(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        );

        // Get thread_id and verify messages stored in the DB.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        let messages = state
            .storage
            .list_messages_by_thread(thread_id)
            .await
            .unwrap();
        assert_eq!(messages.len(), 2, "user message + assistant message");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content, "Hi");
        assert_eq!(messages[1].role, Role::Assistant);
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
                metadata: None,
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
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
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
        });

        let (tx, mut rx) = mpsc::unbounded_channel();

        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            process_turn(
                &*state_clone,
                session_id,
                None,
                "Hi".to_string(),
                tx,
                Arc::new(Mutex::new(SubscriptionState::new(true))),
                None,
            )
            .await
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

        handle
            .await
            .expect("task should not panic")
            .expect("process_turn ok");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::Cancelled {})),
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

        process_turn(
            &state,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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
                user_input: None,
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
        let result = process_turn(
            &state,
            session_id,
            None,
            "Another message".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await;

        assert!(
            matches!(result, Err(GatewayError::TurnInProgress { .. })),
            "expected TurnInProgress, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn pending_turn_rejected_with_turn_in_progress() {
        let (state, session_id) = make_state_with_session(vec![]).await;

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;

        state
            .storage
            .create_turn(NewTurn {
                thread_id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = process_turn(
            &state,
            session_id,
            None,
            "Another message".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await;

        assert!(
            matches!(result, Err(GatewayError::TurnInProgress { .. })),
            "expected TurnInProgress for pending turn, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn blocked_inbound_hook_marks_turn_failed_and_clears_active_turn() {
        let (mut state, session_id) = make_state_with_session(vec![]).await;
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(HookRegistration::new(Arc::new(BlockInboundHook)));
        state.hook_registry = Some(Arc::new(hook_registry));

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = process_turn(
            &state,
            session_id,
            None,
            "blocked message".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await;

        assert!(
            matches!(result, Err(GatewayError::ConfigError(ref message)) if message.contains("blocked by test hook")),
            "expected blocked inbound hook error, got: {result:?}"
        );
        assert!(
            !state.active_turns.lock().await.contains_key(&session_id),
            "blocked inbound hook must not leave a stale active_turns entry"
        );

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns.len(), 1, "expected placeholder turn to be created");
        assert_eq!(turns[0].state, TurnState::Failed);
    }

    // -----------------------------------------------------------------------
    // Mock tool for the execution loop
    // -----------------------------------------------------------------------

    struct EchoMockTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for EchoMockTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echo the input back"
        }
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

    struct BlockingMockTool {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for BlockingMockTool {
        fn name(&self) -> &str {
            "block"
        }
        fn description(&self) -> &str {
            "Block until the test releases execution"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "additionalProperties": true
            })
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _ctx: &dyn swage_tool::context::ToolContext,
        ) -> Result<serde_json::Value, swage_core::ToolError> {
            self.started.notify_waiters();
            self.release.notified().await;
            Ok(serde_json::json!({"output": "released"}))
        }
    }

    /// Build an AppState with a multi-round provider and an EchoMockTool.
    async fn make_state_with_tools(rounds: Vec<Vec<StreamChunk>>) -> (AppState, Uuid) {
        let storage = Arc::new(MockStorage::new());

        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
                metadata: None,
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
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
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![Arc::new(
                EchoMockTool,
            )
                as Arc<dyn crate::tool::Tool>])),
            sandbox: None,
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
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
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "The tool said: hello".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Use echo".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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

        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        );
    }

    #[tokio::test]
    async fn tool_call_stores_tool_call_record() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_store".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "stored" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Store this".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state
            .storage
            .list_tool_calls_by_turn(turns[0].id)
            .await
            .unwrap();
        assert_eq!(tool_calls.len(), 1, "expected 1 tool call record");
        assert_eq!(tool_calls[0].call_id, "call_store");
        assert_eq!(tool_calls[0].tool, "echo");
        assert!(
            tool_calls[0].result.is_some(),
            "tool call should have a result"
        );
        // The agent serialises the full ToolOutput.result JSON as the stored string.
        assert_eq!(
            tool_calls[0].result.as_deref(),
            Some("{\"output\":\"stored\"}")
        );
    }

    #[tokio::test]
    async fn unknown_tool_returns_error_to_model() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_unknown".into(),
                name: "nonexistent_tool".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "Sorry, that tool doesn't exist.".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Use unknown".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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

        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        );
    }

    #[tokio::test]
    async fn cancelling_during_tool_execution_interrupts_without_waiting_for_tool_completion() {
        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "call_block".into(),
                name: "block".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let storage = Arc::new(MockStorage::new());
        let project = storage
            .get_or_create_default()
            .await
            .expect("create default project");
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
                metadata: None,
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .expect("create default thread");

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: Arc::new(MultiRoundMockProvider::new(vec![round_1])) as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![Arc::new(
                BlockingMockTool {
                    started: started.clone(),
                    release: release.clone(),
                },
            )
                as Arc<dyn crate::tool::Tool>])),
            sandbox: None,
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        let state = Arc::new(state);
        let worker_state = state.clone();
        let handle = tokio::spawn(async move {
            process_turn(
                &worker_state,
                session_id,
                None,
                "Run block".to_string(),
                tx,
                Arc::new(Mutex::new(SubscriptionState::new(true))),
                None,
            )
            .await
        });

        started.notified().await;
        cancel_turn(&state, session_id)
            .await
            .expect("cancel should succeed while tool is running");

        handle
            .await
            .expect("task should not panic")
            .expect("process_turn ok");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::Cancelled {})),
            "expected Cancelled event, got: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                ServerEvent::ToolEnd { tool, call_id, status, .. }
                if tool == "block" && call_id == "call_block" && status == "cancelled"
            )),
            "expected cancelled ToolEnd event, got: {events:?}"
        );

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns[0].state, TurnState::Interrupted);

        let thread = ThreadStore::get(&*state.storage, thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(thread.state, ThreadState::Idle);

        release.notify_waiters();
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
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::ToolUse {
                call_id: "c3".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "third" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_3 = vec![
            StreamChunk::Delta {
                content: "All done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let (state, session_id) = make_state_with_tools(vec![round_1, round_2, round_3]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Multi-round".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let tool_starts: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ServerEvent::ToolStart { .. }))
            .collect();
        let tool_ends: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ServerEvent::ToolEnd { .. }))
            .collect();
        assert_eq!(tool_starts.len(), 3, "expected 3 ToolStart events");
        assert_eq!(tool_ends.len(), 3, "expected 3 ToolEnd events");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state
            .storage
            .list_tool_calls_by_turn(turns[0].id)
            .await
            .unwrap();
        assert_eq!(tool_calls.len(), 3, "expected 3 tool call records");

        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delegate_tool_executes_child_agent_and_resumes_parent_turn() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let project = storage
            .get_or_create_default()
            .await
            .expect("default project");
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
                metadata: None,
            })
            .await
            .expect("create session");
        let session_id = session.id;
        let root_thread = ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .expect("create root thread");

        let parent_round = vec![
            StreamChunk::ToolUse {
                call_id: "delegate-1".into(),
                name: "delegate".into(),
                arguments: serde_json::json!({
                    "target_agent": "agent-b",
                    "task": "Summarise this request",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let child_round = vec![
            StreamChunk::Delta {
                content: "Delegate answer".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let parent_resume_round = vec![
            StreamChunk::Delta {
                content: "Parent resumed after delegation".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let provider = Arc::new(MultiRoundMockProvider::new(vec![
            parent_round,
            child_round,
            parent_resume_round,
        ]));

        let swage_home = tempfile::TempDir::new().expect("temp swage home");
        std::fs::create_dir_all(swage_home.path().join("projects/default/agents/agent-b"))
            .expect("create delegated agent directory");
        let _guard = SwageHomeGuard::set_to(swage_home.path());

        let dispatcher =
            crate::tool::ToolDispatcher::allow_all(vec![
                Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn crate::tool::Tool>,
            ]);

        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            Arc::clone(&storage) as Arc<dyn swage_storage::store::ProjectStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            dispatcher,
            None,
            None,
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        process_turn(
            &state,
            session_id,
            None,
            "Delegate this".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        assert!(
            events.iter().any(|event| matches!(
                event,
                ServerEvent::DelegationStarted { target_agent_id, parent_turn_id, .. }
                if target_agent_id == "agent-b" && *parent_turn_id != Uuid::nil()
            )),
            "expected DelegationStarted event, got: {events:?}"
        );

        assert!(
            events.iter().any(|event| matches!(
                event,
                ServerEvent::DelegationCompleted { target_agent_id, success, .. }
                if target_agent_id == "agent-b" && *success
            )),
            "expected DelegationCompleted event, got: {events:?}"
        );

        let root_turns = state
            .storage
            .list_turns_by_thread(root_thread.id)
            .await
            .expect("list root turns");
        assert_eq!(root_turns.len(), 1, "expected exactly one root turn");
        let root_turn = &root_turns[0];
        assert_eq!(root_turn.state, TurnState::Completed);
        assert_eq!(
            root_turn.assistant_response.as_deref(),
            Some("Parent resumed after delegation"),
        );

        let child_sessions = state
            .storage
            .list_sessions_by_agent_id("agent-b")
            .await
            .expect("list child sessions");
        assert_eq!(child_sessions.len(), 1, "expected one child session");

        let child_threads = state
            .storage
            .list_by_session(child_sessions[0].id)
            .await
            .expect("list child threads");
        assert_eq!(child_threads.len(), 1, "expected one child thread");

        let child_turns = state
            .storage
            .list_turns_by_thread(child_threads[0].id)
            .await
            .expect("list child turns");
        assert_eq!(child_turns.len(), 1, "expected one child turn");
        assert_eq!(child_turns[0].state, TurnState::Completed);
        assert_eq!(child_turns[0].parent_turn_id, Some(root_turn.id));
        assert_eq!(
            child_turns[0].assistant_response.as_deref(),
            Some("Delegate answer")
        );
    }

    // -----------------------------------------------------------------------
    // Project context and config resolution tests
    // -----------------------------------------------------------------------

    struct CapturingProvider {
        captured: Arc<std::sync::Mutex<Option<CompletionRequest>>>,
        chunks: Vec<StreamChunk>,
    }

    impl CapturingProvider {
        fn new(
            chunks: Vec<StreamChunk>,
        ) -> (Self, Arc<std::sync::Mutex<Option<CompletionRequest>>>) {
            let captured = Arc::new(std::sync::Mutex::new(None));
            (
                Self {
                    captured: Arc::clone(&captured),
                    chunks,
                },
                captured,
            )
        }
    }

    #[async_trait]
    impl LlmProvider for CapturingProvider {
        fn model_id(&self) -> &str {
            "capturing-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unimplemented!()
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!()
        }

        fn stream(
            &self,
            request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            *self.captured.lock().unwrap() = Some(request);
            Box::pin(stream::iter(self.chunks.clone()))
        }
    }

    struct ContextCaptureTool {
        captured: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for ContextCaptureTool {
        fn name(&self) -> &str {
            "capture_context"
        }
        fn description(&self) -> &str {
            "Captures the working_directory from the tool context"
        }
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
                metadata: None,
            })
            .await
            .expect("session");
        let session_id = session.id;
        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .expect("thread");
        (storage, session_id)
    }

    #[tokio::test]
    async fn process_turn_passes_tool_context_to_tools() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let capture_tool = Arc::new(ContextCaptureTool {
            captured: Arc::clone(&captured),
        });

        let (storage, session_id) = make_storage_with_session().await;

        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "cc_1".into(),
                name: "capture_context".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
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
                capture_tool as Arc<dyn crate::tool::Tool>,
            ])),
            sandbox: None,
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(
            &state,
            session_id,
            None,
            "capture".into(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn ok");

        let working_dir = captured
            .lock()
            .unwrap()
            .take()
            .expect("ToolContext should have been passed during tool execution");

        assert!(
            working_dir.is_absolute(),
            "working_directory should be absolute: {working_dir:?}"
        );
    }

    #[tokio::test]
    async fn process_turn_resolves_config_per_project_and_completes_turn() {
        let chunks = vec![
            StreamChunk::Delta {
                content: "Hello".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (provider, captured_request) = CapturingProvider::new(chunks);

        let storage = Arc::new(MockStorage::new());
        let project = ProjectStore::create(
            &*storage,
            swage_storage::model::NewProject {
                name: "Test Config Project".to_string(),
                slug: "test-config-resolution-no-project-toml".to_string(),
                workspace_path: None,
            },
        )
        .await
        .expect("create test project");
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
                metadata: None,
            })
            .await
            .expect("session");
        let session_id = session.id;
        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
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
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(
            &state,
            session_id,
            None,
            "hi".into(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn ok");

        // Verify the provider was invoked — the agent delegates model selection
        // to the provider via model_id() rather than setting CompletionRequest.model.
        let req = captured_request.lock().unwrap().take();
        assert!(
            req.is_some(),
            "provider should have been invoked for config-resolved turn"
        );
    }

    #[tokio::test]
    async fn text_only_turn_still_works_with_tool_registry() {
        let (state, session_id) = make_state_with_tools(vec![vec![
            StreamChunk::Delta {
                content: "Just text".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]])
        .await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::Token { content } if content == "Just text"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        );

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ServerEvent::ToolStart { .. }))
        );
    }

    // -----------------------------------------------------------------------
    // ToolContext propagation tests
    // -----------------------------------------------------------------------

    struct WorkingDirReadTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for WorkingDirReadTool {
        fn name(&self) -> &str {
            "working_dir_read"
        }
        fn description(&self) -> &str {
            "Returns the working_directory from the ToolContext"
        }
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
        let project = ProjectStore::create(
            &*storage,
            NewProject {
                name: "my-project".to_string(),
                slug: "my-project".to_string(),
                workspace_path: None,
            },
        )
        .await
        .expect("create project");

        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
                metadata: None,
            })
            .await
            .expect("create session");
        let session_id = session.id;

        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .expect("thread");

        let round_1 = vec![
            StreamChunk::ToolUse {
                call_id: "ctx-check".into(),
                name: "working_dir_read".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            project_store: storage as Arc<dyn swage_storage::store::ProjectStore>,
            provider: provider as Arc<dyn LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![Arc::new(
                WorkingDirReadTool,
            )
                as Arc<dyn crate::tool::Tool>])),
            sandbox: None,
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        process_turn(
            &state,
            session_id,
            None,
            "Check context".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        let tool_calls = state
            .storage
            .list_tool_calls_by_turn(turns[0].id)
            .await
            .unwrap();
        assert_eq!(tool_calls.len(), 1);
        // The agent stores the full JSON-serialised tool output.
        let result = tool_calls[0].result.as_deref().unwrap_or("");
        let result_json: serde_json::Value =
            serde_json::from_str(result).expect("tool result should be valid JSON");
        let path_str = result_json["output"]
            .as_str()
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
        result: std::sync::Mutex<
            Option<Result<swage_sandbox::ToolCallResult, swage_core::SandboxError>>,
        >,
    }

    impl MockSandbox {
        fn returning(
            result: Result<swage_sandbox::ToolCallResult, swage_core::SandboxError>,
        ) -> Self {
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
            self.result
                .lock()
                .unwrap()
                .take()
                .expect("execute_tool called more than once")
        }

        async fn shutdown(&mut self) -> Result<(), swage_core::SandboxError> {
            Ok(())
        }
    }

    /// Minimal tool stub for sandbox tests — registered so dispatch can find
    /// the tool, but execution is intercepted by SandboxedExecutor before
    /// reaching this implementation.
    struct StubTool {
        tool_name: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for StubTool {
        fn name(&self) -> &str {
            self.tool_name
        }
        fn description(&self) -> &str {
            "stub for sandbox routing tests"
        }
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
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".to_string(),
                metadata: None,
            })
            .await
            .expect("session");
        let session_id = session.id;

        ThreadStore::create(
            &*storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
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
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                tx
            },
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            delivery_log: Arc::new(Mutex::new(Vec::new())),
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
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Ok(
            swage_sandbox::ToolCallResult::success(serde_json::json!("sandbox_output"), 5),
        )));
        // Register the tool so dispatch step 1 (lookup) succeeds; execution
        // is intercepted at step 6 by SandboxedExecutor.
        let tools: Vec<Arc<dyn crate::tool::Tool>> =
            vec![Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>];
        let (state, session_id) =
            make_state_with_sandbox(vec![round_1, round_2], sandbox, tools).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Use echo".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Err(
            swage_core::SandboxError::IpcError {
                message: "pipe broken".to_string(),
            },
        )));
        let tools: Vec<Arc<dyn crate::tool::Tool>> = vec![Arc::new(StubTool {
            tool_name: "broken",
        }) as Arc<dyn crate::tool::Tool>];
        let (state, session_id) =
            make_state_with_sandbox(vec![round_1, round_2], sandbox, tools).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Use broken".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let round_2 = vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];

        let sandbox = Arc::new(MockSandbox::returning(Ok(
            swage_sandbox::ToolCallResult::failed("tool execution failed inside sandbox", 10),
        )));
        let tools: Vec<Arc<dyn crate::tool::Tool>> = vec![Arc::new(StubTool {
            tool_name: "fail_tool",
        }) as Arc<dyn crate::tool::Tool>];
        let (state, session_id) =
            make_state_with_sandbox(vec![round_1, round_2], sandbox, tools).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Use fail_tool".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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
        use tracing_subscriber::{Registry, layer::SubscriberExt};

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
                        StreamChunk::Delta {
                            content: "ok".to_string(),
                        },
                        StreamChunk::Done {
                            finish_reason: None,
                        },
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

                    process_turn(
                        &state,
                        session_id,
                        None,
                        "test".to_string(),
                        tx,
                        Arc::new(Mutex::new(SubscriptionState::new(true))),
                        None,
                    )
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
        use tracing_subscriber::{Registry, layer::SubscriberExt};

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
                        StreamChunk::Delta {
                            content: "ok".to_string(),
                        },
                        StreamChunk::Usage(TokenUsage {
                            input_tokens: 100,
                            output_tokens: 50,
                        }),
                        StreamChunk::Done {
                            finish_reason: None,
                        },
                    ];
                    let (state, session_id) = make_state_with_session(chunks).await;
                    let (tx, _rx) = mpsc::unbounded_channel();

                    tracing::callsite::rebuild_interest_cache();

                    process_turn(
                        &state,
                        session_id,
                        None,
                        "test".to_string(),
                        tx,
                        Arc::new(Mutex::new(SubscriptionState::new(true))),
                        None,
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
        let history = HistoryElement::new(vec![ChatMessage::new(Role::User, "Hello")]);

        let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(prompts), Box::new(history)];

        let config = swage_config::ContextConfig::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

        let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

        // System message from instructions appears first.
        assert_eq!(
            output.messages[0].role,
            Role::System,
            "first message should be system instructions"
        );
        // User message from history follows.
        assert_eq!(
            output.messages[1].role,
            Role::User,
            "second message should be user history"
        );

        // Report should have non-zero token counts for both elements.
        let prompts_report = output
            .report
            .elements
            .iter()
            .find(|e| e.name == "prompts")
            .expect("report should include prompts element");
        assert!(
            prompts_report.token_count > 0,
            "prompts element should have non-zero token count"
        );

        let history_report = output
            .report
            .elements
            .iter()
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
        let history = HistoryElement::new(vec![ChatMessage::new(Role::User, "Hello")]);

        let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(prompts), Box::new(history)];

        let config = swage_config::ContextConfig::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

        let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

        // No system message when instructions are empty.
        assert!(
            output.messages.iter().all(|m| m.role != Role::System),
            "expected no system message when prompts are empty"
        );
        assert_eq!(
            output.messages.len(),
            1,
            "should only contain the user message"
        );
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

        assert_eq!(
            content.len(),
            1,
            "should produce exactly one system message"
        );
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
        assert_eq!(
            messages.len(),
            1,
            "non-empty memory should produce one message"
        );
        // content is Vec<ContentPart> — extract the text to check the value.
        let text = messages[0].content.iter().find_map(|p| {
            if let swage_core::ContentPart::Text { text } = p {
                Some(text.as_str())
            } else {
                None
            }
        });
        assert!(
            text.is_some_and(|t| t.contains("Important project context")),
            "memory message should contain the provided content"
        );
    }

    /// load_project_memory returns an empty string when the project has no
    /// memory directory.
    #[tokio::test]
    async fn load_project_memory_returns_empty_when_dir_absent() {
        let dir = tempfile::tempdir().unwrap();
        // No memory directory created — the helper should not panic.
        let result = load_project_memory(dir.path(), "no-such-project").await;
        assert!(
            result.is_empty(),
            "should return empty string when memory dir is absent"
        );
    }

    /// load_project_memory reads all .md files from the memory directory and
    /// joins them in alphabetical filename order.
    #[tokio::test]
    async fn load_project_memory_concatenates_md_files_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir
            .path()
            .join("projects")
            .join("my-project")
            .join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();

        // b_notes sorts after a_context.
        std::fs::write(memory_dir.join("b_notes.md"), "Second file content").unwrap();
        std::fs::write(memory_dir.join("a_context.md"), "First file content").unwrap();
        // Non-.md file must not be included.
        std::fs::write(memory_dir.join("other.txt"), "Should be ignored").unwrap();

        let result = load_project_memory(dir.path(), "my-project").await;

        assert!(
            result.contains("First file content"),
            "should include first .md file"
        );
        assert!(
            result.contains("Second file content"),
            "should include second .md file"
        );
        assert!(
            !result.contains("Should be ignored"),
            "should not include non-.md files"
        );

        // a_context.md sorts before b_notes.md alphabetically.
        let first_pos = result.find("First file content").unwrap();
        let second_pos = result.find("Second file content").unwrap();
        assert!(
            first_pos < second_pos,
            "files should appear in alphabetical order"
        );
    }

    /// load_project_memory skips files with only whitespace content.
    #[tokio::test]
    async fn load_project_memory_skips_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir
            .path()
            .join("projects")
            .join("my-project")
            .join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();

        std::fs::write(memory_dir.join("empty.md"), "   \n  ").unwrap();
        std::fs::write(memory_dir.join("content.md"), "Real content").unwrap();

        let result = load_project_memory(dir.path(), "my-project").await;

        assert!(result.contains("Real content"));
        assert!(
            !result.contains("   "),
            "whitespace-only files should be excluded"
        );
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
            StreamChunk::Delta {
                content: "Hello".to_string(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 42,
                output_tokens: 17,
            }),
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");

        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
        assert_eq!(turns.len(), 1);

        let turn = &turns[0];
        assert_eq!(
            turn.input_tokens,
            Some(42),
            "input_tokens should be persisted from provider usage"
        );
        assert_eq!(
            turn.output_tokens,
            Some(17),
            "output_tokens should be persisted from provider usage"
        );
    }

    /// When the provider does not return a Usage chunk, token fields should
    /// remain NULL (None) in the turn record.
    #[tokio::test]
    async fn token_usage_remains_null_when_provider_sends_no_usage() {
        let chunks = vec![
            StreamChunk::Delta {
                content: "Hello".to_string(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
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
            turn.input_tokens,
            Some(0),
            "input_tokens should be zero when no usage reported"
        );
        assert_eq!(
            turn.output_tokens,
            Some(0),
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
            StreamChunk::Delta {
                content: "Hello".to_string(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
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
        let chunks = vec![
            StreamChunk::Delta {
                content: "Hi".to_string(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, _rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
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
        let chunks = vec![
            StreamChunk::Delta {
                content: "Hi".to_string(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = false, no channels subscribed.
        process_turn(
            &state,
            session_id,
            None,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
            None,
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
            StreamChunk::Delta {
                content: "Hi".to_string(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            }),
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = true means all channels are subscribed.
        process_turn(
            &state,
            session_id,
            None,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
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
                assert_eq!(
                    *output_tokens,
                    Some(50),
                    "output_tokens should be populated"
                );
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
        use swage_config::OutputConfig;
        use swage_core::FinishReason;

        let chunks = vec![
            StreamChunk::Delta {
                content: r#"{"result":"ok"}"#.to_string(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ];

        // Build a config with an output schema requiring {"result": <string>}.
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "result": { "type": "string" } },
            "required": ["result"]
        });
        let mut config = swage_config::ResolvedConfig::default();
        config.output = OutputConfig {
            schema: Some(schema),
            max_retries: 1,
        };

        let (state, session_id) = make_state_with_config(chunks, config).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "give me json".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
        .expect("turn should complete");

        let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        let complete = events
            .iter()
            .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
            .expect("expected TurnComplete");

        match complete {
            ServerEvent::TurnComplete {
                structured_output, ..
            } => {
                assert!(
                    structured_output.is_some(),
                    "TurnComplete should carry structured_output when schema passes"
                );
                assert_eq!(structured_output.as_ref().unwrap()["result"], "ok");
            }
            other => panic!("expected TurnComplete, got: {other:?}"),
        }
    }

    /// After a valid schema turn, the turn record in storage has structured_output.
    #[tokio::test]
    async fn structured_output_persisted_on_turn_after_schema_validation() {
        let _guard = SwageHomeGuard::isolate();
        use swage_config::OutputConfig;
        use swage_core::FinishReason;
        use swage_storage::store::TurnStore;

        let chunks = vec![
            StreamChunk::Delta {
                content: r#"{"x":42}"#.to_string(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ];

        let schema = serde_json::json!({
            "type": "object",
            "properties": { "x": { "type": "integer" } },
            "required": ["x"]
        });
        let mut config = swage_config::ResolvedConfig::default();
        config.output = OutputConfig {
            schema: Some(schema),
            max_retries: 1,
        };

        let (state, session_id) = make_state_with_config(chunks, config).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        process_turn(
            &state,
            session_id,
            None,
            "give me x".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
            None,
        )
        .await
        .expect("turn should complete");

        // Drain events to let the turn finish.
        while rx.try_recv().is_ok() {}

        // Look up the turn in storage and verify structured_output was persisted.
        let threads = state.storage.list_by_session(session_id).await.unwrap();
        let thread_id = threads[0].id;
        let turns = TurnStore::list_turns_by_thread(&*state.storage, thread_id)
            .await
            .unwrap();
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
            StreamChunk::Delta {
                content: "Hi".to_string(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            }),
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let (state, session_id) = make_state_with_session(chunks).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        // force_on = false, no subscriptions.
        process_turn(
            &state,
            session_id,
            None,
            "Hello".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(false))),
            None,
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
                assert_eq!(
                    *input_tokens, None,
                    "input_tokens should be None when unsubscribed"
                );
                assert_eq!(
                    *output_tokens, None,
                    "output_tokens should be None when unsubscribed"
                );
                assert_eq!(
                    *cost_microcents, None,
                    "cost_microcents should be None when unsubscribed"
                );
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
            ServerEvent::ApprovalRequired {
                approval_id: aid,
                turn_id: tid,
                agent_id,
                kind,
                tool_name,
                ..
            } => {
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
            ServerEvent::ApprovalConfirmed {
                approval_id: aid,
                decision,
                reason,
            } => {
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
            ServerEvent::ApprovalExpired {
                approval_id: aid,
                timeout_policy,
            } => {
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
        let (_tx_b, mut rx_b) = mpsc::unbounded_channel::<ServerEvent>();

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
