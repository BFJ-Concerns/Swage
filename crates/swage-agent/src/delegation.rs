//! Delegation executor — orchestrates the full delegation lifecycle.
//!
//! When Agent A delegates work to Agent B, this module handles: instantiating
//! Agent B from config, creating a child turn with parent_turn_id, executing
//! the delegation, collecting the result, and delivering it back to Agent A.

// TODO DelegationExecutor — full delegation flow
// tags: forge:inter-agent-communication
// Define DelegationExecutor struct holding the dependencies needed to
// instantiate and run target agents (config resolver, provider, storage,
// tool registry, hook registry, leak detector).
// Implement execute_delegation(delegate_msg: &AgentMessage, sender_turn_id: Uuid)
//   → Result<AgentMessage, AgentError>:
//   1. Resolve target agent's config from the config cascade
//      (target = delegate_msg.recipient_agent_id)
//      If config not found → fail the Delegate message with "agent not found"
//   2. Apply output schema override if the Delegate payload includes one
//      (delegate schema takes precedence over target's [output] config)
//   3. Create a new session+thread for the target agent (per-task lifecycle)
//   4. Build a TurnRequest with:
//      - The task description from the Delegate payload
//      - Transferred context from the Delegate payload (already assembled at send time)
//   5. Create the child turn with parent_turn_id = sender_turn_id
//   6. Construct the target Agent via AgentBuilder
//   7. Execute agent.process_turn_streaming() — collect result
//   8. On success: create a Result message with the validated output,
//      correlated to the Delegate message ID
//   9. On failure: create a failure notification for the sender
//      (construction failure, execution error, schema exhaustion)
//  10. Deliver the Result message back to the sender
// Each delegation creates an independent agent instance — no shared state.
// Multi-hop delegation (A → B → C) is supported: B's DelegationExecutor
// creates C's turn with parent_turn_id = B's turn, forming a chain.
// Success: Agent A delegates to Agent B, Agent B executes independently,
// Agent A receives the typed result, delegation chain is traceable via
// parent_turn_id.
// ---
