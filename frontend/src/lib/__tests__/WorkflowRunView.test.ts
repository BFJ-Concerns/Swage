// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { mount, unmount } from "svelte";
import WorkflowRunView from "../components/WorkflowRunView.svelte";
import type { WorkflowRun } from "../protocol/WorkflowRun";
import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";
import type { WorkflowRunEnding } from "../protocol/WorkflowRunEnding";
import type { WorkflowRunTerminalAccount } from "../protocol/WorkflowRunTerminalAccount";

type MountedComponent = ReturnType<typeof mount>;

function run(
  terminalAccount: WorkflowRunTerminalAccount | null,
  id = "run-1",
  scriptName = "nightly sweep",
): WorkflowRun {
  return {
    id,
    topic_id: "topic-1",
    script_id: null,
    script_name: scriptName,
    script_version: null,
    carrying_skill: null,
    state: terminalAccount ? "cancelled" : "running",
    final_output: null,
    handover: null,
    terminal_account: terminalAccount,
    interruptions: [],
    error_message: null,
    created_at: "2026-09-02T10:00:00Z",
    completed_at: null,
  } as unknown as WorkflowRun;
}

function detail(workflowRun: WorkflowRun): WorkflowRunDetailResponse {
  return {
    run: workflowRun,
    journal: [],
    wake_metrics: {
      gate_evaluations: 0,
      gate_wakes: 0,
      gate_suppressions: 0,
      events_posted: 0,
      events_pending: 0,
    },
    holds: [],
    hold_metrics: { holds_open: 0, holds_answered: 0, answers_rejected: 0 },
  };
}

function render(workflowRun: WorkflowRun): MountedComponent {
  return mount(WorkflowRunView, {
    target: document.body,
    props: { detail: detail(workflowRun), loadState: "loaded", errorMessage: "" },
  });
}

function emptyAccount(ending: WorkflowRunEnding): WorkflowRunTerminalAccount {
  return {
    ending,
    workers: { done: [], failed: [], cut_short: [], unsettled: [] },
    partial_results: undefined,
    settled_at: "2026-09-02T10:05:00Z",
  };
}

