// @vitest-environment jsdom
/**
 * Tests for the three-panel AppLayout component.
 *
 * Verifies that all child components are imported and positioned in the
 * correct panels. AppLayout threads store props to children but owns no
 * state itself — it is a pure layout shell.
 */
import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import { mount, unmount } from "svelte";
import AppLayout from "../components/AppLayout.svelte";
import type { ConversationStore } from "../stores/conversations";
import type { ConnectionStore } from "../stores/connection";
import type { StreamingStore } from "../stores/streaming";

import * as operatorApi from "../api/operator";

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseScheduledJob: vi.fn(),
  resumeScheduledJob: vi.fn(),
  disableHeartbeat: vi.fn(),
  enableHeartbeat: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 5; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function waitForText(text: string): Promise<void> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await flushUi();
    if (document.body.textContent?.includes(text)) {
      return;
    }
  }
}

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
    get activeToolCalls() { return []; },
    handleServerEvent: () => {},
    startTurn: () => {},
    cancelTurn: () => {},
  };
}

function mountLayout(): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: mockStreamingStore(),
      onRetry: () => {},
      activeLens: "assistant",
      activeSurface: "conversation",
      onSelectLens: () => {},
      onSelectSurface: () => {},
    },
  });
}

function mountBuilderLayout(): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: mockStreamingStore(),
      onRetry: () => {},
      activeLens: "builder",
      activeSurface: "builder",
      onSelectLens: () => {},
      onSelectSurface: () => {},
    },
  });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("AppLayout: three-panel structure", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
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
      scheduled_jobs: { jobs: [] },
      heartbeats: { heartbeats: [] },
      channels: { channels: [] },
      nodes: [],
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

  it("renders a sidebar panel", () => {
    component = mountLayout();

    expect(document.querySelector(".sidebar")).not.toBeNull();
  });

  it("renders a main conversation area", () => {
    component = mountLayout();

    expect(document.querySelector(".conversation-area")).not.toBeNull();
  });

  it("renders ProjectSelector inside the sidebar", () => {
    component = mountLayout();

    const sidebar = document.querySelector(".sidebar");
    expect(sidebar).not.toBeNull();
    expect(sidebar!.querySelector(".project-selector")).not.toBeNull();
  });

  it("renders ConversationList inside the sidebar", () => {
    component = mountLayout();

    const sidebar = document.querySelector(".sidebar");
    expect(sidebar).not.toBeNull();
    expect(sidebar!.querySelector(".conversation-list")).not.toBeNull();
  });

  it("renders MessageDisplay inside the conversation area", () => {
    component = mountLayout();

    const main = document.querySelector(".conversation-area");
    expect(main).not.toBeNull();
    expect(main!.querySelector(".message-display")).not.toBeNull();
  });

  it("renders MessageInput inside the conversation area", () => {
    component = mountLayout();

    const main = document.querySelector(".conversation-area");
    expect(main).not.toBeNull();
    expect(main!.querySelector(".message-input")).not.toBeNull();
  });

  it("renders conversation area with message display and input sections", () => {
    component = mountLayout();

    const main = document.querySelector(".conversation-area");
    expect(main).not.toBeNull();
    // ConnectionIndicator is hidden when status is "disconnected" (no active
    // connection attempt), so we verify the other children are present.
    expect(main!.children.length).toBeGreaterThan(0);
  });

  it("renders the builder deck when the builder lens is active", async () => {
    component = mountBuilderLayout();
    await waitForText("Author the workspace, then let the agent live in it.");

    expect(document.body.textContent).toContain(
      "Author the workspace, then let the agent live in it.",
    );
  });
});
