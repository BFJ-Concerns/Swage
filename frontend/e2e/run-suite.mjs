import { mkdtemp, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawn } from "node:child_process";

const playwrightCli = resolve("node_modules/@playwright/test/cli.js");
const invocationDir = await mkdtemp("/tmp/scratch/swage-browser-e2e-");
const child = spawn(process.execPath, [playwrightCli, "test", ...process.argv.slice(2)], {
  stdio: "inherit",
  env: {
    ...process.env,
    SWAGE_E2E_INVOCATION_DIR: invocationDir,
    SWAGE_E2E_RESULTS_DIR: join(invocationDir, "playwright-results"),
  },
});

const code = await new Promise((resolveExit) => child.once("exit", resolveExit));
await rm(invocationDir, { recursive: true, force: true });
process.exit(code ?? 1);
