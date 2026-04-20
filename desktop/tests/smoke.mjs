import http from "node:http";
import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { _electron as electron, chromium } from "playwright";
import { WebSocketServer } from "ws";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const desktopRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(desktopRoot, "..");
const frontendDist = path.resolve(repoRoot, "frontend", "dist");
const desktopMain = path.resolve(desktopRoot, "src", "main.mjs");

function createConversationState(id, projectId) {
  const now = new Date().toISOString();
  return {
    id,
    project_id: projectId,
    created_at: now,
    last_activity: now,
    messages: [],
  };
}

function createMockGateway() {
  const project = {
    id: "proj-default",
    name: "Default",
    slug: "default",
    is_default: true,
    workspace_path: "/tmp/swage-desktop-smoke",
    created_at: "2026-04-17T12:00:00Z",
  };

  const conversations = new Map();
  const socketsByConversation = new Map();
  const transportSockets = new Set();
  const availableLenses = [
    {
      id: "assistant",
      title: "Assistant",
      summary: "General-purpose multi-agent workspace.",
      default_surface: "conversation",
      ui_modules: ["conversation", "operator"],
      prominent_team_ids: [],
      source_kind: "built_in",
    },
    {
      id: "builder",
      title: "Builder",
      summary: "Compose lenses, teams, and extensions.",
      default_surface: "builder",
      ui_modules: ["conversation", "operator", "builder"],
      prominent_team_ids: [],
      source_kind: "built_in",
    },
  ];
  let activeLens = "assistant";
  let conversationCounter = 0;

  function listConversationSummaries() {
    return [...conversations.values()]
      .sort((left, right) => right.last_activity.localeCompare(left.last_activity))
      .map((conversation) => ({
        id: conversation.id,
        created_at: conversation.created_at,
        last_activity: conversation.last_activity,
        message_preview: conversation.messages.at(-1)?.content ?? null,
        has_interrupted_turn: false,
      }));
  }

  function writeJson(response, statusCode, body) {
    response.writeHead(statusCode, { "content-type": "application/json" });
    response.end(JSON.stringify(body));
  }

  function buildLensRuntime() {
    return {
      active_lens: activeLens,
      updated_at: new Date().toISOString(),
      available_lenses: availableLenses,
    };
  }

  async function writeAsset(response, pathname) {
    const relativePath = pathname === "/" ? "index.html" : pathname.slice(1);
    const assetPath = path.resolve(frontendDist, relativePath);

    if (!assetPath.startsWith(frontendDist)) {
      response.writeHead(403);
      response.end("forbidden");
      return;
    }

    try {
      const fileBuffer = await fs.readFile(assetPath);
      const extension = path.extname(assetPath);
      const contentType =
        extension === ".html"
          ? "text/html; charset=utf-8"
          : extension === ".js"
            ? "text/javascript; charset=utf-8"
            : extension === ".css"
              ? "text/css; charset=utf-8"
              : extension === ".svg"
                ? "image/svg+xml"
                : "application/octet-stream";
      response.writeHead(200, { "content-type": contentType });
      response.end(fileBuffer);
    } catch (error) {
      response.writeHead(404);
      response.end(`missing asset: ${pathname}`);
    }
  }

  const server = http.createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    const pathname = url.pathname;

    if (request.method === "GET" && pathname === "/api/health") {
      writeJson(response, 200, { status: "ok" });
      return;
    }

    if (request.method === "GET" && pathname === "/api/projects") {
      writeJson(response, 200, { projects: [project] });
      return;
    }

    if (request.method === "GET" && pathname === "/api/lens-runtime") {
      writeJson(response, 200, buildLensRuntime());
      return;
    }

    if (request.method === "PUT" && pathname === "/api/lens-runtime") {
      let body = "";
      request.setEncoding("utf8");
      request.on("data", (chunk) => {
        body += chunk;
      });
      request.on("end", () => {
        const nextLens = JSON.parse(body || "{}").lens;
        if (!availableLenses.some((lens) => lens.id === nextLens)) {
          writeJson(response, 404, { error: "lens_not_found" });
          return;
        }

        activeLens = nextLens;
        writeJson(response, 200, buildLensRuntime());
      });
      return;
    }

    if (
      request.method === "GET"
      && pathname === `/api/projects/${project.id}/sessions`
    ) {
      writeJson(response, 200, { conversations: listConversationSummaries() });
      return;
    }

    if (
      request.method === "POST"
      && pathname === `/api/projects/${project.id}/sessions`
    ) {
      conversationCounter += 1;
      const id = `conv-${conversationCounter}`;
      const conversation = createConversationState(id, project.id);
      conversations.set(id, conversation);
      writeJson(response, 200, {
        id,
        project_id: project.id,
        created_at: conversation.created_at,
        last_activity: conversation.last_activity,
      });
      return;
    }

    if (request.method === "GET" && pathname.startsWith("/api/conversations/")) {
      const id = pathname.split("/").at(-1);
      const conversation = id ? conversations.get(id) : null;

      if (!conversation) {
        writeJson(response, 404, { error: "not_found" });
        return;
      }

      writeJson(response, 200, conversation);
      return;
    }

    if (!pathname.startsWith("/api/")) {
      await writeAsset(response, pathname);
      return;
    }

    writeJson(response, 404, { error: "not_found" });
  });

  server.on("connection", (socket) => {
    transportSockets.add(socket);
    socket.on("close", () => {
      transportSockets.delete(socket);
    });
  });

  const websocketServer = new WebSocketServer({ noServer: true });

  server.on("upgrade", (request, socket, head) => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    const match = url.pathname.match(/^\/api\/conversations\/([^/]+)\/ws$/);

    if (!match) {
      socket.destroy();
      return;
    }

    const conversationId = match[1];
    if (!conversations.has(conversationId)) {
      socket.destroy();
      return;
    }

    websocketServer.handleUpgrade(request, socket, head, (ws) => {
      websocketServer.emit("connection", ws, conversationId);
    });
  });

  websocketServer.on("connection", (ws, conversationId) => {
    const sockets = socketsByConversation.get(conversationId) ?? new Set();
    sockets.add(ws);
    socketsByConversation.set(conversationId, sockets);

    ws.on("close", () => {
      sockets.delete(ws);
      if (sockets.size === 0) {
        socketsByConversation.delete(conversationId);
      }
    });

    ws.on("message", (rawFrame) => {
      const frame = JSON.parse(rawFrame.toString());
      if (frame.type !== "send_message") {
        return;
      }

      const conversation = conversations.get(conversationId);
      if (!conversation) return;

      const timestamp = new Date().toISOString();
      conversation.last_activity = timestamp;
      conversation.messages.push({
        role: "user",
        content: frame.content,
        timestamp,
        turn_state: "completed",
        tool_calls: [],
      });

      const reply = `Desktop shell echoed: ${frame.content}`;
      const turnId = `turn-${Date.now()}`;
      const audience = socketsByConversation.get(conversationId) ?? new Set();

      setTimeout(() => {
        for (const client of audience) {
          client.send(JSON.stringify({ type: "token", content: reply }));
        }
      }, 20);

      setTimeout(() => {
        conversation.last_activity = new Date().toISOString();
        conversation.messages.push({
          role: "assistant",
          content: reply,
          timestamp: conversation.last_activity,
          turn_state: "completed",
          tool_calls: [],
        });

        for (const client of audience) {
          client.send(JSON.stringify({ type: "turn_complete", turn_id: turnId }));
        }
      }, 60);
    });
  });

  return {
    async start() {
      await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
      const address = server.address();
      if (!address || typeof address === "string") {
        throw new Error("mock gateway did not expose a TCP port");
      }

      return `http://127.0.0.1:${address.port}`;
    },

    async stop() {
      for (const client of websocketServer.clients) {
        client.terminate();
      }

      for (const socket of transportSockets) {
        socket.destroy();
      }

      await new Promise((resolve, reject) => {
        websocketServer.close((error) => (error ? reject(error) : resolve()));
      });
      await new Promise((resolve, reject) => {
        server.close((error) => (error ? reject(error) : resolve()));
      });
    },
  };
}

