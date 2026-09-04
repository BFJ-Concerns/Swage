/**
 * Tests for the relativeTime helper.
 *
 * Covers the boundaries between each display tier so the output is
 * predictable without needing to freeze the system clock.
 */
import { describe, it, expect } from "vitest";

import { relativeTime } from "../utils/relative-time";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Return an ISO timestamp that is `ms` milliseconds in the past. */
function ago(ms: number): string {
  return new Date(Date.now() - ms).toISOString();
}

const MINUTE = 60 * 1000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("relativeTime", () => {
  it("returns 'just now' for timestamps within the last minute", () => {
    expect(relativeTime(ago(30 * 1000))).toBe("just now");
    expect(relativeTime(ago(59 * 1000))).toBe("just now");
  });

  it("returns minutes-ago for timestamps 1–59 minutes old", () => {
    expect(relativeTime(ago(1 * MINUTE))).toBe("1m ago");
    expect(relativeTime(ago(5 * MINUTE))).toBe("5m ago");
    expect(relativeTime(ago(59 * MINUTE))).toBe("59m ago");
  });

  it("returns hours-ago for timestamps 1–23 hours old", () => {
    expect(relativeTime(ago(1 * HOUR))).toBe("1h ago");
    expect(relativeTime(ago(12 * HOUR))).toBe("12h ago");
    expect(relativeTime(ago(23 * HOUR))).toBe("23h ago");
  });

  it("returns 'yesterday' for timestamps 1–2 days old", () => {
    expect(relativeTime(ago(1 * DAY))).toBe("yesterday");
    // Just under two full days is still "yesterday" (not a date).
    expect(relativeTime(ago(1.9 * DAY))).toBe("yesterday");
  });

  it("returns a short date for timestamps older than 2 days", () => {
    const result = relativeTime(ago(3 * DAY));
    // A day+month format (en-GB locale: "17 Mar"; "1 Sept" under newer ICU data) — not a relative string.
    expect(result).toMatch(/^\d{1,2} [A-Z][a-z]{2,3}$/);
  });

  it("handles ISO string input (not just Date objects)", () => {
    const iso = new Date(Date.now() - 3 * MINUTE).toISOString();
    expect(relativeTime(iso)).toBe("3m ago");
  });
});
