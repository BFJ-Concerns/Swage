import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: "line",
  outputDir: "/tmp/scratch/swage-browser-e2e-results",
  use: {
    baseURL: "http://127.0.0.1:33100",
    browserName: "chromium",
    headless: true,
    trace: "retain-on-failure",
  },
  webServer: {
    command: "node e2e/run-stack.mjs",
    url: "http://127.0.0.1:33100/api/health",
    timeout: 120_000,
    reuseExistingServer: false,
  },
});
