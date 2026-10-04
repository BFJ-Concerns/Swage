// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

vi.mock("../api/operator", () => ({
  fetchAuditEvents: vi.fn(async () => ({ events: [], next_cursor: null })),
  fetchOperatorHistory: vi.fn(),
}));
vi.mock("../api/turns", () => ({
  fetchTurnContext: vi.fn(),
}));

import { fetchAuditEvents, fetchOperatorHistory } from "../api/operator";
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
  it("reads the audit record by the filters the operator sets and pages older events", async () => {
    vi.mocked(fetchOperatorHistory).mockResolvedValue({
      configuration_changes: [],
      variant_selections: [],
      assembly_reports: [],
    });
    const event = (id: string, kind: string, createdAt: string) => ({
      id,
      session_id: "session-1",
      entity_kind: "surface",
      entity_id: "s-1",
      event_kind: kind,
      summary: `${kind} on surface s-1`,
      metadata: { module_id: "m-1" },
      created_at: createdAt,
    });
    vi.mocked(fetchAuditEvents)
      .mockResolvedValueOnce({
        events: [event("e2", "surface_module_written", "2026-10-04T10:02:00Z")],
        next_cursor: { created_at: "2026-10-04T10:02:00Z", id: "e2" },
      })
      .mockResolvedValueOnce({
        events: [event("e1", "surface_created", "2026-10-04T10:01:00Z")],
        next_cursor: null,
      })
      .mockResolvedValueOnce({
        events: [event("e3", "surface_annotation_created", "2026-10-04T10:03:00Z")],
        next_cursor: null,
      });

    component = mount(OperatorHistoryPane, { target: document.body });

    await vi.waitFor(() => expect(document.querySelector(".audit-entry")).not.toBeNull());
    expect(fetchAuditEvents).toHaveBeenLastCalledWith({
      sessionId: undefined,
      entityKind: undefined,
      entityId: undefined,
      eventKind: undefined,
      before: undefined,
      limit: 50,
    });
    expect(document.body.textContent).toContain("surface_module_written · surface · s-1");
    expect(document.body.textContent).toContain('"module_id": "m-1"');

    const older = Array.from(document.querySelectorAll("button")).find((button) =>
      button.textContent?.includes("Load older events"),
    );
    expect(older).toBeDefined();
    older?.click();
    await vi.waitFor(() => expect(document.querySelectorAll(".audit-entry")).toHaveLength(2));
    expect(fetchAuditEvents).toHaveBeenLastCalledWith(
      expect.objectContaining({ before: { created_at: "2026-10-04T10:02:00Z", id: "e2" }, limit: 50 }),
    );
    expect(
      Array.from(document.querySelectorAll("button")).some((button) =>
        button.textContent?.includes("Load older events"),
      ),
    ).toBe(false);

    const form = document.querySelector('form[aria-label="Audit record filters"]') as HTMLFormElement;
    const kindInput = form.querySelector('input[placeholder="configuration_change…"]') as HTMLInputElement;
    kindInput.value = "surface_annotation_created";
    kindInput.dispatchEvent(new Event("input", { bubbles: true }));
    const sessionInput = form.querySelector('input[placeholder="session id"]') as HTMLInputElement;
    sessionInput.value = " session-1 ";
    sessionInput.dispatchEvent(new Event("input", { bubbles: true }));
    form.requestSubmit();
    await vi.waitFor(() =>
      expect(fetchAuditEvents).toHaveBeenLastCalledWith(
        expect.objectContaining({ eventKind: "surface_annotation_created", sessionId: "session-1" }),
      ),
    );
    await vi.waitFor(() => expect(document.body.textContent).toContain("surface_annotation_created · surface"));
    expect(document.querySelectorAll(".audit-entry")).toHaveLength(1);
  });

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
        topic_id: "topic-1",
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
      declared_omissions: [],
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
    expect(document.querySelector(".summary")?.closest(".history-entry")).toBeNull();
    expect(document.body.textContent).toContain("[REDACTED]");
    expect(document.body.textContent).not.toContain(secretValue);
    (document.querySelector('button[aria-label="Close"]') as HTMLButtonElement).click();
    await vi.waitFor(() => expect(document.querySelector(".context-inspector")).toBeNull());
  });
});
