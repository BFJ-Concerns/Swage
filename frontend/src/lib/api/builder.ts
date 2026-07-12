import type {
  BuilderApplyResponse,
  BuilderConfigOperation,
  BuilderProposalResponse,
  ContextSnapshotSharedReferencePayload,
  CreateSpaceExtensionRequest,
  SharedReferenceDetailResponse,
  SharedReferenceKind,
  SharedReferenceSummary,
  TeamTopologySharedReferencePayload,
  CreateTeamRequest,
  SpaceExtensionSummary,
  TeamSummary,
  UpdateSpaceExtensionRequest,
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

export async function listTeams(spaceId: string): Promise<TeamSummary[]> {
  const response = await request(`/api/spaces/${spaceId}/teams`, "GET");
  const body = (await response.json()) as { teams: TeamSummary[] };
  return body.teams;
}

export async function createTeam(
  spaceId: string,
  req: CreateTeamRequest,
): Promise<TeamSummary> {
  const response = await request(`/api/spaces/${spaceId}/teams`, "POST", req);
  return response.json() as Promise<TeamSummary>;
}

export async function updateTeam(
  teamId: string,
  req: UpdateTeamRequest,
): Promise<TeamSummary> {
  const response = await request(`/api/teams/${teamId}`, "PUT", req);
  return response.json() as Promise<TeamSummary>;
}

export async function listSpaceExtensions(
  spaceId: string,
): Promise<SpaceExtensionSummary[]> {
  const response = await request(`/api/spaces/${spaceId}/extensions`, "GET");
  const body = (await response.json()) as {
    extensions: SpaceExtensionSummary[];
  };
  return body.extensions;
}

export async function createSpaceExtension(
  spaceId: string,
  req: CreateSpaceExtensionRequest,
): Promise<SpaceExtensionSummary> {
  const response = await request(
    `/api/spaces/${spaceId}/extensions`,
    "POST",
    req,
  );
  return response.json() as Promise<SpaceExtensionSummary>;
}

export async function updateSpaceExtension(
  spaceId: string,
  extensionName: string,
  req: UpdateSpaceExtensionRequest,
): Promise<SpaceExtensionSummary> {
  const response = await request(
    `/api/spaces/${spaceId}/extensions/${extensionName}`,
    "PUT",
    req,
  );
  return response.json() as Promise<SpaceExtensionSummary>;
}

export async function approveWorkspaceExtension(
  spaceId: string,
  extensionName: string,
): Promise<SpaceExtensionSummary> {
  const response = await request(
    `/api/spaces/${spaceId}/extensions/${extensionName}/approve-workspace`,
    "POST",
  );
  return response.json() as Promise<SpaceExtensionSummary>;
}

export async function createBuilderProposal(
  spaceId: string,
  prompt: string,
): Promise<BuilderProposalResponse> {
  const response = await request(`/api/spaces/${spaceId}/builder/proposals`, "POST", {
    prompt,
  });
  return response.json() as Promise<BuilderProposalResponse>;
}

export async function applyBuilderProposal(
  spaceId: string,
  operations: BuilderConfigOperation[],
  requestText?: string,
): Promise<{ status: number; body: BuilderApplyResponse }> {
  const response = await fetch(`${baseUrl}/api/spaces/${spaceId}/builder/apply`, {
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
  spaceId: string,
): Promise<SharedReferenceSummary[]> {
  const response = await request(`/api/shared-objects?space_id=${spaceId}`, "GET");
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
