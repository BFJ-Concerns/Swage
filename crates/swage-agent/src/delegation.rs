//! Delegation executor — orchestrates the full delegation lifecycle.
//!
//! When Agent A delegates work to Agent B, this module handles: resolving
//! config, creating a child session/turn, instantiating Agent B, executing
//! the task, collecting the result, and delivering it back to Agent A.
//! Each delegation creates an independent agent instance — no shared state.

use std::sync::Arc;

use uuid::Uuid;

use swage_core::{ChatMessage, MessageKind, MessageStatus, Role};
use swage_hook::HookRegistry;
use swage_provider::LlmProvider;
use swage_safety::LeakDetector;
use swage_storage::model::{AgentMessage, NewAgentMessage, NewSession, NewThread, NewTurn};
use swage_storage::store::{SessionStore, ThreadStore};
use swage_storage::{Storage, TurnState};
use swage_tool::dispatch::ToolDispatcher;

use crate::builder::AgentBuilder;
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::event::AgentEvent;
use crate::message::TurnRequest;
use crate::router::MessageRouter;
use crate::schema_contract::resolve_delegation_schema;

/// Dependencies needed to instantiate and execute target agents for delegation.
pub struct DelegationExecutor {
    pub storage: Arc<dyn Storage>,
    pub provider: Arc<dyn LlmProvider>,
    pub hook_registry: Option<Arc<HookRegistry>>,
    pub leak_detector: Option<Arc<LeakDetector>>,
    pub router: Arc<MessageRouter>,
}

impl DelegationExecutor {
    /// Execute a delegation: instantiate the target agent, run it, and deliver
    /// the result back to the sender.
    ///
    /// `project_id` is the project the child session should belong to (inherited
    /// from the parent session). `target_config` and `target_tool_dispatcher`
    /// are resolved by the caller (gateway) from the config cascade.
    pub async fn execute_delegation(
        &self,
        delegate_msg: &AgentMessage,
        sender_turn_id: Uuid,
        project_id: Uuid,
        target_config: AgentConfig,
        target_tool_dispatcher: Option<ToolDispatcher>,
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

        let agent = builder.build()?;

        // Create the cancellation token for the delegation.
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

        let turn_request = TurnRequest {
            session_id: session.id,
            thread_id: thread.id,
            user_message: task_description.to_string(),
            assembled_context,
            cancellation_token: cancel_token.clone(),
        };

        // Execute — the agent creates its own turn internally via process_turn_streaming.
        let result = agent.process_turn_streaming(turn_request, event_tx).await;

        // Drain events (we don't forward them — delegation events are internal).
        while event_rx.try_recv().is_ok() {}

        // 6. Create Result or failure message.
        match result {
            Ok(outbound) => {
                // Build result payload with the structured output (if any).
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
                    .send_message(NewAgentMessage {
                        sender_agent_id: delegate_msg.recipient_agent_id.clone(),
                        recipient_agent_id: delegate_msg.sender_agent_id.clone(),
                        kind: MessageKind::Result,
                        correlation_id: Some(delegate_msg.id),
                        payload: result_payload,
                    })
                    .await?;

                Ok(result_msg)
            }
            Err(e) => {
                // Delegation failed — create a failure notification.
                let failure_payload = serde_json::json!({
                    "error": e.to_string(),
                });

                let result_msg = self
                    .router
                    .send_message(NewAgentMessage {
                        sender_agent_id: delegate_msg.recipient_agent_id.clone(),
                        recipient_agent_id: delegate_msg.sender_agent_id.clone(),
                        kind: MessageKind::Result,
                        correlation_id: Some(delegate_msg.id),
                        payload: failure_payload,
                    })
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
