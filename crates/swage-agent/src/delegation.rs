//! Delegation executor — orchestrates the full delegation lifecycle.
//!
//! When Agent A delegates work to Agent B, this module handles: resolving
//! config, creating a child session/turn, instantiating Agent B, executing
//! the task, collecting the result, and delivering it back to Agent A.
//! Each delegation creates an independent agent instance — no shared state.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use swage_core::{ChatMessage, MessageKind, MessageStatus, Role};
use swage_hook::HookRegistry;
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::model::{AgentMessage, NewAgentMessage, NewSession, NewThread, NewTurn};
use swage_storage::store::{SessionStore, ThreadStore};
use swage_storage::{Storage, TurnState};
use swage_tool::dispatch::ToolDispatcher;

use crate::approval::ApprovalChannelRegistrar;
use crate::builder::AgentBuilder;
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::event::AgentEvent;
use crate::message::TurnRequest;
use crate::router::MessageRouter;
use crate::schema_contract::resolve_delegation_schema;

/// In-flight delegation metadata — keyed by delegate message ID.
/// Shared between `DelegationExecutor` (registers/deregisters) and
/// `handle_cancel` / `handle_shutdown` (looks up tokens to cancel precisely).
#[derive(Debug)]
pub struct ActiveDelegation {
    /// The child session created for this delegation.
    pub session_id: Uuid,
    /// The child thread within the delegation's session.
    pub thread_id: Uuid,
    /// The agent that is executing this delegation (the target).
    pub recipient_agent_id: String,
    /// Cancellation token for the in-flight agent execution.
    pub cancel_token: tokio_util::sync::CancellationToken,
}

/// Registry of active delegations, shared between executor and cancel handler.
pub type ActiveDelegations = Arc<RwLock<HashMap<Uuid, ActiveDelegation>>>;

