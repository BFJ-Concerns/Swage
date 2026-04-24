// HTTP API client for bench CRUD and bench-scoped session operations.
//
// Wraps the gateway's REST endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type {
  Bench,
  CreateBenchRequest,
  CreateWorkspaceAttachmentRequest,
  UpdateBenchRequest,
  UpdateWorkspaceTrustRequest,
  WorkspaceAttachment,
  ConversationSummary,
  ConversationResponse,
} from "../types";

// Base URL prepended to every request. Defaults to "" so requests resolve to
// the same origin — the Vite dev proxy then forwards /api/* to the gateway.
// Override with setBenchesBaseUrl() for testing or alternative deployments.
let baseUrl = "";

export function setBenchesBaseUrl(url: string): void {
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
// Bench CRUD
// ---------------------------------------------------------------------------

/** Fetch all benches. */
export async function listBenches(): Promise<Bench[]> {
  const response = await request("/api/benches", "GET");
  // Gateway wraps the array in { benches: [...] }.
  const body = (await response.json()) as { benches: Bench[] };
  return body.benches;
}

/** Create a new bench and return its metadata. */
export async function createBench(
  req: CreateBenchRequest,
): Promise<Bench> {
  const response = await request("/api/benches", "POST", req);
  return response.json() as Promise<Bench>;
}

/** Fetch a single bench by ID. */
export async function getBench(id: string): Promise<Bench> {
  const response = await request(`/api/benches/${id}`, "GET");
  return response.json() as Promise<Bench>;
}

/** Update a bench's metadata and return the updated record. */
export async function updateBench(
  id: string,
  req: UpdateBenchRequest,
): Promise<Bench> {
  const response = await request(`/api/benches/${id}`, "PUT", req);
  return response.json() as Promise<Bench>;
}

/**
 * Delete a bench by ID.
 *
 * The gateway returns 204 No Content on success — there is no response body
 * to parse.
 */
export async function deleteBench(id: string): Promise<void> {
  await request(`/api/benches/${id}`, "DELETE");
}

export async function listBenchWorkspaces(
  benchId: string,
): Promise<WorkspaceAttachment[]> {
  const response = await request(`/api/benches/${benchId}/workspaces`, "GET");
  const body = (await response.json()) as { workspaces: WorkspaceAttachment[] };
  return body.workspaces;
}

export async function attachBenchWorkspace(
  benchId: string,
  req: CreateWorkspaceAttachmentRequest,
): Promise<WorkspaceAttachment> {
  const response = await request(`/api/benches/${benchId}/workspaces`, "POST", req);
  return response.json() as Promise<WorkspaceAttachment>;
}

export async function updateBenchWorkspaceTrust(
  benchId: string,
  workspaceId: string,
  req: UpdateWorkspaceTrustRequest,
): Promise<WorkspaceAttachment> {
  const response = await request(
    `/api/benches/${benchId}/workspaces/${workspaceId}/trust`,
    "PUT",
    req,
  );
  return response.json() as Promise<WorkspaceAttachment>;
}

export async function detachBenchWorkspace(
  benchId: string,
  workspaceId: string,
): Promise<void> {
  await request(`/api/benches/${benchId}/workspaces/${workspaceId}`, "DELETE");
}

// ---------------------------------------------------------------------------
// Bench-scoped session operations
// ---------------------------------------------------------------------------

/** Fetch all sessions belonging to a bench. */
export async function listBenchSessions(
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

/** Create a new session within a bench and return its metadata. */
export async function createBenchSession(
  benchId: string,
): Promise<ConversationResponse> {
  const response = await request(
    `/api/benches/${benchId}/sessions`,
    "POST",
  );
  return response.json() as Promise<ConversationResponse>;
}
