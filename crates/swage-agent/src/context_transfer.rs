//! Context transfer policy evaluation for inter-agent delegation.
//!
//! Evaluates the configured context transfer policy at send time, assembling
//! the transferred content to include in the Delegate message payload.

// TODO Context transfer assembler
// tags: forge:inter-agent-communication
// Define ContextTransferAssembler struct (or free functions).
// Implement evaluate_transfer(policy, sender_context) → TransferredContext:
//   Mode::None → return empty (task description only)
//   Mode::Explicit → filter sender's assembled context elements by allowed_fields
//     (element types: "instructions", "recent_messages", "project_memory", "skills", "tools")
//     Silently omit fields that don't exist in the sender's context.
//   Mode::Summary → use swage-context compaction to summarise the sender's
//     context within the summary_budget token count. On compaction failure,
//     fall back to Mode::None with an error logged (fail-open pattern).
// TransferredContext is serialised into the Delegate message payload.
// The policy is resolved from CommunicationConfig: check targets map for the
// specific recipient, fall back to the default mode.
// Success: mode None returns empty context, mode Explicit returns only named
// fields, mode Summary produces a compacted summary within budget, compaction
// failure gracefully falls back to None.
// ---
