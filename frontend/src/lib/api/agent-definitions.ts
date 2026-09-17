import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
import type { EffectiveAgentDefinitionListResponse } from "../protocol/EffectiveAgentDefinitionListResponse";
import { ApiError } from "./error";

/**
 * The agent definitions a new session may run under: the topic's effective
 * set, or — with no topic — the general scope's (the global definitions and
 * the built-in default).
 */
export async function listEffectiveAgentDefinitions(
  topicId: string | null,
): Promise<EffectiveAgentDefinitionResponse[]> {
  const path = topicId
    ? `/api/topics/${encodeURIComponent(topicId)}/agent-definitions`
    : "/api/agent-definitions";
  const response = await fetch(path, { method: "GET" });
  if (!response.ok) {
    throw await ApiError.fromResponse(response, "GET", path);
  }
  const body = (await response.json()) as EffectiveAgentDefinitionListResponse;
  return body.definitions;
}
