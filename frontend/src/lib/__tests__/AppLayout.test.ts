// @vitest-environment jsdom
/**
 * Tests for the AppLayout riso/cyanotype shell.
 *
 * Verifies the new shell structure:
 *   - top bar (lens tabs) is present when a bench is selected
 *   - left nav (.nav) holds bench head + ConversationList
 *   - body holds a .conversation-pane (always) and an optional .stage-pane
 *
 * The right-hand WorkbenchRail was dropped per Surfaces.md — these tests no
 * longer assert on `.workbench-rail`.
 */
import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import { mount, unmount } from "svelte";
import AppLayout from "../components/AppLayout.svelte";
import type { ConversationStore } from "../stores/conversations";
import type { ConnectionStore } from "../stores/connection";
import type { StreamingStore } from "../stores/streaming";
import type { BenchStore } from "../stores/benches";

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
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    await flushUi();
    if (document.body.textContent?.includes(text)) {
      return;
    }
  }

  throw new Error(`Timed out waiting for text: ${text}`);
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
    get turnStartedAt() { return null; },
    get lastInputTokens() { return null; },
    get lastOutputTokens() { return null; },
    get activeToolCalls() { return []; },
    get pendingApproval() { return null; },
    handleServerEvent: () => {},
    startTurn: () => {},
    cancelTurn: () => {},
  };
}

// AppLayout collapses the top bar and shows a "pick a bench" placeholder when
// no bench is selected, so the shell tests need a bench in scope.
function mockBenchStore(): BenchStore {
  return {
    get benches() {
      return [{ id: "default", name: "default", is_default: true, slug: "default", created_at: "" }];
    },
    get currentBench() {
      return { id: "default", name: "default", is_default: true, slug: "default", created_at: "" };
    },
    get recentBenches() { return []; },
    get isLoading() { return false; },
    loadBenches: async () => {},
    selectBench: async () => {},
    createBench: async () => {},
    deleteBench: async () => {},
  };
}

function mountLayout(): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: mockStreamingStore(),
      benchStore: mockBenchStore(),
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
      benchStore: mockBenchStore(),
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
      benchStore: mockBenchStore(),
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

  it("renders the nav panel", () => {
    component = mountLayout();

    expect(document.querySelector(".nav")).not.toBeNull();
  });

  it("renders the conversation pane", () => {
    component = mountLayout();

    expect(document.querySelector(".conversation-pane")).not.toBeNull();
  });

  it("renders the top bar with lens tabs when a bench is selected", () => {
    component = mountLayout();

    const topbar = document.querySelector(".topbar");
    expect(topbar).not.toBeNull();
    expect(topbar!.querySelector(".lens")).not.toBeNull();
  });

  it("renders BenchSelector inside the nav", () => {
    component = mountLayout();

    const nav = document.querySelector(".nav");
    expect(nav).not.toBeNull();
    expect(nav!.querySelector(".bench-selector")).not.toBeNull();
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

  it("renders the builder deck on the stage when the builder lens is active", async () => {
    component = mountBuilderLayout();
    await waitForText("Author the workspace, then let the agent live in it.");

    expect(document.body.textContent).toContain(
      "Author the workspace, then let the agent live in it.",
    );
    // The stage pane sits next to the conversation pane in stage-dominant mode.
    expect(document.querySelector(".stage-pane")).not.toBeNull();
    expect(document.querySelector(".body.stage-dominant")).not.toBeNull();
  });

  it("renders the operator stage when the research lens is active", async () => {
    component = mountResearchLayout();
    await waitForText("Live control surface for the running gateway");

    expect(document.body.textContent).toContain("Live control surface for the running gateway");
    expect(document.querySelector(".stage-pane")).not.toBeNull();
  });

  it("collapses the top bar when no bench is selected", () => {
    // Replace the bench-store mock with one that has no current bench.
    const noBenchStore: BenchStore = {
      get benches() { return []; },
      get currentBench() { return null; },
      get recentBenches() { return []; },
      get isLoading() { return false; },
      loadBenches: async () => {},
      selectBench: async () => {},
      createBench: async () => {},
      deleteBench: async () => {},
    };
    component = mount(AppLayout, {
      target: document.body,
      props: {
        conversationStore: mockConversationStore(),
        connectionStore: mockConnectionStore(),
        streamingStore: mockStreamingStore(),
        benchStore: noBenchStore,
        onRetry: () => {},
        activeLens: "assistant",
        availableLenses: [],
        activeSurface: "conversation",
        onSelectLens: () => {},
        onRefreshLenses: () => {},
        onSelectSurface: () => {},
      },
    });

    // The window flips into no-topbar mode and the body shows the BenchPicker.
    expect(document.querySelector(".window.no-topbar")).not.toBeNull();
    expect(document.querySelector(".bench-grid")).not.toBeNull();
  });
});
