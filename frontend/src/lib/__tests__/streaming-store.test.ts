/**
 * Tests for the streaming store.
 *
 * The streaming store coordinates three concerns: accumulating tokens from the
 * gateway, pushing finalised or partial messages to the conversation store, and
 * dispatching user actions via the connection store. Dependencies are injected
 * so this suite runs without any network activity.
 *
 * Scenarios covered:
 *   - Initial state
 *   - startTurn(): sets isStreaming, clears prior state, pushes user message,
 *     sends via connection store
 *   - token events accumulate in streamedContent
 *   - turn_complete: pushes assistant message, resets streaming state
 *   - error: preserves partial response, records turnError, stops streaming
 *   - cancelled: preserves partial response, stops streaming
 *   - cancelTurn() delegates to connection store
 *   - Graceful no-ops when dependencies are not injected
 *   - activeToolCalls: initial empty state
 *   - tool_start: adds in-progress entry to activeToolCalls
 *   - tool_end: updates matching entry by call_id
 *   - turn_complete: clears activeToolCalls after committing
 *   - error/cancelled: preserve activeToolCalls in current state
 *   - startTurn: clears activeToolCalls from prior turn
 */
import { describe, it, expect, vi } from "vitest";

import {
  createStreamingStore,
  withConversationAutoCreate,
} from "../stores/streaming";
import type { ServerEvent } from "../types";

// ---------------------------------------------------------------------------
// Mock dependency factories
// ---------------------------------------------------------------------------

function createMockConversationStore() {
  const messages: Array<{
    role: string;
    content: string;
    timestamp: string;
    turn_state: string;
    turn_id?: string;
    tool_calls?: Array<{ call_id: string }>;
  }> = [];

  return {
    activeConversation: { id: "test-conversation", messages: messages as never[] },
    appendOptimisticUserMessage: vi.fn((msg) => {
      messages.push({ role: "user", turn_state: "pending", ...msg });
    }),
    appendAssistantMessage: vi.fn((msg) => {
      messages.push({ role: "assistant", ...msg });
    }),
    getMessages: () => [...messages],
  };
}

function createMockConnectionStore() {
  return {
    sendMessage: vi.fn(),
    cancelTurn: vi.fn(),
    setStreamActive: vi.fn(),
  };
}

// ---------------------------------------------------------------------------
// Initial state
// ---------------------------------------------------------------------------

