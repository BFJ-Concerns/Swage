// HTTP API client for conversation CRUD operations.
//
// Wraps the gateway's REST endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type {
  ConversationSummary,
  ConversationDetail,
  ConversationResponse,
} from "../types";

// Base URL prepended to every request. Defaults to "" so requests resolve to
// the same origin — the Vite dev proxy then forwards /api/* to the gateway.
// Override with setBaseUrl() for testing or alternative deployments.
let baseUrl = "";

export function setBaseUrl(url: string): void {
  baseUrl = url;
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/**
 * Perform a fetch and throw a descriptive error on non-2xx status.
 *
 * Pass a body to send a JSON payload (e.g. for POST/PUT). When a body is
 * present, the Content-Type header is set to application/json automatically.
 */
async function request(
  path: string,
  method: string,
  body?: unknown,
): Promise<Response> {
  const init: RequestInit = { method };

  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }

  const response = await fetch(`${baseUrl}${path}`, init);

  if (!response.ok) {
    throw new Error(
      `HTTP ${response.status} from ${method} ${path}`,
    );
  }

  return response;
}

// ---------------------------------------------------------------------------
// API functions
// ---------------------------------------------------------------------------

/** Fetch all conversations belonging to the given bench. */
export async function listConversations(
  benchId: string,
): Promise<ConversationSummary[]> {
  const response = await request(
    `/api/benches/${benchId}/sessions`,
    "GET",
  );
  // Gateway wraps the array in { conversations: [...] }.
  const body = (await response.json()) as {
    conversations: ConversationSummary[];
  };
  return body.conversations;
}

/** Create a new conversation in the given bench and return its metadata. */
export async function createConversation(
  benchId: string,
): Promise<ConversationResponse> {
  const response = await request(
    `/api/benches/${benchId}/sessions`,
    "POST",
  );
  return response.json() as Promise<ConversationResponse>;
}

/** Fetch a conversation's full detail, including its message history. */
export async function getConversation(id: string): Promise<ConversationDetail> {
  const response = await request(`/api/conversations/${id}`, "GET");
  return response.json() as Promise<ConversationDetail>;
}

/**
 * Delete a conversation by ID.
 *
 * The gateway returns 204 No Content on success — there is no response body
 * to parse.
 */
export async function deleteConversation(id: string): Promise<void> {
  await request(`/api/conversations/${id}`, "DELETE");
}
