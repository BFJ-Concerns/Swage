// The provider is controlled; Chromium, the rendered Svelte app, HTTP/WebSocket
// transport, gateway turn loop, storage, cancellation and approvals are live.
import { createServer } from "node:net";
import { access, readFile, rm } from "node:fs/promises";
import { spawn, type ChildProcess } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import { makeScratchDir } from "./scratch-root.mjs";

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
  const root = await makeScratchDir("swage-browser-e2e-isolation-");
  let child: ChildProcess | undefined;
  try {
    const gatewayPort = await unusedPort();
    const providerPort = await unusedPort();
    child = spawn(process.execPath, [join(frontendDir, "e2e/run-stack.mjs")], {
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
  } catch (error) {
    if (child) await stopStack(child);
    await rm(root, { recursive: true, force: true });
    throw error;
  }
}

async function stopStack(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM");
  await new Promise<void>((resolveExit) => child.once("exit", () => resolveExit()));
}

async function listConcurrentSuite(): Promise<void> {
  const child = spawn(process.execPath, [join(frontendDir, "e2e/run-suite.mjs"), "--list"], {
    cwd: frontendDir,
    stdio: "pipe",
    env: {
      ...process.env,
      SWAGE_E2E_INVOCATION_DIR: undefined,
      SWAGE_E2E_RESULTS_DIR: undefined,
    },
  });
  let output = "";
  child.stdout?.on("data", (chunk) => { output += chunk; });
  child.stderr?.on("data", (chunk) => { output += chunk; });
  const code = await new Promise<number | null>((resolveExit) => child.once("exit", resolveExit));
  if (code !== 0) throw new Error(`Concurrent suite listing failed (${code}):\n${output}`);
}

/**
 * From the front door, start a session in the general scope: the composer
 * opens and the first message creates the conversation. A page already in
 * a session (the composer visible) is left where it is.
 */
async function openComposer(page: Page): Promise<void> {
  const composer = page.getByRole("textbox", { name: "Message" });
  if (!(await composer.isVisible())) {
    await page
      .getByRole("region", { name: "Start a session" })
      .getByRole("button", { name: "Start", exact: true })
      .click();
  }
  await expect(composer).toBeVisible();
}

async function send(page: Page, prompt: string): Promise<void> {
  await page.goto("/");
  await openComposer(page);
  const composer = page.getByRole("textbox", { name: "Message" });
  await expect(composer).toBeEnabled();
  await composer.fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByLabel("Your message").getByText(prompt, { exact: true })).toBeVisible();
}

async function providerRequests(): Promise<Array<{ prompt: string | null; effort: string | null }>> {
  const root = process.env.SWAGE_E2E_INVOCATION_DIR;
  if (!root) throw new Error("The E2E invocation directory was not provided");
  try {
    return (await readFile(join(root, "provider-requests.jsonl"), "utf8"))
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line) as { prompt: string | null; effort: string | null });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  }
}

async function sendNewConversation(page: Page, prompt: string, effort?: "high"): Promise<void> {
  await expect(page.getByRole("textbox", { name: "Message" })).toBeVisible();
  if (effort) await page.getByRole("combobox", { name: "Effort" }).selectOption(effort);
  await page.getByRole("textbox", { name: "Message" }).fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByLabel("Your message").getByText(prompt, { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
}

const effortJourneyPrompts = [
  "first high effort conversation",
  "second high effort conversation",
  "default effort after switch",
] as const;

test("resolves light-theme tokens before runtime theme application", async ({ page }) => {
  await page.addInitScript(() => {
    const add = DOMTokenList.prototype.add;
    DOMTokenList.prototype.add = function (...tokens: string[]) {
      return add.apply(this, tokens.filter((token) => !token.startsWith("theme-")));
    };
  });

  await page.goto("/");
  await expect(page.locator("body")).not.toHaveClass(/theme-(riso|cyanotype)/);
  await expect.poll(() => page.evaluate(() => {
    const styles = getComputedStyle(document.body);
    return {
      ink: styles.getPropertyValue("--ink").trim(),
      paperDeep: styles.getPropertyValue("--paper-deep").trim(),
    };
  })).toEqual({ ink: "#0e3b66", paperDeep: "#ececea" });
});

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
  await page.waitForTimeout(8_100);
  await expect(page.locator("body")).not.toContainText(
    "This tail must never render.",
  );
  await page.reload();
  await expect(page.getByText("Response interrupted")).toBeVisible();
  await expect(page.locator("body")).not.toContainText(
    "This tail must never render.",
  );
});