describe("initial state", () => {
  it("starts with isStreaming false, empty content, no error", () => {
    const store = createStreamingStore();

    expect(store.isStreaming).toBe(false);
    expect(store.streamedContent).toBe("");
    expect(store.turnError).toBeNull();
    expect(store.providerRetry).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Reconnect reconciliation
// ---------------------------------------------------------------------------

describe("reconnect reconciliation", () => {
  it("clears a live projection when the recovered same turn is terminal", () => {
    const messages = [{
      role: "assistant" as const,
      content: "Recovered response",
      timestamp: "2026-08-24T10:00:00Z",
      turn_state: "completed" as const,
      turn_id: "recovered-turn",
    }];
    const store = createStreamingStore({
      conversationStore: {
        get activeConversation() {
          return { messages };
        },
        appendOptimisticUserMessage: () => {},
        appendAssistantMessage: () => {},
      },
    });

    store.startTurn("Continue the recovered turn");
    const recoveredTurnGeneration = store.activeTurnGeneration;
    store.handleServerEvent({ type: "token", content: "live content" });

    store.reconcileAfterReconnect(recoveredTurnGeneration);

    expect(store.isStreaming).toBe(false);
    expect(store.streamedContent).toBe("");
  });

  it("does not let a delayed old refresh stop a newer stream", () => {
    const messages = [{
      role: "assistant" as const,
      content: "Recovered response",
      timestamp: "2026-08-24T10:00:00Z",
      turn_state: "completed" as const,
      turn_id: "older-turn",
    }];
    const store = createStreamingStore({
      conversationStore: {
        get activeConversation() {
          return { messages };
        },
        appendOptimisticUserMessage: () => {},
        appendAssistantMessage: () => {},
      },
    });

    store.startTurn("Old turn before the refresh");
    const recoveredTurnGeneration = store.activeTurnGeneration;

    // Force the refresh to remain pending while a newer turn becomes live:
    // a message sent meanwhile was queued, and the gateway started the next
    // turn on it when the old one ended.
    store.handleServerEvent({ type: "queue_drained", session_id: "conv-1", turn_id: "newer-turn" });
    store.handleServerEvent({ type: "token", content: "new live content" });

    store.reconcileAfterReconnect(recoveredTurnGeneration);

    expect(store.isStreaming).toBe(true);
    expect(store.streamedContent).toBe("new live content");
  });
});

// ---------------------------------------------------------------------------
// startTurn
// ---------------------------------------------------------------------------

describe("startTurn", () => {
  it("sets isStreaming to true", () => {
    const store = createStreamingStore();
    store.startTurn("Hello");

    expect(store.isStreaming).toBe(true);
  });

  it("clears any previous streamedContent and turnError", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    // Simulate a completed prior turn.
    store.startTurn("First question");
    store.handleServerEvent({ type: "token", content: "old content" });
    store.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "oops",
    });

    // Start a fresh turn — should clear state.
    store.startTurn("Second question");

    expect(store.streamedContent).toBe("");
    expect(store.turnError).toBeNull();
    expect(store.isStreaming).toBe(true);
  });

  it("sends a message typed mid-turn without touching the live stream, and shows it once the gateway queues it", () => {
    const conv = {
      ...createMockConversationStore(),
      appendQueuedMessage: vi.fn(),
      clearQueuedMessages: vi.fn(),
    };
    const conn = createMockConnectionStore();
    const store = createStreamingStore({ conversationStore: conv, connectionStore: conn });

    store.startTurn("first");
    store.handleServerEvent({ type: "token", content: "streaming…" });
    const liveGeneration = store.activeTurnGeneration;

    store.startTurn("a second thought");

    // Sent, but neither appended optimistically nor treated as a new stream.
    expect(conn.sendMessage).toHaveBeenCalledTimes(2);
    expect(conv.appendOptimisticUserMessage).toHaveBeenCalledTimes(1);
    expect(store.streamedContent).toBe("streaming…");
    expect(store.activeTurnGeneration).toBe(liveGeneration);

    const queued = {
      id: "q-1",
      origin: "user" as const,
      content: "a second thought",
      queued_at: "2026-10-04T10:00:00Z",
    };
    store.handleServerEvent({ type: "message_queued", session_id: "conv-9", queued });
    expect(conv.appendQueuedMessage).toHaveBeenCalledWith(queued, "conv-9");

    // The old turn ends and the queue runs: the view sees a turn start.
    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });
    expect(store.isStreaming).toBe(false);
    store.handleServerEvent({ type: "queue_drained", session_id: "conv-9", turn_id: "t-2" });
    expect(conv.clearQueuedMessages).toHaveBeenCalledWith("conv-9");
    expect(store.isStreaming).toBe(true);
    expect(store.streamedContent).toBe("");
  });

  it("routes a recorded message the gateway announces to the conversation store", () => {
    const conv = { ...createMockConversationStore(), appendRecordedMessage: vi.fn() };
    const store = createStreamingStore({ conversationStore: conv });
    const message = {
      id: "msg-brief",
      thread_id: "thread-1",
      role: "user",
      origin: "schedule" as const,
      content: "Summarise the inbox.",
      timestamp: "2026-01-01T00:05:00Z",
      turn_state: "pending" as const,
      tool_calls: [],
    };

    store.handleServerEvent({ type: "message_appended", session_id: "conv-9", message }, "conv-9");

    // Installed by the gateway's own identity for it, on the conversation
    // the event names — never re-derived from the stream.
    expect(conv.appendRecordedMessage).toHaveBeenCalledWith(message, "conv-9");
    expect(store.isStreaming).toBe(false);
  });

  it("pushes the user message to the conversation store", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("What is Rust?");

    expect(conv.appendOptimisticUserMessage).toHaveBeenCalledOnce();
    const call = conv.appendOptimisticUserMessage.mock.calls[0][0];
    expect(call.content).toBe("What is Rust?");
    expect(typeof call.timestamp).toBe("string");
  });

  it("sends the message via the connection store", () => {
    const conn = createMockConnectionStore();
    const conv = createMockConversationStore();
    const store = createStreamingStore({ connectionStore: conn, conversationStore: conv });

    store.startTurn("Tell me about Svelte");

    expect(conn.sendMessage).toHaveBeenCalledWith(
      "test-conversation",
      "Tell me about Svelte",
      undefined,
      undefined,
      null,
    );
  });

  it("delivers a reply to the thread its turn was sent on, not the thread now in view", () => {
    const conv = { ...createMockConversationStore(), activeThreadId: "branch-1" as string | null };
    const conn = createMockConnectionStore();
    const store = createStreamingStore({ conversationStore: conv, connectionStore: conn });

    store.startTurn("On the branch");
    // The person switches back to the root while the turn runs.
    conv.activeThreadId = null;
    store.handleServerEvent({ type: "token", content: "reply" });
    store.handleServerEvent({ type: "turn_complete", turn_id: "turn-b" });

    expect(conv.appendAssistantMessage).toHaveBeenCalledWith(
      expect.objectContaining({ content: "reply", thread_id: "branch-1" }),
      "test-conversation",
    );
  });

  it("sends the turn on the active branch when the conversation store names one", () => {
    const conv = createMockConversationStore();
    const conn = createMockConnectionStore();
    const store = createStreamingStore({
      conversationStore: { ...conv, activeThreadId: "branch-1" },
      connectionStore: conn,
    });

    store.startTurn("Try it this way");

    expect(conn.sendMessage).toHaveBeenCalledWith(
      "test-conversation",
      "Try it this way",
      undefined,
      undefined,
      "branch-1",
    );
  });

  it("does not throw when no dependencies are injected", () => {
    const store = createStreamingStore();

    expect(() => store.startTurn("test")).not.toThrow();
    expect(store.isStreaming).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Token accumulation
// ---------------------------------------------------------------------------

describe("token events", () => {
  it("appends each token to streamedContent", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({ type: "token", content: "Rust " });
    store.handleServerEvent({ type: "token", content: "is " });
    store.handleServerEvent({ type: "token", content: "fast." });

    expect(store.streamedContent).toBe("Rust is fast.");
  });

  it("accumulates tokens across multiple events without resetting", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    for (const word of ["one", " two", " three"]) {
      store.handleServerEvent({ type: "token", content: word });
    }

    expect(store.streamedContent).toBe("one two three");
  });
});

