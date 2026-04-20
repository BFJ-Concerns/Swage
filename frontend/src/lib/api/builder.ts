import type {
  BuilderApplyResponse,
  BuilderConfigOperation,
  BuilderProposalResponse,
  ContextSnapshotSharedReferencePayload,
  CreateProjectExtensionRequest,
  SharedReferenceDetailResponse,
  SharedReferenceKind,
  SharedReferenceSummary,
  TeamTopologySharedReferencePayload,
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

export async function createBuilderProposal(
  projectId: string,
  prompt: string,
): Promise<BuilderProposalResponse> {
  const response = await request(`/api/projects/${projectId}/builder/proposals`, "POST", {
    prompt,
  });
  return response.json() as Promise<BuilderProposalResponse>;
}

export async function applyBuilderProposal(
  projectId: string,
  operations: BuilderConfigOperation[],
  requestText?: string,
): Promise<{ status: number; body: BuilderApplyResponse }> {
  const response = await fetch(`${baseUrl}/api/projects/${projectId}/builder/apply`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      request_text: requestText ?? null,
      operations,
    }),
  });

  const body = (await response.json()) as BuilderApplyResponse;
  return { status: response.status, body };
}

export async function listSharedReferences(
  projectId: string,
): Promise<SharedReferenceSummary[]> {
  const response = await request(`/api/shared-objects?project_id=${projectId}`, "GET");
  const body = (await response.json()) as { objects: SharedReferenceSummary[] };
  return body.objects;
}

export async function getSharedReference(
  kind: SharedReferenceKind,
  id: string,
): Promise<SharedReferenceDetailResponse> {
  const response = await request(`/api/shared-objects/${kind}/${id}`, "GET");
  return response.json() as Promise<SharedReferenceDetailResponse>;
}

export async function createSharedReferenceEvent(
  kind: SharedReferenceKind,
  id: string,
  eventKind: string,
  payload: Record<string, unknown>,
): Promise<void> {
  await request(`/api/shared-objects/${kind}/${id}/events`, "POST", {
    event_kind: eventKind,
    payload,
  });
}

export async function updateTeamTopologyReference(
  teamId: string,
  payload: Pick<TeamTopologySharedReferencePayload, "entry_agent_id" | "members">,
): Promise<SharedReferenceDetailResponse> {
  const response = await request(`/api/shared-objects/team_topology/${teamId}`, "PUT", {
    entry_agent_id: payload.entry_agent_id,
    members: payload.members,
  });
  return response.json() as Promise<SharedReferenceDetailResponse>;
}

export function isTeamTopologyPayload(
  payload: SharedReferenceDetailResponse["object"]["payload"],
): payload is TeamTopologySharedReferencePayload {
  return "nodes" in payload;
}

export function isContextSnapshotPayload(
  payload: SharedReferenceDetailResponse["object"]["payload"],
): payload is ContextSnapshotSharedReferencePayload {
  return "elements" in payload;
}
