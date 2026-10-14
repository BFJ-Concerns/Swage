// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import { compile } from "svelte/compiler";

import OperatorDashboard from "../components/OperatorDashboard.svelte";
import operatorDashboardSource from "../components/OperatorDashboard.svelte?raw";
import * as operatorApi from "../api/operator";
import type { OperatorOverview } from "../types";
import type { ScheduleRun } from "../protocol/ScheduleRun";
import { UNSETTLED_RUN_REFRESH_MS } from "../one-shot-runs";
import { makeSchedule } from "./fixtures/schedules";

type MountedComponent = ReturnType<typeof mount>;

vi.mock("../api/operator", () => ({
  fetchAuditEvents: vi.fn(async () => ({ events: [], next_cursor: null })),
  fetchOperatorOverview: vi.fn(),
  fetchOperatorHistory: vi.fn(),
  fetchScheduleRuns: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  triggerSchedule: vi.fn(),
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
        topic_id: "proj-1",
        topic_name: "default",
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
        makeSchedule({
          message: "Daily digest",
          spec: { kind: "interval", every: "every 5 minutes" },
          session: { policy: "continuation", session_id: "conv-1" },
          creator: { kind: "agent", agent_id: "researcher" },
          gate: { model: "cheap", criteria: null },
          next_run_at: null,
          created_at: "2026-04-07T12:00:00Z",
          updated_at: "2026-04-07T12:00:00Z",
        }),
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
    devices: [
      {
        device_id: "docker-a",
        label: "Build device",
        kind: "docker",
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

    document.head.querySelectorAll("style").forEach((style) => style.remove());
    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }
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
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(
      makeOverview({ schedules: { schedules: [paused, active] } }),
    );
    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const rows = [...document.querySelectorAll(".control-card")];
    const pausedRow = rows.find((row) => row.textContent?.includes("Paused reminder"));
    const activeRow = rows.find((row) => row.textContent?.includes("Active reminder"));
    expect(pausedRow).toBeDefined();
    expect(activeRow).toBeDefined();
    expect(activeRow?.textContent).toContain(new Date(nextRun).toLocaleString());
    expect(pausedRow?.textContent).not.toContain(new Date(nextRun).toLocaleString());
    expect(pausedRow?.textContent).not.toContain("next");
  });

  it("renders operator inventory from the overview payload", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(
      makeOverview(),
    );

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    expect(document.body.textContent).toContain("Daily digest");
    // Attribution is shown on this surface too, not just the automation panel.
    expect(document.body.textContent).toContain("created by agent researcher");
    expect(document.body.textContent).toContain("Allow shell command");
    expect(document.body.textContent).toContain("Build device");
    expect(document.body.textContent).toContain("dev_dm");
  });

  it("opens the read-only history pane on demand", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(makeOverview());
    vi.mocked(operatorApi.fetchOperatorHistory).mockResolvedValueOnce({
      configuration_changes: [],
      variant_selections: [],
      assembly_reports: [],
    });
    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const button = Array.from(document.querySelectorAll("button")).find(
      (candidate) => candidate.textContent?.includes("View history"),
    );
    expect(button).toBeDefined();
    button?.click();
    await settleUi();

    expect(operatorApi.fetchOperatorHistory).toHaveBeenCalledOnce();
    expect(document.querySelector('[aria-label="Operator history"]')).not.toBeNull();
    expect(document.body.textContent).toContain("No configuration changes recorded");
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
          executed_message: "Daily digest",
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

  it("explains an empty devices card and how to pair a device", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOverview({ devices: [] }));

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const devicesPanel = Array.from(document.querySelectorAll("article.panel")).find(
      (panel) => panel.querySelector(".panel-kicker")?.textContent?.trim() === "Devices",
    );
    expect(devicesPanel).toBeDefined();
    const emptyCopy = devicesPanel!.querySelector(".empty-copy")?.textContent ?? "";
    expect(emptyCopy).toContain("No devices are attached.");
    expect(emptyCopy).toContain("one-time pairing code");
    expect(emptyCopy).toContain("swage device connect");
    const command = devicesPanel!.querySelector(".empty-copy code") as HTMLElement | null;
    expect(command?.textContent).toBe("swage device connect");
    // jsdom loads no component CSS, so apply the component's compiled styles
    // under the scope class the mounted paragraph carries.
    const scope = Array.from(command!.parentElement!.classList).find((name) => name.startsWith("svelte-"));
    expect(scope).toBeDefined();
    const { css } = compile(operatorDashboardSource, { filename: "OperatorDashboard.svelte" });
    const style = document.createElement("style");
    style.textContent = (css?.code ?? "").replace(/svelte-[a-z0-9]+/g, scope!);
    document.head.appendChild(style);
    expect(getComputedStyle(command!.parentElement!).textTransform).toBe("uppercase");
    expect(getComputedStyle(command!).textTransform).toBe("none");
    expect(emptyCopy).toContain("Registration and pairing");
  });

  it("lists attached devices without the empty-state explanation", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOverview());

    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    expect(document.body.textContent).toContain("Build device");
    expect(document.body.textContent).not.toContain("No devices are attached.");
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
          executed_message: "Daily digest",
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

