// The launcher, gateway, memory assembly, provider transport and browser are live.
import { spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import { access, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import { makeScratchDir } from "./scratch-root.mjs";

const frontendDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");

async function unusedPort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const port = (server.address() as { port: number }).port;
  await new Promise<void>((done) => server.close(() => done()));
  return port;
}

async function stop(child: ChildProcess, exit: Promise<number | null>): Promise<void> {
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  await exit;
}

for (const outcome of ["success", "provider failure", "gateway crash"] as const) {
  test(`browser stack confines memory and cleans up after ${outcome}`, async ({ browser }) => {
    const invocation = await makeScratchDir("swage-memory-isolation-");
    const state = join(invocation, "state");
    const gatewayPort = await unusedPort();
    const providerPort = await unusedPort();
    const baseURL = `http://127.0.0.1:${gatewayPort}`;
    const child = spawn(process.execPath, [join(frontendDir, "e2e/run-stack.mjs")], {
      cwd: frontendDir,
      stdio: ["ignore", "pipe", "pipe"],
      env: {
        ...process.env,
        SWAGE_E2E_INVOCATION_DIR: invocation,
        SWAGE_E2E_GATEWAY_PORT: String(gatewayPort),
        SWAGE_E2E_PROVIDER_PORT: String(providerPort),
      },
    });
    let output = "";
    child.stdout!.on("data", (chunk) => { output += chunk; });
    child.stderr!.on("data", (chunk) => { output += chunk; });
    const exit = new Promise<number | null>((done) => child.once("exit", done));
    const page = await browser.newPage();
    try {
      await expect.poll(async () => {
        if (child.exitCode !== null) throw new Error(output);
        try { return (await fetch(`${baseURL}/api/health`)).ok; }
        catch { return false; }
      }).toBe(true);
      const memory = join(state, "general/memory/profile.md");
      await mkdir(dirname(memory), { recursive: true });
      const marker = `memory belongs to ${invocation}`;
      await writeFile(memory, `summary: Isolated profile\n\n${marker}\n`);
      await page.goto(baseURL);
      await page.getByRole("region", { name: "Start a session" })
        .getByRole("button", { name: "Start", exact: true }).click();
      const prompt = outcome === "provider failure" ? "controlled provider failure" : "read my isolated profile";
      await page.getByRole("textbox", { name: "Message" }).fill(prompt);
      await page.getByRole("button", { name: "Send" }).click();
      if (outcome === "provider failure") {
        await expect(page.getByRole("alert")).toContainText("Controlled provider failure");
      } else {
        await expect(page.getByLabel("Assistant message")).toContainText("response reached the browser.");
      }
      const requests = await readFile(join(invocation, "provider-requests.jsonl"), "utf8");
      expect(requests).toContain(marker);
      expect(await readFile(memory, "utf8")).toContain(marker);
      if (outcome === "gateway crash") {
        const metadata = JSON.parse(await readFile(join(state, "run/metadata.json"), "utf8")) as { pid: number };
        process.kill(metadata.pid, "SIGKILL");
        expect(await exit).not.toBe(0);
      }
    } finally {
      await page.close();
      await stop(child, exit);
      try {
        await expect(access(state)).rejects.toThrow();
      } finally {
        await rm(invocation, { recursive: true, force: true });
      }
    }
  });
}
