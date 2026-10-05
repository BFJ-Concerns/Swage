// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, tick, unmount } from "svelte";

import type { RecentSession } from "../protocol/RecentSession";
import type { RecentSessionsEntry } from "../protocol/RecentSessionsEntry";

const listRecentSessions = vi.fn<() => Promise<RecentSessionsEntry[]>>();

vi.mock("../api/sessions", () => ({
  listRecentSessions: () => listRecentSessions(),
}));

import RecentSessions from "../components/RecentSessions.svelte";

type MountedComponent = ReturnType<typeof mount>;

const BRIEF =
  "Check the nightly build at /home/ops/builds/nightly/artefacts/reports/summary and post the result to the team\nThen tidy up.";

function run(index: number, flags: Partial<RecentSession> = {}): RecentSession {
  return {
    id: `run-${index}`,
    topic_name: "General",
    agent_id: "default",
    origin: { kind: "schedule_run", schedule_id: "sched-1" } as RecentSession["origin"],
    created_at: "2026-10-05T09:00:00Z",
    last_activity: "2026-10-05T09:00:00Z",
    message_preview: `Run ${index}`,
    has_interrupted_turn: false,
    has_running_turn: false,
    ...flags,
  };
}

function scheduleEntry(latest: Partial<RecentSession> = {}): RecentSessionsEntry {
  const automated = Array.from({ length: 11 }, (_, index) => run(index, index === 0 ? latest : {}));
  return {
    source: { kind: "schedule", id: "sched-1", message: BRIEF },
    last_activity: "2026-10-05T09:00:00Z",
    automated,
  };
}

async function render(entries: RecentSessionsEntry[]): Promise<MountedComponent> {
  listRecentSessions.mockResolvedValue(entries);
  const component = mount(RecentSessions, { target: document.body, props: {} });
  await tick();
  await Promise.resolve();
  flushSync();
  return component;
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
    const summary = document.querySelector(".group-summary");
    expect(summary?.textContent).toContain("11 runs");
    expect(summary?.textContent).toContain("Running");

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
    expect(document.querySelector(".group-summary")?.textContent ?? "").toContain("Interrupted");
    unmount(component);
    document.body.replaceChildren();

    component = await render([scheduleEntry()]);
    const summary = document.querySelector(".group-summary")?.textContent ?? "";
    expect(summary).toContain("Last run ended");
    expect(summary).not.toMatch(/complete|finished/i);
  });

  it("remembers each group's choice across visits", async () => {
    const conversation: RecentSessionsEntry = {
      source: { kind: "conversation", session: { ...run(99), id: "conv-1", origin: { kind: "direct" } as RecentSession["origin"] } },
      last_activity: "2026-10-05T08:00:00Z",
      automated: [run(100)],
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
