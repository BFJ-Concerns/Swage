// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { mount, unmount } from "svelte";
import WorkflowRunDetail from "../components/WorkflowRunDetail.svelte";
import workflowRunDetailSource from "../components/WorkflowRunDetail.svelte?raw";
import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";
import type { WorkflowAgentCallDetail } from "../protocol/WorkflowAgentCallDetail";

let component: ReturnType<typeof mount> | undefined;

afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.replaceChildren();
});

function runWith(call: WorkflowAgentCallDetail): WorkflowRunDetailResponse {
  return {
    run: {
      id: "run-1",
      topic_id: "topic-1",
      parent_run_id: null,
      parent_seq: null,
      script_id: null,
      script_name: null,
      script_version: null,
      carrying_skill: null,
      source: null,
      args: null,
      budget: {
        wall_clock_ms: null,
        agent_calls: null,
        input_tokens: null,
        output_tokens: null,
      },
      state: "running",
      final_output: null,
      handover: null,
      error_message: null,
      engine_version: "test",
      wake_at: null,
      created_at: "2026-09-02T10:00:00Z",
      completed_at: null,
    },
    journal: [
      {
        id: "entry-1",
        seq: 0n,
        completed_order: null,
        kind: "agent_call",
        status: "started",
        label: null,
        request: null,
        result: null,
        error_message: null,
        agent_call: call,
        effect: null,
        started_at: "2026-09-02T10:00:01Z",
        completed_at: null,
      },
    ],
    wake_metrics: {
      gate_evaluations: 0,
      gate_wakes: 0,
      gate_suppressions: 0,
      events_posted: 0,
      events_pending: 0,
    },
  } as unknown as WorkflowRunDetailResponse;
}

const baseCall: WorkflowAgentCallDetail = {
  agent_definition_id: "default",
  session_id: null,
  thread_id: null,
  turn_id: null,
  usage: { availability: "known", input_tokens: 11n, output_tokens: 7n },
  termination: { termination: "settled" },
  dispatch: {
    prompt: "read the briefing",
    agent_definition_id: "default",
    requested_model: null,
    requested_effort: "low",
    placement: null,
    grants: ["file_read"],
    attempts: [
      {
        model: "test-model",
        effort: "low",
        session_id: null,
        thread_id: null,
        turn_id: null,
      },
    ],
  },
  provider_rounds: [
    {
      attempt: 1,
      round: 1,
      started_at: "2026-09-02T10:00:01Z",
      completed_at: "2026-09-02T10:00:02Z",
      outcome: { outcome: "responded", tool_calls: 0 },
    },
  ],
} as unknown as WorkflowAgentCallDetail;

async function render(detail: WorkflowRunDetailResponse): Promise<void> {
  component = mount(WorkflowRunDetail, {
    target: document.body,
    props: { detail, loadState: "loaded", errorMessage: "" },
  });
  expect(document.querySelector(".call")).not.toBeNull();
}

