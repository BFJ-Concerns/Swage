// The operator's memory as Settings reads and changes it: pages listed and
// read per scope, and changes made only by instructing the curator.

import * as generated from "../protocol/client";
import type { MemoryInstructionResponse } from "../protocol/MemoryInstructionResponse";
import type { MemoryPageResponse } from "../protocol/MemoryPageResponse";
import type { MemoryPageSummary } from "../protocol/MemoryPageSummary";

/** Every page of one scope: a topic's, or — with no topic — general memory. */
export async function listMemoryPages(topicId: string | null): Promise<MemoryPageSummary[]> {
  const body = await generated.listMemoryPages(topicId ?? undefined);
  return body.pages;
}

/** One page, whole, from the scope it belongs to. */
export function getMemoryPage(reference: string, topicId: string | null): Promise<MemoryPageResponse> {
  return generated.getMemoryPage(reference, topicId ?? undefined);
}

/** Tell the curator what to change in a scope's memory. */
export function instructCurator(
  topicId: string | null,
  instruction: string,
): Promise<MemoryInstructionResponse> {
  return generated.applyMemoryInstruction({
    scope: topicId ? { kind: "topic", topic_id: topicId } : { kind: "general" },
    instruction,
  });
}
