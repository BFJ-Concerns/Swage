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
import * as agentDefinitionsApi from "../lib/api/agent-definitions";
import * as conversationsApi from "../lib/api/conversations";
import * as materialsApi from "../lib/api/materials";
import * as operatorApi from "../lib/api/operator";
import * as settingsApi from "../lib/api/settings";
import * as spacesApi from "../lib/api/spaces";
import type {
  ClientEvent,
  ConnectionStatus,
  ConversationDetail,
  ConversationResponse,
  ConversationSummary,
  OperatorOverview,
  Space,
  ServerEvent,
  WorkspaceAttachment,
  SettingsResponse,
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

vi.mock("../lib/api/spaces", () => ({
  listSpaces: vi.fn(),
  createSpace: vi.fn(),
  updateSpace: vi.fn(),
  deleteSpace: vi.fn(),
  listSpaceWorkspaces: vi.fn(),
  attachSpaceWorkspace: vi.fn(),
  detachSpaceWorkspace: vi.fn(),
  listSpaceSessions: vi.fn(),
}));

vi.mock("../lib/api/conversations", () => ({
  listConversations: vi.fn(),
  createConversation: vi.fn(),
  getConversation: vi.fn(),
  deleteConversation: vi.fn(),
}));

vi.mock("../lib/api/agent-definitions", () => ({
  listEffectiveAgentDefinitions: vi.fn(),
}));

vi.mock("../lib/api/materials", () => ({
  listSpaceMaterials: vi.fn(),
  listSessionMaterials: vi.fn(),
  createMaterial: vi.fn(),
}));

vi.mock("../lib/api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  fetchScheduleRuns: vi.fn(),
}));

vi.mock("../lib/api/settings", () => ({
  discoverModels: vi.fn(),
  fetchSettings: vi.fn(),
  restartGateway: vi.fn(),
  saveSettings: vi.fn(),
}));

vi.mock("../lib/api/websocket", () => ({
  createWebSocketClient: websocketHarness.factory,
}));

function makeSpace(overrides: Partial<Space> = {}): Space {
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
    space_id: "proj-default",
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
    space_id: "proj-default",
    created_at: "2026-04-07T12:00:00Z",
    last_activity: "2026-04-07T12:00:00Z",
    messages: [],
    ...overrides,
  };
}