async function createConversationAndWaitForReply(page, expectedRuntimeText) {
  await page.getByRole("button", { name: /\+ new conversation/i }).click();
  await page.waitForURL(/\/conversations\/conv-\d+$/);
  await page.locator("textarea").fill("Hello from smoke");
  await page.getByRole("button", { name: /send/i }).click();
  await page.getByText("Desktop shell echoed: Hello from smoke").waitFor();

  if (expectedRuntimeText) {
    await page.getByText(expectedRuntimeText, { exact: true }).waitFor();
  }
}

async function runBrowserSmoke(baseUrl) {
  console.log("browser smoke: start");
  const browser = await chromium.launch();
  const page = await browser.newPage();

  try {
    await page.goto(baseUrl, { waitUntil: "networkidle" });
    await createConversationAndWaitForReply(page, null);
  } finally {
    await browser.close();
    console.log("browser smoke: done");
  }
}

async function runElectronSmoke(baseUrl) {
  console.log("electron smoke: start");
  const electronApp = await electron.launch({
    args: [desktopMain],
    cwd: repoRoot,
    env: {
      ...process.env,
      SWAGE_DESKTOP_HEADLESS: "1",
      SWAGE_DESKTOP_TARGET_URL: baseUrl,
    },
  });

  try {
    console.log("electron smoke: launched");
    let window = electronApp.windows()[0] ?? null;
    const startedAt = Date.now();
    while (!window && Date.now() - startedAt < 30_000) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      window = electronApp.windows()[0] ?? null;
    }

    if (!window) {
      throw new Error("Electron shell did not expose a renderer window");
    }

    console.log("electron smoke: first window");
    await window.waitForLoadState("domcontentloaded");
    console.log("electron smoke: domcontentloaded");
    await createConversationAndWaitForReply(window, "Desktop shell");
    await window.getByText("Remote gateway", { exact: true }).waitFor();
  } finally {
    await electronApp.close();
    console.log("electron smoke: done");
  }
}

const gateway = createMockGateway();

try {
  const baseUrl = await gateway.start();
  console.log(`mock gateway: ${baseUrl}`);
  await runBrowserSmoke(baseUrl);
  await runElectronSmoke(baseUrl);
} finally {
  await gateway.stop();
  console.log("mock gateway: stopped");
}
