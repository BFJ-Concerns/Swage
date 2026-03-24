//! Message routing service for inter-agent communication.
//!
//! Central service that validates, routes, persists, and delivers messages
//! between agents, with hook dispatch at each lifecycle stage.

// TODO MessageRouter — core message routing service
// tags: forge:inter-agent-communication
// Define MessageRouter struct holding Arc<dyn Storage>, Option<Arc<HookRegistry>>.
// Implement:
//   send_message(msg: NewAgentMessage) → Result<AgentMessage, AgentError>
//     1. Validate: correlation_id required on Result/Cancel kinds
//     2. Dispatch MessageSend hook — if blocked, return error (message not created)
//     3. For Notify: attempt immediate in-memory delivery only (no persistence)
//        If target not running, silently discard — this is defined semantics
//     4. For durable kinds: persist via AgentMessageStore.create_message()
//     5. Return the stored message
//   deliver_message(msg: &AgentMessage) → Result<(), AgentError>
//     1. Dispatch MessageDeliver hook — if blocked, transition to failed
//     2. Dispatch MessageReceive hook — if blocked, transition to failed
//     3. Update status to Delivered, set delivered_at
//   fail_message(id: Uuid, reason: &str) → Result<(), AgentError>
//     Transition message to Failed with the given reason.
// The router does NOT instantiate agents — that's the DelegationExecutor's job.
// The router handles message lifecycle (create, persist, deliver, fail).
// Success: Delegate message persisted with pending status, Notify not persisted,
// hook block prevents message creation, hook failures are fail-open.
// ---
