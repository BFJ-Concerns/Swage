// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import ScheduleAutomationPanel from "../components/ScheduleAutomationPanel.svelte";
import * as sessionsApi from "../api/sessions";
import * as surfaceApi from "../api/surfaces";
import * as agentApi from "../api/agent-definitions";
import { makeAgentDefinition } from "./fixtures/agent-definitions";
import * as workflowApi from "../api/workflows";
import * as operatorApi from "../api/operator";
import type { Schedule } from "../protocol/Schedule";
import type { ScheduleRun } from "../protocol/ScheduleRun";
import { UNSETTLED_RUN_REFRESH_MS } from "../one-shot-runs";
import { makeSchedule } from "./fixtures/schedules";

vi.mock("../api/sessions", () => ({ listRecentSessions: vi.fn(async () => []) }));
vi.mock("../api/surfaces", () => ({ listSurfaces: vi.fn(async () => ({ surfaces: [] })) }));
vi.mock("../api/agent-definitions", () => ({ listEffectiveAgentDefinitions: vi.fn() }));

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
  overlap_policy: "skip" as const,
  schedule_id: "schedule-1",
  trigger: "manual",
  state: "completed",
  skip_reason: null,
  executed_message: SCHEDULE.message,
  deliveries: [],
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
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockReset();
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockResolvedValue([makeAgentDefinition("gateway", "built_in")]);
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

  it("excludes platform-only continuation sessions from the picker", async () => {
    const session = {
      id: "ordinary-session", topic_id: "other-topic", topic_name: "Orchard", agent_id: "gateway",
      origin: { kind: "direct" as const }, created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z", message_preview: "Orchard review",
      has_interrupted_turn: false, has_running_turn: false,
    };
    vi.mocked(sessionsApi.listRecentSessions).mockResolvedValueOnce([{
      source: { kind: "conversation", session }, last_activity: session.last_activity,
      automated: [{ ...session, id: "curator-session", agent_id: "memory-curator" }],
    }]);
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockResolvedValue([
      makeAgentDefinition("gateway", "built_in"),
      makeAgentDefinition("memory-curator", "built_in", { selectable: false }),
    ]);
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: "curator-session" } });
    await flushUi();
    [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => /This conversation|Continue a conversation/.test(button.textContent ?? ""))?.click();
    await flushUi();
    const options = [...document.querySelectorAll<HTMLOptionElement>('select[aria-label="Run conversation"] option')].map((option) => option.value);
    expect(options).not.toContain("curator-session");
    expect(options).toContain("ordinary-session");
    expect(agentApi.listEffectiveAgentDefinitions).toHaveBeenCalledWith("other-topic");
    expect(document.querySelector<HTMLSelectElement>('select[aria-label="Run conversation"]')?.value).toBe("");
  });

  it("names the continuation policy independently of the selected conversation", async () => {
    const session = {
      id: "other-session", topic_name: "Orchard", agent_id: "gateway",
      origin: { kind: "direct" as const }, created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z", message_preview: "Orchard review",
      has_interrupted_turn: false, has_running_turn: false,
    };
    vi.mocked(sessionsApi.listRecentSessions).mockResolvedValueOnce([{
      source: { kind: "conversation", session }, last_activity: session.last_activity, automated: [],
    }]);
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: "current-session" } });
    await flushUi();
    const policy = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => /This conversation|Continue a conversation/.test(button.textContent ?? ""))!;
    policy.click();
    await flushUi();
    const selector = document.querySelector<HTMLSelectElement>('select[aria-label="Run conversation"]')!;
    selector.value = "other-session";
    selector.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    expect(selector.value).toBe("other-session");
    expect(policy.textContent).not.toContain("This conversation");
    expect(policy.textContent?.trim()).toBe("Continue a conversation");
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

  it("keeps long schedule headings short and distinct with the complete briefs in disclosures", async () => {
    const prefix = "Review the outstanding work and report each decision. ".repeat(9);
    const briefs = [prefix + "Check the orchard.", prefix + "Check the harbour."];
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: briefs.map((message, index) =>
      makeSchedule({ id: `distinct-${index}`, message })) });
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: null } });
    await flushUi();
    const headings = [...document.querySelectorAll(".schedule-main strong")].map((node) => node.textContent ?? "");
    expect(headings).toHaveLength(2);
    expect(headings.every((heading) => heading.length <= 100)).toBe(true);
    expect(new Set(headings).size).toBe(2);
    const disclosures = [...document.querySelectorAll(".schedule-main details")];
    expect(disclosures).toHaveLength(2);
    disclosures.forEach((node, index) => {
      expect(node.querySelector("summary")?.textContent).toContain("Full brief");
      expect(node.querySelector("p")?.textContent).toBe(briefs[index]);
    });
  });

  it.each([false, true])("sends explicit routing after changing local logging to %s", async (keepLocalLog) => {
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(SCHEDULE);
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: null } });
    await flushUi();
    setFieldValue(document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!, "Review the queue");
    const localLog = document.querySelector<HTMLInputElement>('input[aria-label="Keep a local log"]')!;
    localLog.click();
    if (keepLocalLog) localLog.click();
    clickButton("Create schedule");
    await flushUi();

    expect(workflowApi.createSchedule).toHaveBeenCalledWith(expect.objectContaining({
      routing: { deliver_to_origin: false, store_local_output: keepLocalLog, explicit_targets: [] },
    }));
  });

  it("sends the chosen topic, effective agent and webhook plus local delivery from the form", async () => {
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockImplementation(async (topic) => [
      makeAgentDefinition("gateway", "built_in"),
      ...(topic === "topic-b" ? [makeAgentDefinition("researcher", "topic", { display_name: "Research assistant" })] : []),
    ]);
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(SCHEDULE);
    component = mount(ScheduleAutomationPanel, { target: document.body, props: {
      topicId: null, sessionId: null, topics: [{ id: "topic-b", name: "Orchard", slug: "orchard", description: "", created_at: "2026-01-01T00:00:00Z" }],
    } });
    await flushUi();
    const topic = document.querySelector<HTMLSelectElement>('select[aria-label="Run topic"]');
    if (topic) topic.value = "topic-b";
    topic?.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    const agent = document.querySelector<HTMLSelectElement>('select[aria-label="Agent"]');
    if (agent) agent.value = "researcher";
    agent?.dispatchEvent(new Event("change", { bubbles: true }));
    setFieldValue(document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!, "Check the trees");
    [...document.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.includes("Add destination"))?.click();
    await flushUi();
    const destination = document.querySelector<HTMLSelectElement>('select[aria-label="Destination type"]')!;
    if (destination) destination.value = "webhook";
    destination?.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    const webhook = document.querySelector<HTMLInputElement>('input[aria-label="Webhook URL"]');
    if (webhook) setFieldValue(webhook, "https://example.test/results");
    clickButton("Create schedule");
    await flushUi();
    expect(workflowApi.createSchedule).toHaveBeenCalledWith(expect.objectContaining({
      session: { policy: "fresh", topic_id: "topic-b", agent_id: "researcher" },
      routing: { deliver_to_origin: false, store_local_output: true, explicit_targets: [
        { kind: "webhook", url: "https://example.test/results" },
      ] },
    }));
  });

  it("sends multiple explicit destinations including channel and the continuation's surface", async () => {
    vi.mocked(surfaceApi.listSurfaces).mockResolvedValue({ surfaces: [{ id: "11111111-1111-4111-8111-111111111111", title: "Digest", updated_at: "2026-01-01T00:00:00Z" }] });
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(SCHEDULE);
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: "session-1" } });
    await flushUi();
    setFieldValue(document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!, "Deliver the digest");
    clickButton("Continue a conversation");
    [...document.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.includes("Add destination"))?.click();
    await flushUi();
    const selectType = async (index: number, kind: string) => {
      const select = document.querySelectorAll<HTMLSelectElement>('select[aria-label="Destination type"]')[index];
      if (select) select.value = kind;
      select?.dispatchEvent(new Event("change", { bubbles: true }));
      await flushUi();
    };
    await selectType(0, "channel");
    { const field = document.querySelector<HTMLInputElement>('input[aria-label="Channel account"]'); if (field) setFieldValue(field, "personal"); }
    { const field = document.querySelector<HTMLInputElement>('input[aria-label="Channel conversation"]'); if (field) setFieldValue(field, "orchard-chat"); }
    [...document.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.includes("Add destination"))?.click();
    await flushUi();
    await selectType(1, "surface");
    const surface = document.querySelector<HTMLSelectElement>('select[aria-label="Conversation surface"]');
    if (surface) surface.value = "11111111-1111-4111-8111-111111111111";
    surface?.dispatchEvent(new Event("change", { bubbles: true }));
    clickButton("Create schedule");
    await flushUi();
    expect(workflowApi.createSchedule).toHaveBeenCalledWith(expect.objectContaining({
      session: { policy: "continuation", session_id: "session-1" },
      routing: { deliver_to_origin: false, store_local_output: true, explicit_targets: [
        { kind: "channel", integration: "telegram", account: "personal", conversation: "orchard-chat" },
        { kind: "surface", surface_id: "11111111-1111-4111-8111-111111111111" },
      ] },
    }));
  });

  it("clears an agent when the run topic changes and keeps default creation available", async () => {
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockResolvedValue([
      makeAgentDefinition("gateway", "built_in"), makeAgentDefinition("researcher", "topic"),
    ]);
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(SCHEDULE);
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: "topic-a", sessionId: null } });
    await flushUi();
    const agent = document.querySelector<HTMLSelectElement>('select[aria-label="Agent"]');
    if (agent) agent.value = "researcher";
    agent?.dispatchEvent(new Event("change", { bubbles: true }));
    setFieldValue(document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!, "Topic review");
    clickButton("Create schedule");
    await flushUi();
    expect(workflowApi.createSchedule).toHaveBeenLastCalledWith(expect.objectContaining({ session: { policy: "fresh", topic_id: "topic-a", agent_id: "researcher" } }));
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockResolvedValue([makeAgentDefinition("gateway", "built_in")]);
    const topic = document.querySelector<HTMLSelectElement>('select[aria-label="Run topic"]')!;
    topic.value = "";
    topic.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    expect(agent!.value).toBe("");
    setFieldValue(document.querySelector<HTMLTextAreaElement>('textarea[placeholder="Run the review checklist."]')!, "General review");
    clickButton("Create schedule");
    await flushUi();
    expect(workflowApi.createSchedule).toHaveBeenCalledWith(expect.objectContaining({ session: { policy: "fresh", agent_id: "gateway" } }));
  });

  it("shows a paused schedule's overlap policy and pending cap", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({
      schedules: [makeSchedule({ status: "paused", overlap_policy: "queue", pending_cap: 3 })],
    });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    const row = document.querySelector(".schedule-row");
    expect(row?.textContent).toContain("While a run is active: queue (cap 3)");
    expect(row?.textContent).toContain("Resume");
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

  it("shows the brief a run executed only when the schedule has since changed", async () => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({
      runs: [
        { ...RUN, id: "run-current", executed_message: SCHEDULE.message },
        { ...RUN, id: "run-earlier", executed_message: "Review the inbox.", output_text: "earlier output" },
      ],
    });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    const rows = [...document.querySelectorAll(".run-row")];
    const earlier = rows.find((row) => row.textContent?.includes("earlier output"));
    const current = rows.find((row) => row.textContent?.includes("did the thing"));
    expect(earlier?.textContent).toContain("Ran an earlier brief: Review the inbox.");
    expect(current?.textContent).not.toContain("Ran an earlier brief");
  });

  it("shows every destination of a run with its own outcome, and links the run to its conversation at its turn", async () => {
    const onNavigate = vi.fn();
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({
      runs: [
        {
          ...RUN,
          deliveries: [
            { target: { kind: "local_storage" }, outcome: "sent", error: null, delivered_at: "2026-04-07T12:05:01Z" },
            { target: { kind: "webhook", url: "https://example.test/hook" }, outcome: "failed", error: "connection refused", delivered_at: "2026-04-07T12:05:02Z" },
            { target: { kind: "channel", integration: "dev_dm", account: "acct", conversation: "person" }, outcome: "not_sent", error: "development channel target is a deliberate no-op", delivered_at: "2026-04-07T12:05:03Z" },
          ],
        },
        { ...RUN, id: "run-nowhere", session_id: null, turn_id: null, output_text: "nowhere output", deliveries: [{ target: null, outcome: "not_delivered", error: "gate skip", delivered_at: "2026-04-07T12:06:00Z" }] },
      ],
    });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1", onNavigate },
    });
    await flushUi();

    const rows = [...document.querySelectorAll(".run-row")];
    const delivered = rows.find((row) => row.textContent?.includes("did the thing"))!;
    const outcomes = [...delivered.querySelectorAll(".run-delivery")].map((node) => node.textContent?.replace(/\s+/g, " ").trim());
    expect(outcomes).toEqual([
      "local storage — sent",
      "webhook https://example.test/hook — failed: connection refused",
      "dev_dm person — not sent: development channel target is a deliberate no-op",
    ]);
    const nowhere = rows.find((row) => row.querySelector(".outcome-not_delivered"))!;
    expect(nowhere.querySelector(".run-delivery")?.textContent).toContain("no destination — not delivered: gate skip");
    expect(nowhere.querySelector("a.run-link")).toBeNull();

    const link = delivered.querySelector<HTMLAnchorElement>("a.run-link")!;
    expect(link.getAttribute("href")).toBe("/conversations/session-1/turns/turn-1");
    link.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    expect(onNavigate).toHaveBeenCalledWith("/conversations/session-1/turns/turn-1");
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

  it("shows a gate's declaration flags on its schedule, so unobserved criteria are visible before the first fire", async () => {
    const message =
      "This gate declares no files to observe, so its criteria are decided against the schedule's own record alone.";
    const flagged = makeSchedule({
      id: "flagged",
      message: "Work the inbox.",
      gate: { model: "cheap", criteria: "run only when inbox/todo.txt is non-empty" },
      gate_warnings: [{ kind: "unobserved_criteria", message }],
    });
    const seeing = makeSchedule({
      id: "seeing",
      message: "Watch the inbox.",
      gate: {
        model: "cheap",
        criteria: "run only when inbox/todo.txt is non-empty",
        observations: [{ kind: "file", path: "inbox/todo.txt" }],
      },
    });
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [flagged, seeing] });
    component = mount(ScheduleAutomationPanel, {
      target: document.body,
      props: { topicId: "topic-1", sessionId: "session-1" },
    });
    await flushUi();

    const rows = [...document.querySelectorAll(".schedule-row")];
    const flaggedRow = rows.find((row) => row.textContent?.includes("Work the inbox."));
    const seeingRow = rows.find((row) => row.textContent?.includes("Watch the inbox."));
    expect(flaggedRow?.querySelector(".gate-warning")?.textContent).toBe(message);
    expect(seeingRow?.querySelector(".gate-warning")).toBeNull();
    expect(seeingRow?.textContent).toContain("watching 1 file");
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
  it("schedule outcomes submits the selected queue cap through the form", async () => {
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(makeSchedule({ overlap_policy: "queue", pending_cap: 2 }));
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: "topic-1", sessionId: "session-1" } });
    await flushUi();
    const label = [...document.querySelectorAll("label")].find(node => node.textContent?.includes("While a previous run is active"));
    const select = label?.querySelector("select");
    expect(select, "overlap choice is available in the form").toBeDefined();
    if (!select) throw new Error("missing overlap selector");
    select.value = "queue"; select.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    const cap = document.querySelector<HTMLInputElement>('input[type="number"]');
    expect(cap).not.toBeNull();
    setFieldValue(cap!, "2");
    const message = document.querySelector<HTMLTextAreaElement>("textarea");
    setFieldValue(message!, "queued work");
    clickButton("Create schedule"); await flushUi();
    expect(workflowApi.createSchedule).toHaveBeenCalledWith(expect.objectContaining({ overlap_policy: "queue", pending_cap: 2 }));
  });

  it("schedule outcomes renders completed tool errors as an error badge", async () => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [{ ...RUN, task_outcome: "Completed with errors: shell (call-23): error" }] });
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: "topic-1", sessionId: "session-1" } });
    await flushUi();
    expect(document.querySelector(".status-run-failed")?.textContent).toBe("completed with errors");
    expect(document.body.textContent).toContain("shell (call-23): error");
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
    overlap_policy: "skip",
    id: "older", schedule_id: schedule.id, trigger: "tick", state: "completed",
    executed_message: schedule.message, created_at: "2026-01-01T00:00:00Z",
    deliveries: [],
  };
  const latest: ScheduleRun = {
    ...older, id: "latest", state: "failed", created_at: "2026-01-01T00:01:00Z",
    error_message: "405 Method Not Allowed",
  };
  beforeEach(() => {
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockReset();
    vi.mocked(agentApi.listEffectiveAgentDefinitions).mockResolvedValue([makeAgentDefinition("gateway", "built_in")]);
    vi.clearAllMocks();
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [older, latest] });
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [schedule] });
  });
  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
  });
  async function renderCard(): Promise<Element> {
    component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: null } });
    await flushUi();
    return document.querySelector(".schedule-row")!;
  }
  it.each(["failed", "running", "completed_empty"] as const)("shows the newest %s run on the unopened card", async (state) => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [older, { ...latest, state }] });
    const card = await renderCard();
    expect(card.querySelector(".schedule-facts .status")?.textContent?.trim()).toBe(state === "completed_empty" ? "completed empty" : state);
  });
  it("runs the passed one-shot again through the manual trigger", async () => {
    vi.mocked(operatorApi.triggerSchedule).mockResolvedValue({ run: { ...latest, state: "completed" } });
    const card = await renderCard();
    const buttons = [...card.querySelectorAll("button")];
    const runAgain = buttons.find(button => button.textContent?.trim() === "Run again");
    expect(runAgain).toBeDefined();
    expect(buttons.some(button => button.textContent?.trim() === "Resume")).toBe(false);
    runAgain!.click();
    await flushUi();
    expect(operatorApi.triggerSchedule).toHaveBeenCalledWith(schedule.id);
    expect(operatorApi.resumeSchedule).not.toHaveBeenCalled();
  });
  it("keeps the lifecycle badge when a passed one-shot has no runs", async () => {
    vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValue({ runs: [] });
    const card = await renderCard();
    expect(card.querySelector(".schedule-facts .status")?.textContent?.trim()).toBe("completed");
  });
  it("offers only Run again for an active passed one-shot", async () => {
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [{ ...schedule, status: "active" }] });
    const card = await renderCard();
    const actions = [...card.querySelectorAll(".row-actions button")]
      .map(button => button.textContent?.trim())
      .filter(label => label !== "Runs" && label !== "Hide runs");
    expect(actions).toEqual(["Run again"]);
  });
  it("re-reads a running card's history until the run settles", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(operatorApi.fetchScheduleRuns).mockResolvedValueOnce({ runs: [older, { ...latest, state: "running" }] });
      component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: null } });
      await vi.advanceTimersByTimeAsync(0);
      const status = () => document.querySelector(".schedule-row .schedule-facts .status")?.textContent?.trim();
      expect(status()).toBe("running");
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(status()).toBe("failed");
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
      component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: null } });
      await vi.advanceTimersByTimeAsync(0);
      const status = () => document.querySelector(".schedule-row .schedule-facts .status")?.textContent?.trim();
      expect(status()).toBe("failed");
      [...document.querySelector(".schedule-row")!.querySelectorAll("button")].find(button => button.textContent?.trim() === "Run again")!.click();
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
      component = mount(ScheduleAutomationPanel, { target: document.body, props: { topicId: null, sessionId: null } });
      await vi.advanceTimersByTimeAsync(0);
      const status = () => document.querySelector(".schedule-row .schedule-facts .status")?.textContent?.trim();
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
