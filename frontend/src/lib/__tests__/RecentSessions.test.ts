// @vitest-environment jsdom
/**
 * The recent-sessions list: order, grouping beneath a source, the topic
 * filter as a membership change, and the archived view.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import RecentSessions from "../components/RecentSessions.svelte";
import * as sessionsApi from "../api/sessions";
import type { RecentSession } from "../protocol/RecentSession";
import type { RecentSessionsEntry } from "../protocol/RecentSessionsEntry";

vi.mock("../api/sessions", () => ({
  listRecentSessions: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

function makeSession(overrides: Partial<RecentSession> = {}): RecentSession {
  return {
    id: "conv-1",
    topic_name: "General",
    agent_id: "gateway",
    origin: { kind: "direct" },
    created_at: "2026-09-15T08:00:00Z",
    last_activity: "2026-09-15T08:00:00Z",
    message_preview: "Plan the release",
    has_interrupted_turn: false,
    has_running_turn: false,
    ...overrides,
  };
}

function conversationEntry(
  session: RecentSession,
  automated: RecentSession[] = [],
): RecentSessionsEntry {
  return {
    source: { kind: "conversation", session },
    last_activity: automated[0]?.last_activity ?? session.last_activity,
    automated,
  };
}

async function settle(): Promise<void> {
  for (let i = 0; i < 6; i += 1) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  flushSync();
}

function buttonNamed(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (!button) throw new Error(`Button not found: ${text}\n${document.body.textContent ?? ""}`);
  return button as HTMLButtonElement;
}

describe("RecentSessions", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(sessionsApi.listRecentSessions).mockReset();
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.replaceChildren();
  });

  it("lists every scope's sessions in the order the gateway gave, automated ones nested beneath their source", async () => {
    const worker = makeSession({
      id: "worker-1",
      topic_name: "Research",
      topic_id: "topic-research",
      agent_id: "builder",
      origin: { kind: "workflow_worker", run_id: "run-1" },
      message_preview: null,
      last_activity: "2026-09-15T09:00:00Z",
    });
    vi.mocked(sessionsApi.listRecentSessions).mockResolvedValueOnce([
      conversationEntry(
        makeSession({ id: "conv-dispatcher", topic_name: "Research", topic_id: "topic-research", message_preview: "Ship it" }),
        [worker],
      ),
      conversationEntry(makeSession({ id: "conv-general", message_preview: "Quiet chat" })),
    ]);
    const onSelectSession = vi.fn();

    component = mount(RecentSessions, {
      target: document.body,
      props: { topics: [{ id: "topic-research", name: "Research", description: "", slug: "research", created_at: "" }], onSelectSession },
    });
    await settle();

    expect(sessionsApi.listRecentSessions).toHaveBeenCalledWith(null, false);
    const previews = Array.from(document.querySelectorAll(".row.source .preview")).map((el) => el.textContent?.trim());
    expect(previews).toEqual(["Ship it", "Quiet chat"]);
    const nested = document.querySelector('[aria-label="Automated sessions"]');
    expect(nested?.textContent).toContain("workflow worker");
    expect(nested?.textContent).toContain("builder");

    flushSync(() => (nested?.querySelector("button") as HTMLButtonElement).click());
    expect(onSelectSession).toHaveBeenCalledWith("worker-1");
  });

  it("narrows to a topic with the filter and returns to all sessions", async () => {
    vi.mocked(sessionsApi.listRecentSessions)
      .mockResolvedValueOnce([conversationEntry(makeSession({ id: "a" })), conversationEntry(makeSession({ id: "b", topic_id: "topic-ops", topic_name: "Ops" }))])
      .mockResolvedValueOnce([conversationEntry(makeSession({ id: "b", topic_id: "topic-ops", topic_name: "Ops" }))])
      .mockResolvedValueOnce([conversationEntry(makeSession({ id: "a" })), conversationEntry(makeSession({ id: "b", topic_id: "topic-ops", topic_name: "Ops" }))]);

    component = mount(RecentSessions, {
      target: document.body,
      props: { topics: [{ id: "topic-ops", name: "Ops", description: "", slug: "ops", created_at: "" }] },
    });
    await settle();
    expect(document.querySelectorAll(".entry")).toHaveLength(2);

    flushSync(() => buttonNamed("Ops").click());
    await settle();
    expect(sessionsApi.listRecentSessions).toHaveBeenLastCalledWith("topic-ops", false);
    expect(document.querySelectorAll(".entry")).toHaveLength(1);
    expect(buttonNamed("Ops").getAttribute("aria-pressed")).toBe("true");

    flushSync(() => buttonNamed("All sessions").click());
    await settle();
    expect(sessionsApi.listRecentSessions).toHaveBeenLastCalledWith(null, false);
    expect(document.querySelectorAll(".entry")).toHaveLength(2);
  });

  it("shows the archived sessions on request and lets one be resumed from there", async () => {
    vi.mocked(sessionsApi.listRecentSessions)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([
        conversationEntry(makeSession({ id: "old", message_preview: "From last month", archived_at: "2026-09-01T00:00:00Z" })),
      ]);
    const onSelectSession = vi.fn();

    component = mount(RecentSessions, { target: document.body, props: { onSelectSession } });
    await settle();
    expect(document.body.textContent).toContain("No sessions yet.");

    flushSync(() => buttonNamed("Archived").click());
    await settle();
    expect(sessionsApi.listRecentSessions).toHaveBeenLastCalledWith(null, true);
    expect(document.querySelector(".entry.archived")).not.toBeNull();

    flushSync(() => (document.querySelector(".row.source") as HTMLButtonElement).click());
    expect(onSelectSession).toHaveBeenCalledWith("old");
  });

  it("names a schedule by its instruction and does not open it as a session", async () => {
    const run = makeSession({ id: "run-session", origin: { kind: "schedule_run", schedule_id: "sched-1" }, message_preview: "Inbox summarised" });
    vi.mocked(sessionsApi.listRecentSessions).mockResolvedValueOnce([
      { source: { kind: "schedule", id: "sched-1", message: "Summarise the inbox" }, last_activity: run.last_activity, automated: [run] },
    ]);
    const onSelectSession = vi.fn();

    component = mount(RecentSessions, { target: document.body, props: { onSelectSession } });
    await settle();

    const source = document.querySelector(".row.source") as HTMLButtonElement;
    expect(source.textContent).toContain("Summarise the inbox");
    expect(source.disabled).toBe(true);
    expect(document.querySelector('[aria-label="Automated sessions"]')?.textContent).toContain("scheduled run");
  });
});