// ---------------------------------------------------------------------------
// turn_complete
// ---------------------------------------------------------------------------

describe("turn_complete", () => {
  it("sets isStreaming to false", () => {
    const store = createStreamingStore();
    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "answer" });

    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    expect(store.isStreaming).toBe(false);
  });

  it("keeps a settled turn in the projection for the conversation that started it", () => {
    let activeConversation = { id: "conv-a", messages: [] };
    const appendAssistantMessage = vi.fn();
    const store = createStreamingStore({
      conversationStore: {
        get activeConversation() {
          return activeConversation;
        },
        appendOptimisticUserMessage: vi.fn(),
        appendAssistantMessage,
      },
    });

    store.startTurn("Question for A");
    store.handleServerEvent({ type: "token", content: "Answer for A" });
    activeConversation = { id: "conv-b", messages: [] };
    store.handleServerEvent({ type: "turn_complete", turn_id: "turn-a" }, "conv-a");

    expect(store.streamFor?.("conv-a").isStreaming).toBe(false);
    expect(store.streamFor?.("conv-b").streamedContent).toBe("");
    expect(appendAssistantMessage).toHaveBeenCalledWith(
      expect.objectContaining({ content: "Answer for A", turn_id: "turn-a" }),
      "conv-a",
    );
  });

  it("keeps concurrent turns in their owning conversation projections", () => {
    let activeConversation = { id: "conv-a", messages: [] };
    const appendAssistantMessage = vi.fn();
    const connection = createMockConnectionStore();
    const store = createStreamingStore({
      conversationStore: {
        get activeConversation() {
          return activeConversation;
        },
        appendOptimisticUserMessage: vi.fn(),
        appendAssistantMessage,
      },
      connectionStore: connection,
    });

    store.startTurn("Question for A");
    store.handleServerEvent({ type: "token", content: "A first" }, "conv-a");

    activeConversation = { id: "conv-b", messages: [] };
    store.startTurn("Question for B");
    store.handleServerEvent({ type: "token", content: "B reply" }, "conv-b");

    expect(store.streamFor?.("conv-a").isStreaming).toBe(true);
    expect(store.streamFor?.("conv-a").streamedContent).toBe("A first");
    expect(store.streamFor?.("conv-b").isStreaming).toBe(true);
    expect(store.streamFor?.("conv-b").streamedContent).toBe("B reply");

    store.handleServerEvent({ type: "turn_complete", turn_id: "turn-b" }, "conv-b");
    expect(appendAssistantMessage).toHaveBeenLastCalledWith(
      expect.objectContaining({ content: "B reply", turn_id: "turn-b" }),
      "conv-b",
    );

    activeConversation = { id: "conv-a", messages: [] };
    store.handleServerEvent({ type: "token", content: " and last" }, "conv-a");
    store.handleServerEvent({ type: "turn_complete", turn_id: "turn-a" }, "conv-a");

    expect(appendAssistantMessage).toHaveBeenLastCalledWith(
      expect.objectContaining({ content: "A first and last", turn_id: "turn-a" }),
      "conv-a",
    );
    expect(connection.sendMessage).toHaveBeenLastCalledWith(
      "conv-b",
      "Question for B",
      undefined,
      undefined,
      null,
    );
  });

  it("resets streamedContent to empty", () => {
    const store = createStreamingStore();
    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "answer" });

    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    expect(store.streamedContent).toBe("");
  });

  it("pushes the finalised assistant message to the conversation store", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "The answer is 42." });
    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    // First message is the optimistic user turn from startTurn.
    // Second is the assistant turn from turn_complete.
    const messages = conv.getMessages();
    expect(messages).toHaveLength(2);
    const assistant = messages.find((m) => m.role === "assistant");
    expect(assistant).toBeDefined();
    expect(assistant!.content).toBe("The answer is 42.");
    expect(assistant!.turn_id).toBe("t-1");
  });

  it("attaches completed tool calls to the finalised assistant message", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("read the file");
    store.handleServerEvent({
      type: "tool_start",
      call_id: "call-1",
      tool: "file_read",
      arguments: { path: "README.md" },
    });
    store.handleServerEvent({
      type: "tool_end",
      call_id: "call-1",
      tool: "file_read",
      result: "contents",
      status: "success",
      duration_ms: 8,
      truncated: false,
      total_bytes: 8,
    });
    store.handleServerEvent({ type: "token", content: "Here is the file." });
    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    const assistant = conv.getMessages().find((message) => message.role === "assistant");
    expect(assistant?.tool_calls).toEqual([
      expect.objectContaining({
        call_id: "call-1",
        tool: "file_read",
        result: "contents",
        status: "success",
      }),
    ]);
    expect(store.activeToolCalls).toEqual([]);
  });

  it("retains completed tool calls when no assistant text arrived", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("read the file");
    store.handleServerEvent({
      type: "tool_start",
      call_id: "call-1",
      tool: "file_read",
      arguments: { path: "README.md" },
    });
    store.handleServerEvent({
      type: "tool_end",
      call_id: "call-1",
      tool: "file_read",
      result: "contents",
      status: "success",
      duration_ms: 8,
      truncated: false,
      total_bytes: 8,
    });
    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    const assistant = conv.getMessages().find((message) => message.role === "assistant");
    expect(assistant).toEqual(
      expect.objectContaining({
        content: "",
        turn_id: "t-1",
        tool_calls: [expect.objectContaining({ call_id: "call-1" })],
      }),
    );
    expect(store.activeToolCalls).toEqual([]);
  });

  it("does not push an empty assistant message when no tokens arrived", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    // No token events — immediate turn_complete (edge case).
    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    const messages = conv.getMessages();
    const assistantMessages = messages.filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// error
// ---------------------------------------------------------------------------

describe("error events", () => {
  it("sets isStreaming to false", () => {
    const store = createStreamingStore();
    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "partial" });

    store.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "rate limited",
    });

    expect(store.isStreaming).toBe(false);
  });

  it("records the turnError", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "rate limited",
    });

    expect(store.turnError).toEqual({
      code: "provider_error",
      message: "rate limited",
    });
  });

  it("preserves partial response in the conversation store", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "partial answer" });
    store.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "oops",
    });

    const assistantMessages = conv
      .getMessages()
      .filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(1);
    expect(assistantMessages[0].content).toBe("partial answer");
  });

  const offscreenTerminalEvents: ServerEvent[] = [
    { type: "error", code: "provider_error", message: "stopped" },
    { type: "output_schema_failed", turn_id: "turn-a", attempts: 1, errors: [] },
    { type: "shutdown", reason: "stopped" },
  ];

  it.each(offscreenTerminalEvents)("settles an off-screen partial response with its owning conversation for %o", (event) => {
    let activeConversation = { id: "conv-a", messages: [] };
    const appendAssistantMessage = vi.fn();
    const store = createStreamingStore({
      conversationStore: {
        get activeConversation() {
          return activeConversation;
        },
        appendOptimisticUserMessage: vi.fn(),
        appendAssistantMessage,
      },
    });

    store.startTurn("Question for A");
    store.handleServerEvent({ type: "token", content: "Partial A" }, "conv-a");
    activeConversation = { id: "conv-b", messages: [] };
    store.handleServerEvent(event, "conv-a");

    expect(appendAssistantMessage).toHaveBeenCalledWith(
      expect.objectContaining({ content: "Partial A", turn_state: "failed" }),
      "conv-a",
    );
    expect(store.streamFor?.("conv-a").streamedContent).toBe("");
    expect(store.streamFor?.("conv-a").isStreaming).toBe(false);
  });

  it("does not push an empty assistant message when no tokens arrived", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    store.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "oops",
    });

    const assistantMessages = conv
      .getMessages()
      .filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// cancelled
