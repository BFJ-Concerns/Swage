import { readdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawn } from "node:child_process";

const scratchParent = "/tmp/scratch";
const playwrightCli = resolve("node_modules/@playwright/test/cli.js");
const child = spawn(process.execPath, [playwrightCli, "test", ...process.argv.slice(2)], {
  stdio: "inherit",
});

const code = await new Promise((resolveExit) => child.once("exit", resolveExit));
for (const entry of readdirSync(scratchParent)) {
  if (entry.startsWith("swage-browser-e2e-")) {
    rmSync(join(scratchParent, entry), { recursive: true, force: true });
  }
}
process.exit(code ?? 1);
