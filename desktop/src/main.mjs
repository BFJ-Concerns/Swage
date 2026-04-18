import { app, BrowserWindow } from "electron";
import { spawn } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..", "..");
const preloadPath = path.resolve(__dirname, "preload.mjs");

const targetPort = Number.parseInt(process.env.SWAGE_DESKTOP_PORT ?? "8787", 10);
const targetUrl =
  process.env.SWAGE_DESKTOP_TARGET_URL ?? `http://127.0.0.1:${targetPort}`;
const isHeadlessShell = process.env.SWAGE_DESKTOP_HEADLESS === "1";

let gatewayProcess = null;

if (isHeadlessShell) {
  app.commandLine.appendSwitch("headless");
  app.commandLine.appendSwitch("disable-gpu");
}

async function waitForGateway(url, timeoutMs = 30_000) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    try {
      const response = await fetch(new URL("/api/health", url));
      if (response.ok) return;
    } catch {
      // Gateway not ready yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 300));
  }
  throw new Error(`Timed out waiting for gateway at ${url}`);
}

function launchManagedGateway() {
  if (process.env.SWAGE_DESKTOP_TARGET_URL) return null;

  const child = spawn(
    "cargo",
    [
      "run",
      "-p",
      "swage-gateway",
      "--bin",
      "swage",
      "--",
      "--server-mode",
      "gateway",
      "start",
      "--host",
      "127.0.0.1",
      "--port",
      String(targetPort),
    ],
    {
      cwd: repoRoot,
      stdio: process.env.SWAGE_DESKTOP_GATEWAY_STDIO === "inherit" ? "inherit" : "pipe",
      env: {
        ...process.env,
        SWAGE_DESKTOP_TARGET_URL: targetUrl,
      },
    },
  );

  child.on("exit", (code, signal) => {
    if (!app.isQuitting) {
      console.error(
        `Managed gateway exited before the desktop shell closed (code=${code}, signal=${signal})`,
      );
    }
  });

  return child;
}

async function createWindow() {
  gatewayProcess = launchManagedGateway();
  await waitForGateway(targetUrl);

  const win = new BrowserWindow({
    width: 1440,
    height: 940,
    minWidth: 1080,
    minHeight: 720,
    backgroundColor: "#f7f2ea",
    titleBarStyle: "hiddenInset",
    show: false,
    webPreferences: {
      contextIsolation: true,
      preload: preloadPath,
    },
  });

  win.once("ready-to-show", () => {
    if (!isHeadlessShell) {
      win.show();
    }
    if (process.env.SWAGE_DESKTOP_OPEN_DEVTOOLS === "1") {
      win.webContents.openDevTools({ mode: "detach" });
    }
  });

  await win.loadURL(targetUrl);
  return win;
}

app.isQuitting = false;

app.whenReady().then(async () => {
  await createWindow();

  app.on("activate", async () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      await createWindow();
    }
  });
});

app.on("before-quit", () => {
  app.isQuitting = true;
  if (gatewayProcess && !gatewayProcess.killed) {
    gatewayProcess.kill("SIGTERM");
  }
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