// ---------------------------------------------------------------------------

describe("cancelled events", () => {
  it("sets isStreaming to false", () => {
    const store = createStreamingStore();
    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "partial" });

    store.handleServerEvent({ type: "cancelled" });

    expect(store.isStreaming).toBe(false);
  });

  it("does not set turnError on cancel", () => {
    const store = createStreamingStore();
    store.startTurn("question");
    store.handleServerEvent({ type: "cancelled" });

    expect(store.turnError).toBeNull();
  });

  it("preserves partial response in the conversation store", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "begun answer" });
    store.handleServerEvent({ type: "cancelled" });

    const assistantMessages = conv
      .getMessages()
      .filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(1);
    expect(assistantMessages[0].content).toBe("begun answer");
  });

  it("does not push when cancelled with no content accumulated", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    store.handleServerEvent({ type: "cancelled" });

    const assistantMessages = conv
      .getMessages()
      .filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// cancelTurn
// ---------------------------------------------------------------------------

describe("cancelTurn", () => {
  it("delegates to the connection store", () => {
    const conn = createMockConnectionStore();
    const conv = createMockConversationStore();
    const store = createStreamingStore({ connectionStore: conn, conversationStore: conv });

    store.startTurn("cancel me");
    store.cancelTurn();

    expect(conn.cancelTurn).toHaveBeenCalledWith("test-conversation");
  });

  it("does not throw when no connection store is injected", () => {
    const store = createStreamingStore();

    expect(() => store.cancelTurn()).not.toThrow();
  });
});

// ---------------------------------------------------------------------------
// activeToolCalls — initial state
// ---------------------------------------------------------------------------

describe("activeToolCalls initial state", () => {
  it("starts as an empty array", () => {
    const store = createStreamingStore();

    expect(store.activeToolCalls).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// tool_start events
// ---------------------------------------------------------------------------

describe("tool_start events", () => {
  it("adds a new in-progress entry to activeToolCalls", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "read_file",
      arguments: { path: "/tmp/foo" },
    });

    expect(store.activeToolCalls).toHaveLength(1);
    const entry = store.activeToolCalls[0];
    expect(entry.call_id).toBe("c1");
    expect(entry.tool).toBe("read_file");
    expect(entry.arguments).toEqual({ path: "/tmp/foo" });
  });

  it("sets result, status, and duration_ms to null for in-progress calls", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "shell",
      arguments: { command: "ls" },
    });

    const entry = store.activeToolCalls[0];
    expect(entry.result).toBeNull();
    expect(entry.status).toBeNull();
    expect(entry.duration_ms).toBeNull();
  });

  it("accumulates multiple tool_start entries in order", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "read_file",
      arguments: {},
    });
    store.handleServerEvent({
      type: "tool_start",
      call_id: "c2",
      tool: "shell",
      arguments: {},
    });

    expect(store.activeToolCalls).toHaveLength(2);
    expect(store.activeToolCalls[0].call_id).toBe("c1");
    expect(store.activeToolCalls[1].call_id).toBe("c2");
  });
});

