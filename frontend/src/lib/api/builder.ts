import type {
  CreateProjectExtensionRequest,
  CreateTeamRequest,
  ProjectExtensionSummary,
  TeamSummary,
  UpdateProjectExtensionRequest,
  UpdateTeamRequest,
} from "../types";

let baseUrl = "";

export function setBuilderBaseUrl(url: string): void {
  baseUrl = url;
}

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

export async function listTeams(projectId: string): Promise<TeamSummary[]> {
  const response = await request(`/api/projects/${projectId}/teams`, "GET");
  const body = (await response.json()) as { teams: TeamSummary[] };
  return body.teams;
}

export async function createTeam(
  projectId: string,
  req: CreateTeamRequest,
): Promise<TeamSummary> {
  const response = await request(`/api/projects/${projectId}/teams`, "POST", req);
  return response.json() as Promise<TeamSummary>;
}

export async function updateTeam(
  teamId: string,
  req: UpdateTeamRequest,
): Promise<TeamSummary> {
  const response = await request(`/api/teams/${teamId}`, "PUT", req);
  return response.json() as Promise<TeamSummary>;
}

export async function listProjectExtensions(
  projectId: string,
): Promise<ProjectExtensionSummary[]> {
  const response = await request(`/api/projects/${projectId}/extensions`, "GET");
  const body = (await response.json()) as {
    extensions: ProjectExtensionSummary[];
  };
  return body.extensions;
}

export async function createProjectExtension(
  projectId: string,
  req: CreateProjectExtensionRequest,
): Promise<ProjectExtensionSummary> {
  const response = await request(
    `/api/projects/${projectId}/extensions`,
    "POST",
    req,
  );
  return response.json() as Promise<ProjectExtensionSummary>;
}

export async function updateProjectExtension(
  projectId: string,
  extensionName: string,
  req: UpdateProjectExtensionRequest,
): Promise<ProjectExtensionSummary> {
  const response = await request(
    `/api/projects/${projectId}/extensions/${extensionName}`,
    "PUT",
    req,
  );
  return response.json() as Promise<ProjectExtensionSummary>;
}

export async function approveWorkspaceExtension(
  projectId: string,
  extensionName: string,
): Promise<ProjectExtensionSummary> {
  const response = await request(
    `/api/projects/${projectId}/extensions/${extensionName}/approve-workspace`,
    "POST",
  );
  return response.json() as Promise<ProjectExtensionSummary>;
}
