// HTTP API client for project CRUD and project-scoped session operations.
//
// Wraps the gateway's REST endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type {
  Project,
  CreateProjectRequest,
  UpdateProjectRequest,
  ConversationSummary,
  ConversationResponse,
} from "../types";

// Base URL prepended to every request. Defaults to "" so requests resolve to
// the same origin — the Vite dev proxy then forwards /api/* to the gateway.
// Override with setProjectsBaseUrl() for testing or alternative deployments.
let baseUrl = "";

export function setProjectsBaseUrl(url: string): void {
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
// Project CRUD
// ---------------------------------------------------------------------------

/** Fetch all projects. */
export async function listProjects(): Promise<Project[]> {
  const response = await request("/api/projects", "GET");
  // Gateway wraps the array in { projects: [...] }.
  const body = (await response.json()) as { projects: Project[] };
  return body.projects;
}

/** Create a new project and return its metadata. */
export async function createProject(
  req: CreateProjectRequest,
): Promise<Project> {
  const response = await request("/api/projects", "POST", req);
  return response.json() as Promise<Project>;
}

/** Fetch a single project by ID. */
export async function getProject(id: string): Promise<Project> {
  const response = await request(`/api/projects/${id}`, "GET");
  return response.json() as Promise<Project>;
}

/** Update a project's name or workspace path and return the updated record. */
export async function updateProject(
  id: string,
  req: UpdateProjectRequest,
): Promise<Project> {
  const response = await request(`/api/projects/${id}`, "PUT", req);
  return response.json() as Promise<Project>;
}

/**
 * Delete a project by ID.
 *
 * The gateway returns 204 No Content on success — there is no response body
 * to parse.
 */
export async function deleteProject(id: string): Promise<void> {
  await request(`/api/projects/${id}`, "DELETE");
}

// ---------------------------------------------------------------------------
// Project-scoped session operations
// ---------------------------------------------------------------------------

/** Fetch all sessions belonging to a project. */
export async function listProjectSessions(
  projectId: string,
): Promise<ConversationSummary[]> {
  const response = await request(
    `/api/projects/${projectId}/sessions`,
    "GET",
  );
  // Gateway wraps the array in { conversations: [...] }.
  const body = (await response.json()) as {
    conversations: ConversationSummary[];
  };
  return body.conversations;
}

/** Create a new session within a project and return its metadata. */
export async function createProjectSession(
  projectId: string,
): Promise<ConversationResponse> {
  const response = await request(
    `/api/projects/${projectId}/sessions`,
    "POST",
  );
  return response.json() as Promise<ConversationResponse>;
}
