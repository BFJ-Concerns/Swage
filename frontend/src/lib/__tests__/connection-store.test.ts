/**
 * Tests for the connection store.
 *
 * The store is tested with a mock WebSocket client factory so no real network
 * activity occurs. Each test injects a fresh mock client and drives it by
 * triggering status changes and server events, then asserts the store's
 * public surface reacts correctly.
 *
 * Scenarios covered:
 *   - Initial status is "disconnected"
 *   - connectTo() delegates to the WS client and sets status via callback
 *   - disconnect() delegates and surfaces "disconnected"
 *   - sendMessage() emits a send_message ClientEvent
 *   - cancelTurn() emits a cancel ClientEvent
 *   - Server events are routed to registered handlers
 *   - Error status + message on handshake rejection (mirrors 409 Conflict)
 *   - onReconnect callback fires when reconnect succeeds
 *   - errorMessage cleared when connection recovers
 */
import { describe, it, expect, vi } from "vitest";

import type { ConnectionStatus, ServerEvent } from "../types";
import type { WebSocketClient } from "../api/websocket";
import { createConnectionStore } from "../stores/connection";

// ---------------------------------------------------------------------------
// Mock WS client factory
// ---------------------------------------------------------------------------

interface MockWsClient extends WebSocketClient {
  // Test helpers to drive callbacks registered by the store.
  triggerStatus(s: ConnectionStatus): void;
  triggerEvent(e: ServerEvent): void;
  // Introspection on what the store sent.
  sentEvents(): string[];
  lastConnectedId(): string | undefined;
}

function createMockWsClient(): MockWsClient {
  let statusHandler: ((s: ConnectionStatus) => void) | null = null;
  let eventHandler: ((e: ServerEvent) => void) | null = null;
  const connected: string[] = [];
  const sent: string[] = [];

  const client: MockWsClient = {
    connect: vi.fn((id: string) => {
      connected.push(id);
    }),
    disconnect: vi.fn(),
    send: vi.fn((event) => {
      sent.push(JSON.stringify(event));
    }),
    onEvent: vi.fn((handler) => {
      eventHandler = handler;
    }),
    onStatusChange: vi.fn((handler) => {
      statusHandler = handler;
    }),

    triggerStatus: (s) => statusHandler?.(s),
    triggerEvent: (e) => eventHandler?.(e),
    sentEvents: () => [...sent],
    lastConnectedId: () => connected[connected.length - 1],
  };

  return client;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("initial state", () => {
  it("starts in disconnected status with no error message", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    expect(store.status).toBe("disconnected");
    expect(store.errorMessage).toBeNull();
  });
});

describe("connectTo", () => {
  it("delegates to the WebSocket client with the conversation ID", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");

    expect(mock.connect).toHaveBeenCalledWith("conv-abc");
    expect(mock.lastConnectedId()).toBe("conv-abc");
  });

  it("reflects connected status when the WS client reports open", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    mock.triggerStatus("connected");

    expect(store.status).toBe("connected");
  });

  it("reflects reconnecting status while the WS client is backing off", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    mock.triggerStatus("connected");
    mock.triggerStatus("reconnecting");

    expect(store.status).toBe("reconnecting");
  });
});

describe("disconnect", () => {
  it("delegates to the WebSocket client", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    store.disconnect();

    expect(mock.disconnect).toHaveBeenCalledOnce();
  });

  it("reflects disconnected status when the WS client closes cleanly", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    mock.triggerStatus("connected");
    store.disconnect();
    mock.triggerStatus("disconnected");

    expect(store.status).toBe("disconnected");
  });
});

describe("sendMessage", () => {
  it("sends a send_message ClientEvent via the WS client", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    store.sendMessage("Hello, world");

    const events = mock.sentEvents().map((s) => JSON.parse(s));
    expect(events).toContainEqual({ type: "send_message", content: "Hello, world" });
  });
});

describe("cancelTurn", () => {
  it("sends a cancel ClientEvent via the WS client", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    store.cancelTurn();

    const events = mock.sentEvents().map((s) => JSON.parse(s));
    expect(events).toContainEqual({ type: "cancel" });
  });
});

describe("server event routing", () => {
  it("routes incoming server events to registered handlers", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });
    const received: ServerEvent[] = [];

    store.onServerEvent((e) => received.push(e));
    store.connectTo("conv-abc");

    mock.triggerEvent({ type: "token", content: "Hello" });

    expect(received).toHaveLength(1);
    expect(received[0]).toEqual({ type: "token", content: "Hello" });
  });

  it("routes to multiple registered handlers", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });
    const a: ServerEvent[] = [];
    const b: ServerEvent[] = [];

    store.onServerEvent((e) => a.push(e));
    store.onServerEvent((e) => b.push(e));
    store.connectTo("conv-abc");

    mock.triggerEvent({ type: "cancelled" });

    expect(a).toHaveLength(1);
    expect(b).toHaveLength(1);
  });
});

describe("error handling (covers 409 Conflict / duplicate tab)", () => {
  it("sets error status when the WS client reports an error", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    mock.triggerStatus("error");

    expect(store.status).toBe("error");
  });

  it("sets a descriptive errorMessage when status is error", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    mock.triggerStatus("error");

    expect(store.errorMessage).not.toBeNull();
    expect(typeof store.errorMessage).toBe("string");
  });

  it("clears errorMessage when connection recovers", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    mock.triggerStatus("error");
    expect(store.errorMessage).not.toBeNull();

    // Re-connect clears the error.
    store.connectTo("conv-abc");
    mock.triggerStatus("connected");
    expect(store.errorMessage).toBeNull();
  });
});

describe("reconnect callback", () => {
  it("invokes onReconnect when transitioning from reconnecting to connected", () => {
    const onReconnect = vi.fn();
    const mock = createMockWsClient();
    const store = createConnectionStore({
      wsClientFactory: () => mock,
      onReconnect,
    });

    store.connectTo("conv-abc");
    mock.triggerStatus("connected");
    mock.triggerStatus("reconnecting");
    mock.triggerStatus("connected"); // reconnect succeeds

    expect(onReconnect).toHaveBeenCalledOnce();
  });

  it("does not invoke onReconnect on initial connection (not a reconnect)", () => {
    const onReconnect = vi.fn();
    const mock = createMockWsClient();
    const store = createConnectionStore({
      wsClientFactory: () => mock,
      onReconnect,
    });

    store.connectTo("conv-abc");
    mock.triggerStatus("connected"); // initial connect, not a reconnect

    expect(onReconnect).not.toHaveBeenCalled();
  });

  it("does not invoke onReconnect when no callback is provided", () => {
    const mock = createMockWsClient();
    const store = createConnectionStore({ wsClientFactory: () => mock });

    store.connectTo("conv-abc");
    mock.triggerStatus("connected");
    mock.triggerStatus("reconnecting");

    // Should not throw even without a callback.
    expect(() => mock.triggerStatus("connected")).not.toThrow();
  });
});
