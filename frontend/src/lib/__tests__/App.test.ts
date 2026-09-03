// @vitest-environment jsdom
/**
 * Whole-shell coverage for the ambient channel's control path.
 *
 * AppLayout can only be driven through the props it is handed, so a test that
 * mounts it directly cannot reach the decisions App.svelte owns: which surface
 * a selected conversation lands the user on, and whether moving to a surface
 * keeps or drops the conversation socket. Those live in App.svelte's own
 * handlers, so these tests mount App itself and exercise the real controls,
 * with the HTTP and WebSocket boundaries mocked.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";
import App from "../../App.svelte";
import { getConversation } from "../api/conversations";
import type { WebSocketClient } from "../api/websocket";

const space = {
  id: "space-1",
  name: "Default",
  description: "",
  is_default: true,
  slug: "default",
  created_at: "2026-08-29T09:00:00Z",
};

const summaries = [
  {
    id: "conv-1",
    created_at: "2026-08-29T10:00:00Z",
    last_activity: "2026-08-29T10:00:00Z",
    message_preview: "Release checklist review",
    has_interrupted_turn: false,
  },
  {
    id: "conv-2",
    created_at: "2026-08-29T10:05:00Z",
    last_activity: "2026-08-29T10:05:00Z",
    message_preview: "Ledger migration",
    has_interrupted_turn: false,
  },
];

function detail(id: string) {
  return {
    id,
    space_id: space.id,
    created_at: "2026-08-29T10:00:00Z",
    last_activity: "2026-08-29T10:00:00Z",
    messages: [
      {
        role: "user" as const,
        content: `History of ${id}`,
        timestamp: "2026-08-29T10:00:01Z",
        turn_state: "completed" as const,
      },
    ],
  };
}

const wsClient = {
  connect: vi.fn(),
  send: vi.fn(),
  disconnect: vi.fn(),
  onEvent: vi.fn(),
  onStatusChange: vi.fn(),
} satisfies WebSocketClient;

vi.mock("../api/websocket", () => ({
  createWebSocketClient: () => wsClient,
}));

vi.mock("../api/spaces", () => ({
  listSpaces: vi.fn(async () => [space]),
  getSpace: vi.fn(async () => space),
  createSpace: vi.fn(),
  updateSpace: vi.fn(),
  deleteSpace: vi.fn(),
  setSpacesBaseUrl: vi.fn(),
  listSpaceWorkspaces: vi.fn(async () => []),
  listSpaceSessions: vi.fn(async () => []),
}));

vi.mock("../api/conversations", () => ({
  setBaseUrl: vi.fn(),
  listConversations: vi.fn(async () => summaries),
  getConversation: vi.fn(async (id: string) => detail(id)),
  createConversation: vi.fn(),
  deleteConversation: vi.fn(),
}));

vi.mock("../api/settings", () => ({
  fetchSettings: vi.fn(async () => ({ models: {}, default_model: null })),
}));

vi.mock("../api/agent-definitions", () => ({
  listEffectiveAgentDefinitions: vi.fn(async () => []),
}));

vi.mock("../api/pages", () => ({
  setBaseUrl: vi.fn(),
  listPages: vi.fn(async () => ({ pages: [] })),
  listPageTemplates: vi.fn(async () => ({ templates: [] })),
  getPage: vi.fn(),
  createPage: vi.fn(),
  writePageModule: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

function surfaceButton(label: string): HTMLButtonElement {
  const navigation = document.querySelector('nav[aria-label="Surface"]');
  const button = Array.from(navigation?.querySelectorAll("button") ?? []).find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error(`Surface control "${label}" was not rendered`);
  }
  return button;
}

/** Mount App on a deep link, so a conversation is live before the test acts. */
async function mountAppOnConversation(): Promise<MountedComponent> {
  window.history.replaceState({}, "", "/conversations/conv-1");
  const component = mount(App, { target: document.body });
  await vi.waitFor(() => {
    expect(document.querySelector('nav[aria-label="Surface"]')).not.toBeNull();
    expect(document.body.textContent).toContain("History of conv-1");
  });
  return component;
}

describe("ambient channel control path", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    window.localStorage.clear();
    window.localStorage.setItem("swage:active-space", JSON.stringify(space.id));
    vi.clearAllMocks();
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.replaceChildren();
    window.history.replaceState({}, "", "/");
  });

  it("keeps the conversation socket when the user moves to the work surface", async () => {
    component = await mountAppOnConversation();
    wsClient.connect.mockClear();
    vi.mocked(getConversation).mockClear();

    surfaceButton("Pages").click();
    await vi.waitFor(() => {
      expect(surfaceButton("Pages").getAttribute("aria-pressed")).toBe("true");
    });

    // The socket is the conversation's: the work surface must not cost it.
    expect(wsClient.disconnect).not.toHaveBeenCalled();
    expect(wsClient.connect).not.toHaveBeenCalled();
    expect(getConversation).not.toHaveBeenCalled();
    expect(document.querySelector('main.pages-surface [aria-label="Conversation"]')?.textContent)
      .toContain("History of conv-1");
  });

  it("keeps the selected conversation socket while the operator surface opens", async () => {
    component = await mountAppOnConversation();
    wsClient.connect.mockClear();
    wsClient.disconnect.mockClear();

    surfaceButton("Operator").click();
    await vi.waitFor(() => {
      expect(surfaceButton("Operator").getAttribute("aria-pressed")).toBe("true");
    });

    expect(wsClient.disconnect).not.toHaveBeenCalled();
    expect(wsClient.connect).not.toHaveBeenCalled();
  });

  it("cycles sessions in place, leaving the user on the work surface", async () => {
    component = await mountAppOnConversation();
    surfaceButton("Pages").click();
    await vi.waitFor(() => {
      expect(surfaceButton("Pages").getAttribute("aria-pressed")).toBe("true");
    });

    const selector = document.querySelector(
      'main.pages-surface select[aria-label="Active conversation"]',
    ) as HTMLSelectElement;
    selector.value = "conv-2";
    selector.dispatchEvent(new Event("change", { bubbles: true }));

    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("History of conv-2");
    });
    // Picking up the other conversation did not move the user off their work.
    expect(surfaceButton("Pages").getAttribute("aria-pressed")).toBe("true");
    expect(document.querySelector("main.pages-surface")).not.toBeNull();
    expect(wsClient.connect).toHaveBeenLastCalledWith("conv-2");
  });

  it("starts a conversation in place, leaving the user on the work surface", async () => {
    component = await mountAppOnConversation();
    surfaceButton("Pages").click();
    await vi.waitFor(() => {
      expect(surfaceButton("Pages").getAttribute("aria-pressed")).toBe("true");
    });

    (document.querySelector(
      'main.pages-surface button.new-session',
    ) as HTMLButtonElement).click();

    await vi.waitFor(() => {
      expect(document.body.textContent).not.toContain("History of conv-1");
    });
    expect(surfaceButton("Pages").getAttribute("aria-pressed")).toBe("true");
  });

  it("routes a workflow run URL into the run view and requests that run", async () => {
    window.history.replaceState({}, "", "/workflow-runs/run-route");
    const fetch = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ run: { id: "run-route", state: "running" }, journal: [] }), {
        status: 200,
      }),
    );
    component = mount(App, { target: document.body });

    await vi.waitFor(() => {
      expect(fetch).toHaveBeenCalledWith("/api/workflow-runs/run-route", {
        method: "GET",
      });
    });
  });
});
