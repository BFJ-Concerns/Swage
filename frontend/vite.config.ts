import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import path from "node:path";
import { fileURLToPath } from "node:url";

const frontendRoot = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  plugins: [svelte()],
  test: {
    include: ["src/**/*.test.ts"],
    // Use jsdom so Svelte components can mount and query the DOM in tests.
    environment: "jsdom",
    environmentOptions: {
      jsdom: {
        url: "http://localhost/",
      },
    },
    // Alias svelte to the browser client entry so mount() is available.
    // Without this, vitest resolves to index-server.js (the default export),
    // which throws lifecycle_function_unavailable on mount().
    alias: [
      {
        find: /^svelte$/,
        replacement: path.resolve(
          frontendRoot,
          "node_modules/svelte/src/index-client.js",
        ),
      },
      // The app-level tests exercise fromStore() in mounted components.
      // Point Vitest at the client store entry so those helpers participate
      // in browser-side reactivity instead of the server fallback module.
      {
        find: /^svelte\/store$/,
        replacement: path.resolve(
          frontendRoot,
          "node_modules/svelte/src/store/index-client.js",
        ),
      },
    ],
  },
  server: {
    port: 5173,
    proxy: {
      // Proxy API and WebSocket requests to the gateway during development.
      // SWAGE_GATEWAY_URL points the dev UI at a gateway on a non-default
      // port (isolated development instances).
      "/api": {
        target: process.env.SWAGE_GATEWAY_URL ?? "http://127.0.0.1:3000",
        changeOrigin: true,
        ws: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("/node_modules/highlight.js/")) {
            return "code-highlighting";
          }
          if (
            id.includes("/node_modules/marked/") ||
            id.includes("/node_modules/marked-highlight/") ||
            id.includes("/node_modules/dompurify/")
          ) {
            return "markdown-rendering";
          }
          return undefined;
        },
      },
    },
  },
});
