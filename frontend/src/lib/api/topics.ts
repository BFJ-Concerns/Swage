// HTTP API client for topic CRUD and topic-scoped session operations.
//
// Wraps the gateway's REST endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type {
  Topic,
  CreateTopicRequest,
  CreateWorkspaceAttachmentRequest,
  UpdateTopicRequest,
  UpdateWorkspaceTrustRequest,
  WorkspaceAttachment,
  ConversationSummary,
  ConversationResponse,
} from "../types";
import * as generated from "../protocol/client";
import { ApiError } from "./error";

// Base URL prepended to every request. Defaults to "" so requests resolve to
// the same origin — the Vite dev proxy then forwards /api/* to the gateway.
// Override with setTopicsBaseUrl() for testing or alternative deployments.
let baseUrl = "";

export function setTopicsBaseUrl(url: string): void {
  baseUrl = url;
  generated.setClientBaseUrl(url);
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
    throw await ApiError.fromResponse(response, method, path);
  }

  return response;
}

// ---------------------------------------------------------------------------
// Topic CRUD — served by the generated client (frontend/src/lib/protocol/client.ts),
// rendered from the protocol route manifest; these wrappers only adapt the
// view type and the list unwrapping.
// ---------------------------------------------------------------------------

/** Fetch all topics. */
export async function listTopics(): Promise<Topic[]> {
  const body = await generated.listTopics();
  return body.topics;
}

/** Create a new topic and return its metadata. */
export async function createTopic(
  req: CreateTopicRequest,
): Promise<Topic> {
  return generated.createTopic({ description: "", ...req });
}

/** Fetch a single topic by ID. */
export async function getTopic(id: string): Promise<Topic> {
  return generated.getTopic(id);
}

/** Update a topic's metadata and return the updated record. */
export async function updateTopic(
  id: string,
  req: UpdateTopicRequest,
): Promise<Topic> {
  return generated.updateTopic(id, req);
}

/**
 * Delete a topic by ID.
 *
 * The gateway returns 204 No Content on success — there is no response body
 * to parse.
 */
export async function deleteTopic(id: string): Promise<void> {
  await generated.deleteTopic(id);
}

export async function listTopicWorkspaces(
  topicId: string,
): Promise<WorkspaceAttachment[]> {
  const response = await request(`/api/topics/${topicId}/workspaces`, "GET");
  const body = (await response.json()) as { workspaces: WorkspaceAttachment[] };
  return body.workspaces;
}

export async function attachTopicWorkspace(
  topicId: string,
  req: CreateWorkspaceAttachmentRequest,
): Promise<WorkspaceAttachment> {
  const response = await request(`/api/topics/${topicId}/workspaces`, "POST", req);
  return response.json() as Promise<WorkspaceAttachment>;
}

export async function updateTopicWorkspaceTrust(
  topicId: string,
  workspaceId: string,
  req: UpdateWorkspaceTrustRequest,
): Promise<WorkspaceAttachment> {
  const response = await request(
    `/api/topics/${topicId}/workspaces/${workspaceId}/trust`,
    "PUT",
    req,
  );
  return response.json() as Promise<WorkspaceAttachment>;
}

export async function detachTopicWorkspace(
  topicId: string,
  workspaceId: string,
): Promise<void> {
  await request(`/api/topics/${topicId}/workspaces/${workspaceId}`, "DELETE");
}

// ---------------------------------------------------------------------------
// Topic-scoped session operations
// ---------------------------------------------------------------------------

/** Fetch all sessions belonging to a topic. */
export async function listTopicSessions(
  topicId: string,
): Promise<ConversationSummary[]> {
  const response = await request(
    `/api/topics/${topicId}/sessions`,
    "GET",
  );
  // Gateway wraps the array in { conversations: [...] }.
  const body = (await response.json()) as {
    conversations: ConversationSummary[];
  };
  return body.conversations;
}

/** Create a new session within a topic and return its metadata. */
export async function createTopicSession(
  topicId: string,
): Promise<ConversationResponse> {
  const response = await request(
    `/api/topics/${topicId}/sessions`,
    "POST",
  );
  return response.json() as Promise<ConversationResponse>;
}
