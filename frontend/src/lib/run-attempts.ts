import type { WorkflowAttemptCause } from "./protocol/WorkflowAttemptCause";

export function describeRedispatchCause(cause: WorkflowAttemptCause): string | null {
  switch (cause.cause) {
    case "after_interruption":
      return "after a gateway interruption";
    case "after_provider_failure":
      return `after a recorded provider failure (${cause.failure})`;
    default:
      return null;
  }
}
