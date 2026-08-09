// HTTP API client for turn-level records: the assembled-context report.
//
// A turn's context report is the stored account of how its context was
// built — which elements contributed, at what token cost, with what
// compaction and warnings. The gateway records it at assembly time; this
// client only ever reads it back.

let baseUrl = "";

export function setBaseUrl(url: string): void {
  baseUrl = url;
}

// These types transcribe swage-context's AssemblyReport serialisation.
// The report types are not yet part of swage-protocol's projected wire
// contract (the one-definition rule says they should be — tracked in the
// annexe); until they are projected, this file is the single frontend
// transcription and nothing else may re-declare these shapes.
export interface ElementReport {
  name: string;
  source: string;
  token_count: number;
  preview?: string | null;
  priority: number;
}

export interface CompactionRecord {
  element: string;
  requested_strategy: string;
  actual_strategy: string;
  messages_removed: number;
  tokens_before: number;
  tokens_after: number;
}

export interface AssemblyErrorReport {
  assembly_name: string;
  error_kind: string;
  message: string;
  fallback_used: string;
}

export interface TurnContextReport {
  assembly_name: string;
  elements: ElementReport[];
  total_tokens: number;
  compaction_applied: CompactionRecord[];
  warnings: string[];
  errors: AssemblyErrorReport[];
  // serde's default Duration serialisation.
  duration: { secs: number; nanos: number };
}

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
  const response = await fetch(`${baseUrl}/api/turns/${turnId}/context`);
  if (response.status === 404) {
    return null;
  }
  if (!response.ok) {
    throw new Error(
      `failed to load the turn's context report: HTTP ${response.status}`,
    );
  }
  const body = (await response.json()) as { report: TurnContextReport };
  return body.report;
}
