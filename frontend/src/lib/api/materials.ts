import type {
  CreateMaterialRequest,
  MaterialRecord,
} from "../types";

let baseUrl = "";

export function setMaterialsBaseUrl(url: string): void {
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

export async function listProjectMaterials(projectId: string): Promise<MaterialRecord[]> {
  const response = await request(`/api/projects/${projectId}/materials`, "GET");
  const body = (await response.json()) as { materials: MaterialRecord[] };
  return body.materials;
}

export async function listSessionMaterials(sessionId: string): Promise<MaterialRecord[]> {
  const response = await request(`/api/sessions/${sessionId}/materials`, "GET");
  const body = (await response.json()) as { materials: MaterialRecord[] };
  return body.materials;
}

export async function createMaterial(
  requestBody: CreateMaterialRequest,
): Promise<MaterialRecord> {
  const response = await request("/api/materials", "POST", requestBody);
  return response.json() as Promise<MaterialRecord>;
}