describe("WorkflowRunDetail", () => {
  it("shows what the worker was dispatched with and what it resolved", async () => {
    await render(runWith(baseCall));

    const call = document.querySelector(".call")!.textContent!;
    expect(call).toContain("read the briefing");
    expect(call).toContain("file_read");
    expect(call).toContain("test-model");
    expect(document.querySelector(".usage")!.textContent).toContain(
      "11 in / 7 out",
    );
  });

  it("preserves structured prompt whitespace and wraps long tokens", async () => {
    const promptText = "Read both sections:\n\n  First   item\n  Second item";
    await render(
      runWith({
        ...baseCall,
        dispatch: { ...baseCall.dispatch!, prompt: promptText },
      } as unknown as WorkflowAgentCallDetail),
    );

    const prompt = document.querySelector<HTMLElement>(".prompt")!;
    expect(prompt.textContent).toBe(promptText);
    expect(workflowRunDetailSource).toMatch(
      /\.prompt\s*\{[^}]*white-space:\s*pre-wrap;[^}]*overflow-wrap:\s*anywhere;/s,
    );
  });

  it("renders absent usage as not recorded with its reason, never as zero", async () => {
    await render(
      runWith({
        ...baseCall,
        usage: {
          availability: "unavailable",
          reason: "provider_reported_no_usage",
        },
      } as unknown as WorkflowAgentCallDetail),
    );

    const usage = document.querySelector(".usage")!.textContent!;
    expect(usage).toContain("Not reported");
    expect(usage).toContain("the provider reported no usage");
    expect(usage).not.toContain("0");
  });

  it("distinguishes a cancelled call's unsettled usage from provider non-reporting", async () => {
    await render(
      runWith({
        ...baseCall,
        termination: { termination: "unsettled" },
        usage: {
          availability: "unavailable",
          reason: "call_did_not_settle",
        },
      } as unknown as WorkflowAgentCallDetail),
    );

    const usage = document.querySelector(".usage")!.textContent!;
    expect(usage).toContain("Not reported");
    expect(usage).toContain("the call never settled");
    expect(usage).not.toContain("the provider reported no usage");
  });

  it.each([
    [{ termination: "outstanding" }, "the worker is still in progress"],
    [
      { termination: "failed", message: "AgentCallTimeoutError" },
      "the worker failed before usage was reported",
    ],
    [
      { termination: "cut_short", cause: "run_cancelled" },
      "the worker was cut short before usage was reported",
    ],
    [
      { termination: "settled" },
      "the worker finished without reporting usage",
    ],
  ])("describes unavailable spend for %j", async (termination, wording) => {
    await render(
      runWith({
        ...baseCall,
        usage: { availability: "unavailable", reason: "call_did_not_settle" },
        termination,
      } as unknown as WorkflowAgentCallDetail),
    );
    const usage = document.querySelector(".usage")!.textContent!;
    expect(usage).toContain(wording);
    expect(usage).not.toContain("never settled");
    expect(usage).not.toContain("0 in");
  });

  it("names a cut-short worker as the run's end, not the worker's failure", async () => {
    await render(
      runWith({
        ...baseCall,
        termination: { termination: "cut_short", cause: "run_cancelled" },
      } as unknown as WorkflowAgentCallDetail),
    );

    const termination = document.querySelector(".termination")!.textContent!;
    expect(termination).toContain("Cut short");
    expect(termination).toContain("the run was cancelled");
    expect(termination).not.toContain("Failed");
  });

  it("tells an outstanding provider round from a settled one", async () => {
    await render(
      runWith({
        ...baseCall,
        provider_rounds: [
          {
            attempt: 1,
            round: 1,
            started_at: "2026-09-02T10:00:01Z",
            completed_at: "2026-09-02T10:00:02Z",
            outcome: { outcome: "responded", tool_calls: 2 },
          },
          {
            attempt: 1,
            round: 2,
            started_at: "2026-09-02T10:00:03Z",
            completed_at: null,
            outcome: { outcome: "outstanding" },
          },
        ],
      } as unknown as WorkflowAgentCallDetail),
    );

    const rows = document.querySelectorAll("table.rounds tbody tr");
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain("responded with 2 tool call(s)");
    expect(rows[1].textContent).toContain("no response yet");
  });

  it("renders a failed provider round as failed rather than outstanding", async () => {
    await render(
      runWith({
        ...baseCall,
        termination: {
          termination: "failed",
          message: "scripted provider failure",
        },
        provider_rounds: [
          {
            attempt: 1,
            round: 1,
            started_at: "2026-09-02T10:00:01Z",
            completed_at: "2026-09-02T10:00:02Z",
            outcome: {
              outcome: "failed",
              message: "scripted provider failure",
            },
          },
        ],
      } as unknown as WorkflowAgentCallDetail),
    );

    const row = document.querySelector("table.rounds tbody tr")!.textContent!;
    expect(row).toContain("failed — scripted provider failure");
    expect(row).not.toContain("no response yet");
  });

  it("keeps an interrupted attempt's rounds beside the attempt that followed", async () => {
    await render(
      runWith({
        ...baseCall,
        dispatch: {
          ...baseCall.dispatch!,
          attempts: [
            {
              model: "interrupted-model",
              effort: "low",
              session_id: null,
              thread_id: null,
              turn_id: null,
            },
            {
              model: "test-model",
              effort: "low",
              session_id: null,
              thread_id: null,
              turn_id: null,
            },
          ],
        },
        provider_rounds: [
          {
            attempt: 1,
            round: 1,
            started_at: "2026-09-02T10:00:01Z",
            completed_at: "2026-09-02T10:00:02Z",
            outcome: { outcome: "responded", tool_calls: 1 },
          },
          {
            attempt: 1,
            round: 2,
            started_at: "2026-09-02T10:00:03Z",
            completed_at: "2026-09-02T10:04:07Z",
            outcome: { outcome: "interrupted" },
          },
          {
            attempt: 2,
            round: 1,
            started_at: "2026-09-02T10:05:01Z",
            completed_at: "2026-09-02T10:05:02Z",
            outcome: { outcome: "responded", tool_calls: 0 },
          },
        ],
      } as unknown as WorkflowAgentCallDetail),
    );

    const rows = document.querySelectorAll("table.rounds tbody tr");
    expect(rows.length).toBe(3);
    expect(rows[1].textContent).toContain("interrupted");
    expect(rows[1].textContent).not.toContain("no response yet");
    // The interrupted round ended when nothing could answer it any more, so
    // the Ended cell carries that time rather than a dash.
    const interruptedEnd = rows[1].querySelectorAll("td")[3].textContent;
    expect(interruptedEnd).toContain("2026-09-02T10:04:07Z");
    expect(interruptedEnd).not.toContain("—");
    expect(document.querySelector(".attempts")!.textContent).toContain("2");
    // The call ran to its end on the last attempt, and that is what "ran
    // with" names.
    expect(document.querySelector(".call")!.textContent).toContain(
      "test-model",
    );
  });
});
