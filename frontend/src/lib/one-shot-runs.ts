import type { Schedule } from "./protocol/Schedule";
import type { ScheduleRun } from "./protocol/ScheduleRun";

/** How often a card re-reads a run that can still change state. */
export const UNSETTLED_RUN_REFRESH_MS = 3000;

/** A one-shot schedule whose moment has passed; its card reports its latest run. */
export function passedOneShot(schedule: Schedule): boolean {
  return schedule.spec.kind === "one_shot" && Date.parse(schedule.spec.at) <= Date.now();
}

/** Whether a run has yet to reach an outcome. */
export function runUnsettled(run: ScheduleRun | undefined): boolean {
  return run?.state === "pending" || run?.state === "running";
}
