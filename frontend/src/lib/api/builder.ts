import type {
  BuilderApplyResponse,
  BuilderConfigOperation,
  BuilderProposalResponse,
  ContextSnapshotSharedReferencePayload,
  CreateBenchExtensionRequest,
  SharedReferenceDetailResponse,
  SharedReferenceKind,
  SharedReferenceSummary,
  TeamTopologySharedReferencePayload,
  CreateTeamRequest,
  BenchExtensionSummary,
  TeamSummary,
  UpdateBenchExtensionRequest,
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

export async function listTeams(benchId: string): Promise<TeamSummary[]> {
  const response = await request(`/api/benches/${benchId}/teams`, "GET");
  const body = (await response.json()) as { teams: TeamSummary[] };
  return body.teams;
}

export async function createTeam(
  benchId: string,
  req: CreateTeamRequest,
): Promise<TeamSummary> {
  const response = await request(`/api/benches/${benchId}/teams`, "POST", req);
  return response.json() as Promise<TeamSummary>;
}

export async function updateTeam(
  teamId: string,
  req: UpdateTeamRequest,
): Promise<TeamSummary> {
  const response = await request(`/api/teams/${teamId}`, "PUT", req);
  return response.json() as Promise<TeamSummary>;
}

export async function listBenchExtensions(
  benchId: string,
): Promise<BenchExtensionSummary[]> {
  const response = await request(`/api/benches/${benchId}/extensions`, "GET");
  const body = (await response.json()) as {
    extensions: BenchExtensionSummary[];
  };
  return body.extensions;
}

export async function createBenchExtension(
  benchId: string,
  req: CreateBenchExtensionRequest,
): Promise<BenchExtensionSummary> {
  const response = await request(
    `/api/benches/${benchId}/extensions`,
    "POST",
    req,
  );
  return response.json() as Promise<BenchExtensionSummary>;
}

export async function updateBenchExtension(
  benchId: string,
  extensionName: string,
  req: UpdateBenchExtensionRequest,
): Promise<BenchExtensionSummary> {
  const response = await request(
    `/api/benches/${benchId}/extensions/${extensionName}`,
    "PUT",
    req,
  );
  return response.json() as Promise<BenchExtensionSummary>;
}

export async function approveWorkspaceExtension(
  benchId: string,
  extensionName: string,
): Promise<BenchExtensionSummary> {
  const response = await request(
    `/api/benches/${benchId}/extensions/${extensionName}/approve-workspace`,
    "POST",
  );
  return response.json() as Promise<BenchExtensionSummary>;
}

export async function createBuilderProposal(
  benchId: string,
  prompt: string,
): Promise<BuilderProposalResponse> {
  const response = await request(`/api/benches/${benchId}/builder/proposals`, "POST", {
    prompt,
  });
  return response.json() as Promise<BuilderProposalResponse>;
}

export async function applyBuilderProposal(
  benchId: string,
  operations: BuilderConfigOperation[],
  requestText?: string,
): Promise<{ status: number; body: BuilderApplyResponse }> {
  const response = await fetch(`${baseUrl}/api/benches/${benchId}/builder/apply`, {
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
  benchId: string,
): Promise<SharedReferenceSummary[]> {
  const response = await request(`/api/shared-objects?bench_id=${benchId}`, "GET");
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
