// @vitest-environment jsdom
/**
 * Tests for the AppLayout riso/cyanotype shell.
 *
 * Verifies the new shell structure:
 *   - top bar (lens tabs) is present when a space is selected
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
import * as builderApi from "../api/builder";

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  fetchScheduleRuns: vi.fn(),
}));

vi.mock("../api/builder", () => ({
  createBuilderProposal: vi.fn(),
  applyBuilderProposal: vi.fn(),
  listTeams: vi.fn(),
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  listSpaceExtensions: vi.fn(),
  createSpaceExtension: vi.fn(),
  updateSpaceExtension: vi.fn(),
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
      spaceStore: mockSpaceStore(),
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
      spaceStore: mockSpaceStore(),
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
    vi.mocked(builderApi.listTeams).mockResolvedValue([
      {
        id: "research-lead",
        space_id: "default",
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
    vi.mocked(builderApi.listSpaceExtensions).mockResolvedValue([]);
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

  it("renders the top bar with lens tabs when a space is selected", () => {
    component = mountLayout();

    const topbar = document.querySelector(".topbar");
    expect(topbar).not.toBeNull();
    expect(topbar!.querySelector(".lens")).not.toBeNull();
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

  it("renders the builder deck on the stage when the builder lens is active", async () => {
    component = mountBuilderLayout();
    await waitForText("Author the space, then let the agent live in it.");

    expect(document.body.textContent).toContain(
      "Author the space, then let the agent live in it.",
    );
    // Stage-surface lenses take the wide pane; conversation becomes the rail.
    expect(document.querySelector(".stage-pane")).not.toBeNull();
    expect(document.querySelector(".body.stage-dominant")).not.toBeNull();
  });

  it("gives the stage the primary pane when a stage surface is active", async () => {
    component = mountBuilderLayout();
    await waitForText("Author the space, then let the agent live in it.");

    const body = document.querySelector(".body.stage-dominant");
    expect(body).not.toBeNull();
    expect(body!.firstElementChild?.classList.contains("conversation-pane")).toBe(true);
    expect(body!.lastElementChild?.classList.contains("stage-pane")).toBe(true);
  });

  it("selects and persists the builder mode from the lens menu", async () => {
    component = mountBuilderLayout();
    await waitForText("Author the space, then let the agent live in it.");

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
        spaceStore: mockSpaceStore(),
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
        activeLens: "assistant",
        availableLenses: [],
        activeSurface: "conversation",
        onSelectLens: () => {},
        onRefreshLenses: () => {},
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
