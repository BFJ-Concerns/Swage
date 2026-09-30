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
import * as sessionsApi from "../lib/api/sessions";
import * as settingsApi from "../lib/api/settings";
import * as topicsApi from "../lib/api/topics";
import {
  makeConversationDetail,
  makeConversationSummary,
} from "../lib/__tests__/fixtures/conversations";
import type { EffectiveAgentDefinitionResponse } from "../lib/protocol/EffectiveAgentDefinitionResponse";
import type {
  ClientEvent,
  ConnectionStatus,
  ConversationDetail,
  ConversationResponse,
  OperatorOverview,
  Topic,
  ServerEvent,
  WorkspaceAttachment,
  SettingsResponse,
} from "../lib/types";

type MountedComponent = ReturnType<typeof mount>;

const websocketHarness = vi.hoisted(() => {
  type HarnessClient = {
    connect: ReturnType<typeof vi.fn>;
    send: ReturnType<typeof vi.fn<(event: ClientEvent) => void>>;
    disconnect: ReturnType<typeof vi.fn>;
    onEvent: ReturnType<typeof vi.fn>;
    onStatusChange: ReturnType<typeof vi.fn>;
    fireEvent(event: ServerEvent): void;
    fireStatus(status: ConnectionStatus): void;
  };
  const clients: HarnessClient[] = [];
  const clientsByConversation = new Map<string, HarnessClient>();

  function createClient(): HarnessClient {
    let eventHandler: ((event: ServerEvent) => void) | null = null;
    let statusHandler: ((status: ConnectionStatus) => void) | null = null;
    const client: HarnessClient = {
      connect: vi.fn((conversationId: string) => {
        clientsByConversation.set(conversationId, client);
      }),
      send: vi.fn<(event: ClientEvent) => void>(),
      disconnect: vi.fn(),
      onEvent: vi.fn((handler: (event: ServerEvent) => void) => {
        eventHandler = handler;
      }),
      onStatusChange: vi.fn((handler: (status: ConnectionStatus) => void) => {
        statusHandler = handler;
      }),
      fireEvent(event: ServerEvent) {
        eventHandler?.(event);
      },
      fireStatus(status: ConnectionStatus) {
        statusHandler?.(status);
      },
    };
    clients.push(client);
    return client;
  }

  return {
    get client() {
      return clients.at(-1)!;
    },
    clientFor(conversationId: string) {
      return clientsByConversation.get(conversationId);
    },
    factory: vi.fn(createClient),
    fireEvent(event: ServerEvent, conversationId?: string) {
      (conversationId
        ? clientsByConversation.get(conversationId)
        : clients[0]
      )?.fireEvent(event);
    },
    fireStatus(status: ConnectionStatus, conversationId?: string) {
      (conversationId
        ? clientsByConversation.get(conversationId)
        : clients[0]
      )?.fireStatus(status);
    },
    reset() {
      clients.length = 0;
      clientsByConversation.clear();
      this.factory.mockClear();
    },
  };
});

vi.mock("../lib/api/topics", () => ({
  listTopics: vi.fn(),
  createTopic: vi.fn(),
  updateTopic: vi.fn(),
  deleteTopic: vi.fn(),
  listTopicWorkspaces: vi.fn(),
  attachTopicWorkspace: vi.fn(),
  detachTopicWorkspace: vi.fn(),
  relinkTopicWorkspace: vi.fn(),
  listTopicSessions: vi.fn(),
}));

vi.mock("../lib/api/conversations", () => ({
  listConversations: vi.fn(),
  createConversation: vi.fn(),
  getConversation: vi.fn(),
  deleteConversation: vi.fn(),
  listBranches: vi.fn(),
  createBranch: vi.fn(),
  getThread: vi.fn(),
  abandonThread: vi.fn(),
}));

vi.mock("../lib/api/agent-definitions", () => ({
  listEffectiveAgentDefinitions: vi.fn(),
}));

vi.mock("../lib/api/materials", () => ({
  listTopicMaterials: vi.fn(),
  listSessionMaterials: vi.fn(),
  createMaterial: vi.fn(),
}));

