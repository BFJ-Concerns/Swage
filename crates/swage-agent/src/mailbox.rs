//! Mailbox delivery — pending message retrieval on agent instantiation.
//!
//! When an agent is instantiated, it checks for pending messages in storage.
//! Cancel and Shutdown messages are processed before starting work; pending
//! Delegate messages are queued for processing.

// TODO Mailbox delivery on agent instantiation
// tags: forge:inter-agent-communication
// Define check_mailbox(storage, agent_id) → MailboxContents:
//   1. Query AgentMessageStore.list_pending_for_agent(agent_id)
//   2. Separate messages by kind:
//      - Shutdown → set flag to terminate immediately (don't start work)
//      - Cancel → collect correlation IDs for cancelled delegations
//      - Delegate → queue for processing
//      - Result → deliver to waiting delegation (if any)
//   3. For each delivered message, update status to Delivered
// Define MailboxContents struct:
//   should_shutdown: bool,
//   cancelled_delegations: Vec<Uuid>,  // correlation IDs
//   pending_delegates: Vec<AgentMessage>,
//   pending_results: Vec<AgentMessage>,
// Wire into agent startup path — check_mailbox is called when Agent
// is constructed or when process_turn_streaming begins.
// A Cancel for a delegation that hasn't started yet prevents it from starting.
// A Shutdown when the agent starts causes immediate termination.
// Success: stored Delegate delivered on agent instantiation, Cancel prevents
// work from starting, Shutdown causes immediate termination.
// ---
