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
    const { requestHeaders } = await invoke(second.handlers.before, { url: `${origin}/api/conversations`, requestHeaders: {} });
    await fetch(`${origin}/api/conversations`, { headers: requestHeaders });
    assert.equal(seen.at(-1), "desktop-token", "a fresh desktop session must authenticate its HTTP request");
    const upgrade = await invoke(second.handlers.before, { url: `${origin.replace("http:", "ws:")}/api/conversations/id/ws`, requestHeaders: {} });
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
    const original = await invoke(restored.handlers.before, { url: "http://gateway.example/api/", requestHeaders: {} });
    assert.equal(original.requestHeaders["X-Swage-Token"], "remembered-token");
    const result = await invoke(restored.handlers.before, { url: "http://other.example/api/", requestHeaders: original.requestHeaders });
    assert.equal(Object.keys(result.requestHeaders).some((key) => key.toLowerCase() === "x-swage-token"), false);
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});
