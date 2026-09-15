import type { SkillItem } from "../protocol/SkillItem";
import type { SkillListResponse } from "../protocol/SkillListResponse";
import { ApiError } from "./error";

export async function listSkills(topicId: string): Promise<SkillItem[]> {
  const path = `/api/skills?topic_id=${encodeURIComponent(topicId)}`;
  const response = await fetch(path, { method: "GET" });
  if (!response.ok) {
    throw await ApiError.fromResponse(response, "GET", path);
  }
  const body = (await response.json()) as SkillListResponse;
  return body.skills;
}
