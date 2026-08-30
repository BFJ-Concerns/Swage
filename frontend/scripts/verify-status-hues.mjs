// @ts-nocheck — dependency-free Node ESM verification script, not app TS. Its
// correctness is exercised at runtime by src/lib/__tests__/status-hues.test.ts
// (which checks the measurements against known-good values), so tsc leaves the
// node globals and colour maths here alone.
/*
  Status-register hue verification.

  The status badge paints six semantic registers (calm / in-motion /
  wants-a-human / not-yet / elsewhere / trouble). This script is the arbiter of
  their colour values: it reads the ACTUAL --status-*-hue tokens (plus --ink,
  --paper, --error, --accent) out of riso.css for both themes and checks two
  things the eye cannot be trusted to judge:

    1. WCAG 2.1 contrast — every register's text/glyph and the shared primary
       button ink clear AA (4.5:1) on the backing they actually render on.
    2. CIEDE2000 (ΔE00) role separation — every spot hue stays measurably clear
       of the two warm roles the palette reserves (--accent owns action,
       --error owns trouble), and confirmation feedback (--success) stays
       distinct from every status register. This is the check that caught the
       dark-scheme collision (--error sat ~6 ΔE00 from --accent); the fixes are
       gated here.

  Pairwise distances between the registers are reported for awareness but only
  warn — several pairs lean on treatment + label by design (calm ink vs not-yet
  slate are both muted blues, told apart by solid-vs-outline and the label).

  No dependencies: sRGB→linear→XYZ→Lab→ΔE00 and WCAG luminance are inlined.
  Run:  node scripts/verify-status-hues.mjs   (exit 1 if any hard check fails)
  The measure() export is also asserted by the vitest suite so `npm test` and
  CI catch a token that drifts out of tolerance.
*/

import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, resolve } from "node:path";

// ---- Thresholds (documented, tunable) --------------------------------------

// A spot hue this close to a reserved role would start to read as that role.
// Set in the spirit of the riso palette's own role separations (~18 ΔE00).
const MIN_DELTA_E_VS_ROLE = 15; // spot hue vs --accent, and --error vs --accent
const MIN_DELTA_E_VS_ERROR = 12; // spot hue vs --error (must not read as trouble)
const MIN_DELTA_E_SUCCESS_VS_REGISTER = 12; // confirmation feedback vs status registers
const AA_SMALL_TEXT = 4.5; // WCAG 2.1 AA for small text/glyph
const WARN_DELTA_E_PAIR = 10; // pairwise register distance below this: warn only

// ---- Colour maths ----------------------------------------------------------

/** "#rrggbb" → [r,g,b] in 0..255. */
function hexToRgb(hex) {
  const h = hex.trim().replace(/^#/, "");
  if (!/^(?:[\da-f]{3}|[\da-f]{6})$/i.test(h)) {
    throw new Error(`expected a hex colour, received ${hex}`);
  }
  const full = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  return [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16));
}

