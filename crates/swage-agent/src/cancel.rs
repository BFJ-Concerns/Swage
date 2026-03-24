//! Cancel and Shutdown message handling for inter-agent communication.
//!
//! Cancel interrupts a specific delegation (by correlation_id).
//! Shutdown terminates all active work under a target agent.

// TODO Cancel and Shutdown message handlers
// tags: forge:inter-agent-communication
// Implement handle_cancel(storage, cancel_msg: &AgentMessage) → Result<(), AgentError>:
//   1. Look up the original Delegate by correlation_id
//   2. Find the delegated turn (the turn created for that delegation)
//   3. Transition the delegated turn to Interrupted
//   4. If the target agent is currently running, signal cancellation
//      via the CancellationToken
//   5. Create an acknowledgement — the sender knows the cancellation took effect
//   6. If the target is NOT running, the Cancel is stored; on next instantiation
//      the mailbox check (mailbox.rs) prevents work from starting
// Implement handle_shutdown(storage, shutdown_msg: &AgentMessage) → Result<(), AgentError>:
//   1. Find all active turns for the target agent
//      (state = Processing or Pending or AwaitingApproval)
//   2. Transition each to Interrupted
//   3. If the target is running, signal cancellation for all active work
//   4. If not running, the Shutdown is stored; on next instantiation the
//      mailbox check terminates the agent immediately
// Both handlers update the message status to Delivered after processing.
// Success: Cancel interrupts a running delegation and sender receives ack,
// Shutdown interrupts all active turns, stored Cancel/Shutdown honoured
// on next instantiation.
// ---