vi.mock("../lib/api/sessions", () => ({
  listRecentSessions: vi.fn(async () => []),
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

function makeTopic(overrides: Partial<Topic> = {}): Topic {
  return {
    id: "proj-default",
    name: "default",
    description: "",
    slug: "default",
    created_at: "2026-04-07T12:00:00Z",
    ...overrides,
  };
}

function makeWorkspaceAttachment(
  overrides: Partial<WorkspaceAttachment> = {},
): WorkspaceAttachment {
  return {
    id: "workspace-1",
    topic_id: "proj-default",
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

function makeSettings(
  models: SettingsResponse["models"] = {},
): SettingsResponse {
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
    tool_permissions: {
      allow: ["*"],
      deny: [],
      bundles: {},
      require_approval: [],
    },
    hitl: { timeout: 300, timeout_policy: "reject" },
    sandbox: { enabled: true, allow_degraded_execution: false },
    context: { assembly: "default", timeout: 5, budget: null },
    pricing: { models: {} },
    observability: { force_on: false },
    communication: {
      context_transfer: {
        mode: "none",
        allowed_fields: [],
        summary_budget: null,
        targets: {},
      },
      message_ttl_secs: 86_400,
      expiry_interval_secs: 60,
    },
    sessions: { archive_after_days: 7 },
    voice: {},
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
        topic_id: "proj-default",
        topic_name: "default",
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
        label: "Local device",
        kind: "local",
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

function makeAgentDefinition(
  overrides: Partial<EffectiveAgentDefinitionResponse> = {},
): EffectiveAgentDefinitionResponse {
  return {
    id: "agent-1",
    display_name: "Agent one",
    description: "",
    instructions: "",
    model_id: null,
    tool_permissions: {},
    skills: [],
    output_schema: null,
    output_max_retries: null,
    is_builtin: false,
    source_scope: "global",
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
 * Start a session from the front door. Swage lands on the recent-sessions
 * list; a test that needs the shell open in a topic's scope starts a session
 * there — nothing is created until the first message, so the composer is
 * open with the topic in view.
 */
async function startSessionInTopic(topicId: string | null): Promise<void> {
  const topicSelect = document.querySelector<HTMLSelectElement>(
    'select[aria-label="Topic"]',
  );
  if (!topicSelect)
    throw new Error("the session-start control is not on screen");
  flushSync(() => {
    topicSelect.value = topicId ?? "";
    topicSelect.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const start = Array.from(
    document.querySelectorAll<HTMLButtonElement>(
      '[aria-label="Start a session"] button',
    ),
  ).find((button) => button.textContent?.trim() === "Start");
  if (!start) throw new Error("the Start button is not on screen");
  flushSync(() => start.click());
  await settleUi();
}

describe("App", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    websocketHarness.reset();
    vi.clearAllMocks();
    setPath("/");
    delete window.swageDesktop;
    localStorage.clear();
    vi.mocked(materialsApi.listTopicMaterials).mockResolvedValue([]);
    vi.mocked(materialsApi.listSessionMaterials).mockResolvedValue([]);
    // Selecting a conversation lists its branches beside its detail; these
    // journeys run on conversations that have none.
    vi.mocked(conversationsApi.listBranches).mockResolvedValue([]);
    vi.mocked(
      agentDefinitionsApi.listEffectiveAgentDefinitions,
    ).mockResolvedValue([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(
      makeOperatorOverview(),
    );
    vi.mocked(settingsApi.fetchSettings).mockRejectedValue(
      new Error("settings are not configured in this test"),
    );
    // TopicPicker fetches workspace/session detail on card selection — return
    // empty arrays by default so the picker renders without errors. Individual
    // tests can override these mocks if they exercise the detail panel.
    vi.mocked(topicsApi.listTopicWorkspaces).mockResolvedValue([]);
    vi.mocked(topicsApi.listTopicSessions).mockResolvedValue([]);
  });

  afterEach(async () => {
    if (component) {
      unmount(component);
      component = undefined;
      await settleUi();
    }

    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }

    setPath("/");
    localStorage.clear();
  });

  it("opens to the front door regardless of a previously open topic", async () => {
    const restoredTopic = makeTopic({ id: "proj-restored", name: "Restored" });
    localStorage.setItem("swage:active-topic", restoredTopic.id);
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([
      makeTopic(),
      restoredTopic,
    ]);

    component = mount(App, { target: document.body });
    await settleUi();

    // Sessions are the front door: no topic's conversations are loaded
    // until a session brings its scope into view.
    expect(conversationsApi.listConversations).not.toHaveBeenCalled();
    expect(
      document.querySelector('[aria-label="Recent sessions"]'),
    ).not.toBeNull();
    expect(document.querySelector('nav[aria-label="Surface"]')).toBeNull();
  });

  it("starts a general-assistant session from the front door and creates it on the first message", async () => {
    const created: ConversationResponse = {
      id: "conv-general",
      agent_id: "gateway",
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(
      created,
    );

    component = mount(App, { target: document.body });
    await settleUi();
    await startSessionInTopic(null);

    // The shell opens in the general scope with the composer, and nothing
    // has been created yet.
    expect(
      document.querySelector('[aria-label="Session scope"]')?.textContent,
    ).toContain("General");
    expect(conversationsApi.createConversation).not.toHaveBeenCalled();
    expect(
      agentDefinitionsApi.listEffectiveAgentDefinitions,
    ).toHaveBeenCalledWith(null);

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      textarea!.value = "Hello, general assistant";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledWith(
      null,
      null,
      null,
      null,
    );
    expect(window.location.pathname).toBe("/conversations/conv-general");
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(
      "conv-general",
    );
  });

  it("keeps a session created for one scope out of the scope the user moved to meanwhile", async () => {
    const topic = makeTopic();
    let resolveCreation: (value: ConversationResponse) => void = () => {};
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([topic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValue([]);
    vi.mocked(conversationsApi.createConversation).mockReturnValueOnce(
      new Promise<ConversationResponse>((resolve) => {
        resolveCreation = resolve;
      }),
    );

    component = mount(App, { target: document.body });
    await settleUi();
    await startSessionInTopic(topic.id);

    // The first message starts creating a session in the topic...
    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      textarea!.value = "Start in the topic";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    await settleUi();
    expect(conversationsApi.createConversation).toHaveBeenCalledWith(
      topic.id,
      null,
      null,
      null,
    );

    // ...and the user leaves for the front door before it settles.
    const allSessions = Array.from(
      document.querySelectorAll<HTMLButtonElement>("button"),
    ).find((button) => button.textContent?.trim() === "All sessions");
    flushSync(() => allSessions!.click());
    await settleUi();

    resolveCreation({
      id: "conv-late",
      agent_id: "gateway",
      topic_id: topic.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    });
    await settleUi();

    // The late session is not activated where the user now stands.
    expect(window.location.pathname).toBe("/");
    expect(websocketHarness.clientFor("conv-late")).toBeUndefined();
    expect(
      document.querySelector('[aria-label="Recent sessions"]'),
    ).not.toBeNull();
  });

  it("opens to the recent-sessions list and resumes a session from it", async () => {
    const defaultTopic = makeTopic();
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(sessionsApi.listRecentSessions).mockResolvedValueOnce([
      {
        source: {
          kind: "conversation",
          session: {
            id: "conv-recent",
            topic_id: defaultTopic.id,
            topic_name: defaultTopic.name,
            agent_id: "gateway",
            origin: { kind: "direct" },
            created_at: "2026-09-15T08:00:00Z",
            last_activity: "2026-09-15T09:00:00Z",
            message_preview: "Pick up where we left off",
            has_interrupted_turn: false,
            has_running_turn: false,
          },
        },
        last_activity: "2026-09-15T09:00:00Z",
        automated: [],
      },
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValue(
      makeConversationDetail({ id: "conv-recent", topic_id: defaultTopic.id }),
    );
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({
        id: "conv-recent",
        message_preview: "Pick up where we left off",
      }),
    ]);

    component = mount(App, { target: document.body });
    await waitForText("Pick up where we left off");
    expect(sessionsApi.listRecentSessions).toHaveBeenCalledWith(null, false);
    // The recent list and the session-start control share the front door.
    expect(
      document.querySelector('[aria-label="Start a session"]'),
    ).not.toBeNull();

    const row = document.querySelector<HTMLButtonElement>(
      ".recent-sessions .row.source",
    );
    if (!row) throw new Error("recent session row missing");
    flushSync(() => row.click());
    await settleUi();

    expect(window.location.pathname).toBe("/conversations/conv-recent");
    expect(websocketHarness.client.connect).toHaveBeenCalledWith("conv-recent");
    expect(conversationsApi.listConversations).toHaveBeenCalledWith(
      defaultTopic.id,
    );
    expect(document.querySelector('[aria-label="Start a session"]')).toBeNull();
    expect(
      document.querySelector('[aria-label="Session scope"]')?.textContent,
    ).toContain(defaultTopic.name);
  });

  it("connects exactly once when popstate selects a different conversation", async () => {
    const defaultTopic = makeTopic();
    const first = makeConversationSummary({ id: "conv-first" });
    const second = makeConversationSummary({ id: "conv-second" });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      first,
      second,
    ]);
    vi.mocked(conversationsApi.getConversation).mockImplementation(async (id) =>
      makeConversationDetail({ id }),
    );

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
    await vi.waitFor(() =>
      expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1),
    );
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(first.id);
  });

  it("restores a deep-linked conversation on startup and opens its WebSocket", async () => {
    const defaultTopic = makeTopic();
    const linkedConversation = makeConversationSummary({
      id: "conv-linked",
      message_preview: "Deep-linked conversation",
    });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
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

    expect(topicsApi.listTopics).toHaveBeenCalledTimes(1);
    expect(conversationsApi.listConversations).toHaveBeenCalledWith(
      defaultTopic.id,
    );
    expect(conversationsApi.getConversation).toHaveBeenCalledWith(
      "conv-linked",
    );
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

    const activePreview = document.querySelector(
      ".conversation-item.active .preview",
    );
    expect(activePreview?.textContent).toContain("Deep-linked conversation");
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Restored from URL",
    );
  });

  it("keeps the gateway available when startup agent inventory loading fails", async () => {
    const defaultTopic = makeTopic();
    const linkedConversation = makeConversationSummary({ id: "conv-linked" });
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      linkedConversation,
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValue(
      makeConversationDetail({ id: linkedConversation.id }),
    );
    vi.mocked(
      agentDefinitionsApi.listEffectiveAgentDefinitions,
    ).mockRejectedValueOnce(new Error("agent inventory unavailable"));
    window.history.replaceState(
      null,
      "",
      `/conversations/${linkedConversation.id}`,
    );

    component = mount(App, { target: document.body });
    await settleUi();

    expect(
      agentDefinitionsApi.listEffectiveAgentDefinitions,
    ).toHaveBeenCalledWith(defaultTopic.id);
    expect(document.querySelector(".conversation-list")).not.toBeNull();
    expect(document.querySelector(".error-screen")).toBeNull();
  });

  it("falls back to the root with a notice for a stale deep link", async () => {
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
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
    expect(websocketHarness.clientFor("stale-id")).toBeUndefined();
    await startSessionInTopic("proj-default");
    expect(
      document.querySelector<HTMLTextAreaElement>(
        'textarea[aria-label="Message"]',
      ),
    ).not.toBeNull();
  });

  it("shows the not-found notice when popstate restores a deleted conversation", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi
      .spyOn(window, "addEventListener")
      .mockImplementation(
        (
          type: string,
          listener: EventListenerOrEventListenerObject,
          options?: boolean | AddEventListenerOptions,
        ) => {
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
      agent_id: "gateway",
      topic_id: "proj-default",
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
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
    expect(conversationsApi.getConversation).toHaveBeenLastCalledWith(
      "deleted-id",
    );
    expect(window.location.pathname).toBe("/");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain(
      "Conversation not found",
    );
    expect(
      websocketHarness.clientFor(deletedConversation.id)?.connect,
    ).toHaveBeenCalledTimes(1);
    expect(
      websocketHarness.clientFor(deletedConversation.id)?.disconnect,
    ).toHaveBeenCalledOnce();
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

    const sendButton =
      document.querySelector<HTMLButtonElement>(".action.send");
    expect(sendButton).not.toBeNull();
    flushSync(() => sendButton!.click());
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledWith(
      "proj-default",
      null,
      null,
      null,
    );
    expect(
      websocketHarness.clientFor(replacementConversation.id)?.connect,
    ).toHaveBeenCalledWith(replacementConversation.id);
  });

  it("shows the not-found notice when popstate restores a stale ID with no active conversation", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi
      .spyOn(window, "addEventListener")
      .mockImplementation(
        (
          type: string,
          listener: EventListenerOrEventListenerObject,
          options?: boolean | AddEventListenerOptions,
        ) => {
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

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
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
    expect(websocketHarness.clientFor("stale-id")).toBeUndefined();
  });

  it("keeps the active conversation when its route existence check fails transiently", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi
      .spyOn(window, "addEventListener")
      .mockImplementation(
        (
          type: string,
          listener: EventListenerOrEventListenerObject,
          options?: boolean | AddEventListenerOptions,
        ) => {
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
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
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

    expect(window.location.pathname).toBe(
      `/conversations/${activeConversation.id}`,
    );
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Still active after gateway trouble",
    );
    expect(document.querySelector('[role="alert"]')).toBeNull();
    expect(websocketHarness.client.disconnect).not.toHaveBeenCalled();
  });

  it("renders the conversation and removes Settings after browser Back", async () => {
    let popstateHandler: EventListener | null = null;
    const addEventListener = window.addEventListener.bind(window);
    const addEventListenerSpy = vi
      .spyOn(window, "addEventListener")
      .mockImplementation(
        (
          type: string,
          listener: EventListenerOrEventListenerObject,
          options?: boolean | AddEventListenerOptions,
        ) => {
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
    const defaultTopic = makeTopic();
    const linkedConversation = makeConversationSummary({
      id: "conv-settings-back",
      message_preview: "Settings return journey",
    });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
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

    window.history.pushState(
      null,
      "",
      `/conversations/${linkedConversation.id}`,
    );
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

    const conversationFetchCount = vi.mocked(conversationsApi.getConversation)
      .mock.calls.length;
    const websocketConnectCount =
      websocketHarness.client.connect.mock.calls.length;

    expect(popstateHandler).not.toBeNull();
    setPath(`/conversations/${linkedConversation.id}`);
    popstateHandler!(new PopStateEvent("popstate"));
    await settleUi();

    expect(window.location.pathname).toBe(
      `/conversations/${linkedConversation.id}`,
    );
    expect(
      document.querySelector('[aria-label="Conversation"]'),
    ).not.toBeNull();
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
    const defaultTopic = makeTopic();
    const selected = makeConversationSummary({
      id: "conv-picked",
      message_preview: "Pick me",
    });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      selected,
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: selected.id }),
    );

    component = mount(App, { target: document.body });
    await settleUi();
    await startSessionInTopic(defaultTopic.id);

    const selectButton = document.querySelector<HTMLButtonElement>(
      ".conversation-select-btn",
    );
    expect(selectButton).not.toBeNull();
    flushSync(() => selectButton!.click());
    await settleUi();

    const selectedClient = websocketHarness.clientFor(selected.id);
    expect(selectedClient?.connect).toHaveBeenCalledTimes(1);
    expect(selectedClient?.connect).toHaveBeenCalledWith(selected.id);
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

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);

    component = mount(App, { target: document.body });
    await settleUi();
    // The app lands on the front door — start a session so the shell
    // navigation and its runtime context render.
    await startSessionInTopic("proj-default");

    const surfaceNavigation = document.querySelector(
      'nav[aria-label="Surface"]',
    );
    expect(surfaceNavigation?.parentElement?.textContent).toContain("MANAGED");
  });

  it("creates and navigates when the sidebar starts a conversation", async () => {
    const defaultTopic = makeTopic();
    const created: ConversationResponse = {
      id: "conv-new",
      agent_id: "gateway",
      topic_id: defaultTopic.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(
      created,
    );
    vi.mocked(settingsApi.fetchSettings).mockResolvedValueOnce(
      makeSettings({
        "local:reasoning": {
          provider_id: "local",
          model_id: "reasoning",
          aliases: [],
          context_window: 128_000,
          generation_preset: null,
          default_effort: "low",
          capabilities: { reasoning_replay: false },
        },
      }),
    );

    component = mount(App, { target: document.body });
    await settleUi();
    // Start a session in the topic so ConversationList (and its "New" button) renders.
    await startSessionInTopic(defaultTopic.id);

    const newButton = document.querySelector(
      ".new-btn",
    ) as HTMLButtonElement | null;
    expect(newButton).not.toBeNull();

    flushSync(() => newButton!.click());
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledWith(
      defaultTopic.id,
      undefined,
      undefined,
      undefined,
    );
    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith("conv-new");
    expect(window.location.pathname).toBe("/conversations/conv-new");
    expect(document.querySelector(".message-display")).not.toBeNull();
  });

  it("uses the active creation picker for both New controls", async () => {
    const defaultTopic = makeTopic();
    const existing = makeConversationSummary({ id: "conv-existing" });
    const createdFromChannel: ConversationResponse = {
      id: "conv-channel-new",
      agent_id: "gateway",
      topic_id: defaultTopic.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };
    const createdFromSidebar: ConversationResponse = {
      id: "conv-sidebar-new",
      agent_id: "gateway",
      topic_id: defaultTopic.id,
      created_at: "2026-04-07T12:04:00Z",
      last_activity: "2026-04-07T12:04:00Z",
    };

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      existing,
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValue(
      makeConversationDetail({ id: existing.id, topic_id: defaultTopic.id }),
    );
    vi.mocked(conversationsApi.createConversation)
      .mockResolvedValueOnce(createdFromChannel)
      .mockResolvedValueOnce(createdFromSidebar);
    vi.mocked(
      agentDefinitionsApi.listEffectiveAgentDefinitions,
    ).mockResolvedValue([
      makeAgentDefinition({
        id: "gateway",
        display_name: "Swage default",
        is_builtin: true,
      }),
      makeAgentDefinition({
        id: "picker-witness",
        display_name: "Picker witness",
      }),
    ]);
    setPath(`/conversations/${existing.id}`);

    component = mount(App, { target: document.body });
    await settleUi();

    const picker = document.querySelector<HTMLSelectElement>(
      'select[aria-label="Agent"]',
    );
    expect(picker).not.toBeNull();
    flushSync(() => {
      picker!.value = "picker-witness";
      picker!.dispatchEvent(new Event("change", { bubbles: true }));
    });

    flushSync(() =>
      (document.querySelector(".new-session") as HTMLButtonElement).click(),
    );
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledTimes(1);
    expect(conversationsApi.createConversation).toHaveBeenLastCalledWith(
      defaultTopic.id,
      undefined,
      "picker-witness",
      undefined,
    );
    expect(window.location.pathname).toBe(
      `/conversations/${createdFromChannel.id}`,
    );
    expect(document.querySelectorAll(".conversation-item")).toHaveLength(2);

    flushSync(() =>
      (document.querySelector(".new-btn") as HTMLButtonElement).click(),
    );
    await settleUi();

    expect(conversationsApi.createConversation).toHaveBeenCalledTimes(2);
    expect(conversationsApi.createConversation).toHaveBeenLastCalledWith(
      defaultTopic.id,
      undefined,
      "picker-witness",
      undefined,
    );
    expect(window.location.pathname).toBe(
      `/conversations/${createdFromSidebar.id}`,
    );
    expect(document.querySelectorAll(".conversation-item")).toHaveLength(3);
  });

  it("connects exactly once when the composer auto-creates a conversation", async () => {
    const defaultTopic = makeTopic();
    const created: ConversationResponse = {
      id: "conv-auto",
      agent_id: "gateway",
      topic_id: defaultTopic.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(
      created,
    );

    component = mount(App, { target: document.body });
    await settleUi();
    await startSessionInTopic(defaultTopic.id);

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    expect(textarea).not.toBeNull();
    flushSync(() => {
      textarea!.value = "Start from the composer";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const sendButton =
      document.querySelector<HTMLButtonElement>(".action.send");
    expect(sendButton).not.toBeNull();
    flushSync(() => sendButton!.click());
    await settleUi();

    expect(websocketHarness.client.connect).toHaveBeenCalledTimes(1);
    expect(websocketHarness.client.connect).toHaveBeenCalledWith(created.id);
  });

  it("creates the conversation under the agent chosen in the composer", async () => {
    const defaultTopic = makeTopic();
    const created: ConversationResponse = {
      id: "conv-agent",
      agent_id: "gateway",
      topic_id: defaultTopic.id,
      created_at: "2026-04-07T12:03:00Z",
      last_activity: "2026-04-07T12:03:00Z",
    };

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(conversationsApi.createConversation).mockResolvedValueOnce(
      created,
    );
    vi.mocked(
      agentDefinitionsApi.listEffectiveAgentDefinitions,
    ).mockResolvedValue([
      makeAgentDefinition({
        id: "swage-default",
        display_name: "Swage default",
        is_builtin: true,
      }),
      makeAgentDefinition({ id: "reviewer", display_name: "Review companion" }),
    ]);

    component = mount(App, { target: document.body });
    await settleUi();
    await startSessionInTopic(defaultTopic.id);

    const picker = document.querySelector<HTMLSelectElement>(
      'select[aria-label="Agent"]',
    );
    expect(picker).not.toBeNull();
    expect(Array.from(picker!.options, (option) => option.value)).toEqual([
      "",
      "reviewer",
    ]);

    flushSync(() => {
      picker!.value = "reviewer";
      picker!.dispatchEvent(new Event("change", { bubbles: true }));
    });

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    expect(textarea).not.toBeNull();
    flushSync(() => {
      textarea!.value = "Review the release checklist";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const sendButton =
      document.querySelector<HTMLButtonElement>(".action.send");
    expect(sendButton).not.toBeNull();
    flushSync(() => sendButton!.click());
    await settleUi();

    // One send from an empty topic yields a conversation governed by the
    // definition the picker was left on.
    expect(conversationsApi.createConversation).toHaveBeenCalledWith(
      defaultTopic.id,
      null,
      "reviewer",
      null,
    );
    expect(window.location.pathname).toBe("/conversations/conv-agent");
  });

  it("retains the selected conversation subscription while visiting Operator", async () => {
    const defaultTopic = makeTopic();
    const linked = makeConversationSummary({ id: "conv-surface" });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      linked,
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: linked.id }),
    );

    setPath(`/conversations/${linked.id}`);
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.client.connect.mockClear();

    const surfaceButtons = Array.from(
      document.querySelectorAll<HTMLButtonElement>(
        'nav[aria-label="Surface"] button',
      ),
    );
    const operator = surfaceButtons.find((button) =>
      button.textContent?.includes("Operator"),
    );
    expect(operator).toBeDefined();
    flushSync(() => operator!.click());
    await settleUi();

    const conversation = Array.from(
      document.querySelectorAll<HTMLButtonElement>(
        'nav[aria-label="Surface"] button',
      ),
    ).find((button) => button.textContent?.includes("Conversation"));
    expect(conversation).toBeDefined();
    flushSync(() => conversation!.click());
    await settleUi();

    expect(websocketHarness.client.connect).not.toHaveBeenCalled();
  });

  it("connects exactly once when manual retry is requested", async () => {
    const defaultTopic = makeTopic();
    const linked = makeConversationSummary({ id: "conv-retry" });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      linked,
    ]);
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
    const defaultTopic = makeTopic();

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
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

    const textarea = document.querySelector(
      "textarea",
    ) as HTMLTextAreaElement | null;
    expect(textarea).not.toBeNull();

    flushSync(() => {
      textarea!.value = "Hello from the app";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const sendButton = document.querySelector(
      ".action.send",
    ) as HTMLButtonElement | null;
    expect(sendButton).not.toBeNull();

    flushSync(() => sendButton!.click());
    await settleUi();

    expect(websocketHarness.client.send).toHaveBeenCalledWith({
      type: "send_message",
      content: "Hello from the app",
    });
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Hello from the app",
    );
    expect(document.querySelector(".action.stop")).not.toBeNull();

    websocketHarness.fireEvent({ type: "token", content: "Streamed " });
    websocketHarness.fireEvent({ type: "token", content: "reply." });
    await settleUi();

    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Streamed reply.",
    );

    websocketHarness.fireEvent({
      type: "turn_complete",
      turn_id: "turn-123",
    });
    await settleUi();

    expect(document.querySelector(".action.stop")).toBeNull();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Streamed reply.",
    );
  });

  it("keeps a live stream on its conversation when another conversation is selected", async () => {
    const defaultTopic = makeTopic();

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({
        id: "conv-a",
        message_preview: "Conversation A",
      }),
      makeConversationSummary({
        id: "conv-b",
        message_preview: "Conversation B",
      }),
    ]);
    vi.mocked(conversationsApi.getConversation)
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-a" }))
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-b" }))
      .mockResolvedValueOnce(
        makeConversationDetail({
          id: "conv-a",
          messages: [
            {
              role: "assistant",
              content: "Reply for A while B is selected",
              timestamp: "2026-04-07T12:04:00Z",
              turn_state: "completed",
              turn_id: "turn-a",
            },
          ],
        }),
      );

    setPath("/conversations/conv-a");
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.fireStatus("connected");

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      textarea!.value = "Keep this reply with A";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    websocketHarness.fireEvent({ type: "token", content: "Reply for A" });
    await settleUi();

    const aClient = websocketHarness.clientFor("conv-a")!;
    aClient.connect.mockClear();
    aClient.disconnect.mockClear();
    const conversationButtons = document.querySelectorAll<HTMLButtonElement>(
      ".conversation-select-btn",
    );
    flushSync(() => conversationButtons[1]!.click());
    await settleUi();

    const bClient = websocketHarness.clientFor("conv-b")!;
    expect(bClient).toBeDefined();
    expect(bClient.connect).toHaveBeenCalledWith("conv-b");
    expect(aClient.disconnect).not.toHaveBeenCalled();
    expect(
      document.querySelector(".message-display")?.textContent,
    ).not.toContain("Reply for A");
    expect(document.querySelector(".action.stop")).toBeNull();
    expect(document.querySelector(".action.send")).not.toBeNull();

    // A's socket remains live after B becomes the visible conversation. Its
    // event must retain that ownership at the App wiring seam rather than be
    // attributed to whichever conversation happens to be selected.
    websocketHarness.fireEvent(
      { type: "token", content: " while B is selected" },
      "conv-a",
    );
    await settleUi();
    expect(
      document.querySelector(".message-display")?.textContent,
    ).not.toContain("while B is selected");

    const BTextarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      BTextarea!.value = "Message for B";
      BTextarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    expect(bClient.send).toHaveBeenLastCalledWith({
      type: "send_message",
      content: "Message for B",
    });
    expect(document.querySelector(".action.stop")).not.toBeNull();

    websocketHarness.fireEvent(
      { type: "token", content: "Reply for B" },
      "conv-b",
    );
    await settleUi();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Reply for B",
    );

    websocketHarness.fireEvent(
      { type: "turn_complete", turn_id: "turn-b" },
      "conv-b",
    );
    await settleUi();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Reply for B",
    );
    expect(document.querySelector(".action.stop")).toBeNull();

    // A completes while B remains selected. The real App wiring must settle
    // A, release its retained socket, and leave B's connection usable.
    websocketHarness.fireEvent(
      { type: "turn_complete", turn_id: "turn-a" },
      "conv-a",
    );
    await settleUi();

    expect(aClient.disconnect).toHaveBeenCalledOnce();
    expect(bClient.disconnect).not.toHaveBeenCalled();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Reply for B",
    );
    expect(document.querySelector(".action.send")).not.toBeNull();

    const refreshedConversationButtons =
      document.querySelectorAll<HTMLButtonElement>(".conversation-select-btn");
    flushSync(() => refreshedConversationButtons[0]!.click());
    await settleUi();

    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Reply for A while B is selected",
    );
    expect(document.querySelector(".action.stop")).toBeNull();
  });

  it("returns to a still-streaming conversation after clearing the route without replacing its client", async () => {
    const defaultTopic = makeTopic();

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-a" }),
      makeConversationSummary({ id: "conv-b" }),
    ]);
    vi.mocked(conversationsApi.getConversation)
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-a" }))
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-b" }))
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-a" }));

    setPath("/conversations/conv-a");
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.fireStatus("connected");

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      textarea!.value = "Keep streaming";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    websocketHarness.fireEvent(
      { type: "token", content: "First A token" },
      "conv-a",
    );
    await settleUi();

    const aClient = websocketHarness.clientFor("conv-a")!;
    aClient.connect.mockClear();
    const buttons = document.querySelectorAll<HTMLButtonElement>(
      ".conversation-select-btn",
    );
    flushSync(() => buttons[1]!.click());
    await settleUi();
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".new-btn")!.click(),
    );
    await settleUi();
    flushSync(() =>
      document
        .querySelectorAll<HTMLButtonElement>(".conversation-select-btn")[0]!
        .click(),
    );
    await settleUi();

    expect(websocketHarness.clientFor("conv-a")).toBe(aClient);
    expect(aClient.connect).not.toHaveBeenCalled();
    expect(aClient.disconnect).not.toHaveBeenCalled();
    websocketHarness.fireEvent(
      { type: "token", content: " Second A token" },
      "conv-a",
    );
    await settleUi();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "First A token Second A token",
    );
    expect(document.querySelector(".action.stop")).not.toBeNull();
  });

  it("reconciles an off-screen reconnect with its owner without changing B", async () => {
    const defaultTopic = makeTopic();
    let resolveRecovery!: (conversation: ConversationDetail) => void;
    const recovery = new Promise<ConversationDetail>((resolve) => {
      resolveRecovery = resolve;
    });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-a" }),
      makeConversationSummary({ id: "conv-b" }),
    ]);
    vi.mocked(conversationsApi.getConversation)
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-a" }))
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-b" }))
      .mockReturnValueOnce(recovery)
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-a" }));

    setPath("/conversations/conv-a");
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.fireStatus("connected");

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      textarea!.value = "A needs recovery";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    websocketHarness.fireEvent(
      { type: "token", content: "Partial A" },
      "conv-a",
    );
    await settleUi();

    flushSync(() =>
      document
        .querySelectorAll<HTMLButtonElement>(".conversation-select-btn")[1]!
        .click(),
    );
    await settleUi();
    const bClient = websocketHarness.clientFor("conv-b")!;
    bClient.disconnect.mockClear();
    websocketHarness.fireStatus("reconnecting", "conv-a");
    websocketHarness.fireStatus("connected", "conv-a");
    await settleUi();

    expect(
      document.querySelector(".message-display")?.textContent,
    ).not.toContain("Partial A");
    expect(document.querySelector(".action.stop")).toBeNull();
    expect(document.querySelector(".action.send")).not.toBeNull();

    expect(bClient.disconnect).not.toHaveBeenCalled();

    // Return to A before its reconnect snapshot resolves. The ordinary
    // selection fetch is older; the recovery must replace it when it lands.
    flushSync(() =>
      document
        .querySelectorAll<HTMLButtonElement>(".conversation-select-btn")[0]!
        .click(),
    );
    await settleUi();
    resolveRecovery(
      makeConversationDetail({
        id: "conv-a",
        messages: [
          {
            role: "assistant",
            content: "Recovered A",
            timestamp: "2026-04-07T12:04:00Z",
            turn_state: "completed",
            turn_id: "turn-a",
          },
        ],
      }),
    );
    await settleUi();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "Recovered A",
    );
    expect(document.querySelector(".action.stop")).toBeNull();
  });

  it("keeps the live conversation subscription while Operator opens", async () => {
    const defaultTopic = makeTopic();

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-a" }),
    ]);
    vi.mocked(conversationsApi.getConversation).mockResolvedValueOnce(
      makeConversationDetail({ id: "conv-a" }),
    );

    setPath("/conversations/conv-a");
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.fireStatus("connected");

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    flushSync(() => {
      textarea!.value = "Keep the subscription";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    websocketHarness.fireEvent({ type: "token", content: "Still live" });
    await settleUi();

    websocketHarness.client.connect.mockClear();
    websocketHarness.client.disconnect.mockClear();
    vi.mocked(operatorApi.fetchOperatorOverview).mockClear();
    const operator = Array.from(
      document.querySelectorAll<HTMLButtonElement>(
        'nav[aria-label="Surface"] button',
      ),
    ).find((button) => button.textContent?.trim() === "Operator");
    flushSync(() => operator!.click());
    await waitForText("Live control surface for the running gateway");

    expect(operatorApi.fetchOperatorOverview).toHaveBeenCalledOnce();
    expect(websocketHarness.client.disconnect).not.toHaveBeenCalled();
    expect(websocketHarness.client.connect).not.toHaveBeenCalled();
  });

  it("keeps a newer stream live when reconnect refresh completes late", async () => {
    const defaultTopic = makeTopic();
    let resolveReconnectRefresh!: (conversation: ConversationDetail) => void;
    const reconnectRefresh = new Promise<ConversationDetail>((resolve) => {
      resolveReconnectRefresh = resolve;
    });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([
      makeConversationSummary({ id: "conv-reconnect" }),
    ]);
    vi.mocked(conversationsApi.getConversation)
      .mockResolvedValueOnce(makeConversationDetail({ id: "conv-reconnect" }))
      .mockReturnValueOnce(reconnectRefresh);

    setPath("/conversations/conv-reconnect");
    component = mount(App, { target: document.body });
    await settleUi();
    websocketHarness.fireStatus("connected");

    const textarea = document.querySelector<HTMLTextAreaElement>("textarea");
    expect(textarea).not.toBeNull();
    flushSync(() => {
      textarea!.value = "Turn active before reconnect";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    websocketHarness.fireEvent({
      type: "token",
      content: "Older live content",
    });
    await settleUi();

    websocketHarness.fireStatus("reconnecting");
    websocketHarness.fireStatus("connected");
    expect(conversationsApi.getConversation).toHaveBeenCalledTimes(2);

    websocketHarness.fireEvent({
      type: "turn_complete",
      turn_id: "older-turn",
    });
    await settleUi();
    flushSync(() => {
      textarea!.value = "New turn while refresh is pending";
      textarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    flushSync(() =>
      document.querySelector<HTMLButtonElement>(".action.send")!.click(),
    );
    websocketHarness.fireEvent({ type: "token", content: "New live content" });
    await settleUi();

    resolveReconnectRefresh(
      makeConversationDetail({
        id: "conv-reconnect",
        messages: [
          {
            role: "assistant",
            content: "Recovered older response",
            timestamp: "2026-04-07T12:04:00Z",
            turn_state: "completed",
            turn_id: "older-turn",
          },
        ],
      }),
    );
    await settleUi();

    expect(document.querySelector(".action.stop")).not.toBeNull();
    expect(document.querySelector(".message-display")?.textContent).toContain(
      "New live content",
    );
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
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
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
    await waitForText(
      "Saved automatically. New turns will use the updated runtime.",
    );

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
    const sendButton =
      document.querySelector<HTMLButtonElement>(".action.send");
    expect(sendButton?.disabled).toBe(false);
    flushSync(() => sendButton!.click());
    await settleUi();

    expect(settingsApi.fetchSettings).toHaveBeenCalledTimes(3);
    expect(websocketHarness.client.send).toHaveBeenCalledWith({
      type: "send_message",
      content: "Send with the newly saved model",
      selected_model_id: "local/new-model",
    });
    websocketHarness.fireEvent({
      type: "turn_complete",
      turn_id: "first-model-turn",
    });
    await settleUi();

    flushSync(() => settingsButton!.click());
    await settleUi();
    const existingModelButton = Array.from(
      document.querySelectorAll("button"),
    ).find((button) =>
      button.textContent?.trim().startsWith("local/existing-model"),
    );
    flushSync(() => existingModelButton!.click());
    await waitForText(
      "Saved automatically. New turns will use the updated runtime.",
    );
    expect(document.body.textContent).not.toContain(
      "transient catalogue failure",
    );
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(2);
    expect(settingsApi.fetchSettings).toHaveBeenCalledTimes(5);

    const retainedCloseButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Close settings"]',
    );
    flushSync(() => retainedCloseButton!.click());
    await settleUi();
    expect(document.body.textContent).toContain("local/new-model");

    const retainedTextarea =
      document.querySelector<HTMLTextAreaElement>("textarea");
    const retainedSendButton =
      document.querySelector<HTMLButtonElement>(".action.send");
    flushSync(() => {
      retainedTextarea!.value = "Send after the failed catalogue refresh";
      retainedTextarea!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(retainedSendButton?.disabled).toBe(false);
    flushSync(() => retainedSendButton!.click());
    await settleUi();

    expect(websocketHarness.client.send).toHaveBeenLastCalledWith({
      type: "send_message",
      content: "Send after the failed catalogue refresh",
      selected_model_id: "local/new-model",
    });
  });

  it("switches to the operator surface and renders the overview", async () => {
    const defaultTopic = makeTopic();

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue(
      makeOperatorOverview(),
    );
    component = mount(App, { target: document.body });
    await settleUi();
    await startSessionInTopic(defaultTopic.id);

    const surfaceNavigation = document.querySelector(
      'nav[aria-label="Surface"]',
    );
    const operatorButton = Array.from(
      surfaceNavigation?.querySelectorAll("button") ?? [],
    ).find((button) => button.textContent?.trim() === "Operator") as
      HTMLButtonElement | undefined;

    expect(operatorButton).toBeDefined();

    flushSync(() => operatorButton!.click());
    await waitForText("Live control surface for the running gateway");

    expect(operatorApi.fetchOperatorOverview).toHaveBeenCalled();
    expect(document.body.textContent).toContain(
      "Live control surface for the running gateway",
    );
    expect(document.body.textContent).toContain("dev_dm");
    expect(document.body.textContent).toContain("Local device");
  });

  it("shows missing topic workspaces in the topic's settings and relinks them", async () => {
    const defaultTopic = makeTopic({ name: "Topic" });
    const missingWorkspace = makeWorkspaceAttachment({ is_missing: true });
    const relinkedWorkspace = makeWorkspaceAttachment({
      path: "/srv/topic",
      canonical_path: "/srv/topic",
      is_missing: false,
    });

    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([defaultTopic]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(topicsApi.listTopicWorkspaces)
      .mockResolvedValueOnce([missingWorkspace])
      .mockResolvedValueOnce([relinkedWorkspace]);
    vi.mocked(topicsApi.relinkTopicWorkspace).mockResolvedValueOnce(
      relinkedWorkspace,
    );

    setPath("/settings/topic/default/workspaces");
    component = mount(App, { target: document.body });
    await waitForText("Missing folder");

    const relinkInput = document.querySelector<HTMLInputElement>(
      'input[aria-label="Replacement path for Codebase"]',
    );
    expect(relinkInput).toBeDefined();
    flushSync(() => {
      relinkInput!.value = "/srv/topic";
      relinkInput!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const relinkButton = Array.from(
      document.querySelectorAll<HTMLButtonElement>("button"),
    ).find((button) => button.textContent?.includes("Relink"));
    expect(relinkButton).toBeDefined();
    flushSync(() => relinkButton!.click());
    await settleUi();

    expect(topicsApi.relinkTopicWorkspace).toHaveBeenCalledWith(
      defaultTopic.id,
      missingWorkspace.id,
      "/srv/topic",
    );
    expect(topicsApi.detachTopicWorkspace).not.toHaveBeenCalled();
    expect(topicsApi.attachTopicWorkspace).not.toHaveBeenCalled();
  });

  it("reaches the voice settings from the settings navigation", async () => {
    vi.mocked(topicsApi.listTopics).mockResolvedValueOnce([makeTopic()]);
    vi.mocked(conversationsApi.listConversations).mockResolvedValueOnce([]);
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(makeSettings({}));
    setPath("/settings/providers");
    component = mount(App, { target: document.body });
    await waitForText("Automation");

    const voice = Array.from(
      document.querySelectorAll<HTMLButtonElement>(
        'nav[aria-label="Settings sections"] button',
      ),
    ).find((button) => button.textContent?.trim() === "Voice");
    if (!voice) throw new Error("Settings navigation has no Voice entry");
    flushSync(() => voice.click());
    await settleUi();

    expect(window.location.pathname).toBe("/settings/voice");
    expect(
      document.querySelector('input[aria-label="Transcription model"]'),
    ).not.toBeNull();
    expect(document.body.textContent).toContain("OpenAI protocol");
  });
});
