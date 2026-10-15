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
import type { SaveSecuritySettingsRequest } from "../protocol/SaveSecuritySettingsRequest";
import type { SecuritySettingsResponse } from "../protocol/SecuritySettingsResponse";
import * as generated from "../protocol/client";

export async function fetchSettings(): Promise<SettingsResponse> {
  return generated.getSettings();
}

export async function saveSettings(payload: SettingsPayload): Promise<SettingsResponse> {
  return generated.saveSettings(payload);
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

// Security checks per scope (C69): what sessions in the install or in one
// topic run under, the scope's own overrides, and each check standing
// relaxed with the scope that relaxed it.
export async function fetchInstallSecuritySettings(): Promise<SecuritySettingsResponse> {
  return generated.getInstallSecuritySettings();
}

export async function saveInstallSecuritySettings(
  overrides: SaveSecuritySettingsRequest,
): Promise<SecuritySettingsResponse> {
  return generated.saveInstallSecuritySettings(overrides);
}

export async function fetchTopicSecuritySettings(topicId: string): Promise<SecuritySettingsResponse> {
  return generated.getTopicSecuritySettings(topicId);
}

export async function saveTopicSecuritySettings(
  topicId: string,
  overrides: SaveSecuritySettingsRequest,
): Promise<SecuritySettingsResponse> {
  return generated.saveTopicSecuritySettings(topicId, overrides);
}

export async function fetchConversationSecuritySettings(
  conversationId: string,
): Promise<SecuritySettingsResponse> {
  return generated.conversationSecuritySettings(conversationId);
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
