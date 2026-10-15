// Pairing belongs to this desktop profile and gateway origin.
import fs from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";

export async function installPairing(session, gatewayUrl, configurationDirectory) {
  const origin = new URL(gatewayUrl).origin;
  const storagePath = path.join(configurationDirectory, "gateway-pairings.json");
  let tokens;
  try {
    tokens = JSON.parse(await fs.readFile(storagePath, "utf8"));
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    tokens = {};
  }
  function isGateway(url) {
    const parsed = new URL(url);
    if (parsed.protocol === "ws:") parsed.protocol = "http:";
    if (parsed.protocol === "wss:") parsed.protocol = "https:";
    return parsed.origin === origin;
  }
  function gatewayHeaders(url, supplied = {}) {
    const headers = { ...supplied };
    for (const key of Object.keys(headers)) {
      if (key.toLowerCase() === "x-swage-token") delete headers[key];
    }
    if (isGateway(url) && tokens[origin]) headers["X-Swage-Token"] = tokens[origin];
    return headers;
  }
  session.webRequest.onBeforeSendHeaders((details, callback) => {
    callback({ requestHeaders: gatewayHeaders(details.url, details.requestHeaders) });
  });
  session.webRequest.onHeadersReceived((details, callback) => {
    const url = new URL(details.url);
    if (!isGateway(details.url) || url.pathname !== "/api/pair" || details.statusCode !== 200) {
      callback({}); return;
    }
    const cookies = Object.entries(details.responseHeaders ?? {})
      .filter(([key]) => key.toLowerCase() === "set-cookie").flatMap(([, values]) => values);
    const cookie = cookies.find((value) => value.startsWith("swage_token="));
    if (!cookie) { callback({}); return; }
    const token = cookie.slice("swage_token=".length).split(";", 1)[0];
    const updated = { ...tokens, [origin]: token };
    const temporary = `${storagePath}.${randomUUID()}.tmp`;
    void (async () => {
      try {
        await fs.mkdir(configurationDirectory, { recursive: true });
        await fs.writeFile(temporary, JSON.stringify(updated), { mode: 0o600, flag: "wx" });
        await fs.rename(temporary, storagePath);
        tokens = updated;
        callback({});
      } catch (error) {
        await fs.rm(temporary, { force: true });
        console.error("Unable to remember gateway pairing:", error.message);
        callback({ cancel: true });
      }
    })();
  });
  return { headers: (url) => gatewayHeaders(url) };
}
