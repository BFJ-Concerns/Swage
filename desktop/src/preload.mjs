import { contextBridge } from "electron";

const gatewayUrl = process.env.SWAGE_DESKTOP_TARGET_URL ?? "http://127.0.0.1:8787";

contextBridge.exposeInMainWorld("swageDesktop", {
  shell: "electron",
  mode: "desktop",
  gatewayUrl,
  managedGateway: process.env.SWAGE_DESKTOP_TARGET_URL == null,
  versions: {
    chrome: process.versions.chrome,
    electron: process.versions.electron,
    node: process.versions.node,
  },
});
