// @vitest-environment jsdom
/** Behavioural coverage for the shell's conversation, operator, and settings surfaces. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { createClassComponent } from "svelte/legacy";
import AppLayout from "../components/AppLayout.svelte";
import { parseRoute } from "../router";
import type { ConversationStore } from "../stores/conversations";
import type { ConnectionStore } from "../stores/connection";
import type { TopicStore } from "../stores/topics";
import type { StreamingStore } from "../stores/streaming";
import type { SurfacesStore } from "../stores/surfaces";
import type { MessageView } from "../types";
import {
  makeConversationDetail,
  makeConversationSummary,
} from "./fixtures/conversations";

import * as operatorApi from "../api/operator";

vi.mock("../api/sessions", () => ({
  listRecentSessions: vi.fn(async () => []),
}));

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  fetchScheduleRuns: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

function mockConversationStore(): ConversationStore {
  return {
    get conversations() {
      return [];
    },
    get activeConversation() {
      return null;
    },
    get isLoading() {
      return false;
    },
    loadConversations: async () => {},
    selectConversation: async () => {},
    createConversation: async () => {},
    deleteConversation: async () => {},
    appendOptimisticUserMessage: () => {},
    appendAssistantMessage: () => {},
    appendRecordedMessage: () => {},
    clearActiveConversation: () => {},
    refreshActiveConversation: async () => {},
  };
}

function mockConversationStoreWithMessages(
  messages: MessageView[],
): ConversationStore {
  const detail = makeConversationDetail({ topic_id: "default", messages });
  return {
    ...mockConversationStore(),
    get activeConversation() {
      return detail;
    },
  };
}

function mockConversationStoreWithSessions(
  previews: string[],
  activeIndex: number,
): ConversationStore {
  const summaries = previews.map((preview, index) =>
    makeConversationSummary({
      id: `conv-${index + 1}`,
      message_preview: preview,
    }),
  );
  const active = makeConversationDetail({
    id: summaries[activeIndex].id,
    topic_id: "default",
  });
  return {
    ...mockConversationStore(),
    get conversations() {
      return summaries;
    },
    get activeConversation() {
      return active;
    },
  };
}

function mockSurfacesStore(): SurfacesStore {
  const surface = {
    id: "surface-1",
    session_id: "conv-1",
    title: "Release checklist",
    created_at: "2026-08-29T10:00:00Z",
    updated_at: "2026-08-29T10:00:00Z",
    modules: [
      {
        id: "module-1",
        kind: "document",
        position: 0,
        source: "Ship the gateway",
        updated_at: "2026-08-29T10:00:00Z",
        annotations: [],
      },
    ],
  };
  return {
    get surfaces() {
      return [
        {
          id: surface.id,
          title: surface.title,
          updated_at: surface.updated_at,
        },
      ];
    },
    get activeSurface() {
      return surface;
    },
    get library() {
      return [];
    },
    entryName: () => null,
    saveToLibrary: async () => {},
    loadSurfaces: async () => {},
    openSurface: async () => {},
    closeSurface: () => {},
    createSurface: async () => null,
    refreshLibrary: async () => {},
    writeModule: async () => {},
    annotate: async () => {},
    updateAnnotation: async () => {},
    removeAnnotation: async () => {},
    stopListening: () => {},
  };
}

function mockConnectionStore(): ConnectionStore {
  return {
    get status() {
      return "disconnected" as const;
    },
    get errorMessage() {
      return null;
    },
    connectTo: () => {},
    disconnect: () => {},
    sendMessage: () => {},
    cancelTurn: () => {},
    onServerEvent: () => {},
  };
}

function mockStreamingStore(): StreamingStore {
  return {
    get isStreaming() {
      return false;
    },
    get activeTurnGeneration() {
      return 0;
    },
    get streamedContent() {
      return "";
    },
    get turnError() {
      return null;
    },
    get turnStartedAt() {
      return null;
    },
    get activeToolCalls() {
      return [];
    },
    get providerRetry() {
      return null;
    },
    get pendingApproval() {
      return null;
    },
    handleServerEvent: () => {},
    startTurn: () => {},
    cancelTurn: () => {},
    reconcileAfterReconnect: () => {},
  };
}

function mockTopicStore(
  hasCurrentTopic = true,
  topicId = "default",
): TopicStore {
  const topic = {
    id: topicId,
    name: topicId,
    description: "",
    slug: topicId,
    created_at: "",
  };

  return {
    get topics() {
      return hasCurrentTopic ? [topic] : [];
    },
    get currentTopic() {
      return hasCurrentTopic ? topic : null;
    },
    get recentTopics() {
      return [];
    },
    get isLoading() {
      return false;
    },
    loadTopics: async () => {},
    selectTopic: async () => {},
    closeTopic: () => {},
    createTopic: async () => {},
    deleteTopic: async () => {},
  };
}

interface MountOptions {
  activeSurface?: "conversation" | "operator" | "surfaces";
  hasCurrentTopic?: boolean;
  // A session about to start: the shell is in scope with the composer open.
  // Defaults to a pending start in the mock topic, so the shell renders.
  pendingStart?: { topicId: string | null; agentId: string | null } | null;
  onNavigate?: (path: string) => void;
  onSelectSurface?: (surface: "conversation" | "operator" | "surfaces") => void;
  routePath?: string;
  agentDefinitions?: import("../protocol/EffectiveAgentDefinitionResponse").EffectiveAgentDefinitionResponse[];
  modelOptions?: string[];
  streamingStore?: StreamingStore;
  conversationStore?: ConversationStore;
  onNewConversation?: () => void;
  surfacesStore?: SurfacesStore;
}

function mountLayout(options: MountOptions = {}): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: options.conversationStore ?? mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: options.streamingStore ?? mockStreamingStore(),
      topicStore: mockTopicStore(options.hasCurrentTopic ?? true),
      pendingStart:
        options.pendingStart === undefined
          ? {
              topicId: (options.hasCurrentTopic ?? true) ? "default" : null,
              agentId: null,
            }
          : options.pendingStart,
      onRetry: () => {},
      activeSurface: options.activeSurface ?? "conversation",
      onSelectSurface: options.onSelectSurface ?? (() => {}),
      onNavigate: options.onNavigate,
      route: options.routePath ? parseRoute(options.routePath) : undefined,
      agentDefinitions: options.agentDefinitions ?? [],
      modelOptions: options.modelOptions,
      onNewConversation: options.onNewConversation,
      surfacesStore: options.surfacesStore,
    },
  });
}

function surfaceButton(label: string): HTMLButtonElement {
  const navigation = document.querySelector('nav[aria-label="Surface"]');
  const button = Array.from(navigation?.querySelectorAll("button") ?? []).find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error(`Surface control "${label}" was not rendered`);
  }
  return button;
}

describe("AppLayout shell navigation", () => {
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
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("shows the recent-sessions front door with the session-start control when nothing is in view", () => {
    component = mountLayout({ hasCurrentTopic: false, pendingStart: null });

    expect(
      document.querySelector('[aria-label="Recent sessions"]'),
    ).not.toBeNull();
    expect(
      document.querySelector('[aria-label="Start a session"]'),
    ).not.toBeNull();
    expect(document.querySelector('nav[aria-label="Surface"]')).toBeNull();
    // The start control offers a session with no topic first.
    const options = Array.from(
      document.querySelectorAll('select[aria-label="Topic"] option'),
    ).map((option) => option.textContent);
    expect(options[0]).toContain("No topic");
  });

  it("starts a session in the chosen scope without creating a conversation", () => {
    const onStartSession = vi.fn();
    const conversationStore = mockConversationStore();
    const createConversation = vi.spyOn(
      conversationStore,
      "createConversation",
    );
    component = mount(AppLayout, {
      target: document.body,
      props: {
        conversationStore,
        connectionStore: mockConnectionStore(),
        streamingStore: mockStreamingStore(),
        topicStore: mockTopicStore(true, "research"),
        pendingStart: null,
        onStartSession,
        onRetry: () => {},
        activeSurface: "conversation",
        onSelectSurface: () => {},
      },
    });
    const topic = document.querySelector(
      'select[aria-label="Topic"]',
    ) as HTMLSelectElement;
    topic.value = "research";
    topic.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();
    const start = Array.from(
      document.querySelectorAll('[aria-label="Start a session"] button'),
    ).find(
      (button) => button.textContent?.trim() === "Start",
    ) as HTMLButtonElement;
    flushSync(() => start.click());

    expect(onStartSession).toHaveBeenCalledWith({
      topicId: "research",
      agentId: null,
    });
    expect(createConversation).not.toHaveBeenCalled();
  });

  it("names the session's scope in the bar and links a topic to its settings", () => {
    const onNavigate = vi.fn();
    component = mountLayout({ onNavigate });

    const scope = document.querySelector('[aria-label="Session scope"]');
    expect(scope?.textContent).toContain("default");
    flushSync(() =>
      (scope?.querySelector("button") as HTMLButtonElement).click(),
    );
    expect(onNavigate).toHaveBeenCalledWith("/settings/topic/default/overview");
  });

  it("names the general scope for a session without a topic", () => {
    component = mountLayout({
      hasCurrentTopic: false,
      pendingStart: { topicId: null, agentId: null },
    });

    const scope = document.querySelector('[aria-label="Session scope"]');
    expect(scope?.textContent).toContain("General");
    expect(scope?.querySelector("button")).toBeNull();
  });

  it("opens the shared library from the surface controls and returns from it", () => {
    const onNavigate = vi.fn();
    component = mountLayout({
      onNavigate,
      routePath: "/library",
      surfacesStore: mockSurfacesStore(),
    });

    expect(
      document.querySelector('[aria-label="Surface library"]'),
    ).not.toBeNull();
    expect(surfaceButton("Library").getAttribute("aria-pressed")).toBe("true");
    expect(surfaceButton("Conversation").getAttribute("aria-pressed")).toBe(
      "false",
    );
    flushSync(() =>
      (
        document.querySelector(
          '[aria-label="Close library"]',
        ) as HTMLButtonElement
      ).click(),
    );
    expect(onNavigate).toHaveBeenCalledWith("/");
  });

  it("offers the library from the front door without a session to add to", () => {
    const onNavigate = vi.fn();
    component = mountLayout({
      hasCurrentTopic: false,
      pendingStart: null,
      onNavigate,
    });
    const library = Array.from(
      document.querySelectorAll("main.landing button"),
    ).find(
      (button) => button.textContent?.trim() === "Library",
    ) as HTMLButtonElement;
    flushSync(() => library.click());
    expect(onNavigate).toHaveBeenCalledWith("/library");
  });

  it("renders the conversation surface with its list, messages, and composer", () => {
    component = mountLayout();

    expect(document.querySelector(".conversation-list")).not.toBeNull();
    expect(
      document.querySelector('[aria-label="Conversation messages"]'),
    ).not.toBeNull();
    expect(
      document.querySelector('textarea[aria-label="Message"]'),
    ).not.toBeNull();
  });

  it("loads one shared record for the workflow run account and journal", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response("", { status: 404 }));
    component = mountLayout({ routePath: "/workflow-runs/run%201" });

    await vi.waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledOnce();
    });
    expect(fetchSpy).toHaveBeenCalledWith("/api/workflow-runs/run%201");
    await vi.waitFor(() => {
      expect(
        document.querySelectorAll(
          ".workflow-run .notice, .workflow-run .status",
        ),
      ).toHaveLength(2);
    });
    expect(document.querySelector(".workflow-run")?.textContent).not.toContain(
      "HTTP 404",
    );
  });

  it("offers the shipped default and effective definitions when starting a conversation", () => {
    component = mountLayout({
      agentDefinitions: [
        {
          id: "gateway",
          display_name: "Built-in assistant",
          description: "",
          instructions: "",
          model_id: null,
          tool_permissions: null,
          skills: [],
          output_schema: null,
          output_max_retries: null,
          is_builtin: true,
          source_scope: "built_in",
        },
        {
          id: "researcher",
          display_name: "Researcher",
          description: "",
          instructions: "cite",
          model_id: null,
          tool_permissions: null,
          skills: [],
          output_schema: null,
          output_max_retries: null,
          is_builtin: false,
          source_scope: "global",
        },
      ],
    });

    const options = Array.from(
      document.querySelectorAll('select[aria-label="Agent"] option'),
    ).map((option) => option.textContent);
    expect(options).toEqual(["Built-in assistant", "Researcher"]);
  });

  it("opens the creation surface without eagerly creating a conversation", () => {
    const conversationStore = mockConversationStore();
    const createConversation = vi.spyOn(
      conversationStore,
      "createConversation",
    );
    const onNewConversation = vi.fn();
    component = mountLayout({
      conversationStore,
      onNewConversation,
      agentDefinitions: [
        {
          id: "researcher",
          display_name: "Researcher",
          description: "",
          instructions: "cite",
          model_id: null,
          tool_permissions: null,
          skills: [],
          output_schema: null,
          output_max_retries: null,
          is_builtin: false,
          source_scope: "global",
        },
      ],
    });
    (document.querySelector("button.new-btn") as HTMLButtonElement).click();

    expect(onNewConversation).toHaveBeenCalledOnce();
    expect(createConversation).not.toHaveBeenCalled();
  });

  it("does not retain an agent when the selected topic changes", () => {
    const conversationStore = mockConversationStore();
    const createConversation = vi.spyOn(
      conversationStore,
      "createConversation",
    );
    const layout = createClassComponent({
      component: AppLayout,
      target: document.body,
      props: {
        conversationStore,
        connectionStore: mockConnectionStore(),
        streamingStore: mockStreamingStore(),
        topicStore: mockTopicStore(),
        pendingStart: { topicId: "default", agentId: null },
        onRetry: () => {},
        activeSurface: "conversation",
        onSelectSurface: () => {},
        agentDefinitions: [
          {
            id: "topic-one-agent",
            display_name: "Topic one agent",
            description: "",
            instructions: "",
            model_id: null,
            tool_permissions: null,
            skills: [],
            output_schema: null,
            output_max_retries: null,
            is_builtin: false,
            source_scope: "topic",
          },
        ],
      },
    });
    const picker = document.querySelector(
      'select[aria-label="Agent"]',
    ) as HTMLSelectElement;
    picker.value = "topic-one-agent";
    picker.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    layout.$set({ topicStore: mockTopicStore(true, "second-topic") });
    flushSync();

    const cleared = document.querySelector(
      'select[aria-label="Agent"]',
    ) as HTMLSelectElement;
    expect(cleared.value).toBe("");
    (document.querySelector("button.new-btn") as HTMLButtonElement).click();
    expect(createConversation).not.toHaveBeenCalled();
    layout.$destroy();
  });

  it("clears a selected agent when refreshed definitions remove it", () => {
    const startTurn = vi.fn();
    const researcher: import("../protocol/EffectiveAgentDefinitionResponse").EffectiveAgentDefinitionResponse =
      {
        id: "researcher",
        display_name: "Researcher",
        description: "",
        instructions: "cite",
        model_id: null,
        tool_permissions: null,
        skills: [],
        output_schema: null,
        output_max_retries: null,
        is_builtin: false,
        source_scope: "global",
      };
    const layout = createClassComponent({
      component: AppLayout,
      target: document.body,
      props: {
        conversationStore: mockConversationStore(),
        connectionStore: mockConnectionStore(),
        streamingStore: { ...mockStreamingStore(), startTurn },
        topicStore: mockTopicStore(),
        pendingStart: { topicId: "default", agentId: null },
        onRetry: () => {},
        activeSurface: "conversation",
        onSelectSurface: () => {},
        agentDefinitions: [researcher],
      },
    });
    const picker = document.querySelector(
      'select[aria-label="Agent"]',
    ) as HTMLSelectElement;
    picker.value = researcher.id;
    picker.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    layout.$set({ agentDefinitions: [] });
    flushSync();

    expect(
      (
        document.querySelector(
          'select[aria-label="Agent"]',
        ) as HTMLSelectElement
      ).value,
    ).toBe("");
    const message = document.querySelector(
      'textarea[aria-label="Message"]',
    ) as HTMLTextAreaElement;
    message.value = "Use the refreshed definitions";
    message.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    (document.querySelector("button.send") as HTMLButtonElement).click();
    expect(startTurn).toHaveBeenCalledWith(
      "Use the refreshed definitions",
      null,
      null,
      null,
    );
    layout.$destroy();
  });

  it("passes the selected agent when the first message creates a conversation", () => {
    const startTurn = vi.fn();
    component = mountLayout({
      streamingStore: { ...mockStreamingStore(), startTurn },
      agentDefinitions: [
        {
          id: "researcher",
          display_name: "Researcher",
          description: "",
          instructions: "cite",
          model_id: null,
          tool_permissions: null,
          skills: [],
          output_schema: null,
          output_max_retries: null,
          is_builtin: false,
          source_scope: "global",
        },
      ],
    });
    const picker = document.querySelector(
      'select[aria-label="Agent"]',
    ) as HTMLSelectElement;
    picker.value = "researcher";
    picker.dispatchEvent(new Event("change", { bubbles: true }));
    const message = document.querySelector(
      'textarea[aria-label="Message"]',
    ) as HTMLTextAreaElement;
    message.value = "Hello";
    message.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    (document.querySelector("button.send") as HTMLButtonElement).click();

    expect(startTurn).toHaveBeenCalledWith("Hello", null, "researcher", null);
  });

  it("clears effort after the active conversation changes before the next new conversation", () => {
    const startTurn = vi.fn();
    const firstConversation = {
      id: "first-conversation",
      topic_id: "default",
      selected_model_id: null,
      agent_id: "gateway",
      created_at: "",
      last_activity: "",
      messages: [],
    };
    const secondConversation = {
      ...firstConversation,
      id: "second-conversation",
    };
    const conversationStore = {
      ...mockConversationStore(),
      get activeConversation() {
        return null;
      },
    };
    const layout = createClassComponent({
      component: AppLayout,
      target: document.body,
      props: {
        conversationStore,
        connectionStore: mockConnectionStore(),
        streamingStore: { ...mockStreamingStore(), startTurn },
        topicStore: mockTopicStore(),
        pendingStart: { topicId: "default", agentId: null },
        onRetry: () => {},
        activeSurface: "conversation",
        onSelectSurface: () => {},
        modelOptions: ["e2e-model"],
        agentDefinitions: [
          {
            id: "researcher",
            display_name: "Researcher",
            description: "",
            instructions: "",
            model_id: null,
            tool_permissions: null,
            skills: [],
            output_schema: null,
            output_max_retries: null,
            is_builtin: false,
            source_scope: "global",
          },
        ],
      },
    });
    const effort = document.querySelector(
      '[aria-label="Effort"]',
    ) as HTMLSelectElement;
    effort.value = "high";
    effort.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    layout.$set({
      conversationStore: {
        ...conversationStore,
        get activeConversation() {
          return firstConversation;
        },
      },
    });
    flushSync();
    layout.$set({
      conversationStore: {
        ...conversationStore,
        get activeConversation() {
          return secondConversation;
        },
      },
    });
    flushSync();
    layout.$set({ conversationStore });
    flushSync();

    const message = document.querySelector(
      'textarea[aria-label="Message"]',
    ) as HTMLTextAreaElement;
    message.value = "Use the new conversation default";
    message.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    (document.querySelector("button.send") as HTMLButtonElement).click();

    expect(startTurn).toHaveBeenCalledWith(
      "Use the new conversation default",
      "e2e-model",
      null,
      null,
    );
    layout.$destroy();
  });

  it("marks conversation as the selected surface", () => {
    component = mountLayout();

    expect(surfaceButton("Conversation").getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(surfaceButton("Operator").getAttribute("aria-pressed")).toBe(
      "false",
    );
  });

  it("requests the operator surface from its labelled control", () => {
    const onSelectSurface = vi.fn();
    component = mountLayout({ onSelectSurface });

    flushSync(() => surfaceButton("Operator").click());

    expect(onSelectSurface).toHaveBeenCalledWith("operator");
  });

  it("renders the operator dashboard when that surface is selected", () => {
    component = mountLayout({ activeSurface: "operator" });

    expect(document.querySelector("main.operator h1")?.textContent).toContain(
      "Live control surface for the running gateway",
    );
    expect(surfaceButton("Operator").getAttribute("aria-pressed")).toBe("true");
  });

  it("navigates to global settings from the surface controls", () => {
    const onNavigate = vi.fn();
    component = mountLayout({ onNavigate });

    flushSync(() => surfaceButton("Settings").click());

    expect(onNavigate).toHaveBeenCalledWith("/settings/providers");
  });

  it.each(["conversation", "operator"] as const)(
    "marks settings, rather than the %s surface, while settings is open",
    (activeSurface) => {
      const onNavigate = vi.fn();
      component = mountLayout({
        activeSurface,
        onNavigate,
        routePath: "/settings/providers",
      });

      expect(document.querySelector('[aria-label="Settings"]')).not.toBeNull();
      expect(surfaceButton("Conversation").getAttribute("aria-pressed")).toBe(
        "false",
      );
      expect(surfaceButton("Conversation").classList.contains("active")).toBe(
        false,
      );
      expect(surfaceButton("Operator").getAttribute("aria-pressed")).toBe(
        "false",
      );
      expect(surfaceButton("Operator").classList.contains("active")).toBe(
        false,
      );
      expect(surfaceButton("Settings").getAttribute("aria-pressed")).toBe(
        "true",
      );
      expect(surfaceButton("Settings").classList.contains("active")).toBe(true);
      flushSync(() =>
        (
          document.querySelector(
            '[aria-label="Close settings"]',
          ) as HTMLButtonElement
        ).click(),
      );

      expect(onNavigate).toHaveBeenCalledWith("/");
    },
  );

  it("navigates to the agent definitions settings tab", () => {
    const onNavigate = vi.fn();
    component = mountLayout({ onNavigate, routePath: "/settings/providers" });
    const settingsNavigation = document.querySelector(
      'nav[aria-label="Settings sections"]',
    );
    const agents = Array.from(
      settingsNavigation?.querySelectorAll("button") ?? [],
    ).find((button) => button.textContent?.trim() === "Agents");

    flushSync(() => agents?.click());

    expect(onNavigate).toHaveBeenCalledWith("/settings/agents");
  });

  it("navigates to command and skill settings tabs", () => {
    const onNavigate = vi.fn();
    component = mountLayout({ onNavigate, routePath: "/settings/providers" });
    const settingsNavigation = document.querySelector(
      'nav[aria-label="Settings sections"]',
    );
    const command = Array.from(
      settingsNavigation?.querySelectorAll("button") ?? [],
    ).find((button) => button.textContent?.trim() === "Commands");
    const skill = Array.from(
      settingsNavigation?.querySelectorAll("button") ?? [],
    ).find((button) => button.textContent?.trim() === "Skills");

    flushSync(() => command?.click());
    flushSync(() => skill?.click());

    expect(onNavigate).toHaveBeenNthCalledWith(1, "/settings/commands");
    expect(onNavigate).toHaveBeenNthCalledWith(2, "/settings/skills");
  });

  it("lists every topic in the settings sections and opens its page", () => {
    const onNavigate = vi.fn();
    component = mountLayout({ onNavigate, routePath: "/settings/providers" });
    const settingsNavigation = document.querySelector(
      'nav[aria-label="Settings sections"]',
    );
    const topic = Array.from(
      settingsNavigation?.querySelectorAll("button") ?? [],
    ).find((button) => button.textContent?.trim() === "default");

    flushSync(() => topic?.click());

    expect(onNavigate).toHaveBeenCalledWith("/settings/topic/default/overview");
  });

  it("navigates to a topic's workspaces from its settings page tabs", () => {
    const onNavigate = vi.fn();
    component = mountLayout({
      onNavigate,
      routePath: "/settings/topic/default/overview",
    });
    const tabs = document.querySelector(
      'nav[aria-label="Topic settings sections"]',
    );
    const workspaces = Array.from(tabs?.querySelectorAll("button") ?? []).find(
      (button) => button.textContent?.trim() === "Workspaces",
    );

    flushSync(() => workspaces?.click());

    expect(onNavigate).toHaveBeenCalledWith(
      "/settings/topic/default/workspaces",
    );
  });

  it("opens global settings from the front door", () => {
    const onNavigate = vi.fn();
    component = mountLayout({
      hasCurrentTopic: false,
      pendingStart: null,
      onNavigate,
    });

    const settingsButton = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Settings",
    );
    expect(settingsButton).toBeInstanceOf(HTMLButtonElement);

    flushSync(() => settingsButton?.click());

    expect(onNavigate).toHaveBeenCalledWith("/settings/providers");
  });

  it("renders global settings from the front door without the sessions list beneath", () => {
    component = mountLayout({
      hasCurrentTopic: false,
      pendingStart: null,
      routePath: "/settings/providers",
    });

    expect(document.querySelector('[aria-label="Settings"]')).not.toBeNull();
    expect(document.querySelector('[aria-label="Recent sessions"]')).toBeNull();
  });
});

describe("ambient conversation channel", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.replaceChildren();
  });

  it("carries the conversation channel beside the surfaces work surface", () => {
    component = mountLayout({
      activeSurface: "surfaces",
      surfacesStore: mockSurfacesStore(),
      conversationStore: mockConversationStoreWithMessages([
        {
          role: "user",
          content: "Draft the checklist",
          timestamp: "2026-08-29T10:00:01Z",
          turn_state: "completed",
        },
      ]),
    });

    // The work surface is genuinely on screen...
    expect(
      document.querySelector('main.surfaces-surface [aria-label="Surface"] h2')
        ?.textContent,
    ).toBe("Release checklist");
    // ...and so is the control channel, with the ongoing conversation in it.
    const channel = document.querySelector(
      'main.surfaces-surface [aria-label="Conversation"]',
    );
    expect(channel).not.toBeNull();
    expect(channel?.textContent).toContain("Draft the checklist");
    expect(
      channel?.querySelector('textarea[aria-label="Message"]'),
    ).not.toBeNull();
  });

  it("sends from the channel while standing on a surface", () => {
    const startTurn = vi.fn();
    component = mountLayout({
      activeSurface: "surfaces",
      surfacesStore: mockSurfacesStore(),
      conversationStore: mockConversationStoreWithMessages([]),
      streamingStore: { ...mockStreamingStore(), startTurn },
    });
    const message = document.querySelector(
      'main.surfaces-surface [aria-label="Conversation"] textarea[aria-label="Message"]',
    ) as HTMLTextAreaElement;
    message.value = "Add a rollback step";
    message.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    (document.querySelector("button.send") as HTMLButtonElement).click();

    expect(startTurn).toHaveBeenCalledWith(
      "Add a rollback step",
      null,
      null,
      null,
    );
  });

  it("keeps the conversation and its history across a move to the work surface", () => {
    const conversationStore = mockConversationStoreWithMessages([
      {
        role: "user",
        content: "What is left to ship?",
        timestamp: "2026-08-29T10:00:01Z",
        turn_state: "completed",
      },
    ]);
    const selectConversation = vi.spyOn(
      conversationStore,
      "selectConversation",
    );
    const loadConversations = vi.spyOn(conversationStore, "loadConversations");
    const refreshActiveConversation = vi.spyOn(
      conversationStore,
      "refreshActiveConversation",
    );
    const layout = createClassComponent({
      component: AppLayout,
      target: document.body,
      props: {
        conversationStore,
        connectionStore: mockConnectionStore(),
        streamingStore: mockStreamingStore(),
        topicStore: mockTopicStore(),
        pendingStart: { topicId: "default", agentId: null },
        onRetry: () => {},
        activeSurface: "conversation",
        surfacesStore: mockSurfacesStore(),
        onSelectSurface: () => {},
      },
    });
    expect(
      document.querySelector('main.conversation [aria-label="Conversation"]')
        ?.textContent,
    ).toContain("What is left to ship?");

    layout.$set({ activeSurface: "surfaces" });
    flushSync();

    expect(
      document.querySelector(
        'main.surfaces-surface [aria-label="Conversation"]',
      )?.textContent,
    ).toContain("What is left to ship?");
    expect(selectConversation).not.toHaveBeenCalled();
    expect(loadConversations).not.toHaveBeenCalled();
    expect(refreshActiveConversation).not.toHaveBeenCalled();

    layout.$set({ activeSurface: "conversation" });
    flushSync();

    expect(
      document.querySelector('main.conversation [aria-label="Conversation"]')
        ?.textContent,
    ).toContain("What is left to ship?");
    expect(selectConversation).not.toHaveBeenCalled();
    layout.$destroy();
  });

  it("cycles to another conversation from the work surface", () => {
    const conversationStore = mockConversationStoreWithSessions(
      ["Release checklist review", "Ledger migration"],
      0,
    );
    const selectConversation = vi.spyOn(
      conversationStore,
      "selectConversation",
    );
    component = mountLayout({
      activeSurface: "surfaces",
      surfacesStore: mockSurfacesStore(),
      conversationStore,
    });
    const selector = document.querySelector(
      'main.surfaces-surface [aria-label="Conversation"] select[aria-label="Active conversation"]',
    ) as HTMLSelectElement;
    expect(
      Array.from(selector.options).map((option) => option.textContent?.trim()),
    ).toEqual(["Release checklist review", "Ledger migration"]);
    expect(selector.value).toBe("conv-1");

    selector.value = "conv-2";
    selector.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    // Reached from where the user was standing: the work surface is still on
    // screen, so cycling cost no surface.
    expect(selectConversation).toHaveBeenCalledWith("conv-2");
    expect(
      document.querySelector('main.surfaces-surface [aria-label="Surface"] h2')
        ?.textContent,
    ).toBe("Release checklist");
  });

  it("opens the surface the URL names and keeps the address in step with the one opened", () => {
    const onNavigate = vi.fn();
    const surfacesStore = mockSurfacesStore();
    const openSurface = vi.spyOn(surfacesStore, "openSurface");
    component = mountLayout({
      activeSurface: "surfaces",
      surfacesStore,
      onNavigate,
      conversationStore: mockConversationStoreWithMessages([]),
      routePath: "/conversations/conv-1/surfaces/surface-1",
    });
    flushSync();

    // The routed surface is already the open one, so nothing is refetched...
    expect(openSurface).not.toHaveBeenCalled();
    // ...and choosing it from the list addresses it.
    const entry = document.querySelector(
      'main.surfaces-surface aside[aria-label="Surfaces"] li button',
    ) as HTMLButtonElement;
    flushSync(() => entry.click());
    expect(onNavigate).toHaveBeenCalledWith(
      "/conversations/conv-1/surfaces/surface-1",
    );
  });

  it("reaches the library from the work surface", () => {
    const onNavigate = vi.fn();
    component = mountLayout({
      activeSurface: "surfaces",
      surfacesStore: mockSurfacesStore(),
      onNavigate,
      conversationStore: mockConversationStoreWithMessages([]),
    });
    const link = document.querySelector(
      "main.surfaces-surface .library-link",
    ) as HTMLButtonElement;
    flushSync(() => link.click());
    expect(onNavigate).toHaveBeenCalledWith("/library");
  });

  it("starts a conversation from the work surface", () => {
    const onNewConversation = vi.fn();
    component = mountLayout({
      activeSurface: "surfaces",
      surfacesStore: mockSurfacesStore(),
      conversationStore: mockConversationStoreWithSessions(
        ["Release checklist review"],
        0,
      ),
      onNewConversation,
    });

    flushSync(() =>
      (
        document.querySelector(
          'main.surfaces-surface [aria-label="Conversation"] button.new-session',
        ) as HTMLButtonElement
      ).click(),
    );

    expect(onNewConversation).toHaveBeenCalledOnce();
  });
});
