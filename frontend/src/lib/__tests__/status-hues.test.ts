// @ts-nocheck -- these fixtures use Node's filesystem APIs under Vitest; the
// browser-only application type-check intentionally does not include Node types.
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
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
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

  it("keeps shared primary-button ink AA-safe on the accent fill", () => {
    const primaryFillRows = result.rows.filter(
      (r) => r.register === "--primary-fill-ink on --accent",
    );
    expect(primaryFillRows).toHaveLength(2);
    for (const row of primaryFillRows) {
      expect(row.contrast).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("routes shared primary-button foreground paint through the AA-safe role", () => {
    const risoCss = readFileSync(RISO_CSS_PATH, "utf8");
    expect(risoCss).toMatch(
      /\.riso-btn\.primary\s*\{[^}]*color:\s*var\(--primary-fill-ink\)\s*;/s,
    );
  });

  it("keeps the dark error red clear of the action orange (the collision fix)", () => {
    const darkErrorVsAccent = result.rows.find(
      (r) => r.scheme === "dark" && r.errorVsAccent != null,
    );
    // The dark --error/--accent gap was ~6 ΔE00 before this work; hold it wide.
    expect(darkErrorVsAccent?.errorVsAccent).toBeGreaterThan(15);
  });

  it("keeps confirmation feedback distinct from every dark status register", () => {
    const darkSuccessDistances = result.rows.filter(
      (r) => r.scheme === "dark" && r.successVsRegister != null,
    );
    expect(darkSuccessDistances).toHaveLength(6);
    for (const row of darkSuccessDistances) {
      expect(row.successVsRegister).toBeGreaterThanOrEqual(12);
    }
  });

  it("rejects an unresolved fallback that is not a colour", () => {
    const fixture = writeRisoFixture((css) =>
      css.replace("--status-in-motion-hue: #0f6fb2", "--status-in-motion-hue: var(--missing-hue, not-a-colour)"),
    );
    try {
      expect(() => measure(fixture.path)).toThrow("expected a hex colour");
    } finally {
      rmSync(fixture.directory, { recursive: true, force: true });
    }
  });

  it("uses a later root block when resolving shared tokens", () => {
    const fixture = writeRisoFixture((css) => `${css}\n:root { --accent: var(--error); }\n`);
    try {
      const result = measure(fixture.path);
      expect(result.ok).toBe(false);
      expect(result.failures).toContainEqual(expect.stringContaining("--error"));
    } finally {
      rmSync(fixture.directory, { recursive: true, force: true });
    }
  });
});

function writeRisoFixture(transform: (css: string) => string): { directory: string; path: string } {
  const directory = mkdtempSync(join(tmpdir(), "swage-status-hues-"));
  const fixture = join(directory, "riso.css");
  writeFileSync(fixture, transform(readFileSync(RISO_CSS_PATH, "utf8")));
  return { directory, path: fixture };
}
