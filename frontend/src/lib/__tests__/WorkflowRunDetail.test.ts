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
        cause: { cause: "first" },
        opened_at: "2026-09-02T10:00:00Z",
        model: "test-model",
        effort: "low",
        session_id: null,
        thread_id: null,
        turn_id: "turn-1",
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
  it("renders liveness and provider times locally while retaining exact stamps", async () => {
    const started = "2026-10-01T19:28:49.393288230Z";
    const ended = "2026-10-01T19:28:50.893288230Z";
    await render(runWith({
      ...baseCall,
      liveness: { started_at: started, first_evidence_at: ended },
      provider_rounds: [{ ...baseCall.provider_rounds![0], started_at: started, completed_at: ended }],
    } as WorkflowAgentCallDetail));
    for (const selector of [".liveness", ".rounds"]) {
      const surface = document.querySelector(selector)!;
      expect(surface.querySelectorAll("time")).toHaveLength(2);
      for (const time of surface.querySelectorAll("time")) {
        const stamp = time.getAttribute("datetime")!;
        expect(time.title).toBe(stamp);
        expect(time.textContent).toBe(new Date(stamp).toLocaleString());
        expect(time.textContent).not.toBe(stamp);
      }
    }
    expect(document.querySelector(".rounds tbody")?.textContent).toContain(
      `${(1.5).toLocaleString(undefined, { maximumFractionDigits: 1 })} s`,
    );
  });

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

  it("names an opened attempt no turn has resolved as such, never as defaults", async () => {
    const first = baseCall.dispatch!.attempts[0];
    await render(
      runWith({
        ...baseCall,
        termination: { termination: "outstanding" },
        dispatch: {
          ...baseCall.dispatch!,
          attempts: [{ ...first, model: null, effort: null, turn_id: null }],
        },
      } as unknown as WorkflowAgentCallDetail),
    );
    const call = document.querySelector(".call")!.textContent!;
    expect(call).toContain("opened, not yet resolved by a turn");
    expect(call).not.toContain("default model");
  });

  it("words a redispatch after an interruption apart from a fallback", async () => {
    const first = baseCall.dispatch!.attempts[0];
    await render(
      runWith({
        ...baseCall,
        dispatch: {
          ...baseCall.dispatch!,
          attempts: [
            first,
            { ...first, cause: { cause: "after_interruption" }, model: "test-model" },
          ],
        },
      } as unknown as WorkflowAgentCallDetail),
    );
    let attempts = document.querySelector(".attempts")!.textContent!;
    expect(attempts).toContain("attempt 2 — dispatched again after a gateway interruption");
    expect(attempts).toContain("attempt 1's outcome is unknown");
    expect(attempts).toContain("may have been applied twice");
    expect(attempts).not.toContain("fallback");

    unmount(component!);
    component = undefined;
    document.body.replaceChildren();
    await render(
      runWith({
        ...baseCall,
        dispatch: {
          ...baseCall.dispatch!,
          attempts: [
            first,
            {
              ...first,
              cause: { cause: "provider_fallback", primary_failure: "unknown model pinned" },
              model: "fallback",
            },
          ],
        },
      } as unknown as WorkflowAgentCallDetail),
    );
    attempts = document.querySelector(".attempts")!.textContent!;
    expect(attempts).toContain("dispatched to the declared fallback because the primary failed (unknown model pinned)");
    expect(attempts).not.toContain("served");
    expect(attempts).not.toContain("interrupt");
    expect(document.querySelector(".call")!.textContent).toContain("fallback, low");
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
              cause: { cause: "first" },
              opened_at: "2026-09-02T10:00:00Z",
              model: "interrupted-model",
              effort: "low",
              session_id: null,
              thread_id: null,
              turn_id: "turn-1",
            },
            {
              cause: { cause: "after_interruption" },
              opened_at: "2026-09-02T10:00:03Z",
              model: "test-model",
              effort: "low",
              session_id: null,
              thread_id: null,
              turn_id: "turn-2",
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
    expect(interruptedEnd).toContain(new Date("2026-09-02T10:04:07Z").toLocaleString());
    expect(interruptedEnd).not.toContain("—");
    expect(document.querySelector(".attempts")!.textContent).toContain("2");
    // The call ran to its end on the last attempt, and that is what "ran
    // with" names.
    expect(document.querySelector(".call")!.textContent).toContain(
      "test-model",
    );
  });
});

describe("complete worker detail", () => {
  it("shows every attempt's options and session, liveness and null-grant meaning", async () => {
    await render(runWith({ ...baseCall, session_id: "worker-session", liveness: { started_at: "2026-09-02T11:00:00Z", first_evidence_at: "2026-09-02T11:00:04Z" }, dispatch: { ...baseCall.dispatch!, grants: null, placement: { path: "/Worktree/ExactCase", can_write: false }, attempts: [
      { cause: { cause: "first" }, opened_at: "2026-09-02T10:00:00Z", model: "earlier-model", effort: "high", session_id: "earlier-session", turn_id: "earlier-turn" },
      { cause: { cause: "after_interruption" }, opened_at: "2026-09-02T10:01:00Z", model: "later-model", effort: "low", session_id: "later-session", turn_id: "later-turn" },
    ] } }));
    const attempts = document.querySelectorAll(".attempts li");
    expect(attempts[0].textContent).toContain("earlier-model, high");
    expect(attempts[1].textContent).toContain("later-model, low");
    expect(attempts[0].querySelector("a")?.getAttribute("href")).toBe("/conversations/earlier-session");
    expect(attempts[1].querySelector("a")?.getAttribute("href")).toBe("/conversations/later-session");
    expect(document.querySelector(".liveness")?.textContent).toContain(new Date("2026-09-02T11:00:00Z").toLocaleString());
    expect(document.querySelector(".liveness")?.textContent).toContain(new Date("2026-09-02T11:00:04Z").toLocaleString());
    expect(document.body.textContent).toContain("narrowed nothing");
    expect(document.body.textContent).toContain("/Worktree/ExactCase");
    expect(document.querySelector('a[href="/conversations/worker-session"]')).not.toBeNull();
  });

  it("reads an outstanding journal worker through its terminal account", async () => {
    const detail = runWith({ ...baseCall, termination: { termination: "outstanding" } });
    detail.run.state = "completed";
    detail.run.terminal_account = { ending: { ending: "returned" }, settled_at: "2026-09-02T10:05:00Z", workers: { done: [], failed: [], unsettled: [], cut_short: [{ journal_entry_id: "entry-1", seq: 0, redispatched_after_interruption: false, termination: { termination: "cut_short", cause: "run_returned", detail: "The script returned" } }] } };
    await render(detail);
    expect(document.querySelector(".termination")?.textContent).toBe("Cut short");
    expect(document.body.textContent).not.toContain("Still working");
  });
});
