// Relative timestamp formatting for conversation list entries.
//
// Returns short, human-readable strings — "2m ago", "yesterday", "Jan 5" —
// so the sidebar doesn't waste space on full timestamps.

/**
 * Format an ISO timestamp as a human-readable relative string.
 *
 * Display tiers:
 *   < 1 minute  →  "just now"
 *   < 1 hour    →  "Nm ago"
 *   < 24 hours  →  "Nh ago"
 *   < 2 days    →  "yesterday"
 *   Otherwise   →  "Jan 5" (or "Jan 5, 2024" if not this year)
 */
export function relativeTime(iso: string): string {
  const now = Date.now();
  const then = new Date(iso).getTime();
  const diffMs = now - then;

  const MINUTE = 60 * 1000;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;

  if (diffMs < MINUTE) {
    return "just now";
  }

  if (diffMs < HOUR) {
    return `${Math.floor(diffMs / MINUTE)}m ago`;
  }

  if (diffMs < DAY) {
    return `${Math.floor(diffMs / HOUR)}h ago`;
  }

  if (diffMs < 2 * DAY) {
    return "yesterday";
  }

  // Older: show a short calendar date. Include year only if it differs from now.
  const date = new Date(iso);
  const thisYear = new Date().getFullYear();
  const options: Intl.DateTimeFormatOptions =
    date.getFullYear() === thisYear
      ? { month: "short", day: "numeric" }
      : { month: "short", day: "numeric", year: "numeric" };

  return date.toLocaleDateString("en-GB", options);
}
