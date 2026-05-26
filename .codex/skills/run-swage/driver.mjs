#!/usr/bin/env node
// Headless browser driver for the Swage web UI.
//
// The product surface that most PRs touch is the Svelte SPA served by the
// `swage` gateway binary. This driver reaches into that running app: navigate
// to a route, wait for it to hydrate, optionally evaluate page state, and write
// a screenshot to disk so a headless agent can actually *see* the result.
//
// There is no `chromium-cli` on this machine, but Playwright's bundled Chromium
// is already installed (it powers the desktop smoke test). We borrow Playwright
// from `desktop/node_modules` rather than vendoring a second copy / browser
// download — see resolvePlaywright() below.
//
// Usage:
//   node driver.mjs screenshot <url> [--out FILE] [--route PATH] [--wait SEL]
//                                     [--full-page] [--settle MS]
//   node driver.mjs probe <url> [--route PATH]    # report DOM landmarks as JSON
//   node driver.mjs flow <url> [--out FILE]        # first-run UI peek + screenshot
//
// Examples (all run in this container during skill authoring):
//   node driver.mjs screenshot http://127.0.0.1:3939 --out /tmp/swage-home.png
//   node driver.mjs screenshot http://127.0.0.1:3939 --route /settings --out /tmp/swage-settings.png
//   node driver.mjs probe http://127.0.0.1:3939

import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// driver.mjs lives at <repo>/.claude/skills/run-swage/ — climb to the repo root.
const repoRoot = path.resolve(__dirname, "..", "..", "..");
const desktopNodeModules = path.join(repoRoot, "desktop", "node_modules");

// Playwright is an ESM package; NODE_PATH does not influence ESM bare-specifier
// resolution, so we resolve its entry point explicitly against desktop's
// node_modules and dynamic-import the absolute path.
async function resolvePlaywright() {
  const require = createRequire(import.meta.url);
  let entry;
  try {
    entry = require.resolve("playwright", { paths: [desktopNodeModules] });
  } catch {
    throw new Error(
      `Could not find Playwright under ${desktopNodeModules}. ` +
        `Run: npm --prefix desktop ci`,
    );
  }
  // Playwright's entry is CommonJS; dynamic import() puts its exports on
  // `.default` under Node's CJS interop, while a true ESM build would expose
  // them as named exports. Accept whichever shape we get.
  const mod = await import(entry);
  return mod.chromium ? mod : mod.default;
}

function parseFlags(args) {
  const flags = { _: [] };
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === "--full-page") flags.fullPage = true;
    else if (arg === "--out") flags.out = args[++i];
    else if (arg === "--route") flags.route = args[++i];
    else if (arg === "--wait") flags.wait = args[++i];
    else if (arg === "--settle") flags.settle = Number(args[++i]);
    else flags._.push(arg);
  }
  return flags;
}

// Wait for the SPA to mount. The app mounts into <div id="app">; an empty app
// div means Svelte has not hydrated yet (or the bundle 404'd).
async function waitForApp(page, customSelector) {
  if (customSelector) {
    await page.waitForSelector(customSelector, { timeout: 15_000 });
    return;
  }
  await page.waitForFunction(
    () => {
      const root = document.getElementById("app");
      return root && root.children.length > 0;
    },
    { timeout: 15_000 },
  );
}

async function withPage(url, fn) {
  const { chromium } = await resolvePlaywright();
  const browser = await chromium.launch({ args: ["--no-sandbox"] });
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    const consoleErrors = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 20_000 });
    return await fn(page, consoleErrors);
  } finally {
    await browser.close();
  }
}

async function screenshot(flags) {
  const base = flags._[0];
  if (!base) throw new Error("screenshot: missing <url>");
  const target = flags.route ? new URL(flags.route, base).href : base;
  const out = flags.out ?? "/tmp/swage-screenshot.png";

  await withPage(target, async (page, consoleErrors) => {
    await waitForApp(page, flags.wait);
    // Give async loads (bench list, lens runtime) a beat to paint.
    await page.waitForTimeout(flags.settle ?? 600);
    await page.screenshot({ path: out, fullPage: Boolean(flags.fullPage) });
    console.log(`screenshot -> ${out}`);
    console.log(`title: ${await page.title()}`);
    if (consoleErrors.length) {
      console.log(`console errors (${consoleErrors.length}):`);
      for (const err of consoleErrors.slice(0, 10)) console.log(`  ${err}`);
    }
  });
}

// Report DOM landmarks as JSON. Playwright's page.evaluate runs the supplied
// function in the page context — no eval(), no arbitrary string injection.
async function probe(flags) {
  const base = flags._[0];
  if (!base) throw new Error("probe: missing <url>");
  const target = flags.route ? new URL(flags.route, base).href : base;
  await withPage(target, async (page, consoleErrors) => {
    await waitForApp(page, flags.wait);
    await page.waitForTimeout(flags.settle ?? 400);
    const landmarks = await page.evaluate(() => ({
      title: document.title,
      url: location.href,
      headings: [...document.querySelectorAll("h1, h2, h3")]
        .map((h) => h.textContent.trim())
        .filter(Boolean)
        .slice(0, 12),
      buttons: [...document.querySelectorAll("button")]
        .map((b) => b.textContent.trim())
        .filter(Boolean)
        .slice(0, 16),
    }));
    console.log(JSON.stringify({ ...landmarks, consoleErrors }, null, 2));
  });
}

// Drive the first-run flow end to end: the SPA opens on the bench picker, so
// click the default bench card, open it, and screenshot the workspace that
// loads. This proves the app is genuinely interactive, not just rendering a
// static landing page. Extend with form fills / message sends as PRs demand.
async function flow(flags) {
  const base = flags._[0];
  if (!base) throw new Error("flow: missing <url>");
  const out = flags.out ?? "/tmp/swage-flow.png";
  await withPage(base, async (page, consoleErrors) => {
    await waitForApp(page, flags.wait);
    await page.waitForTimeout(600);

    // Two-stage bench select: click the card to preview, then "Open bench".
    await page.getByRole("button", { name: /default/i }).first().click();
    await page.getByRole("button", { name: "Open bench", exact: true }).click();

    // Wait for the workspace shell to replace the picker.
    await page.waitForFunction(
      () => !document.body.textContent.includes("Choose where to work"),
      { timeout: 15_000 },
    );
    await page.waitForTimeout(flags.settle ?? 800);
    await page.screenshot({ path: out, fullPage: true });

    const landmarks = await page.evaluate(() => ({
      url: location.href,
      headings: [...document.querySelectorAll("h1, h2, h3")].map((h) => h.textContent.trim()).filter(Boolean).slice(0, 10),
      buttons: [...document.querySelectorAll("button")].map((b) => b.textContent.trim()).filter(Boolean).slice(0, 16),
    }));
    console.log(`flow screenshot -> ${out}`);
    console.log(JSON.stringify({ ...landmarks, consoleErrors }, null, 2));
  });
}

const [command, ...rest] = process.argv.slice(2);
const flags = parseFlags(rest);

const commands = { screenshot, probe, flow };
const handler = commands[command];

if (!handler) {
  console.error(
    "Usage: node driver.mjs <screenshot|probe|flow> <url> [options]\n" +
      "  screenshot <url> [--route PATH] [--out FILE] [--wait SEL] [--full-page] [--settle MS]\n" +
      "  probe <url> [--route PATH]\n" +
      "  flow <url> [--out FILE]",
  );
  process.exit(2);
}

handler(flags).catch((error) => {
  console.error(`error: ${error.message}`);
  process.exit(1);
});
