// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, tick, unmount } from "svelte";
import WorkflowRunView from "../components/WorkflowRunView.svelte";
import WorkflowRunViewHarness from "./fixtures/WorkflowRunViewHarness.svelte";
import type { WorkflowRun } from "../protocol/WorkflowRun";
import type { WorkflowRunTerminalAccount } from "../protocol/WorkflowRunTerminalAccount";

type MountedComponent = ReturnType<typeof mount>;

function run(
  terminalAccount: WorkflowRunTerminalAccount | null,
  id = "run-1",
  scriptName = "nightly sweep",
): WorkflowRun {
  return {
    id,
    space_id: "space-1",
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

function respondWith(body: unknown): ReturnType<typeof vi.spyOn> {
  return vi
    .spyOn(globalThis, "fetch")
    .mockResolvedValue(new Response(JSON.stringify(body), { status: 200 }));
}

describe("WorkflowRunView", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
    vi.restoreAllMocks();
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
            termination: { cause: "failed", error: "the worker exhausted its attempts" },
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
            termination: { cause: "cut_short", detail: "the run was cancelled" },
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
            termination: { cause: "unsettled" },
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
    const fetchSpy = respondWith({ run: run(account), journal: [], wake_metrics: {} });

    component = mount(WorkflowRunView, { target: document.body, props: { runId: "run 1" } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("Cancelled"));

    expect(fetchSpy).toHaveBeenCalledWith("/api/workflow-runs/run%201", { method: "GET" });
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
    respondWith({ run: run(null), journal: [], wake_metrics: {} });

    component = mount(WorkflowRunView, { target: document.body, props: { runId: "run-1" } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("has not ended yet"));

    expect(document.body.textContent).not.toContain("Cut short");
  });

  it("surfaces a failed read rather than an empty run", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response("", { status: 404 }));

    component = mount(WorkflowRunView, { target: document.body, props: { runId: "missing" } });
    await tick();
    await vi.waitFor(() =>
      expect(document.querySelector('[role="alert"]')?.textContent).toContain("404"),
    );
  });

  it("reloads for a new run ID and ignores the previous run's late response", async () => {
    let resolveFirst: ((response: Response) => void) | undefined;
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation((input) => {
      const url = String(input);
      if (url.endsWith("/run-a")) {
        return new Promise<Response>((resolve) => {
          resolveFirst = resolve;
        });
      }
      if (url.endsWith("/run-b")) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              run: run(null, "run-b", "second run"),
              journal: [],
              wake_metrics: {},
            }),
            { status: 200 },
          ),
        );
      }
      throw new Error(`unexpected request: ${url}`);
    });

    const harness = mount(WorkflowRunViewHarness, { target: document.body });
    component = harness;
    await vi.waitFor(() => expect(fetchSpy).toHaveBeenCalledWith("/api/workflow-runs/run-a", { method: "GET" }));

    harness.show("run-b");
    await vi.waitFor(() => expect(document.body.textContent).toContain("second run"));

    resolveFirst?.(
      new Response(
        JSON.stringify({
          run: run(null, "run-a", "first run"),
          journal: [],
          wake_metrics: {},
        }),
        { status: 200 },
      ),
    );
    await tick();
    expect(document.body.textContent).toContain("second run");
    expect(document.body.textContent).not.toContain("first run");
  });
});
