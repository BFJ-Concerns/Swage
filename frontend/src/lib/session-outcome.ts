import type { TurnState } from "./protocol/TurnState";

/** The latest recorded response needs attention only until a newer turn supersedes it. */
export function sessionOutcomeLabel(state: TurnState | null | undefined): string | null {
  if (state === "failed") return "Last response failed";
  if (state === "interrupted") return "Interrupted";
  return null;
}
