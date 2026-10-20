/** Display recorded run timestamps locally; callers retain the stamp in <time>. */
export function formatRunTime(stamp: string): string {
  return new Date(stamp).toLocaleString();
}

/** Elapsed provider time, in seconds, from the recorded start and end. */
export function formatRunDuration(start: string, end: string): string {
  const seconds = (new Date(end).getTime() - new Date(start).getTime()) / 1000;
  return `${seconds.toLocaleString(undefined, { maximumFractionDigits: 1 })} s`;
}
