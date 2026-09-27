// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import AgentPicker from "../components/AgentPicker.svelte";
import { makeAgentDefinition } from "./fixtures/agent-definitions";

type MountedComponent = ReturnType<typeof mount>;

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
          makeAgentDefinition("default", "global", { display_name: "Swage default", is_builtin: true }),
          makeAgentDefinition("reviewer", "global", { display_name: "Review companion", is_builtin: true }),
          makeAgentDefinition("project", "global", { display_name: "Project agent", is_builtin: false }),
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
          makeAgentDefinition("default", "global", { display_name: "Swage default", is_builtin: true }),
          makeAgentDefinition("reviewer", "global", { display_name: "Review companion", is_builtin: true }),
          makeAgentDefinition("project", "global", { display_name: "Project agent", is_builtin: false }),
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
