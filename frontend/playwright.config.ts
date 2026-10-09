import { join } from "node:path";
import { defineConfig } from "@playwright/test";
import { scratchParent } from "./e2e/scratch-root.mjs";

const gatewayPort = Number(process.env.SWAGE_E2E_GATEWAY_PORT ?? "33100");
const resultsDir = process.env.SWAGE_E2E_RESULTS_DIR
  ?? join(scratchParent, "swage-browser-e2e-results");

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: "line",
  outputDir: resultsDir,
  use: {
    baseURL: `http://127.0.0.1:${gatewayPort}`,
    browserName: "chromium",
    headless: true,
    trace: "retain-on-failure",
  },
  webServer: {
    command: "node e2e/run-stack.mjs",
    url: `http://127.0.0.1:${gatewayPort}/api/health`,
    timeout: 120_000,
    reuseExistingServer: false,
  },
});
