// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, tick, unmount } from "svelte";
import AgentDefinitionsPanel from "../components/AgentDefinitionsPanel.svelte";
import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";

type MountedComponent = ReturnType<typeof mount>;

function definition(
  id: string,
  source_scope: EffectiveAgentDefinitionResponse["source_scope"],
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
    is_builtin: source_scope === "built_in",
    source_scope,
    ...overrides,
  };
}

describe("AgentDefinitionsPanel", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("shows loading and request errors", async () => {
    let rejectRequest: ((reason: Error) => void) | undefined;
    vi.spyOn(globalThis, "fetch").mockImplementation(() => new Promise((_, reject) => {
      rejectRequest = reject;
    }));
    component = mount(AgentDefinitionsPanel, { target: document.body, props: { spaceId: "space-1" } });
    await tick();

    expect(document.body.textContent).toContain("Loading agent definitions");
    rejectRequest?.(new Error("registry unavailable"));
    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')).not.toBeNull());

    expect(document.querySelector('[role="alert"]')?.textContent).toContain("registry unavailable");
  });

  it("lists effective definitions with source labels and opens complete details", async () => {
    const spaceWinner = definition("shared", "space", {
      display_name: "Primary researcher",
      instructions: "Use primary sources.",
      model_id: "research/model",
      tool_permissions: { allow: ["web_search"] },
      skills: ["citations", "analysis"],
      output_schema: { type: "object", required: ["answer"] },
    });
    const definitions = [
      definition("gateway", "built_in", { display_name: "Standard assistant" }),
      spaceWinner,
      definition("global-only", "global", { display_name: "Shared helper" }),
    ];
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ definitions }), { status: 200 }),
    );
    component = mount(AgentDefinitionsPanel, { target: document.body, props: { spaceId: "space-1" } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("Standard assistant"));

    const sourceLabels = Array.from(document.querySelectorAll(".definition-list .source"), (label) => label.textContent);
    expect(sourceLabels).toEqual(["Built-in default", "This space", "Global"]);
    expect(document.body.textContent).toContain(spaceWinner.display_name);

    const spaceResearcher = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.includes("Primary researcher"),
    );
    flushSync(() => spaceResearcher?.click());

    const detail = document.querySelector('[aria-label="Primary researcher configuration"]');
    expect(detail?.textContent).toContain("Use primary sources.");
    expect(detail?.textContent).toContain("research/model");
    expect(detail?.textContent).toContain("web_search");
    expect(detail?.textContent).toContain("citations, analysis");
    expect(detail?.textContent).toContain('"required"');
    expect(detail?.querySelector("button, input, select, textarea")).toBeNull();
    expect(detail?.textContent).not.toContain("variant");
  });

  it("renders absent optional configuration explicitly", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({
      definitions: [definition("minimal", "global", { display_name: "Minimal" })],
    }), { status: 200 }));
    component = mount(AgentDefinitionsPanel, { target: document.body, props: { spaceId: "space-1" } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("Minimal"));

    const detail = document.querySelector('[aria-label="Minimal configuration"]');
    expect(detail?.textContent).toContain("Use the configured default");
    expect(detail?.textContent).toContain("None");
    expect(detail?.textContent?.match(/Not set/g)?.length).toBe(3);
  });

  it("does not fetch configuration without a selected space", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    component = mount(AgentDefinitionsPanel, { target: document.body, props: { spaceId: null } });

    expect(document.body.textContent).toContain("Select a space");
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