// ---------------------------------------------------------------------------
// tool_end events
// ---------------------------------------------------------------------------

describe("tool_end events", () => {
  it("updates the matching entry with result, status, and duration", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "read_file",
      arguments: { path: "/etc/hosts" },
    });
    store.handleServerEvent({
      type: "tool_end",
      call_id: "c1",
      tool: "read_file",
      result: "127.0.0.1 localhost",
      status: "success",
      duration_ms: 42,
      truncated: false,
      total_bytes: null,
    });

    const entry = store.activeToolCalls[0];
    expect(entry.result).toBe("127.0.0.1 localhost");
    expect(entry.status).toBe("success");
    expect(entry.duration_ms).toBe(42);
    expect(entry.truncated).toBe(false);
    expect(entry.total_bytes).toBeNull();
  });

  it("updates only the matching call when multiple are active", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "read_file",
      arguments: {},
    });
    store.handleServerEvent({
      type: "tool_start",
      call_id: "c2",
      tool: "shell",
      arguments: {},
    });
    store.handleServerEvent({
      type: "tool_end",
      call_id: "c1",
      tool: "read_file",
      result: "contents",
      status: "success",
      duration_ms: 10,
      truncated: false,
      total_bytes: 8,
    });

    // c1 is resolved, c2 is still in-progress.
    expect(store.activeToolCalls[0].status).toBe("success");
    expect(store.activeToolCalls[1].status).toBeNull();
  });

  it("handles tool_end with status: 'error'", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "write_file",
      arguments: {},
    });
    store.handleServerEvent({
      type: "tool_end",
      call_id: "c1",
      tool: "write_file",
      result: "Permission denied",
      status: "error",
      duration_ms: 3,
      truncated: false,
      total_bytes: null,
    });

    expect(store.activeToolCalls[0].status).toBe("error");
    expect(store.activeToolCalls[0].result).toBe("Permission denied");
  });

  it("does not throw when tool_end arrives for an unknown call_id", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    expect(() =>
      store.handleServerEvent({
        type: "tool_end",
        call_id: "unknown",
        tool: "read_file",
        result: "data",
        status: "success",
        duration_ms: 5,
        truncated: false,
        total_bytes: null,
      }),
    ).not.toThrow();
  });
});

