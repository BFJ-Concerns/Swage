// HTTP API client for turn-level records: the assembled-context report.
//
// A turn's context report is the stored account of how its context was
// built — which elements contributed, at what token cost, with what
// compaction and warnings. The gateway records it at assembly time; this
// client only ever reads it back.

export function setBaseUrl(url: string): void {
  generated.setClientBaseUrl(url);
}

import type { AssemblyReport } from "../protocol/AssemblyReport";
import * as generated from "../protocol/client";
import { ApiError } from "./error";

export type TurnContextReport = AssemblyReport;

/**
 * Fetch the assembled-context report recorded for a turn.
 *
 * Returns null when no report was recorded — older turns and turns that
 * failed before assembly have none, and the inspector renders that state
 * rather than treating it as an error.
 */
export async function fetchTurnContext(
  turnId: string,
): Promise<TurnContextReport | null> {
  try {
    return (await generated.getTurnContext(turnId)).report;
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      return null;
    }
    throw error;
  }
}
