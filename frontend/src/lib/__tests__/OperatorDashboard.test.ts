// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import OperatorDashboard from "../components/OperatorDashboard.svelte";
import * as operatorApi from "../api/operator";
import type { OperatorOverview } from "../types";

type MountedComponent = ReturnType<typeof mount>;

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseScheduledJob: vi.fn(),
  resumeScheduledJob: vi.fn(),
  disableHeartbeat: vi.fn(),
  enableHeartbeat: vi.fn(),
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
    scheduled_jobs: {
      jobs: [
        {
          id: "job-1",
          session_id: "conv-1",
          message: "Daily digest",
          status: "active",
          schedule: { kind: "interval", every: "every 5 minutes" },
          routing: {},
          next_run_at: null,
          last_run_at: null,
        },
      ],
    },
    heartbeats: {
      heartbeats: [
        {
          id: "hb-1",
          session_id: "conv-1",
          message: "Check in",
          status: "disabled",
          interval_seconds: 90,
          next_beat_at: null,
          last_beat_at: null,
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

  it("runs approval and automation actions from the operator deck", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview)
      .mockResolvedValueOnce(makeOverview())
      .mockResolvedValue(makeOverview());
    vi.mocked(operatorApi.approvePendingAction).mockResolvedValueOnce();
    vi.mocked(operatorApi.pauseScheduledJob).mockResolvedValueOnce();
    vi.mocked(operatorApi.enableHeartbeat).mockResolvedValueOnce();

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const buttons = Array.from(document.querySelectorAll("button"));
    const approve = buttons.find((button) => button.textContent?.includes("Approve"));
    const pause = buttons.find((button) => button.textContent?.includes("Pause"));
    const restart = buttons.find((button) => button.textContent?.includes("Restart"));

    expect(approve).toBeDefined();
    expect(pause).toBeDefined();
    expect(restart).toBeDefined();

    flushSync(() => approve!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await settleUi();
    expect(operatorApi.approvePendingAction).toHaveBeenCalledWith(
      "approval-1",
      "operator deck approve",
    );

    flushSync(() => pause!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await settleUi();
    expect(operatorApi.pauseScheduledJob).toHaveBeenCalledWith("job-1");

    flushSync(() => restart!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await settleUi();
    expect(operatorApi.enableHeartbeat).toHaveBeenCalledWith("hb-1");
  });
});
