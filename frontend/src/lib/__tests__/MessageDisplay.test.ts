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
import { describe, it, expect, afterEach } from "vitest";
import { mount, unmount } from "svelte";
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
): MessageView {
  _msgSeq += 1;
  return {
    role: "assistant",
    content,
    timestamp: `2026-03-20T10:00:${String(_msgSeq).padStart(2, "0")}Z`,
    turn_state: turnState,
    ...(tool_calls ? { tool_calls } : {}),
    ...(model !== undefined ? { model } : {}),
  };
}

function makeConversationStore(messages: MessageView[]): ConversationStore {
  const detail: ConversationDetail = {
    id: "conv-1",
    space_id: "proj-1",
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
    clearActiveConversation: () => {},
    refreshActiveConversation: async () => {},
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

// ---------------------------------------------------------------------------
// Historical tool calls (committed messages)
// ---------------------------------------------------------------------------

describe("historical tool calls from committed messages", () => {
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
