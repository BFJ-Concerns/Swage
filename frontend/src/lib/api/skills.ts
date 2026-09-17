import type { SkillItem } from "../protocol/SkillItem";
import type { SkillListResponse } from "../protocol/SkillListResponse";
import { ApiError } from "./error";

/**
 * The skills resolved for a topic's hierarchy, or — with no topic — for the
 * general scope, which has the global layer alone beneath the agent's own.
 */
export async function listSkills(topicId: string | null): Promise<SkillItem[]> {
  const path = topicId
    ? `/api/skills?topic_id=${encodeURIComponent(topicId)}`
    : "/api/skills";
  const response = await fetch(path, { method: "GET" });
  if (!response.ok) {
    throw await ApiError.fromResponse(response, "GET", path);
  }
  const body = (await response.json()) as SkillListResponse;
  return body.skills;
}
