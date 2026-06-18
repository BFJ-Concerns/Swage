// @vitest-environment jsdom
/**
 * App-level integration tests.
 *
 * These tests mount the real root component and let it wire the actual stores,
 * router, and child components together. Only the network boundary is mocked:
 * HTTP API clients and the WebSocket client factory.
 *
 * The goal is to prove user journeys rather than isolated implementation details:
 *   - startup restores a deep-linked conversation
 *   - creating a conversation updates URL and connection state
 *   - sending a message flows through optimistic UI, streaming, and completion
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import App from "../App.svelte";
import * as builderApi from "../lib/api/builder";
import * as conversationsApi from "../lib/api/conversations";
import * as lensApi from "../lib/api/lens";
import * as materialsApi from "../lib/api/materials";
import * as operatorApi from "../lib/api/operator";
import * as benchesApi from "../lib/api/benches";
import type {
  AppLens,
  ClientEvent,
  ConnectionStatus,
  ConversationDetail,
  ConversationResponse,
  ConversationSummary,
  LensRuntimeResponse,
  OperatorOverview,
  Bench,
  ServerEvent,
  WorkspaceAttachment,
} from "../lib/types";

type MountedComponent = ReturnType<typeof mount>;

const websocketHarness = vi.hoisted(() => {
  let eventHandler: ((event: ServerEvent) => void) | null = null;
  let statusHandler: ((status: ConnectionStatus) => void) | null = null;

  const client = {
    connect: vi.fn(),
    send: vi.fn<(event: ClientEvent) => void>(),
    disconnect: vi.fn(),
    onEvent: vi.fn((handler: (event: ServerEvent) => void) => {
      eventHandler = handler;
    }),
    onStatusChange: vi.fn((handler: (status: ConnectionStatus) => void) => {
      statusHandler = handler;
    }),
  };

  return {
    client,
    factory: vi.fn(() => client),
    fireEvent(event: ServerEvent) {
      eventHandler?.(event);
    },
    fireStatus(status: ConnectionStatus) {
      statusHandler?.(status);
    },
    reset() {
      eventHandler = null;
      statusHandler = null;
      client.connect.mockReset();
      client.send.mockReset();
      client.disconnect.mockReset();
      client.onEvent.mockClear();
      client.onStatusChange.mockClear();
      this.factory.mockClear();
    },
  };
});

vi.mock("../lib/api/benches", () => ({
  listBenches: vi.fn(),
  createBench: vi.fn(),
  updateBench: vi.fn(),
  deleteBench: vi.fn(),
  listBenchWorkspaces: vi.fn(),
  attachBenchWorkspace: vi.fn(),
  detachBenchWorkspace: vi.fn(),
  listBenchSessions: vi.fn(),
}));

vi.mock("../lib/api/conversations", () => ({
  listConversations: vi.fn(),
  createConversation: vi.fn(),
  getConversation: vi.fn(),
  deleteConversation: vi.fn(),
}));

vi.mock("../lib/api/lens", () => ({
  fetchLensRuntime: vi.fn(),
  updateLensRuntime: vi.fn(),
}));

vi.mock("../lib/api/builder", () => ({
  listTeams: vi.fn(),
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  listBenchExtensions: vi.fn(),
  createBenchExtension: vi.fn(),
  updateBenchExtension: vi.fn(),
  approveWorkspaceExtension: vi.fn(),
  listSharedReferences: vi.fn(),
}));

vi.mock("../lib/api/materials", () => ({
  listBenchMaterials: vi.fn(),
  listSessionMaterials: vi.fn(),
  createMaterial: vi.fn(),
}));

vi.mock("../lib/api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseScheduledJob: vi.fn(),
  resumeScheduledJob: vi.fn(),
  disableHeartbeat: vi.fn(),
  enableHeartbeat: vi.fn(),
}));

vi.mock("../lib/api/websocket", () => ({
  createWebSocketClient: websocketHarness.factory,
}));

function makeBench(overrides: Partial<Bench> = {}): Bench {
  return {
    id: "proj-default",
    name: "default",
    description: "",
    slug: "default",
    is_default: true,
    created_at: "2026-04-07T12:00:00Z",
    ...overrides,
  };
}

function makeWorkspaceAttachment(
  overrides: Partial<WorkspaceAttachment> = {},
): WorkspaceAttachment {
  return {
    id: "workspace-1",
    bench_id: "proj-default",
    label: "Codebase",
    path: "/missing/codebase",
    canonical_path: "/missing/codebase",
    is_missing: false,
    is_default: true,
    permissions: ["read", "write"],
    is_trusted: true,
    created_at: "2026-04-07T12:00:00Z",
    ...overrides,
  };
}

function makeConversationSummary(
  overrides: Partial<ConversationSummary> = {},
): ConversationSummary {
  return {
    id: "conv-1",
    created_at: "2026-04-07T12:00:00Z",
    last_activity: "2026-04-07T12:00:00Z",
    message_preview: "Earlier message",
    has_interrupted_turn: false,
    ...overrides,
  };
}

function makeConversationDetail(
  overrides: Partial<ConversationDetail> = {},
): ConversationDetail {
  return {
    id: "conv-1",
    bench_id: "proj-default",
    created_at: "2026-04-07T12:00:00Z",
    last_activity: "2026-04-07T12:00:00Z",
    messages: [],
    ...overrides,
  };
}

function makeOperatorOverview(
  overrides: Partial<OperatorOverview> = {},
): OperatorOverview {
  return {
    gateway: {
      status: "running",
      health: "healthy",
      shutdown_requested: false,
      seconds_since_activity: 3,
      active_turn_count: 1,
      active_connection_count: 1,
    },
    sessions: [
      {
        id: "conv-ops",
        bench_id: "proj-default",
        bench_name: "default",
        created_at: "2026-04-07T12:00:00Z",
        last_activity: "2026-04-07T12:04:00Z",
        message_preview: "Operator view",
        has_interrupted_turn: false,
        has_active_connection: true,
        has_running_turn: true,
      },
    ],
    approvals: [],
    scheduled_jobs: { jobs: [] },
    heartbeats: { heartbeats: [] },
    channels: {
      channels: [
        {
          integration: "dev_dm",
          status: "active",
          route_count: 1,
          delivery_count: 2,
          last_delivery_at: "2026-04-07T12:04:00Z",
        },
      ],
    },
    nodes: [
      {
        host_id: "local",
        label: "Local machine",
        kind: "local",
        base_url: null,
        workspace_root: "/tmp/swage",
        capabilities: [],
        registered_at: "2026-04-07T12:00:00Z",
        state: "connected",
        last_seen_at: null,
        disconnected_at: null,
      },
    ],
    ...overrides,
  };
}

function makeLensRuntime(
  activeLens: AppLens = "assistant",
): LensRuntimeResponse {
  return {
    bench_id: "proj-default",
    active_lens: activeLens,
    updated_at: "2026-04-07T12:00:00Z",
    stage_modules: [],
    available_lenses: [
      {
        id: "assistant",
        title: "Assistant lens",
        summary: "Conversation-first view",
        default_surface: "conversation",
        ui_modules: ["conversation", "operator"],
        prominent_team_ids: ["default"],
        source_kind: "built_in",
      },
      {
        id: "builder",
        title: "Builder lens",
        summary: "Workspace-first view",
        default_surface: "builder",
        ui_modules: ["builder", "conversation"],
        prominent_team_ids: ["workspace-core"],
        source_kind: "built_in",
      },
      {
        // Operator-led lens — the new shell reaches the operator surface by
        // selecting a lens whose default_surface is "operator", rather than
        // by switching surface tabs within a lens.
        id: "operator",
        title: "Operator lens",
        summary: "Operator-first view",
        default_surface: "operator",
        ui_modules: ["operator", "conversation"],
        prominent_team_ids: [],
        source_kind: "built_in",
      },
    ],
  };
}

async function settleUi(): Promise<void> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function waitForText(text: string): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    await settleUi();
    if (document.body.textContent?.includes(text)) {
      return;
    }
  }

  throw new Error(`Timed out waiting for text: ${text}`);
}

function setPath(path: string): void {
  window.history.replaceState(null, "", path);
}

/**
 * Open a bench from the picker. Swage lands on the bench picker on entry, so
 * tests that need a bench open must go through it. Clicking a card's Open
 * button triggers `openBench` in BenchPicker.
 */
