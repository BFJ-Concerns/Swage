import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import os from "node:os";
import http from "node:http";
import { installPairing } from "../src/pairing.mjs";

function session() {
  const handlers = {};
  return { handlers, webRequest: {
    onBeforeSendHeaders: (callback) => handlers.before = callback,
    onHeadersReceived: (callback) => handlers.received = callback,
  } };
}
const invoke = (handler, details) => new Promise((resolve) => handler(details, resolve));

test("desktop remembers a redeemed token and sends it on HTTP and WebSocket upgrades", async () => {
  const directory = await fs.mkdtemp(path.join(process.env.TMPDIR ?? os.tmpdir(), "swage-desktop-pairing-"));
  const seen = [];
  const server = http.createServer((request, response) => {
    seen.push(request.headers["x-swage-token"]);
    if (request.url === "/api/pair") response.setHeader("Set-Cookie", "swage_token=desktop-token; Path=/; HttpOnly");
    response.end("{}");
  });
  try {
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    const first = session();
    await installPairing(first, origin, directory);
    const pair = await fetch(`${origin}/api/pair`, { method: "POST" });
    await invoke(first.handlers.received, { url: `${origin}/api/pair`, statusCode: pair.status,
      responseHeaders: { "set-cookie": pair.headers.getSetCookie() } });
    const second = session();
    await installPairing(second, origin, directory);
    const { requestHeaders } = await invoke(second.handlers.before, { url: `${origin}/api/conversations`, frame: { url: origin }, requestHeaders: {} });
    await fetch(`${origin}/api/conversations`, { headers: requestHeaders });
    assert.equal(seen.at(-1), "desktop-token", "a fresh desktop session must authenticate its HTTP request");
    const upgrade = await invoke(second.handlers.before, { url: `${origin.replace("http:", "ws:")}/api/conversations/id/ws`, frame: { url: origin }, requestHeaders: {} });
    assert.equal(upgrade.requestHeaders["X-Swage-Token"], "desktop-token");
    assert.equal((await fs.stat(path.join(directory, "gateway-pairings.json"))).mode & 0o777, 0o600);
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await fs.rm(directory, { recursive: true, force: true });
  }
});

test("desktop never forwards a gateway token to another origin", async () => {
  const directory = await fs.mkdtemp(path.join(process.env.TMPDIR ?? os.tmpdir(), "swage-desktop-pairing-"));
  try {
    const client = session();
    await installPairing(client, "http://gateway.example", directory);
    await invoke(client.handlers.received, { url: "http://gateway.example/api/pair", statusCode: 200,
      responseHeaders: { "set-cookie": ["swage_token=remembered-token; Path=/; HttpOnly"] } });
    const restored = session();
    await installPairing(restored, "http://gateway.example", directory);
    const original = await invoke(restored.handlers.before, { url: "http://gateway.example/api/", frame: { url: "http://gateway.example/" }, requestHeaders: {} });
    assert.equal(original.requestHeaders["X-Swage-Token"], "remembered-token");
    const result = await invoke(restored.handlers.before, { url: "http://other.example/api/", requestHeaders: original.requestHeaders });
    assert.equal(Object.keys(result.requestHeaders).some((key) => key.toLowerCase() === "x-swage-token"), false);
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});


test("desktop does not lend paired authority to a foreign or missing initiating frame", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "swage-desktop-pairing-"));
  try {
    const client = session();
    await installPairing(client, "http://gateway.example", directory);
    await invoke(client.handlers.received, { url: "http://gateway.example/api/pair", statusCode: 200,
      responseHeaders: { "set-cookie": ["swage_token=remembered-token; Path=/; HttpOnly"] } });
    for (const frame of [{ url: "https://other.example/" }, undefined]) {
      for (const protocol of ["http", "ws"]) {
        const result = await invoke(client.handlers.before, {
          url: `${protocol}://gateway.example/api/`, frame,
          requestHeaders: { "x-swage-token": "supplied-token" },
        });
        assert.equal(Object.keys(result.requestHeaders).some((key) => key.toLowerCase() === "x-swage-token"),
          false, "a foreign or missing initiating frame must receive no paired token");
      }
    }
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});

test("desktop window refuses navigation and redirects away from the gateway", async () => {
  // Exercise createWindow with Electron's window boundary replaced by a test double.
  const source = await fs.readFile(new URL("../src/main.mjs", import.meta.url), "utf8");
  const body = source.slice(source.indexOf("async function createWindow()"), source.indexOf("\napp.isQuitting = false"));
  const handlers = {};
  let openHandler;
  const contents = { on: (event, handler) => handlers[event] = handler,
    setWindowOpenHandler: (handler) => openHandler = handler };
  const create = new Function("BrowserWindow", "launchManagedGateway", "installPairing", "waitForGateway", "app", "targetUrl", "preloadPath", "isHeadlessShell",
    `let gatewayProcess; ${body}; return createWindow;`)(
      class { constructor() { this.webContents = contents; } once() {} async loadURL() {} },
      () => null, async () => ({}), async () => {}, { getPath: () => "unused" },
      "http://gateway.example", "unused", true);
  await create();
  for (const event of ["will-navigate", "will-redirect", "will-frame-navigate"]) {
    let prevented = false;
    handlers[event]?.({ url: "https://other.example/", preventDefault: () => prevented = true }, "https://other.example/");
    assert.equal(prevented, true, `${event} must refuse a foreign origin`);
    prevented = false;
    handlers[event]?.({ url: "http://gateway.example/topic", preventDefault: () => prevented = true }, "http://gateway.example/topic");
    assert.equal(prevented, false, `${event} must allow the gateway origin`);
  }
  assert.equal(openHandler?.({ url: "https://other.example/" }).action, "deny", "foreign popup must be refused");
});
