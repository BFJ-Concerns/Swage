import type {
  GenerationPresetConfig,
  ModelDiscoveryResponse,
  ProviderDiscoveryDraft,
  RestartStatusResponse,
  SettingsPayload,
  SettingsResponse,
} from "../types";

async function request(path: string, method = "GET", body?: unknown): Promise<Response> {
  const init: RequestInit = { method };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const response = await fetch(path, init);
  if (!response.ok) throw new Error(`HTTP ${response.status} from ${method} ${path}`);
  return response;
}

export async function fetchSettings(): Promise<SettingsResponse> {
  return (await request("/api/settings")).json() as Promise<SettingsResponse>;
}

export async function saveSettings(payload: SettingsPayload): Promise<SettingsResponse> {
  return (await request("/api/settings", "PUT", payload)).json() as Promise<SettingsResponse>;
}

export async function discoverModels(
  providerId: string,
  q = "",
  provider?: ProviderDiscoveryDraft,
): Promise<ModelDiscoveryResponse> {
  const query = q ? `?q=${encodeURIComponent(q)}` : "";
  const method = provider ? "POST" : "GET";
  return (await request(`/api/settings/providers/${encodeURIComponent(providerId)}/models${query}`, method, provider))
    .json() as Promise<ModelDiscoveryResponse>;
}

export async function fetchRestartStatus(): Promise<RestartStatusResponse> {
  return (await request("/api/settings/restart-required")).json() as Promise<RestartStatusResponse>;
}

export async function restartGateway(): Promise<RestartStatusResponse> {
  return (await request("/api/settings/restart", "POST")).json() as Promise<RestartStatusResponse>;
}

export function emptyPreset(): GenerationPresetConfig {
  return { stop_sequences: [] };
}
