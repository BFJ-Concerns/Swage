import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const frontendDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoDir = resolve(frontendDir, "..");
const scratchParent = "/tmp/scratch";
const resultsDir = join(scratchParent, "swage-browser-e2e-results");
await mkdir(scratchParent, { recursive: true });
const swageRoot = await mkdtemp(join(scratchParent, "swage-browser-e2e-"));

await writeFile(join(swageRoot, "config.toml"), `
[server]
host = "127.0.0.1"
port = 33100

[providers]
default = "e2e"

[providers.entries.e2e]
protocol = "anthropic"
api_key_env = "SWAGE_E2E_PROVIDER_KEY"
base_url = "http://127.0.0.1:33101"

[models]
default = "e2e-model"

[models.entries.e2e-model]
provider_id = "e2e"
model_id = "e2e-model"

[sandbox]
enabled = false
allow_degraded_execution = true

[tools]
allow = ["*"]
deny = []
require_approval = ["shell"]
`);

const children = [];
function start(command, args, options = {}) {
  const child = spawn(command, args, { stdio: "inherit", ...options });
  children.push(child);
  child.once("exit", (code, signal) => {
    if (!stopping && (code !== 0 || signal)) {
      console.error(`${command} exited before the E2E suite (${signal ?? code})`);
      void stop(code ?? 1);
    }
  });
  return child;
}

let stopping = false;
async function stop(code = 0) {
  if (stopping) return;
  stopping = true;
  for (const child of children) child.kill("SIGTERM");
  await Promise.all(children.map((child) => child.exitCode !== null
    ? Promise.resolve()
    : new Promise((done) => child.once("exit", done))));
  await rm(swageRoot, { recursive: true, force: true });
  await rm(resultsDir, { recursive: true, force: true });
  process.exit(code);
}

process.on("SIGTERM", () => void stop());
process.on("SIGINT", () => void stop(130));
process.on("exit", () => {
  for (const child of children) child.kill("SIGTERM");
  rmSync(swageRoot, { recursive: true, force: true });
  rmSync(resultsDir, { recursive: true, force: true });
});

start(process.execPath, [join(frontendDir, "e2e/mock-provider.mjs")]);
start(join(repoDir, "target/debug/swage"), [
  "--server-mode", "gateway", "start", "--host", "127.0.0.1", "--port", "33100",
], {
  cwd: repoDir,
  env: {
    ...process.env,
    SWAGE_HOME: swageRoot,
    SWAGE_E2E_PROVIDER_KEY: "test-only",
    SWAGE_MANAGED_SECRET_KEY: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
  },
});

await new Promise(() => {});
