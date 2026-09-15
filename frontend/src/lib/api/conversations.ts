// HTTP API client for conversation CRUD operations.
//
// Wraps the gateway's REST endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type {
  ConversationSummary,
  ConversationDetail,
  ConversationResponse,
} from "../types";
import type { CreateConversationRequest } from "../protocol/CreateConversationRequest";
import type { Effort } from "../protocol/Effort";
import * as generated from "../protocol/client";

export function setBaseUrl(url: string): void {
  generated.setClientBaseUrl(url);
}

// ---------------------------------------------------------------------------
// API functions
// ---------------------------------------------------------------------------

/**
 * Fetch the conversations of the given topic, or — with no topic — those of
 * the general scope.
 */
export async function listConversations(
  topicId: string | null,
): Promise<ConversationSummary[]> {
  const response = topicId
    ? await generated.listTopicSessions(topicId)
    : await generated.listConversations();
  return response.conversations;
}

/**
 * Create a new conversation and return its metadata: in the given topic, or —
 * with no topic — as a general assistant in the general scope, which the
 * gateway reports as an absent `topic_id`.
 */
export async function createConversation(
  topicId: string | null,
  selectedModelId?: string | null,
  agentId?: string | null,
  effort?: Effort | null,
): Promise<ConversationResponse> {
  const body: CreateConversationRequest = {
    ...(selectedModelId ? { selected_model_id: selectedModelId } : {}),
    ...(agentId ? { agent_id: agentId } : {}),
    ...(effort ? { effort } : {}),
  };
  const payload = Object.keys(body).length > 0 ? body : undefined;
  return topicId
    ? generated.createTopicSession(topicId, payload)
    : generated.createConversation(payload);
}

/** Fetch a conversation's full detail, including its message history. */
export async function getConversation(id: string): Promise<ConversationDetail> {
  return (await generated.getConversation(id)) as unknown as ConversationDetail;
}

/**
 * Delete a conversation by ID.
 *
 * The gateway returns 204 No Content on success — there is no response body
 * to parse.
 */
export async function deleteConversation(id: string): Promise<void> {
  await generated.deleteConversation(id);
}