/** sRGB channel (0..255) → linear (0..1). */
function toLinear(c) {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

/** WCAG relative luminance of an sRGB colour. */
function relativeLuminance([r, g, b]) {
  return 0.2126 * toLinear(r) + 0.7152 * toLinear(g) + 0.0722 * toLinear(b);
}

/** WCAG 2.1 contrast ratio between two sRGB colours. */
function contrastRatio(rgbA, rgbB) {
  const la = relativeLuminance(rgbA);
  const lb = relativeLuminance(rgbB);
  const [hi, lo] = la >= lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/** sRGB (0..255) → CIE Lab (D65). */
function rgbToLab([r, g, b]) {
  const [rl, gl, bl] = [toLinear(r), toLinear(g), toLinear(b)];
  // Linear sRGB → XYZ (D65).
  let x = rl * 0.4124564 + gl * 0.3575761 + bl * 0.1804375;
  let y = rl * 0.2126729 + gl * 0.7151522 + bl * 0.072175;
  let z = rl * 0.0193339 + gl * 0.119192 + bl * 0.9503041;
  // Normalise to D65 white.
  x /= 0.95047;
  y /= 1.0;
  z /= 1.08883;
  const f = (t) => (t > 0.008856 ? Math.cbrt(t) : 7.787 * t + 16 / 116);
  const [fx, fy, fz] = [f(x), f(y), f(z)];
  return [116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)];
}

/** CIEDE2000 colour difference between two sRGB colours. */
function deltaE00(rgbA, rgbB) {
  const [l1, a1, b1] = rgbToLab(rgbA);
  const [l2, a2, b2] = rgbToLab(rgbB);
  const rad = Math.PI / 180;
  const deg = 180 / Math.PI;

  const avgLp = (l1 + l2) / 2;
  const c1 = Math.hypot(a1, b1);
  const c2 = Math.hypot(a2, b2);
  const avgC = (c1 + c2) / 2;

  const g =
    0.5 * (1 - Math.sqrt(Math.pow(avgC, 7) / (Math.pow(avgC, 7) + Math.pow(25, 7))));
  const a1p = a1 * (1 + g);
  const a2p = a2 * (1 + g);
  const c1p = Math.hypot(a1p, b1);
  const c2p = Math.hypot(a2p, b2);
  const avgCp = (c1p + c2p) / 2;

  const hp = (ap, bp) => {
    if (ap === 0 && bp === 0) return 0;
    const h = Math.atan2(bp, ap) * deg;
    return h >= 0 ? h : h + 360;
  };
  const h1p = hp(a1p, b1);
  const h2p = hp(a2p, b2);

  let dhp;
  if (c1p * c2p === 0) dhp = 0;
  else if (Math.abs(h2p - h1p) <= 180) dhp = h2p - h1p;
  else if (h2p - h1p > 180) dhp = h2p - h1p - 360;
  else dhp = h2p - h1p + 360;

  const dLp = l2 - l1;
  const dCp = c2p - c1p;
  const dHp = 2 * Math.sqrt(c1p * c2p) * Math.sin((dhp * rad) / 2);

  let avgHp;
  if (c1p * c2p === 0) avgHp = h1p + h2p;
  else if (Math.abs(h1p - h2p) <= 180) avgHp = (h1p + h2p) / 2;
  else if (h1p + h2p < 360) avgHp = (h1p + h2p + 360) / 2;
  else avgHp = (h1p + h2p - 360) / 2;

  const t =
    1 -
    0.17 * Math.cos((avgHp - 30) * rad) +
    0.24 * Math.cos(2 * avgHp * rad) +
    0.32 * Math.cos((3 * avgHp + 6) * rad) -
    0.2 * Math.cos((4 * avgHp - 63) * rad);

  const dTheta = 30 * Math.exp(-Math.pow((avgHp - 275) / 25, 2));
  const rc =
    2 * Math.sqrt(Math.pow(avgCp, 7) / (Math.pow(avgCp, 7) + Math.pow(25, 7)));
  const sl =
    1 + (0.015 * Math.pow(avgLp - 50, 2)) / Math.sqrt(20 + Math.pow(avgLp - 50, 2));
  const sc = 1 + 0.045 * avgCp;
  const sh = 1 + 0.015 * avgCp * t;
  const rt = -Math.sin(2 * dTheta * rad) * rc;

  return Math.sqrt(
    Math.pow(dLp / sl, 2) +
      Math.pow(dCp / sc, 2) +
      Math.pow(dHp / sh, 2) +
      rt * (dCp / sc) * (dHp / sh),
  );
}

// ---- riso.css token parsing ------------------------------------------------

/** Strip /* *\/ block comments so declaration parsing is clean. */
function stripComments(css) {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

/** Pull every declaration body for `selector { ... }` blocks, in source order. */
function blockBodies(css, selector) {
  const re = new RegExp(
    selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "\\s*\\{([^}]*)\\}",
    "g",
  );
  return Array.from(stripComments(css).matchAll(re), (match) => match[1]);
}

/** Parse `--name: value;` pairs from a block body into a Map. */
function parseDecls(body) {
  const map = new Map();
  for (const decl of body.split(";")) {
    const m = decl.match(/(--[\w-]+)\s*:\s*(.+)/);
    if (m) map.set(m[1].trim(), m[2].trim());
  }
  return map;
}

/** Later blocks override earlier blocks, matching CSS custom-property cascade. */
function parseSelectorDecls(css, selector) {
  return new Map(blockBodies(css, selector).flatMap((body) => [...parseDecls(body)]));
}

/**
 * Resolve a token to a concrete #hex, following one or more `var(--x)` hops
 * within the theme first, then falling back to :root. Non-hex, non-var values
 * (rgba(), color-mix()) are returned as-is — the caller only asks for tokens
 * that resolve to hex.
 */
function resolveHex(name, theme, root, seen = new Set()) {
  if (seen.has(name)) throw new Error(`var() cycle at ${name}`);
  seen.add(name);
  const raw = theme.get(name) ?? root.get(name);
  if (raw === undefined) throw new Error(`token ${name} not found`);
  const varMatch = raw.match(/^var\((--[\w-]+)(?:,\s*(.+))?\)$/);
  if (varMatch) {
    const [, reference, fallback] = varMatch;
    if (theme.has(reference) || root.has(reference)) {
      return resolveHex(reference, theme, root, seen);
    }
    if (fallback) return fallback.trim();
    throw new Error(`token ${reference} not found while resolving ${name}`);
  }
  return raw;
}

// ---- The registers and how each is measured --------------------------------
// treatment 'stamp'   → solid fill of the hue, text is --paper (measure paper/fill)
// treatment 'outline' → transparent, line + text are the hue (measure hue/paper)
const REGISTERS = [
  { key: "calm", token: "--status-calm-hue", treatment: "stamp" },
  { key: "in-motion", token: "--status-in-motion-hue", treatment: "outline" },
  { key: "wants-a-human", token: "--status-wants-human-hue", treatment: "stamp" },
  { key: "not-yet", token: "--status-not-yet-hue", treatment: "outline" },
  { key: "elsewhere", token: "--status-elsewhere-hue", treatment: "outline" },
  { key: "trouble", token: "--status-trouble-hue", treatment: "stamp" },
];

// The two warm roles a spot hue must stay clear of. calm/trouble ARE roles
// (ink / error), so they are exempt from the spot-hue role checks.
const SPOT_KEYS = new Set(["in-motion", "wants-a-human", "not-yet", "elsewhere"]);

/**
 * Read riso.css and measure every register in both themes. Returns a plain
 * object (ok + failures + rows) so the vitest suite can assert on it.
 */
export function measure(cssPath) {
  const css = readFileSync(cssPath, "utf8");
  const root = parseSelectorDecls(css, ":root");
  const themes = {
    light: parseSelectorDecls(css, ".theme-riso"),
    dark: parseSelectorDecls(css, ".theme-cyanotype"),
  };

  const failures = [];
  const warnings = [];
  const rows = [];

  for (const [scheme, theme] of Object.entries(themes)) {
    const paper = hexToRgb(resolveHex("--paper", theme, root));
    const accent = hexToRgb(resolveHex("--accent", theme, root));
    const primaryFillInk = hexToRgb(resolveHex("--primary-fill-ink", theme, root));
    const error = hexToRgb(resolveHex("--error", theme, root));
    const success = hexToRgb(resolveHex("--success", theme, root));

    const primaryFillContrast = contrastRatio(primaryFillInk, accent);
    if (primaryFillContrast < AA_SMALL_TEXT) {
      failures.push(
        `[${scheme}] --primary-fill-ink: contrast ${primaryFillContrast.toFixed(2)} < ${AA_SMALL_TEXT} (text on --accent)`,
      );
    }
    rows.push({ scheme, register: "--primary-fill-ink on --accent", contrast: primaryFillContrast });

    // Resolve each register's representative hue for this scheme.
    const hues = {};
    for (const reg of REGISTERS) hues[reg.key] = hexToRgb(resolveHex(reg.token, theme, root));

    for (const reg of REGISTERS) {
      const hue = hues[reg.key];
      // Contrast on the backing this register actually paints.
      const contrast =
        reg.treatment === "stamp" ? contrastRatio(paper, hue) : contrastRatio(hue, paper);
      const backing = reg.treatment === "stamp" ? "text on fill" : "line on paper";
      const aaPass = contrast >= AA_SMALL_TEXT;
      if (!aaPass) {
        failures.push(
          `[${scheme}] ${reg.key}: contrast ${contrast.toFixed(2)} < ${AA_SMALL_TEXT} (${backing})`,
        );
      }

      // Role separation — spot hues only (calm/trouble ARE the roles).
      let dAccent = null;
      let dError = null;
      if (SPOT_KEYS.has(reg.key)) {
        dAccent = deltaE00(hue, accent);
        dError = deltaE00(hue, error);
        if (dAccent < MIN_DELTA_E_VS_ROLE) {
          failures.push(
            `[${scheme}] ${reg.key}: ΔE00 ${dAccent.toFixed(1)} from --accent < ${MIN_DELTA_E_VS_ROLE}`,
          );
        }
        if (dError < MIN_DELTA_E_VS_ERROR) {
          failures.push(
            `[${scheme}] ${reg.key}: ΔE00 ${dError.toFixed(1)} from --error < ${MIN_DELTA_E_VS_ERROR}`,
          );
        }
      }

      rows.push({ scheme, register: reg.key, treatment: reg.treatment, contrast, dAccent, dError });
    }

    // The collision fix itself: --error must stay clear of --accent.
    const errorVsAccent = deltaE00(error, accent);
    if (errorVsAccent < MIN_DELTA_E_VS_ROLE) {
      failures.push(
        `[${scheme}] --error: ΔE00 ${errorVsAccent.toFixed(1)} from --accent < ${MIN_DELTA_E_VS_ROLE}`,
      );
    }
    rows.push({ scheme, register: "--error vs --accent", errorVsAccent });

    for (const reg of REGISTERS) {
      const successVsRegister = deltaE00(success, hues[reg.key]);
      if (successVsRegister < MIN_DELTA_E_SUCCESS_VS_REGISTER) {
        failures.push(
          `[${scheme}] --success: ΔE00 ${successVsRegister.toFixed(1)} from ${reg.token} < ${MIN_DELTA_E_SUCCESS_VS_REGISTER}`,
        );
      }
      rows.push({ scheme, register: `--success vs ${reg.token}`, successVsRegister });
    }

    // Pairwise register distinctness (warn only).
    const keys = REGISTERS.map((r) => r.key);
    for (let i = 0; i < keys.length; i++) {
      for (let j = i + 1; j < keys.length; j++) {
        const d = deltaE00(hues[keys[i]], hues[keys[j]]);
        if (d < WARN_DELTA_E_PAIR) {
          warnings.push(
            `[${scheme}] ${keys[i]} ↔ ${keys[j]}: ΔE00 ${d.toFixed(1)} (leans on treatment + label)`,
          );
        }
      }
    }
  }

  return { ok: failures.length === 0, failures, warnings, rows };
}

// ---- CLI -------------------------------------------------------------------

function formatReport(result) {
  const lines = [];
  lines.push("Status-register hue verification");
  lines.push("=".repeat(64));
  for (const scheme of ["light", "dark"]) {
    lines.push(`\n${scheme.toUpperCase()} scheme`);
    lines.push(
      "  register        treatment  contrast   ΔE00·accent  ΔE00·error",
    );
    for (const r of result.rows.filter((x) => x.scheme === scheme && x.register.startsWith("--") === false)) {
      const c = r.contrast.toFixed(2).padStart(6);
      const da = r.dAccent == null ? "     —" : r.dAccent.toFixed(1).padStart(6);
      const de = r.dError == null ? "     —" : r.dError.toFixed(1).padStart(6);
      lines.push(
        `  ${r.register.padEnd(14)}  ${r.treatment.padEnd(8)}  ${c}:1   ${da}      ${de}`,
      );
    }
    const ea = result.rows.find((x) => x.scheme === scheme && x.errorVsAccent != null);
    lines.push(`  --error vs --accent: ΔE00 ${ea.errorVsAccent.toFixed(1)}`);
    const primary = result.rows.find((x) => x.scheme === scheme && x.register === "--primary-fill-ink on --accent");
    lines.push(`  --primary-fill-ink on --accent: ${primary.contrast.toFixed(2)}:1`);
    for (const r of result.rows.filter((x) => x.scheme === scheme && x.successVsRegister != null)) {
      lines.push(`  ${r.register}: ΔE00 ${r.successVsRegister.toFixed(1)}`);
    }
  }
  if (result.warnings.length) {
    lines.push("\nWarnings (informational — carried by treatment + label):");
    for (const w of result.warnings) lines.push(`  · ${w}`);
  }
  lines.push("");
  if (result.ok) lines.push("PASS — all registers clear AA and role separation.");
  else {
    lines.push("FAIL:");
    for (const f of result.failures) lines.push(`  ✗ ${f}`);
  }
  return lines.join("\n");
}

// Resolve riso.css relative to this script so it runs from any cwd.
export const RISO_CSS_PATH = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../src/lib/styles/riso.css",
);

// Run the CLI only when invoked directly (`node scripts/verify-status-hues.mjs`),
// not when imported (the vitest suite, or `node -e` with no argv[1]).
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const result = measure(RISO_CSS_PATH);
  console.log(formatReport(result));
  process.exit(result.ok ? 0 : 1);
}
