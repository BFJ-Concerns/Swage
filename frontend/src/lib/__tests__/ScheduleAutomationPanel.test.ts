// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import ScheduleAutomationPanel from "../components/ScheduleAutomationPanel.svelte";
import * as workflowApi from "../api/workflows";
import * as operatorApi from "../api/operator";
import type { Schedule } from "../protocol/Schedule";
import type { ScheduleRun } from "../protocol/ScheduleRun";

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

const SCHEDULE: Schedule = {
  id: "schedule-1",
  message: "Review the queue.",
  status: "active",
  spec: { kind: "interval", every: "every 1 hours" },
  session: { policy: "fresh", space_id: "space-1", agent_id: "gateway" },
  creator: { kind: "user" },
  routing: {
    deliver_to_origin: false,
    home_target: null,
    explicit_targets: [],
    store_local_output: true,
  },
  gate: { model: "cheap", criteria: null },
  next_run_at: "2026-01-01T01:00:00Z",
  last_run_at: null,
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

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

  it("lists schedules with their status and run history", async () => {
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { spaceId: "space-1", sessionId: "session-1" },
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
      props: { spaceId: "space-1", sessionId: "session-1" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("by you");
    expect(document.body.textContent).toContain("by agent researcher");
  });

  it("shows empty successful runs as a visible non-outcome", async () => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [EMPTY_RUN] });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { spaceId: "space-1", sessionId: "session-1" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("completed empty");
    expect(document.body.textContent).toContain("No assistant text produced.");
  });

  it("runs a schedule now through the trigger endpoint", async () => {
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { spaceId: "space-1", sessionId: "session-1" },
    });
    await flushUi();

    clickButton("Run now");
    await flushUi();

    expect(operatorApi.triggerSchedule).toHaveBeenCalledWith("schedule-1");
  });

  it("blocks a ticked gate with a blank model before submitting", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [] });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { spaceId: "space-1", sessionId: "session-1" },
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
