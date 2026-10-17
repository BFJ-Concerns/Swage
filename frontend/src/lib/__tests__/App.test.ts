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
import { UNSETTLED_RUN_REFRESH_MS } from "../one-shot-runs";
import App from "../../App.svelte";
import { createConversation, getConversation } from "../api/conversations";
import type { WebSocketClient } from "../api/websocket";

const topic = {
  id: "topic-1",
  name: "Default",
  description: "",
  is_default: true,
  slug: "default",
  created_at: "2026-08-29T09:00:00Z",
};

const summaries = [
  {
    id: "conv-1",
    agent_id: "gateway",
    created_at: "2026-08-29T10:00:00Z",
    last_activity: "2026-08-29T10:00:00Z",
    message_preview: "Release checklist review",
    has_interrupted_turn: false,
  },
  {
    id: "conv-2",
    agent_id: "gateway",
    created_at: "2026-08-29T10:05:00Z",
    last_activity: "2026-08-29T10:05:00Z",
    message_preview: "Ledger migration",
    has_interrupted_turn: false,
  },
];

function detail(id: string) {
  return {
    id,
    topic_id: topic.id,
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

vi.mock("../api/topics", () => ({
  listTopics: vi.fn(async () => [topic]),
  getTopic: vi.fn(async () => topic),
  createTopic: vi.fn(),
  updateTopic: vi.fn(),
  deleteTopic: vi.fn(),
  setTopicsBaseUrl: vi.fn(),
  listTopicWorkspaces: vi.fn(async () => []),
  listTopicSessions: vi.fn(async () => []),
}));

vi.mock("../api/conversations", () => ({
  setBaseUrl: vi.fn(),
  listConversations: vi.fn(async () => summaries),
  getConversation: vi.fn(async (id: string) => detail(id)),
  createConversation: vi.fn(),
  deleteConversation: vi.fn(),
  listBranches: vi.fn(async () => []),
  createBranch: vi.fn(),
  getThread: vi.fn(),
  abandonThread: vi.fn(),
}));

vi.mock("../api/sessions", () => ({
  listRecentSessions: vi.fn(async () => []),
}));

vi.mock("../api/settings", () => ({
  fetchSettings: vi.fn(async () => ({ models: {}, default_model: null })),
}));

vi.mock("../api/agent-definitions", () => ({
  listEffectiveAgentDefinitions: vi.fn(async () => []),
}));

vi.mock("../api/surfaces", () => ({
  setBaseUrl: vi.fn(),
  listSurfaces: vi.fn(async () => ({ surfaces: [] })),
  listLibrary: vi.fn(async () => ({ entries: [] })),
  getSurface: vi.fn(),
  createSurface: vi.fn(),
  writeSurfaceModule: vi.fn(),
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
    window.localStorage.setItem("swage:active-topic", JSON.stringify(topic.id));
    vi.clearAllMocks();
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    vi.restoreAllMocks();
    document.body.replaceChildren();
    window.history.replaceState({}, "", "/");
  });

  it("keeps the conversation socket when the user moves to the work surface", async () => {
    component = await mountAppOnConversation();
    wsClient.connect.mockClear();
    vi.mocked(getConversation).mockClear();

    surfaceButton("Surfaces").click();
    await vi.waitFor(() => {
      expect(surfaceButton("Surfaces").getAttribute("aria-pressed")).toBe("true");
    });

    // The socket is the conversation's: the work surface must not cost it.
    expect(wsClient.disconnect).not.toHaveBeenCalled();
    expect(wsClient.connect).not.toHaveBeenCalled();
    expect(getConversation).not.toHaveBeenCalled();
    expect(document.querySelector('main.surfaces-surface [aria-label="Conversation"]')?.textContent)
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
    surfaceButton("Surfaces").click();
    await vi.waitFor(() => {
      expect(surfaceButton("Surfaces").getAttribute("aria-pressed")).toBe("true");
    });

    const selector = document.querySelector(
      'main.surfaces-surface select[aria-label="Active conversation"]',
    ) as HTMLSelectElement;
    selector.value = "conv-2";
    selector.dispatchEvent(new Event("change", { bubbles: true }));

    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("History of conv-2");
    });
    // Picking up the other conversation did not move the user off their work.
    expect(surfaceButton("Surfaces").getAttribute("aria-pressed")).toBe("true");
    expect(document.querySelector("main.surfaces-surface")).not.toBeNull();
    expect(wsClient.connect).toHaveBeenLastCalledWith("conv-2");
  });

  it("creates a conversation in place, leaving the user on the work surface", async () => {
    component = await mountAppOnConversation();
    surfaceButton("Surfaces").click();
    await vi.waitFor(() => {
      expect(surfaceButton("Surfaces").getAttribute("aria-pressed")).toBe("true");
    });

    vi.mocked(createConversation).mockResolvedValueOnce({
      id: "conv-new",
      agent_id: "gateway",
      topic_id: topic.id,
      selected_model_id: null,
      created_at: "2026-08-29T10:10:00Z",
      last_activity: "2026-08-29T10:10:00Z",
    });

    (document.querySelector(
      'main.surfaces-surface button.new-session',
    ) as HTMLButtonElement).click();

    await vi.waitFor(() => {
      expect(window.location.pathname).toBe("/conversations/conv-new");
    });
    expect(createConversation).toHaveBeenCalledWith(topic.id, undefined, undefined, undefined);
    expect((document.querySelector(
      'main.surfaces-surface select[aria-label="Active conversation"]',
    ) as HTMLSelectElement).value).toBe("conv-new");
    expect(document.body.textContent).not.toContain("History of conv-1");
    expect(surfaceButton("Surfaces").getAttribute("aria-pressed")).toBe("true");
  });

  it("routes a workflow run URL into the run view and requests that run", async () => {
    window.history.replaceState({}, "", "/workflow-runs/run-route");
    const fetch = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(
        JSON.stringify({
          run: { id: "run-route", state: "running", interruptions: [] },
          journal: [],
        }),
        { status: 200 },
      ),
    );
    component = mount(App, { target: document.body });

    await vi.waitFor(() => {
      expect(fetch).toHaveBeenCalledOnce();
    });
    expect(fetch).toHaveBeenCalledWith("/api/workflow-runs/run-route");
  });
  function runResponse(id: string, state = "running") {
    const ended = ["completed", "failed", "cancelled"].includes(state);
    return new Response(JSON.stringify({ run: { id, state, interruptions: [], created_at: "2026-09-02T10:00:00Z", terminal_account: ended ? { ending: state === "failed" ? { ending: "threw", error: "timer threw" } : state === "cancelled" ? { ending: "cancelled" } : { ending: "returned" }, workers: { done: [], failed: [], cut_short: [], unsettled: [] }, settled_at: "2026-09-02T10:05:00Z" } : null }, journal: [] }), { status: 200 });
  }

  async function startRunView() {
    window.history.replaceState({}, "", "/workflow-runs/run-polled");
    component = mount(App, { target: document.body });
    await vi.waitFor(() => expect(document.querySelector(".run .state")).not.toBeNull(), { timeout: 5000 });
    vi.useFakeTimers();
  }

  it.each([["parked", "failed"], ["running", "cancelled"], ["running", "completed"]])("run view refreshes %s to %s and stops at its account", async (initial, terminal) => {
    const fetch = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(runResponse("run-polled", initial)).mockImplementation(async () => runResponse("run-polled", terminal));
    try {
      await startRunView();
      // The initial read scheduled its timeout under the real clock.
      // Re-entering the route below lets fake time own the refresh timer.
      window.history.pushState({}, "", "/workflow-runs/run-polled-again");
      window.dispatchEvent(new PopStateEvent("popstate"));
      fetch.mockReset().mockResolvedValueOnce(runResponse("run-polled-again", initial)).mockImplementation(async () => runResponse("run-polled-again", terminal));
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(document.querySelector(".run .state")?.textContent).toBe(terminal);
      expect(document.querySelector(".ending-line")?.textContent).toBe(terminal === "failed" ? "Threw — timer threw" : terminal === "cancelled" ? "Cancelled" : "Returned");
      expect(document.body.textContent).not.toContain("This run has not ended yet");
      const calls = fetch.mock.calls.length;
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS * 3);
      expect(fetch).toHaveBeenCalledTimes(calls);
    } finally { vi.useRealTimers(); }
  });

  it("run view retries a failed refresh and receives the terminal account", async () => {
    const fetch = vi.spyOn(globalThis, "fetch").mockImplementation(async () => runResponse("run-polled"));
    try {
      await startRunView();
      window.history.pushState({}, "", "/workflow-runs/run-retry");
      window.dispatchEvent(new PopStateEvent("popstate"));
      fetch.mockReset().mockResolvedValueOnce(runResponse("run-retry")).mockRejectedValueOnce(new Error("refresh unavailable")).mockImplementation(async () => runResponse("run-retry", "completed"));
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(document.querySelector('.run [role="alert"]')?.textContent ?? "").toContain("refresh unavailable");
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(document.querySelector(".ending-line")?.textContent).toBe("Returned");
    } finally { vi.useRealTimers(); }
  });

  it("run view drops an old route's pending read and clears its timer", async () => {
    let release!: (response: Response) => void;
    const fetch = vi.spyOn(globalThis, "fetch").mockImplementation(async () => runResponse("run-polled"));
    try {
      await startRunView();
      window.history.pushState({}, "", "/workflow-runs/run-pending");
      window.dispatchEvent(new PopStateEvent("popstate"));
      fetch.mockReset().mockResolvedValueOnce(runResponse("run-pending")).mockImplementationOnce(() => new Promise<Response>((resolve) => { release = resolve; })).mockImplementation(async () => runResponse("new-run", "completed"));
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(fetch).toHaveBeenCalledTimes(2);
      window.history.pushState({}, "", "/workflow-runs/new-run");
      window.dispatchEvent(new PopStateEvent("popstate"));
      await vi.advanceTimersByTimeAsync(0);
      release(runResponse("run-pending"));
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS * 3);
      expect(document.querySelector(".run .identity")?.textContent).toContain("new-run");
      expect(document.querySelector(".run .state")?.textContent).toBe("completed");
      expect(fetch).toHaveBeenCalledTimes(3);
    } finally { vi.useRealTimers(); }
  });

  it("run view stops refreshing after unmount even with a read pending", async () => {
    let release!: (response: Response) => void;
    const fetch = vi.spyOn(globalThis, "fetch").mockImplementation(async () => runResponse("run-polled"));
    try {
      await startRunView();
      window.history.pushState({}, "", "/workflow-runs/run-unmounted");
      window.dispatchEvent(new PopStateEvent("popstate"));
      fetch.mockReset().mockResolvedValueOnce(runResponse("run-unmounted")).mockImplementationOnce(() => new Promise<Response>((resolve) => { release = resolve; })).mockImplementation(async () => runResponse("run-unmounted"));
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS);
      expect(fetch).toHaveBeenCalledTimes(2);
      await unmount(component!); component = undefined;
      release(runResponse("run-unmounted"));
      await vi.advanceTimersByTimeAsync(UNSETTLED_RUN_REFRESH_MS * 3);
      expect(fetch).toHaveBeenCalledTimes(2);
    } finally { vi.useRealTimers(); }
  });

});
