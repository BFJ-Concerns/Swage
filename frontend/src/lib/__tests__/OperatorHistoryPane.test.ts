// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

vi.mock("../api/operator", () => ({
  fetchOperatorHistory: vi.fn(),
}));
vi.mock("../api/turns", () => ({
  fetchTurnContext: vi.fn(),
}));

import { fetchOperatorHistory } from "../api/operator";
import { fetchTurnContext } from "../api/turns";
import OperatorHistoryPane from "../components/OperatorHistoryPane.svelte";

let component: ReturnType<typeof mount> | undefined;

afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.replaceChildren();
  vi.resetAllMocks();
});

describe("OperatorHistoryPane", () => {
  it("shows a loading state while the history request is pending", () => {
    vi.mocked(fetchOperatorHistory).mockReturnValue(new Promise(() => {}));

    component = mount(OperatorHistoryPane, { target: document.body });

    expect(document.body.textContent).toContain("Loading recorded history");
  });

  it("shows request failures without inventing history", async () => {
    vi.mocked(fetchOperatorHistory).mockRejectedValue(new Error("history unavailable"));

    component = mount(OperatorHistoryPane, { target: document.body });

    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("history unavailable"));
    expect(document.querySelector(".configuration-entry")).toBeNull();
  });

  it("renders recorded state and attribution, then opens assembly detail in the turn inspector", async () => {
    const secretValue = "actual-secret";
    vi.mocked(fetchOperatorHistory).mockResolvedValue({
      configuration_changes: [{
        id: "change-1",
        resource_kind: "provider",
        resource_id: "openai",
        action: "updated",
        source: "settings_ui",
        session_id: "session-1",
        prior_state: { model: "old", api_key: "[REDACTED]" },
        new_state: { model: "new", api_key: "[REDACTED]" },
        recorded_at: "2026-08-23T10:00:00Z",
      }],
      variant_selections: [{
        id: "variant-1",
        location_id: "context_assembly",
        variant: "compact",
        reason: "drawn",
        turn_id: "turn-1",
        session_id: "session-1",
        recorded_at: "2026-08-23T10:01:00Z",
      }],
      assembly_reports: [{
        turn_id: "turn-1",
        session_id: "session-1",
        space_id: "space-1",
        recorded_at: "2026-08-23T10:02:00Z",
        assembly_name: "balanced",
      }],
    });
    vi.mocked(fetchTurnContext).mockResolvedValue({
      assembly_name: "balanced",
      elements: [{
        name: "provider request",
        source: "recorded context",
        token_count: 20,
        preview: "[REDACTED]",
        priority: 100,
        named_assembly: null,
      }],
      total_tokens: 321,
      budget: null,
      budget_remaining: null,
      compaction_applied: [],
      warnings: [],
      errors: [],
      effective_tool_permissions: undefined,
      provider_visible: { requests: [{ api_key: secretValue }] },
      duration: { secs: 0, nanos: 1 },
    });

    component = mount(OperatorHistoryPane, { target: document.body });

    await vi.waitFor(() => expect(document.querySelector(".configuration-entry")).not.toBeNull());
    const text = document.body.textContent ?? "";
    expect(text).toContain("provider · openai");
    expect(text).toContain("settings_ui");
    expect(text).toContain("session-1");
    expect(text).toContain('"model": "old"');
    expect(text).toContain('"model": "new"');
    expect(text).toContain("[REDACTED]");
    expect(text).not.toContain(secretValue);
    expect(text).toContain("context_assembly · compact");
    expect(text).toContain("balanced");
    expect(text).not.toContain("request total:");

    const inspectButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.includes("Inspect turn context"),
    );
    expect(inspectButton).toBeDefined();
    inspectButton?.click();

    await vi.waitFor(() => expect(fetchTurnContext).toHaveBeenCalledWith("turn-1"));
    expect(document.querySelector(".summary")?.textContent?.replace(/\s+/g, " ")).toContain(
      "request total: 321 tokens",
    );
    expect(document.body.textContent).toContain("[REDACTED]");
    expect(document.body.textContent).not.toContain(secretValue);
  });
});
