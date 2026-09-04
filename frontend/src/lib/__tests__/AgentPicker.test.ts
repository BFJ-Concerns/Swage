// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";

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

  it("addresses each definition by its own id and reports the chosen one", () => {
    let selectedAgentId = "";
    component = mount(AgentPicker, {
      target: document.body,
      props: {
        definitions: [
          definition("default", "Swage default", true),
          definition("reviewer", "Review companion", true),
          definition("project", "Project agent", false),
        ],
        // Getter/setter pair stands in for `bind:selectedAgentId`, so the
        // value the picker writes back is observable from the test.
        get selectedAgentId() {
          return selectedAgentId;
        },
        set selectedAgentId(value: string) {
          selectedAgentId = value;
        },
      },
    });

    const picker = document.querySelector<HTMLSelectElement>('select[aria-label="Agent"]')!;
    const values = Array.from(picker.options, (option) => option.value);
    // The empty value stands for the builtin default; every other option
    // carries its definition's id, so no two options select the same agent.
    expect(values).toEqual(["", "reviewer", "project"]);
    expect(new Set(values).size).toBe(values.length);

    flushSync(() => {
      picker.value = "project";
      picker.dispatchEvent(new Event("change", { bubbles: true }));
    });

    expect(selectedAgentId).toBe("project");
  });
});