describe("WorkflowRunView", () => {
  let component: MountedComponent | undefined;

  it("run accounts preserve the author defect and hide empty partial results", () => {
    const account = emptyAccount({ ending: "author_error", error: "UnknownAgentOptionError: unknown option bogusOption" });
    account.partial_results = { reason: "no result", results: [] };
    component = render(run(account));
    expect(document.querySelector(".ending-line")?.textContent).toBe("Author error — UnknownAgentOptionError: unknown option bogusOption");
    expect(document.body.textContent).not.toContain("Partial results");
  });

  it("run accounts render cancellation provenance", () => {
    const account = emptyAccount({ ending: "cancelled" });
    account.cancellation = { source: "API request", actor: "operator", session_id: "session-1", requested_at: "2026-10-15T10:00:00Z" };
    account.workers.cut_short.push({ journal_entry_id: "cut-short", seq: 1, label: "worker", agent_definition_id: null, placement: null, worktree: null,
      redispatched_after_interruption: false,
      termination: { termination: "cut_short", cause: "run_cancelled", detail: "WorkflowCancelledError: run cancelled" } });
    component = render(run(account));
    expect(getComputedStyle(document.querySelector(".detail")!).textTransform).toBe("none");
    const text = document.querySelector(".ending")?.textContent;
    expect(text).toContain("API request");
    expect(text).toContain("2026-10-15T10:00:00Z");
    expect(text).toContain("operator");
    expect(text).toContain("session-1");
  });

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
  });

  it("reads the ending, every worker's standing, and the recoverable work", async () => {
    const account: WorkflowRunTerminalAccount = {
      ending: { ending: "cancelled" },
      workers: {
        done: [
          {
            journal_entry_id: "entry-1",
            seq: 0,
            label: "finishing",
            agent_definition_id: null,
            placement: "/worktrees/run-1/finishing",
            worktree: null,
            termination: null,
            redispatched_after_interruption: false,
          },
        ],
        failed: [
          {
            journal_entry_id: "entry-3",
            seq: 1,
            label: "broken",
            agent_definition_id: null,
            placement: "/worktrees/run-1/broken",
            worktree: null,
            termination: {
              termination: "failed",
              message: "the worker exhausted its attempts",
            },
            redispatched_after_interruption: false,
          },
        ],
        cut_short: [
          {
            journal_entry_id: "entry-2",
            seq: 2,
            label: "hanging",
            agent_definition_id: null,
            placement: "/worktrees/run-1/hanging",
            worktree: null,
            termination: {
              termination: "cut_short",
              cause: "run_cancelled",
              detail: "the run was cancelled",
            },
            redispatched_after_interruption: false,
          },
        ],
        unsettled: [
          {
            journal_entry_id: "entry-4",
            seq: 3,
            label: null,
            agent_definition_id: null,
            placement: null,
            worktree: null,
            termination: { termination: "unsettled" },
            redispatched_after_interruption: false,
          },
        ],
      },
      partial_results: {
        reason: "the run was cancelled before returning",
        results: [
          {
            journal_entry_id: "entry-1",
            seq: 0,
            label: "finishing",
            output: "the survey text",
            placement: "/worktrees/run-1/finishing",
          },
        ],
      },
      settled_at: "2026-09-02T10:05:00Z",
    };
    component = render(run(account));
    const text = document.body.textContent ?? "";
    // The partition is legible as a partition: the cancelled run's in-flight
    // worker reads as cut short rather than failed, and says so where it ran.
    expect(text).toContain("Done (1)");
    expect(text).toContain("Failed (1)");
    expect(text).toContain("Cut short (1)");
    expect(text).toContain("Unsettled (1)");
    expect(text).toContain("hanging");
    expect(text).toContain("the run was cancelled");
    expect(text).toContain("/worktrees/run-1/hanging");
    // Each standing renders its own workers with its own cause, so a bucket
    // dropped from the view takes an observable line with it.
    expect(text).toContain("broken");
    expect(text).toContain("the worker exhausted its attempts");
    // A worker with no label is still named by the seq the run gave it.
    expect(text).toContain("worker 3");
    expect(text).toContain("never settled");
    // The completed worker's output survives the run that never returned.
    expect(text).toContain("the run was cancelled before returning");
    expect(text).toContain("the survey text");
  });

  it("shows each interruption the run lived through and marks workers redispatched after one", async () => {
    const account: WorkflowRunTerminalAccount = {
      ...emptyAccount({ ending: "returned" }),
      workers: {
        done: [
          {
            journal_entry_id: "entry-1",
            seq: 0,
            label: "appender",
            agent_definition_id: null,
            placement: null,
            worktree: null,
            termination: null,
            redispatched_after_interruption: true,
          },
          {
            journal_entry_id: "entry-2",
            seq: 1,
            label: "clean",
            agent_definition_id: null,
            placement: null,
            worktree: null,
            termination: null,
            redispatched_after_interruption: false,
          },
        ],
        failed: [],
        cut_short: [],
        unsettled: [],
      },
    };
    const interrupted = {
      ...run(account),
      interruptions: [
        { detected_at: "2026-09-02T10:02:00Z", run_state: "parked" },
        { detected_at: "2026-09-02T10:04:00Z", run_state: "running" },
      ],
    } as unknown as WorkflowRun;
    component = render(interrupted);
    const text = document.body.textContent ?? "";
    expect(text).toContain("Interruptions");
    expect(text).toContain("the gateway stopped while this run was parked");
    expect(text).toContain("the gateway stopped while this run was executing; the calls it had in flight were cut off");
    expect(text).not.toContain("dispatched again");
    expect(text).toContain("Workers redispatched after an interruption may have repeated their effects.");
    const done = Array.from(document.querySelectorAll(".standing li"));
    const appender = done.find((item) => item.textContent?.includes("appender"))!;
    expect(appender.textContent).toContain("redispatched after an interruption");
    expect(appender.textContent).toContain("unknown outcomes and may have been applied twice");
    const clean = done.find((item) => item.textContent?.includes("clean"))!;
    expect(clean.textContent).not.toContain("redispatched");
  });

  it("says a run that has not ended has no account yet", async () => {
    component = render(run(null));

    expect(document.body.textContent).not.toContain("Cut short");
  });

  it.each([
    [{ ending: "returned" } satisfies WorkflowRunEnding, "Returned"],
    [
      { ending: "threw", error: "the script rejected its input" } satisfies WorkflowRunEnding,
      "Threw — the script rejected its input",
    ],
    [
      { ending: "wound_down", error: "the closing reserve was exhausted" } satisfies WorkflowRunEnding,
      "Wound down — the closing reserve was exhausted",
    ],
    [
      {
        ending: "interrupted_without_resume",
        reason: "the workflow source was unavailable",
      } satisfies WorkflowRunEnding,
      "Interrupted without resume — the workflow source was unavailable",
    ],
  ])("describes the %s ending", async (ending, expected) => {
    component = render(run(emptyAccount(ending)));
    expect(document.body.textContent).toContain(expected);
  });

  it("surfaces a failed shared-record read rather than an empty run", () => {
    component = mount(WorkflowRunView, {
      target: document.body,
      props: { detail: null, loadState: "error", errorMessage: "read failed" },
    });

    expect(document.querySelector('[role="alert"]')?.textContent).toContain("read failed");
  });
});