// ---------------------------------------------------------------------------
// turn_complete clears activeToolCalls
// ---------------------------------------------------------------------------

describe("turn_complete and activeToolCalls", () => {
  it("clears activeToolCalls when the turn completes", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "read_file",
      arguments: {},
    });
    store.handleServerEvent({
      type: "tool_end",
      call_id: "c1",
      tool: "read_file",
      result: "data",
      status: "success",
      duration_ms: 10,
      truncated: false,
      total_bytes: null,
    });
    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    expect(store.activeToolCalls).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// error/cancelled preserve activeToolCalls
// ---------------------------------------------------------------------------

describe("error preserves activeToolCalls", () => {
  it("keeps activeToolCalls intact when an error occurs mid-turn", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "shell",
      arguments: { command: "ls" },
    });
    store.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "rate limited",
    });

    // The in-progress tool call should remain so the UI can show what was
    // happening when the error occurred.
    expect(store.activeToolCalls).toHaveLength(1);
    expect(store.activeToolCalls[0].call_id).toBe("c1");
    expect(store.activeToolCalls[0].status).toBeNull();
  });
});

describe("cancelled preserves activeToolCalls", () => {
  it("keeps activeToolCalls intact when the turn is cancelled", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "read_file",
      arguments: {},
    });
    store.handleServerEvent({ type: "cancelled" });

    expect(store.activeToolCalls).toHaveLength(1);
    expect(store.activeToolCalls[0].call_id).toBe("c1");
  });
});