/// Create an empty active-delegations registry.
pub fn active_delegations() -> ActiveDelegations {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Dependencies needed to instantiate and execute target agents for delegation.
pub struct DelegationExecutor {
    pub storage: Arc<dyn Storage>,
    pub provider: Arc<dyn LlmProvider>,
    pub hook_registry: Option<Arc<HookRegistry>>,
    pub leak_detector: Option<Arc<LeakDetector>>,
    pub approval_registrar: Option<ApprovalChannelRegistrar>,
    pub delegation_handler: Option<Arc<dyn DelegationHandler>>,
    pub router: Arc<MessageRouter>,
    /// Shared registry of in-flight delegations — lets `handle_cancel`
    /// find the cancellation token and child session for a specific delegation.
    pub active_delegations: ActiveDelegations,
}

impl DelegationExecutor {
    /// Execute a delegation: instantiate the target agent, run it, and deliver
    /// the result back to the sender.
    ///
    /// `project_id` is the project the child session should belong to (inherited
    /// from the parent session). `target_config` and `target_tool_dispatcher`
    /// are resolved by the caller (gateway) from the config cascade.
    /// `cancel_token` is a child token for this delegation: cancelling the
    /// parent turn cascades into the delegate, while delegate-specific cancel
    /// signals do not interrupt the parent turn directly.
    pub async fn execute_delegation(
        &self,
        delegate_msg: &AgentMessage,
        sender_turn_id: Uuid,
        project_id: Uuid,
        target_config: AgentConfig,
        target_context_transfer_config: swage_config::ContextTransferConfig,
        target_tool_dispatcher: Option<ToolDispatcher>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<AgentMessage, AgentError> {
        let target_agent_id = &delegate_msg.recipient_agent_id;

        // 1. Resolve output schema override.
        let output_config = swage_config::OutputConfig {
            schema: target_config.output_schema.clone(),
            max_retries: target_config.max_output_retries,
        };
        let delegation_output = resolve_delegation_schema(&delegate_msg.payload, &output_config);

        // 2. Create a child session + thread (per-task lifecycle).
        let session = self
            .storage
            .create_session(NewSession {
                project_id,
                agent_id: target_agent_id.clone(),
            })
            .await?;
        let thread = ThreadStore::create(
            self.storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: target_agent_id.clone(),
            },
        )
        .await?;

        // Annotate the delegate message with the child thread_id so the
        // fallback cancel path (when the active registry has no entry) can
        // scope interruption to the correct thread.
        {
            let mut payload = delegate_msg.payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "_thread_id".to_string(),
                    serde_json::Value::String(thread.id.to_string()),
                );
            }
            // Fail-open: a payload update failure shouldn't block delegation,
            // but the consequence is non-trivial — without the _thread_id
            // annotation, the fallback cancel path interrupts ALL in-progress
            // turns for the target agent instead of scoping to this thread.
            if let Err(e) = self
                .storage
                .update_agent_message_payload(delegate_msg.id, payload)
                .await
            {
                tracing::warn!(
                    delegate_id = %delegate_msg.id,
                    thread_id = %thread.id,
                    error = %e,
                    "failed to annotate delegate message with _thread_id — cancel fallback will use broad scope"
                );
            }
        }

        // 3. Build the task description from the Delegate payload.
        let task_description = delegate_msg
            .payload
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("Execute delegated work");

        // Build the initial context: transferred context + task description.
        let transferred_context = delegate_msg
            .payload
            .get("context")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value::<ChatMessage>(v.clone()).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut assembled_context = transferred_context;
        assembled_context.push(ChatMessage::new(Role::User, task_description));

        // 4. Build agent config with schema override.
        let mut agent_config = target_config;
        agent_config.output_schema = delegation_output.schema;
        agent_config.max_output_retries = delegation_output.max_retries;

        // 5. Construct and execute the target agent.
        let mut builder = AgentBuilder::new()
            .config(agent_config)
            .provider(self.provider.clone())
            .storage(self.storage.clone());

        if let Some(dispatcher) = target_tool_dispatcher {
            builder = builder.tool_dispatcher(dispatcher);
        }
        if let Some(ref registry) = self.hook_registry {
            builder = builder.hook_registry(registry.clone());
        }
        if let Some(ref detector) = self.leak_detector {
            builder = builder.leak_detector(detector.clone());
        }
        if let Some(ref registrar) = self.approval_registrar {
            builder = builder.approval_registrar(registrar.clone());
        }
        if let Some(ref delegation_handler) = self.delegation_handler {
            builder = builder.delegation_handler(delegation_handler.clone());
        }

        builder = builder
            .project_id(project_id)
            .context_transfer_config(target_context_transfer_config);

        let agent = builder.build()?;

        // Register the cancellation token so handle_cancel can interrupt
        // this delegation by message ID. The RAII guard ensures the entry is
        // removed even if this future is dropped mid-execution.
        let delegation_id = delegate_msg.id;
        {
            let mut active = self.active_delegations.write().await;
            active.insert(delegation_id, ActiveDelegation {
                session_id: session.id,
                thread_id: thread.id,
                recipient_agent_id: delegate_msg.recipient_agent_id.clone(),
                cancel_token: cancel_token.clone(),
            });
        }
        let _guard = DelegationGuard {
            delegations: self.active_delegations.clone(),
            delegation_id,
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

        let turn_request = TurnRequest {
            session_id: session.id,
            thread_id: thread.id,
            user_message: task_description.to_string(),
            assembled_context,
            parent_turn_id: Some(sender_turn_id),
            cancellation_token: cancel_token.clone(),
        };

        // Execute — the agent creates its own turn internally via process_turn_streaming.
        let result = agent.process_turn_streaming(turn_request, event_tx).await;

        // DelegationGuard handles deregistration on drop (including mid-
        // execution cancellation). No explicit removal needed.

        // Drain events (we don't forward them — delegation events are internal).
        while event_rx.try_recv().is_ok() {}

        // 6. Create Result or failure message — only the payload differs.
        let send_result = |payload| {
            NewAgentMessage {
                sender_agent_id: delegate_msg.recipient_agent_id.clone(),
                recipient_agent_id: delegate_msg.sender_agent_id.clone(),
                kind: MessageKind::Result,
                correlation_id: Some(delegate_msg.id),
                payload,
            }
        };

        match result {
            Ok(outbound) => {
                let mut result_payload = serde_json::json!({
                    "response": outbound.response,
                    "input_tokens": outbound.input_tokens,
                    "output_tokens": outbound.output_tokens,
                });
                if let Some(ref structured) = outbound.structured_output {
                    result_payload["structured_output"] = structured.clone();
                }

                let result_msg = self
                    .router
                    .send_message(send_result(result_payload))
                    .await?;

                Ok(result_msg)
            }
            Err(e) => {
                let failure_payload = serde_json::json!({
                    "error": e.to_string(),
                });

                let result_msg = self
                    .router
                    .send_message(send_result(failure_payload))
                    .await?;

                // Also fail the original delegate message.
                self.router
                    .fail_message(delegate_msg.id, &e.to_string())
                    .await
                    .ok();

                Ok(result_msg)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DelegationHandler — trait for tool-as-delegation
// ---------------------------------------------------------------------------

/// Trait for handling delegation requests from within the agent turn loop.
///
/// When the LLM calls the "delegate" tool, the agent catches
/// `ToolError::RequiresDelegation` and invokes this handler. The handler
/// resolves the target agent's config, runs the delegation, and returns
/// the result as the tool call's output.
///
/// The gateway implements this trait with the full delegation infrastructure.
/// In tests, it can be mocked to verify the delegation control flow without
/// actually running a second agent.
#[async_trait::async_trait]
pub trait DelegationHandler: Send + Sync {
    /// Execute a delegation to the target agent and return its result.
    ///
    /// The handler is responsible for:
    /// - Resolving the target agent's config from the hierarchy
    /// - Creating the delegate message and child session
    /// - Running the target agent via DelegationExecutor
    /// - Returning the result (or error) as a JSON value
    async fn handle_delegation(
        &self,
        target_agent_id: &str,
        task: &str,
        context: Option<serde_json::Value>,
        sender_agent_id: &str,
        sender_turn_id: Uuid,
        project_id: Uuid,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<serde_json::Value, AgentError>;
}

// ---------------------------------------------------------------------------
// RAII guard
// ---------------------------------------------------------------------------

/// RAII guard that removes a delegation from the active registry on drop.
///
/// Async futures don't run remaining code when dropped (e.g. by a parent
/// `select!` or timeout). This guard ensures the `active_delegations` entry
/// is cleaned up regardless of how the future exits, preventing memory leaks
/// in the delegation registry.
struct DelegationGuard {
    delegations: ActiveDelegations,
    delegation_id: Uuid,
}

impl Drop for DelegationGuard {
    fn drop(&mut self) {
        let delegations = self.delegations.clone();
        let id = self.delegation_id;
        // Spawn a blocking task to acquire the write lock — Drop is sync,
        // so we can't .await directly. The tokio runtime may already be
        // shutting down, in which case the spawn fails silently (acceptable:
        // the registry is being torn down anyway).
        let _ = tokio::task::spawn(async move {
            let mut active = delegations.write().await;
            active.remove(&id);
        });
    }
}
