/**
 * Palette guard for the status registers.
 *
 * The real arbiter is scripts/verify-status-hues.mjs (run via `npm run
 * verify:hues`), which measures the ACTUAL --status-*-hue tokens in riso.css.
 * This test runs the same measurement inside the suite so `npm test` and CI
 * fail the moment a token drifts below AA contrast or role separation — the
 * failure mode the register system exists to make catchable rather than
 * aesthetic folklore.
 */
import { describe, it, expect } from "vitest";
// The measurement module is dependency-free JS shared with the CLI script.
import { measure, RISO_CSS_PATH } from "../../../scripts/verify-status-hues.mjs";

describe("status register hues", () => {
  const result = measure(RISO_CSS_PATH);

  it("passes every AA contrast and ΔE00 role-separation check", () => {
    // A non-empty failures array is the script's own verdict — surface it
    // verbatim so a broken token names itself in the test output.
    expect(result.failures).toEqual([]);
    expect(result.ok).toBe(true);
  });

  it("keeps the dark error red clear of the action orange (the collision fix)", () => {
    const darkErrorVsAccent = result.rows.find(
      (r) => r.scheme === "dark" && r.errorVsAccent != null,
    );
    // The dark --error/--accent gap was ~6 ΔE00 before this work; hold it wide.
    expect(darkErrorVsAccent?.errorVsAccent).toBeGreaterThan(15);
  });
});