async function openBenchFromPicker(name: string): Promise<void> {
  const card = Array.from(
    document.querySelectorAll<HTMLElement>(".bench-card"),
  ).find((el) => el.textContent?.includes(name));

  if (!card) {
    throw new Error(`Bench card with name "${name}" not found in picker`);
  }

  const openBtn = card.querySelector<HTMLButtonElement>(".open-btn");
  if (!openBtn) {
    throw new Error(`Open button missing on bench card "${name}"`);
  }

  flushSync(() => openBtn.click());
  await settleUi();
}

describe("App", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    websocketHarness.reset();
    vi.clearAllMocks();
    setPath("/");
    delete window.swageDesktop;
    vi.mocked(lensApi.fetchLensRuntime).mockResolvedValue(makeLensRuntime());
    vi.mocked(lensApi.updateLensRuntime).mockResolvedValue(makeLensRuntime("builder"));
    vi.mocked(builderApi.listTeams).mockResolvedValue([]);
    vi.mocked(builderApi.listBenchExtensions).mockResolvedValue([]);
    vi.mocked(builderApi.listSharedReferences).mockResolvedValue([]);
    vi.mocked(materialsApi.listBenchMaterials).mockResolvedValue([]);
    vi.mocked(materialsApi.listSessionMaterials).mockResolvedValue([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOperatorOverview());
    // BenchPicker fetches workspace/session detail on card selection — return
    // empty arrays by default so the picker renders without errors. Individual
    // tests can override these mocks if they exercise the detail panel.
    vi.mocked(benchesApi.listBenchWorkspaces).mockResolvedValue([]);
    vi.mocked(benchesApi.listBenchSessions).mockResolvedValue([]);
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }

    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }

    setPath("/");
  });

  it("restores a deep-linked conversation on startup and opens its WebSocket", async () => {
    const defaultBench = makeBench();
    const linkedConversation = makeConversationSummary({
      id: "conv-linked",
      message_preview: "Deep-linked conversation",
    });

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([defaultBench]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-other", message_preview: "Other" }),
      linkedConversation,
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({
        id: "conv-linked",
        messages: [
          {
            role: "assistant",
            content: "Restored from URL",
            timestamp: "2026-04-07T12:01:00Z",
            turn_state: "completed",
          },
        ],
      }),
    );

    setPath("/conversations/conv-linked");
    component = mount(App, { target: document.body });
    await settleUi();

    expect(benchesApi.listBenches).toHaveBeenCalledTimes(1);
    expect(lensApi.fetchLensRuntime).toHaveBeenCalledTimes(1);
    expect(conversationsApi.listConversations).toHaveBeenCalledWith(defaultBench.id);
    expect(conversationsApi.getConversation).toHaveBeenCalledWith("conv-linked");
    expect(websocketHarness.client.connect).toHaveBeenCalledWith("conv-linked");
    expect(window.location.pathname).toBe("/conversations/conv-linked");

    const activePreview = document.querySelector(".conversation-item.active .preview");
    expect(activePreview?.textContent).toContain("Deep-linked conversation");
    expect(document.querySelector(".message-display")?.textContent).toContain("Restored from URL");
  });

  it("shows desktop shell context when the electron bridge is present", async () => {
    window.swageDesktop = {
      shell: "electron",
      mode: "desktop",
      gatewayUrl: "http://127.0.0.1:8787",
      managedGateway: true,
      versions: {
        chrome: "136.0.0.0",
        electron: "36.0.0",
        node: "22.0.0",
      },
    };

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([makeBench()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);

    component = mount(App, { target: document.body });
    await settleUi();
    // The app lands on the bench picker — open the default bench so the
    // top bar (which holds the desktop shell stamp) renders.
    await openBenchFromPicker("default");

    // The riso shell condenses runtime context into a top-bar "stamp": the
    // managed/remote token replaces the older verbose "Desktop shell" panel.
    const stamp = document.querySelector(".topbar .stamp");
    expect(stamp).not.toBeNull();
    expect(stamp!.textContent).toContain("MANAGED");
  });

  it("creates a conversation from the real app wiring and navigates to it", async () => {
    const defaultBench = makeBench();
    const created: ConversationResponse = {
      id: "conv-new",
      bench_id: defaultBench.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([defaultBench]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(created);

    component = mount(App, { target: document.body });
    await settleUi();
    // Open the bench so ConversationList (and its "New" button) renders.
    await openBenchFromPicker(defaultBench.name);

    const newButton = document.querySelector(".new-btn") as HTMLButtonElement | null;
    expect(newButton).not.toBeNull();

    flushSync(() => newButton!.click());
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledWith(defaultBench.id, undefined);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith("conv-new");
    expect(window.location.pathname).toBe("/conversations/conv-new");
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Send a message to start.",
    );
  });

  it("sends a message and finalises a streamed reply through the full app stack", async () => {
    const defaultBench = makeBench();

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([defaultBench]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({
        id: "conv-stream",
        message_preview: "Earlier message",
      }),
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: "conv-stream" }),
    );

    setPath("/conversations/conv-stream");
    component = mount(App, { target: document.body });
    await settleUi();

    websocketHarness.fireStatus("connected");

    const textarea = document.querySelector("textarea") as HTMLTextAreaElement | null;
    expect(textarea).not.toBeNull();

    flushSync(() => {
      textarea!.value = "Hello from the app";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const sendButton = document.querySelector(".action.send") as HTMLButtonElement | null;
    expect(sendButton).not.toBeNull();

    flushSync(() => sendButton!.click());
    await settleUi();

    expect(websocketHarness.client.send).toHaveBeenCalledWith({
      type: "send_message",
      content: "Hello from the app",
    });
    expect(document.querySelector(".message-display")?.textContent).toContain("Hello from the app");
    expect(document.querySelector(".action.stop")).not.toBeNull();

    websocketHarness.fireEvent({ type: "token", content: "Streamed " });
    websocketHarness.fireEvent({ type: "token", content: "reply." });
    await settleUi();

    expect(document.querySelector(".message-display")?.textContent).toContain("Streamed reply.");

    websocketHarness.fireEvent({
      type: "turn_complete",
      turn_id: "turn-123",
    });
    await settleUi();

    expect(document.querySelector(".action.stop")).toBeNull();
    expect(document.querySelector(".message-display")?.textContent).toContain("Streamed reply.");
  });

  it("switches to the operator deck and renders the overview", async () => {
    const defaultBench = makeBench();

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([defaultBench]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(
      makeOperatorOverview(),
    );
    // Override the default beforeEach mock (which returns "builder") so
    // selecting the Operator lens lands us in operator mode.
    vi.mocked(lensApi.updateLensRuntime).mockResolvedValueOnce(
      makeLensRuntime("operator"),
    );

    component = mount(App, { target: document.body });
    await settleUi();
    // Open the bench so the lens tabs render in the top bar.
    await openBenchFromPicker(defaultBench.name);

    // Lens tabs sit in the top bar with class .lens. We look for the lens
    // whose title resolves to "Operator" (after stripping the trailing "lens"
    // word in AppLayout).
    const operatorLens = Array.from(
      document.querySelectorAll(".topbar .lens"),
    ).find((button) => button.textContent?.toLowerCase().includes("operator")) as
      | HTMLButtonElement
      | undefined;

    expect(operatorLens).toBeDefined();

    flushSync(() => operatorLens!.click());
    await waitForText("Live control surface for the running gateway");

    expect(operatorApi.fetchOperatorOverview).toHaveBeenCalled();
    expect(document.body.textContent).toContain(
      "Live control surface for the running gateway",
    );
    expect(document.body.textContent).toContain("dev_dm");
    expect(document.body.textContent).toContain("Local machine");
  });

  it("loads the builder lens and renders the builder deck by default", async () => {
    const defaultBench = makeBench({ name: "Workbench" });

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([defaultBench]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(
      makeOperatorOverview(),
    );
    vi.mocked(lensApi.fetchLensRuntime).mockResolvedValueOnce(
      makeLensRuntime("builder"),
    );

    component = mount(App, { target: document.body });
    await settleUi();
    // Open the bench so the builder lens runtime is fetched and the deck
    // gets placed on the stage.
    await openBenchFromPicker(defaultBench.name);
    await waitForText("Author the workspace, then let the agent live in it.");

    expect(document.body.textContent).toContain(
      "Author the workspace, then let the agent live in it.",
    );
    // Bench name still surfaces in the bench head.
    expect(document.body.textContent).toContain("Workbench");
    // Operator overview is no longer pre-fetched on mount — the rail that
    // displayed it was dropped. The operator lens loads its own data.
  });

  it("shows missing bench workspaces and relinks them before opening", async () => {
    const defaultBench = makeBench({ name: "Workbench" });
    const missingWorkspace = makeWorkspaceAttachment({ is_missing: true });
    const relinkedWorkspace = makeWorkspaceAttachment({
      path: "/srv/workbench",
      canonical_path: "/srv/workbench",
      is_missing: false,
    });

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([defaultBench]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(benchesApi.listBenchWorkspaces)
      .mockResolvedValueOnce([missingWorkspace])
      .mockResolvedValueOnce([relinkedWorkspace]);
    vi.mocked(benchesApi.detachBenchWorkspace).mockResolvedValueOnce(undefined);
    vi.mocked(benchesApi.attachBenchWorkspace).mockResolvedValueOnce(relinkedWorkspace);

    component = mount(App, { target: document.body });
    await settleUi();

    const card = Array.from(
      document.querySelectorAll<HTMLElement>(".bench-card"),
    ).find((el) => el.textContent?.includes(defaultBench.name));
    expect(card).toBeDefined();
    // Preview/selection is triggered by the bench-name button, not the card
    // container (UI audit f-2 — card is no longer a single interactive widget).
    const nameBtn = card!.querySelector<HTMLButtonElement>(".bench-name-btn");
    expect(nameBtn).toBeDefined();
    flushSync(() => nameBtn!.click());
    await waitForText("Missing folder");

    const relinkInput = document.querySelector<HTMLInputElement>(
      'input[aria-label="Replacement path for Codebase"]',
    );
    expect(relinkInput).toBeDefined();
    flushSync(() => {
      relinkInput!.value = "/srv/workbench";
      relinkInput!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const relinkButton = Array.from(
      document.querySelectorAll<HTMLButtonElement>("button"),
    ).find((button) => button.textContent?.includes("Relink"));
    expect(relinkButton).toBeDefined();
    flushSync(() => relinkButton!.click());
    await waitForText("Workspace relinked.");

    expect(benchesApi.detachBenchWorkspace).toHaveBeenCalledWith(
      defaultBench.id,
      missingWorkspace.id,
    );
    expect(benchesApi.attachBenchWorkspace).toHaveBeenCalledWith(defaultBench.id, {
      path: "/srv/workbench",
      label: "Codebase",
      permissions: ["read", "write"],
      is_trusted: true,
      set_as_default: true,
    });
  });

  // The right-hand WorkbenchRail (materials / shared references / operator
  // pulse) was dropped per Surfaces.md ("Reference rail dropped; shared
  // reference objects sit in the module stage"). The test that exercised the
  // rail's contents is no longer applicable to the riso shell. When stage
  // modules grow to expose those views, equivalent coverage should be added
  // there.

  it("persists a lens switch and changes the available primary surface", async () => {
    const defaultBench = makeBench();

    vi.mocked(benchesApi.listBenches).mockResolvedValueOnce([defaultBench]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);

    component = mount(App, { target: document.body });
    await settleUi();
    // Open the bench so the lens tabs render in the top bar.
    await openBenchFromPicker(defaultBench.name);

    const builderLens = Array.from(
      document.querySelectorAll(".topbar .lens"),
    ).find((button) => button.textContent?.toLowerCase().includes("builder")) as
      | HTMLButtonElement
      | undefined;

    expect(builderLens).toBeDefined();

    flushSync(() => builderLens!.click());
    await waitForText("Author the workspace, then let the agent live in it.");

    expect(lensApi.updateLensRuntime).toHaveBeenCalledWith("proj-default", "builder");
    expect(document.body.textContent).toContain(
      "Author the workspace, then let the agent live in it.",
    );
  });
});
