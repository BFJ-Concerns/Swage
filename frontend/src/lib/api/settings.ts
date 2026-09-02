import type {
  GenerationPresetConfig,
  ModelDiscoveryResponse,
  ProviderDiscoveryDraft,
  RestartStatusResponse,
  SettingsPayload,
  SettingsResponse,
} from "../types";
import type { ProviderSignInState } from "../protocol/ProviderSignInState";
import type { SignInMethod } from "../protocol/SignInMethod";
import type { StartProviderSignInResponse } from "../protocol/StartProviderSignInResponse";
import { ApiError } from "./error";

async function request(path: string, method = "GET", body?: unknown): Promise<Response> {
  const init: RequestInit = { method };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const response = await fetch(path, init);
  if (!response.ok) {
    throw await ApiError.fromResponse(response, method, path);
  }
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

export async function fetchProviderSignIn(providerId: string): Promise<ProviderSignInState> {
  return (await request(`/api/settings/providers/${encodeURIComponent(providerId)}/sign-in`))
    .json() as Promise<ProviderSignInState>;
}

export async function startProviderSignIn(
  providerId: string,
  method: SignInMethod = "browser",
): Promise<StartProviderSignInResponse> {
  return (await request(`/api/settings/providers/${encodeURIComponent(providerId)}/sign-in`, "POST", { method }))
    .json() as Promise<StartProviderSignInResponse>;
}

export async function signOutProvider(providerId: string): Promise<ProviderSignInState> {
  return (await request(`/api/settings/providers/${encodeURIComponent(providerId)}/sign-in`, "DELETE"))
    .json() as Promise<ProviderSignInState>;
}

export function emptyPreset(): GenerationPresetConfig {
  return { stop_sequences: [] };
}
