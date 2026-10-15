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
          full_text: null,
          priority: 110,
          named_assembly: null,
        },
      ],
      provider_attempts: [],
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

  it("names each declared omission and the reason given for it", async () => {
    vi.mocked(fetchTurnContext).mockResolvedValue({
      assembly_name: "focus",
      elements: [
        {
          name: "prompts",
          source: "resolved-instructions",
          token_count: 40,
          preview: null,
          full_text: null,
          priority: 100,
          named_assembly: null,
        },
      ],
      provider_attempts: [],
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

function recordedRequest() {
  return {
    assembly_name: "default", total_tokens: 350, budget: 2000, budget_remaining: 1650,
    elements: [{ name: "working_norms", source: "built-in working norms (GPT family)",
      token_count: 120, preview: "First paragraph.", full_text: "First paragraph.\n\nDistinctive second paragraph.", priority: 105, named_assembly: null }],
    compaction_applied: [], warnings: [], errors: [], declared_omissions: [],
    duration: { secs: 0, nanos: 0 },
    provider_visible: {
      effective_model: { model_id: "serving-model", provider_id: "serving-provider" },
      effort: { requested: "xhigh", clamped_to: "high", control: { kind: "level", level: "high" } },
      requests: [{ round: 1, request: { body: {
        messages: [{ role: "system", content: "Complete provider context" }],
        tools: [{ type: "function", function: { name: "read_workspace", parameters: { type: "object" } } }],
      }, divergences: [{ kind: "protocol_difference", text: "Uses Chat Completions" }],
      variant_id: "private-draw-identifier", variant_name: "private-draw-name" } }],
    },
    provider_attempts: [{ id: "recorded-attempt", round: 1, attempt: 1, started_at_ms: 1000, ended_at_ms: 2000,
      outcome: "failed" as const, error: "Recorded temporary failure" }],
  };
}

it.each([
  ["full provider request", ["Complete provider context", "read_workspace"]],
  ["complete element text", ["Distinctive second paragraph."]],
  ["effective request facts", ["serving-model", "serving-provider", "xhigh → high (clamped)", "Sent effort: high", "Configured budget: 2000", "Remaining: 1650"]],
  ["request divergences", ["Request divergences", "protocol_difference", "Uses Chat Completions"]],
  ["recorded provider attempts", ["attempt 1: failed", "1970-01-01T00:00:01.000Z", "1970-01-01T00:00:02.000Z", "Recorded temporary failure"]],
])("opens the %s from the recorded report", async (_label, expected) => {
  vi.mocked(fetchTurnContext).mockResolvedValue(recordedRequest());
  component = mount(TurnContextInspector, { target: document.body, props: { turnId: "recorded-turn", onClose: () => {} } });
  await vi.waitFor(() => expect(document.querySelector("table.elements")).not.toBeNull());
  for (const summary of document.querySelectorAll("summary")) (summary as HTMLElement).click();
  for (const text of expected) expect(document.body.textContent).toContain(text);
});

it("keeps assignment metadata out of an opened provider request", async () => {
  vi.mocked(fetchTurnContext).mockResolvedValue(recordedRequest());
  component = mount(TurnContextInspector, { target: document.body, props: { turnId: "recorded-turn", onClose: () => {} } });
  await vi.waitFor(() => expect(document.querySelector("details summary")).not.toBeNull());
  (document.querySelector("details summary") as HTMLElement).click();
  expect(document.body.textContent).toContain("Complete provider context");
  expect(document.body.textContent).not.toContain("private-draw-identifier");
  expect(document.body.textContent).not.toContain("private-draw-name");
});