function makeSettings(models: SettingsResponse["models"] = {}): SettingsResponse {
  const defaultModel = Object.keys(models)[0] ?? "";
  return {
    providers: defaultModel
      ? {
          local: {
            protocol: "openai",
            base_url: "http://provider.test/v1",
            has_credential: true,
            command: null,
            state_dir: null,
          },
        }
      : {},
    default_provider: defaultModel ? "local" : "",
    models,
    default_model: defaultModel,
    generation_presets: {},
    tool_runtime: {
      shell_timeout_secs: 30,
      python_execute: {
        enabled: true,
        timeout_secs: 30,
        startup_timeout_secs: 10,
        idle_ttl_secs: 60,
        warm_target: 1,
        max_workers: 1,
        runtime_policy: "bundled_then_system",
        runtime_path: null,
        require_sandbox: true,
      },
      max_output_bytes: 100_000,
      plugin_directory: null,
      search: null,
    },
    tool_permissions: { allow: ["*"], deny: [], bundles: {}, require_approval: [] },
    hitl: { timeout: 300, timeout_policy: "reject" },
    sandbox: { enabled: true, allow_degraded_execution: false },
    context: { assembly: "default", timeout: 5, budget: null },
    pricing: { models: {} },
    observability: { force_on: false },
    communication: {
      context_transfer: { mode: "none", allowed_fields: [], summary_budget: null, targets: {} },
      message_ttl_secs: 86_400,
      expiry_interval_secs: 60,
    },
    restart_required: false,
    can_restart: false,
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
        space_id: "proj-default",
        space_name: "default",
        created_at: "2026-04-07T12:00:00Z",
        last_activity: "2026-04-07T12:04:00Z",
        message_preview: "Operator view",
        has_interrupted_turn: false,
        has_active_connection: true,
        has_running_turn: true,
      },
    ],
    approvals: [],
    schedules: { schedules: [] },
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
    devices: [
      {
        device_id: "local",
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
 * Open a space from the picker. Swage lands on the space picker on entry, so
 * tests that need a space open must go through it. Clicking a card's Open
 * button triggers `openSpace` in SpacePicker.
 */
async function openSpaceFromPicker(name: string): Promise<void> {
  const card = Array.from(
    document.querySelectorAll<HTMLElement>(".space-card"),
  ).find((el) => el.textContent?.includes(name));

  if (!card) {
    throw new Error(`Space card with name "${name}" not found in picker`);
  }

  const openBtn = card.querySelector<HTMLButtonElement>(".open-btn");
  if (!openBtn) {
    throw new Error(`Open button missing on space card "${name}"`);
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
    vi.mocked(materialsApi.listSpaceMaterials).mockResolvedValue([]);
    vi.mocked(materialsApi.listSessionMaterials).mockResolvedValue([]);
    vi.mocked(agentDefinitionsApi.listEffectiveAgentDefinitions).mockResolvedValue([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(makeOperatorOverview());
    vi.mocked(settingsApi.fetchSettings).mockRejectedValue(
      new Error("settings are not configured in this test"),
    );
    // SpacePicker fetches workspace/session detail on card selection — return
    // empty arrays by default so the picker renders without errors. Individual
    // tests can override these mocks if they exercise the detail panel.
    vi.mocked(spacesApi.listSpaceWorkspaces).mockResolvedValue([]);
    vi.mocked(spacesApi.listSpaceSessions).mockResolvedValue([]);
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

  it("connects exactly once when popstate selects a different conversation", async () => {
    const defaultSpace = makeSpace();
    const first = makeConversationSummary({ id: "conv-first" });
    const second = makeConversationSummary({ id: "conv-second" });

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      first,
      second,
    ]);
    vi.mocked(conversationsApi.getConversation)
      .mockResolvedValueOnce(makeConversationDetail({ id: second.id }))
      .mockResolvedValueOnce(makeConversationDetail({ id: first.id }));

    setPath(`/conversations/${first.id}`);
    window.history.pushState(null, "", `/conversations/${second.id}`);
    const historyLength = window.history.length;
    component = mount(App, { target: document.body });
    await settleUi();

    expect(window.history.length).toBe(historyLength);
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenLastCalledWith(second.id);

    websocketHarness.client.connect.mockClear();
    setPath(`/conversations/${first.id}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    await settleUi();

    expect(window.location.pathname).toBe(`/conversations/${first.id}`);
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(first.id);
  });

  it("restores a deep-linked conversation on startup and opens its WebSocket", async () => {
    const defaultSpace = makeSpace();
    const linkedConversation = makeConversationSummary({
      id: "conv-linked",
      message_preview: "Deep-linked conversation",
    });

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-other", message_preview: "Other" }),
      linkedConversation,
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValue(
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

    window.history.pushState(null, "", "/conversations/conv-linked");
    const historyLength = window.history.length;
    component = mount(App, { target: document.body });
    await settleUi();

    expect(spacesApi.listSpaces).toHaveBeenCalledTimes(1);
    expect(conversationsApi.listConversations).toHaveBeenCalledWith(defaultSpace.id);
    expect(conversationsApi.getConversation).toHaveBeenCalledWith("conv-linked");
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith("conv-linked");
    expect(window.location.pathname).toBe("/conversations/conv-linked");

    const popped = new Promise<void>((resolve) => {
      window.addEventListener("popstate", () => resolve(), { once: true });
    });
    window.history.back();
    await popped;
    await settleUi();

    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(window.location.pathname).toBe("/");
    expect(window.history.length).toBe(historyLength);

    const activePreview = document.querySelector(".conversation-item.active .preview");
    expect(activePreview?.textContent).toContain("Deep-linked conversation");
    expect(document.querySelector(".message-display")?.textContent).toContain("Restored from URL");
  });

  it("keeps the gateway available when startup agent inventory loading fails", async () => {
    const defaultSpace = makeSpace();
    const linkedConversation = makeConversationSummary({ id: "conv-linked" });
    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([linkedConversation]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValue(
      makeConversationDetail({ id: linkedConversation.id }),
    );
    vi.mocked(agentDefinitionsApi.listEffectiveAgentDefinitions).mockRejectedValueOnce(
      new Error("agent inventory unavailable"),
    );
    window.history.replaceState(null, "", `/conversations/${linkedConversation.id}`);

    component = mount(App, { target: document.body });
    await settleUi();

    expect(agentDefinitionsApi.listEffectiveAgentDefinitions).toHaveBeenCalledWith(defaultSpace.id);
    expect(document.querySelector(".conversation-list")).not.toBeNull();
    expect(document.querySelector(".error-screen")).toBeNull();
  });

  it("falls back to the root with a notice for a stale deep link", async () => {
    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([makeSpace()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.getConversation).mockRejectedValueOnce(
      new Error("HTTP 404 from GET /api/conversations/stale-id"),
    );

    window.history.replaceState(null, "", "/conversations/stale-id");
    component = mount(App, { target: document.body });
    await settleUi();

    expect(conversationsApi.getConversation).toHaveBeenCalledWith("stale-id");
    expect(window.location.pathname).toBe("/");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain(
      "Conversation not found",
    );
    expect(websocketHarness.client.connect).not.toHaveBeenCalled();
    await openSpaceFromPicker("default");
    expect(
      document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]'),
    ).not.toBeNull();
  });

  it("shows the not-found notice when popstate restores a deleted conversation", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi.spyOn(window, "addEventListener").mockImplementation(
      (type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions) => {
        if (type === "popstate") {
          popstateHandler =
            typeof listener === "function"
              ? listener
              : (event) => listener.handleEvent(event);
          return;
        }
        addEventListener(type, listener, options);
      },
    );

    const deletedConversation = makeConversationSummary({ id: "deleted-id" });
    const replacementConversation: ConversationResponse = {
      id: "replacement-id",
      space_id: "proj-default",
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };
    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([makeSpace()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      deletedConversation,
    ]);
    vi.mocked(conversationsApi.getConversation)
      .mockResolvedValueOnce(
        makeConversationDetail({
          id: deletedConversation.id,
          messages: [
            {
              role: "assistant",
              content: "Present before server-side deletion",
              timestamp: "2026-04-07T12:01:00Z",
              turn_state: "completed",
            },
          ],
        }),
      )
      .mockRejectedValueOnce(
        new Error("HTTP 404 from GET /api/conversations/deleted-id"),
      );
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(
      replacementConversation,
    );

    setPath(`/conversations/${deletedConversation.id}`);
    component = mount(App, { target: document.body });
    await settleUi();
    addEventListenerSpy.mockRestore();

    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Present before server-side deletion",
    );
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);

    expect(popstateHandler).not.toBeNull();
    setPath("/");
    popstateHandler!(new PopStateEvent("popstate"));
    await settleUi();
    setPath(`/conversations/${deletedConversation.id}`);
    popstateHandler!(new PopStateEvent("popstate"));
    await settleUi();

    expect(conversationsApi.getConversation).toHaveBeenCalledTimes(2);
    expect(conversationsApi.getConversation).toHaveBeenLastCalledWith("deleted-id");
    expect(window.location.pathname).toBe("/");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain(
      "Conversation not found",
    );
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.disconnect).toHaveBeenCalledOnce();
    expect(document.body.textContent).not.toContain(
      "Present before server-side deletion",
    );

    const dismissButton = document.querySelector<HTMLButtonElement>(".dismiss");
    expect(dismissButton).not.toBeNull();
    flushSync(() => dismissButton!.click());

    const textarea = document.querySelector<HTMLTextAreaElement>(
      'textarea[aria-label="Message"]',
    );
    expect(textarea).not.toBeNull();
    flushSync(() => {
      textarea!.value = "Continue after recovery";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const sendButton = document.querySelector<HTMLButtonElement>(".action.send");
    expect(sendButton).not.toBeNull();
    flushSync(() => sendButton!.click());
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledWith(
      "proj-default",
      null,
      null,
    );
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(2);
    expect(websocketHarness.client.connect).toHaveBeenLastCalledWith(
      replacementConversation.id,
    );
  });

  it("shows the not-found notice when popstate restores a stale ID with no active conversation", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi.spyOn(window, "addEventListener").mockImplementation(
      (type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions) => {
        if (type === "popstate") {
          popstateHandler =
            typeof listener === "function"
              ? listener
              : (event) => listener.handleEvent(event);
          return;
        }
        addEventListener(type, listener, options);
      },
    );

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([makeSpace()]);
    component = mount(App, { target: document.body });
    await settleUi();
    addEventListenerSpy.mockRestore();

    vi.mocked(conversationsApi.getConversation).mockRejectedValueOnce(
      new Error("HTTP 404 from GET /api/conversations/stale-id"),
    );
    expect(popstateHandler).not.toBeNull();
    setPath("/conversations/stale-id");
    popstateHandler!(new PopStateEvent("popstate"));
    await settleUi();

    expect(conversationsApi.getConversation).toHaveBeenCalledWith("stale-id");
    expect(window.location.pathname).toBe("/");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain(
      "Conversation not found",
    );
    expect(websocketHarness.client.disconnect).not.toHaveBeenCalled();
  });

  it("keeps the active conversation when its route existence check fails transiently", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi.spyOn(window, "addEventListener").mockImplementation(
      (type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions) => {
        if (type === "popstate") {
          popstateHandler =
            typeof listener === "function"
              ? listener
              : (event) => listener.handleEvent(event);
          return;
        }
        addEventListener(type, listener, options);
      },
    );
    const activeConversation = makeConversationSummary({ id: "active-id" });
    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([makeSpace()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      activeConversation,
    ]);
    vi.mocked(conversationsApi.getConversation)
      .mockResolvedValueOnce(
        makeConversationDetail({
          id: activeConversation.id,
          messages: [
            {
              role: "assistant",
              content: "Still active after gateway trouble",
              timestamp: "2026-04-07T12:01:00Z",
              turn_state: "completed",
            },
          ],
        }),
      )
      .mockRejectedValueOnce(
        new Error("HTTP 503 from GET /api/conversations/active-id"),
      );

    setPath(`/conversations/${activeConversation.id}`);
    component = mount(App, { target: document.body });
    await settleUi();
    addEventListenerSpy.mockRestore();

    expect(popstateHandler).not.toBeNull();
    setPath("/");
    popstateHandler!(new PopStateEvent("popstate"));
    await settleUi();
    setPath(`/conversations/${activeConversation.id}`);
    popstateHandler!(new PopStateEvent("popstate"));
    await settleUi();

    expect(window.location.pathname).toBe(`/conversations/${activeConversation.id}`);
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Still active after gateway trouble",
    );
    expect(document.querySelector('[role="alert"]')).toBeNull();
    expect(websocketHarness.client.disconnect).not.toHaveBeenCalled();
  });

  it("renders the conversation and removes Settings after browser Back", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi.spyOn(window, "addEventListener").mockImplementation(
      (type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions) => {
        if (type === "popstate") {
          popstateHandler =
            typeof listener === "function"
              ? listener
              : (event) => listener.handleEvent(event);
          return;
        }
        addEventListener(type, listener, options);
      },
    );
    const defaultSpace = makeSpace();
    const linkedConversation = makeConversationSummary({
      id: "conv-settings-back",
      message_preview: "Settings return journey",
    });

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      linkedConversation,
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValue(
      makeConversationDetail({
        id: linkedConversation.id,
        messages: [
          {
            role: "assistant",
            content: "Conversation restored after Back",
            timestamp: "2026-04-07T12:01:00Z",
            turn_state: "completed",
          },
        ],
      }),
    );

    window.history.pushState(null, "", `/conversations/${linkedConversation.id}`);
    component = mount(App, { target: document.body });
    await settleUi();

    const settingsButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Settings",
    );
    expect(settingsButton).toBeDefined();
    flushSync(() => settingsButton!.click());
    await settleUi();

    expect(window.location.pathname).toBe("/settings/providers");
    expect(document.querySelector('[aria-label="Settings"]')).not.toBeNull();

    const conversationFetchCount = vi.mocked(conversationsApi.getConversation).mock.calls
      .length;
    const websocketConnectCount = websocketHarness.client.connect.mock.calls.length;

    expect(popstateHandler).not.toBeNull();
    setPath(`/conversations/${linkedConversation.id}`);
    popstateHandler!(new PopStateEvent("popstate"));
    await settleUi();

    expect(window.location.pathname).toBe(`/conversations/${linkedConversation.id}`);
    expect(document.querySelector('[aria-label="Conversation"]')).not.toBeNull();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Conversation restored after Back",
    );
    expect(document.querySelector('[aria-label="Settings"]')).toBeNull();
    expect(conversationsApi.getConversation).toHaveBeenCalledTimes(
      conversationFetchCount + 1,
    );
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(
      websocketConnectCount,
    );
    addEventListenerSpy.mockRestore();
  });

  it("connects exactly once when a conversation is picked from the sidebar", async () => {
    const defaultSpace = makeSpace();
    const selected = makeConversationSummary({
      id: "conv-picked",
      message_preview: "Pick me",
    });

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([selected]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: selected.id }),
    );

    component = mount(App, { target: document.body });
    await settleUi();
    await openSpaceFromPicker(defaultSpace.name);

    const selectButton = document.querySelector<HTMLButtonElement>(
      ".conversation-select-btn",
    );
    expect(selectButton).not.toBeNull();
    flushSync(() => selectButton!.click());
    await settleUi();

    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(selected.id);
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

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([makeSpace()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);

    component = mount(App, { target: document.body });
    await settleUi();
    // The app lands on the space picker — open the default space so the
    // shell navigation and its runtime context render.
    await openSpaceFromPicker("default");

    const surfaceNavigation = document.querySelector('nav[aria-label="Surface"]');
    expect(surfaceNavigation?.parentElement?.textContent).toContain("MANAGED");
  });

  it("creates a conversation from the real app wiring and navigates to it", async () => {
    const defaultSpace = makeSpace();
    const created: ConversationResponse = {
      id: "conv-new",
      space_id: defaultSpace.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(created);

    component = mount(App, { target: document.body });
    await settleUi();
    // Open the space so ConversationList (and its "New" button) renders.
    await openSpaceFromPicker(defaultSpace.name);

    const newButton = document.querySelector(".new-btn") as HTMLButtonElement | null;
    expect(newButton).not.toBeNull();

    flushSync(() => newButton!.click());
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledWith(
      defaultSpace.id,
      undefined,
      null,
    );
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith("conv-new");
    expect(window.location.pathname).toBe("/conversations/conv-new");
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Send a message below to start the conversation.",
    );
  });

  it("connects exactly once when the composer auto-creates a conversation", async () => {
    const defaultSpace = makeSpace();
    const created: ConversationResponse = {
      id: "conv-auto",
      space_id: defaultSpace.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(created);

    component = mount(App, { target: document.body });
    await settleUi();
    await openSpaceFromPicker(defaultSpace.name);

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    expect(textarea).not.toBeNull();
    flushSync(() => {
      textarea!.value = "Start from the composer";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const sendButton = document.querySelector<HTMLButtonElement>(".action.send");
    expect(sendButton).not.toBeNull();
    flushSync(() => sendButton!.click());
    await settleUi();

    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(created.id);
  });

  it("connects exactly once when returning to the conversation surface", async () => {
    const defaultSpace = makeSpace();
    const linked = makeConversationSummary({ id: "conv-surface" });

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([linked]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: linked.id }),
    );

    setPath(`/conversations/${linked.id}`);
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.client.connect.mockClear();

    const surfaceButtons = Array.from(
      document.querySelectorAll<HTMLButtonElement>('nav[aria-label="Surface"] button'),
    );
    const operator = surfaceButtons.find((button) =>
      button.textContent?.includes("Operator"),
    );
    expect(operator).toBeDefined();
    flushSync(() => operator!.click());
    await settleUi();

    const conversation = Array.from(
      document.querySelectorAll<HTMLButtonElement>('nav[aria-label="Surface"] button'),
    ).find((button) => button.textContent?.includes("Conversation"));
    expect(conversation).toBeDefined();
    flushSync(() => conversation!.click());
    await settleUi();

    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(linked.id);
  });

  it("connects exactly once when manual retry is requested", async () => {
    const defaultSpace = makeSpace();
    const linked = makeConversationSummary({ id: "conv-retry" });

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([linked]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: linked.id }),
    );

    setPath(`/conversations/${linked.id}`);
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.client.connect.mockClear();
    websocketHarness.fireStatus("error");
    await settleUi();

    const retry = document.querySelector<HTMLButtonElement>(".retry-btn");
    expect(retry).not.toBeNull();
    flushSync(() => retry!.click());
    await settleUi();

    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(linked.id);
  });

  it("sends a message and finalises a streamed reply through the full app stack", async () => {
    const defaultSpace = makeSpace();

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
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

  it("refreshes saved models before returning to the conversation and sending", async () => {
    const configured = makeSettings({
      "local/existing-model": {
        provider_id: "local",
        model_id: "existing-model",
        aliases: [],
        generation_preset: null,
      },
      "local/new-model": {
        provider_id: "local",
        model_id: "new-model",
        aliases: [],
        generation_preset: null,
      },
    });
    const saved = structuredClone(configured);
    saved.default_model = "local/new-model";
    vi.mocked(settingsApi.fetchSettings)
      .mockResolvedValueOnce(makeSettings())
      .mockResolvedValueOnce(configured)
      .mockResolvedValueOnce(saved)
      .mockResolvedValueOnce(saved)
      .mockRejectedValueOnce(new Error("transient catalogue failure"));
    vi.mocked(settingsApi.saveSettings)
      .mockResolvedValueOnce(saved)
      .mockResolvedValueOnce(configured);
    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([makeSpace()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-settings-model" }),
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: "conv-settings-model" }),
    );

    setPath("/conversations/conv-settings-model");
    component = mount(App, { target: document.body });
    await settleUi();
    expect(document.body.textContent).toContain("No models are configured");

    const settingsButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Settings",
    );
    flushSync(() => settingsButton!.click());
    await settleUi();
    const newModelButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim().startsWith("local/new-model"),
    );
    flushSync(() => newModelButton!.click());
    await waitForText("Saved automatically. New turns will use the updated runtime.");

    const closeButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Close settings"]',
    );
    flushSync(() => closeButton!.click());
    await settleUi();
    expect(document.body.textContent).toContain("local/new-model");

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      textarea!.value = "Send with the newly saved model";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const sendButton = document.querySelector<HTMLButtonElement>(".action.send");
    expect(sendButton?.disabled).toBe(false);
    flushSync(() => sendButton!.click());
    await settleUi();

    expect(settingsApi.fetchSettings).toHaveBeenCalledTimes(3);
    expect(websocketHarness.client.send).toHaveBeenCalledWith({
      type: "send_message",
      content: "Send with the newly saved model",
      selected_model_id: "local/new-model",
    });

    flushSync(() => settingsButton!.click());
    await settleUi();
    const existingModelButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim().startsWith("local/existing-model"),
    );
    flushSync(() => existingModelButton!.click());
    await waitForText("Saved automatically. New turns will use the updated runtime.");
    expect(document.body.textContent).not.toContain("transient catalogue failure");
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(2);
    expect(settingsApi.fetchSettings).toHaveBeenCalledTimes(5);
  });

  it("switches to the operator surface and renders the overview", async () => {
    const defaultSpace = makeSpace();

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(
      makeOperatorOverview(),
    );
    component = mount(App, { target: document.body });
    await settleUi();
    await openSpaceFromPicker(defaultSpace.name);

    const surfaceNavigation = document.querySelector('nav[aria-label="Surface"]');
    const operatorButton = Array.from(
      surfaceNavigation?.querySelectorAll("button") ?? [],
    ).find((button) => button.textContent?.trim() === "Operator") as
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

  it("shows missing space workspaces and relinks them before opening", async () => {
    const defaultSpace = makeSpace({ name: "Space" });
    const missingWorkspace = makeWorkspaceAttachment({ is_missing: true });
    const relinkedWorkspace = makeWorkspaceAttachment({
      path: "/srv/space",
      canonical_path: "/srv/space",
      is_missing: false,
    });

    vi.mocked(spacesApi.listSpaces).mockResolvedValueOnce([defaultSpace]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(spacesApi.listSpaceWorkspaces)
      .mockResolvedValueOnce([missingWorkspace])
      .mockResolvedValueOnce([relinkedWorkspace]);
    vi.mocked(spacesApi.detachSpaceWorkspace).mockResolvedValueOnce(undefined);
    vi.mocked(spacesApi.attachSpaceWorkspace).mockResolvedValueOnce(relinkedWorkspace);

    component = mount(App, { target: document.body });
    await settleUi();

    const card = Array.from(
      document.querySelectorAll<HTMLElement>(".space-card"),
    ).find((el) => el.textContent?.includes(defaultSpace.name));
    expect(card).toBeDefined();
    // Preview/selection is triggered by the space-name button, not the card
    // container (UI audit f-2 — card is no longer a single interactive widget).
    const nameBtn = card!.querySelector<HTMLButtonElement>(".space-name-btn");
    expect(nameBtn).toBeDefined();
    flushSync(() => nameBtn!.click());
    await waitForText("Missing folder");

    const relinkInput = document.querySelector<HTMLInputElement>(
      'input[aria-label="Replacement path for Codebase"]',
    );
    expect(relinkInput).toBeDefined();
    flushSync(() => {
      relinkInput!.value = "/srv/space";
      relinkInput!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const relinkButton = Array.from(
      document.querySelectorAll<HTMLButtonElement>("button"),
    ).find((button) => button.textContent?.includes("Relink"));
    expect(relinkButton).toBeDefined();
    flushSync(() => relinkButton!.click());
    await waitForText("Workspace relinked.");

    expect(spacesApi.detachSpaceWorkspace).toHaveBeenCalledWith(
      defaultSpace.id,
      missingWorkspace.id,
    );
    expect(spacesApi.attachSpaceWorkspace).toHaveBeenCalledWith(defaultSpace.id, {
      path: "/srv/space",
      label: "Codebase",
      permissions: ["read", "write"],
      is_trusted: true,
      set_as_default: true,
    });
  });

});
