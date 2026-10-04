// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import ScheduleAutomationPanel from "../components/ScheduleAutomationPanel.svelte";
import * as workflowApi from "../api/workflows";
import * as operatorApi from "../api/operator";
import type { Schedule } from "../protocol/Schedule";
import type { ScheduleRun } from "../protocol/ScheduleRun";
import { makeSchedule } from "./fixtures/schedules";

vi.mock("../api/workflows", () => ({
  createSchedule: vi.fn(),
}));

vi.mock("../api/operator", () => ({
  listSchedules: vi.fn(),
  triggerSchedule: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  fetchScheduleRuns: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

function clickButton(label: string): void {
  const button = [...document.querySelectorAll<HTMLButtonElement>("button")].find((candidate) =>
    candidate.textContent?.includes(label),
  );
  if (!button) throw new Error(`missing button ${label}`);
  button.click();
}

function setFieldValue(field: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  field.value = value;
  field.dispatchEvent(new Event("input", { bubbles: true }));
}

const SCHEDULE: Schedule = makeSchedule({ gate: { model: "cheap", criteria: null } });

const RUN: ScheduleRun = {
  id: "run-1",
  schedule_id: "schedule-1",
  trigger: "manual",
  state: "completed",
  skip_reason: null,
  scheduled_for: null,
  session_id: "session-1",
  turn_id: "turn-1",
  gate: null,
  output_text: "did the thing",
  error_message: null,
  created_at: "2026-01-01T00:30:00Z",
  started_at: "2026-01-01T00:30:00Z",
  completed_at: "2026-01-01T00:30:05Z",
};

const EMPTY_RUN: ScheduleRun = {
  ...RUN,
  id: "run-empty",
  state: "completed_empty",
  output_text: "",
};

describe("ScheduleAutomationPanel", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [SCHEDULE] });
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [RUN] });
    vi.mocked(operatorApi.triggerSchedule).mockResolvedValue({
      run: { ...RUN, id: "run-2" },
    });
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }
    vi.clearAllMocks();
  });

  it.each([false, true])("hides a paused schedule's next run while retaining an active schedule's (gated: %s)", async (gated) => {
    const nextRun = "2026-01-01T01:00:00Z";
    const paused = makeSchedule({
      id: "paused-schedule",
      message: "Paused reminder",
      status: "paused",
      spec: { kind: "interval", every: "every 1 minutes" },
      gate: gated ? { model: "cheap", criteria: null } : null,
      next_run_at: nextRun,
    });
    const active = {
      ...paused,
      id: "active-schedule",
      message: "Active reminder",
      status: "active" as const,
    };
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [paused, active] });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    const rows = [...document.querySelectorAll(".schedule-row")];
    const pausedRow = rows.find((row) => row.textContent?.includes("Paused reminder"));
    const activeRow = rows.find((row) => row.textContent?.includes("Active reminder"));
    expect(pausedRow).toBeDefined();
    expect(activeRow).toBeDefined();
    expect(activeRow?.textContent).toContain(new Date(nextRun).toLocaleString());
    expect(pausedRow?.textContent).not.toContain(new Date(nextRun).toLocaleString());
    expect(pausedRow?.textContent).not.toContain("next");
  });

  it("lists schedules with their status and run history", async () => {
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    expect(operatorApi.listSchedules).toHaveBeenCalled();
    expect(document.body.textContent).toContain("Review the queue.");
    expect(document.body.textContent).toContain("did the thing");
  });

  it("attributes each schedule to its creator", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({
      schedules: [
        SCHEDULE,
        {
          ...SCHEDULE,
          id: "schedule-2",
          message: "Agent sweep.",
          creator: { kind: "agent", agent_id: "researcher" },
        },
      ],
    });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("by you");
    expect(document.body.textContent).toContain("by agent researcher");
  });

  it("shows the complete fresh-session target from another selected topic", async () => {
    // Agent `gateway` creates a schedule that runs as a different agent.
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({
      schedules: [
        {
          ...SCHEDULE,
          session: { policy: "fresh", topic_id: "target-topic", agent_id: "researcher" },
          creator: { kind: "agent", agent_id: "gateway" },
        },
      ],
    });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: {
        topicId: "viewing-topic",
        sessionId: "viewing-session",
        topics: [
          {
            id: "target-topic",
            name: "Research",
            slug: "research",
            description: "",
            is_default: false,
            created_at: "2026-01-01T00:00:00Z",
          },
        ],
      },
    });
    await flushUi();

    // The execution target and creator are both explicit, even though this
    // operator-level schedule is being viewed from a different topic.
    expect(document.body.textContent).toContain("topic Research · agent researcher");
    expect(document.body.textContent).toContain("by agent gateway");
  });

  it("falls back to a fresh-session target's topic ID when its name is unavailable", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({
      schedules: [
        {
          ...SCHEDULE,
          session: { policy: "fresh", topic_id: "unknown-topic", agent_id: "gateway" },
        },
      ],
    });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "viewing-topic", sessionId: "viewing-session", topics: [] },
    });
    await flushUi();

    expect(document.body.textContent).toContain("topic unknown-topic · agent gateway");
  });

  it("shows the specific continuation-session target", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({
      schedules: [
        {
          ...SCHEDULE,
          session: { policy: "continuation", session_id: "session-target" },
        },
      ],
    });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "viewing-topic", sessionId: "viewing-session" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("continues session session-target");
  });

  it("shows empty successful runs as a visible non-outcome", async () => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [EMPTY_RUN] });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("completed empty");
    expect(document.body.textContent).toContain("No assistant text produced.");
  });

  it("runs a schedule now through the trigger endpoint", async () => {
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    clickButton("Run now");
    await flushUi();

    expect(operatorApi.triggerSchedule).toHaveBeenCalledWith("schedule-1");
  });

  it("creates a fresh schedule without a topic from the general scope", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [] });
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(SCHEDULE);
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: null, sessionId: null },
    });
    await flushUi();

    setFieldValue(
      document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!,
      "Check the queue.",
    );
    clickButton("Create schedule");
    await flushUi();

    const request = vi.mocked(workflowApi.createSchedule).mock.calls[0]?.[0];
    expect(request?.session).toEqual({ policy: "fresh", agent_id: "gateway" });
  });

  it("sends the files a gate should look at, one per line, relative to the workspace", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [] });
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(SCHEDULE);
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    setFieldValue(
      document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!,
      "Work the inbox.",
    );
    const gateToggle = document.querySelector<HTMLInputElement>('input[type="checkbox"]');
    if (!gateToggle) throw new Error("missing gate toggle");
    gateToggle.click();
    await flushUi();
    setFieldValue(
      document.querySelector<HTMLInputElement>('input[placeholder="model id or alias"]')!,
      "cheap",
    );
    setFieldValue(
      document.querySelector<HTMLTextAreaElement>('textarea[placeholder^="inbox/todo.txt"]')!,
      "inbox/todo.txt\n\n  notes.md  \n",
    );
    clickButton("Create schedule");
    await flushUi();

    const request = vi.mocked(workflowApi.createSchedule).mock.calls[0]?.[0];
    expect(request?.gate).toEqual({
      model: "cheap",
      criteria: null,
      observations: [
        { kind: "file", path: "inbox/todo.txt" },
        { kind: "file", path: "notes.md" },
      ],
    });
  });

  it("blocks a ticked gate with a blank model before submitting", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [] });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    setFieldValue(
      document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!,
      "Do a thing.",
    );
    const gateToggle = document.querySelector<HTMLInputElement>('input[type="checkbox"]');
    if (!gateToggle) throw new Error("missing gate toggle");
    gateToggle.click();
    await flushUi();
    clickButton("Create schedule");
    await flushUi();

    expect(workflowApi.createSchedule).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("Enter a gate model");
  });
});