// ---------------------------------------------------------------------------
// startTurn clears activeToolCalls from the previous turn
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// shutdown events
// ---------------------------------------------------------------------------

describe("shutdown events", () => {
  it("sets isStreaming to false", () => {
    const store = createStreamingStore();
    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "partial" });

    store.handleServerEvent({ type: "shutdown", reason: "admin shutdown request" });

    expect(store.isStreaming).toBe(false);
  });

  it("records a turnError with code 'gateway_shutdown' and the reason as the message", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({ type: "shutdown", reason: "admin shutdown request" });

    expect(store.turnError).toEqual({
      code: "gateway_shutdown",
      message: "admin shutdown request",
    });
  });

  it("preserves partial response in the conversation store", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "partial" });
    store.handleServerEvent({ type: "shutdown", reason: "admin shutdown request" });

    const assistantMessages = conv
      .getMessages()
      .filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(1);
    expect(assistantMessages[0].content).toBe("partial");
  });
});

// ---------------------------------------------------------------------------
// output_schema_failed events
// ---------------------------------------------------------------------------

describe("output_schema_failed events", () => {
  it("sets isStreaming to false and records a protocol-facing turn error", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "output_schema_failed",
      turn_id: "t-1",
      attempts: 3,
      errors: [{ path: "/answer", message: "is required" }],
    });

    expect(store.isStreaming).toBe(false);
    expect(store.turnError).toEqual({
      code: "output_schema_failed",
      message:
        "Structured output validation failed after 3 attempts: /answer: is required",
    });
  });

  it("preserves partial response in the conversation store", () => {
    const conv = createMockConversationStore();
    const store = createStreamingStore({ conversationStore: conv });

    store.startTurn("question");
    store.handleServerEvent({ type: "token", content: "partial" });
    store.handleServerEvent({
      type: "output_schema_failed",
      turn_id: "t-1",
      attempts: 2,
      errors: [{ path: "", message: "response was not valid JSON" }],
    });

    const assistantMessages = conv
      .getMessages()
      .filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(1);
    expect(assistantMessages[0].content).toBe("partial");
  });
});

// ---------------------------------------------------------------------------
// context_assembled events
// ---------------------------------------------------------------------------

describe("context_assembled events", () => {
  const report = {
    assembly_name: "default",
    elements: [],
    total_tokens: 0,
    budget: null,
    budget_remaining: null,
    compaction_applied: [],
    warnings: [],
    errors: [],
    declared_omissions: [],
    duration: { secs: 0, nanos: 0 },
  };

  it("does not throw or crash the streaming store", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    expect(() =>
      store.handleServerEvent({
        type: "context_assembled",
        turn_id: "t-1",
        report,
      }),
    ).not.toThrow();
  });

  it("does not change isStreaming state", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "context_assembled",
      turn_id: "t-1",
      report,
    });

    // Assembly reporting does not affect the streaming lifecycle.
    expect(store.isStreaming).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// provider retry events
// ---------------------------------------------------------------------------

describe("provider retry events", () => {
  it("records live retry wait progress", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "provider_retry_started",
      turn_id: "t-1",
      attempt: 2,
      max_retries: 3,
      provider: "anthropic",
      wait_ms: 4_000,
    });

    expect(store.providerRetry).toEqual({
      turn_id: "t-1",
      attempt: 2,
      max_retries: 3,
      provider: "anthropic",
      wait_ms: 4_000,
      status: "waiting",
    });
  });

  it("clears retry progress when a turn completes", () => {
    const store = createStreamingStore();
    store.startTurn("question");

    store.handleServerEvent({
      type: "provider_retry_started",
      turn_id: "t-1",
      attempt: 1,
      max_retries: 3,
      provider: "openai",
      wait_ms: 500,
    });
    store.handleServerEvent({ type: "turn_complete", turn_id: "t-1" });

    expect(store.providerRetry).toBeNull();
  });
});

