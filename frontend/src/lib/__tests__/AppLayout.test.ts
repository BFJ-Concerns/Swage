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
import * as builderApi from "../api/builder";

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseScheduledJob: vi.fn(),
  resumeScheduledJob: vi.fn(),
  disableHeartbeat: vi.fn(),
  enableHeartbeat: vi.fn(),
}));

vi.mock("../api/builder", () => ({
  createBuilderProposal: vi.fn(),
  applyBuilderProposal: vi.fn(),
  listTeams: vi.fn(),
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  listBenchExtensions: vi.fn(),
  createBenchExtension: vi.fn(),
  updateBenchExtension: vi.fn(),
  approveWorkspaceExtension: vi.fn(),
  listSharedReferences: vi.fn(),
  getSharedReference: vi.fn(),
  createSharedReferenceEvent: vi.fn(),
  updateTeamTopologyReference: vi.fn(),
  isTeamTopologyPayload: (payload: Record<string, unknown>) => "nodes" in payload,
  isContextSnapshotPayload: (payload: Record<string, unknown>) => "elements" in payload,
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
    get providerRetry() { return null; },
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
      return [{
        id: "default",
        name: "default",
        description: "",
        is_default: true,
        slug: "default",
        created_at: "",
      }];
    },
    get currentBench() {
      return {
        id: "default",
        name: "default",
        description: "",
        is_default: true,
        slug: "default",
        created_at: "",
      };
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
      scheduled_jobs: { jobs: [] },
      heartbeats: { heartbeats: [] },
      channels: { channels: [] },
      nodes: [],
    });
    vi.mocked(builderApi.listTeams).mockResolvedValue([
      {
        id: "research-lead",
        bench_id: "default",
        name: "research-lead",
        description: "Research coordination team",
        entry_agent_id: "lead",
        agents: [
          {
            id: "lead",
            display_name: "Research Lead",
            description: "Keeps research moving.",
            instructions: "Coordinate research.",
            model: "claude-sonnet-4-20250514",
          },
        ],
        members: ["lead"],
        tasks: [],
        created_at: "2026-04-18T10:00:00Z",
        updated_at: "2026-04-18T10:00:00Z",
      },
    ]);
    vi.mocked(builderApi.listSharedReferences).mockResolvedValue([]);
    vi.mocked(builderApi.listBenchExtensions).mockResolvedValue([]);
    vi.mocked(builderApi.createBuilderProposal).mockResolvedValue({
      summary: "No changes proposed.",
      operations: [],
      diagnostics: [],
      can_apply: false,
    });
    vi.mocked(builderApi.applyBuilderProposal).mockResolvedValue({
      status: 200,
      body: { success: true, summary: "Applied.", diagnostics: [], applied: [] },
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
    // The stage pane sits next to the conversation pane as a secondary rail.
    expect(document.querySelector(".stage-pane")).not.toBeNull();
    expect(document.querySelector(".body.conversation-dominant")).not.toBeNull();
  });

  it("keeps conversation as the primary pane when a stage is present", async () => {
    component = mountBuilderLayout();
    await waitForText("Author the workspace, then let the agent live in it.");

    const body = document.querySelector(".body.conversation-dominant");
    expect(body).not.toBeNull();
    expect(body!.firstElementChild?.classList.contains("conversation-pane")).toBe(true);
    expect(body!.lastElementChild?.classList.contains("stage-pane")).toBe(true);
  });

  it("selects and persists the builder mode from the lens menu", async () => {
    component = mountBuilderLayout();
    await waitForText("Author the workspace, then let the agent live in it.");

    (document.querySelector(".lens-caret") as HTMLButtonElement).click();
    await flushUi();
    (
      Array.from(document.querySelectorAll(".builder-pop-item")).find((button) =>
        button.textContent?.includes("Lens"),
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(window.localStorage.getItem("swage.builder.activeMode")).toBe("lens");
    expect(document.body.textContent).toContain("Builder mode Lens");
    expect(document.body.textContent).toContain("Lens catalogue");
    expect(document.body.textContent).not.toContain("Entry team");
  });

  it("renders the operator stage when the research lens is active", async () => {
    component = mountResearchLayout();
    await waitForText("Live control surface for the running gateway");

    expect(document.body.textContent).toContain("Live control surface for the running gateway");
    expect(document.querySelector(".stage-pane")).not.toBeNull();
  });

  it("renders placed stage modules from the lens runtime", async () => {
    component = mount(AppLayout, {
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
            summary: "Conversation-first research workspace",
            default_surface: "conversation",
            ui_modules: ["conversation", "operator"],
            prominent_team_ids: ["research-lead"],
            source_kind: "installed",
          },
        ],
        stageModules: [
          {
            id: "research:shared-reference",
            kind: "shared_reference",
            title: "Shared references",
            team_ids: ["research-lead"],
          },
          {
            id: "research:team-topology",
            kind: "active_team_topology",
            title: "Active team topology",
            team_ids: ["research-lead"],
          },
        ],
        activeSurface: "conversation",
        onSelectLens: () => {},
        onRefreshLenses: () => {},
        onSelectSurface: () => {},
      },
    });

    await waitForText("Shared references");
    await waitForText("research-lead");

    expect(document.body.textContent).toContain("Active team topology");
    expect(document.body.textContent).toContain("Live");
    expect(document.body.textContent).not.toContain("SHELL");
    expect(builderApi.listTeams).toHaveBeenCalledWith("default");
    expect(builderApi.listSharedReferences).toHaveBeenCalledWith("default");
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

  it("keeps global settings reachable when no bench is selected", () => {
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
    const onNavigate = vi.fn();
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
