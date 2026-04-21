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
import * as projectsApi from "../lib/api/projects";
import type {
  AppLens,
  ClientEvent,
  ConnectionStatus,
  ConversationDetail,
  ConversationResponse,
  ConversationSummary,
  LensRuntimeResponse,
  OperatorOverview,
  Project,
  ServerEvent,
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

vi.mock("../lib/api/projects", () => ({
  listProjects: vi.fn(),
  createProject: vi.fn(),
  deleteProject: vi.fn(),
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
  listProjectExtensions: vi.fn(),
  createProjectExtension: vi.fn(),
  updateProjectExtension: vi.fn(),
  approveWorkspaceExtension: vi.fn(),
  listSharedReferences: vi.fn(),
}));

vi.mock("../lib/api/materials", () => ({
  listProjectMaterials: vi.fn(),
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

function makeProject(overrides: Partial<Project> = {}): Project {
  return {
    id: "proj-default",
    name: "default",
    slug: "default",
    is_default: true,
    workspace_path: "/tmp/default",
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
    project_id: "proj-default",
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
        project_id: "proj-default",
        project_name: "default",
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
    active_lens: activeLens,
    updated_at: "2026-04-07T12:00:00Z",
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
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await settleUi();
    if (document.body.textContent?.includes(text)) {
      return;
    }
  }
}

function setPath(path: string): void {
  window.history.replaceState(null, "", path);
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
    vi.mocked(builderApi.listProjectExtensions).mockResolvedValue([]);
    vi.mocked(builderApi.listSharedReferences).mockResolvedValue([]);
    vi.mocked(materialsApi.listProjectMaterials).mockResolvedValue([]);
    vi.mocked(materialsApi.listSessionMaterials).mockResolvedValue([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOperatorOverview());
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
    const defaultProject = makeProject();
    const linkedConversation = makeConversationSummary({
      id: "conv-linked",
      message_preview: "Deep-linked conversation",
    });

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([defaultProject]);
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

    expect(projectsApi.listProjects).toHaveBeenCalledTimes(1);
    expect(lensApi.fetchLensRuntime).toHaveBeenCalledTimes(1);
    expect(conversationsApi.listConversations).toHaveBeenCalledWith(defaultProject.id);
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

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([makeProject()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);

    component = mount(App, { target: document.body });
    await settleUi();

    expect(document.body.textContent).toContain("Desktop shell");
    expect(document.body.textContent).toContain("Managed gateway");
  });

  it("creates a conversation from the real app wiring and navigates to it", async () => {
    const defaultProject = makeProject();
    const created: ConversationResponse = {
      id: "conv-new",
      project_id: defaultProject.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([defaultProject]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(created);

    component = mount(App, { target: document.body });
    await settleUi();

    const newButton = document.querySelector(".new-btn") as HTMLButtonElement | null;
    expect(newButton).not.toBeNull();

    flushSync(() => newButton!.click());
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledWith(defaultProject.id);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith("conv-new");
    expect(window.location.pathname).toBe("/conversations/conv-new");
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Send a message to start.",
    );
  });

  it("sends a message and finalises a streamed reply through the full app stack", async () => {
    const defaultProject = makeProject();

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([defaultProject]);
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

    const sendButton = document.querySelector(".send-btn") as HTMLButtonElement | null;
    expect(sendButton).not.toBeNull();

    flushSync(() => sendButton!.click());
    await settleUi();

    expect(websocketHarness.client.send).toHaveBeenCalledWith({
      type: "send_message",
      content: "Hello from the app",
    });
    expect(document.querySelector(".message-display")?.textContent).toContain("Hello from the app");
    expect(document.querySelector(".stop-btn")).not.toBeNull();

    websocketHarness.fireEvent({ type: "token", content: "Streamed " });
    websocketHarness.fireEvent({ type: "token", content: "reply." });
    await settleUi();

    expect(document.querySelector(".message-display")?.textContent).toContain("Streamed reply.");

    websocketHarness.fireEvent({
      type: "turn_complete",
      turn_id: "turn-123",
    });
    await settleUi();

    expect(document.querySelector(".stop-btn")).toBeNull();
    expect(document.querySelector(".message-display")?.textContent).toContain("Streamed reply.");
  });

  it("switches to the operator deck and renders the overview", async () => {
    const defaultProject = makeProject();

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([defaultProject]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(
      makeOperatorOverview(),
    );

    component = mount(App, { target: document.body });
    await settleUi();

    const operatorButton = Array.from(
      document.querySelectorAll(".toggle-btn"),
    ).find((button) => button.textContent?.includes("Operator")) as
      | HTMLButtonElement
      | undefined;

    expect(operatorButton).toBeDefined();

    flushSync(() => operatorButton!.click());
    await waitForText("Live control surface for the running gateway");

    expect(operatorApi.fetchOperatorOverview).toHaveBeenCalled();
    expect(document.body.textContent).toContain(
      "Live control surface for the running gateway",
    );
    expect(document.body.textContent).toContain("dev_dm");
    expect(document.body.textContent).toContain("Local machine");
  });

  it("loads the builder lens and renders the builder deck by default", async () => {
    const defaultProject = makeProject({ name: "Workbench" });

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([defaultProject]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValueOnce(
      makeOperatorOverview(),
    );
    vi.mocked(lensApi.fetchLensRuntime).mockResolvedValueOnce(
      makeLensRuntime("builder"),
    );

    component = mount(App, { target: document.body });
    await waitForText("Author the workspace, then let the agent live in it.");

    expect(document.body.textContent).toContain(
      "Author the workspace, then let the agent live in it.",
    );
    expect(document.body.textContent).toContain("Workbench");
    expect(operatorApi.fetchOperatorOverview).toHaveBeenCalledTimes(1);
  });

  it("keeps materials, shared references, and operator pulse visible in the workbench rail", async () => {
    const defaultProject = makeProject({ name: "Workbench" });

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([defaultProject]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-rail", message_preview: "Live thread" }),
    ]);
    vi.mocked(materialsApi.listProjectMaterials).mockResolvedValueOnce([
      {
        id: "mat-1",
        scope_kind: "project",
        session_id: null,
        project_id: defaultProject.id,
        source_kind: "manual",
        source_attachment_id: null,
        status: "active",
        title: "Workspace brief",
        relative_path: "shared/brief.md",
        absolute_path: "/tmp/default/shared/brief.md",
        mime_type: "text/markdown",
        content_text: "brief",
        created_at: "2026-04-07T12:00:00Z",
        updated_at: "2026-04-07T12:01:00Z",
        archived_at: null,
      },
    ]);
    vi.mocked(materialsApi.listSessionMaterials).mockResolvedValueOnce([]);
    vi.mocked(builderApi.listSharedReferences).mockResolvedValueOnce([
      {
        kind: "team_topology",
        id: "default",
        title: "Workspace team",
        subtitle: "default lead",
        updated_at: "2026-04-07T12:01:00Z",
      },
    ]);

    component = mount(App, { target: document.body });
    await waitForText("Capture material");

    expect(document.body.textContent).toContain("Capture material");
    expect(document.body.textContent).toContain("Workspace brief");
    expect(document.body.textContent).toContain("Workspace team");
    expect(document.body.textContent).toContain("Operator pulse");
  });

  it("persists a lens switch and changes the available primary surface", async () => {
    const defaultProject = makeProject();

    vi.mocked(projectsApi.listProjects).mockResolvedValueOnce([defaultProject]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);

    component = mount(App, { target: document.body });
    await settleUi();

    const builderButton = Array.from(
      document.querySelectorAll(".toggle-btn"),
    ).find((button) => button.textContent?.includes("Builder")) as
      | HTMLButtonElement
      | undefined;

    expect(builderButton).toBeDefined();

    flushSync(() => builderButton!.click());
    await waitForText("Author the workspace, then let the agent live in it.");

    expect(lensApi.updateLensRuntime).toHaveBeenCalledWith("builder");
    expect(document.body.textContent).toContain("Builder deck");
    expect(document.body.textContent).toContain(
      "Author the workspace, then let the agent live in it.",
    );
  });
});
