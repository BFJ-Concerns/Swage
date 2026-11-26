import { rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { makeScratchDir } from "./scratch-root.mjs";

const playwrightCli = resolve("node_modules/@playwright/test/cli.js");
const invocationDir = await makeScratchDir("swage-browser-e2e-");
// Hold both listeners until allocation finishes so the ports are distinct.
const listeners = [createServer(), createServer()];
await Promise.all(listeners.map((server) => new Promise((done, reject) => {
  server.once("error", reject);
  server.listen(0, "127.0.0.1", done);
})));
const [gatewayPort, providerPort] = listeners.map((server) => server.address().port);
await Promise.all(listeners.map((server) => new Promise((done) => server.close(done))));
const child = spawn(process.execPath, [playwrightCli, "test", ...process.argv.slice(2)], {
  stdio: "inherit",
  env: {
    ...process.env,
    SWAGE_E2E_INVOCATION_DIR: invocationDir,
    SWAGE_E2E_RESULTS_DIR: join(invocationDir, "playwright-results"),
    SWAGE_E2E_GATEWAY_PORT: process.env.SWAGE_E2E_GATEWAY_PORT ?? String(gatewayPort),
    SWAGE_E2E_PROVIDER_PORT: process.env.SWAGE_E2E_PROVIDER_PORT ?? String(providerPort),
  },
});

const code = await new Promise((resolveExit) => child.once("exit", resolveExit));
await rm(invocationDir, { recursive: true, force: true });
process.exit(code ?? 1);
