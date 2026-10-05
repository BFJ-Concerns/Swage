// @vitest-environment jsdom
/**
 * Tests for settled responses and tool call rendering in MessageDisplay.
 *
 * Verifies that ToolCallDisplay components appear for both persisted
 * message history and live streaming tool activity.
 *
 * Scenarios covered:
 *   - Settled partial content renders once after streaming ends
 *   - Interrupted and failed responses have distinct user-facing notices
 *   - Tool calls from committed messages render after the assistant text
 *   - Each tool call produces a .tool-call element
 *   - Multiple tool calls on one message render in order
 *   - Messages without tool_calls do not produce .tool-call elements
 *   - Live activeToolCalls render before the streaming content block
 *   - Live tool calls without result have isLive=true (pulse indicator)
 *   - Live tool calls with a result have isLive=false (no pulse)
 */
import { describe, it, expect, afterEach, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
vi.mock("../api/turns", () => ({
  fetchTurnContext: vi.fn(),
}));

import { fetchTurnContext } from "../api/turns";
import MessageDisplay from "../components/MessageDisplay.svelte";
import type { ConversationStore } from "../stores/conversations";
import {
  createStreamingStore,
  type ProviderRetryState,
  type StreamingStore,
} from "../stores/streaming";
import type { ConversationDetail, MessageView, ToolCallView } from "../types";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeToolCall(overrides: Partial<ToolCallView> = {}): ToolCallView {
  return {
    call_id: "c1",
    tool: "read_file",
    arguments: { path: "/tmp/foo" },
    result: "file contents",
    status: "success",
    duration_ms: 10,
    truncated: false,
    total_bytes: 12,
    ...overrides,
  };
}

// Counter ensures each message gets a unique timestamp so Svelte's keyed
// each block doesn't complain about duplicates within a single test.
let _msgSeq = 0;

function makeAssistantMessage(
  content: string,
  tool_calls?: ToolCallView[],
  model?: string | null,
  turnState: MessageView["turn_state"] = "completed",
  turnId?: string,
): MessageView {
  _msgSeq += 1;
  return {
    role: "assistant",
    content,
    timestamp: `2026-03-20T10:00:${String(_msgSeq).padStart(2, "0")}Z`,
    turn_state: turnState,
    ...(tool_calls ? { tool_calls } : {}),
    ...(model !== undefined ? { model } : {}),
    ...(turnId !== undefined ? { turn_id: turnId } : {}),
  };
}

function makeConversationStore(
  messages: MessageView[],
  branchFrom?: (messageId: string) => Promise<void>,
): ConversationStore {
  const detail: ConversationDetail = {
    id: "conv-1",
    agent_id: "gateway",
    topic_id: "proj-1",
    created_at: "2026-03-20T10:00:00Z",
    last_activity: "2026-03-20T10:00:00Z",
    messages,
  };
  return {
    get conversations() {
      return [];
    },
    get activeConversation() {
      return detail;
    },
    get isLoading() {
      return false;
    },
    loadConversations: async () => {},
    selectConversation: async () => {},
    createConversation: async () => {},
    deleteConversation: async () => {},
    appendOptimisticUserMessage: () => {},
    appendAssistantMessage: (message) => {
      messages.push({ role: "assistant", ...message });
    },
    appendRecordedMessage: () => {},
    appendQueuedMessage: () => {},
    clearQueuedMessages: () => {},
    clearActiveConversation: () => {},
    refreshActiveConversation: async () => {},
    ...(branchFrom ? { branchFrom } : {}),
  };
}

function makeStreamingStore(
  activeToolCalls: ToolCallView[] = [],
  isStreaming = false,
  providerRetry: ProviderRetryState | null = null,
): StreamingStore {
  return {
    get isStreaming() {
      return isStreaming;
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
      return activeToolCalls;
    },
    get providerRetry() {
      return providerRetry;
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

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

let component: MountedComponent | undefined;

afterEach(() => {
  if (component) {
    unmount(component);
    component = undefined;
  }
  while (document.body.firstChild) {
    document.body.removeChild(document.body.firstChild);
  }
  _msgSeq = 0;
  vi.resetAllMocks();
});

describe("turn context inspector trigger", () => {
  it("switches the shared inspector when two completed turns are opened in sequence", async () => {
    vi.mocked(fetchTurnContext).mockImplementation(async (turnId) => ({
      assembly_name: "balanced",
      elements: [],
      total_tokens: turnId === "turn-42" ? 12 : 24,
      budget: null,
      budget_remaining: null,
      compaction_applied: [],
      warnings: [],
      errors: [],
      declared_omissions: [],
      effective_tool_permissions: undefined,
      provider_visible: undefined,
      duration: { secs: 0, nanos: 1 },
    }));
    const store = makeConversationStore([
      makeAssistantMessage("First recorded response", undefined, undefined, "completed", "turn-42"),
      makeAssistantMessage("Second recorded response", undefined, undefined, "completed", "turn-43"),
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    const contextLinks = document.querySelectorAll<HTMLButtonElement>("button.context-link");
    contextLinks[0].click();

    await vi.waitFor(() => expect(fetchTurnContext).toHaveBeenCalledWith("turn-42"));
    expect(document.body.textContent?.replace(/\s+/g, " ")).toContain("request total: 12 tokens");
    expect(document.querySelector(".summary")?.closest(".message-display")).toBeNull();
    expect(document.querySelector(".summary")?.closest(".message")).toBeNull();

    contextLinks[1].click();

    await vi.waitFor(() => expect(fetchTurnContext).toHaveBeenCalledWith("turn-43"));
    expect(document.querySelectorAll(".context-inspector")).toHaveLength(1);
    expect(document.body.textContent?.replace(/\s+/g, " ")).toContain("request total: 24 tokens");

    (document.querySelector('button[aria-label="Close"]') as HTMLButtonElement).click();
    await vi.waitFor(() => expect(document.querySelector(".context-inspector")).toBeNull());
  });
});

describe("assistant Markdown time blanks", () => {
  it.each(["__:__", "___:___", "____:____"])("preserves the time blank %s", (content) => {
    component = mount(MessageDisplay, {
      target: document.body,
      props: { store: makeConversationStore([makeAssistantMessage(content)]) },
    });

    expect(document.querySelector(".assistant-content")?.textContent?.trim()).toBe(content);
  });

  it("preserves Arrived and Left time blanks in a settled template", () => {
    const content = "Arrived at __:__.\nLeft at __:__.";
    component = mount(MessageDisplay, {
      target: document.body,
      props: { store: makeConversationStore([makeAssistantMessage(content)]) },
    });

    expect(document.querySelector(".assistant-content")?.textContent?.trim()).toBe(content);
  });

  it("preserves Arrived and Left time blanks in a streaming template", () => {
    const content = "Arrived at __:__.\nLeft at __:__.";
    component = mount(MessageDisplay, {
      target: document.body,
      props: {
        store: makeConversationStore([]),
        streamingStore: { ...makeStreamingStore([], true), streamedContent: content },
      },
    });

    expect(document.querySelector(".streaming .assistant-content p")?.textContent?.trim()).toBe(content);
  });

  it("control: preserves underscores separated by spaces", () => {
    component = mount(MessageDisplay, {
      target: document.body,
      props: { store: makeConversationStore([makeAssistantMessage("a __ b")]) },
    });

    expect(document.querySelector(".assistant-content")?.textContent?.trim()).toBe("a __ b");
  });

  it("control: renders ordinary underscore bold emphasis", () => {
    component = mount(MessageDisplay, {
      target: document.body,
      props: { store: makeConversationStore([makeAssistantMessage("__bold__")]) },
    });

    expect(document.querySelector(".assistant-content strong")?.textContent).toBe("bold");
  });
});

// ---------------------------------------------------------------------------
// Settled partial responses
// ---------------------------------------------------------------------------

describe("settled partial responses", () => {
  it("renders settled partial content only once after the turn ends", () => {
    const partialContent = "The response reached this point";
    const store = makeConversationStore([]);
    const streamingStore = createStreamingStore({ conversationStore: store });

    streamingStore.startTurn("Continue until stopped");
    streamingStore.handleServerEvent({
      type: "token",
      content: partialContent,
    });
    streamingStore.handleServerEvent({ type: "cancelled" });

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore },
    });

    expect(
      document.body.textContent?.match(new RegExp(partialContent, "g")),
    ).toHaveLength(1);
  });

  it("shows an output-limited streamed reply as incomplete", () => {
    const store = makeConversationStore([]);
    const streamingStore = createStreamingStore({ conversationStore: store });
    streamingStore.startTurn("Compare");
    streamingStore.handleServerEvent({ type: "token", content: "The answer ends at a" });
    streamingStore.handleServerEvent(JSON.parse('{"type":"turn_complete","turn_id":"limited-turn","incomplete_reason":"length"}'));
    component = mount(MessageDisplay, { target: document.body, props: { store, streamingStore } });
    expect(document.body.textContent).toContain("The answer ends at a");
    expect(document.body.textContent).toContain("Response incomplete");
    expect(document.body.textContent).not.toContain("Response interrupted");
  });

  it("distinguishes interrupted and failed responses for the user", () => {
    const store = makeConversationStore([
      makeAssistantMessage("Stopped partial", undefined, undefined, "interrupted"),
      makeAssistantMessage("Failed partial", undefined, undefined, "failed"),
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    expect(document.body.textContent).toContain("Response interrupted");
    expect(document.body.textContent).toContain("Response failed");
    expect(document.body.textContent?.match(/Response interrupted/g)).toHaveLength(1);
  });
});

describe("turns that ended without a reply, as loaded", () => {
  function makeRequest(overrides: Partial<MessageView>): MessageView {
    _msgSeq += 1;
    return {
      id: `m-${_msgSeq}`,
      role: "user",
      origin: "user",
      content: "write the file",
      timestamp: `2026-03-20T10:00:${String(_msgSeq).padStart(2, "0")}Z`,
      turn_id: "turn-1",
      turn_state: "interrupted",
      ...overrides,
    };
  }

  it("shows an interrupted turn's tool calls and state on its request", () => {
    const store = makeConversationStore([
      makeRequest({
        tool_calls: [
          makeToolCall({ call_id: "write", tool: "file_write", status: "success" }),
          makeToolCall({ call_id: "shell", tool: "shell", status: "interrupted", result: null }),
        ],
      }),
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    const request = document.body.querySelector('[aria-label="Your message"]');
    expect(request?.querySelectorAll(".tool-call")).toHaveLength(2);
    expect(request?.textContent).toContain("file_write");
    expect(request?.textContent).toContain("Interrupted before a reply");
  });

  it("shows a failed turn's explanation on its request and retries the request", () => {
    const startTurn = vi.fn();
    const store = makeConversationStore([
      makeRequest({
        content: "Hello?",
        turn_state: "failed",
        failure_message: "provider unreachable: connection refused",
      }),
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: { ...makeStreamingStore(), startTurn } },
    });

    const request = document.body.querySelector('[aria-label="Your message"]');
    expect(request?.querySelector('[role="alert"]')?.textContent).toContain(
      "provider unreachable: connection refused",
    );
    const retry = [...(request?.querySelectorAll("button") ?? [])].find(
      (button) => button.textContent === "Retry",
    );
    retry?.click();
    expect(startTurn).toHaveBeenCalledWith("Hello?");
  });

  it("keeps the explanation and Retry for a failed turn under its partial reply", () => {
    const startTurn = vi.fn();
    const partial = {
      ...makeAssistantMessage("Partial answer", undefined, undefined, "failed", "turn-1"),
      failure_message: "stream reset by provider",
    };
    const store = makeConversationStore([
      makeRequest({ content: "Explain it", turn_state: "failed" }),
      partial,
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: { ...makeStreamingStore(), startTurn } },
    });

    expect(document.body.querySelector('[role="alert"]')?.textContent).toContain(
      "stream reset by provider",
    );
    const retries = [...document.body.querySelectorAll("button")].filter(
      (button) => button.textContent === "Retry",
    );
    expect(retries).toHaveLength(1);
    retries[0].click();
    expect(startTurn).toHaveBeenCalledWith("Explain it");
  });

  it("shows a stopped request settled live, before it has a turn id, as interrupted", () => {
    const store = makeConversationStore([
      {
        role: "user",
        content: "Run sleep 30",
        timestamp: "2026-03-20T10:00:59Z",
        turn_state: "interrupted",
        tool_calls: [makeToolCall({ call_id: "sleep", tool: "shell", status: "interrupted", result: null })],
      },
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    const request = document.body.querySelector('[aria-label="Your message"]');
    expect(request?.querySelectorAll(".tool-call")).toHaveLength(1);
    expect(request?.textContent).toContain("Interrupted before a reply");
  });

  it("shows a running call stopped live with an unknown outcome, before any reload", async () => {
    const api = await import("../api/conversations");
    vi.spyOn(api, "createConversation").mockResolvedValue({
      id: "conv-live",
      agent_id: "gateway",
      topic_id: "proj-1",
      created_at: "2026-03-20T10:00:00Z",
      last_activity: "2026-03-20T10:00:00Z",
    });
    const { createConversationStore } = await import("../stores/conversations");
    const store = createConversationStore();
    await store.createConversation("proj-1");
    const streamingStore = createStreamingStore({ conversationStore: store });

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore },
    });
    streamingStore.startTurn("Run sleep 30");
    streamingStore.handleServerEvent({
      type: "tool_start",
      call_id: "call-sleep",
      tool: "shell",
      arguments: { command: "sleep 30" },
    });
    // On Stop the gateway ends the running call as cancelled with no result
    // before it reports the turn cancelled. The wire carries that missing
    // result as JSON null, which the generated event type does not admit.
    streamingStore.handleServerEvent({
      type: "tool_end",
      call_id: "call-sleep",
      tool: "shell",
      result: null as unknown as string,
      status: "cancelled",
      duration_ms: 6200,
      truncated: false,
    });
    streamingStore.handleServerEvent({ type: "cancelled" });
    flushSync();

    const request = document.body.querySelector('[aria-label="Your message"]');
    expect(request?.querySelector(".tool-call")?.textContent).toContain(
      "its outcome is unknown",
    );
    expect(request?.textContent).toContain("Interrupted before a reply");
  });

  it("leaves a request whose turn replied without a failure account", () => {
    const store = makeConversationStore([
      makeRequest({ content: "Hi", turn_state: "failed" }),
      makeAssistantMessage("Partial", undefined, undefined, "failed", "turn-1"),
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    expect(document.body.textContent).not.toContain("No reply");
    expect(document.body.textContent).toContain("Response failed");
  });
});

// ---------------------------------------------------------------------------
// Historical tool calls (committed messages)
// ---------------------------------------------------------------------------

describe("historical tool calls from committed messages", () => {
  it("renders persisted rejected approvals as failed beside successful calls", () => {
    const store = makeConversationStore([
      makeAssistantMessage("The request was denied and the turn continued.", [
        makeToolCall({ call_id: "successful", tool: "read_file", status: "success" }),
        makeToolCall({
          call_id: "rejected",
          tool: "write_file",
          result: "Tool call rejected: reviewer denied access",
          status: "rejected",
        }),
      ]),
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    const calls = document.querySelectorAll(".tool-calls-history .tool-call");
    expect(calls).toHaveLength(2);
    expect(calls[0].classList.contains("error")).toBe(false);
    expect(calls[1].classList.contains("error")).toBe(true);
    expect(calls[1].querySelector(".error-icon")?.textContent).toBe("✗");
  });

  it("renders a .tool-call element for each tool call on an assistant message", () => {
    const store = makeConversationStore([
      makeAssistantMessage("Here is the file:", [makeToolCall()]),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    const toolCalls = document.querySelectorAll(".tool-call");
    expect(toolCalls.length).toBeGreaterThanOrEqual(1);
  });

  it("renders multiple tool calls in order", () => {
    const store = makeConversationStore([
      makeAssistantMessage("I ran two tools:", [
        makeToolCall({ call_id: "c1", tool: "read_file" }),
        makeToolCall({ call_id: "c2", tool: "shell" }),
      ]),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    const toolCalls = document.querySelectorAll(".tool-call");
    expect(toolCalls.length).toBeGreaterThanOrEqual(2);
  });

  it("renders persisted tool calls before the assistant response", () => {
    const store = makeConversationStore([
      makeAssistantMessage("Here is the result.", [makeToolCall()]),
    ]);

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    const assistant = document.querySelector(".message.assistant");
    const toolCalls = assistant?.querySelector(".tool-calls-history");
    const response = assistant?.querySelector(".assistant-content");
    expect(toolCalls).not.toBeNull();
    expect(response).not.toBeNull();
    expect(
      toolCalls!.compareDocumentPosition(response!) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("does not render .tool-call for messages without tool calls", () => {
    const store = makeConversationStore([
      makeAssistantMessage("Plain response, no tools."),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    const toolCalls = document.querySelectorAll(".tool-call");
    expect(toolCalls.length).toBe(0);
  });

  it("renders assistant content as plain text when markdown rendering is disabled", () => {
    const store = makeConversationStore([
      makeAssistantMessage("**not bold**"),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: {
        store,
        streamingStore: streaming,
        displayPreferences: {
          theme: "system",
          density: "compact",
          message_model_display: "off",
          conversation_rendering: "plain",
          print_marks: "full",
        },
      },
    });

    expect(document.querySelector("strong")).toBeNull();
    expect(document.body.textContent).toContain("**not bold**");
  });

  it("shows the message model id when one is reported", () => {
    const store = makeConversationStore([
      makeAssistantMessage("Plain response.", undefined, "gpt-4o"),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: {
        store,
        streamingStore: streaming,
        displayPreferences: {
          theme: "system",
          density: "compact",
          message_model_display: "assistant",
          conversation_rendering: "markdown",
          print_marks: "full",
        },
      },
    });

    // The chip renders the real model id, not a permanent "Model not reported"
    // placeholder (UI audit f-9: omit the chip when no model is available).
    expect(document.body.textContent).toContain("gpt-4o");
    expect(document.body.textContent).not.toContain("Model not reported");
  });

  it("omits the model chip when the API did not report one", () => {
    const store = makeConversationStore([
      makeAssistantMessage("Plain response."),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: {
        store,
        streamingStore: streaming,
        displayPreferences: {
          theme: "system",
          density: "compact",
          message_model_display: "assistant",
          conversation_rendering: "markdown",
          print_marks: "full",
        },
      },
    });

    expect(document.body.textContent).not.toContain("Model not reported");
    expect(document.querySelector(".message-model")).toBeNull();
  });

  it("renders tool calls from multiple messages independently", () => {
    const store = makeConversationStore([
      makeAssistantMessage("First turn", [makeToolCall({ call_id: "a" })]),
      makeAssistantMessage("Second turn", [
        makeToolCall({ call_id: "b" }),
        makeToolCall({ call_id: "c" }),
      ]),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    // 1 + 2 = 3 tool call elements in total.
    const toolCalls = document.querySelectorAll(".tool-call");
    expect(toolCalls.length).toBeGreaterThanOrEqual(3);
  });

  it("historical tool calls do not show the pulse/in-progress indicator", () => {
    const store = makeConversationStore([
      makeAssistantMessage("Done", [
        makeToolCall({ status: "success" }),
      ]),
    ]);
    const streaming = makeStreamingStore();

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    // Completed historical calls must not show a pulsing indicator.
    const pulse = document.querySelector(".tool-call .pulse");
    expect(pulse).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Live tool calls (activeToolCalls from streaming store)
// ---------------------------------------------------------------------------

describe("live tool calls from streaming store", () => {
  it("renders a live rejected approval as failed beside a successful call", () => {
    const store = makeConversationStore([]);
    const streaming = makeStreamingStore(
      [
        makeToolCall({ call_id: "successful", tool: "read_file", status: "success" }),
        makeToolCall({
          call_id: "rejected",
          tool: "write_file",
          result: "Tool call rejected: reviewer denied access",
          status: "rejected",
        }),
      ],
      true,
    );

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    const calls = document.querySelectorAll(".tool-calls-live .tool-call");
    expect(calls).toHaveLength(2);
    expect(calls[0].classList.contains("error")).toBe(false);
    expect(calls[1].classList.contains("error")).toBe(true);
    expect(calls[1].querySelector(".error-icon")?.textContent).toBe("✗");
  });

  it("renders live tool calls before the streamed response", () => {
    const store = makeConversationStore([]);
    const streaming = {
      ...makeStreamingStore([makeToolCall({ call_id: "live-1" })], true),
      get streamedContent() {
        return "Answer after the tool";
      },
    };

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    const toolCalls = document.querySelector(".tool-calls-live");
    const response = document.querySelector(".message.streaming");
    expect(toolCalls).not.toBeNull();
    expect(response).not.toBeNull();
    expect(toolCalls!.compareDocumentPosition(response!) & Node.DOCUMENT_POSITION_FOLLOWING)
      .not.toBe(0);
  });

  it("renders a .tool-call element for each active tool call", () => {
    const store = makeConversationStore([]);
    const streaming = makeStreamingStore(
      [makeToolCall({ call_id: "live-1", result: null, status: null, duration_ms: null })],
      true,
    );

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    const toolCalls = document.querySelectorAll(".tool-call");
    expect(toolCalls.length).toBeGreaterThanOrEqual(1);
  });

  it("shows the pulse indicator for in-progress live tool calls (no result)", () => {
    const store = makeConversationStore([]);
    const streaming = makeStreamingStore(
      [makeToolCall({ call_id: "live-1", result: null, status: null, duration_ms: null })],
      true,
    );

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    // An in-progress live tool call must show the pulse indicator.
    const pulse = document.querySelector(".tool-call .pulse");
    expect(pulse).not.toBeNull();
  });

  it("does not show the pulse indicator for completed live tool calls (has result)", () => {
    const store = makeConversationStore([]);
    const streaming = makeStreamingStore(
      [makeToolCall({ call_id: "live-1", status: "success" })],
      true,
    );

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    // A completed live tool call should look identical to a historical one.
    const pulse = document.querySelector(".tool-call .pulse");
    expect(pulse).toBeNull();
  });

  it("renders live provider retry progress", () => {
    const store = makeConversationStore([]);
    const streaming = makeStreamingStore([], true, {
      turn_id: "turn-1",
      attempt: 2,
      max_retries: 3,
      provider: "anthropic",
      wait_ms: 4_000,
      status: "waiting",
    });

    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: streaming },
    });

    expect(document.body.textContent).toContain("WAIT 4s");
    expect(document.body.textContent).toContain("retry 2 of 3 · provider · anthropic");
    expect(document.body.textContent).not.toContain("SHELL");
  });
});

// ---------------------------------------------------------------------------
// Branching from a message
// ---------------------------------------------------------------------------

describe("branching from a message", () => {
  let component: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
  });

  it("offers a branch action on every recorded message and none on a pending one", async () => {
    const branchFrom = vi.fn(async () => {});
    const recorded = { ...makeAssistantMessage("recorded"), id: "msg-9" };
    const pending: MessageView = {
      role: "user",
      content: "not yet recorded",
      timestamp: "2026-03-20T10:59:00Z",
      turn_state: "pending",
    };
    component = mount(MessageDisplay, {
      target: document.body,
      props: {
        store: makeConversationStore([recorded, pending], branchFrom),
        streamingStore: makeStreamingStore(),
      },
    });

    const actions = document.querySelectorAll<HTMLButtonElement>(
      'button[aria-label="Branch from this message"]',
    );
    expect(actions).toHaveLength(1);
    actions[0].click();

    await vi.waitFor(() => expect(branchFrom).toHaveBeenCalledWith("msg-9"));
  });

  it("labels a message by who wrote it, never by the role that carried it", () => {
    // A schedule's brief rides the user role to the model, so the model
    // treats it as the task; the person must still see it was not theirs.
    const brief: MessageView = {
      id: "msg-brief",
      role: "user",
      origin: "schedule",
      content: "Summarise the **inbox**.",
      timestamp: "2026-03-20T11:00:00Z",
      turn_state: "pending",
    };
    const typed: MessageView = {
      role: "user",
      content: "Thanks",
      timestamp: "2026-03-20T11:00:01Z",
      turn_state: "pending",
    };
    component = mount(MessageDisplay, {
      target: document.body,
      props: {
        store: makeConversationStore([brief, typed]),
        streamingStore: makeStreamingStore(),
      },
    });

    const labels = Array.from(document.querySelectorAll(".role-label")).map(
      (label) => label.textContent?.trim(),
    );
    expect(labels).toEqual(["Schedule", "You"]);

    const briefElement = document.querySelector('[aria-label="Schedule message"]');
    expect(briefElement).not.toBeNull();
    // Not the person's words: it sits on the assistant's side, and renders
    // as the platform wrote it (markdown), not as typed plain text.
    expect(briefElement!.classList.contains("user")).toBe(false);
    expect(briefElement!.querySelector("strong")?.textContent).toBe("inbox");
    expect(document.querySelector('[aria-label="Your message"]')).not.toBeNull();
  });

  it("shows messages held for the next turn after the recorded ones, labelled as queued", () => {
    const store = makeConversationStore([makeAssistantMessage("so far")]);
    const detail = store.activeConversation!;
    detail.queued = [
      {
        id: "q-1",
        origin: "user",
        content: "and one more thing",
        queued_at: "2026-03-20T11:05:00Z",
      },
    ];
    component = mount(MessageDisplay, {
      target: document.body,
      props: { store, streamingStore: makeStreamingStore() },
    });

    const held = document.querySelector('[aria-label="Queued message"]');
    expect(held).not.toBeNull();
    expect(held!.querySelector(".role-label")?.textContent?.trim()).toBe("You · queued");
    expect(held!.textContent).toContain("and one more thing");
    // Not a recorded message: nothing to branch from.
    expect(held!.querySelector('button[aria-label="Branch from this message"]')).toBeNull();
  });

  it("offers no branch action when the store cannot branch", () => {
    const recorded = { ...makeAssistantMessage("recorded"), id: "msg-9" };
    component = mount(MessageDisplay, {
      target: document.body,
      props: {
        store: makeConversationStore([recorded]),
        streamingStore: makeStreamingStore(),
      },
    });

    expect(document.querySelector('button[aria-label="Branch from this message"]')).toBeNull();
  });
});