describe("OperatorDashboard approvals layout", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }

    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }
  });

  function panelByKicker(kicker: string): HTMLElement {
    const panel = [...document.querySelectorAll<HTMLElement>(".ops-grid > .panel")].find(
      (candidate) => candidate.querySelector(".panel-kicker")?.textContent === kicker,
    );
    expect(panel).toBeDefined();
    return panel as HTMLElement;
  }

  async function mountWith(overview: OperatorOverview): Promise<void> {
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(overview);
    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();
  }

  it("collapses an empty Approvals panel and gives Schedules the full grid width", async () => {
    await mountWith(makeOverview({ approvals: [] }));

    const approvals = panelByKicker("Approvals");
    const schedules = panelByKicker("Schedules");
    expect(approvals.classList.contains("panel-compact")).toBe(true);
    expect(approvals.classList.contains("panel-wide")).toBe(true);
    expect(schedules.classList.contains("panel-wide")).toBe(true);
    expect(approvals.textContent).toContain("Nothing is waiting for operator approval.");
  });

  it("restores the side-by-side Approvals panel while a decision is pending", async () => {
    await mountWith(makeOverview());

    const approvals = panelByKicker("Approvals");
    const schedules = panelByKicker("Schedules");
    expect(approvals.classList.contains("panel-compact")).toBe(false);
    expect(approvals.classList.contains("panel-wide")).toBe(false);
    expect(schedules.classList.contains("panel-wide")).toBe(false);
    expect(approvals.textContent).toContain("Allow shell command");
  });
});

describe("OperatorDashboard session summaries", () => {
  let component: MountedComponent | undefined;
  let stylesheet: HTMLStyleElement | undefined;

  beforeEach(() => vi.clearAllMocks());
  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    stylesheet?.remove();
    stylesheet = undefined;
    document.body.replaceChildren();
  });

  it.each([
    ["**Amber Cabinet** - **Size:** `120 × 60 cm`", "Amber Cabinet - Size: 120 × 60 cm"],
    ["## Ingredients\n\n| Name | ID |\n| --- | --- |\n| Lentils | 42 |", "Ingredients Name ID Lentils 42"],
    ["[Open child](swage://session/child-id)", "Open child"],
  ])("renders the session's latest message as plain text: %s", async (source, expected) => {
    const overview = makeOverview();
    overview.sessions[0].message_preview = source;
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(overview);
    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();
    expect(document.querySelector(".ledger-row .secondary-line")?.textContent?.trim().replace(/\s+/g, " ")).toBe(`default · ${expected}`);
  });

  it("applies a three-line clamp to a long session summary", async () => {
    // Vitest does not install Svelte's external CSS. Install the component's
    // compiled stylesheet so assertions read styles applied to its real DOM.
    const componentPath = "../components/OperatorDashboard.svelte";
    const compiled = compile(operatorDashboardSource, {
      filename: decodeURIComponent(new URL(componentPath, import.meta.url).pathname),
      generate: "client",
    });
    stylesheet = document.createElement("style");
    stylesheet.textContent = compiled.css?.code ?? "";
    document.head.append(stylesheet);
    const overview = makeOverview();
    overview.sessions[0].message_preview = Array.from({ length: 300 }, (_, i) => `Line ${i}: **${"Amber".repeat(50)}**`).join("\n");
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(overview);
    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();

    const summary = document.querySelector(".ledger-row .secondary-line")!;
    const style = getComputedStyle(summary);
    expect(style.getPropertyValue("-webkit-line-clamp")).toBe("3");
    expect(style.display).toBe("-webkit-box");
    expect(style.getPropertyValue("-webkit-box-orient")).toBe("vertical");
    expect(style.overflow).toBe("hidden");
    expect(style.getPropertyValue("overflow-wrap")).toBe("anywhere");
  });
});


