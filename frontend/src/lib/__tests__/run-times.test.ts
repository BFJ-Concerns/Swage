import { describe, expect, it } from "vitest";
import { formatRunDuration, formatRunTime } from "../run-times";

describe("run times", () => {
  it.each([
    "2026-10-01T19:28:49.393288230Z",
    "2026-10-01T21:28:49.393288230+02:00",
  ])("renders %s in the reader's locale", (stamp) => {
    expect(formatRunTime(stamp)).toBe(new Date(stamp).toLocaleString());
    expect(formatRunTime(stamp)).not.toBe(stamp);
  });

  it.each([0, 1.5, 125.2])("renders an elapsed %s seconds", (seconds) => {
    const start = "2026-10-01T19:28:49.393Z";
    const end = new Date(new Date(start).getTime() + seconds * 1000).toISOString();
    expect(formatRunDuration(start, end)).toBe(
      `${seconds.toLocaleString(undefined, { maximumFractionDigits: 1 })} s`,
    );
  });
});
