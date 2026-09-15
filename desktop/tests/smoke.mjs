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

function createConversationState(id, topicId) {
  const now = new Date().toISOString();
  return {
    id,
    topic_id: topicId,
    created_at: now,
    last_activity: now,
    messages: [],
  };
}

function createMockGateway() {
  const topic = {
    id: "topic-default",
    name: "Default",
    description: "Smoke-test topic",
    slug: "default",
    created_at: "2026-04-17T12:00:00Z",
  };

  const conversations = new Map();
  const socketsByConversation = new Map();
  const transportSockets = new Set();
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

    if (request.method === "GET" && pathname === "/api/topics") {
      writeJson(response, 200, { topics: [topic] });
      return;
    }

    if (request.method === "GET" && pathname === "/api/sessions") {
      writeJson(response, 200, {
        entries: listConversationSummaries().map((conversation) => ({
          source: {
            kind: "conversation",
            session: {
              ...conversation,
              topic_id: topic.id,
              topic_name: topic.name,
              agent_id: "gateway",
              origin: { kind: "direct" },
              has_running_turn: false,
            },
          },
          last_activity: conversation.last_activity,
          automated: [],
        })),
      });
      return;
    }

    if (request.method === "GET" && pathname === `/api/topics/${topic.id}/materials`) {
      writeJson(response, 200, { materials: [] });
      return;
    }

    if (request.method === "GET" && pathname === "/api/operator/overview") {
      writeJson(response, 200, {
        gateway: {
          status: "running",
          health: "healthy",
          shutdown_requested: false,
          seconds_since_activity: 0,
          active_turn_count: 0,
          active_connection_count: 1,
        },
        sessions: listConversationSummaries().map((conversation) => ({
          ...conversation,
          topic_id: topic.id,
          topic_name: topic.name,
          has_active_connection: false,
          has_running_turn: false,
        })),
        approvals: [],
        scheduled_jobs: { jobs: [] },
        heartbeats: { heartbeats: [] },
        channels: { channels: [] },
        nodes: [],
      });
      return;
    }

    if (request.method === "GET" && pathname === "/api/voice/capabilities") {
      writeJson(response, 200, {
        transcription: false,
        synthesis: false,
        default_voice: null,
        supported_voices: [],
      });
      return;
    }

    if (request.method === "GET" && pathname === "/api/recovery/export") {
      writeJson(response, 200, {
        version: 1,
        topics: [topic],
        workspaces: [],
        sessions: [],
        secrets: [],
        channel_links: [],
        devices: [],
        scheduled_jobs: [],
        heartbeats: [],
      });
      return;
    }

    if (
      request.method === "GET"
      && pathname === `/api/topics/${topic.id}/sessions`
    ) {
      writeJson(response, 200, { conversations: listConversationSummaries() });
      return;
    }

    if (
      request.method === "POST"
      && pathname === `/api/topics/${topic.id}/sessions`
    ) {
      conversationCounter += 1;
      const id = `conv-${conversationCounter}`;
      const conversation = createConversationState(id, topic.id);
      conversations.set(id, conversation);
      writeJson(response, 200, {
        id,
        topic_id: topic.id,
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

    if (
      request.method === "GET"
      && pathname.match(/^\/api\/sessions\/conv-\d+\/materials$/)
    ) {
      writeJson(response, 200, { materials: [] });
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
  const defaultTopic = page.getByRole("article", { name: "Topic Default" });
  await defaultTopic
    .getByRole("button", { name: "Preview topic Default", exact: true })
    .click();
  await defaultTopic.getByRole("button", { name: "Open", exact: true }).click();
  await page.getByRole("button", { name: /\+ new conversation/i }).click();
  await page
    .getByPlaceholder("Send a message — Shift+Enter for a new line")
    .fill("Hello from smoke");
  await page.getByRole("button", { name: /send/i }).click();
  await page.waitForURL(/\/conversations\/conv-\d+$/);
  await page
    .getByLabel("Conversation messages")
    .getByText("Desktop shell echoed: Hello from smoke")
    .waitFor();

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
  if (!process.env.DISPLAY && !process.env.WAYLAND_DISPLAY) {
    console.log("electron smoke: skipped (no display server)");
    return;
  }

  console.log("electron smoke: start");
  const electronApp = await electron.launch({
    args: [desktopMain],
    cwd: repoRoot,
    env: {
      ...process.env,
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
    await createConversationAndWaitForReply(window, "REMOTE");
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
