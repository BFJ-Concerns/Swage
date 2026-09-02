// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { mount, unmount } from "svelte";

import AgentPicker from "../components/AgentPicker.svelte";
import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";

type MountedComponent = ReturnType<typeof mount>;

function definition(
  id: string,
  displayName: string,
  isBuiltin: boolean,
): EffectiveAgentDefinitionResponse {
  return {
    id,
    display_name: displayName,
    description: "",
    instructions: "",
    model_id: null,
    tool_permissions: {},
    skills: [],
    output_schema: null,
    output_max_retries: null,
    is_builtin: isBuiltin,
    source_scope: "global",
  };
}

describe("AgentPicker", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
  });

  it("lists every returned definition by display name and leaves the default selected", () => {
    component = mount(AgentPicker, {
      target: document.body,
      props: {
        definitions: [
          definition("default", "Swage default", true),
          definition("reviewer", "Review companion", true),
          definition("project", "Project agent", false),
        ],
      },
    });

    const picker = document.querySelector<HTMLSelectElement>('select[aria-label="Agent"]');
    expect(picker).not.toBeNull();
    expect(picker!.value).toBe("");
    expect(Array.from(picker!.options, (option) => option.textContent)).toEqual([
      "Swage default",
      "Review companion",
      "Project agent",
    ]);
  });
});
