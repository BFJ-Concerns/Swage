// @vitest-environment jsdom
/** Behavioural coverage for the shell's conversation, operator, and settings surfaces. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import AppLayout from "../components/AppLayout.svelte";
import { parseRoute } from "../router";
import type { ConversationStore } from "../stores/conversations";
import type { ConnectionStore } from "../stores/connection";
import type { SpaceStore } from "../stores/spaces";
import type { StreamingStore } from "../stores/streaming";

import * as operatorApi from "../api/operator";

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  fetchScheduleRuns: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

function mockConversationStore(): ConversationStore {
  return {
    get conversations() { return []; },
    get activeConversation() { return null; },
    get isLoading() { return false; },
    loadConversations: async () => {},
    selectConversation: async () => {},
    createConversation: async () => {},
    deleteConversation: async () => {},
    appendOptimisticUserMessage: () => {},
    appendAssistantMessage: () => {},
    clearActiveConversation: () => {},
    refreshActiveConversation: async () => {},
  };
}

function mockConnectionStore(): ConnectionStore {
  return {
    get status() { return "disconnected" as const; },
    get errorMessage() { return null; },
    connectTo: () => {},
    disconnect: () => {},
    sendMessage: () => {},
    cancelTurn: () => {},
    onServerEvent: () => {},
  };
}

function mockStreamingStore(): StreamingStore {
  return {
    get isStreaming() { return false; },
    get streamedContent() { return ""; },
    get turnError() { return null; },
    get turnStartedAt() { return null; },
    get activeToolCalls() { return []; },
    get providerRetry() { return null; },
    get pendingApproval() { return null; },
    handleServerEvent: () => {},
    startTurn: () => {},
    cancelTurn: () => {},
  };
}

function mockSpaceStore(hasCurrentSpace = true): SpaceStore {
  const space = {
    id: "default",
    name: "default",
    description: "",
    is_default: true,
    slug: "default",
    created_at: "",
  };

  return {
    get spaces() { return hasCurrentSpace ? [space] : []; },
    get currentSpace() { return hasCurrentSpace ? space : null; },
    get recentSpaces() { return []; },
    get isLoading() { return false; },
    loadSpaces: async () => {},
    selectSpace: async () => {},
    closeSpace: () => {},
    createSpace: async () => {},
    deleteSpace: async () => {},
  };
}

interface MountOptions {
  activeSurface?: "conversation" | "operator";
  hasCurrentSpace?: boolean;
  onNavigate?: (path: string) => void;
  onSelectSurface?: (surface: "conversation" | "operator") => void;
  routePath?: string;
}

function mountLayout(options: MountOptions = {}): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: mockStreamingStore(),
      spaceStore: mockSpaceStore(options.hasCurrentSpace ?? true),
      onRetry: () => {},
      activeSurface: options.activeSurface ?? "conversation",
      onSelectSurface: options.onSelectSurface ?? (() => {}),
      onNavigate: options.onNavigate,
      route: options.routePath ? parseRoute(options.routePath) : undefined,
    },
  });
}

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

describe("AppLayout shell navigation", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    window.localStorage.clear();
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue({
      gateway: {
        status: "running",
        health: "healthy",
        shutdown_requested: false,
        seconds_since_activity: 0,
        active_turn_count: 0,
        active_connection_count: 0,
      },
      sessions: [],
      approvals: [],
      schedules: { schedules: [] },
      channels: { channels: [] },
      devices: [],
    });
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.replaceChildren();
  });

  it("shows the space picker before a space is selected", () => {
    component = mountLayout({ hasCurrentSpace: false });

    expect(document.querySelector("h1")?.textContent).toBe("Choose where to work.");
    expect(document.querySelector('nav[aria-label="Surface"]')).toBeNull();
  });

  it("renders the conversation surface with its list, messages, and composer", () => {
    component = mountLayout();

    expect(document.querySelector(".conversation-list")).not.toBeNull();
    expect(document.querySelector('[aria-label="Conversation messages"]')).not.toBeNull();
    expect(document.querySelector('textarea[aria-label="Message"]')).not.toBeNull();
  });

  it("renders the space switcher in the shell", () => {
    component = mountLayout();

    expect(document.querySelector('button[title="Switch space"]')?.textContent).toContain(
      "Switch — default",
    );
  });

  it("marks conversation as the selected surface", () => {
    component = mountLayout();

    expect(surfaceButton("Conversation").getAttribute("aria-pressed")).toBe("true");
    expect(surfaceButton("Operator").getAttribute("aria-pressed")).toBe("false");
  });

  it("requests the operator surface from its labelled control", () => {
    const onSelectSurface = vi.fn();
    component = mountLayout({ onSelectSurface });

    flushSync(() => surfaceButton("Operator").click());

    expect(onSelectSurface).toHaveBeenCalledWith("operator");
  });

  it("renders the operator dashboard when that surface is selected", () => {
    component = mountLayout({ activeSurface: "operator" });

    expect(document.querySelector("main.operator h1")?.textContent).toContain(
      "Live control surface for the running gateway",
    );
    expect(surfaceButton("Operator").getAttribute("aria-pressed")).toBe("true");
  });

  it("navigates to global settings from the surface controls", () => {
    const onNavigate = vi.fn();
    component = mountLayout({ onNavigate });

    flushSync(() => surfaceButton("Settings").click());

    expect(onNavigate).toHaveBeenCalledWith("/settings/providers");
  });

  it("renders settings and returns to the shell", () => {
    const onNavigate = vi.fn();
    component = mountLayout({ onNavigate, routePath: "/settings/providers" });

    expect(document.querySelector('[aria-label="Settings"]')).not.toBeNull();
    flushSync(() =>
      (document.querySelector('[aria-label="Close settings"]') as HTMLButtonElement).click(),
    );

    expect(onNavigate).toHaveBeenCalledWith("/");
  });

  it("opens global settings without a selected space", () => {
    const onNavigate = vi.fn();
    component = mountLayout({
      hasCurrentSpace: false,
      onNavigate,
    });

    const settingsButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Settings",
    );
    expect(settingsButton).toBeInstanceOf(HTMLButtonElement);

    flushSync(() => settingsButton?.click());

    expect(onNavigate).toHaveBeenCalledWith("/settings/providers");
  });

  it("renders global settings without a selected space", () => {
    component = mountLayout({
      hasCurrentSpace: false,
      routePath: "/settings/providers",
    });

    expect(document.querySelector('[aria-label="Settings"]')).not.toBeNull();
    expect(document.body.textContent).not.toContain("Choose where to work.");
  });
});
