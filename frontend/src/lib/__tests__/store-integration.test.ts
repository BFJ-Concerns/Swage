/**
 * Cross-task integration tests for store coordination.
 *
 * These tests verify behaviour that only emerges when multiple stores interact:
 *
 * - Streaming store [cl9v] → Conversation store [xfkw]:
 *   When a turn completes, the streaming store must push the finalised assistant
 *   message into the conversation store's active conversation. Neither store can
 *   prove this alone — the streaming store accumulates tokens, the conversation
 *   store owns messages.
 *
 * - Connection store [ycn1] → Conversation store [xfkw]:
 *   On WebSocket reconnect, the connection store must trigger the conversation
 *   store to re-fetch from HTTP, reconciling any state changes that happened
 *   while disconnected.
 *
 * Journey proved: Conversational Assistance — send message, stream tokens,
 * finalise turn, persist to conversation history.
 */
import { describe, it, expect, vi } from "vitest";

import { createStreamingStore } from "../stores/streaming";
import { createConversationStore } from "../stores/conversations";
import { createConnectionStore } from "../stores/connection";
import type { StatusChangeHandler, ServerEventHandler } from "../api/websocket";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Seed an active conversation into the store by calling addMessage on a
 * store that has been set up with a fake active conversation via the API mock.
 */
async function seedActiveConversation(store: ReturnType<typeof createConversationStore>) {
  // Mock the conversation API so createConversation returns a fake response.
  const api = await import("../api/conversations");
  const createSpy = vi.spyOn(api, "createConversation").mockResolvedValue({
    id: "conv-1",
    space_id: "proj-1",
    created_at: new Date().toISOString(),
    last_activity: new Date().toISOString(),
  });
  const listSpy = vi.spyOn(api, "listConversations").mockResolvedValue([]);

  await store.createConversation("proj-1");

  createSpy.mockRestore();
  listSpy.mockRestore();
}

/**
 * Create a mock WS client factory that captures the event/status handlers
 * so tests can simulate WebSocket events and status transitions.
 */
function makeMockWsClient() {
  let eventHandler: ServerEventHandler | null = null;
  let statusHandler: StatusChangeHandler | null = null;

  const client = {
    connect: vi.fn(),
    send: vi.fn(),
    disconnect: vi.fn(),
    onEvent(handler: ServerEventHandler) {
      eventHandler = handler;
    },
    onStatusChange(handler: StatusChangeHandler) {
      statusHandler = handler;
    },
  };

  return {
    factory: () => client,
    client,
    fireEvent: (event: Parameters<ServerEventHandler>[0]) => eventHandler?.(event),
    fireStatus: (status: Parameters<StatusChangeHandler>[0]) => statusHandler?.(status),
  };
}

// ---------------------------------------------------------------------------
// Streaming → Conversation store coordination
// ---------------------------------------------------------------------------

describe("streaming store → conversation store: turn lifecycle", () => {
  it("appends the user message to conversation store when a turn starts", async () => {
    const conversationStore = createConversationStore();
    await seedActiveConversation(conversationStore);

    // Wire the streaming store to the conversation store via constructor injection.
    const streamingStore = createStreamingStore({
      conversationStore,
    });

    streamingStore.startTurn("Hello, Swage");

    const active = conversationStore.activeConversation;
    expect(active).not.toBeNull();
    expect(active!.messages).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          role: "user",
          content: "Hello, Swage",
        }),
      ]),
    );
  });

  it("accumulates tokens and finalises the assistant message on turn_complete", async () => {
    const conversationStore = createConversationStore();
    await seedActiveConversation(conversationStore);

    const streamingStore = createStreamingStore({
      conversationStore,
    });

    streamingStore.startTurn("What is Rust?");

    streamingStore.handleServerEvent({ type: "token", content: "Rust is " });
    streamingStore.handleServerEvent({ type: "token", content: "a systems " });
    streamingStore.handleServerEvent({ type: "token", content: "language." });

    expect(streamingStore.streamedContent).toBe("Rust is a systems language.");
    expect(streamingStore.isStreaming).toBe(true);

    streamingStore.handleServerEvent({
      type: "turn_complete",
      turn_id: "turn-123",
    });

    expect(streamingStore.isStreaming).toBe(false);
    expect(streamingStore.streamedContent).toBe("");

    const messages = conversationStore.activeConversation!.messages;
    // User message + assistant message
    expect(messages).toHaveLength(2);
    expect(messages[1]).toEqual(
      expect.objectContaining({
        role: "assistant",
        content: "Rust is a systems language.",
        turn_id: "turn-123",
      }),
    );
  });

  it("preserves partial response in conversation store on error", async () => {
    const conversationStore = createConversationStore();
    await seedActiveConversation(conversationStore);

    const streamingStore = createStreamingStore({
      conversationStore,
    });

    streamingStore.startTurn("Tell me about Svelte");
    streamingStore.handleServerEvent({ type: "token", content: "Svelte is" });
    streamingStore.handleServerEvent({
      type: "error",
      code: "provider_error",
      message: "rate limited",
    });

    expect(streamingStore.isStreaming).toBe(false);
    expect(streamingStore.turnError).toEqual({
      code: "provider_error",
      message: "rate limited",
    });
    expect(streamingStore.streamedContent).toBe("");

    const messages = conversationStore.activeConversation!.messages;
    const assistantMessages = messages.filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(1);
    expect(assistantMessages[0].content).toBe("Svelte is");
    expect(assistantMessages[0].turn_state).toBe("failed");
  });

  it("preserves partial response on cancellation", async () => {
    const conversationStore = createConversationStore();
    await seedActiveConversation(conversationStore);

    const streamingStore = createStreamingStore({
      conversationStore,
    });

    streamingStore.startTurn("Write a long essay");
    streamingStore.handleServerEvent({
      type: "token",
      content: "Here is the beginning",
    });
    streamingStore.handleServerEvent({ type: "cancelled" });

    expect(streamingStore.isStreaming).toBe(false);
    expect(streamingStore.streamedContent).toBe("");

    const messages = conversationStore.activeConversation!.messages;
    const assistantMessages = messages.filter((m) => m.role === "assistant");
    expect(assistantMessages).toHaveLength(1);
    expect(assistantMessages[0].content).toBe("Here is the beginning");
    expect(assistantMessages[0].turn_state).toBe("interrupted");
  });
});

// ---------------------------------------------------------------------------
// Connection → Conversation store coordination
// ---------------------------------------------------------------------------

describe("connection store → conversation store: reconnect reconciliation", () => {
  it("triggers conversation refresh on reconnect (reconnecting → connected)", () => {
    const refreshSpy = vi.fn();
    const mock = makeMockWsClient();

    // Wire the connection store with a mock WS client and an onReconnect callback.
    const connectionStore = createConnectionStore({
      wsClientFactory: mock.factory,
      onReconnect: refreshSpy,
    });

    // Simulate initial connection.
    connectionStore.connectTo("test-conversation-id");
    mock.fireStatus("connected");
    expect(connectionStore.status).toBe("connected");
    expect(refreshSpy).not.toHaveBeenCalled(); // Initial connect, not reconnect.

    // Simulate disconnect then reconnect.
    mock.fireStatus("reconnecting");
    expect(connectionStore.status).toBe("reconnecting");

    mock.fireStatus("connected");
    expect(connectionStore.status).toBe("connected");

    // The reconnect callback should have fired exactly once.
    expect(refreshSpy).toHaveBeenCalledTimes(1);
  });
});
