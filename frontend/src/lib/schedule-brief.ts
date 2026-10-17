import type { Schedule } from "./protocol/Schedule";

/** A bounded heading and whether the complete brief needs a disclosure. */
export function formatScheduleBrief(schedule: Pick<Schedule, "id" | "message">): {
  heading: string;
  truncated: boolean;
} {
  const brief = schedule.message.trim();
  const truncated = brief.length > 72;
  return {
    heading: truncated
      ? `${brief.slice(0, 60).replace(/\s+/g, " ")}… · ${schedule.id.slice(-8)}`
      : brief.replace(/\s+/g, " "),
    truncated,
  };
}
