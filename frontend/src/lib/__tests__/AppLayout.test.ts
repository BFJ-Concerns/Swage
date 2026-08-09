// @vitest-environment jsdom
/**
 * Tests for the AppLayout riso/cyanotype shell.
 *
 * Verifies the new shell structure:
 *   - top bar is present when a space is selected
 *   - left nav (.nav) holds space head + ConversationList
 *   - body holds a .conversation-pane (always) and an optional .stage-pane
 *
 * The right-hand SpaceRail was dropped per Surfaces.md — these tests no
 * longer assert on `.space-rail`.
 */
import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import { mount, unmount } from "svelte";
import AppLayout from "../components/AppLayout.svelte";
import type { ConversationStore } from "../stores/conversations";
import type { ConnectionStore } from "../stores/connection";
import type { StreamingStore } from "../stores/streaming";
import type { SpaceStore } from "../stores/spaces";

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

// ---------------------------------------------------------------------------
// Minimal mock stores — satisfy the interface so AppLayout can thread them.
// ---------------------------------------------------------------------------

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
    get lastInputTokens() { return null; },
    get lastOutputTokens() { return null; },
    get activeToolCalls() { return []; },
    get providerRetry() { return null; },
    get pendingApproval() { return null; },
    handleServerEvent: () => {},
    startTurn: () => {},
    cancelTurn: () => {},
  };
}

// AppLayout collapses the top bar and shows a "pick a space" placeholder when
// no space is selected, so the shell tests need a space in scope.
function mockSpaceStore(): SpaceStore {
  return {
    get spaces() {
      return [{
        id: "default",
        name: "default",
        description: "",
        is_default: true,
        slug: "default",
        created_at: "",
      }];
    },
    get currentSpace() {
      return {
        id: "default",
        name: "default",
        description: "",
        is_default: true,
        slug: "default",
        created_at: "",
      };
    },
    get recentSpaces() { return []; },
    get isLoading() { return false; },
    loadSpaces: async () => {},
    selectSpace: async () => {},
    closeSpace: () => {},
    createSpace: async () => {},
    deleteSpace: async () => {},
  };
}

function mountLayout(): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: mockStreamingStore(),
      spaceStore: mockSpaceStore(),
      onRetry: () => {},
      activeSurface: "conversation",
      onSelectSurface: () => {},
    },
  });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("AppLayout: space shell", () => {
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
    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }
  });

  it("renders the nav panel", () => {
    component = mountLayout();

    expect(document.querySelector(".nav")).not.toBeNull();
  });

  it("renders the conversation pane", () => {
    component = mountLayout();

    expect(document.querySelector(".conversation-pane")).not.toBeNull();
  });

  it("renders SpaceSelector inside the nav", () => {
    component = mountLayout();

    const nav = document.querySelector(".nav");
    expect(nav).not.toBeNull();
    expect(nav!.querySelector(".space-selector")).not.toBeNull();
  });

  it("renders ConversationList inside the nav", () => {
    component = mountLayout();

    const nav = document.querySelector(".nav");
    expect(nav).not.toBeNull();
    expect(nav!.querySelector(".conversation-list")).not.toBeNull();
  });

  it("renders MessageDisplay inside the conversation pane", () => {
    component = mountLayout();

    const pane = document.querySelector(".conversation-pane");
    expect(pane).not.toBeNull();
    expect(pane!.querySelector(".message-display")).not.toBeNull();
  });

  it("renders MessageInput inside the conversation pane", () => {
    component = mountLayout();

    const pane = document.querySelector(".conversation-pane");
    expect(pane).not.toBeNull();
    expect(pane!.querySelector(".message-input")).not.toBeNull();
  });

  it("renders conversation pane with message display and input sections", () => {
    component = mountLayout();

    const pane = document.querySelector(".conversation-pane");
    expect(pane).not.toBeNull();
    // ConnectionIndicator is hidden when status is "disconnected" (no active
    // connection attempt), so we verify the other children are present.
    expect(pane!.children.length).toBeGreaterThan(0);
  });

  it("collapses the top bar when no space is selected", () => {
    // Replace the space-store mock with one that has no current space.
    const noSpaceStore: SpaceStore = {
      get spaces() { return []; },
      get currentSpace() { return null; },
      get recentSpaces() { return []; },
      get isLoading() { return false; },
      loadSpaces: async () => {},
      selectSpace: async () => {},
      closeSpace: () => {},
      createSpace: async () => {},
      deleteSpace: async () => {},
    };
    component = mount(AppLayout, {
      target: document.body,
      props: {
        conversationStore: mockConversationStore(),
        connectionStore: mockConnectionStore(),
        streamingStore: mockStreamingStore(),
        spaceStore: noSpaceStore,
        onRetry: () => {},
        activeSurface: "conversation",
        onSelectSurface: () => {},
      },
    });

    // The window flips into no-topbar mode and the body shows the SpacePicker.
    expect(document.querySelector(".window.no-topbar")).not.toBeNull();
    expect(document.querySelector(".space-grid")).not.toBeNull();
  });

  it("keeps global settings reachable when no space is selected", () => {
    const noSpaceStore: SpaceStore = {
      get spaces() { return []; },
      get currentSpace() { return null; },
      get recentSpaces() { return []; },
      get isLoading() { return false; },
      loadSpaces: async () => {},
      selectSpace: async () => {},
      closeSpace: () => {},
      createSpace: async () => {},
      deleteSpace: async () => {},
    };
    const onNavigate = vi.fn();
    component = mount(AppLayout, {
      target: document.body,
      props: {
        conversationStore: mockConversationStore(),
        connectionStore: mockConnectionStore(),
        streamingStore: mockStreamingStore(),
        spaceStore: noSpaceStore,
        onRetry: () => {},
        activeSurface: "conversation",
        onSelectSurface: () => {},
        onNavigate,
      },
    });

    (
      Array.from(document.querySelectorAll(".footer-item")).find((button) =>
        button.textContent?.includes("Settings"),
      ) as HTMLButtonElement
    ).click();

    expect(onNavigate).toHaveBeenCalledWith("/settings/providers");
  });
});
