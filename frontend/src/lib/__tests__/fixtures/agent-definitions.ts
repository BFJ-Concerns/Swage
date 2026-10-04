import type { EffectiveAgentDefinitionResponse } from "../../protocol/EffectiveAgentDefinitionResponse";

/** A complete effective definition with no optional configuration. */
export function makeAgentDefinition(
  id: string,
  sourceScope: EffectiveAgentDefinitionResponse["source_scope"] = "global",
  overrides: Partial<EffectiveAgentDefinitionResponse> = {},
): EffectiveAgentDefinitionResponse {
  return {
    id,
    display_name: id,
    description: "",
    instructions: "",
    model_id: null,
    tool_permissions: null,
    skills: [],
    output_schema: null,
    output_max_retries: null,
    is_builtin: sourceScope === "built_in",
    selectable: true,
    source_scope: sourceScope,
    ...overrides,
  };
}