describe("passed one-shot cards", () => {
  let component: MountedComponent | undefined;
  const schedule = makeSchedule({
    status: "completed",
    spec: { kind: "one_shot", at: "2026-01-01T00:00:00Z" },
    next_run_at: null,
  });
  const older: ScheduleRun = {
    id: "older", schedule_id: schedule.id, trigger: "tick", state: "completed",
    executed_message: schedule.message, created_at: "2026-01-01T00:00:00Z",
  };
  const latest: ScheduleRun = {
    ...older, id: "latest", state: "failed", created_at: "2026-01-01T00:01:00Z",
    error_message: "405 Method Not Allowed",
  };
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [older, latest] });
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOverview({ schedules: { schedules: [schedule] } }));
  });
  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
  });
  async function renderCard(): Promise<Element> {
    component = mount(OperatorDashboard, { target: document.body });
    await settleUi();
    return [...document.querySelectorAll(".control-card")].find(card => card.textContent?.includes(schedule.message))!;
  }
  it.each(["failed", "running", "completed_empty"] as const)("shows the newest %s run on the unopened card", async (state) => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [older, { ...latest, state }] });
    const card = await renderCard();
    expect(card.querySelector(".primary-line .badge")?.textContent?.trim()).toBe(state === "completed_empty" ? "completed empty" : state);
    expect(card.querySelector(".run-history")).toBeNull();
  });
  it("runs the passed one-shot again through the manual trigger", async () => {
    vi.mocked(operatorApi.triggerSchedule).mockResolvedValue({ run: { ...latest, state: "completed" } });
    const card = await renderCard();
    const buttons = [...card.querySelectorAll("button")];
    const runAgain = buttons.find(button => button.textContent?.trim() === "Run again");
    expect(runAgain).toBeDefined();
    expect(buttons.some(button => button.textContent?.trim() === "Resume")).toBe(false);
    runAgain!.click();
    await settleUi();
    expect(operatorApi.triggerSchedule).toHaveBeenCalledWith(schedule.id);
    expect(operatorApi.resumeSchedule).not.toHaveBeenCalled();
  });
  it("refreshes only passed one-shot histories with the overview", async () => {
    const recurring = makeSchedule({ id: "recurring" });
    const future = makeSchedule({ id: "future", spec: { kind: "one_shot", at: "2099-01-01T00:00:00Z" } });
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOverview({ schedules: { schedules: [schedule, recurring, future] } }));
    const card = await renderCard();
    expect(vi.mocked(operatorApi.fetchScheduleRuns).mock.calls).toEqual([[schedule.id]]);
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [older, { ...latest, state: "running" }] });
    [...document.querySelectorAll("button")].find(button => button.textContent?.trim() === "Refresh")!.click();
    await settleUi();
    expect(vi.mocked(operatorApi.fetchScheduleRuns).mock.calls).toEqual([[schedule.id], [schedule.id]]);
    expect(card.querySelector(".primary-line .badge")?.textContent?.trim()).toBe("running");
  });
  it("keeps the lifecycle badge when a passed one-shot has no runs", async () => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [] });
    const card = await renderCard();
    expect(card.querySelector(".primary-line .badge")?.textContent?.trim()).toBe("completed");
  });
  it("offers only Run again for an active passed one-shot", async () => {
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOverview({ schedules: { schedules: [{ ...schedule, status: "active" }] } }));
    const card = await renderCard();
    const actions = [...card.querySelectorAll(".action-row button")]
      .map(button => button.textContent?.trim())
      .filter(label => label !== "Runs" && label !== "Hide runs");
    expect(actions).toEqual(["Run again"]);
  });
  it.each(["running", "pending"] as const)("uses the same register for a %s card and its run row", async (state) => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [{ ...latest, state }] });
    const card = await renderCard();
    [...card.querySelectorAll("button")].find(button => button.textContent?.trim() === "Runs")!.click();
    await settleUi();
    const cardBadge = card.querySelector<HTMLElement>(".primary-line .badge")!;
    const runBadge = card.querySelector<HTMLElement>(".run-history .badge")!;
    expect(cardBadge.textContent?.trim()).toBe(state);
    expect(runBadge.textContent?.trim()).toBe(state);
    expect(cardBadge.style.getPropertyValue("--badge-hue")).toBe("var(--status-not-yet-hue)");
    expect(runBadge.style.getPropertyValue("--badge-hue")).toBe("var(--status-not-yet-hue)");
  });
  it("re-reads a running card's history until the run settles", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValueOnce({ runs: [older, { ...latest, state: "running" }] });
      component = mount(OperatorDashboard, { target: document.body });
      await vi.advanceTimersByTimeAsync(0);
      const badge = () => [...document.querySelectorAll(".control-card")]
        .find(card => card.textContent?.includes(schedule.message))
        ?.querySelector(".primary-line .badge")?.textContent?.trim();
      expect(badge()).toBe("running");
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(badge()).toBe("failed");
      const reads = vi.mocked(operatorApi.fetchScheduleRuns).mock.calls.length;
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS * 3);
      expect(vi.mocked(operatorApi.fetchScheduleRuns).mock.calls.length).toBe(reads);
    } finally {
      vi.useRealTimers();
    }
  });
  it("shows a manual rerun as running while its trigger is in flight", async () => {
    vi.useFakeTimers();
    try {
      let finish!: () => void;
      vi.mocked(operatorApi.triggerSchedule).mockReturnValue(new Promise((resolve) => {
        finish = () => resolve({ run: { ...latest, id: "rerun", state: "completed" } });
      }));
      component = mount(OperatorDashboard, { target: document.body });
      await vi.advanceTimersByTimeAsync(0);
      const status = () => [...document.querySelectorAll(".control-card")].find(card => card.textContent?.includes(schedule.message))?.querySelector(".primary-line .badge")?.textContent?.trim();
      expect(status()).toBe("failed");
      [...[...document.querySelectorAll(".control-card")].find(card => card.textContent?.includes(schedule.message))!.querySelectorAll("button")].find(button => button.textContent?.trim() === "Run again")!.click();
      await vi.advanceTimersByTimeAsync(0);
      const rerun: ScheduleRun = { ...latest, id: "rerun", state: "running", created_at: "2026-01-01T00:02:00Z" };
      vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [older, latest, rerun] });
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(status()).toBe("running");
      vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [older, latest, { ...rerun, state: "completed" }] });
      finish();
      await vi.advanceTimersByTimeAsync(0);
      expect(status()).toBe("completed");
    } finally {
      vi.useRealTimers();
    }
  });
  it("retries a failed re-read of a running card", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(operatorApi.fetchScheduleRuns)
        .mockResolvedValueOnce({ runs: [older, { ...latest, state: "running" }] })
        .mockRejectedValueOnce(new Error("gateway unreachable"));
      component = mount(OperatorDashboard, { target: document.body });
      await vi.advanceTimersByTimeAsync(0);
      const status = () => [...document.querySelectorAll(".control-card")].find(card => card.textContent?.includes(schedule.message))?.querySelector(".primary-line .badge")?.textContent?.trim();
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(document.body.textContent).toContain("gateway unreachable");
      expect(status()).toBe("running");
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(status()).toBe("failed");
    } finally {
      vi.useRealTimers();
    }
  });
});
