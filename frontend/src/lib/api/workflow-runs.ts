import { ApiError } from "./error";
import * as generated from "../protocol/client";

/** Let stale run links render the absent state. */
export async function fetchWorkflowRun(runId: string) {
  try {
    return await generated.getWorkflowRun(runId);
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

export async function cancelWorkflowRun(runId: string): Promise<void> {
  await generated.cancelWorkflowRun(runId);
}
