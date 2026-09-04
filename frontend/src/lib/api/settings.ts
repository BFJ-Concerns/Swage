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
import type { SaveProviderSettings } from "../protocol/SaveProviderSettings";
import type { SaveSettingsRequest } from "../protocol/SaveSettingsRequest";
import * as generated from "../protocol/client";

export async function fetchSettings(): Promise<SettingsResponse> {
  return generated.getSettings();
}

export async function saveSettings(payload: SettingsPayload): Promise<SettingsResponse> {
  return generated.saveSettings(payload as unknown as SaveSettingsRequest);
}

export async function discoverModels(
  providerId: string,
  q = "",
  provider?: ProviderDiscoveryDraft,
): Promise<ModelDiscoveryResponse> {
  return provider
    ? generated.discoverDraftProviderModels(
      providerId,
      provider as unknown as SaveProviderSettings,
      q || undefined,
    )
    : generated.discoverProviderModels(providerId, q || undefined);
}

export async function fetchRestartStatus(): Promise<RestartStatusResponse> {
  return generated.getRestartStatus();
}

export async function restartGateway(): Promise<RestartStatusResponse> {
  return generated.restartGateway();
}

export async function fetchProviderSignIn(providerId: string): Promise<ProviderSignInState> {
  return generated.getProviderSignInState(providerId);
}

export async function startProviderSignIn(
  providerId: string,
  method: SignInMethod = "browser",
): Promise<StartProviderSignInResponse> {
  return generated.startProviderSignIn(providerId, { method });
}

export async function signOutProvider(providerId: string): Promise<ProviderSignInState> {
  return generated.signOutProvider(providerId);
}

export function emptyPreset(): GenerationPresetConfig {
  return { stop_sequences: [] };
}
