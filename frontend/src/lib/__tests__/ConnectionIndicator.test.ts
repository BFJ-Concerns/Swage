// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { mount, unmount } from "svelte";

import ConnectionIndicator from "../components/ConnectionIndicator.svelte";
import type { ConnectionStore } from "../stores/connection";

let component: ReturnType<typeof mount> | undefined;

afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.replaceChildren();
});

function store(status: ConnectionStore["status"]): ConnectionStore {
  return {
    get status() { return status; },
    get errorMessage() { return "Gateway refused the connection"; },
    connectTo: () => {},
    disconnect: () => {},
    sendMessage: () => {},
    cancelTurn: () => {},
    onServerEvent: () => {},
  } as ConnectionStore;
}

describe("ConnectionIndicator", () => {
  it("marks only a connection error as the error banner", () => {
    for (const status of ["disconnected", "reconnecting", "error"] as const) {
      component = mount(ConnectionIndicator, { target: document.body, props: { store: store(status) } });
      const banner = document.querySelector(".banner");
      expect(banner).not.toBeNull();
      expect(banner!.classList.contains("error")).toBe(status === "error");
      unmount(component);
      component = undefined;
      document.body.replaceChildren();
    }
  });
});