describe("startTurn clears activeToolCalls", () => {
  it("resets activeToolCalls when a new turn begins", () => {
    const store = createStreamingStore();

    // First turn — generates a tool call, then errors out.
    store.startTurn("first question");
    store.handleServerEvent({
      type: "tool_start",
      call_id: "c1",
      tool: "shell",
      arguments: {},
    });
    store.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "oops",
    });
    expect(store.activeToolCalls).toHaveLength(1);

    // Second turn — should start with a clean slate.
    store.startTurn("second question");
    expect(store.activeToolCalls).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// withConversationAutoCreate
// ---------------------------------------------------------------------------
// The wrapper handed to the composer: startTurn with no conversation in scope
// creates one first, then dispatches through the wrapped store.

describe("withConversationAutoCreate", () => {
  function createWrappedStore(hasActive: () => boolean) {
    const conversation = createMockConversationStore();
    const connection = createMockConnectionStore();
    const base = createStreamingStore({
      conversationStore: conversation,
      connectionStore: connection,
    });

    // Deferred so tests control exactly when creation settles.
    let settleCreate!: (id: string | null) => void;
    const createConversation = vi.fn(
      () =>
        new Promise<string | null>((resolve) => {
          settleCreate = resolve;
        }),
    );

    const store = withConversationAutoCreate(base, {
      hasActiveConversation: hasActive,
      createConversation,
    });

    return {
      store,
      base,
      connection,
      createConversation,
      settleCreate: (id: string | null) => {
        if (id) conversation.activeConversation.id = id;
        settleCreate(id);
      },
    };
  }

  it("delegates directly when a conversation is active", () => {
    const { store, connection, createConversation } = createWrappedStore(
      () => true,
    );

    store.startTurn("hello", "model-a");

    expect(createConversation).not.toHaveBeenCalled();
    expect(connection.sendMessage).toHaveBeenCalledWith(
      "test-conversation",
      "hello",
      "model-a",
      undefined,
      null,
    );
  });

  it("creates a conversation first when none is active, then starts the turn", async () => {
    const { store, base, connection, createConversation, settleCreate } =
      createWrappedStore(() => false);

    store.startTurn("first message", "model-a", "researcher", "high");

    // Dispatch must wait for the conversation to exist.
    expect(createConversation).toHaveBeenCalledWith("model-a", "researcher", "high");
    expect(connection.sendMessage).not.toHaveBeenCalled();

    settleCreate("conv-9");
    await vi.waitFor(() =>
      expect(connection.sendMessage).toHaveBeenCalledWith(
        "conv-9",
        "first message",
        "model-a",
        undefined,
        null,
      ),
    );
    expect(base.streamFor?.("conv-9").isStreaming).toBe(true);
    expect(connection.setStreamActive).toHaveBeenCalledWith("conv-9", true);
  });

  it("reports streaming during the creation window and ignores a second send", async () => {
    const { store, createConversation, settleCreate } = createWrappedStore(
      () => false,
    );

    store.startTurn("first");
    expect(store.isStreaming).toBe(true);

    // A second send while creation is in flight must not fork a second
    // conversation.
    store.startTurn("second");
    expect(createConversation).toHaveBeenCalledTimes(1);

    settleCreate("conv-9");
    await vi.waitFor(() => expect(store.isStreaming).toBe(true));
  });

  it("surfaces a failed creation as a turn error carrying the content", async () => {
    const { store, connection, settleCreate } = createWrappedStore(() => false);

    store.startTurn("precious words");
    settleCreate(null);

    await vi.waitFor(() => {
      expect(store.turnError).not.toBeNull();
    });
    expect(store.turnError?.code).toBe("conversation_create_failed");
    // The composer cleared its draft on send — the error echoes the content
    // back so it isn't lost.
    expect(store.turnError?.message).toContain("precious words");
    expect(store.isStreaming).toBe(false);
    expect(connection.sendMessage).not.toHaveBeenCalled();
  });
});
