import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
import type { EffectiveAgentDefinitionListResponse } from "../protocol/EffectiveAgentDefinitionListResponse";
import { ApiError } from "./error";

export async function listEffectiveAgentDefinitions(
  spaceId: string,
): Promise<EffectiveAgentDefinitionResponse[]> {
  const response = await fetch(`/api/spaces/${spaceId}/agent-definitions`, {
    method: "GET",
  });
  if (!response.ok) {
    throw await ApiError.fromResponse(
      response,
      "GET",
      `/api/spaces/${spaceId}/agent-definitions`,
    );
  }
  const body = (await response.json()) as EffectiveAgentDefinitionListResponse;
  return body.definitions;
}
