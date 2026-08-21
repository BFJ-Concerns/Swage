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
});
