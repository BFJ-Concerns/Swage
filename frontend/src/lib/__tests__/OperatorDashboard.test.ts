// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import OperatorDashboard from "../components/OperatorDashboard.svelte";
import * as operatorApi from "../api/operator";
import type { OperatorOverview } from "../types";

type MountedComponent = ReturnType<typeof mount>;

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  fetchScheduleRuns: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
}));

function makeOverview(overrides: Partial<OperatorOverview> = {}): OperatorOverview {
  return {
    gateway: {
      status: "running",
      health: "busy",
      shutdown_requested: false,
      seconds_since_activity: 5,
      active_turn_count: 2,
      active_connection_count: 1,
    },
    sessions: [
      {
        id: "conv-1",
        bench_id: "proj-1",
        bench_name: "default",
        created_at: "2026-04-07T12:00:00Z",
        last_activity: "2026-04-07T12:02:00Z",
        message_preview: "Busy session",
        has_interrupted_turn: false,
        has_active_connection: true,
        has_running_turn: true,
      },
    ],
    approvals: [
      {
        id: "approval-1",
        turn_id: "turn-1",
        agent_id: "gateway",
        kind: "tool",
        tool_name: "shell",
        prompt: "Allow shell command",
        timeout_at: "2026-04-07T12:10:00Z",
      },
    ],
    schedules: {
      schedules: [
        {
          id: "schedule-1",
          message: "Daily digest",
          status: "active",
          spec: { kind: "interval", every: "every 5 minutes" },
          session: { policy: "continuation", session_id: "conv-1" },
          routing: {
            deliver_to_origin: false,
            home_target: null,
            explicit_targets: [],
            store_local_output: true,
          },
          gate: { model: "cheap", criteria: null },
          next_run_at: null,
          last_run_at: null,
          created_at: "2026-04-07T12:00:00Z",
          updated_at: "2026-04-07T12:00:00Z",
        },
      ],
    },
    channels: {
      channels: [
        {
          integration: "dev_dm",
          status: "active",
          route_count: 1,
          delivery_count: 3,
          last_delivery_at: "2026-04-07T12:03:00Z",
        },
      ],
    },
    nodes: [
      {
        host_id: "docker-a",
        label: "Build node",
        kind: "docker",
        base_url: "http://127.0.0.1:4100",
        workspace_root: "/srv/work",
        capabilities: ["shell"],
        registered_at: "2026-04-07T12:00:00Z",
        state: "connected",
        last_seen_at: "2026-04-07T12:03:00Z",
        disconnected_at: null,
      },
    ],
    ...overrides,
  };
}

async function settleUi(): Promise<void> {
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await Promise.resolve();
}

describe("OperatorDashboard", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }

    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }
  });

  it("renders operator inventory from the overview payload", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(
      makeOverview(),
    );

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    expect(document.body.textContent).toContain("Daily digest");
    expect(document.body.textContent).toContain("Allow shell command");
    expect(document.body.textContent).toContain("Build node");
    expect(document.body.textContent).toContain("dev_dm");
  });

  it("runs approval and schedule actions from the operator deck", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview)
      .mockResolvedValueOnce(makeOverview())
      .mockResolvedValue(makeOverview());
    vi.mocked(operatorApi.approvePendingAction).mockResolvedValueOnce();
    vi.mocked(operatorApi.pauseSchedule).mockResolvedValueOnce();

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const buttons = Array.from(document.querySelectorAll("button"));
    const approve = buttons.find((button) => button.textContent?.includes("Approve"));
    const pause = buttons.find((button) => button.textContent?.includes("Pause"));

    expect(approve).toBeDefined();
    expect(pause).toBeDefined();

    flushSync(() => approve!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await settleUi();
    expect(operatorApi.approvePendingAction).toHaveBeenCalledWith(
      "approval-1",
      "operator deck approve",
    );

    flushSync(() => pause!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await settleUi();
    expect(operatorApi.pauseSchedule).toHaveBeenCalledWith("schedule-1");
  });

  it("inspects a schedule's recorded runs, gate verdicts included", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview)
      .mockResolvedValueOnce(makeOverview())
      .mockResolvedValue(makeOverview());
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValueOnce({
      runs: [
        {
          id: "run-1",
          schedule_id: "schedule-1",
          trigger: "tick",
          state: "skipped",
          skip_reason: "gate",
          scheduled_for: "2026-04-07T12:05:00Z",
          session_id: null,
          turn_id: null,
          gate: {
            model: "cheap",
            verdict: "skip",
            reasoning: "nothing new since the last digest",
          },
          output_text: null,
          error_message: null,
          created_at: "2026-04-07T12:05:00Z",
          started_at: null,
          completed_at: "2026-04-07T12:05:01Z",
        },
      ],
    });

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const runsButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Runs",
    );
    expect(runsButton).toBeDefined();
    flushSync(() => runsButton!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await settleUi();

    expect(operatorApi.fetchScheduleRuns).toHaveBeenCalledWith("schedule-1");
    expect(document.body.textContent).toContain("skipped");
    expect(document.body.textContent).toContain(
      "nothing new since the last digest",
    );
  });

  it("shows empty successful schedule runs as a visible non-outcome", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview)
      .mockResolvedValueOnce(makeOverview())
      .mockResolvedValue(makeOverview());
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValueOnce({
      runs: [
        {
          id: "run-empty",
          schedule_id: "schedule-1",
          trigger: "tick",
          state: "completed_empty",
          skip_reason: null,
          scheduled_for: "2026-04-07T12:05:00Z",
          session_id: "session-1",
          turn_id: "turn-1",
          gate: null,
          output_text: "",
          error_message: null,
          created_at: "2026-04-07T12:05:00Z",
          started_at: "2026-04-07T12:05:00Z",
          completed_at: "2026-04-07T12:05:01Z",
        },
      ],
    });

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const runsButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Runs",
    );
    expect(runsButton).toBeDefined();
    flushSync(() => runsButton!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await settleUi();

    expect(document.body.textContent).toContain("completed empty");
    expect(document.body.textContent).toContain("No assistant text produced.");
  });
});
