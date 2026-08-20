import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
import type { EffectiveAgentDefinitionListResponse } from "../protocol/EffectiveAgentDefinitionListResponse";

export async function listEffectiveAgentDefinitions(
  spaceId: string,
): Promise<EffectiveAgentDefinitionResponse[]> {
  const response = await fetch(`/api/spaces/${spaceId}/agent-definitions`, {
    method: "GET",
  });
  if (!response.ok) {
    throw new Error(
      `HTTP ${response.status} from GET /api/spaces/${spaceId}/agent-definitions`,
    );
  }
  const body = (await response.json()) as EffectiveAgentDefinitionListResponse;
  return body.definitions;
}
