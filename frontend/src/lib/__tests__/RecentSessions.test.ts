// @vitest-environment jsdom
/**
 * The recent-sessions list: order, grouping beneath a source, the topic
 * filter as a membership change, the archived view, and the disclosure
 * that collapses a group's automated sessions.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, tick, unmount } from "svelte";

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
    localStorage.clear();
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

  it("names a schedule by its instruction and toggles its runs rather than opening it as a session", async () => {
    const run = makeSession({ id: "run-session", origin: { kind: "schedule_run", schedule_id: "sched-1" }, message_preview: "Inbox summarised" });
    vi.mocked(sessionsApi.listRecentSessions).mockResolvedValueOnce([
      { source: { kind: "schedule", id: "sched-1", message: "Summarise the inbox" }, last_activity: run.last_activity, automated: [run] },
    ]);
    const onSelectSession = vi.fn();

    component = mount(RecentSessions, { target: document.body, props: { onSelectSession } });
    await settle();

    const source = document.querySelector(".row.source") as HTMLButtonElement;
    expect(source.textContent).toContain("Summarise the inbox");
    expect(document.querySelector('[aria-label="Automated sessions"]')).toBeNull();
    flushSync(() => source.click());
    expect(onSelectSession).not.toHaveBeenCalled();
    expect(document.querySelector('[aria-label="Automated sessions"]')?.textContent).toContain("scheduled run");
  });
});

const BRIEF =
  "Check the nightly build at /home/ops/builds/nightly/artefacts/reports/summary and post the result to the team\nThen tidy up.";

function scheduleEntry(latest: Partial<RecentSession> = {}): RecentSessionsEntry {
  const automated = Array.from({ length: 11 }, (_, index) =>
    makeSession({
      id: `run-${index}`,
      origin: { kind: "schedule_run", schedule_id: "sched-1" },
      message_preview: `Run ${index}`,
      ...(index === 0 ? latest : {}),
    }),
  );
  return {
    source: { kind: "schedule", id: "sched-1", message: BRIEF },
    last_activity: "2026-10-05T09:00:00Z",
    automated,
  };
}

async function render(entries: RecentSessionsEntry[]): Promise<MountedComponent> {
  vi.mocked(sessionsApi.listRecentSessions).mockResolvedValue(entries);
  const component = mount(RecentSessions, { target: document.body, props: {} });
  await tick();
  await Promise.resolve();
  flushSync();
  return component;
}

// The disclosure line with its whitespace collapsed, read whole so a wrong
// count or state cannot pass as a substring.
function summaryText(): string {
  return (document.querySelector(".group-summary")?.textContent ?? "").replace(/\s+/g, " ").trim();
}

function childRows(): number {
  return document.querySelectorAll(".automated li").length;
}

function scheduleHeading(): HTMLButtonElement {
  const heading = document.querySelector<HTMLButtonElement>(".row.source.schedule");
  expect(heading).not.toBeNull();
  return heading!;
}

describe("RecentSessions schedule groups", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(sessionsApi.listRecentSessions).mockReset();
    localStorage.clear();
  });

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
  });

  it("starts collapsed with a count and the latest run's state, and toggles its runs from the heading", async () => {
    component = await render([scheduleEntry({ has_running_turn: true })]);

    expect(childRows()).toBe(0);
    expect(summaryText()).toBe("▸ 11 runs · Running");

    const heading = scheduleHeading();
    expect(heading.disabled).toBe(false);
    heading.click();
    flushSync();
    expect(childRows()).toBe(11);
    expect(heading.getAttribute("aria-expanded")).toBe("true");

    heading.click();
    flushSync();
    expect(childRows()).toBe(0);
  });

  it("labels the group by the brief's first line cut at a word boundary, keeping the full brief as its title", async () => {
    component = await render([scheduleEntry()]);

    const heading = scheduleHeading();
    const label = heading.querySelector(".preview")?.textContent ?? "";
    expect(label).toBe("Check the nightly build at…");
    expect(heading.title).toBe(BRIEF);
  });

  it("reports a run with neither flag as ended, not completed", async () => {
    component = await render([scheduleEntry({ has_interrupted_turn: true })]);
    expect(summaryText()).toBe("▸ 11 runs · Interrupted");
    unmount(component);
    document.body.replaceChildren();

    component = await render([scheduleEntry()]);
    expect(summaryText()).toMatch(/^▸ 11 runs · Last run ended \S.*$/);
    expect(summaryText()).not.toMatch(/complete|finished/i);
  });

  it("remembers each group's choice across visits", async () => {
    const conversation: RecentSessionsEntry = {
      source: { kind: "conversation", session: makeSession({ id: "conv-1" }) },
      last_activity: "2026-10-05T08:00:00Z",
      automated: [makeSession({ id: "run-100", origin: { kind: "schedule_run", schedule_id: "sched-1" } })],
    };
    component = await render([conversation]);
    expect(childRows()).toBe(1);
    document.querySelector<HTMLButtonElement>(".group-summary")?.click();
    flushSync();
    expect(childRows()).toBe(0);
    unmount(component);
    document.body.replaceChildren();

    component = await render([conversation]);
    expect(childRows()).toBe(0);
    unmount(component);
    document.body.replaceChildren();

    component = await render([scheduleEntry()]);
    expect(childRows()).toBe(0);
    scheduleHeading().click();
    flushSync();
    expect(childRows()).toBe(11);
    unmount(component);
    document.body.replaceChildren();

    component = await render([scheduleEntry()]);
    expect(childRows()).toBe(11);
  });
});