test("keeps concurrent browser-suite stacks isolated during teardown", async ({ browser }) => {
  let first: Awaited<ReturnType<typeof startIsolatedStack>> | undefined;
  let second: Awaited<ReturnType<typeof startIsolatedStack>> | undefined;
  let firstPage: Page | undefined;
  let secondPage: Page | undefined;

  try {
    first = await startIsolatedStack();
    second = await startIsolatedStack();
    firstPage = await browser.newPage();
    secondPage = await browser.newPage();

    await firstPage.goto(first.baseUrl);
    await secondPage.goto(second.baseUrl);
    await openComposer(firstPage);
    await openComposer(secondPage);
    await access(join(first.root, "state"));
    await access(join(second.root, "state"));

    await listConcurrentSuite();
    await access(join(first.root, "state"));
    await access(join(second.root, "state"));

    await stopStack(first.child);
    await waitForHealth(second.baseUrl);
    await access(join(second.root, "state"));
    await secondPage.reload();
    await openComposer(secondPage);
  } finally {
    if (first) await stopStack(first.child);
    if (second) await stopStack(second.child);
    if (firstPage) await firstPage.close();
    if (secondPage) await secondPage.close();
    if (first) await rm(first.root, { recursive: true, force: true });
    if (second) await rm(second.root, { recursive: true, force: true });
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

test("clears effort after switching conversations before the following new-conversation send", async ({ page }) => {
  await page.goto("/");
  await openComposer(page);
  await sendNewConversation(page, effortJourneyPrompts[0], "high");
  await page.getByRole("button", { name: "+ New conversation" }).click();
  await sendNewConversation(page, effortJourneyPrompts[1], "high");

  // Newly created conversations are prepended, so the prior one is index one
  // even when earlier suite journeys already populated the shared gateway.
  await page.locator(".conversation-select-btn").nth(1).click();
  await expect(page.getByRole("textbox", { name: "Message" })).toBeVisible();
  await page.getByRole("button", { name: "+ New conversation" }).click();
  await expect(page.getByRole("combobox", { name: "Effort" })).toHaveValue("");
  await sendNewConversation(page, effortJourneyPrompts[2]);

  const effortRequests = async () => (await providerRequests()).filter(
    (request) => typeof request.prompt === "string"
      && effortJourneyPrompts.includes(request.prompt as typeof effortJourneyPrompts[number]),
  );
  await expect.poll(effortRequests).toEqual([
    expect.objectContaining({ prompt: effortJourneyPrompts[0], effort: "high" }),
    expect.objectContaining({ prompt: effortJourneyPrompts[1], effort: "high" }),
    expect.objectContaining({ prompt: effortJourneyPrompts[2], effort: null }),
  ]);
});

test("renders the dark-theme status badge and controlled provider error", async ({ page }) => {
  await page.goto("/");
  await openComposer(page);
  await page.getByRole("button", { name: "Dark theme" }).click();
  await expect(page.locator("body")).toHaveClass(/theme-cyanotype/);

  await page.getByRole("button", { name: "Operator" }).click();
  const connectedBadge = page.locator(".badge", { hasText: "connected" });
  await expect(connectedBadge).toBeVisible();
  await expect(connectedBadge).toHaveCSS("background-color", "rgb(255, 248, 238)");

  await page.getByRole("button", { name: "Conversation", exact: true }).click();
  await send(page, "controlled provider failure");
  const error = page.getByRole("alert");
  await expect(error).toContainText("Controlled provider failure");
  await expect(error).toHaveCSS("border-left-color", "rgb(248, 93, 120)");
});

test("separates the cells of a multi-column table in a surface document module", async ({ page }, testInfo) => {
  await send(page, "plan the volunteer rota");
  await page.getByRole("navigation", { name: "Surface" }).getByRole("button", { name: "Surfaces" }).click();
  await page.getByPlaceholder("New surface title").fill("Planning");
  await page.getByPlaceholder("New surface title").press("Enter");
  await page.getByRole("button", { name: "Add module" }).click();
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await page.locator("article.module textarea").fill(
    "| Priya | Tom | Ellis |\n| --- | --- | --- |\n| 280 | Confirmed | Likely |",
  );
  await page.getByRole("button", { name: "Save", exact: true }).click();

  const table = page.locator("article.module .module-content table");
  await expect(table).toBeVisible();
  await table.screenshot({ path: testInfo.outputPath("module-table.png") });
  for (const row of [table.locator("th"), table.locator("tbody td")]) {
    const cells = await row.evaluateAll((elements) =>
      elements.map((element) => {
        const text = document.createRange();
        text.selectNodeContents(element);
        const glyphs = text.getBoundingClientRect();
        return { left: glyphs.left, right: glyphs.right };
      }),
    );
    expect(cells).toHaveLength(3);
    for (let index = 1; index < cells.length; index += 1) {
      // The gap between one cell's text and the next is visible space, not touching glyphs.
      expect(cells[index].left - cells[index - 1].right).toBeGreaterThanOrEqual(8);
    }
  }
});


test("keeps a long topic heading compact and all workspace controls visible", async ({ page }) => {
  const topic = {
    id: "topic-layout", slug: "layout", description: "", created_at: "2026-10-15T00:00:00Z",
    name: "Marsh Atlas regional observations and comparative field research across coastal habitats",
  };
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.route((url) => url.pathname.startsWith("/api/"), async (route) => {
    const path = new URL(route.request().url()).pathname;
    let body: unknown = {};
    if (path === "/api/settings") body = { models: {}, default_model: "" };
    else if (path === "/api/topics") body = { topics: [topic] };
    else if (path === `/api/topics/${topic.id}`) body = topic;
    else if (path.endsWith("/workspaces")) body = { workspaces: [1, 2, 3].map((id) => ({
      id: `workspace-${id}`, topic_id: topic.id, label: `Folder ${id}`, path: `/work/folder-${id}`,
      canonical_path: `/work/folder-${id}`, is_missing: false, is_default: id === 1,
      is_trusted: true, permissions: ["read"], created_at: topic.created_at,
    })) };
    else if (path.endsWith("/sessions") || path === "/api/conversations") body = { conversations: [] };
    await route.fulfill({ json: body });
  });
  await page.goto("/settings/topic/layout/workspaces");
  const heading = page.getByRole("heading", { name: topic.name.trim(), exact: true });
  await expect(heading).toBeVisible();
  const bounds = await heading.boundingBox();
  expect(bounds!.height).toBeLessThanOrEqual(80);
  await expect(page.getByRole("button", { name: "Attach workspace", exact: true })).toBeInViewport({ ratio: 1 });
  const rows = page.locator(".record-row");
  await expect(rows).toHaveCount(3);
  for (const row of await rows.all()) {
    await expect(row.getByText(/trusted · read/)).toBeInViewport({ ratio: 1 });
    await expect(row.getByRole("button", { name: "Revoke trust" })).toBeInViewport({ ratio: 1 });
    await expect(row.getByRole("button", { name: "Detach", exact: true })).toBeInViewport({ ratio: 1 });
  }
});
