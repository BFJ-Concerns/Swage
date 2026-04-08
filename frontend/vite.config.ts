import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import path from "node:path";

export default defineConfig({
  plugins: [svelte()],
  test: {
    // Use jsdom so Svelte components can mount and query the DOM in tests.
    environment: "jsdom",
    // Alias svelte to the browser client entry so mount() is available.
    // Without this, vitest resolves to index-server.js (the default export),
    // which throws lifecycle_function_unavailable on mount().
    alias: [
      {
        find: /^svelte$/,
        replacement: path.resolve(
          __dirname,
          "node_modules/svelte/src/index-client.js",
        ),
      },
      // The app-level tests exercise fromStore() in mounted components.
      // Point Vitest at the client store entry so those helpers participate
      // in browser-side reactivity instead of the server fallback module.
      {
        find: /^svelte\/store$/,
        replacement: path.resolve(
          __dirname,
          "node_modules/svelte/src/store/index-client.js",
        ),
      },
    ],
  },
  server: {
    port: 5173,
    proxy: {
      // Proxy API and WebSocket requests to the gateway during development.
      "/api": {
        target: "http://127.0.0.1:3000",
        changeOrigin: true,
        ws: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
