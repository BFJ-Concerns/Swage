// HTTP API client for workflow run records and cancellation.
//
// A run's journal is the account of what its workers were asked to do and
// what came back — the prompt and options each call was dispatched under,
// the provider rounds its turn ran, what it cost or why that is not known,
// and how it stopped.

import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";

let baseUrl = "";

export function setBaseUrl(url: string): void {
  baseUrl = url;
}

/**
 * Fetch a workflow run with its journal.
 *
 * Returns null when no run has that id, so a stale link renders a "not
 * found" state rather than an error.
 */
export async function fetchWorkflowRun(
  runId: string,
): Promise<WorkflowRunDetailResponse | null> {
  const response = await fetch(
    `${baseUrl}/api/workflow-runs/${encodeURIComponent(runId)}`,
  );
  if (response.status === 404) {
    return null;
  }
  if (!response.ok) {
    throw new Error(
      `failed to load the workflow run: HTTP ${response.status}`,
    );
  }
  return (await response.json()) as WorkflowRunDetailResponse;
}

/** Cancel through the run's existing control route. */
export async function cancelWorkflowRun(runId: string): Promise<void> {
  const response = await fetch(`${baseUrl}/api/workflow-runs/${encodeURIComponent(runId)}/cancel`, { method: "POST" });
  if (!response.ok) throw new Error(`Failed to cancel workflow run: HTTP ${response.status}`);
}
