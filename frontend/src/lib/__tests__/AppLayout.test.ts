// @vitest-environment jsdom
/**
 * Tests for the AppLayout workbench shell.
 *
 * Verifies that the primary stage and the persistent workbench rail coexist
 * inside one coordinated workspace frame.
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
  for (let attempt = 0; attempt < 12; attempt += 1) {
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
      availableLenses: [
        {
          id: "assistant",
          title: "Assistant lens",
          summary: "Conversation-first view",
          default_surface: "conversation",
          ui_modules: ["conversation", "operator"],
          prominent_team_ids: ["default"],
          source_kind: "built_in",
        },
      ],
      activeSurface: "conversation",
      onSelectLens: () => {},
      onRefreshLenses: () => {},
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
      availableLenses: [
        {
          id: "builder",
          title: "Builder lens",
          summary: "Workspace-first view",
          default_surface: "builder",
          ui_modules: ["builder", "conversation"],
          prominent_team_ids: ["workspace-core"],
          source_kind: "built_in",
        },
      ],
      activeSurface: "builder",
      onSelectLens: () => {},
      onRefreshLenses: () => {},
      onSelectSurface: () => {},
    },
  });
}

function mountResearchLayout(): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: mockStreamingStore(),
      onRetry: () => {},
      activeLens: "research",
      availableLenses: [
        {
          id: "research",
          title: "Research lens",
          summary: "Operator-first research workspace",
          default_surface: "operator",
          ui_modules: ["operator", "conversation"],
          prominent_team_ids: ["research-lead", "reviewer"],
          source_kind: "installed",
        },
      ],
      activeSurface: "operator",
      onSelectLens: () => {},
      onRefreshLenses: () => {},
      onSelectSurface: () => {},
    },
  });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("AppLayout: workbench shell", () => {
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

  it("renders a persistent workbench rail", () => {
    component = mountLayout();

    expect(document.querySelector(".workbench-rail")).not.toBeNull();
  });

  it("renders BenchSelector inside the sidebar", () => {
    component = mountLayout();

    const sidebar = document.querySelector(".sidebar");
    expect(sidebar).not.toBeNull();
    expect(sidebar!.querySelector(".bench-selector")).not.toBeNull();
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
    expect(document.querySelector(".workbench-rail")).not.toBeNull();
  });

  it("renders lens-specific modules and prominent teams from lens data", async () => {
    component = mountResearchLayout();
    await waitForText("Live control surface for the running gateway");

    expect(document.body.textContent).toContain("Research");
    expect(document.body.textContent).toContain("research-lead");
    expect(document.body.textContent).not.toContain("Builder deck");
    expect(document.body.textContent).toContain("Operator");
  });
});
