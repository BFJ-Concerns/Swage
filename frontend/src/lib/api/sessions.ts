// HTTP API client for the recent-sessions list: every scope's sessions ordered
// by their latest user or assistant message, automated sessions grouped
// beneath their source. The wire shape is the generated client's; this module
// only unwraps the envelope and turns the caller's options into query values.

import type { RecentSessionsEntry } from "../protocol/RecentSessionsEntry";
import * as generated from "../protocol/client";

/**
 * Fetch the recent-sessions list. `topicId` narrows to one topic's sessions
 * (null lists every scope); `archived` lists the archived sessions instead of
 * the live ones.
 */
export async function listRecentSessions(
  topicId: string | null,
  archived = false,
): Promise<RecentSessionsEntry[]> {
  const response = await generated.listRecentSessions(
    topicId ?? undefined,
    archived ? "true" : undefined,
  );
  return response.entries;
}
