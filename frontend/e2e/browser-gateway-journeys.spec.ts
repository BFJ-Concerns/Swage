// The provider is controlled; Chromium, the rendered Svelte app, HTTP/WebSocket
// transport, gateway turn loop, storage, cancellation and approvals are live.
import { expect, test, type Page } from "@playwright/test";

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

test("streams a provider response into the rendered conversation", async ({ page }) => {
  await send(page, "stream this turn");
  await expect(page.getByLabel("Assistant message")).toContainText(
    "Streamed response reached the browser.",
  );
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
});

test("cancels an active turn from the rendered conversation", async ({ page }) => {
  await send(page, "cancel this turn");
  await expect(page.getByLabel("Assistant message")).toContainText(
    "Partial response before cancellation.",
  );
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await expect(page.getByLabel("Assistant message")).not.toContainText(
    "This tail must never render.",
  );
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
