// The provider is controlled; Chromium, the rendered Svelte app, HTTP/WebSocket
// transport, gateway turn loop, storage, cancellation and approvals are live.
import { access, readFile, rm } from "node:fs/promises";
import { spawn, type ChildProcess } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";
import { makeScratchDir } from "./scratch-root.mjs";
import { unusedPorts } from "./unused-ports.mjs";

const frontendDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");

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
    const [gatewayPort, providerPort] = await unusedPorts(2);
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

async function providerRequests(): Promise<Array<{ prompt: unknown; effort: string | null; model: string }>> {
  const root = process.env.SWAGE_E2E_INVOCATION_DIR;
  if (!root) throw new Error("The E2E invocation directory was not provided");
  try {
    return (await readFile(join(root, "provider-requests.jsonl"), "utf8"))
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line) as { prompt: unknown; effort: string | null; model: string });
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

test("colours code tokens and follows message links with native fragment behaviour", async ({ page }) => {
  await page.goto("/");
  await openComposer(page);
  await sendNewConversation(page, "render code and links");

  const code = page.locator("pre code").filter({ hasText: 'const answer = "green";' });
  const tokenColours = () => code.evaluate((block) => [
    getComputedStyle(block).color,
    ...Array.from(block.querySelectorAll("span"), (span) => getComputedStyle(span).color),
  ]);
  for (const theme of ["Light", "Dark"]) {
    await page.getByRole("button", { name: `${theme} theme` }).click();
    const colours = await tokenColours();
    expect(new Set(colours).size).toBeGreaterThanOrEqual(3);
  }

  await page.getByRole("link", { name: "Here", exact: true }).click();
  await expect(page.locator(":target")).toHaveText("Anchor text");
  await page.getByRole("link", { name: "Top", exact: true }).click();
  await expect(page).toHaveURL(/#$/);
  await expect(page.locator(":target")).toHaveCount(0);

  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await expect(page).toHaveURL(/\/settings\/display$/);
  await page.goBack();
  await page.getByRole("link", { name: "Settings fragment", exact: true }).click();
  await expect(page).toHaveURL(/\/settings\/display#test-anchor$/);
});

test("refreshes open agent pickers and uses the preferred model on the first turn", async ({ page, context }) => {
  const pickerPage = await context.newPage();
  await pickerPage.goto("/");
  const frontAgent = pickerPage.getByRole("combobox", { name: "Agent" });
  await expect(frontAgent).toBeVisible();
  await page.goto("/");
  await openComposer(page);
  await sendNewConversation(page, "Create a preferred agent");
  await expect(page.getByLabel("Assistant message").getByText("Definition tool returned to the provider.")).toBeVisible();
  await expect(page.getByLabel("Session agent")).toBeVisible();
  await expect(frontAgent.getByRole("option", { name: "Preferred agent" })).toHaveCount(1);

  await sendNewConversation(page, "Set an unknown preferred model");
  await expect.poll(async () => {
    const requests = await providerRequests();
    return requests.flatMap(({ prompt }) => Array.isArray(prompt) ? prompt : [])
      .find((part) => part.type === "tool_result" && part.tool_use_id === "refuse-e2e-agent");
  }).toMatchObject({
    is_error: true,
    content: expect.stringMatching(/unknown-model.*not in the configured catalogue.*preferred-model/),
  });

  async function sendWithAgent(prompt: string, override?: string) {
    await pickerPage.goto("/");
    await frontAgent.selectOption("preferred-agent");
    await openComposer(pickerPage);
    await expect(frontAgent).toHaveValue("preferred-agent");
    const model = pickerPage.getByRole("combobox", { name: "Model" });
    await expect(model).toHaveValue("preferred-model");
    if (override) await model.selectOption(override);
    await sendNewConversation(pickerPage, prompt);
    await expect.poll(async () => {
      const requests = await providerRequests();
      return requests.find((request) => request.prompt === prompt)?.model;
    }).toBe(override === "e2e-model" ? "claude-opus-4-6" : "preferred-model");
  }

  await sendWithAgent("First turn on the agent preference");
  await sendWithAgent("First turn on the explicit model", "e2e-model");
});

const effortJourneyPrompts = [
  "first high effort conversation",
  "second high effort conversation",
  "default effort after switch",
] as const;

test("hides the old conversation while opening a destination and ignores an overtaken load", async ({ page }) => {
  const prompts = ["Which route is current?", "What is the slower route?", "Where did this visit start?"];
  const paths: string[] = [];
  for (const prompt of prompts) {
    await send(page, prompt);
    await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible();
    paths.push(new URL(page.url()).pathname);
  }

  let releaseSlowRead!: () => void;
  let markSlowReadStarted!: () => void;
  const slowRead = new Promise<void>((resolve) => { releaseSlowRead = resolve; });
  const slowReadStarted = new Promise<void>((resolve) => { markSlowReadStarted = resolve; });
  const slowEndpoint = `/api${paths[1]}`;
  // Use the session switcher to make consecutive history entries in the SPA.
  const switcher = page.getByRole("combobox", { name: "Active conversation" });
  // The setup navigated between documents. First select the two destinations
  // in this document, then exercise Back twice through those entries.
  await switcher.selectOption(paths[0].split("/").at(-1)!);
  await expect(page.getByLabel("Your message").getByText(prompts[0], { exact: true })).toBeVisible();
  await switcher.selectOption(paths[1].split("/").at(-1)!);
  await expect(page.getByLabel("Your message").getByText(prompts[1], { exact: true })).toBeVisible();
  await switcher.selectOption(paths[2].split("/").at(-1)!);
  await expect(page.getByLabel("Your message").getByText(prompts[2], { exact: true })).toBeVisible();
  await page.route((url) => url.pathname === slowEndpoint, async (route) => {
    const response = await route.fetch();
    markSlowReadStarted();
    await slowRead;
    await route.fulfill({ response });
  });

  try {
    await page.goBack();
    await slowReadStarted;
    await expect(page.getByText("Opening conversation…")).toBeVisible();
    await expect(page.getByRole("textbox", { name: "Message" })).toBeHidden();
    await expect(page.getByLabel("Your message")).toHaveCount(0);

    await page.goBack();
    await expect(page.getByLabel("Your message").getByText(prompts[0], { exact: true })).toBeVisible();
    const response = page.waitForResponse((result) => new URL(result.url()).pathname === slowEndpoint);
    releaseSlowRead();
    await response;
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await expect(page).toHaveURL(paths[0]);
    await expect(page.getByLabel("Your message").getByText(prompts[0], { exact: true })).toBeVisible();
  } finally {
    releaseSlowRead();
  }
});

test("keeps the first question as the label and follows a send from earlier in the transcript", async ({ page }) => {
  const opening = "Where should we start?";
  await page.setViewportSize({ width: 900, height: 660 });
  await send(page, opening);
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible();
  for (let index = 0; index < 3; index++) {
    await sendNewConversation(page, `Follow-up ${index}. ${"Further reading. ".repeat(90).trim()}`);
  }
  await page.reload();
  const channel = page.getByRole("region", { name: "Conversation" });
  const messages = channel.getByRole("region", { name: "Conversation messages" });
  await expect(channel.getByRole("combobox", { name: "Active conversation" }).locator("option:checked")).toHaveText(opening);
  await expect(page.getByRole("complementary").getByRole("button", { name: /^Where should we start/ })).toBeVisible();
  await expect.poll(() => messages.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThan(50);
  await messages.hover();
  await page.mouse.wheel(0, -1600);
  await expect.poll(() => messages.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeGreaterThan(100);
  await sendNewConversation(page, "Follow this send");
  await expect.poll(() => messages.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThan(50);

  await page.setViewportSize({ width: 390, height: 760 });
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("approve a tool");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const approval = page.getByRole("dialog", { name: "Approval required" });
  await expect(approval.getByRole("button", { name: "Allow once", exact: true })).toBeInViewport();
  await expect(messages.getByText("Waiting for your approval", { exact: true })).toHaveAttribute("aria-live", "polite");
  await page.getByRole("button", { name: "Surfaces", exact: true }).click();
  await expect(approval.getByRole("button", { name: "Allow once", exact: true })).toBeInViewport();
  await approval.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(page.getByLabel("Assistant message").getByText("Approval completed through the gateway.")).toBeVisible();
});

test("keeps model discovery results for the latest query", async ({ page }) => {
  await page.goto("/settings/providers");
  let releaseOld!: () => void;
  let markOldStarted!: () => void;
  const oldQuery = new Promise<void>((resolve) => { releaseOld = resolve; });
  const oldStarted = new Promise<void>((resolve) => { markOldStarted = resolve; });
  await page.route(/\/api\/settings\/providers\/[^/]+\/models/, async (route) => {
    const query = new URL(route.request().url()).searchParams.get("q");
    if (query === "earlier") {
      markOldStarted();
      await oldQuery;
    }
    await route.fulfill({ json: { models: query ? [{ id: query, name: `${query} result` }] : [] } });
  });

  try {
    const search = page.getByPlaceholder("Search models");
    await search.fill("earlier");
    await oldStarted;
    await search.fill("current");
    await expect(page.getByRole("button", { name: /current result/ })).toBeVisible();
    const response = page.waitForResponse((result) => new URL(result.url()).searchParams.get("q") === "earlier");
    releaseOld();
    await response;
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await expect(page.getByRole("button", { name: /current result/ })).toBeVisible();
  } finally {
    releaseOld();
  }
});

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
  await expect(page.getByText("Response cancelled")).toBeVisible();
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await page.waitForTimeout(8_100);
  await expect(page.locator("body")).not.toContainText(
    "This tail must never render.",
  );
  await page.reload();
  await expect(page.getByText("Response cancelled")).toBeVisible();
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
  await expect(page.getByLabel("Assistant message")).toContainText(
    "First half before disconnect. Second half after reconnect.",
  );
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
});

test("keeps an accepted message sent across a disconnect before the first server event", async ({ page }) => {
  const prompt = "accepted before disconnect";
  let openingSocket: WebSocketRoute | undefined;
  await page.routeWebSocket(/\/conversations\/[^/]+\/ws$/, (socket) => {
    const gateway = socket.connectToServer();
    if (openingSocket) return;
    openingSocket = gateway;
    // The request reaches the gateway, but this connection loses its replies.
    gateway.onMessage(() => {});
  });
  await send(page, prompt);
  await expect.poll(async () => (await providerRequests()).some((request) => request.prompt === prompt)).toBe(true);
  await openingSocket!.close({ code: 4000 });
  await expect(page.getByText("Reconnecting")).toBeVisible();
  await expect(page.getByRole("button", { name: "Retry", exact: true })).toHaveCount(0);
  await expect(page.getByLabel("Assistant message")).toContainText("The accepted request finished.");
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible();
});

test("restores an unrecorded message and lets the person retry after reconnect", async ({ page }) => {
  const prompt = "never reaches the gateway";
  let dropped = false;
  await page.routeWebSocket(/\/conversations\/[^/]+\/ws$/, (socket) => {
    const gateway = socket.connectToServer();
    if (dropped) return;
    socket.onMessage((message) => {
      if (JSON.parse(String(message)).type === "send_message") {
        dropped = true;
        void socket.close({ code: 4000 });
      } else {
        gateway.send(message);
      }
    });
  });

  await send(page, prompt);
  await expect(page.getByText("Reconnecting")).toBeVisible();
  await expect(page.getByRole("alert")).toContainText("The gateway has no record of this message after reconnecting.");
  await expect(page.getByLabel("Your message")).toContainText(prompt);
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible();
  expect((await providerRequests()).filter((request) => request.prompt === prompt)).toHaveLength(0);

  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.getByLabel("Assistant message")).toContainText("Streamed response reached the browser.");
  await expect.poll(async () => (await providerRequests()).filter((request) => request.prompt === prompt)).toHaveLength(1);
});

test("stops generating when reconnect recovers an output-limited reply", async ({ page }) => {
  await page.routeWebSocket(/\/conversations\/[^/]+\/ws$/, (socket) => {
    const gateway = socket.connectToServer();
    gateway.onMessage((message) => {
      if (JSON.parse(String(message)).type === "turn_complete") {
        void socket.close({ code: 4000 });
      } else {
        socket.send(message);
      }
    });
  });
  await send(page, "limit this turn");
  await expect(page.getByText("Reconnecting")).toBeVisible();
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible();
  const reply = page.getByLabel("Assistant message");
  await expect(reply).toContainText("Partial response at the output limit.");
  await expect(reply).toContainText("Response incomplete");
});

for (const ending of ["reply", "failure"] as const) {
  test(`recovers a queued turn that settles with a ${ending} across a disconnect`, async ({ page }) => {
    let disconnected = false;
    await page.routeWebSocket(/\/conversations\/[^/]+\/ws$/, (socket) => {
      const gateway = socket.connectToServer();
      let runningQueuedTurn = false;
      gateway.onMessage((message) => {
        const event = JSON.parse(String(message));
        if (event.type === "queue_drained") runningQueuedTurn = true;
        if (runningQueuedTurn && !disconnected && (event.type === "turn_complete" || event.type === "error")) {
          disconnected = true;
          void socket.close({ code: 4000 });
        } else {
          socket.send(message);
        }
      });
    });
    await send(page, "hold for the batch");
    await expect(page.getByLabel("Assistant message")).toContainText("Holding the current request.");
    const input = ending === "reply" ? "What comes after this queued request?" : "failed batch second";
    const composer = page.getByRole("textbox", { name: "Message", exact: true });
    await composer.fill(input);
    await composer.press("Enter");
    await expect(page.getByLabel("Queued message").getByText(input, { exact: true })).toBeVisible();
    await expect(page.getByText("Reconnecting")).toBeVisible();
    await expect(page.getByLabel("Your message").getByText(input, { exact: true })).toBeVisible();
    if (ending === "reply") {
      await expect(page.getByLabel("Assistant message").getByText("Streamed response reached the browser.")).toBeVisible();
    } else {
      await expect(page.getByRole("button", { name: "Retry", exact: true })).toBeVisible();
    }
    await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeHidden();
  });
}

test("settles a reconnected turn while another conversation is open", async ({ page }) => {
  await send(page, "Which conversation stays in view?");
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible();
  const other = new URL(page.url()).pathname;
  let interruptedSocket: WebSocketRoute | undefined;
  await page.routeWebSocket(/\/conversations\/[^/]+\/ws$/, (socket) => {
    socket.connectToServer();
    interruptedSocket ??= socket;
  });
  await send(page, "finish an off-screen turn");
  const running = new URL(page.url()).pathname;
  await expect(page.getByLabel("Assistant message")).toContainText("The turn began.");
  const switcher = page.getByRole("combobox", { name: "Active conversation" });
  await switcher.selectOption(other.split("/").at(-1)!);
  await expect(page).toHaveURL(other);
  await expect(page.getByLabel("Your message").getByText("Which conversation stays in view?", { exact: true })).toBeVisible();
  await interruptedSocket!.close({ code: 4000 });
  // Wait on the gateway's completed transcript while the other session is in view.
  await expect.poll(async () => {
    const response = await page.request.get(`/api${running}`);
    const detail = await response.json();
    return detail.messages.some((message: { content: string }) => message.content.includes("The off-screen turn finished."));
  }).toBe(true);
  await switcher.selectOption(running.split("/").at(-1)!);
  await expect(page.getByLabel("Assistant message").getByText(/The off-screen turn finished/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeHidden();
});

test("clears a stopped approval and keeps the tool under its partial reply", async ({ page, context }) => {
  await send(page, "partial then approval");
  const approval = page.getByRole("dialog", { name: "Approval required" });
  await expect(approval).toBeVisible();

  const other = await context.newPage();
  try {
    await other.goto(page.url());
    await other.getByRole("button", { name: "Stop", exact: true }).click();
    await expect(approval).toBeHidden();
    const reply = page.getByLabel("Assistant message").filter({ hasText: "Partial response before approval." });
    await expect(reply).toContainText("Response cancelled");
    await expect(reply).toContainText("tool call was cancelled when its turn was stopped");
    await expect(page.getByRole("textbox", { name: "Message" })).toBeVisible();
    await page.reload();
    await expect(reply).toContainText("Response cancelled");
    await expect(reply).toContainText("tool call was cancelled when its turn was stopped");
  } finally {
    await other.close();
  }
});

test("clears the approval when reconnect recovers a stopped request with no reply", async ({ page, context }) => {
  let openingSocket: WebSocketRoute | undefined;
  await page.routeWebSocket(/\/conversations\/[^/]+\/ws$/, (socket) => {
    const gateway = socket.connectToServer();
    openingSocket ??= gateway;
  });
  await send(page, "approve a tool");
  const approval = page.getByRole("dialog", { name: "Approval required" });
  await expect(approval).toBeVisible();

  const other = await context.newPage();
  try {
    await other.goto(page.url());
    const stop = other.getByRole("button", { name: "Stop", exact: true });
    await expect(stop).toBeVisible();
    await openingSocket!.close({ code: 4000 });
    await stop.click();
    await expect(page.getByText("Cancelled before a reply")).toBeVisible();
    await expect(approval).toBeHidden();
  } finally {
    await other.close();
  }
});

test("offers one Retry when the provider fails before sending a reply", async ({ page }) => {
  await send(page, "controlled provider failure");
  await expect(page.getByRole("alert")).toContainText("Controlled provider failure");
  await expect(page.getByRole("button", { name: "Retry", exact: true })).toHaveCount(1);
});

test("offers one Retry on the recorded refusal account", async ({ page }) => {
  await send(page, "refuse this turn");
  await expect(page.getByRole("button", { name: "Retry request", exact: true })).toBeEnabled();
  await expect(page.getByRole("button", { name: /^Retry(?: request)?$/ })).toHaveCount(1);
});

test("retries the complete failed batch before and after reload", async ({ page }) => {
  await send(page, "hold for the batch");
  await expect(page.getByLabel("Assistant message")).toContainText("Holding the current request.");
  const composer = page.getByRole("textbox", { name: "Message" });
  const inputs = ["failed batch first", "failed batch second"];
  for (const input of inputs) {
    await composer.fill(input);
    await composer.press("Enter");
    await expect(page.getByLabel("Queued message").getByText(input, { exact: true })).toBeVisible();
  }

  const retry = page.getByRole("button", { name: "Retry", exact: true });
  await expect(retry).toHaveCount(1);
  await page.reload();
  await expect(retry).toHaveCount(1);
  await retry.click();
  await expect(page.getByLabel("Your message").last()).toHaveText(/failed batch first\s+failed batch second/);
});

test("reports the waited child's incomplete reply live and after reload", async ({ page }) => {
  await send(page, "wait for an incomplete child");
  await expect(page.getByText("Child response incomplete", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible();
  await page.reload();
  await expect(page.getByText("Child response incomplete", { exact: true })).toBeVisible();
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
  const firstId = new URL(page.url()).pathname.split("/").at(-1)!;
  await page.getByRole("button", { name: "+ New conversation" }).click();
  await sendNewConversation(page, effortJourneyPrompts[1], "high");

  await page.getByRole("combobox", { name: "Active conversation" }).selectOption(firstId);
  await expect(page.getByLabel("Your message").getByText(effortJourneyPrompts[0], { exact: true })).toBeVisible();
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
  await send(page, "Keep a session connected while viewing the fleet");
  await page.getByRole("button", { name: "Dark theme" }).click();
  await expect(page.locator("body")).toHaveClass(/theme-cyanotype/);

  await page.getByRole("button", { name: "Operator" }).click();
  const connectedBadge = page.getByText("connected", { exact: true });
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

for (const width of [1280, 1600]) {
  test(`gives a long surface bordered controls, room and direct module navigation at ${width}x800`, async ({ page }, testInfo) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width, height: 800 });
    await send(page, `plan Harbour at ${width}`);
    await page.getByRole("navigation", { name: "Surface" }).getByRole("button", { name: "Surfaces" }).click();
    await page.getByPlaceholder("New surface title").fill("Harbour");
    await page.getByPlaceholder("New surface title").press("Enter");
    const articles = page.locator("article.module");
    for (let index = 0; index < 7; index += 1) {
      await page.getByRole("button", { name: "Add module", exact: true }).click();
      await expect(articles).toHaveCount(index + 1);
      const article = articles.nth(index);
      await article.getByRole("button", { name: "Edit", exact: true }).click();
      await article.locator("textarea").fill(
        `# Harbour ${index + 1}\n\n` + Array.from({ length: 12 }, (_, line) =>
          `Harbour planning detail ${line + 1}: keep the shared work in view.`,
        ).join("\n\n"),
      );
      await article.getByRole("button", { name: "Save", exact: true }).click();
      await expect(article.locator("textarea")).toHaveCount(0);
    }
    await articles.first().getByRole("button", { name: "Add note", exact: true }).click();
    await page.getByPlaceholder("Note on this module").fill(
      "Check the harbour entrance before allocating the next shift. ".repeat(8),
    );
    await page.getByRole("button", { name: "Save note", exact: true }).click();
    await expect(articles.first().getByRole("button", { name: "Remove", exact: true })).toBeVisible();

    // Saving through the real form also makes the creation library selector available.
    await page.getByRole("button", { name: "Save to library", exact: true }).click();
    await page.getByRole("textbox", { name: "Library description", exact: true }).fill("Harbour planning modules");
    await page.getByRole("form", { name: "Save to the library" }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("combobox", { name: "From the library" })).toBeVisible();

    const pane = page.locator(".surfaces");
    const work = page.locator(".surfaces-surface .work");
    await pane.evaluate((element) => { element.scrollTop = 0; });
    await page.screenshot({ path: testInfo.outputPath("surface-controls.png") });
    const divider = await work.evaluate((element) => element.getBoundingClientRect().right);
    const controls = pane.locator('button, input:not([type="checkbox"]), select, textarea');
    const measurements = await controls.evaluateAll((elements) => elements.map((element) => {
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return {
        name: element.getAttribute("aria-label") ?? element.textContent?.trim() ?? element.tagName,
        left: rect.left,
        right: rect.right,
        border: parseFloat(style.borderTopWidth),
        borderStyle: style.borderTopStyle,
        paddingX: parseFloat(style.paddingLeft),
        paddingY: parseFloat(style.paddingTop),
      };
    }));
    for (const control of measurements) {
      expect.soft(control.border, `${control.name}: visible border`).toBeGreaterThanOrEqual(1);
      expect.soft(control.borderStyle, `${control.name}: solid border`).toBe("solid");
      expect.soft(control.paddingX, `${control.name}: horizontal padding`).toBeGreaterThanOrEqual(6);
      expect.soft(control.paddingY, `${control.name}: vertical padding`).toBeGreaterThanOrEqual(4);
      expect.soft(control.left, `${control.name}: window inset`).toBeGreaterThanOrEqual(12);
      expect.soft(divider - control.right, `${control.name}: divider inset`).toBeGreaterThanOrEqual(12);
    }
    const moduleWidth = await articles.first().evaluate((element) => element.getBoundingClientRect().width);
    console.log(JSON.stringify({ viewport: width, moduleWidth, divider, controls: measurements }));
    expect.soft(moduleWidth, "module column exceeds the captured 720px").toBeGreaterThan(720);
    const list = page.getByRole("complementary", { name: "Surfaces", exact: true });
    const existing = list.getByRole("button", { name: "Harbour", exact: true });
    const title = page.getByPlaceholder("New surface title");
    const listBox = await existing.boundingBox();
    const titleBox = await title.boundingBox();
    expect.soft(listBox!.y + listBox!.height, "existing surfaces precede creation").toBeLessThanOrEqual(titleBox!.y);

    await articles.first().getByRole("button", { name: "Remove", exact: true }).scrollIntoViewIfNeeded();
    await page.screenshot({ path: testInfo.outputPath("surface-module-actions.png") });
    await pane.evaluate((element) => { element.scrollTop = 0; });
    const navigation = page.getByRole("navigation", { name: "Modules", exact: true });
    await expect(navigation.getByRole("link")).toHaveCount(7);
    for (let index = 0; index < 7; index += 1) {
      const link = navigation.getByRole("link").nth(index);
      await expect(link).toContainText(`Harbour ${index + 1}`);
      await link.click();
      const target = articles.nth(index);
      await expect(target).toBeInViewport();
      expect(await target.getAttribute("id")).toBe((await link.getAttribute("href"))?.slice(1));
    }
    // Reach seven from the start without stepping through earlier modules.
    await pane.evaluate((element) => { element.scrollTop = 0; });
    await expect(articles.nth(6)).not.toBeInViewport();
    await navigation.getByRole("link").nth(6).click();
    await expect(articles.nth(6)).toBeInViewport();
    await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath("surface-seventh-module.png") });
  });
}


test("shows the available model after Settings removes the session’s selected model", async ({ page }) => {
  const stack = await startIsolatedStack();
  try {
    await page.goto(stack.baseUrl);
    await openComposer(page);
    await sendNewConversation(page, "Which model remains after removing the selected one?");
    const model = page.getByRole("combobox", { name: "Model", exact: true });
    await expect(model).toHaveValue("e2e-model");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const configuredModels = page.locator(".model-row");
    const replacement = configuredModels.filter({ hasText: "preferred-model" });
    await replacement.getByRole("button", { name: "Use", exact: true }).click();
    const saved = page.waitForResponse(async (response) => {
      if (response.request().method() !== "PUT" || !response.url().endsWith("/api/settings") || !response.ok()) return false;
      const settings = await response.json();
      return settings.default_model === "preferred-model" && !settings.models["e2e-model"];
    });
    await configuredModels.filter({ hasText: "e2e-model" }).getByRole("button", { name: "Remove", exact: true }).click();
    await saved;
    await page.getByRole("button", { name: "Close settings", exact: true }).click();
    await expect(model).toHaveValue("preferred-model");
  } finally {
    await stopStack(stack.child);
    await rm(stack.root, { recursive: true, force: true });
  }
});
