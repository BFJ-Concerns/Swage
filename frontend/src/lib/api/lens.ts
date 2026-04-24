import type {
  AppLens,
  CreateLensRequest,
  LensDescriptor,
  LensRuntimeResponse,
  UpdateLensRequest,
} from "../types";

let baseUrl = "";

export function setLensBaseUrl(url: string): void {
  baseUrl = url;
}

async function request(path: string, method: string, body?: unknown): Promise<Response> {
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

export async function fetchLensRuntime(benchId: string): Promise<LensRuntimeResponse> {
  const response = await request(`/api/benches/${benchId}/lens-runtime`, "GET");
  return response.json() as Promise<LensRuntimeResponse>;
}

export async function updateLensRuntime(
  benchId: string,
  lens: AppLens,
): Promise<LensRuntimeResponse> {
  const response = await request(`/api/benches/${benchId}/lens-runtime`, "PUT", { lens });
  return response.json() as Promise<LensRuntimeResponse>;
}

export async function listLenses(): Promise<LensDescriptor[]> {
  const response = await request("/api/lenses", "GET");
  const body = (await response.json()) as { lenses: LensDescriptor[] };
  return body.lenses;
}

export async function createLens(requestBody: CreateLensRequest): Promise<LensDescriptor> {
  const response = await request("/api/lenses", "POST", requestBody);
  return response.json() as Promise<LensDescriptor>;
}

export async function updateLens(
  lensId: string,
  requestBody: UpdateLensRequest,
): Promise<LensDescriptor> {
  const response = await request(`/api/lenses/${lensId}`, "PUT", requestBody);
  return response.json() as Promise<LensDescriptor>;
}

export async function installLensPreset(presetId: string): Promise<LensDescriptor> {
  const response = await request("/api/lenses/install", "POST", {
    preset_id: presetId,
  });
  return response.json() as Promise<LensDescriptor>;
}
