// The provider is controlled; Chromium, the rendered Svelte app, HTTP/WebSocket
// transport, gateway turn loop, storage, cancellation and approvals are live.
import { createServer } from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { spawn, type ChildProcess } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";

const frontendDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");

async function unusedPort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolveListen) => server.listen(0, "127.0.0.1", resolveListen));
  const port = (server.address() as { port: number }).port;
  await new Promise<void>((resolveClose) => server.close(() => resolveClose()));
  return port;
}

async function waitForHealth(baseUrl: string): Promise<void> {
  await expect.poll(async () => {
    try {
      return (await fetch(`${baseUrl}/api/health`)).ok;
    } catch {
      return false;
    }
  }, { timeout: 30_000 }).toBe(true);
}

async function startIsolatedStack(): Promise<{ baseUrl: string; child: ChildProcess; root: string }> {
  const root = await mkdtemp("/tmp/scratch/swage-browser-e2e-isolation-");
  const gatewayPort = await unusedPort();
  const providerPort = await unusedPort();
  const child = spawn(process.execPath, [join(frontendDir, "e2e/run-stack.mjs")], {
    cwd: frontendDir,
    stdio: "pipe",
    env: {
      ...process.env,
      SWAGE_E2E_INVOCATION_DIR: root,
      SWAGE_E2E_GATEWAY_PORT: String(gatewayPort),
      SWAGE_E2E_PROVIDER_PORT: String(providerPort),
    },
  });
  const baseUrl = `http://127.0.0.1:${gatewayPort}`;
  await waitForHealth(baseUrl);
  return { baseUrl, child, root };
}

async function stopStack(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM");
  await new Promise<void>((resolveExit) => child.once("exit", () => resolveExit()));
}

async function openComposer(page: Page): Promise<void> {
  const openSpace = page.getByRole("button", { name: "Open" });
  if (await openSpace.isVisible()) await openSpace.click();
  await expect(page.getByRole("textbox", { name: "Message" })).toBeVisible();
}

async function send(page: Page, prompt: string): Promise<void> {
  await page.goto("/");
  const openSpace = page.getByRole("button", { name: "Open" });
  if (await openSpace.isVisible()) await openSpace.click();
  const composer = page.getByRole("textbox", { name: "Message" });
  await expect(composer).toBeEnabled();
  await composer.fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByLabel("Your message").getByText(prompt, { exact: true })).toBeVisible();
}

test("renders an early streamed response before the delayed remainder", async ({ page }) => {
  await send(page, "stream this turn");
  const assistant = page.getByLabel("Assistant message");
  await expect(assistant).toContainText("Streamed ");
  await expect(assistant).not.toContainText("response reached the browser.");
  await expect(assistant).toContainText("Streamed response reached the browser.");
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
});

test("cancels an active turn from the rendered conversation", async ({ page }) => {
  await send(page, "cancel this turn");
  await expect(page.getByLabel("Assistant message")).toContainText(
    "Partial response before cancellation.",
  );
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Response interrupted")).toBeVisible();
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await expect(page.getByLabel("Assistant message")).not.toContainText(
    "This tail must never render.",
  );
});

test("keeps concurrent browser-suite stacks isolated during teardown", async ({ browser }) => {
  const first = await startIsolatedStack();
  const second = await startIsolatedStack();
  const firstPage = await browser.newPage();
  const secondPage = await browser.newPage();

  try {
    await firstPage.goto(first.baseUrl);
    await secondPage.goto(second.baseUrl);
    await openComposer(firstPage);
    await openComposer(secondPage);

    await stopStack(first.child);
    await waitForHealth(second.baseUrl);
    await secondPage.reload();
    await openComposer(secondPage);
  } finally {
    await stopStack(first.child);
    await stopStack(second.child);
    await firstPage.close();
    await secondPage.close();
    await rm(first.root, { recursive: true, force: true });
    await rm(second.root, { recursive: true, force: true });
  }
});

test("reconnects mid-stream and restores the completed conversation", async ({ page }) => {
  await page.addInitScript(() => {
    const NativeWebSocket = window.WebSocket;
    const sockets: WebSocket[] = [];
    Object.defineProperty(window, "__e2eSockets", { value: sockets });
    window.WebSocket = class extends NativeWebSocket {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        sockets.push(this);
      }
    };
  });
  await send(page, "reconnect this turn");
  await expect(page.getByLabel("Assistant message")).toContainText(
    "First half before disconnect.",
  );
  await page.evaluate(() => {
    const sockets = (window as typeof window & { __e2eSockets: WebSocket[] }).__e2eSockets;
    sockets.at(-1)?.close(4000, "E2E connection interruption");
  });
  await expect(page.getByText("Reconnecting")).toBeVisible();
  await expect(page.getByText("Reconnecting")).not.toBeVisible();
  await expect(page.getByText(
    "First half before disconnect. Second half after reconnect.",
    { exact: true },
  )).toBeVisible();
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
});

test("approves a pending tool call through the rendered UI", async ({ page }) => {
  await send(page, "approve a tool");
  const approval = page.getByRole("dialog", { name: "Approval required" });
  await expect(approval).toContainText("shell");
  await approval.getByRole("button", { name: "Allow once" }).click();
  await expect(page.getByLabel("Assistant message")).toContainText(
    "Approval completed through the gateway.",
  );
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
});
