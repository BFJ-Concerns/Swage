// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import TurnGraphPanel from "../components/TurnGraphPanel.svelte";
import type { TurnGraph } from "../protocol/TurnGraph";
import type { TurnGraphResponse } from "../protocol/TurnGraphResponse";

type MountedComponent = ReturnType<typeof mount>;

const shipped: TurnGraph = {
  steps: [
    { id: "context_program", slot: "context_program", filler: { kind: "builtin" } },
    { id: "succession", slot: "succession", filler: { kind: "builtin" } },
  ],
  hooks: [],
  branches: [],
};

const response: TurnGraphResponse = {
  active: 1,
  revisions: [
    { revision: 0, graph: shipped, activation: "superseded", failure: null, created_at: "2026-10-16T20:00:00Z" },
    {
      revision: 1,
      graph: {
        steps: [shipped.steps[0], { id: "succession", slot: "succession", filler: { kind: "plugin", name: "summariser" } }],
        hooks: [{ point: "tool_call", plugin: "guard", mode: "veto" }],
        branches: [{ id: "trial", from: "succession", steps: [{ id: "trial-succession", slot: "succession", filler: { kind: "builtin" } }] }],
      },
      activation: "active",
      failure: null,
      created_at: "2026-10-16T20:01:00Z",
    },
    {
      revision: 2,
      graph: { steps: [...shipped.steps, shipped.steps[1]], hooks: [], branches: [] },
      activation: "failed",
      failure: "step 'succession' is used twice",
      created_at: "2026-10-16T20:02:00Z",
    },
  ],
};

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
}

describe("TurnGraphPanel", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("lists every revision with how it ended and what it changed, and lets an earlier one serve", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url.endsWith("/api/turn-graph")) return jsonResponse(response);
      if (url.includes("/diff/0/1")) {
        return jsonResponse({
          from: 0, to: 1, steps_added: [], steps_removed: [], steps_reordered: [],
          steps_refilled: [{ id: "succession", before: { kind: "builtin" }, after: { kind: "plugin", name: "summariser" } }],
          hooks_attached: [{ point: "tool_call", plugin: "guard", mode: "veto" }], hooks_detached: [], hooks_reordered: false,
          branches_added: [{ id: "trial", from: "succession", steps: [] }], branches_removed: [],
        });
      }
      if (url.includes("/diff/1/2")) {
        return jsonResponse({ from: 1, to: 2, steps_added: [], steps_removed: [], steps_reordered: [], steps_refilled: [], hooks_attached: [], hooks_detached: [], hooks_reordered: false, branches_added: [], branches_removed: [] });
      }
      if (init?.method === "POST" && url.endsWith("/revisions/0/activate")) {
        return jsonResponse({ ...response.revisions[0], activation: "active" });
      }
      return jsonResponse(response);
    });
    component = mount(TurnGraphPanel, { target: document.body });
    await vi.waitFor(() => expect(document.body.textContent).toContain("Revision 2"));

    const list = document.querySelector('[aria-label="Turn graph revisions"]')!;
    expect(list.textContent).toContain("Serving");
    expect(list.textContent).toContain("Failed: step 'succession' is used twice");
    await vi.waitFor(() => expect(list.textContent).toContain("succession: the kernel's own → plugin summariser"));
    expect(list.textContent).toContain("hook guard at tool_call (veto)");
    expect(list.textContent).toContain("branch trial from succession");

    const detail = document.querySelector('[aria-label="Revision 1 details"]')!;
    expect(detail.textContent).toContain("succession: plugin summariser");
    expect(detail.textContent).toContain("guard at tool_call — veto");
    expect(detail.textContent).toContain("a shadow, recorded and never served");
    expect(detail.querySelector("button.btn")).toBeNull();

    // A failed revision shows what it declared, duplicate step id and all.
    const two = Array.from(list.querySelectorAll("button")).find((button) => button.textContent?.includes("Revision 2"))!;
    flushSync(() => two.click());
    const twoDetail = document.querySelector('[aria-label="Revision 2 details"]')!;
    expect(twoDetail.querySelectorAll("ol.plain li")).toHaveLength(3);

    const zero = Array.from(list.querySelectorAll("button")).find((button) => button.textContent?.includes("Revision 0"))!;
    flushSync(() => zero.click());
    const zeroDetail = document.querySelector('[aria-label="Revision 0 details"]')!;
    const serve = zeroDetail.querySelector<HTMLButtonElement>("button.btn")!;
    expect(serve.textContent).toContain("Make it serve");
    flushSync(() => serve.click());
    await vi.waitFor(() => {
      const activated = fetchSpy.mock.calls.find(([, init]) => init?.method === "POST");
      expect(activated?.[0]).toContain("/api/turn-graph/revisions/0/activate");
    });
  });
});
