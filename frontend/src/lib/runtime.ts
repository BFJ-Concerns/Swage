export interface DesktopRuntimeInfo {
  shell: "electron";
  mode: "desktop";
  gatewayUrl: string;
  managedGateway: boolean;
  versions: {
    chrome: string;
    electron: string;
    node: string;
  };
}

declare global {
  interface Window {
    swageDesktop?: DesktopRuntimeInfo;
  }
}

export function getDesktopRuntime(): DesktopRuntimeInfo | null {
  return window.swageDesktop ?? null;
}
