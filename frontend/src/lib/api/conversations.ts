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

/** Fetch all conversations belonging to the given topic. */
export async function listConversations(
  topicId: string,
): Promise<ConversationSummary[]> {
  return (await generated.listTopicSessions(topicId)).conversations;
}

/** Create a new conversation in the given topic and return its metadata. */
export async function createConversation(
  topicId: string,
  selectedModelId?: string | null,
  agentId?: string | null,
  effort?: Effort | null,
): Promise<ConversationResponse> {
  const body: CreateConversationRequest = {
    ...(selectedModelId ? { selected_model_id: selectedModelId } : {}),
    ...(agentId ? { agent_id: agentId } : {}),
    ...(effort ? { effort } : {}),
  };
  return generated.createTopicSession(
    topicId,
    Object.keys(body).length > 0 ? body : undefined,
  );
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