describe("run page completeness", () => {
  let page: MountedComponent | undefined;
  afterEach(() => { if (page) unmount(page); document.body.replaceChildren(); });

  it("shows identity, timing and the launching conversation", () => {
    page = render({ ...run(emptyAccount({ ending: "returned" }), "run-own", "three-whys"),
      dispatching_session_id: "parent-session", completed_at: "2026-09-02T10:05:00Z" });
    const header = document.querySelector(".run header")!;
    expect(header.textContent).toContain("three-whys");
    expect(header.textContent).toContain("run-own");
    expect(header.textContent).toContain("2026-09-02T10:00:00Z");
    expect(header.textContent).toContain("2026-09-02T10:05:00Z");
    expect(header.querySelector("a")?.getAttribute("href")).toBe("/conversations/parent-session");
  });

  it("groups workers sharing a placement once and preserves its case", () => {
    const account = emptyAccount({ ending: "returned" });
    account.workers.done = [0, 1].map((seq) => ({ journal_entry_id: `worker-${seq}`, seq,
      label: `worker ${seq}`, placement: "/Worktrees/MyRun", worktree: null, termination: null,
      redispatched_after_interruption: false }));
    const shared = detail(run(account));
    shared.journal = account.workers.done.map((worker, index) => ({ id: worker.journal_entry_id, result: { kind: "agent_call", output: `Worker output ${index}` } } as unknown as WorkflowRunDetailResponse["journal"][number]));
    page = mount(WorkflowRunView, { target: document.body, props: { detail: shared, loadState: "loaded", errorMessage: "" } });
    expect(document.body.textContent?.split("/Worktrees/MyRun")).toHaveLength(2);
    const placement = document.querySelector(".placement")!;
    expect(getComputedStyle(placement).textTransform).toBe("none");
    expect(placement.parentElement?.textContent).toContain("worker 0");
    expect(placement.parentElement?.textContent).toContain("worker 1");
    expect(placement.parentElement?.textContent).toContain("Worker output 0");
    expect(placement.parentElement?.textContent).toContain("Worker output 1");
  });

  it.each(["timer", "hold", "event"])("names a parked %s wait read-only", (kind) => {
    const parked = detail({ ...run(null), state: "parked", wake_at: kind === "timer" ? "2026-09-03T10:00:00Z" : null });
    if (kind === "hold") parked.holds = [{ id: "decision", run_id: "run-1", seq: 0, content: "Ship the findings?", verdicts: ["approve", "postpone"], state: "open", raised_at: "2026-09-02T10:00:00Z", rejections: [] }];
    if (kind === "event") parked.journal = [{ id: "wait", kind: "await_event", status: "started", request: { kinds: ["review.finished"] } } as unknown as WorkflowRunDetailResponse["journal"][number]];
    page = mount(WorkflowRunView, { target: document.body, props: { detail: parked, loadState: "loaded", errorMessage: "" } });
    const awaited = document.querySelector(".awaited")!;
    expect(awaited).not.toBeNull();
    if (kind === "timer") expect(awaited.textContent).toContain("2026-09-03T10:00:00Z");
    if (kind === "hold") {
      expect(awaited.textContent).toContain("Ship the findings?");
      expect(awaited.textContent).toContain("approve, postpone");
      expect(awaited.textContent).toContain("An agent answers");
    }
    if (kind === "event") expect(awaited.textContent).toContain("review.finished");
    expect(awaited.querySelector("button, input, select")).toBeNull();
  });

  it("shows recorded and failed sources from the returned result independently of workers", () => {
    const completed = run(emptyAccount({ ending: "returned" }));
    completed.final_output = { gathered: [{ url: "https://source.invalid/good", observation_id: "observation-1" }], failed_sources: [{ url: "https://source.invalid/bad", reason: "The source refused access" }], organised: { orientation_cards: 1, note: "Two subjects indexed" } };
    page = render(completed);
    const result = document.querySelector('[aria-label="Run result"]')!;
    expect(result).not.toBeNull();
    const items = result.querySelectorAll("li");
    expect(items[0].textContent).toContain("https://source.invalid/good");
    expect(items[0].textContent).toContain("observation-1");
    expect(items[1].textContent).toContain("https://source.invalid/bad");
    expect(items[1].textContent).toContain("The source refused access");
    expect(result.textContent).toContain("organised");
    expect(result.textContent).toContain("Two subjects indexed");
  });
});
