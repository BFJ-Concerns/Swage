// HTTP API client for space CRUD and space-scoped session operations.
//
// Wraps the gateway's REST endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type {
  Space,
  CreateSpaceRequest,
  CreateWorkspaceAttachmentRequest,
  UpdateSpaceRequest,
  UpdateWorkspaceTrustRequest,
  WorkspaceAttachment,
  ConversationSummary,
  ConversationResponse,
} from "../types";

// Base URL prepended to every request. Defaults to "" so requests resolve to
// the same origin — the Vite dev proxy then forwards /api/* to the gateway.
// Override with setSpacesBaseUrl() for testing or alternative deployments.
let baseUrl = "";

export function setSpacesBaseUrl(url: string): void {
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
    throw new Error(`HTTP ${response.status} from ${method} ${path}`);
  }

  return response;
}

// ---------------------------------------------------------------------------
// Space CRUD
// ---------------------------------------------------------------------------

/** Fetch all spaces. */
export async function listSpaces(): Promise<Space[]> {
  const response = await request("/api/spaces", "GET");
  // Gateway wraps the array in { spaces: [...] }.
  const body = (await response.json()) as { spaces: Space[] };
  return body.spaces;
}

/** Create a new space and return its metadata. */
export async function createSpace(
  req: CreateSpaceRequest,
): Promise<Space> {
  const response = await request("/api/spaces", "POST", req);
  return response.json() as Promise<Space>;
}

/** Fetch a single space by ID. */
export async function getSpace(id: string): Promise<Space> {
  const response = await request(`/api/spaces/${id}`, "GET");
  return response.json() as Promise<Space>;
}

/** Update a space's metadata and return the updated record. */
export async function updateSpace(
  id: string,
  req: UpdateSpaceRequest,
): Promise<Space> {
  const response = await request(`/api/spaces/${id}`, "PUT", req);
  return response.json() as Promise<Space>;
}

/**
 * Delete a space by ID.
 *
 * The gateway returns 204 No Content on success — there is no response body
 * to parse.
 */
export async function deleteSpace(id: string): Promise<void> {
  await request(`/api/spaces/${id}`, "DELETE");
}

export async function listSpaceWorkspaces(
  spaceId: string,
): Promise<WorkspaceAttachment[]> {
  const response = await request(`/api/spaces/${spaceId}/workspaces`, "GET");
  const body = (await response.json()) as { workspaces: WorkspaceAttachment[] };
  return body.workspaces;
}

export async function attachSpaceWorkspace(
  spaceId: string,
  req: CreateWorkspaceAttachmentRequest,
): Promise<WorkspaceAttachment> {
  const response = await request(`/api/spaces/${spaceId}/workspaces`, "POST", req);
  return response.json() as Promise<WorkspaceAttachment>;
}

export async function updateSpaceWorkspaceTrust(
  spaceId: string,
  workspaceId: string,
  req: UpdateWorkspaceTrustRequest,
): Promise<WorkspaceAttachment> {
  const response = await request(
    `/api/spaces/${spaceId}/workspaces/${workspaceId}/trust`,
    "PUT",
    req,
  );
  return response.json() as Promise<WorkspaceAttachment>;
}

export async function detachSpaceWorkspace(
  spaceId: string,
  workspaceId: string,
): Promise<void> {
  await request(`/api/spaces/${spaceId}/workspaces/${workspaceId}`, "DELETE");
}

// ---------------------------------------------------------------------------
// Space-scoped session operations
// ---------------------------------------------------------------------------

/** Fetch all sessions belonging to a space. */
export async function listSpaceSessions(
  spaceId: string,
): Promise<ConversationSummary[]> {
  const response = await request(
    `/api/spaces/${spaceId}/sessions`,
    "GET",
  );
  // Gateway wraps the array in { conversations: [...] }.
  const body = (await response.json()) as {
    conversations: ConversationSummary[];
  };
  return body.conversations;
}

/** Create a new session within a space and return its metadata. */
export async function createSpaceSession(
  spaceId: string,
): Promise<ConversationResponse> {
  const response = await request(
    `/api/spaces/${spaceId}/sessions`,
    "POST",
  );
  return response.json() as Promise<ConversationResponse>;
}
