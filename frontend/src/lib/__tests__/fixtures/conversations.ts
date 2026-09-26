import type { ConversationDetail, ConversationSummary } from "../../types";

// Override-friendly fixtures: each returns a complete value with neutral
// defaults, and a test spreads in only the fields it is about. Adding a
// field to the wire type touches the type and this file, never every test
// that spells the type.

export function makeConversationSummary(
  overrides: Partial<ConversationSummary> = {},
): ConversationSummary {
  return {
    id: "conv-1",
    agent_id: "gateway",
    created_at: "2026-04-07T12:00:00Z",
    last_activity: "2026-04-07T12:00:00Z",
    message_preview: "Earlier message",
    has_interrupted_turn: false,
    ...overrides,
  };
}

export function makeConversationDetail(
  overrides: Partial<ConversationDetail> = {},
): ConversationDetail {
  return {
    id: "conv-1",
    agent_id: "gateway",
    topic_id: "proj-default",
    created_at: "2026-04-07T12:00:00Z",
    last_activity: "2026-04-07T12:00:00Z",
    messages: [],
    ...overrides,
  };
}
