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
