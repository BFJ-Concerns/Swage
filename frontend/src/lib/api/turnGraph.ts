import type { ProposeTurnGraphRequest } from "../protocol/ProposeTurnGraphRequest";
import type { TurnGraphDiff } from "../protocol/TurnGraphDiff";
import type { TurnGraphResponse } from "../protocol/TurnGraphResponse";
import type { TurnGraphRevision } from "../protocol/TurnGraphRevision";
import * as generated from "../protocol/client";

/** The active revision and every revision recorded. */
export async function getTurnGraph(): Promise<TurnGraphResponse> {
  return generated.getTurnGraph();
}

/** Propose a revision: recorded either way, activated when it validates. */
export async function proposeTurnGraph(request: ProposeTurnGraphRequest): Promise<TurnGraphRevision> {
  return generated.proposeTurnGraph(request);
}

/** Make a recorded revision the one that serves, re-validated. */
export async function activateTurnGraphRevision(revision: number): Promise<TurnGraphRevision> {
  return generated.activateTurnGraphRevision(String(revision));
}

/** What changed from one revision to another. */
export async function diffTurnGraph(from: number, to: number): Promise<TurnGraphDiff> {
  return generated.diffTurnGraph(String(from), String(to));
}
