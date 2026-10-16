// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

vi.mock("../api/turns", () => ({
  fetchTurnContext: vi.fn(),
}));

import { fetchTurnContext } from "../api/turns";
import TurnContextInspector from "../components/TurnContextInspector.svelte";

let component: ReturnType<typeof mount> | undefined;

afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.replaceChildren();
  vi.resetAllMocks();
});

describe("TurnContextInspector", () => {
  it("labels per-element token counts as non-additive estimates", async () => {
    vi.mocked(fetchTurnContext).mockResolvedValue({
      assembly_name: "default",
      elements: [
        {
          name: "orientation",
          source: "built-in identity",
          token_count: 120,
          preview: null,
          priority: 110,
          named_assembly: null,
        },
      ],
      total_tokens: 500,
      budget: null,
      budget_remaining: null,
      compaction_applied: [],
      warnings: [],
      errors: [],
      declared_omissions: [],
      effective_tool_permissions: undefined,
      provider_visible: undefined,
      duration: { secs: 0, nanos: 2_000_000 },
    });

    component = mount(TurnContextInspector, {
      target: document.body,
      props: { turnId: "turn-1", onClose: () => {} },
    });
    await vi.waitFor(() => {
      expect(document.querySelector(".summary")?.textContent).toContain(
        "per-element estimates below are non-additive",
      );
    });

    expect(document.querySelector(".summary")?.textContent).toContain(
      "do not sum to the request total",
    );
    expect(document.querySelector("th.numeric")?.textContent).toBe(
      "Estimated tokens",
    );
  });

  it("names the turn-graph revision the turn ran under and what served each slot", async () => {
    vi.mocked(fetchTurnContext).mockResolvedValue({
      assembly_name: "default",
      elements: [],
      total_tokens: 10,
      budget: null,
      budget_remaining: null,
      compaction_applied: [],
      warnings: [],
      errors: [],
      declared_omissions: [],
      effective_tool_permissions: undefined,
      provider_visible: undefined,
      turn_graph: {
        revision: 1,
        context_program: { filler: { kind: "builtin" }, served: "builtin", fallback: null },
        succession: {
          filler: { kind: "plugin", name: "marker-summariser" },
          summariser: "plugin marker-summariser",
          served: "plugin marker-summariser",
          fallback: null,
        },
        hooks: {
          attached: [{ point: "tool_call", plugin: "guard", mode: "veto" }],
          skipped: [{ point: "turn_end", plugin: "gone", mode: "observe" }],
        },
        branches: ["trial"],
      },
      duration: { secs: 0, nanos: 1_000_000 },
    });

    component = mount(TurnContextInspector, {
      target: document.body,
      props: { turnId: "turn-1", onClose: () => {} },
    });
    await vi.waitFor(() => {
      expect(document.querySelector(".turn-graph")).not.toBeNull();
    });
    const text = document.querySelector(".turn-graph")!.textContent!.replace(/\s+/g, " ");
    expect(text).toContain("Revision 1");
    expect(text).toContain("succession: plugin marker-summariser; served by plugin marker-summariser");
    expect(text).toContain("context program: the kernel's own; served by builtin");
    expect(text).toContain("guard at tool_call (veto)");
    expect(text).toContain("hooks skipped (plugin not serving): gone at turn_end");
    expect(text).toContain("branches: trial");
  });

  it("says when the succession step did not run this turn", async () => {
    vi.mocked(fetchTurnContext).mockResolvedValue({
      assembly_name: "default",
      elements: [],
      total_tokens: 10,
      budget: null,
      budget_remaining: null,
      compaction_applied: [],
      warnings: [],
      errors: [],
      declared_omissions: [],
      effective_tool_permissions: undefined,
      provider_visible: undefined,
      turn_graph: {
        revision: 0,
        context_program: { filler: { kind: "plugin", name: "composer" }, served: "builtin", fallback: "composer: the slot answered without messages" },
        succession: {
          filler: { kind: "plugin", name: "gone" },
          summariser: "builtin",
          served: null,
          fallback: "plugin 'gone' is not serving or not enabled for this scope",
        },
        hooks: { attached: [], skipped: [] },
        branches: [],
      },
      duration: { secs: 0, nanos: 1_000_000 },
    });

    component = mount(TurnContextInspector, {
      target: document.body,
      props: { turnId: "turn-2", onClose: () => {} },
    });
    await vi.waitFor(() => {
      expect(document.querySelector(".turn-graph")).not.toBeNull();
    });
    const text = document.querySelector(".turn-graph")!.textContent!.replace(/\s+/g, " ");
    expect(text).toContain("not run this turn");
    expect(text).toContain("fell back: composer: the slot answered without messages");
    expect(text).toContain(
      "succession: plugin gone; not run this turn — fell back: plugin 'gone' is not serving",
    );
  });

  it("names each declared omission and the reason given for it", async () => {
    vi.mocked(fetchTurnContext).mockResolvedValue({
      assembly_name: "focus",
      elements: [
        {
          name: "prompts",
          source: "resolved-instructions",
          token_count: 40,
          preview: null,
          priority: 100,
          named_assembly: null,
        },
      ],
      total_tokens: 40,
      budget: null,
      budget_remaining: null,
      compaction_applied: [],
      warnings: [],
      errors: [],
      declared_omissions: [
        {
          element: "working_norms",
          reason: "the operator instructions carry their own",
        },
      ],
      effective_tool_permissions: undefined,
      provider_visible: undefined,
      duration: { secs: 0, nanos: 1_000_000 },
    });

    component = mount(TurnContextInspector, {
      target: document.body,
      props: { turnId: "turn-2", onClose: () => {} },
    });

    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("Declared omissions");
    });
    expect(document.body.textContent).toContain("working_norms");
    expect(document.querySelector(".reason")?.textContent).toBe(
      "the operator instructions carry their own",
    );
  });
});
