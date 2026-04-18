import type { AppLens, LensRuntimeResponse } from "../types";

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

export async function fetchLensRuntime(): Promise<LensRuntimeResponse> {
  const response = await request("/api/lens-runtime", "GET");
  return response.json() as Promise<LensRuntimeResponse>;
}

export async function updateLensRuntime(lens: AppLens): Promise<LensRuntimeResponse> {
  const response = await request("/api/lens-runtime", "PUT", { lens });
  return response.json() as Promise<LensRuntimeResponse>;
}
