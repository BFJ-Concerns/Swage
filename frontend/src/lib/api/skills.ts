import type { SkillItem } from "../protocol/SkillItem";
import type { SkillListResponse } from "../protocol/SkillListResponse";

export async function listSkills(spaceId: string): Promise<SkillItem[]> {
  const path = `/api/skills?space_id=${encodeURIComponent(spaceId)}`;
  const response = await fetch(path, { method: "GET" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status} from GET ${path}`);
  }
  const body = (await response.json()) as SkillListResponse;
  return body.skills;
}
